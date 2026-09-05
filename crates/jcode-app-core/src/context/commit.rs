use crate::agent::Agent;
use crate::context::draft::{
    ContextTransactionService, DraftEntryState, projection_validation_operations,
    state_with_transaction, validate_capture_identity, validate_capture_identity_parts,
};
use crate::context::history::summarize_context_transaction;
use crate::context::provider_validation::require_supported_projected_messages;
use crate::protocol::{
    ContextDraft, ContextServiceError, ContextTransactionResult, ContextTransactionSummary,
};
use crate::provider::ContextProjectionValidationReport;
use crate::provider::Provider;
use crate::session::Session;
use anyhow::Result;
use chrono::Utc;
use jcode_context_core::{
    ContextEconomicsInput, analyze_cache_prefix, calculate_context_economics, project_context,
    validate_context_state,
};
use jcode_session_types::{
    StoredContextApplication, StoredContextAuthorization, StoredContextCacheWarmth,
    StoredContextEmergencyAudit, StoredContextEmergencyOperationKind, StoredContextEmergencyPolicy,
    StoredContextEmergencyRetryOutcome, StoredContextEmergencyTriggerKind, StoredContextOperation,
    StoredContextTransactionStatusKind, StoredContextViewState, StoredProviderValidationEvidence,
    StoredProviderValidationOutcome,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// Narrow persistence boundary used by context transactions.
///
/// Production delegates to `Session::save`. Tests inject deterministic failures
/// so rollback is exercised after the in-memory context state has changed.
pub trait ContextPersistence: Send + Sync {
    fn persist(&self, agent: &mut Agent) -> Result<()>;
}

/// Narrow persistence boundary for local TUI transitions that own `Session`
/// directly rather than through an `Agent` lock.
pub trait DirectContextSessionPersistence: Send + Sync {
    fn persist(&self, session: &mut Session) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SessionContextPersistence;

impl ContextPersistence for SessionContextPersistence {
    fn persist(&self, agent: &mut Agent) -> Result<()> {
        agent.persist_context_session()
    }
}

#[derive(Debug, Default)]
pub struct DirectSessionContextPersistence;

impl DirectContextSessionPersistence for DirectSessionContextPersistence {
    fn persist(&self, session: &mut Session) -> Result<()> {
        session.save()
    }
}

pub(crate) struct PreparedContextTransition {
    pub state: StoredContextViewState,
    pub result: ContextTransactionResult,
    pub invalidation_detail: String,
}

#[derive(Debug)]
pub struct ContextSessionTransition {
    pub result: ContextTransactionResult,
    pub invalidation_detail: String,
}

impl ContextTransactionService {
    pub(crate) fn preview_unattended_emergency_operations(
        &self,
        agent: &Agent,
        transaction_id: &str,
        authorization: StoredContextAuthorization,
        operations: Vec<StoredContextOperation>,
    ) -> Result<jcode_session_types::StoredContextEconomics, ContextServiceError> {
        if operations.is_empty() {
            return Err(ContextServiceError::EmptyRequest);
        }
        let previous_state = agent.context_view_state().clone();
        let revision = previous_state
            .revision
            .checked_add(1)
            .ok_or(ContextServiceError::RevisionOverflow)?;
        let proposed_state = super::state_with_transaction_and_audit(
            &previous_state,
            transaction_id,
            revision,
            authorization,
            operations,
            None,
            Vec::new(),
            None,
        );
        let transaction_index = proposed_state.transactions.len().saturating_sub(1);
        let prepared = prepare_context_transition(
            agent,
            &previous_state,
            proposed_state,
            transaction_index,
            true,
        )?;
        prepared.state.transactions[transaction_index]
            .economics
            .clone()
            .ok_or_else(|| {
                ContextServiceError::Runtime(
                    "emergency operation preview produced no economics".to_string(),
                )
            })
    }

    pub fn set_emergency_policy_for_session(
        &self,
        session: &mut Session,
        policy: StoredContextEmergencyPolicy,
        processing: bool,
    ) -> Result<(String, StoredContextEmergencyPolicy), ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        validate_emergency_policy(&policy)?;
        let session_id = session.id.clone();
        if session.context_view.emergency_policy == policy {
            return Ok((session_id, policy));
        }
        let previous_state = session.context_view.clone();
        session.context_view.emergency_policy = policy.clone();
        if let Err(error) = self.direct_session_persistence.persist(session) {
            session.context_view = previous_state;
            return Err(ContextServiceError::Persistence(error.to_string()));
        }
        Ok((session_id, policy))
    }

    pub(crate) fn apply_unattended_emergency_operations(
        &self,
        agent: &mut Agent,
        transaction_id: &str,
        authorization: StoredContextAuthorization,
        operations: Vec<StoredContextOperation>,
        curator_usage: Vec<jcode_session_types::StoredContextCuratorUsage>,
        emergency_audit: jcode_session_types::StoredContextEmergencyAudit,
    ) -> Result<ContextTransactionResult, ContextServiceError> {
        if operations.is_empty() {
            return Err(ContextServiceError::EmptyRequest);
        }
        validate_unattended_emergency_transaction(&authorization, &operations, &emergency_audit)?;
        let previous_state = agent.context_view_state().clone();
        let revision = previous_state
            .revision
            .checked_add(1)
            .ok_or(ContextServiceError::RevisionOverflow)?;
        let proposed_state = super::state_with_transaction_and_audit(
            &previous_state,
            transaction_id,
            revision,
            authorization,
            operations,
            None,
            curator_usage,
            Some(emergency_audit),
        );
        let transaction_index = proposed_state.transactions.len().saturating_sub(1);
        let mut prepared = prepare_context_transition(
            agent,
            &previous_state,
            proposed_state,
            transaction_index,
            true,
        )?;
        if let Some(transaction) = prepared.state.transactions.get_mut(transaction_index)
            && let Some(audit) = transaction.emergency_audit.as_mut()
        {
            audit.achieved_reduction_tokens = transaction
                .economics
                .as_ref()
                .map(|economics| economics.deleted_input_tokens)
                .unwrap_or_default();
        }
        self.persist_prepared_transition(agent, previous_state, prepared)
    }

    pub fn set_emergency_policy(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
        policy: StoredContextEmergencyPolicy,
        processing: bool,
    ) -> Result<(String, StoredContextEmergencyPolicy), ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        validate_emergency_policy(&policy)?;
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let session_id = agent.session_id().to_string();
        if agent.context_view_state().emergency_policy == policy {
            return Ok((session_id, policy));
        }
        let previous_state = agent.context_view_state().clone();
        let mut proposed_state = previous_state.clone();
        proposed_state.emergency_policy = policy.clone();
        agent.replace_context_view_state(proposed_state);
        if let Err(error) = self.persistence.persist(&mut agent) {
            agent.replace_context_view_state(previous_state);
            return Err(ContextServiceError::Persistence(error.to_string()));
        }
        Ok((session_id, policy))
    }

    /// Apply one ready draft as one persisted context revision.
    ///
    /// `selected_distillation_ids == None` applies every curator proposal selected by default.
    /// `Some(ids)` applies exactly that validated subset without another curator request.
    pub fn apply_draft(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
        draft_id: &str,
        selected_distillation_ids: Option<Vec<String>>,
        processing: bool,
    ) -> Result<ContextTransactionResult, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let draft = self.reserve_ready_draft(draft_id, agent.session_id())?;

        if let Err(error) = agent.validate_active_agent_profile() {
            let error = ContextServiceError::Stale(error.to_string());
            self.fail_applying_draft(draft_id, error.clone());
            return Err(error);
        }
        let selected_distillations =
            match selected_distillation_operations(&draft, selected_distillation_ids.as_deref()) {
                Ok(operations) => operations,
                Err(error) => {
                    self.restore_applying_draft(draft_id);
                    return Err(error);
                }
            };
        if let Err(error) = validate_capture_identity(&agent, &draft.identity) {
            self.fail_applying_draft(draft_id, error.clone());
            return Err(error);
        }
        if let Err(error) = validate_active_profile_draft(
            agent.messages(),
            agent.active_transition_message_id(),
            &draft,
            &selected_distillations,
        ) {
            self.fail_applying_draft(draft_id, error.clone());
            return Err(error);
        }

        let revision = agent
            .context_view_state()
            .revision
            .checked_add(1)
            .ok_or_else(|| {
                self.fail_applying_draft(draft_id, ContextServiceError::RevisionOverflow);
                ContextServiceError::RevisionOverflow
            })?;
        let mut operations = draft.required_operations.clone();
        operations.extend(selected_distillations);
        if operations.is_empty() {
            self.restore_applying_draft(draft_id);
            return Err(ContextServiceError::EmptyRequest);
        }
        let proposed_state = state_with_transaction(
            agent.context_view_state(),
            &draft.identity.draft_id,
            revision,
            draft.authorization.clone(),
            operations,
            None,
            draft.curator_usage.clone(),
        );
        let transaction_index = proposed_state.transactions.len().saturating_sub(1);
        let previous_state = agent.context_view_state().clone();
        let prepared = match prepare_context_transition(
            &agent,
            &previous_state,
            proposed_state,
            transaction_index,
            true,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_applying_draft(draft_id, error.clone());
                return Err(error);
            }
        };
        let result = match self.persist_prepared_transition(&mut agent, previous_state, prepared) {
            Ok(result) => result,
            Err(error) => {
                self.restore_applying_draft(draft_id);
                return Err(error);
            }
        };
        self.mark_draft_applied(draft_id, &result);
        Ok(result)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "local apply revalidates every independent provider-context identity dimension"
    )]
    pub fn apply_draft_to_session(
        &self,
        session: &mut Session,
        provider: &dyn Provider,
        route: &str,
        estimated_total_request_tokens_before: Option<usize>,
        draft_id: &str,
        selected_distillation_ids: Option<Vec<String>>,
        processing: bool,
    ) -> Result<ContextSessionTransition, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        let draft = self.reserve_ready_draft(draft_id, &session.id)?;
        let selected_distillations =
            match selected_distillation_operations(&draft, selected_distillation_ids.as_deref()) {
                Ok(operations) => operations,
                Err(error) => {
                    self.restore_applying_draft(draft_id);
                    return Err(error);
                }
            };
        if let Err(error) = validate_capture_identity_parts(
            &session.id,
            &session.messages,
            &session.context_view,
            provider,
            route,
            &draft.identity,
        ) {
            self.fail_applying_draft(draft_id, error.clone());
            return Err(error);
        }
        session.validate_active_agent_profile().map_err(|error| {
            let error = ContextServiceError::Stale(error.to_string());
            self.fail_applying_draft(draft_id, error.clone());
            error
        })?;
        if let Err(error) = validate_active_profile_draft(
            &session.messages,
            session.active_transition_message_id(),
            &draft,
            &selected_distillations,
        ) {
            self.fail_applying_draft(draft_id, error.clone());
            return Err(error);
        }
        let revision = session
            .context_view
            .revision
            .checked_add(1)
            .ok_or_else(|| {
                self.fail_applying_draft(draft_id, ContextServiceError::RevisionOverflow);
                ContextServiceError::RevisionOverflow
            })?;
        let mut operations = draft.required_operations.clone();
        operations.extend(selected_distillations);
        if operations.is_empty() {
            self.restore_applying_draft(draft_id);
            return Err(ContextServiceError::EmptyRequest);
        }
        let proposed_state = state_with_transaction(
            &session.context_view,
            &draft.identity.draft_id,
            revision,
            draft.authorization.clone(),
            operations,
            None,
            draft.curator_usage.clone(),
        );
        let transaction_index = proposed_state.transactions.len().saturating_sub(1);
        let previous_state = session.context_view.clone();
        let previous_provider_session_id = session.provider_session_id.clone();
        let prepared = match prepare_context_transition_for_session(
            provider,
            &session.messages,
            &previous_state,
            proposed_state,
            transaction_index,
            true,
            route,
            estimated_total_request_tokens_before,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_applying_draft(draft_id, error.clone());
                return Err(error);
            }
        };
        session.context_view = prepared.state;
        session.provider_session_id = None;
        if let Err(error) = self.direct_session_persistence.persist(session) {
            session.context_view = previous_state;
            session.provider_session_id = previous_provider_session_id;
            self.restore_applying_draft(draft_id);
            return Err(ContextServiceError::Persistence(error.to_string()));
        }
        self.mark_draft_applied(draft_id, &prepared.result);
        Ok(ContextSessionTransition {
            result: prepared.result,
            invalidation_detail: prepared.invalidation_detail,
        })
    }

    pub(crate) fn persist_prepared_transition(
        &self,
        agent: &mut Agent,
        previous_state: StoredContextViewState,
        prepared: PreparedContextTransition,
    ) -> Result<ContextTransactionResult, ContextServiceError> {
        agent.replace_context_view_state(prepared.state);
        if let Err(error) = self.persistence.persist(agent) {
            agent.replace_context_view_state(previous_state);
            return Err(ContextServiceError::Persistence(error.to_string()));
        }

        let mut result = prepared.result;
        if let Err(error) = agent.after_provider_context_changed(
            "context transaction",
            prepared.invalidation_detail,
            true,
        ) {
            result.warnings.push(format!(
                "The context transaction was persisted, but post-commit context-budget reseeding failed: {error}"
            ));
        }
        Ok(result)
    }

    fn reserve_ready_draft(
        &self,
        draft_id: &str,
        expected_session_id: &str,
    ) -> Result<ContextDraft, ContextServiceError> {
        let mut store = self.lock_store();
        store.expire_entries(Utc::now());
        let entry = store
            .entries
            .get_mut(draft_id)
            .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?;
        if entry.identity.session_id != expected_session_id {
            // Draft IDs are process-wide. Treat cross-session lookup as absent so
            // one connected session cannot inspect or mutate another session's
            // retained draft, even if an ID is disclosed accidentally.
            return Err(ContextServiceError::DraftNotFound(draft_id.to_string()));
        }
        let draft = match &entry.state {
            DraftEntryState::Ready(draft) => draft.clone(),
            DraftEntryState::Applying(_) => {
                return Err(ContextServiceError::DraftNotReady(draft_id.to_string()));
            }
            DraftEntryState::Applied { .. } => {
                return Err(ContextServiceError::DraftAlreadyApplied(
                    draft_id.to_string(),
                ));
            }
            DraftEntryState::Canceled => {
                return Err(ContextServiceError::DraftCanceled(draft_id.to_string()));
            }
            DraftEntryState::Expired => {
                return Err(ContextServiceError::DraftExpired(draft_id.to_string()));
            }
            DraftEntryState::Preparing | DraftEntryState::Failed(_) => {
                return Err(ContextServiceError::DraftNotReady(draft_id.to_string()));
            }
        };
        entry.state = DraftEntryState::Applying(draft.clone());
        entry.notify.notify_waiters();
        Ok(draft)
    }

    fn restore_applying_draft(&self, draft_id: &str) {
        let mut store = self.lock_store();
        let Some(entry) = store.entries.get_mut(draft_id) else {
            return;
        };
        let DraftEntryState::Applying(draft) = &entry.state else {
            return;
        };
        entry.state = DraftEntryState::Ready(draft.clone());
        entry.notify.notify_waiters();
    }

    fn fail_applying_draft(&self, draft_id: &str, error: ContextServiceError) {
        let mut store = self.lock_store();
        let Some(entry) = store.entries.get_mut(draft_id) else {
            return;
        };
        if matches!(entry.state, DraftEntryState::Applying(_)) {
            entry.state = DraftEntryState::Failed(error);
            entry.generation_in_flight = false;
            entry.refresh_terminal_reservation();
            entry.notify.notify_waiters();
        }
        store.enforce_total_bytes(self.limits.max_total_bytes);
    }

    fn mark_draft_applied(&self, draft_id: &str, result: &ContextTransactionResult) {
        let mut store = self.lock_store();
        let Some(entry) = store.entries.get_mut(draft_id) else {
            return;
        };
        if matches!(entry.state, DraftEntryState::Applying(_)) {
            entry.state = DraftEntryState::Applied {
                transaction_id: result.transaction.id.clone(),
                revision: result.revision,
            };
            entry.generation_in_flight = false;
            entry.refresh_terminal_reservation();
            entry.notify.notify_waiters();
        }
        store.enforce_total_bytes(self.limits.max_total_bytes);
    }
}

fn validate_active_profile_draft(
    messages: &[crate::session::StoredMessage],
    current_active_message_id: Option<&str>,
    draft: &ContextDraft,
    selected_distillations: &[StoredContextOperation],
) -> Result<(), ContextServiceError> {
    if draft.active_agent_profile_message_id.as_deref() != current_active_message_id {
        return Err(ContextServiceError::Stale(
            "active agent profile changed after context draft capture".to_string(),
        ));
    }
    let Some(active_message_id) = current_active_message_id else {
        return Ok(());
    };
    let active_index = messages
        .iter()
        .position(|message| message.id == active_message_id)
        .ok_or_else(|| {
            ContextServiceError::Stale(format!(
                "active agent profile message {active_message_id} is missing from authoritative history"
            ))
        })?;
    for operation in draft
        .required_operations
        .iter()
        .chain(selected_distillations)
    {
        if let StoredContextOperation::RangeSummary(summary) = operation {
            let start = messages
                .iter()
                .position(|message| message.id == summary.source_range.start_message_id)
                .ok_or_else(|| {
                    ContextServiceError::Stale(
                        "context draft range start disappeared before apply".to_string(),
                    )
                })?;
            let end = messages
                .iter()
                .position(|message| message.id == summary.source_range.end_message_id)
                .ok_or_else(|| {
                    ContextServiceError::Stale(
                        "context draft range end disappeared before apply".to_string(),
                    )
                })?;
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            if start <= active_index && active_index <= end {
                return Err(ContextServiceError::Conflict(format!(
                    "context draft range includes active agent profile message {active_message_id}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_unattended_emergency_transaction(
    authorization: &StoredContextAuthorization,
    operations: &[StoredContextOperation],
    audit: &StoredContextEmergencyAudit,
) -> Result<(), ContextServiceError> {
    validate_emergency_policy(&audit.policy)?;
    let StoredContextAuthorization::UnattendedEmergency {
        authorization_source,
        trigger,
        scheduled_item_id,
    } = authorization
    else {
        return Err(ContextServiceError::InvalidSelection(
            "emergency audit requires unattended-emergency authorization".to_string(),
        ));
    };
    if authorization_source != &audit.authorization_source
        || scheduled_item_id != &audit.scheduled_item_id
    {
        return Err(ContextServiceError::InvalidSelection(
            "emergency authorization and audit provenance do not match".to_string(),
        ));
    }
    let source = authorization_source.trim();
    if source.is_empty() || source.chars().count() > 512 {
        return Err(ContextServiceError::InvalidSelection(
            "emergency authorization source must contain 1 through 512 characters".to_string(),
        ));
    }
    if let Some(item_id) = scheduled_item_id.as_deref() {
        let item_id = item_id.trim();
        if item_id.is_empty()
            || item_id.chars().count() > 128
            || source != format!("scheduled_item:{item_id}")
        {
            return Err(ContextServiceError::InvalidSelection(
                "scheduled emergency provenance is malformed".to_string(),
            ));
        }
    }
    let expected_trigger = match audit.trigger_kind {
        StoredContextEmergencyTriggerKind::PreflightLimit => "preflight_limit",
        StoredContextEmergencyTriggerKind::ProviderContextLimit => "provider_context_limit",
    };
    if trigger.as_deref() != Some(expected_trigger) {
        return Err(ContextServiceError::InvalidSelection(
            "emergency authorization and audit trigger do not match".to_string(),
        ));
    }
    match (audit.trigger_kind, audit.provider_error.as_deref()) {
        (StoredContextEmergencyTriggerKind::PreflightLimit, None) => {}
        (StoredContextEmergencyTriggerKind::ProviderContextLimit, Some(error))
            if !error.trim().is_empty() && error.chars().count() <= 512 => {}
        _ => {
            return Err(ContextServiceError::InvalidSelection(
                "emergency provider-error provenance does not match its trigger".to_string(),
            ));
        }
    }
    if audit.retry_outcome != StoredContextEmergencyRetryOutcome::Pending
        || audit.achieved_reduction_tokens != 0
    {
        return Err(ContextServiceError::InvalidSelection(
            "new emergency audit must begin pending with zero achieved reduction".to_string(),
        ));
    }
    let mut expected_order = Vec::new();
    for kind in [
        StoredContextEmergencyOperationKind::ReasoningSuppression,
        StoredContextEmergencyOperationKind::ToolResultDistillation,
        StoredContextEmergencyOperationKind::OldestRangeSummary,
    ] {
        let present = operations.iter().any(|operation| {
            matches!(
                (kind, operation),
                (
                    StoredContextEmergencyOperationKind::ReasoningSuppression,
                    StoredContextOperation::ReasoningSuppression(_)
                ) | (
                    StoredContextEmergencyOperationKind::ToolResultDistillation,
                    StoredContextOperation::ToolResultDistillation(_)
                ) | (
                    StoredContextEmergencyOperationKind::OldestRangeSummary,
                    StoredContextOperation::RangeSummary(_)
                )
            )
        });
        if present {
            expected_order.push(kind);
        }
    }
    if audit.operation_order != expected_order {
        return Err(ContextServiceError::InvalidSelection(
            "emergency audit operation order does not match the transaction".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_emergency_policy(
    policy: &StoredContextEmergencyPolicy,
) -> Result<(), ContextServiceError> {
    let StoredContextEmergencyPolicy::Authorized {
        protected_recent_assistant_turns,
        target_headroom_percent,
        allow_reasoning_suppression,
        allow_tool_distillation,
        allow_oldest_range_summary,
        authorization_source,
    } = policy
    else {
        return Ok(());
    };
    if *protected_recent_assistant_turns > 1_000 {
        return Err(ContextServiceError::InvalidSelection(
            "protected recent assistant turns cannot exceed 1000".to_string(),
        ));
    }
    if !(1..=99).contains(target_headroom_percent) {
        return Err(ContextServiceError::InvalidSelection(
            "target headroom percent must be between 1 and 99".to_string(),
        ));
    }
    if !(*allow_reasoning_suppression || *allow_tool_distillation || *allow_oldest_range_summary) {
        return Err(ContextServiceError::InvalidSelection(
            "an authorized emergency policy must allow at least one operation".to_string(),
        ));
    }
    let authorization_source = authorization_source.trim();
    if authorization_source.is_empty() || authorization_source.chars().count() > 512 {
        return Err(ContextServiceError::InvalidSelection(
            "authorization source must contain between 1 and 512 characters".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn prepare_context_transition(
    agent: &Agent,
    previous_state: &StoredContextViewState,
    proposed_state: StoredContextViewState,
    economics_transaction_index: usize,
    update_application_identity: bool,
) -> Result<PreparedContextTransition, ContextServiceError> {
    let provider = agent.provider_handle();
    prepare_context_transition_for_session(
        provider.as_ref(),
        agent.messages(),
        previous_state,
        proposed_state,
        economics_transaction_index,
        update_application_identity,
        &agent.context_route_identity(),
        agent.current_context_request_token_estimate(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "transition validation and economics require exact provider, transcript, route, and request estimate inputs"
)]
pub(crate) fn prepare_context_transition_for_session(
    provider: &dyn Provider,
    messages: &[jcode_session_types::StoredMessage],
    previous_state: &StoredContextViewState,
    mut proposed_state: StoredContextViewState,
    economics_transaction_index: usize,
    update_application_identity: bool,
    route: &str,
    estimated_total_request_tokens_before: Option<usize>,
) -> Result<PreparedContextTransition, ContextServiceError> {
    validate_context_state(&proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let before = project_context(messages, previous_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let after = project_context(messages, &proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let validation_operations = projection_validation_operations(&proposed_state);
    let validation =
        require_supported_projected_messages(provider, &after.messages, &validation_operations)
            .map_err(|error| ContextServiceError::ProviderValidation(error.to_string()))?;
    let pricing = crate::provider::pricing::context_pricing_snapshot(
        &provider.model(),
        &provider.display_name(),
        route,
        StoredContextCacheWarmth::Unknown,
    );
    let analysis = analyze_cache_prefix(&before.messages, &after.messages);
    let estimated_total_request_tokens_after = estimated_total_request_tokens_before
        .and_then(|before| before.checked_sub(analysis.old_total_tokens))
        .map(|non_message_tokens| non_message_tokens.saturating_add(analysis.new_total_tokens));
    let economics = calculate_context_economics(ContextEconomicsInput {
        analysis: &analysis,
        estimated_total_request_tokens_before,
        estimated_total_request_tokens_after,
        context_window: Some(provider.context_window()),
        safe_input_budget: None,
        pricing: Some(&pricing),
        resulting_suffix_cacheable: after.diagnostics.projected_provider_token_estimate >= 1_024,
    });

    let application = StoredContextApplication {
        provider: provider.name().to_string(),
        model: provider.model(),
        route: route.to_string(),
        context_window: Some(provider.context_window()),
    };
    let transaction = proposed_state
        .transactions
        .get_mut(economics_transaction_index)
        .ok_or_else(|| {
            ContextServiceError::Projection(format!(
                "transition transaction index {economics_transaction_index} is missing"
            ))
        })?;
    transaction.economics = Some(economics);
    if update_application_identity {
        transaction.application = Some(application);
    }
    record_provider_validation_evidence(&mut proposed_state, &validation);
    validate_context_state(&proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;

    let (summary, revision, status) = {
        let transaction = proposed_state
            .transactions
            .get(economics_transaction_index)
            .expect("validated transaction index must remain present");
        let latest_status = transaction.latest_status().ok_or_else(|| {
            ContextServiceError::Projection(format!(
                "context transaction {} has no status event",
                transaction.id
            ))
        })?;
        (
            summarize_context_transaction(transaction),
            latest_status.revision,
            latest_status.kind,
        )
    };
    let invalidation_detail = context_invalidation_detail(&summary, status);
    Ok(PreparedContextTransition {
        state: proposed_state,
        result: ContextTransactionResult {
            transaction: summary,
            revision,
            status,
            warnings: Vec::new(),
        },
        invalidation_detail,
    })
}

pub(crate) fn selected_distillation_operations(
    draft: &ContextDraft,
    selected_ids: Option<&[String]>,
) -> Result<Vec<StoredContextOperation>, ContextServiceError> {
    let selected = match selected_ids {
        Some(ids) => {
            let mut unique = BTreeSet::new();
            for id in ids {
                if !unique.insert(id.as_str()) {
                    return Err(ContextServiceError::InvalidSelection(format!(
                        "distillation proposal was selected more than once: {id}"
                    )));
                }
            }
            unique
        }
        None => draft
            .distillation_proposals
            .iter()
            .filter(|proposal| proposal.selected_by_default)
            .map(|proposal| proposal.proposal_id.as_str())
            .collect(),
    };
    let known = draft
        .distillation_proposals
        .iter()
        .map(|proposal| proposal.proposal_id.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(unknown) = selected.difference(&known).next() {
        return Err(ContextServiceError::InvalidSelection(format!(
            "unknown distillation proposal: {unknown}"
        )));
    }
    Ok(draft
        .distillation_proposals
        .iter()
        .filter(|proposal| selected.contains(proposal.proposal_id.as_str()))
        .map(|proposal| StoredContextOperation::ToolResultDistillation(proposal.operation.clone()))
        .collect())
}

fn record_provider_validation_evidence(
    state: &mut StoredContextViewState,
    report: &ContextProjectionValidationReport,
) {
    let mut warnings = report
        .normalization_notes
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if report.formatter_placeholder_count > 0 {
        warnings.insert(format!(
            "The production request builder inserted {} formatter placeholder(s).",
            report.formatter_placeholder_count
        ));
    }
    let evidence = StoredProviderValidationEvidence {
        provider: report.provider_name.clone(),
        model: report.model.clone(),
        request_builder: report.evidence_tag.clone(),
        checked_at: Utc::now(),
        outcome: StoredProviderValidationOutcome::Passed,
        warnings: warnings.into_iter().collect(),
    };
    for transaction in &mut state.transactions {
        if !transaction.is_active() {
            continue;
        }
        for operation in &mut transaction.operations {
            let StoredContextOperation::ReasoningSuppression(suppression) = operation else {
                continue;
            };
            suppression.validation.retain(|existing| {
                existing.provider != evidence.provider
                    || existing.model != evidence.model
                    || existing.request_builder != evidence.request_builder
            });
            suppression.validation.push(evidence.clone());
        }
    }
}

fn context_invalidation_detail(
    transaction: &ContextTransactionSummary,
    status: StoredContextTransactionStatusKind,
) -> String {
    format!(
        "context transaction {} {:?} at revision {} (range_summaries={}, reasoning_suppressions={}, tool_result_distillations={}, earliest_changed_provider_item={})",
        transaction.id,
        status,
        transaction.latest_status_revision.unwrap_or_default(),
        transaction.operation_counts.range_summaries,
        transaction.operation_counts.reasoning_suppressions,
        transaction.operation_counts.tool_result_distillations,
        transaction
            .economics
            .as_ref()
            .and_then(|economics| economics.earliest_changed_provider_item)
            .map_or_else(|| "none".to_string(), |item| item.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::draft::{
        ContextDraftEntry, ContextDraftPreviewInput, ContextServiceLimits, build_preview,
    };
    use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
    use crate::protocol::{
        ContextDistillationProposal, ContextDraftIdentity, ContextDraftPhase, ContextDraftProgress,
    };
    use crate::provider::{
        ContextProjectionValidationOperation, ContextProviderFamily,
        ContextProviderValidationIdentity, ContextReasoningBlockKind,
        ContextRequestBuilderValidation, EventStream, Provider,
        context_projection_validation_report,
    };
    use crate::tool::Registry;
    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use jcode_context_core::{
        authoritative_transcript_digest, build_content_target, build_message_range,
        estimate_content_block_tokens, estimate_message_tokens,
        resolve_reasoning_suppression_for_ranges,
    };
    use jcode_session_types::{
        StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextCuratorUsage,
        StoredContextEmergencyAudit, StoredContextEmergencyOperationKind,
        StoredContextEmergencyRetryOutcome, StoredContextEmergencyTriggerKind,
        StoredContextOperation, StoredDisplayRole, StoredMessage, StoredRangeSummary,
        StoredStartupBatchDeliveryState, StoredStartupBatchKind, StoredStartupContextBatch,
        StoredStartupContextReceipt, StoredStartupContextState, StoredStartupFileObservation,
        StoredStartupFileReceipt, StoredStartupObservedState, StoredStartupPathClassification,
        StoredStartupProjectIdentity, StoredToolResultDistillation,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex as StdMutex};
    use std::time::Instant;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct TestProviderState {
        invalidations: AtomicUsize,
        supported: AtomicBool,
    }

    #[derive(Clone)]
    struct TestProvider {
        state: Arc<TestProviderState>,
    }

    impl TestProvider {
        fn new(supported: bool) -> Self {
            let provider = Self {
                state: Arc::new(TestProviderState::default()),
            };
            provider.set_supported(supported);
            provider
        }

        fn invalidation_count(&self) -> usize {
            self.state.invalidations.load(Ordering::SeqCst)
        }

        fn set_supported(&self, supported: bool) {
            self.state.supported.store(supported, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Provider for TestProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty::<Result<StreamEvent>>()))
        }

        fn name(&self) -> &str {
            "phase7-test"
        }

        fn display_name(&self) -> String {
            "Phase 7 Test".to_string()
        }

        fn model(&self) -> String {
            "phase7-model".to_string()
        }

        fn context_window(&self) -> usize {
            372_000
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> ContextProjectionValidationReport {
            let builder = if self.state.supported.load(Ordering::SeqCst) {
                let mut builder = ContextRequestBuilderValidation::new(messages.len());
                builder.formatter_placeholder_count = 1;
                builder
                    .normalization_notes
                    .push("safe normalization note".to_string());
                Ok(builder)
            } else {
                Err("test provider rejected the projected request".to_string())
            };
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: ContextProviderFamily::OpenRouterCompatible,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "phase7_test_builder_v1".to_string(),
                },
                operations,
                Some(ContextReasoningBlockKind::GenericReasoning),
                builder,
            )
        }

        fn invalidate_context_continuation(&self, _reason: &str) {
            self.state.invalidations.fetch_add(1, Ordering::SeqCst);
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    #[derive(Default)]
    struct TestPersistence {
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    impl TestPersistence {
        fn with_failure(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn set_failure(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }
    }

    impl ContextPersistence for TestPersistence {
        fn persist(&self, _agent: &mut Agent) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                bail!("injected context persistence failure");
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestDirectSessionPersistence {
        calls: AtomicUsize,
        fail: AtomicBool,
        observed_provider_session_ids: StdMutex<Vec<Option<String>>>,
    }

    impl TestDirectSessionPersistence {
        fn with_failure(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
                observed_provider_session_ids: StdMutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn observed_provider_session_ids(&self) -> Vec<Option<String>> {
            self.observed_provider_session_ids
                .lock()
                .expect("direct persistence observations")
                .clone()
        }
    }

    impl DirectContextSessionPersistence for TestDirectSessionPersistence {
        fn persist(&self, session: &mut Session) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_provider_session_ids
                .lock()
                .expect("direct persistence observations")
                .push(session.provider_session_id.clone());
            if self.fail.load(Ordering::SeqCst) {
                bail!("injected direct Session persistence failure");
            }
            Ok(())
        }
    }

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_string(),
            cache_control: None,
        }
    }

    fn generator() -> StoredContextArtifactGenerator {
        StoredContextArtifactGenerator {
            provider: "curator-test".to_string(),
            model: "curator-model".to_string(),
            route: "curator-route".to_string(),
            prompt_version: "context-curator-v1".to_string(),
            effort: None,
            role: None,
            selection_source: None,
            transaction_instructions: None,
            task_instructions: None,
        }
    }

    fn populated_agent(provider: Arc<dyn Provider>) -> Agent {
        let mut agent = Agent::new(provider, Registry::empty());
        agent.add_message(Role::User, vec![text(&"old selected context ".repeat(80))]);
        agent.add_message(
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "historical replay reasoning ".repeat(80),
                },
                text("visible answer"),
            ],
        );
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "phase7-call".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "printf test"}),
                thought_signature: Some("must-survive".to_string()),
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "phase7-call".to_string(),
                content: "large tool result ".repeat(400),
                is_error: Some(false),
            }],
        );
        agent
    }

    fn ready_draft(agent: &Agent, provider: &dyn Provider, draft_id: &str) -> ContextDraft {
        let messages = agent.messages();
        let summary_index = messages
            .iter()
            .position(|message| {
                message.content.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text, .. } if text.contains("old selected context"))
                })
            })
            .expect("summary source message");
        let reasoning_index = messages
            .iter()
            .position(|message| {
                message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
            })
            .expect("reasoning source message");
        let result_index = messages
            .iter()
            .position(|message| {
                message.content.iter().any(|block| {
                    matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "phase7-call")
                })
            })
            .expect("tool-result source message");
        let authorization = StoredContextAuthorization::Manual {
            initiated_by: Some("phase7-test".to_string()),
        };
        let range =
            build_message_range(messages, summary_index, summary_index).expect("summary range");
        let summary = StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range: range,
            summary_text: "Selected historical context was summarized without information loss."
                .to_string(),
            file_change_digest: "No files changed.".to_string(),
            changed_files: Vec::new(),
            change_evidence_complete: true,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: Some(generator()),
            source_token_estimate: estimate_message_tokens(&messages[summary_index].to_message()),
            replacement_token_estimate: 32,
            warnings: Vec::new(),
            created_at: Utc::now(),
            legacy_coverage: None,
        });
        let reasoning_range = build_message_range(messages, reasoning_index, reasoning_index)
            .expect("reasoning range");
        let reasoning = StoredContextOperation::ReasoningSuppression(
            resolve_reasoning_suppression_for_ranges(messages, &[reasoning_range])
                .expect("reasoning selection must resolve"),
        );
        let original = estimate_content_block_tokens(&messages[result_index].content[0]);
        let replacement_content = "distilled test result".to_string();
        let replacement = estimate_content_block_tokens(&ContentBlock::ToolResult {
            tool_use_id: "phase7-call".to_string(),
            content: replacement_content.clone(),
            is_error: Some(false),
        });
        assert!(replacement.saturating_mul(5) < original);
        let proposal = ContextDistillationProposal {
            proposal_id: "proposal-1".to_string(),
            selected_by_default: true,
            operation: StoredToolResultDistillation {
                target: build_content_target(messages, result_index, 0)
                    .expect("tool-result target"),
                tool_name: "bash".to_string(),
                tool_call_id: "phase7-call".to_string(),
                replacement_content,
                original_token_estimate: original,
                replacement_token_estimate: replacement,
                replacement_ratio_millionths: ((replacement as u128 * 1_000_000) / original as u128)
                    as u32,
                preservation_rationale: "all operationally relevant output was preserved"
                    .to_string(),
                uncertainties: Vec::new(),
                generator: generator(),
                created_at: Utc::now(),
            },
        };
        let required_operations = vec![summary, reasoning];
        let mut preview_operations = required_operations.clone();
        preview_operations.push(StoredContextOperation::ToolResultDistillation(
            proposal.operation.clone(),
        ));
        let preview = build_preview(ContextDraftPreviewInput {
            provider,
            messages,
            base_state: agent.context_view_state(),
            transaction_id: draft_id,
            proposed_revision: 1,
            authorization: authorization.clone(),
            operations: &preview_operations,
            pricing: None,
            estimated_total_request_tokens_before: agent.current_context_request_token_estimate(),
            notices: Vec::new(),
            ranges: &[],
            proposals: std::slice::from_ref(&proposal),
        })
        .expect("ready preview");
        ContextDraft {
            identity: ContextDraftIdentity {
                draft_id: draft_id.to_string(),
                session_id: agent.session_id().to_string(),
                base_context_revision: agent.context_view_state().revision,
                raw_message_count: messages.len(),
                transcript_digest: authoritative_transcript_digest(messages),
                provider_name: provider.name().to_string(),
                model: provider.model(),
                route: agent.context_route_identity(),
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(30),
            },
            authorization: authorization.clone(),
            active_agent_profile_message_id: agent
                .active_transition_message_id()
                .map(str::to_string),
            required_operations,
            distillation_proposals: vec![proposal],
            ineligible_distillations: Vec::new(),
            preview,
            curator_usage: vec![StoredContextCuratorUsage {
                provider: "curator-test".to_string(),
                model: "curator-model".to_string(),
                route: "curator-route".to_string(),
                effort: None,
                role: None,
                artifact_id: None,
                prompt_version: None,
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: None,
                cost_usd: None,
            }],
        }
    }

    struct CommitServiceFixture {
        service: Arc<ContextTransactionService>,
        agent: Arc<AsyncMutex<Agent>>,
        provider: TestProvider,
        persistence: Arc<TestPersistence>,
        raw_before: Vec<u8>,
    }

    fn service_fixture(supported: bool, persistence_fails: bool) -> CommitServiceFixture {
        let provider = TestProvider::new(true);
        let provider_dyn: Arc<dyn Provider> = Arc::new(provider.clone());
        let agent = populated_agent(provider_dyn);
        let raw_before = serde_json::to_vec(agent.messages()).expect("raw transcript");
        let draft = ready_draft(&agent, &provider, "draft-1");
        let persistence = Arc::new(TestPersistence::with_failure(persistence_fails));
        let service = Arc::new(ContextTransactionService::with_persistence(
            ContextServiceLimits::default(),
            persistence.clone(),
        ));
        let reserved_bytes = serde_json::to_vec(&draft).expect("draft bytes").len();
        service.lock_store().entries.insert(
            draft.identity.draft_id.clone(),
            ContextDraftEntry {
                identity: draft.identity.clone(),
                progress: ContextDraftProgress {
                    phase: ContextDraftPhase::Ready,
                    completed_items: 3,
                    total_items: 3,
                },
                state: DraftEntryState::Ready(draft),
                cancellation: CancellationToken::new(),
                notify: Arc::new(Notify::new()),
                reserved_bytes,
                generation_in_flight: false,
            },
        );
        provider.set_supported(supported);
        CommitServiceFixture {
            service,
            agent: Arc::new(AsyncMutex::new(agent)),
            provider,
            persistence,
            raw_before,
        }
    }

    fn direct_session_fixture(
        persistence_fails: bool,
    ) -> (
        ContextTransactionService,
        Session,
        TestProvider,
        Arc<TestDirectSessionPersistence>,
        Vec<u8>,
    ) {
        let provider = TestProvider::new(true);
        let agent = populated_agent(Arc::new(provider.clone()));
        let draft = ready_draft(&agent, &provider, "draft-local-1");
        let mut session = Session::create(None, None);
        session.id = agent.session_id().to_string();
        session.replace_messages(agent.messages().to_vec());
        session.context_view = agent.context_view_state().clone();
        session.provider_key = Some(draft.identity.route.clone());
        session.model = Some(provider.model());
        session.provider_session_id = Some("stored-provider-continuation".to_string());
        let raw_before = serde_json::to_vec(&session.messages).expect("raw transcript");
        let direct_persistence = Arc::new(TestDirectSessionPersistence::with_failure(
            persistence_fails,
        ));
        let service = ContextTransactionService::with_persistence_boundaries(
            ContextServiceLimits::default(),
            Arc::new(TestPersistence::default()),
            direct_persistence.clone(),
        );
        let reserved_bytes = serde_json::to_vec(&draft).expect("draft bytes").len();
        service.lock_store().entries.insert(
            draft.identity.draft_id.clone(),
            ContextDraftEntry {
                identity: draft.identity.clone(),
                progress: ContextDraftProgress {
                    phase: ContextDraftPhase::Ready,
                    completed_items: 3,
                    total_items: 3,
                },
                state: DraftEntryState::Ready(draft),
                cancellation: CancellationToken::new(),
                notify: Arc::new(Notify::new()),
                reserved_bytes,
                generation_in_flight: false,
            },
        );
        (service, session, provider, direct_persistence, raw_before)
    }

    fn startup_context_direct_fixture() -> (
        ContextTransactionService,
        Session,
        TestProvider,
        Arc<TestDirectSessionPersistence>,
        Vec<u8>,
        Vec<u8>,
    ) {
        let provider = TestProvider::new(true);
        let now = Utc::now();
        let stored = |id: &str, content: Vec<ContentBlock>| StoredMessage {
            origin: None,
            id: id.to_string(),
            role: Role::User,
            content,
            display_role: Some(StoredDisplayRole::System),
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        };
        let mut source = Session::create_with_id(
            "session-startup-context-transaction".to_string(),
            None,
            None,
        );
        source.append_stored_message(stored(
            "startup-transaction-control",
            vec![text("SYNTHETIC_TRANSACTION_STARTUP_CONTROL")],
        ));
        source.append_stored_message(stored(
            "startup-transaction-file",
            vec![
                text("synthetic startup metadata"),
                text("SYNTHETIC_TRANSACTION_STARTUP_FILE_BODY"),
            ],
        ));
        source.append_stored_message(stored(
            "startup-transaction-stale",
            vec![text("SYNTHETIC_TRANSACTION_STALE_MARKER")],
        ));
        source.append_stored_message(stored(
            "startup-transaction-user",
            vec![text("ordinary user prompt after Startup Context")],
        ));
        source.startup_context = Some(StoredStartupContextReceipt {
            schema_version: 1,
            project: StoredStartupProjectIdentity::Directory {
                canonical_root: "/synthetic/context-project".to_string(),
            },
            plan_revision: 9,
            state: StoredStartupContextState::ProviderAccepted,
            batches: vec![StoredStartupContextBatch {
                id: "startup-transaction-batch".to_string(),
                kind: StoredStartupBatchKind::Initial,
                control_message_id: "startup-transaction-control".to_string(),
                files: vec![StoredStartupFileReceipt {
                    spec_id: "startup-transaction-spec".to_string(),
                    message_id: "startup-transaction-file".to_string(),
                    ordinal: 2,
                    logical_path: "PLAN.md".to_string(),
                    resolved_path: "/synthetic/context-project/PLAN.md".to_string(),
                    classification: StoredStartupPathClassification::Project,
                    sha256: "e".repeat(64),
                    bytes: 39,
                    estimated_tokens: 10,
                    latest_observation: StoredStartupFileObservation {
                        observed_at: now,
                        state: StoredStartupObservedState::Changed {
                            sha256: "f".repeat(64),
                            bytes: 40,
                        },
                    },
                    last_notified_observation: Some(StoredStartupObservedState::Changed {
                        sha256: "f".repeat(64),
                        bytes: 40,
                    }),
                    notification_count: 1,
                    stale_marker_message_ids: vec!["startup-transaction-stale".to_string()],
                }],
                appended_at: now,
                delivery_state: StoredStartupBatchDeliveryState::ProviderAccepted,
                first_dispatched_at: Some(now),
                first_provider_accepted_at: Some(now),
            }],
            blocked_issues: Vec::new(),
            pending_updates: Vec::new(),
            last_apply_operation_id: None,
            prepared_at: now,
            first_dispatched_at: Some(now),
            first_provider_accepted_at: Some(now),
            metadata_repair: None,
        });

        let agent = Agent::new_with_session(
            Arc::new(provider.clone()),
            Registry::empty(),
            source.clone(),
            None,
        );
        let route = agent.context_route_identity();
        source.provider_key = Some(route.clone());
        source.model = Some(provider.model());
        let authorization = StoredContextAuthorization::Manual {
            initiated_by: Some("startup-context-transaction-test".to_string()),
        };
        let operation = StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range: build_message_range(agent.messages(), 0, 2)
                .expect("Startup Context source range"),
            summary_text: "Synthetic Startup Context summary".to_string(),
            file_change_digest: "Synthetic fixture changed no files".to_string(),
            changed_files: Vec::new(),
            change_evidence_complete: true,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: Some(generator()),
            source_token_estimate: agent.messages()[0..=2]
                .iter()
                .map(|message| estimate_message_tokens(&message.to_message()))
                .sum(),
            replacement_token_estimate: 12,
            warnings: Vec::new(),
            created_at: now,
            legacy_coverage: None,
        });
        let operations = vec![operation];
        let preview = build_preview(ContextDraftPreviewInput {
            provider: &provider,
            messages: agent.messages(),
            base_state: agent.context_view_state(),
            transaction_id: "draft-startup-context",
            proposed_revision: 1,
            authorization: authorization.clone(),
            operations: &operations,
            pricing: None,
            estimated_total_request_tokens_before: agent.current_context_request_token_estimate(),
            notices: Vec::new(),
            ranges: &[],
            proposals: &[],
        })
        .expect("Startup Context summary preview");
        let draft = ContextDraft {
            identity: ContextDraftIdentity {
                draft_id: "draft-startup-context".to_string(),
                session_id: source.id.clone(),
                base_context_revision: 0,
                raw_message_count: source.messages.len(),
                transcript_digest: authoritative_transcript_digest(&source.messages),
                provider_name: provider.name().to_string(),
                model: provider.model(),
                route,
                created_at: now,
                expires_at: now + chrono::Duration::minutes(30),
            },
            authorization,
            active_agent_profile_message_id: source
                .active_transition_message_id()
                .map(str::to_string),
            required_operations: operations,
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview,
            curator_usage: Vec::new(),
        };
        let direct_persistence = Arc::new(TestDirectSessionPersistence::default());
        let service = ContextTransactionService::with_persistence_boundaries(
            ContextServiceLimits::default(),
            Arc::new(TestPersistence::default()),
            direct_persistence.clone(),
        );
        let reserved_bytes = serde_json::to_vec(&draft).expect("draft bytes").len();
        service.lock_store().entries.insert(
            draft.identity.draft_id.clone(),
            ContextDraftEntry {
                identity: draft.identity.clone(),
                progress: ContextDraftProgress {
                    phase: ContextDraftPhase::Ready,
                    completed_items: 1,
                    total_items: 1,
                },
                state: DraftEntryState::Ready(draft),
                cancellation: CancellationToken::new(),
                notify: Arc::new(Notify::new()),
                reserved_bytes,
                generation_in_flight: false,
            },
        );
        let raw_before = serde_json::to_vec(&source.messages).expect("raw Startup Context");
        let receipt_before =
            serde_json::to_vec(&source.startup_context).expect("Startup Context receipt");
        (
            service,
            source,
            provider,
            direct_persistence,
            raw_before,
            receipt_before,
        )
    }

    #[test]
    fn direct_session_apply_revert_reapply_change_projection_without_mutating_raw_transcript() {
        let (service, mut session, provider, persistence, raw_before) =
            direct_session_fixture(false);
        let route = session
            .provider_key
            .clone()
            .expect("direct fixture route identity");
        let original_projection = session
            .projected_messages_for_provider()
            .expect("original provider view");

        let applied = service
            .apply_draft_to_session(
                &mut session,
                &provider,
                &route,
                None,
                "draft-local-1",
                None,
                false,
            )
            .expect("direct apply");
        let applied_projection = session
            .projected_messages_for_provider()
            .expect("applied provider view");
        assert_ne!(
            serde_json::to_vec(&applied_projection).expect("applied projection bytes"),
            serde_json::to_vec(&original_projection).expect("original projection bytes")
        );
        assert_eq!(session.provider_session_id, None);

        session.provider_session_id = Some("continuation-before-revert".to_string());
        service
            .revert_transaction_in_session(
                &mut session,
                &provider,
                &route,
                None,
                &applied.result.transaction.id,
                false,
            )
            .expect("direct revert");
        assert_eq!(
            serde_json::to_vec(
                &session
                    .projected_messages_for_provider()
                    .expect("reverted provider view")
            )
            .expect("reverted projection bytes"),
            serde_json::to_vec(&original_projection).expect("original projection bytes")
        );
        assert_eq!(session.provider_session_id, None);

        session.provider_session_id = Some("continuation-before-reapply".to_string());
        service
            .reapply_transaction_in_session(
                &mut session,
                &provider,
                &route,
                None,
                &applied.result.transaction.id,
                false,
            )
            .expect("direct reapply");
        assert_eq!(
            serde_json::to_vec(
                &session
                    .projected_messages_for_provider()
                    .expect("reapplied provider view")
            )
            .expect("reapplied projection bytes"),
            serde_json::to_vec(&applied_projection).expect("applied projection bytes")
        );
        assert_eq!(session.provider_session_id, None);
        assert_eq!(
            serde_json::to_vec(&session.messages).expect("raw transcript"),
            raw_before
        );
        assert_eq!(persistence.calls(), 3);
        assert_eq!(
            persistence.observed_provider_session_ids(),
            vec![None, None, None]
        );
    }

    #[test]
    fn startup_context_summary_revert_and_reapply_preserve_source_receipt_and_cache_semantics() {
        let (service, mut session, provider, persistence, raw_before, receipt_before) =
            startup_context_direct_fixture();
        let route = session.provider_key.clone().expect("provider route");
        let original_projection = session
            .projected_messages_for_provider()
            .expect("original Startup Context provider view");

        session.provider_session_id = Some("startup-before-apply".to_string());
        let applied = service
            .apply_draft_to_session(
                &mut session,
                &provider,
                &route,
                None,
                "draft-startup-context",
                None,
                false,
            )
            .expect("apply Startup Context summary");
        let applied_projection = session
            .projected_messages_for_provider()
            .expect("applied Startup Context provider view");
        let applied_json = serde_json::to_string(&applied_projection).unwrap();
        assert!(applied_json.contains("Synthetic Startup Context summary"));
        assert!(!applied_json.contains("SYNTHETIC_TRANSACTION_STARTUP_CONTROL"));
        assert!(!applied_json.contains("SYNTHETIC_TRANSACTION_STARTUP_FILE_BODY"));
        assert!(!applied_json.contains("SYNTHETIC_TRANSACTION_STALE_MARKER"));
        assert_eq!(session.provider_session_id, None);
        assert_eq!(provider.invalidation_count(), 1);

        session.provider_session_id = Some("startup-before-revert".to_string());
        service
            .revert_transaction_in_session(
                &mut session,
                &provider,
                &route,
                None,
                &applied.result.transaction.id,
                false,
            )
            .expect("revert Startup Context summary");
        assert_eq!(
            serde_json::to_vec(
                &session
                    .projected_messages_for_provider()
                    .expect("reverted Startup Context provider view")
            )
            .unwrap(),
            serde_json::to_vec(&original_projection).unwrap()
        );
        assert_eq!(session.provider_session_id, None);

        session.provider_session_id = Some("startup-before-reapply".to_string());
        service
            .reapply_transaction_in_session(
                &mut session,
                &provider,
                &route,
                None,
                &applied.result.transaction.id,
                false,
            )
            .expect("reapply Startup Context summary");
        assert_eq!(
            serde_json::to_vec(
                &session
                    .projected_messages_for_provider()
                    .expect("reapplied Startup Context provider view")
            )
            .unwrap(),
            serde_json::to_vec(&applied_projection).unwrap()
        );
        assert_eq!(session.provider_session_id, None);
        assert_eq!(provider.invalidation_count(), 1);
        assert_eq!(serde_json::to_vec(&session.messages).unwrap(), raw_before);
        assert_eq!(
            serde_json::to_vec(&session.startup_context).unwrap(),
            receipt_before
        );
        assert_eq!(persistence.calls(), 3);
    }

    #[test]
    fn direct_context_apply_rejects_draft_with_stale_active_profile_identity() {
        let provider = TestProvider::new(true);
        let mut session = Session::create_with_id("session-profile-commit".to_string(), None, None);
        session.install_system_prompt(crate::session::StoredSystemPromptState {
            text: "SYNTHETIC_SYSTEM".to_string(),
            active_agent: crate::session::StoredAgentReference {
                scope: crate::instruction::InstructionScope::Global,
                id: "initial".to_string(),
                display_name: "Initial".to_string(),
            },
            first_provider_dispatch_at: Some(Utc::now()),
            active_transition_message_id: None,
        });
        let ordinary_id = session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "ordinary source for summary".repeat(20),
                cache_control: None,
            }],
        );
        let active_profile_id = session
            .append_agent_profile_transition(crate::instruction::AgentProfileTransition {
                agent: crate::session::StoredAgentReference {
                    scope: crate::instruction::InstructionScope::Global,
                    id: "active".to_string(),
                    display_name: "Active".to_string(),
                },
                transition_sentence: "SYNTHETIC_TRANSITION".to_string(),
                complete_instructions: "SYNTHETIC_ACTIVE_PROFILE".to_string(),
                initialized_global_store: false,
            })
            .expect("append active profile");
        let source_range = build_message_range(&session.messages, 0, 0).expect("source range");
        assert_eq!(source_range.start_message_id, ordinary_id);
        let authorization = StoredContextAuthorization::Manual { initiated_by: None };
        let operation = StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range,
            summary_text: "synthetic complete summary".to_string(),
            file_change_digest: "No files changed.".to_string(),
            changed_files: Vec::new(),
            change_evidence_complete: true,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: Some(generator()),
            source_token_estimate: 100,
            replacement_token_estimate: 10,
            warnings: Vec::new(),
            created_at: Utc::now(),
            legacy_coverage: None,
        });
        let preview = build_preview(ContextDraftPreviewInput {
            provider: &provider,
            messages: &session.messages,
            base_state: &session.context_view,
            transaction_id: "draft-profile-commit",
            proposed_revision: 1,
            authorization: authorization.clone(),
            operations: std::slice::from_ref(&operation),
            pricing: None,
            estimated_total_request_tokens_before: None,
            notices: Vec::new(),
            ranges: &[],
            proposals: &[],
        })
        .expect("draft preview");
        let draft = ContextDraft {
            identity: ContextDraftIdentity {
                draft_id: "draft-profile-commit".to_string(),
                session_id: session.id.clone(),
                base_context_revision: 0,
                raw_message_count: session.messages.len(),
                transcript_digest: authoritative_transcript_digest(&session.messages),
                provider_name: provider.name().to_string(),
                model: provider.model(),
                route: "profile-commit-route".to_string(),
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(30),
            },
            authorization,
            // Simulate a draft captured through an obsolete/unprotected path.
            active_agent_profile_message_id: None,
            required_operations: vec![operation],
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview,
            curator_usage: Vec::new(),
        };
        assert_eq!(
            session.active_transition_message_id(),
            Some(active_profile_id.as_str())
        );
        let direct_persistence = Arc::new(TestDirectSessionPersistence::default());
        let service = ContextTransactionService::with_persistence_boundaries(
            ContextServiceLimits::default(),
            Arc::new(TestPersistence::default()),
            direct_persistence.clone(),
        );
        let reserved_bytes = serde_json::to_vec(&draft).expect("draft bytes").len();
        service.lock_store().entries.insert(
            draft.identity.draft_id.clone(),
            ContextDraftEntry {
                identity: draft.identity.clone(),
                progress: ContextDraftProgress {
                    phase: ContextDraftPhase::Ready,
                    completed_items: 1,
                    total_items: 1,
                },
                state: DraftEntryState::Ready(draft),
                cancellation: CancellationToken::new(),
                notify: Arc::new(Notify::new()),
                reserved_bytes,
                generation_in_flight: false,
            },
        );
        let context_before = session.context_view.clone();
        let error = service
            .apply_draft_to_session(
                &mut session,
                &provider,
                "profile-commit-route",
                None,
                "draft-profile-commit",
                None,
                false,
            )
            .expect_err("stale profile protection must block commit");
        assert!(matches!(error, ContextServiceError::Stale(_)));
        assert_eq!(session.context_view, context_before);
        assert_eq!(direct_persistence.calls(), 0);
    }

    #[test]
    fn direct_session_persistence_failure_restores_context_and_provider_continuation() {
        let (service, mut session, provider, persistence, raw_before) =
            direct_session_fixture(true);
        let route = session
            .provider_key
            .clone()
            .expect("direct fixture route identity");
        let state_before = session.context_view.clone();
        let provider_session_id_before = session.provider_session_id.clone();

        let error = service
            .apply_draft_to_session(
                &mut session,
                &provider,
                &route,
                None,
                "draft-local-1",
                None,
                false,
            )
            .expect_err("direct persistence must fail");

        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(session.context_view, state_before);
        assert_eq!(session.provider_session_id, provider_session_id_before);
        assert_eq!(
            serde_json::to_vec(&session.messages).expect("raw transcript"),
            raw_before
        );
        assert_eq!(persistence.calls(), 1);
        assert_eq!(persistence.observed_provider_session_ids(), vec![None]);
        assert!(matches!(
            service
                .draft_status("draft-local-1")
                .expect("draft restored"),
            crate::context::ContextDraftStatus::Ready { .. }
        ));
    }

    #[test]
    fn atomic_apply_persists_all_operations_once_and_preserves_raw_transcript() {
        let _guard = crate::storage::lock_test_env();
        crate::cache_invalidation::clear_for_tests();
        let started = Instant::now();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, false);

        let result = service
            .apply_draft(&agent, "draft-1", None, false)
            .expect("atomic apply");

        assert_eq!(result.revision, 1);
        assert_eq!(result.status, StoredContextTransactionStatusKind::Applied);
        assert_eq!(result.transaction.operation_counts.range_summaries, 1);
        assert_eq!(
            result.transaction.operation_counts.reasoning_suppressions,
            1
        );
        assert_eq!(
            result
                .transaction
                .operation_counts
                .tool_result_distillations,
            1
        );
        assert_eq!(provider.invalidation_count(), 1);
        assert_eq!(persistence.calls(), 1);
        let agent = agent.try_lock().expect("idle agent");
        assert_eq!(agent.context_view_state().revision, 1);
        assert_eq!(agent.context_view_state().transactions.len(), 1);
        assert_eq!(
            serde_json::to_vec(agent.messages()).expect("raw transcript"),
            raw_before
        );
        let transaction = &agent.context_view_state().transactions[0];
        assert_eq!(transaction.curator_usage.len(), 1);
        assert!(transaction.application.is_some());
        assert!(transaction.economics.is_some());
        let reasoning = transaction
            .operations
            .iter()
            .find_map(|operation| match operation {
                StoredContextOperation::ReasoningSuppression(reasoning) => Some(reasoning),
                _ => None,
            })
            .expect("reasoning operation");
        assert_eq!(reasoning.validation.len(), 1);
        assert_eq!(
            reasoning.validation[0].request_builder,
            "phase7_test_builder_v1"
        );
        drop(agent);
        assert!(matches!(
            service.draft_status("draft-1").expect("draft status"),
            crate::context::ContextDraftStatus::Applied { revision: 1, .. }
        ));
        let invalidation =
            crate::cache_invalidation::most_recent_since(started).expect("documented invalidation");
        assert_eq!(invalidation.source, "context transaction");
        assert!(invalidation.detail.contains("range_summaries=1"));
    }

    #[test]
    fn persistence_failure_restores_exact_state_and_ready_draft_without_reset() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before: _,
        } = service_fixture(true, true);
        let before = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();

        let error = service
            .apply_draft(&agent, "draft-1", None, false)
            .expect_err("persistence must fail");

        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(
            agent.try_lock().expect("idle agent").context_view_state(),
            &before
        );
        assert_eq!(provider.invalidation_count(), 0);
        assert_eq!(persistence.calls(), 1);
        assert!(matches!(
            service.draft_status("draft-1").expect("draft status"),
            crate::context::ContextDraftStatus::Ready { .. }
        ));
    }

    #[test]
    fn empty_ready_draft_is_non_applicable_and_causes_no_persistence_or_runtime_reset() {
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, false);
        {
            let mut store = service.lock_store();
            let entry = store.entries.get_mut("draft-1").expect("ready draft entry");
            let DraftEntryState::Ready(draft) = &mut entry.state else {
                panic!("expected ready draft");
            };
            draft.required_operations.clear();
            draft.distillation_proposals.clear();
            draft.preview.operation_previews.clear();
            draft.preview.proposed_context_revision = draft.preview.current_context_revision;
            entry.reserved_bytes = serde_json::to_vec(draft).expect("draft bytes").len();
        }
        let before = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();

        let error = service
            .apply_draft(&agent, "draft-1", Some(Vec::new()), false)
            .expect_err("empty draft must not apply");

        assert!(matches!(error, ContextServiceError::EmptyRequest));
        let guard = agent.try_lock().expect("idle agent");
        assert_eq!(guard.context_view_state(), &before);
        assert_eq!(serde_json::to_vec(guard.messages()).unwrap(), raw_before);
        drop(guard);
        assert_eq!(persistence.calls(), 0);
        assert_eq!(provider.invalidation_count(), 0);
        assert!(matches!(
            service.draft_status("draft-1"),
            Ok(crate::context::ContextDraftStatus::Ready { .. })
        ));
    }

    #[test]
    fn provider_validation_failure_is_terminal_and_cannot_activate_partial_state() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before: _,
        } = service_fixture(false, false);

        let error = service
            .apply_draft(&agent, "draft-1", None, false)
            .expect_err("provider validation must fail");

        assert!(matches!(error, ContextServiceError::ProviderValidation(_)));
        assert_eq!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .revision,
            0
        );
        assert_eq!(provider.invalidation_count(), 0);
        assert_eq!(persistence.calls(), 0);
        assert!(matches!(
            service.draft_status("draft-1").expect("draft status"),
            crate::context::ContextDraftStatus::Failed {
                error: ContextServiceError::ProviderValidation(_),
                ..
            }
        ));
    }

    #[test]
    fn invalid_distillation_selection_restores_ready_and_valid_subset_needs_no_curator() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider: _,
            persistence,
            raw_before: _,
        } = service_fixture(true, false);
        let error = service
            .apply_draft(&agent, "draft-1", Some(vec!["unknown".to_string()]), false)
            .expect_err("unknown proposal must fail");
        assert!(matches!(error, ContextServiceError::InvalidSelection(_)));
        assert!(matches!(
            service.draft_status("draft-1").expect("ready after error"),
            crate::context::ContextDraftStatus::Ready { .. }
        ));

        let result = service
            .apply_draft(&agent, "draft-1", Some(Vec::new()), false)
            .expect("empty optional subset is valid");
        assert_eq!(
            result
                .transaction
                .operation_counts
                .tool_result_distillations,
            0
        );
        assert_eq!(persistence.calls(), 1);
    }

    #[test]
    fn two_clients_racing_one_draft_produce_one_commit_and_one_reset() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before: _,
        } = service_fixture(true, false);
        let barrier = Arc::new(Barrier::new(3));
        let (first, second) = std::thread::scope(|scope| {
            let first_service = Arc::clone(&service);
            let first_agent = Arc::clone(&agent);
            let first_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                first_barrier.wait();
                first_service.apply_draft(&first_agent, "draft-1", None, false)
            });
            let second_service = Arc::clone(&service);
            let second_agent = Arc::clone(&agent);
            let second_barrier = Arc::clone(&barrier);
            let second = scope.spawn(move || {
                second_barrier.wait();
                second_service.apply_draft(&second_agent, "draft-1", None, false)
            });
            barrier.wait();
            (
                first.join().expect("first client"),
                second.join().expect("second client"),
            )
        });
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(provider.invalidation_count(), 1);
        assert_eq!(persistence.calls(), 1);
        assert_eq!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .revision,
            1
        );
    }

    #[test]
    fn apply_revert_and_reapply_each_create_one_revision_and_one_reset() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, false);
        service
            .apply_draft(&agent, "draft-1", None, false)
            .expect("apply");
        let reverted = service
            .revert_transaction(&agent, "draft-1", false)
            .expect("revert");
        let reapplied = service
            .reapply_transaction(&agent, "draft-1", false)
            .expect("reapply");

        assert_eq!(reverted.revision, 2);
        assert_eq!(
            reverted.status,
            StoredContextTransactionStatusKind::Reverted
        );
        assert_eq!(reapplied.revision, 3);
        assert_eq!(
            reapplied.status,
            StoredContextTransactionStatusKind::Reapplied
        );
        assert_eq!(provider.invalidation_count(), 3);
        assert_eq!(persistence.calls(), 3);
        let agent_guard = agent.try_lock().expect("idle agent");
        assert_eq!(agent_guard.context_view_state().revision, 3);
        assert_eq!(
            agent_guard.context_view_state().transactions[0]
                .status_events
                .len(),
            3
        );
        assert_eq!(
            serde_json::to_vec(agent_guard.messages()).expect("raw transcript"),
            raw_before
        );
        drop(agent_guard);
        let history = service.list_transactions(&agent).expect("history");
        assert_eq!(history.len(), 1);
        assert!(history[0].active);
        assert_eq!(
            history[0].latest_status,
            Some(StoredContextTransactionStatusKind::Reapplied)
        );
    }

    #[test]
    fn failed_revert_restores_active_state_without_an_extra_reset() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before: _,
        } = service_fixture(true, false);
        service
            .apply_draft(&agent, "draft-1", None, false)
            .expect("apply");
        persistence.fail.store(true, Ordering::SeqCst);
        let before = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();

        let error = service
            .revert_transaction(&agent, "draft-1", false)
            .expect_err("revert persistence must fail");

        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(
            agent.try_lock().expect("idle agent").context_view_state(),
            &before
        );
        assert_eq!(provider.invalidation_count(), 1);
        assert_eq!(persistence.calls(), 2);
    }

    #[test]
    fn reapply_revalidates_source_targets_after_transcript_rewind() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider: _,
            persistence,
            raw_before: _,
        } = service_fixture(true, false);
        service
            .apply_draft(&agent, "draft-1", None, false)
            .expect("apply");
        service
            .revert_transaction(&agent, "draft-1", false)
            .expect("revert");
        agent
            .try_lock()
            .expect("idle agent")
            .rewind_to_message(1)
            .expect("rewind source transcript");

        let error = service
            .reapply_transaction(&agent, "draft-1", false)
            .expect_err("stale target must reject reapply");

        assert!(matches!(error, ContextServiceError::Projection(_)));
        assert_eq!(persistence.calls(), 2);
        let state = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();
        assert_eq!(state.revision, 2);
        assert!(!state.transactions[0].is_active());
    }

    #[test]
    fn failed_reapply_restores_exact_inactive_state_without_reset_or_invalidation() {
        let _guard = crate::storage::lock_test_env();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before: _,
        } = service_fixture(true, false);
        let applied = service
            .apply_draft(&agent, "draft-1", None, false)
            .expect("initial apply");
        service
            .revert_transaction(&agent, &applied.transaction.id, false)
            .expect("revert before failed reapply");
        let before = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();
        let reset_count_before = provider.invalidation_count();
        let persistence_calls_before = persistence.calls();
        persistence.set_failure(true);

        let error = service
            .reapply_transaction(&agent, &applied.transaction.id, false)
            .expect_err("reapply persistence must fail");

        assert!(matches!(error, ContextServiceError::Persistence(_)));
        let guard = agent.try_lock().expect("idle agent");
        assert_eq!(guard.context_view_state(), &before);
        let transaction = guard
            .context_view_state()
            .transactions
            .iter()
            .find(|transaction| transaction.id == applied.transaction.id)
            .expect("retained transaction");
        assert!(!transaction.is_active());
        drop(guard);
        assert_eq!(provider.invalidation_count(), reset_count_before);
        assert_eq!(persistence.calls(), persistence_calls_before + 1);
    }

    #[test]
    fn emergency_policy_persists_idempotently_and_rolls_back_without_context_reset() {
        let _guard = crate::storage::lock_test_env();
        crate::cache_invalidation::clear_for_tests();
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            ..
        } = service_fixture(true, false);
        let session_id = agent
            .try_lock()
            .expect("idle agent")
            .session_id()
            .to_string();
        let authorized = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 20,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: "explicit phase 8 test authorization".to_string(),
        };

        let (returned_session_id, returned_policy) = service
            .set_emergency_policy(&agent, authorized.clone(), false)
            .expect("authorized policy persists");
        assert_eq!(returned_session_id, session_id);
        assert_eq!(returned_policy, authorized);
        assert_eq!(persistence.calls(), 1);
        {
            let guard = agent.try_lock().expect("idle agent");
            assert_eq!(guard.context_view_state().revision, 0);
            assert_eq!(guard.context_view_state().emergency_policy, returned_policy);
        }
        assert_eq!(provider.invalidation_count(), 0);

        let (same_session_id, same_policy) = service
            .set_emergency_policy(&agent, authorized.clone(), false)
            .expect("same policy is an idempotent no-op");
        assert_eq!(same_session_id, session_id);
        assert_eq!(same_policy, authorized);
        assert_eq!(persistence.calls(), 1);

        persistence.set_failure(true);
        let error = service
            .set_emergency_policy(&agent, StoredContextEmergencyPolicy::Block, false)
            .expect_err("persistence failure must reject policy mutation");
        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(persistence.calls(), 2);
        {
            let guard = agent.try_lock().expect("idle agent");
            assert_eq!(guard.context_view_state().revision, 0);
            assert_eq!(guard.context_view_state().emergency_policy, authorized);
        }
        assert_eq!(provider.invalidation_count(), 0);

        let invalid = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 0,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: false,
            authorization_source: "invalid headroom".to_string(),
        };
        let error = service
            .set_emergency_policy(&agent, invalid, false)
            .expect_err("invalid policy must fail before persistence");
        assert!(matches!(error, ContextServiceError::InvalidSelection(_)));
        assert_eq!(persistence.calls(), 2);
        assert_eq!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .emergency_policy,
            authorized
        );
        assert_eq!(provider.invalidation_count(), 0);
    }

    #[test]
    fn direct_session_emergency_policy_is_atomic_idempotent_and_revision_neutral() {
        let (service, mut session, provider, persistence, raw_before) =
            direct_session_fixture(false);
        let revision_before = session.context_view.revision;
        let provider_session_before = session.provider_session_id.clone();
        let policy = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 7,
            target_headroom_percent: 15,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: true,
            authorization_source: "context_editor_session:local-test".to_string(),
        };

        service
            .set_emergency_policy_for_session(&mut session, policy.clone(), false)
            .expect("policy persists");
        assert_eq!(session.context_view.emergency_policy, policy);
        assert_eq!(session.context_view.revision, revision_before);
        assert_eq!(session.provider_session_id, provider_session_before);
        assert_eq!(provider.invalidation_count(), 0);
        assert_eq!(persistence.calls(), 1);
        assert_eq!(
            serde_json::to_vec(&session.messages).expect("raw transcript"),
            raw_before
        );

        let unchanged_policy = session.context_view.emergency_policy.clone();
        service
            .set_emergency_policy_for_session(&mut session, unchanged_policy, false)
            .expect("idempotent policy succeeds");
        assert_eq!(persistence.calls(), 1);

        let (failing_service, mut failing_session, failing_provider, failing_persistence, _) =
            direct_session_fixture(true);
        let prior_policy = failing_session.context_view.emergency_policy.clone();
        let failing_revision = failing_session.context_view.revision;
        let failing_provider_session = failing_session.provider_session_id.clone();
        let error = failing_service
            .set_emergency_policy_for_session(
                &mut failing_session,
                StoredContextEmergencyPolicy::Authorized {
                    protected_recent_assistant_turns: 3,
                    target_headroom_percent: 10,
                    allow_reasoning_suppression: true,
                    allow_tool_distillation: false,
                    allow_oldest_range_summary: false,
                    authorization_source: "failing-local-policy".to_string(),
                },
                false,
            )
            .expect_err("persistence failure rolls back");
        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(failing_session.context_view.emergency_policy, prior_policy);
        assert_eq!(failing_session.context_view.revision, failing_revision);
        assert_eq!(
            failing_session.provider_session_id,
            failing_provider_session
        );
        assert_eq!(failing_provider.invalidation_count(), 0);
        assert_eq!(failing_persistence.calls(), 1);
    }

    fn emergency_audit_fixture() -> StoredContextEmergencyAudit {
        StoredContextEmergencyAudit {
            authorization_source: "scheduled_item:sched-atomic".to_string(),
            scheduled_item_id: Some("sched-atomic".to_string()),
            policy: StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns: 2,
                target_headroom_percent: 10,
                allow_reasoning_suppression: true,
                allow_tool_distillation: true,
                allow_oldest_range_summary: true,
                authorization_source: "schedule_tool_session:origin".to_string(),
            },
            trigger_kind: StoredContextEmergencyTriggerKind::PreflightLimit,
            provider_error: None,
            context_window: 372_000,
            safe_input_budget: 367_904,
            projected_input_tokens: 370_000,
            required_reduction_to_fit_tokens: 2_096,
            required_reduction_to_target_tokens: 38_886,
            achieved_reduction_tokens: 0,
            protected_recent_assistant_turns: 2,
            protected_message_count: 2,
            operation_order: vec![
                StoredContextEmergencyOperationKind::ReasoningSuppression,
                StoredContextEmergencyOperationKind::ToolResultDistillation,
                StoredContextEmergencyOperationKind::OldestRangeSummary,
            ],
            retry_outcome: StoredContextEmergencyRetryOutcome::Pending,
        }
    }

    #[test]
    fn unattended_emergency_commit_is_one_audited_revision_and_one_reset() {
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, false);
        let mut agent = agent.try_lock().expect("idle agent");
        let draft = ready_draft(&agent, &provider, "emergency-source");
        let mut operations = draft.required_operations;
        operations.extend(
            draft
                .distillation_proposals
                .into_iter()
                .map(|proposal| StoredContextOperation::ToolResultDistillation(proposal.operation)),
        );
        let authorization = StoredContextAuthorization::UnattendedEmergency {
            authorization_source: "scheduled_item:sched-atomic".to_string(),
            trigger: Some("preflight_limit".to_string()),
            scheduled_item_id: Some("sched-atomic".to_string()),
        };

        let result = service
            .apply_unattended_emergency_operations(
                &mut agent,
                "context-emergency-atomic",
                authorization.clone(),
                operations,
                draft.curator_usage,
                emergency_audit_fixture(),
            )
            .expect("emergency transaction commits atomically");

        assert_eq!(result.revision, 1);
        assert_eq!(agent.context_view_state().transactions.len(), 1);
        let transaction = &agent.context_view_state().transactions[0];
        assert_eq!(transaction.authorization, authorization);
        let audit = transaction
            .emergency_audit
            .as_ref()
            .expect("audit retained");
        assert!(audit.achieved_reduction_tokens > 0);
        assert_eq!(
            audit.retry_outcome,
            StoredContextEmergencyRetryOutcome::Pending
        );
        assert_eq!(persistence.calls(), 1);
        assert_eq!(provider.invalidation_count(), 1);
        assert_eq!(serde_json::to_vec(agent.messages()).unwrap(), raw_before);
    }

    #[test]
    fn unattended_emergency_persistence_failure_has_no_transaction_reset_or_retry_state() {
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, true);
        let mut agent = agent.try_lock().expect("idle agent");
        let draft = ready_draft(&agent, &provider, "emergency-failure-source");
        let mut operations = draft.required_operations;
        operations.extend(
            draft
                .distillation_proposals
                .into_iter()
                .map(|proposal| StoredContextOperation::ToolResultDistillation(proposal.operation)),
        );
        let mut audit = emergency_audit_fixture();
        audit.authorization_source = "scheduled_item:sched-fails".to_string();
        audit.scheduled_item_id = Some("sched-fails".to_string());
        let error = service
            .apply_unattended_emergency_operations(
                &mut agent,
                "context-emergency-fails",
                StoredContextAuthorization::UnattendedEmergency {
                    authorization_source: "scheduled_item:sched-fails".to_string(),
                    trigger: Some("preflight_limit".to_string()),
                    scheduled_item_id: Some("sched-fails".to_string()),
                },
                operations,
                draft.curator_usage,
                audit,
            )
            .expect_err("persistence failure rejects the whole transaction");
        assert!(matches!(error, ContextServiceError::Persistence(_)));
        assert_eq!(agent.context_view_state().revision, 0);
        assert!(agent.context_view_state().transactions.is_empty());
        assert_eq!(provider.invalidation_count(), 0);
        assert_eq!(persistence.calls(), 1);
        assert_eq!(serde_json::to_vec(agent.messages()).unwrap(), raw_before);
    }

    #[test]
    fn unattended_emergency_rejects_mismatched_audit_before_persistence_or_reset() {
        let CommitServiceFixture {
            service,
            agent,
            provider,
            persistence,
            raw_before,
        } = service_fixture(true, false);
        let mut agent = agent.try_lock().expect("idle agent");
        let draft = ready_draft(&agent, &provider, "emergency-invalid-audit");
        let mut operations = draft.required_operations;
        operations.extend(
            draft
                .distillation_proposals
                .into_iter()
                .map(|proposal| StoredContextOperation::ToolResultDistillation(proposal.operation)),
        );
        let error = service
            .apply_unattended_emergency_operations(
                &mut agent,
                "context-emergency-invalid-audit",
                StoredContextAuthorization::UnattendedEmergency {
                    authorization_source: "scheduled_item:different".to_string(),
                    trigger: Some("preflight_limit".to_string()),
                    scheduled_item_id: Some("different".to_string()),
                },
                operations,
                draft.curator_usage,
                emergency_audit_fixture(),
            )
            .expect_err("mismatched provenance fails closed");
        assert!(matches!(error, ContextServiceError::InvalidSelection(_)));
        assert_eq!(agent.context_view_state().revision, 0);
        assert!(agent.context_view_state().transactions.is_empty());
        assert_eq!(provider.invalidation_count(), 0);
        assert_eq!(persistence.calls(), 0);
        assert_eq!(serde_json::to_vec(agent.messages()).unwrap(), raw_before);
    }
}
