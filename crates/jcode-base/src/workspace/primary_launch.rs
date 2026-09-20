//! Idempotent primary creation intent and Session-derived publication receipts.
use super::*;
use rusqlite::{TransactionBehavior, params};

pub struct PrimaryLaunchLease {
    _catalog: CatalogLease,
    _file: std::fs::File,
}

impl WorkspaceService {
    pub fn primary_launch_lease(&self, request: RequestId) -> Result<PrimaryLaunchLease> {
        let catalog = self.lease(false)?;
        let file = storage::private_file(
            &self
                .root
                .join("leases")
                .join(format!("primary-{request}.lock")),
            false,
        )?;
        file.try_lock().map_err(|e| {
            issue(
                IssueCode::Busy,
                format!("Primary launch is owned by another caller: {e}"),
            )
        })?;
        Ok(PrimaryLaunchLease {
            _catalog: catalog,
            _file: file,
        })
    }

    /// Intent is durable before Session or working-directory effects. Request
    /// identity is scoped to this operation family, like named catalog backups.
    pub fn reserve_primary_launch(
        &self,
        request: RequestId,
        expected: Revision,
        input: PrimaryLaunchInput,
        concrete_model: PrimaryModel,
    ) -> Result<PrimaryLaunchRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(existing) = read_launch(&transaction, request)? {
            if existing.input != input {
                return Err(issue(
                    IssueCode::Conflict,
                    "Primary launch request already has different settings",
                ));
            }
            return Ok(existing);
        }
        let occupied: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1)",
                [request.to_string()],
                |row| row.get(0),
            )
            .map_err(io)?;
        if occupied {
            return Err(issue(
                IssueCode::Conflict,
                "Launch identity is already used by another operation",
            ));
        }
        organization::require_revision(&transaction, expected)?;
        if let PrimaryPlacement::Standalone { root } = &input.placement
            && !root.is_absolute()
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Standalone root must be absolute",
            ));
        }
        let cwd = input.cwd.as_ref().ok_or_else(|| {
            issue(
                IssueCode::NeedsCwd,
                "Choose a command working directory before launching",
            )
        })?;
        if !cwd.path().is_absolute() {
            return Err(issue(
                IssueCode::NeedsCwd,
                "Command working directory must be absolute",
            ));
        }
        if concrete_model.model.is_empty()
            || concrete_model.provider.is_empty()
            || concrete_model.api_method.is_empty()
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "A concrete execution route is required",
            ));
        }
        let record = PrimaryLaunchRecord {
            request,
            operation: request.to_string().parse().map_err(corrupt)?,
            session: crate::session::Session::create(None, None).id,
            input,
            concrete_model,
            registration_request: RequestId::new(),
            state: PrimaryLaunchState::Pending,
            reviewed_revision: storage::status(&transaction)?.revision,
            published_revision: None,
            issue: None,
            backup_pending: false,
        };
        transaction.execute("INSERT INTO operations(id,kind,state,body) VALUES(?1,'primary_launch','pending',?2)",params![record.operation.to_string(),encode(&record)?]).map_err(io)?;
        if let PrimaryPlacement::Existing { placement } = record.input.placement {
            query::validate_placement(&transaction, placement)?;
            transaction
                .execute(
                    "INSERT INTO operation_targets VALUES(?1,?2)",
                    params![
                        record.operation.to_string(),
                        query::placement_target(placement).to_string()
                    ],
                )
                .map_err(io)?;
        }
        transaction.commit().map_err(io)?;
        self.checkpoint("primary_intent")?;
        Ok(record)
    }

    pub fn inspect_primary_launch(&self, request: RequestId) -> Result<PrimaryLaunchRecord> {
        let _lease = self.lease(false)?;
        read_launch(&self.connection()?, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown primary launch request"))
    }

    pub fn record_primary_launch_failure(
        &self,
        request: RequestId,
        detail: String,
    ) -> Result<PrimaryLaunchRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut record = read_launch(&transaction, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown primary launch request"))?;
        if record.state == PrimaryLaunchState::Complete {
            return Ok(record);
        }
        let retained = crate::session::session_exists(&record.session);
        record.state = if retained {
            PrimaryLaunchState::RecoveryRequired
        } else {
            PrimaryLaunchState::Failed
        };
        record.issue = Some(detail);
        transaction
            .execute(
                "UPDATE operations SET state=?2,body=?3 WHERE id=?1",
                params![
                    record.operation.to_string(),
                    if retained {
                        "recovery_required"
                    } else {
                        "failed"
                    },
                    encode(&record)?
                ],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(record)
    }

    /// Reconstruct publication only from a committed, ready Session checkpoint.
    /// A caller cannot make a Session ready by submitting a catalog projection.
    pub fn reconcile_primary_launch(&self, request: RequestId) -> Result<PrimaryLaunchRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let record = read_launch(&connection, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown primary launch request"))?;
        if record.state == PrimaryLaunchState::Complete {
            return self.finish_primary_backup(record);
        }
        let session = crate::session::Session::load_startup_stub(&record.session)
            .map_err(|e| issue(IssueCode::RecoveryRequired, e.to_string()))?;
        let creation = session.primary_creation.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Session has no primary creation checkpoint",
            )
        })?;
        if creation.request != request || creation.operation != record.operation || !creation.ready
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Primary preparation has not committed its ready checkpoint",
            ));
        }
        let location = session
            .location
            .as_ref()
            .ok_or_else(|| corrupt("Ready primary has no location"))?;
        if location.last_operation != Some(record.operation) || session.id != record.session {
            return Err(issue(
                IssueCode::Conflict,
                "Session checkpoint belongs to another launch",
            ));
        }
        if let PrimaryPlacement::Existing { placement } = &record.input.placement
            && *placement != location.placement
        {
            return Err(issue(
                IssueCode::Conflict,
                "Session placement differs from the accepted launch",
            ));
        }
        if let PrimaryPlacement::Standalone { root } = &record.input.placement {
            let Placement::Standalone(id) = location.placement else {
                return Err(issue(
                    IssueCode::Conflict,
                    "Standalone launch has another placement kind",
                ));
            };
            let Entity::Location(target) = entity(&connection, EntityId::Location(id))? else {
                return Err(corrupt("Standalone identity changed"));
            };
            if target.observed_path != root.canonicalize().map_err(io)? {
                return Err(issue(
                    IssueCode::Conflict,
                    "Standalone root differs from launch intent",
                ));
            }
        }
        let expected = record
            .input
            .cwd
            .as_ref()
            .ok_or_else(|| corrupt("Launch lost its cwd choice"))?
            .path()
            .canonicalize()
            .map_err(io)?;
        if expected != location.cwd.observed_path() {
            return Err(issue(
                IssueCode::Conflict,
                "Session cwd differs from launch intent",
            ));
        }
        let prepared = self.prepare_primary_location(
            location.placement,
            Some(location.cwd.observed_path()),
            record.operation,
        )?;
        if prepared.location.cwd != location.cwd {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Session cwd binding changed before publication",
            ));
        }
        self.checkpoint("primary_before_index")?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut current = read_launch(&transaction, request)?
            .ok_or_else(|| corrupt("Primary launch disappeared"))?;
        if current.operation != record.operation || current.session != record.session {
            return Err(issue(
                IssueCode::Conflict,
                "Primary launch changed during reconciliation",
            ));
        }
        query::validate_placement(&transaction, location.placement)?;
        let index = SessionIndex {
            session: session.id.clone(),
            placement: location.placement,
            session_revision: location.revision,
            operation: record.operation,
            active: true,
            reconciled: true,
        };
        let previous: Option<String> = transaction
            .query_row(
                "SELECT body FROM session_index WHERE session=?1",
                [&session.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(io)?;
        if let Some(previous) = previous {
            let previous: SessionIndex = decode(&previous)?;
            if previous.session_revision > index.session_revision
                || (previous.session_revision == index.session_revision
                    && (previous.operation != index.operation
                        || previous.placement != index.placement))
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Newer Session index cannot be replaced by launch replay",
                ));
            }
        }
        if current.state == PrimaryLaunchState::Complete {
            return Ok(current);
        }
        transaction.execute("INSERT INTO session_index VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET target=excluded.target,body=excluded.body",params![session.id,query::placement_target(location.placement).to_string(),encode(&index)?]).map_err(io)?;
        current.published_revision = Some(
            storage::status(&transaction)?
                .revision
                .checked_add(1)
                .ok_or_else(|| corrupt("Catalog revision exhausted"))?,
        );
        current.state = PrimaryLaunchState::Complete;
        current.backup_pending = true;
        current.issue = None;
        transaction
            .execute(
                "UPDATE operations SET state='complete',body=?2 WHERE id=?1",
                params![current.operation.to_string(), encode(&current)?],
            )
            .map_err(io)?;
        transaction
            .execute("UPDATE catalog SET revision=revision+1", [])
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("primary_index_committed")?;
        self.finish_primary_backup(current)
    }

    fn finish_primary_backup(
        &self,
        mut record: PrimaryLaunchRecord,
    ) -> Result<PrimaryLaunchRecord> {
        if !record.backup_pending {
            return Ok(record);
        }
        match self.automatic_backup() {
            Ok(()) => {
                record.backup_pending = false;
                record.issue = None;
            }
            Err(error) => {
                record.issue = Some(format!(
                    "Primary was published, but automatic catalog backup failed: {error}"
                ))
            }
        }
        self.connection()?
            .execute(
                "UPDATE operations SET body=?2 WHERE id=?1",
                params![record.operation.to_string(), encode(&record)?],
            )
            .map_err(|error| {
                io(format!(
                    "Primary published; backup receipt could not be recorded: {error}"
                ))
            })?;
        Ok(record)
    }
}

fn read_launch(connection: &Connection, request: RequestId) -> Result<Option<PrimaryLaunchRecord>> {
    let value: Option<(String, String)> = connection
        .query_row(
            "SELECT body,state FROM operations WHERE kind='primary_launch' AND id=?1",
            [request.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    value
        .map(|(body, state)| {
            let mut record: PrimaryLaunchRecord = decode(&body)?;
            if record.request != request || record.operation.to_string() != request.to_string() {
                return Err(corrupt(
                    "Primary launch identity differs from its operation key",
                ));
            }
            // Restore updates the operation state without interpreting each producer's
            // payload. The payload's old state is never an execution authority.
            record.state = match state.as_str() {
                "pending" => PrimaryLaunchState::Pending,
                "complete" => PrimaryLaunchState::Complete,
                "failed" => PrimaryLaunchState::Failed,
                "recovery_required" => PrimaryLaunchState::RecoveryRequired,
                _ => return Err(corrupt("Unknown primary operation state")),
            };
            Ok(record)
        })
        .transpose()
}
