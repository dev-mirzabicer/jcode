//! Reviewed whole-output deletion, inside the existing physical storage owner.
use super::*;
use jcode_tool_types::cleanup::{
    CleanupCandidate, CleanupItemOutcome, CleanupOutcome, CleanupReview, CleanupSelection,
};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize)]
struct DeletionTarget {
    id: String,
    physical: PathBuf,
    identity: DirectoryIdentity,
    archive: ArchiveConfig,
    generation: i64,
    files: Vec<Part>,
    affected_snapshots: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct CleanupPlan {
    reader: String,
    review: CleanupReview,
    targets: Vec<DeletionTarget>,
}

impl ExecutionStore {
    /// Recovery consumes only persisted confirmations. Merely reviewed plans
    /// are never executed by maintenance, reload, or a model call.
    pub(in crate::execution) fn resume_confirmed_output_cleanup(
        &self,
        now: i64,
    ) -> Result<Vec<CleanupOutcome>> {
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT id,reader,confirmation FROM cleanup_reviews r WHERE state='confirmed' AND EXISTS(SELECT 1 FROM output_deletions d WHERE d.review_id=r.id AND d.stage='deleting') ORDER BY id")?;
        let plans = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut outcomes = Vec::new();
        for (id, reader, confirmation) in plans {
            match self.confirm_output_cleanup(&reader, &id, &confirmation, now) {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    let mut query = connection.prepare(
                        "SELECT id,stage FROM output_deletions WHERE review_id=?1 ORDER BY id",
                    )?;
                    let items = query
                        .query_map([&id], |row| {
                            Ok(CleanupItemOutcome {
                                run_id: row.get(0)?,
                                deleted: row.get::<_, String>(1)? == "deleted",
                                error: Some(format!(
                                    "Cleanup recovery remains incomplete: {error:#}"
                                )),
                            })
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    outcomes.push(CleanupOutcome {
                        review_id: id,
                        items,
                    });
                }
            }
        }
        Ok(outcomes)
    }

    /// Called only by the human protocol adapter, never registered as a model tool.
    pub fn review_output_cleanup(
        &self,
        reader: &str,
        selection: CleanupSelection,
        now: i64,
    ) -> Result<CleanupReview> {
        self.review_output_cleanup_with_environment(reader, selection, now, &NativeEnvironment)
    }

    pub(super) fn review_output_cleanup_with_environment(
        &self,
        reader: &str,
        selection: CleanupSelection,
        _now: i64,
        environment: &dyn StorageEnvironment,
    ) -> Result<CleanupReview> {
        ensure!(
            !reader.is_empty(),
            "Cleanup review requires an attached trusted-client session"
        );
        let _snapshot_lease = self
            .snapshot_lease(false, false)?
            .context("Snapshot references are busy")?;
        let (requested_bytes, requested_ids) = match selection {
            CleanupSelection::OldestBytes { bytes } => {
                ensure!(bytes > 0, "Cleanup byte target must be positive");
                (Some(bytes), None)
            }
            CleanupSelection::Outputs { run_ids } => {
                let ids: BTreeSet<_> = run_ids.iter().cloned().collect();
                ensure!(
                    !ids.is_empty() && ids.len() == run_ids.len(),
                    "Choose distinct whole outputs"
                );
                (None, Some(ids))
            }
        };
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT r.id,r.created FROM runs r JOIN output_locations l ON l.id=r.id WHERE r.state NOT IN ('prepared','queued','running') AND l.archived=1 AND l.cold_archived_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM output_deletions d WHERE d.id=r.id) AND NOT EXISTS(SELECT 1 FROM relocations m WHERE m.id=r.id AND m.stage<>'complete') ORDER BY r.created,r.id")?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut candidates = Vec::new();
        let mut targets = Vec::new();
        let mut total = 0u64;
        for (id, created_at) in rows {
            if requested_ids.as_ref().is_some_and(|ids| !ids.contains(&id)) {
                continue;
            }
            let record = self
                .inspect(&id)?
                .context("Cleanup candidate disappeared")?;
            let Some(_lease) = try_output_lease(self, &id)? else {
                continue;
            };
            let target = self.capture_deletion_target(&id, environment)?;
            let bytes = target.files.iter().try_fold(0u64, |total, part| {
                total
                    .checked_add(part.bytes)
                    .context("Cleanup size overflow")
            })?;
            total = total.checked_add(bytes).context("Cleanup total overflow")?;
            candidates.push(CleanupCandidate {
                run_id: id,
                session_id: record.session_id,
                bytes,
                created_at,
                affected_snapshot_ids: target.affected_snapshots.clone(),
            });
            targets.push(target);
            if requested_bytes.is_some_and(|wanted| total >= wanted) {
                break;
            }
        }
        if let Some(ids) = requested_ids {
            ensure!(
                targets.len() == ids.len(),
                "One or more selected outputs are not completed cold-archived candidates; no deletion occurred"
            );
        }
        let review = CleanupReview {
            review_id:format!("cleanup-{}",uuid::Uuid::new_v4().simple()),
            confirmation_id:uuid::Uuid::new_v4().simple().to_string(), candidates,
            requested_bytes,selected_bytes:total,overshoot_bytes:requested_bytes.map_or(0,|wanted|total.saturating_sub(wanted)),
            impact:"Deletes only these whole retained output bundles. Referenced snapshots lose further access to their backing output. Already delivered transcript text, original invocation inputs, sessions, project files and child-authored artifacts are not deleted.".into(),
        };
        let plan = CleanupPlan {
            reader: reader.into(),
            review: review.clone(),
            targets,
        };
        let directory = self.root().join("cleanup");
        private_directory(&directory)?;
        let path = directory.join(format!("{}.json", review.review_id));
        let bytes = serde_json::to_vec(&plan)?;
        crate::storage::write_text_secret(&path, std::str::from_utf8(&bytes)?)?;
        self.connection()?.execute("INSERT INTO cleanup_reviews(id,reader,confirmation,digest,state) VALUES (?1,?2,?3,?4,'reviewed')",params![review.review_id,reader,review.confirmation_id,format!("{:x}",Sha256::digest(&bytes))])?;
        Ok(review)
    }

    pub fn confirm_output_cleanup(
        &self,
        reader: &str,
        review_id: &str,
        confirmation_id: &str,
        now: i64,
    ) -> Result<CleanupOutcome> {
        self.confirm_output_cleanup_with_environment(
            reader,
            review_id,
            confirmation_id,
            now,
            &NativeEnvironment,
        )
    }

    pub(super) fn confirm_output_cleanup_with_environment(
        &self,
        reader: &str,
        review_id: &str,
        confirmation_id: &str,
        _now: i64,
        environment: &dyn StorageEnvironment,
    ) -> Result<CleanupOutcome> {
        ensure!(
            review_id.len() == 40
                && review_id.starts_with("cleanup-")
                && review_id[8..].bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid cleanup review identity"
        );
        let _snapshot_lease = self
            .snapshot_lease(false, false)?
            .context("Snapshot inspection is busy")?;
        let connection = self.connection()?;
        let row: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT reader,confirmation,digest,state FROM cleanup_reviews WHERE id=?1",
                [review_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (owner, confirmation, digest, state) =
            row.context("Unknown cleanup review; obtain a new exact preview")?;
        ensure!(
            owner == reader && confirmation == confirmation_id,
            "Cleanup confirmation does not match this human session's exact review; no deletion occurred"
        );
        let path = self
            .root()
            .join("cleanup")
            .join(format!("{review_id}.json"));
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "Cleanup review changed type"
        );
        let bytes = std::fs::read(&path)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == digest,
            "Cleanup review failed integrity validation"
        );
        let plan: CleanupPlan = serde_json::from_slice(&bytes)?;
        ensure!(
            plan.reader == reader
                && plan.review.review_id == review_id
                && plan.review.confirmation_id == confirmation_id,
            "Cleanup review identity mismatch"
        );
        let mut leases = Vec::new();
        for target in &plan.targets {
            leases.push(output_lease(self, &target.id)?);
        }
        if state == "reviewed" {
            // All-target preflight before any destructive effect. A changed
            // candidate set requires another human-visible review, not a fallback.
            for target in &plan.targets {
                let record = self
                    .inspect(&target.id)?
                    .context("Cleanup candidate disappeared")?;
                ensure!(
                    record.state.terminal(),
                    "Cleanup review is stale because a candidate is no longer completed"
                );
                let current = self.capture_deletion_target(&target.id, environment)?;
                ensure!(
                    serde_json::to_vec(&current)? == serde_json::to_vec(target)?,
                    "Cleanup review is stale: output identity, contents or snapshot impact changed"
                );
            }
            let mut connection = self.connection()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                tx.execute(
                    "UPDATE cleanup_reviews SET state='confirmed' WHERE id=?1 AND state='reviewed'",
                    [review_id]
                )? == 1,
                "Cleanup review changed during confirmation"
            );
            for target in &plan.targets {
                tx.execute(
                    "INSERT INTO output_deletions(id,review_id,stage) VALUES (?1,?2,'deleting')",
                    params![target.id, review_id],
                )?;
            }
            tx.commit()?;
        } else {
            ensure!(
                state == "confirmed",
                "Cleanup review has an unsupported state"
            );
        }
        let mut items = Vec::new();
        for target in &plan.targets {
            let result = self.delete_reviewed_output(target, review_id, environment);
            let mut error = result.as_ref().err().map(|error| format!("{error:#}"));
            if let Some(message) = &error {
                let persisted = self.connection().and_then(|connection| {
                    connection
                        .execute(
                            "UPDATE output_deletions SET error=?2 WHERE id=?1",
                            params![target.id, message],
                        )
                        .map_err(anyhow::Error::from)
                });
                if let Err(persistence) = persisted {
                    error = Some(format!(
                        "{message}; cleanup diagnostic persistence also failed: {persistence:#}"
                    ));
                }
            }
            items.push(CleanupItemOutcome {
                run_id: target.id.clone(),
                deleted: result.is_ok(),
                error,
            });
        }
        drop(leases);
        Ok(CleanupOutcome {
            review_id: review_id.into(),
            items,
        })
    }

    fn capture_deletion_target(
        &self,
        id: &str,
        environment: &dyn StorageEnvironment,
    ) -> Result<DeletionTarget> {
        let connection = self.connection()?;
        let (physical,generation,spec,cold):(String,i64,Option<String>,Option<i64>) = connection.query_row("SELECT physical,generation,archive_spec,cold_archived_at FROM output_locations WHERE id=?1 AND archived=1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        ensure!(
            cold.is_some(),
            "Recent emergency spillover is excluded from cleanup"
        );
        let archive: ArchiveConfig =
            serde_json::from_str(&spec.context("Archive identity is unavailable")?)?;
        let root = environment.existing_archive(&archive)?;
        let physical = PathBuf::from(physical);
        let namespace = format!(
            "{:x}",
            Sha256::digest(self.root().as_os_str().as_encoded_bytes())
        );
        ensure!(
            physical.parent() == Some(root.path.join(namespace).as_path()),
            "Cleanup output is outside its exact archive namespace"
        );
        let binding = DirectoryBinding::open(&physical)?;
        let identity: serde_json::Value =
            serde_json::from_reader(binding.read_part("identity.json")?.take(16 * 1024))?;
        ensure!(
            identity["invocation_id"] == id && identity["schema"] == 1,
            "Cleanup output identity changed"
        );
        ensure!(
            std::fs::read_link(self.root().join("outputs").join(id))? == physical,
            "Cleanup alias changed"
        );
        let manifest = MoveManifest::capture(id, &physical, &physical)?;
        let mut query = connection.prepare(
            "SELECT snapshot_id FROM inspection_output_refs WHERE run_id=?1 ORDER BY snapshot_id",
        )?;
        let affected_snapshots = query
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(DeletionTarget {
            id: id.into(),
            physical,
            identity: manifest.source_identity,
            archive,
            generation,
            files: manifest.files,
            affected_snapshots,
        })
    }

    fn delete_reviewed_output(
        &self,
        target: &DeletionTarget,
        review_id: &str,
        environment: &dyn StorageEnvironment,
    ) -> Result<()> {
        let (owner, stage): (String, String) = self.connection()?.query_row(
            "SELECT review_id,stage FROM output_deletions WHERE id=?1",
            [&target.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            owner == review_id,
            "Output deletion belongs to a different confirmation"
        );
        if stage == "deleted" {
            return Ok(());
        }
        ensure!(stage == "deleting", "Invalid output deletion stage");
        let root = environment.existing_archive(&target.archive)?;
        ensure!(
            target.physical.starts_with(&root.path),
            "Deletion archive root changed"
        );
        environment.checkpoint("cleanup_before_delete")?;
        if target.physical.exists() {
            let binding = DirectoryBinding::open(&target.physical)?;
            ensure!(
                binding.identity()? == target.identity,
                "Deletion directory was replaced; preserving it"
            );
            for part in &target.files {
                let path = target.physical.join(&part.name);
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => {
                        ensure!(
                            metadata.is_file()
                                && metadata.len() == part.bytes
                                && hash_reader(binding.read_part(&part.name)?)? == part.sha256,
                            "Deletion part changed; preserving it"
                        );
                        binding.remove_part(&part.name)?;
                        environment.checkpoint("cleanup_after_part")?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            binding.verify()?;
            std::fs::remove_dir(&target.physical)?;
            sync_directory(target.physical.parent().context("Missing archive parent")?)?;
        }
        environment.checkpoint("cleanup_after_files")?;
        let alias = self.root().join("outputs").join(&target.id);
        match std::fs::symlink_metadata(&alias) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_symlink()
                        && std::fs::read_link(&alias)? == target.physical,
                    "Deletion alias was replaced; preserving it"
                );
                std::fs::remove_file(&alias)?;
                sync_directory(alias.parent().context("Missing alias parent")?)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        environment.checkpoint("cleanup_after_alias")?;
        self.connection()?.execute(
            "UPDATE output_deletions SET stage='deleted',error=NULL WHERE id=?1 AND review_id=?2",
            params![target.id, review_id],
        )?;
        Ok(())
    }
}
