//! Immutable action correlation over the existing catalog and execution owners.
use super::*;
use crate::execution::Invocation;

pub const CLOSEOUT_EXECUTION_SESSION: &str = "workspace-operations";
pub const CLOSEOUT_EXECUTION_TOOL: &str = "workspace_closeout";

#[derive(Clone, Serialize, Deserialize)]
struct StoredAction {
    record: CloseoutActionRecord,
    effect: RequestId,
}

impl WorkspaceService {
    pub fn admit_closeout_action(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        spec: CloseoutActionSpec,
    ) -> Result<CloseoutActionRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let input = digest(encode(&("closeout_action", &spec))?.as_bytes());
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if organization::replay(&tx, request, &input)?.is_some() {
            return action_record(&tx, request);
        }
        let current = load(&tx, spec.operation)?;
        require_current(&tx, &current, spec.expected_revision)?;
        let receipt = organization::commit_receipt(
            &tx,
            request,
            &input,
            vec![EntityId::Location(current.record.spec.location)],
        )?;
        let mut record = CloseoutActionRecord {
            request,
            spec,
            initiated_by: client.0.clone(),
            run_id: String::new(),
            result: None,
            issue: None,
        };
        record.run_id = self.closeout_action_invocation(&record)?.id();
        // The closeout itself owns the physical target. This immutable action
        // reference is not another active physical owner in operation_targets.
        tx.execute(
            "INSERT INTO operations(id,kind,state,body) VALUES(?1,'closeout_action','pending',?2)",
            params![
                receipt.operation.to_string(),
                encode(&StoredAction {
                    record: record.clone(),
                    effect: RequestId::new()
                })?
            ],
        )
        .map_err(io)?;
        tx.commit().map_err(io)?;
        self.automatic_backup().map_err(|error| issue(IssueCode::BackupFailed, format!("Closeout action {request} was admitted, but backup failed: {error}. Inspect or retry that exact request, not a new action.")))?;
        Ok(record)
    }

    pub fn inspect_closeout_action(&self, request: RequestId) -> Result<CloseoutActionRecord> {
        let _catalog = self.lease(false)?;
        action_record(&self.connection()?, request)
    }

    pub fn closeout_action_invocation(&self, record: &CloseoutActionRecord) -> Result<Invocation> {
        Ok(Invocation {
            session_id: CLOSEOUT_EXECUTION_SESSION.into(),
            message_id: record.spec.operation.to_string(),
            call_path: vec![record.request.to_string()],
            tool: CLOSEOUT_EXECUTION_TOOL.into(),
            input: serde_json::to_value((&record.spec, &record.initiated_by)).map_err(io)?,
            working_dir: Some(self.root.clone()),
            received_result_digest: None,
        })
    }

    /// Called by the owning producer after its domain operation has stopped.
    /// This records the domain outcome, not execution/output terminal state.
    pub fn record_closeout_action_result(
        &self,
        request: RequestId,
        outcome: std::result::Result<CloseoutActionResult, Issue>,
    ) -> Result<CloseoutActionRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut stored = action_state(&tx, request)?;
        let record = &mut stored.record;
        let (result, problem) = match outcome {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        if record.result.is_some() || record.issue.is_some() {
            if record.result != result || record.issue != problem {
                return Err(issue(
                    IssueCode::Conflict,
                    "Action already has a different immutable result",
                ));
            }
            return Ok(record.clone());
        }
        record.result = result;
        record.issue = problem;
        let state = if record.issue.is_some() {
            "failed"
        } else {
            "complete"
        };
        tx.execute("UPDATE operations SET state=?2,body=?3 WHERE id=(SELECT json_extract(body,'$.operation') FROM receipts WHERE request=?1) AND kind='closeout_action'", params![request.to_string(), state, encode(&stored)?]).map_err(io)?;
        tx.commit().map_err(io)?;
        self.automatic_backup().map_err(|error| issue(IssueCode::BackupFailed, format!("Closeout action {request} has a committed domain receipt, but backup failed: {error}. Its effects were not rolled back.")))?;
        Ok(stored.record)
    }

    #[cfg(target_os = "macos")]
    pub async fn execute_closeout_action(
        &self,
        request: RequestId,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutActionResult> {
        let stored = action_state(&self.connection()?, request)?;
        let effect = stored.effect;
        let record = stored.record;
        if record.result.is_some() || record.issue.is_some() {
            return Err(issue(
                IssueCode::Conflict,
                "Action was already performed; inspect its receipt",
            ));
        }
        if runtime.capture.reference().map_err(io)?.invocation_id != record.run_id {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Closeout action belongs to another execution",
            ));
        }
        runtime.check_stop()?;
        let current = self.inspect_closeout(record.spec.operation)?;
        if current.revision != record.spec.expected_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Closeout changed after action admission",
            ));
        }
        let client = WorkspaceClientAuthority::authenticated(record.initiated_by)?;
        let operation = record.spec.operation;
        let revision = record.spec.expected_revision;
        use CloseoutActionResult as Output;
        match record.spec.action {
            CloseoutAction::Refresh => self
                .refresh_closeout_in(operation, revision, runtime.session_root, runtime.capture)
                .await
                .map(|v| Output::Record(Box::new(v))),
            CloseoutAction::Preserve => self
                .preserve_closeout(operation, revision, runtime.capture)
                .await
                .map(|v| Output::Record(Box::new(v))),
            CloseoutAction::Disposition { decision } => self
                .record_closeout_disposition(operation, revision, decision)
                .map(|v| Output::Record(Box::new(v))),
            CloseoutAction::ReviewRemoval => self
                .review_closeout_removal(operation, revision, runtime)
                .await
                .map(|v| Output::Review(Box::new(v))),
            CloseoutAction::ApproveRemoval { review } => self
                .approve_closeout_removal(
                    &client,
                    effect,
                    CloseoutReviewTarget { operation, review },
                    runtime,
                )
                .await
                .map(|v| Output::Record(Box::new(v))),
            CloseoutAction::Finish => self
                .finish_closeout(operation, runtime)
                .await
                .map(|v| Output::Record(Box::new(v))),
            CloseoutAction::ReviewRecovery { choice } => self
                .review_closeout_recovery(&client, operation, revision, choice, runtime)
                .await
                .map(|v| Output::Recovery(Box::new(v))),
            CloseoutAction::ApplyRecovery { review } => self
                .apply_closeout_recovery(
                    &client,
                    effect,
                    CloseoutReviewTarget { operation, review },
                    runtime,
                )
                .await
                .map(|v| Output::Record(Box::new(v))),
        }
    }
}

fn action_record(connection: &Connection, request: RequestId) -> Result<CloseoutActionRecord> {
    Ok(action_state(connection, request)?.record)
}

fn action_state(connection: &Connection, request: RequestId) -> Result<StoredAction> {
    let body: Option<String> = connection.query_row("SELECT o.body FROM receipts r JOIN operations o ON o.id=json_extract(r.body,'$.operation') WHERE r.request=?1 AND o.kind='closeout_action'", [request.to_string()], |row| row.get(0)).optional().map_err(io)?;
    let stored: StoredAction = decode(&body.ok_or_else(|| {
        issue(
            IssueCode::InvalidIdentity,
            "Unknown closeout action request",
        )
    })?)?;
    if stored.record.request != request {
        return Err(corrupt("Closeout action identity mismatch"));
    }
    Ok(stored)
}
