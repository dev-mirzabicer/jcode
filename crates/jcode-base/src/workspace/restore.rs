use super::*;
use rusqlite::params;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedRestore {
    public: RestoreReview,
    fingerprint: Vec<(String, String)>,
    installation: InstallationId,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreJournal {
    request: RequestId,
    review: ReviewId,
    snapshot: SnapshotId,
    installation: InstallationId,
    originals: Vec<(String, String)>,
    staged_file: String,
    staged_digest: String,
    receipt: Receipt,
}
const FILES: [&str; 3] = [
    "catalog.sqlite3",
    "catalog.sqlite3-wal",
    "catalog.sqlite3-shm",
];

impl WorkspaceService {
    pub fn review_restore(&self, snapshot: SnapshotId) -> Result<RestoreReview> {
        let _lease = self.lease(true)?;
        let selected = self
            .snapshots_locked()?
            .into_iter()
            .find(|s| s.id == snapshot)
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown snapshot"))?;
        let saved = self.verify_snapshot(&selected)?;
        portable::validate_graph(&saved)?;
        let installation = storage::installation(&self.root)?;
        storage::validate(&saved, installation)?;
        let grants = portable::grants(&saved)?;
        drop(saved);
        let current_revision = match self.connection() {
            Ok(connection) => {
                ensure_quiet(&connection)?;
                Some(storage::status(&connection)?.revision)
            }
            Err(error)
                if matches!(
                    error.code,
                    IssueCode::CorruptState | IssueCode::RecoveryRequired
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let public = RestoreReview {
            id: ReviewId::new(),
            snapshot: selected,
            current_revision,
            grants,
            issues: vec![],
        };
        let review = ReviewedRestore {
            public: public.clone(),
            fingerprint: originals(&self.root)?,
            installation,
        };
        storage::atomic_json(
            &self
                .root
                .join("reviews")
                .join(format!("restore-{}.json", public.id)),
            &review,
        )?;
        Ok(public)
    }

    pub fn apply_restore(&self, request: RequestId, review: ReviewId) -> Result<Receipt> {
        let _lease = self.lease(true)?;
        let completed = self
            .root
            .join("recovery")
            .join(format!("restore-{request}.json"));
        if completed.try_exists().map_err(io)? {
            let journal: RestoreJournal = storage::read_json(&completed)?;
            if journal.review != review {
                return Err(issue(
                    IssueCode::Conflict,
                    "Restore request belongs to another review",
                ));
            }
            let pending = self.root.join("restore-pending.json");
            if pending.try_exists().map_err(io)? {
                let unfinished: RestoreJournal = storage::read_json(&pending)?;
                if unfinished.request == request && unfinished.review == review {
                    self.finish_restore(&unfinished)?;
                    std::fs::remove_file(&pending).map_err(io)?;
                    storage::sync_dir(&self.root)?;
                }
            }
            return Ok(journal.receipt);
        }
        let pending = self.root.join("restore-pending.json");
        let journal = if pending.try_exists().map_err(io)? {
            let journal: RestoreJournal = storage::read_json(&pending)?;
            if journal.request != request || journal.review != review {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Another restore is pending; resume its exact request",
                ));
            }
            journal
        } else {
            let approved: ReviewedRestore = storage::read_json(
                &self
                    .root
                    .join("reviews")
                    .join(format!("restore-{review}.json")),
            )?;
            if approved.public.id != review
                || storage::installation(&self.root)? != approved.installation
            {
                return Err(corrupt("Restore review installation mismatch"));
            }
            let source = self.verify_snapshot(&approved.public.snapshot)?;
            portable::validate_graph(&source)?;
            storage::validate(&source, approved.installation)?;
            let current_revision = match self.connection() {
                Ok(connection) => {
                    ensure_quiet(&connection)?;
                    Some(storage::status(&connection)?.revision)
                }
                Err(error)
                    if matches!(
                        error.code,
                        IssueCode::CorruptState | IssueCode::RecoveryRequired
                    ) =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
            if current_revision != approved.public.current_revision
                || originals(&self.root)? != approved.fingerprint
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Catalog changed since restore review",
                ));
            }
            let recovery = self.root.join("recovery").join(request.to_string());
            storage::private_dir(&recovery)?;
            let attempt = recovery.join("review.json");
            if attempt.try_exists().map_err(io)? {
                let original: ReviewId = storage::read_json(&attempt)?;
                if original != review {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Restore request was already prepared against another review",
                    ));
                }
            } else {
                storage::atomic_json(&attempt, &review)?;
            }
            // An incomplete attempt is retained. Retry prepares a distinct owned
            // stage under the same request instead of overwriting uncertain bytes.
            let staged_file = format!("{}.sqlite3", SnapshotId::new());
            let staged = recovery.join(&staged_file);
            backup::copy_database(&source, &staged)?;
            self.checkpoint("restore_staged")?;
            drop(source);
            let mut candidate = storage::raw_connection(&staged, false)?;
            let revision = current_revision
                .unwrap_or(approved.public.snapshot.revision)
                .max(approved.public.snapshot.revision)
                .checked_add(1)
                .ok_or_else(|| corrupt("Revision exhausted"))?;
            let issues = self.revalidate_restored_roots(&candidate)?;
            let tx = candidate.transaction().map_err(io)?;
            // Recorded effects/approvals never become new authority after restore.
            tx.execute("DELETE FROM reviews", []).map_err(io)?;
            if let Ok(current) = self.connection() {
                let mut stmt = current
                    .prepare("SELECT request,input_digest,body FROM receipts")
                    .map_err(io)?;
                for row in stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(io)?
                {
                    let (request, input, body) = row.map_err(io)?;
                    tx.execute("INSERT INTO receipts VALUES(?1,?2,?3) ON CONFLICT(request) DO UPDATE SET input_digest=excluded.input_digest,body=excluded.body",params![request,input,body]).map_err(io)?;
                }
            }
            tx.execute(
                "UPDATE operations SET state='recovery_required' WHERE state='pending'",
                [],
            )
            .map_err(io)?;
            tx.execute("UPDATE session_index SET body=json_set(body,'$.reconciled',json('false'),'$.active',json('false'))",[]).map_err(io)?;
            tx.execute(
                "UPDATE catalog SET revision=?1",
                [i64::try_from(revision).map_err(corrupt)?],
            )
            .map_err(io)?;
            tx.commit().map_err(io)?;
            portable::validate_graph(&candidate)?;
            candidate
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(io)?;
            drop(candidate);
            std::fs::File::open(&staged)
                .and_then(|f| f.sync_all())
                .map_err(io)?;
            if let Ok(connection) = self.connection() {
                self.snapshot_locked(&connection, "Before catalog restore".into(), true)?;
            }
            let journal = RestoreJournal {
                request,
                review,
                snapshot: approved.public.snapshot.id,
                installation: approved.installation,
                originals: originals(&self.root)?,
                staged_file,
                staged_digest: backup::file_digest(&staged)?,
                receipt: Receipt {
                    operation: OperationId::new(),
                    request,
                    revision,
                    targets: vec![],
                    issues,
                },
            };
            storage::atomic_json(&pending, &journal)?;
            self.checkpoint("restore_intent")?;
            journal
        };
        self.finish_restore(&journal)?;
        storage::atomic_json(&completed, &journal)?;
        self.checkpoint("restore_receipt")?;
        std::fs::remove_file(&pending).map_err(io)?;
        storage::sync_dir(&self.root)?;
        Ok(journal.receipt)
    }

    fn revalidate_restored_roots(&self, connection: &Connection) -> Result<Vec<Issue>> {
        let mut unavailable = std::collections::BTreeSet::new();
        let mut issues = vec![];
        let mut stmt = connection
            .prepare("SELECT location,body FROM bindings")
            .map_err(io)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(io)?;
        for row in rows {
            let (id, body) = row.map_err(io)?;
            let bound: BoundLocation = decode(&body)?;
            let id: LocationId = id.parse().map_err(corrupt)?;
            if let Err(error) = self.resolver.resolve_directory(&bound.binding) {
                unavailable.insert(id);
                if let Entity::Location(mut location) = entity(connection, EntityId::Location(id))?
                    && location.lifecycle != LocationLifecycle::Closed
                {
                    location.lifecycle = LocationLifecycle::Unavailable;
                    organization::save_entity(connection, &Entity::Location(location))?;
                }
                issues.push(issue(
                    IssueCode::OfflineVolume,
                    format!("Restored location {id} remains unavailable: {error}"),
                ));
            }
        }
        for mut grant in portable::grants(connection)? {
            // Whole-audience grants must not become active through an incomplete
            // physical revalidation. A later explicit grant review can enable them.
            if grant.state == GrantState::Active && !unavailable.is_empty() {
                grant.state = GrantState::Disabled;
                connection
                    .execute(
                        "UPDATE grants SET body=?1 WHERE id=?2",
                        params![encode(&grant)?, grant.id.to_string()],
                    )
                    .map_err(io)?;
            }
        }
        Ok(issues)
    }

    fn finish_restore(&self, journal: &RestoreJournal) -> Result<()> {
        let recovery = self.root.join("recovery").join(journal.request.to_string());
        let stem = journal
            .staged_file
            .strip_suffix(".sqlite3")
            .ok_or_else(|| corrupt("Invalid restore stage identity"))?;
        let _: SnapshotId = stem.parse().map_err(corrupt)?;
        let staged = recovery.join(&journal.staged_file);
        let destination = self.root.join("catalog.sqlite3");
        if !staged.try_exists().map_err(io)? {
            if backup::file_digest(&destination)? != journal.staged_digest {
                return Err(corrupt(
                    "Published restore differs from its prepared replacement",
                ));
            }
            let connection = storage::raw_connection(&destination, false)?;
            storage::validate(&connection, journal.installation)?;
            return Ok(());
        }
        if backup::file_digest(&staged)? != journal.staged_digest {
            return Err(corrupt("Prepared restore changed"));
        }
        for (leaf, expected) in &journal.originals {
            if !FILES.contains(&leaf.as_str()) {
                return Err(corrupt("Invalid original catalog component"));
            }
            let from = self.root.join(leaf);
            let to = recovery.join(leaf);
            if to.try_exists().map_err(io)? {
                if backup::file_digest(&to)? != *expected || from.try_exists().map_err(io)? {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Restore original changed or replacement appeared",
                    ));
                }
            } else {
                if backup::file_digest(&from)? != *expected {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Catalog component changed before replacement",
                    ));
                }
                std::fs::rename(&from, &to).map_err(io)?;
                storage::sync_dir(&recovery)?;
                storage::sync_dir(&self.root)?;
                self.checkpoint("restore_original_moved")?;
            }
        }
        for leaf in FILES {
            if self.root.join(leaf).try_exists().map_err(io)? {
                return Err(issue(
                    IssueCode::Conflict,
                    "Unexpected catalog component prevents restore publication",
                ));
            }
        }
        std::fs::rename(&staged, &destination).map_err(io)?;
        storage::sync_dir(&self.root)?;
        storage::sync_dir(&recovery)?;
        self.checkpoint("restore_published")?;
        let connection = storage::raw_connection(&destination, false)?;
        storage::validate(&connection, journal.installation)?;
        Ok(())
    }
}
fn originals(root: &Path) -> Result<Vec<(String, String)>> {
    let mut result = vec![];
    for leaf in FILES {
        let path = root.join(leaf);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                result.push((leaf.into(), backup::file_digest(&path)?))
            }
            Ok(_) => return Err(corrupt("Catalog recovery refuses nonregular components")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
    }
    Ok(result)
}
fn ensure_quiet(connection: &Connection) -> Result<()> {
    let active: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE state='pending')",
            [],
            |r| r.get(0),
        )
        .map_err(io)?;
    if active {
        return Err(issue(
            IssueCode::Busy,
            "Pending workspace operations must be reconciled before restore",
        ));
    }
    Ok(())
}
