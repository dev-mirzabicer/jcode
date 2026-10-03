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
    /// Trusted-client admission. The no-loss declaration is agent-only.
    pub fn admit_closeout_action(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        spec: CloseoutActionSpec,
    ) -> Result<CloseoutActionRecord> {
        if matches!(spec.action, CloseoutAction::DeclareNoLoss { .. }) {
            return Err(issue(
                IssueCode::PermissionRequired,
                "A no-loss declaration belongs to the agent completing a conditional closeout; trusted clients approve removal instead",
            ));
        }
        self.admit_action(
            client.0.clone(),
            CloseoutActor::TrustedClient,
            request,
            spec,
        )
    }

    /// Agent admission for the caller's authoritative Session, never for
    /// model-supplied identity. Agents prepare, record dispositions, request
    /// the removal review, declare no loss under a human-issued conditional
    /// authorization and finish only that declared removal. They cannot
    /// approve, recover, begin or revoke a closeout.
    pub fn admit_agent_closeout_action(
        &self,
        session: &crate::session::Session,
        request: RequestId,
        mut spec: CloseoutActionSpec,
    ) -> Result<CloseoutActionRecord> {
        if session.isolated_child.is_some() {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Checkout closeout belongs to the original primary parent",
            ));
        }
        let placement = session.location.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Adopt the legacy Session before working on a checkout closeout",
            )
        })?;
        match &mut spec.action {
            CloseoutAction::Refresh
            | CloseoutAction::Preserve
            | CloseoutAction::ReviewRemoval
            | CloseoutAction::Finish => {}
            CloseoutAction::Disposition { decision } => {
                // Provenance is the acting Session, not a model-authored label.
                decision.recorded_by = session.id.clone();
            }
            CloseoutAction::DeclareNoLoss { assessment, .. } => {
                if assessment.trim().is_empty() {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "A no-loss declaration needs the agent's assessment",
                    ));
                }
            }
            CloseoutAction::ApproveRemoval { .. }
            | CloseoutAction::ReviewRecovery { .. }
            | CloseoutAction::ApplyRecovery { .. } => {
                return Err(issue(
                    IssueCode::PermissionRequired,
                    "Removal approval and closeout recovery require the human management client",
                ));
            }
        }
        {
            let _catalog = self.lease(false)?;
            let connection = self.connection()?;
            let stored = load(&connection, spec.operation)?;
            let scope = self.scope_snapshot_mode(&connection, &session.id, placement, false)?;
            if !scope
                .roots
                .iter()
                .any(|root| root.location.id == stored.record.spec.location)
            {
                return Err(issue(
                    IssueCode::PermissionRequired,
                    format!(
                        "Session {} has no write scope over location {}; closeout preparation needs the checkout in its scope",
                        session.id, stored.record.spec.location
                    ),
                ));
            }
            if matches!(spec.action, CloseoutAction::Finish) {
                require_own_declaration(&stored.record, &session.id)?;
            }
        }
        self.admit_action(session.id.clone(), CloseoutActor::Agent, request, spec)
    }

    fn admit_action(
        &self,
        initiated_by: String,
        actor: CloseoutActor,
        request: RequestId,
        spec: CloseoutActionSpec,
    ) -> Result<CloseoutActionRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let input = match actor {
            CloseoutActor::TrustedClient => digest(encode(&("closeout_action", &spec))?.as_bytes()),
            CloseoutActor::Agent => {
                digest(encode(&("closeout_agent_action", &initiated_by, &spec))?.as_bytes())
            }
        };
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
            initiated_by,
            actor,
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
            // Trusted-client inputs keep their original shape so existing run
            // identities still verify.
            input: match record.actor {
                CloseoutActor::TrustedClient => {
                    serde_json::to_value((&record.spec, &record.initiated_by))
                }
                CloseoutActor::Agent => {
                    serde_json::to_value((&record.spec, &record.initiated_by, "agent"))
                }
            }
            .map_err(io)?,
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
        let operation = record.spec.operation;
        let revision = record.spec.expected_revision;
        let human = matches!(
            record.spec.action,
            CloseoutAction::ApproveRemoval { .. }
                | CloseoutAction::ReviewRecovery { .. }
                | CloseoutAction::ApplyRecovery { .. }
        );
        match record.actor {
            CloseoutActor::Agent if human => {
                return Err(issue(
                    IssueCode::PermissionRequired,
                    "Removal approval and closeout recovery require the human management client",
                ));
            }
            CloseoutActor::TrustedClient
                if matches!(record.spec.action, CloseoutAction::DeclareNoLoss { .. }) =>
            {
                return Err(issue(
                    IssueCode::PermissionRequired,
                    "A no-loss declaration belongs to the agent completing a conditional closeout",
                ));
            }
            CloseoutActor::Agent if matches!(record.spec.action, CloseoutAction::Finish) => {
                require_own_declaration(&current, &record.initiated_by)?;
            }
            _ => {}
        }
        let client = WorkspaceClientAuthority::authenticated(record.initiated_by.clone())?;
        use CloseoutActionResult as Output;
        match record.spec.action {
            CloseoutAction::DeclareNoLoss { review, assessment } => self
                .declare_closeout_no_loss(
                    &record.initiated_by,
                    effect,
                    CloseoutReviewTarget { operation, review },
                    &assessment,
                    runtime,
                )
                .await
                .map(|v| Output::Record(Box::new(v))),
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

/// An agent finishes only a removal its own Session conditionally declared.
fn require_own_declaration(record: &CloseoutRecord, session: &str) -> Result<()> {
    match record.authorization.as_ref().map(|value| &value.source) {
        Some(CloseoutAuthorizationSource::Conditional {
            session: declared, ..
        }) if declared == session => Ok(()),
        _ => Err(issue(
            IssueCode::PermissionRequired,
            "Only the Session that declared no loss under a conditional authorization may finish this removal; a human-approved removal is finished by the human client",
        )),
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
