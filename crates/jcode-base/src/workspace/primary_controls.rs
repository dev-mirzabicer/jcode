//! Location intent is catalog-owned. Effective location and its notice are one
//! Session checkpoint. Recovery only derives the index from that checkpoint.
use super::*;
use rusqlite::params;

pub struct PrimaryControlLease {
    _catalog: CatalogLease,
    _file: std::fs::File,
}

#[derive(Serialize, Deserialize)]
struct StoredChange {
    record: LocationChangeRecord,
    target: PhysicalBinding,
}

impl WorkspaceService {
    pub fn primary_control_lease(&self, session: &str) -> Result<PrimaryControlLease> {
        if session.is_empty()
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Invalid primary Session identity",
            ));
        }
        // A never-initialized catalog has no controls to order. Report that
        // fact rather than a missing lease directory.
        if !self.catalog_present()? {
            return Err(Issue::not_initialized());
        }
        let catalog = self.lease(false)?;
        let file = storage::private_file(
            &self
                .root
                .join("leases")
                .join(format!("control-{session}.lock")),
            false,
        )?;
        file.try_lock().map_err(|e| {
            issue(
                IssueCode::Busy,
                format!("Primary control is being committed: {e}"),
            )
        })?;
        Ok(PrimaryControlLease {
            _catalog: catalog,
            _file: file,
        })
    }

    /// The caller holds the primary writer lease. The control lease orders
    /// submission/cancellation against a safe-boundary checkpoint.
    pub fn request_location_change(
        &self,
        input: LocationChangeRequest,
    ) -> Result<LocationChangeRecord> {
        self.request_location_control(input, None)
    }

    pub fn request_legacy_adoption(
        &self,
        input: LegacyLocationAdoptionRequest,
    ) -> Result<LocationChangeRecord> {
        self.request_location_control(
            LocationChangeRequest {
                request: input.request,
                session: input.session,
                expected_session_revision: 0,
                expected_catalog_revision: input.expected_catalog_revision,
                placement: input.placement,
                cwd: input.cwd,
            },
            Some(LegacyLocationOrigin {
                working_dir: input.expected_working_dir,
            }),
        )
    }

    fn request_location_control(
        &self,
        input: LocationChangeRequest,
        legacy_origin: Option<LegacyLocationOrigin>,
    ) -> Result<LocationChangeRecord> {
        let _control = self.primary_control_lease(&input.session)?;
        let connection = self.connection()?;
        let operation: OperationId = input.request.to_string().parse().map_err(corrupt)?;
        if let Some(stored) = read_change(&connection, operation)? {
            if stored.record.input != input || stored.record.legacy_origin != legacy_origin {
                return Err(issue(
                    IssueCode::Conflict,
                    "Location request identity already has different input",
                ));
            }
            return Ok(stored.record);
        }
        let session = crate::session::Session::load_startup_stub(&input.session).map_err(io)?;
        if session.isolated_child.is_some() {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Child placement remains part of its fixed execution identity",
            ));
        }
        if let Some(origin) = &legacy_origin {
            self.require_legacy_scope_absent(&session.id)?;
            if session.location.is_some()
                || session.primary_creation.is_some()
                || session.scope_copy.is_some()
                || session.working_dir.as_ref().map(PathBuf::from) != origin.working_dir
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Legacy adoption review no longer matches the Session",
                ));
            }
        } else {
            let location = session.location.as_ref().ok_or_else(|| {
                issue(
                    IssueCode::RecoveryRequired,
                    "Legacy Session needs explicit adoption, not a location move",
                )
            })?;
            // Dependent FIFO controls still name the exact expected revision.
            if input.expected_session_revision < location.revision {
                return Err(issue(
                    IssueCode::Conflict,
                    "Session location revision is stale",
                ));
            }
        }
        let prepared = self.prepare_session_location(
            &input.session,
            input.placement,
            Some(&input.cwd),
            operation,
        )?;
        if prepared.catalog_revision != input.expected_catalog_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Catalog changed since location review",
            ));
        }
        let stored = StoredChange {
            record: LocationChangeRecord {
                operation,
                input,
                state: LocationChangeState::Pending,
                effective_revision: None,
                notice_message: None,
                issue: None,
                legacy_origin,
            },
            target: prepared.location.cwd,
        };
        let transaction = connection.unchecked_transaction().map_err(io)?;
        organization::require_revision(
            &transaction,
            stored.record.input.expected_catalog_revision,
        )?;
        transaction.execute("INSERT INTO operations(id,kind,state,body) VALUES(?1,'primary_location','pending',?2)", params![operation.to_string(), encode(&stored)?]).map_err(io)?;
        transaction
            .execute(
                "INSERT INTO operation_targets VALUES(?1,?2)",
                params![
                    operation.to_string(),
                    query::placement_target(stored.record.input.placement).to_string()
                ],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("location_intent")?;
        Ok(stored.record)
    }

    pub fn inspect_location_change(&self, operation: OperationId) -> Result<LocationChangeRecord> {
        let _lease = self.lease(false)?;
        Ok(read_change(&self.connection()?, operation)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown location change"))?
            .record)
    }

    pub fn pending_location_changes(&self, session: &str) -> Result<Vec<LocationChangeRecord>> {
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT id FROM operations WHERE kind='primary_location' AND state IN ('pending','recovery_required') ORDER BY rowid").map_err(io)?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(io)?;
        let mut records = Vec::new();
        for id in ids {
            let operation = id.map_err(io)?.parse().map_err(corrupt)?;
            let stored = read_change(&connection, operation)?
                .ok_or_else(|| corrupt("Location operation disappeared"))?;
            if stored.record.input.session == session {
                records.push(stored.record);
            }
        }
        Ok(records)
    }

    pub fn cancel_location_change(&self, operation: OperationId) -> Result<LocationChangeRecord> {
        let mut stored = read_change(&self.connection()?, operation)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown location change"))?;
        let _control = self.primary_control_lease(&stored.record.input.session)?;
        stored = read_change(&self.connection()?, operation)?
            .ok_or_else(|| corrupt("Location operation disappeared"))?;
        let session =
            crate::session::Session::load_startup_stub(&stored.record.input.session).map_err(io)?;
        if session.location.as_ref().and_then(|l| l.last_operation) == Some(operation) {
            return Err(issue(
                IssueCode::Conflict,
                "Location change has already committed",
            ));
        }
        if stored.record.state == LocationChangeState::Cancelled {
            return Ok(stored.record);
        }
        if stored.record.state != LocationChangeState::Pending {
            return Err(issue(
                IssueCode::Conflict,
                "Only a pending location change can be cancelled",
            ));
        }
        stored.record.state = LocationChangeState::Cancelled;
        write_change(&self.connection()?, &stored)?;
        Ok(stored.record)
    }

    pub fn prepare_location_change(
        &self,
        record: &LocationChangeRecord,
        current: &crate::session::StoredSessionLocation,
    ) -> Result<PreparedPrimaryLocation> {
        if current.revision != record.input.expected_session_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Session location revision changed before application",
            ));
        }
        if record.legacy_origin.is_some() {
            return Err(issue(
                IssueCode::Conflict,
                "Adoption already has a managed binding",
            ));
        }
        let mut prepared = self.prepare_reviewed_location(record)?;
        prepared.location.initial_cwd = current.initial_cwd.clone();
        prepared.location.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("Session location revision exhausted"))?;
        Ok(prepared)
    }

    pub fn prepare_legacy_adoption(
        &self,
        record: &LocationChangeRecord,
        session: &crate::session::Session,
    ) -> Result<PreparedPrimaryLocation> {
        let origin = record.legacy_origin.as_ref().ok_or_else(|| {
            issue(
                IssueCode::InvalidInput,
                "A move cannot implicitly adopt a legacy Session",
            )
        })?;
        if session.id != record.input.session
            || session.location.is_some()
            || session.primary_creation.is_some()
            || session.scope_copy.is_some()
            || session.isolated_child.is_some()
            || session.working_dir.as_ref().map(PathBuf::from) != origin.working_dir
            || record.input.expected_session_revision != 0
        {
            return Err(issue(
                IssueCode::Conflict,
                "Legacy Session changed since adoption review",
            ));
        }
        let mut prepared = self.prepare_reviewed_location(record)?;
        if let Some(cwd) = &origin.working_dir {
            prepared.location.initial_cwd = cwd.clone();
        }
        // Missing historical cwd remains explicit in the adoption receipt.
        Ok(prepared)
    }

    fn prepare_reviewed_location(
        &self,
        record: &LocationChangeRecord,
    ) -> Result<PreparedPrimaryLocation> {
        let stored = read_change(&self.connection()?, record.operation)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown location change"))?;
        if stored.record != *record {
            return Err(issue(
                IssueCode::Conflict,
                "Location control differs from its authoritative intent",
            ));
        }
        if stored.record.state != LocationChangeState::Pending {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Location change is not pending",
            ));
        }
        let prepared = self.prepare_session_location(
            &record.input.session,
            record.input.placement,
            Some(&record.input.cwd),
            record.operation,
        )?;
        if prepared.catalog_revision != record.input.expected_catalog_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Catalog changed before location application",
            ));
        }
        if stored.target != prepared.location.cwd {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Reviewed location target was replaced or relocated",
            ));
        }
        Ok(prepared)
    }

    pub fn fail_location_change(
        &self,
        operation: OperationId,
        problem: Issue,
    ) -> Result<LocationChangeRecord> {
        let mut stored = read_change(&self.connection()?, operation)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown location change"))?;
        stored.record.state = LocationChangeState::Failed;
        stored.record.issue = Some(problem);
        write_change(&self.connection()?, &stored)?;
        Ok(stored.record)
    }

    /// Must run while the caller retains its Session/control leases. It never
    /// executes a move or appends prose, including after catalog restore.
    pub fn reconcile_location_change(
        &self,
        operation: OperationId,
    ) -> Result<LocationChangeRecord> {
        let connection = self.connection()?;
        let mut stored = read_change(&connection, operation)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown location change"))?;
        if stored.record.state == LocationChangeState::Complete {
            return Ok(stored.record);
        }
        let session = crate::session::Session::load(&stored.record.input.session).map_err(io)?;
        let location = session
            .location
            .as_ref()
            .ok_or_else(|| corrupt("Location checkpoint is absent"))?;
        if location.last_operation != Some(operation)
            || location.placement != stored.record.input.placement
            || location.cwd != stored.target
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Location intent has no matching Session checkpoint",
            ));
        }
        let notice = format!("location_{operation}");
        if session.messages.iter().filter(|m| m.id == notice).count() != 1 {
            return Err(corrupt("Location checkpoint has no unique notice"));
        }
        self.checkpoint("location_before_index")?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let index = SessionIndex {
            session: session.id.clone(),
            placement: location.placement,
            session_revision: location.revision,
            operation,
            active: true,
            reconciled: true,
        };
        let previous: Option<String> = transaction
            .query_row(
                "SELECT body FROM session_index WHERE session=?1",
                [&session.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(io)?;
        if let Some(previous) = previous {
            let previous: SessionIndex = decode(&previous)?;
            if previous.session_revision > location.revision {
                return Err(issue(
                    IssueCode::Conflict,
                    "A newer location index is already present",
                ));
            }
        }
        transaction.execute("INSERT INTO session_index VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET target=excluded.target,body=excluded.body", params![session.id, query::placement_target(location.placement).to_string(), encode(&index)?]).map_err(io)?;
        stored.record.state = LocationChangeState::Complete;
        stored.record.effective_revision = Some(location.revision);
        stored.record.notice_message = Some(notice);
        stored.record.issue = None;
        write_change(&transaction, &stored)?;
        transaction
            .execute("UPDATE catalog SET revision=revision+1", [])
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("location_index_committed")?;
        Ok(stored.record)
    }
}

fn read_change(connection: &Connection, operation: OperationId) -> Result<Option<StoredChange>> {
    let value: Option<(String, String)> = connection
        .query_row(
            "SELECT body,state FROM operations WHERE id=?1 AND kind='primary_location'",
            [operation.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    value
        .map(|(body, state)| {
            let mut stored: StoredChange = decode(&body)?;
            if stored.record.operation != operation {
                return Err(corrupt("Location operation identity changed"));
            }
            stored.record.state = match state.as_str() {
                "pending" => LocationChangeState::Pending,
                "complete" => LocationChangeState::Complete,
                "failed" if stored.record.state == LocationChangeState::Cancelled => {
                    LocationChangeState::Cancelled
                }
                "failed" => LocationChangeState::Failed,
                "recovery_required" => LocationChangeState::RecoveryRequired,
                _ => return Err(corrupt("Unknown location operation state")),
            };
            Ok(stored)
        })
        .transpose()
}
fn write_change(connection: &Connection, stored: &StoredChange) -> Result<()> {
    let state = match stored.record.state {
        LocationChangeState::Pending => "pending",
        LocationChangeState::Complete => "complete",
        LocationChangeState::Cancelled | LocationChangeState::Failed => "failed",
        LocationChangeState::RecoveryRequired => "recovery_required",
    };
    connection
        .execute(
            "UPDATE operations SET state=?2,body=?3 WHERE id=?1 AND kind='primary_location'",
            params![stored.record.operation.to_string(), state, encode(stored)?],
        )
        .map_err(io)?;
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::session::Session;

    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => crate::env::set_var(key, value),
                    None => crate::env::remove_var(key),
                }
            }
        }
    }

    fn register(service: &WorkspaceService, path: &Path) -> Placement {
        std::fs::create_dir(path).unwrap();
        let review = service
            .review_organization_change(
                service.status().unwrap().revision,
                OrganizationChange::RegisterLocation {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: path.into(),
                    registration: Registration::Standalone,
                },
            )
            .unwrap();
        let receipt = service
            .apply_organization_change(RequestId::new(), review.id)
            .unwrap();
        let EntityId::Location(id) = receipt.targets[0] else {
            panic!("location")
        };
        Placement::Standalone(id)
    }

    #[test]
    fn location_change_checkpoint_reconciliation_cancel_and_conflict() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let _env = Environment(vec![
            ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
            ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
        ]);
        crate::env::set_var("JCODE_HOME", temp.path().join("home"));
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path().join("runtime"));
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new()).unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let pa = register(&service, &a);
        let pb = register(&service, &b);
        let mut session = Session::create(None, None);
        session.location = Some(
            service
                .prepare_primary_location(pa, Some(&a), OperationId::new())
                .unwrap()
                .location,
        );
        session.working_dir = Some(a.canonicalize().unwrap().to_string_lossy().into_owned());
        session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: "original synthetic input".into(),
                cache_control: None,
            }],
        );
        session.save().unwrap();
        let original = serde_json::to_value(&session.messages).unwrap();
        let initial_cwd = session.location.as_ref().unwrap().initial_cwd.clone();
        let request = |session: &Session| LocationChangeRequest {
            request: RequestId::new(),
            session: session.id.clone(),
            expected_session_revision: session.location.as_ref().unwrap().revision,
            expected_catalog_revision: service.status().unwrap().revision,
            placement: pb,
            cwd: b.clone(),
        };
        let cancelled = service.request_location_change(request(&session)).unwrap();
        assert_eq!(
            service
                .cancel_location_change(cancelled.operation)
                .unwrap()
                .state,
            LocationChangeState::Cancelled
        );
        assert_eq!(
            service
                .inspect_location_change(cancelled.operation)
                .unwrap()
                .state,
            LocationChangeState::Cancelled
        );
        assert!(
            service
                .pending_location_changes(&session.id)
                .unwrap()
                .is_empty()
        );
        for stage in ["location_before_index", "location_index_committed"] {
            let input = request(&session);
            let record = service.request_location_change(input.clone()).unwrap();
            assert_eq!(
                service.request_location_change(input.clone()).unwrap(),
                record
            );
            let mut conflict = input.clone();
            conflict.cwd = a.clone();
            assert_eq!(
                service.request_location_change(conflict).unwrap_err().code,
                IssueCode::Conflict
            );
            assert!(service.reconcile_location_change(record.operation).is_err());
            let _lease = service.primary_control_lease(&session.id).unwrap();
            let prepared = service
                .prepare_location_change(&record, session.location.as_ref().unwrap())
                .unwrap();
            let candidate = session
                .stage_location_change(prepared.location.clone(), "synthetic control".into())
                .unwrap();
            session.commit_location_candidate(candidate).unwrap();
            let mut fault = service.clone();
            fault.fault = Some(std::sync::Arc::new(move |observed| {
                if observed == stage {
                    Err(io("injected checkpoint interruption"))
                } else {
                    Ok(())
                }
            }));
            assert!(fault.reconcile_location_change(record.operation).is_err());
            drop(_lease);
            assert_eq!(
                service
                    .cancel_location_change(record.operation)
                    .unwrap_err()
                    .code,
                IssueCode::Conflict
            );
            let _lease = service.primary_control_lease(&session.id).unwrap();
            let restored = Session::load(&session.id).unwrap();
            assert_eq!(restored.location, session.location);
            assert_eq!(restored.location.as_ref().unwrap().initial_cwd, initial_cwd);
            assert_eq!(
                serde_json::to_value(&restored.messages[..original.as_array().unwrap().len()])
                    .unwrap(),
                original
            );
            let complete = service.reconcile_location_change(record.operation).unwrap();
            assert_eq!(complete.state, LocationChangeState::Complete);
            assert_eq!(
                service.reconcile_location_change(record.operation).unwrap(),
                complete
            );
            assert_eq!(
                service.request_location_change(input).unwrap_err().code,
                IssueCode::Busy
            );
            assert_eq!(
                restored
                    .messages
                    .iter()
                    .filter(|m| m.id == format!("location_{}", record.operation))
                    .count(),
                1
            );
            assert_eq!(
                service.sessions(None, None, 20).unwrap()[0].session_revision,
                restored.location.unwrap().revision
            );
        }
    }

    #[test]
    fn location_change_preserves_old_binding_on_failed_checkpoint_and_replaced_target() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let _env = Environment(vec![
            ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
            ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
        ]);
        crate::env::set_var("JCODE_HOME", temp.path().join("home"));
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path().join("runtime"));
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new()).unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let pa = register(&service, &a);
        let pb = register(&service, &b);
        let mut session = Session::create(None, None);
        session.location = Some(
            service
                .prepare_primary_location(pa, Some(&a), OperationId::new())
                .unwrap()
                .location,
        );
        session.working_dir = Some(a.canonicalize().unwrap().to_string_lossy().into_owned());
        session.save().unwrap();
        let before = session.location.clone();
        let record = service
            .request_location_change(LocationChangeRequest {
                request: RequestId::new(),
                session: session.id.clone(),
                expected_session_revision: 1,
                expected_catalog_revision: service.status().unwrap().revision,
                placement: pb,
                cwd: b.clone(),
            })
            .unwrap();
        let prepared = service
            .prepare_location_change(&record, before.as_ref().unwrap())
            .unwrap();
        let candidate = session
            .stage_location_change(prepared.location.clone(), "synthetic notice".into())
            .unwrap();
        // A different durable checkpoint invalidates this writer without losing
        // either the original location or the accepted pending intent.
        let mut peer = Session::load(&session.id).unwrap();
        peer.replace_messages(vec![]);
        peer.save().unwrap();
        assert!(session.commit_location_candidate(candidate).is_err());
        assert_eq!(session.location, before);
        assert_eq!(Session::load(&session.id).unwrap().location, before);
        std::fs::rename(&b, temp.path().join("b-retained")).unwrap();
        std::fs::create_dir(&b).unwrap();
        assert!(
            service
                .prepare_location_change(&record, before.as_ref().unwrap())
                .is_err()
        );
        assert_eq!(
            service
                .inspect_location_change(record.operation)
                .unwrap()
                .state,
            LocationChangeState::Pending
        );
    }
}

impl WorkspaceService {
    /// A trusted client's read of committed Session location. The Session owns
    /// placement and cwd; the catalog contributes only its unfinished changes.
    /// Catalog unavailability is reported beside the Session facts, not hidden.
    pub fn session_location_view(&self, session: &crate::session::Session) -> SessionLocationView {
        let catalog = self
            .status()
            .and_then(|status| Ok((status.revision, self.pending_location_changes(&session.id)?)));
        let (catalog_revision, pending, catalog_issue) = match catalog {
            Ok((revision, pending)) => (Some(revision), pending, None),
            Err(error) => (None, Vec::new(), Some(error)),
        };
        SessionLocationView {
            session: session.id.clone(),
            location: session
                .location
                .as_ref()
                .map(|location| SessionLocationState {
                    placement: location.placement,
                    cwd: location.cwd.observed_path().to_path_buf(),
                    initial_cwd: location.initial_cwd.clone(),
                    revision: location.revision,
                }),
            legacy_working_dir: session
                .location
                .is_none()
                .then(|| session.working_dir.as_ref().map(PathBuf::from))
                .flatten(),
            isolated_child: session.isolated_child.is_some(),
            pending,
            catalog_revision,
            catalog_issue,
        }
    }
}
