use super::*;
use crate::context::{ContextDraftRuntimeInput, ContextTransactionService};
use crate::protocol::{
    ContextDraft, ContextDraftRequest, ContextDraftStatus, ContextMessageRangeSelection,
    ContextPressureLevel, ContextReasoningSelectionRequest, ContextToolResultSelection,
};
use jcode_context_core::{close_message_ranges, estimate_content_block_tokens};
use jcode_message_types::ContentBlock;
use jcode_session_types::{
    StoredContextAuthorization, StoredContextEmergencyAudit, StoredContextEmergencyOperationKind,
    StoredContextEmergencyPolicy, StoredContextEmergencyRetryOutcome,
    StoredContextEmergencyTriggerKind, StoredContextOperation,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MIN_EMERGENCY_TOOL_RESULT_TOKENS: usize = 2_000;
const MIN_SUMMARY_REPLACEMENT_ALLOWANCE: usize = 256;

impl Agent {
    pub(super) fn finish_emergency_retry_audit(
        &mut self,
        outcome: StoredContextEmergencyRetryOutcome,
    ) -> bool {
        let Some(transaction_id) = self
            .active_turn_context
            .as_ref()
            .and_then(|context| context.emergency_transaction_id.clone())
        else {
            return true;
        };
        let Some(index) = self
            .session
            .context_view
            .transactions
            .iter()
            .position(|transaction| transaction.id == transaction_id)
        else {
            logging::warn("Emergency retry audit transaction is unavailable");
            return false;
        };
        let previous = self.session.context_view.transactions[index]
            .emergency_audit
            .clone();
        let Some(audit) = self.session.context_view.transactions[index]
            .emergency_audit
            .as_mut()
        else {
            logging::warn("Emergency retry audit record is unavailable");
            return false;
        };
        if !matches!(
            audit.retry_outcome,
            StoredContextEmergencyRetryOutcome::Pending
        ) {
            return true;
        }
        audit.retry_outcome = outcome;
        if let Err(error) = self.session.save() {
            self.session.context_view.transactions[index].emergency_audit = previous;
            logging::warn(&format!(
                "Failed to persist emergency retry audit for transaction {}: {}",
                transaction_id, error
            ));
            return false;
        }
        true
    }

    pub(super) async fn try_unattended_emergency_preflight_recovery(
        &mut self,
        report: &ContextPreflightReport,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<bool> {
        self.try_unattended_emergency_recovery(
            report,
            StoredContextEmergencyTriggerKind::PreflightLimit,
            None,
            event_tx,
        )
        .await
    }

    pub(super) async fn try_unattended_emergency_provider_recovery(
        &mut self,
        provider_error: &str,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<bool> {
        if self.provider_output_started() || !Self::is_context_limit_error(provider_error) {
            return Ok(false);
        }
        let Some(mut report) = self
            .active_turn_context
            .as_ref()
            .and_then(|context| context.last_preflight.clone())
        else {
            return Ok(false);
        };
        report.pressure = ContextPressureLevel::Blocked;
        report.required_reduction_tokens = report.required_reduction_tokens.max(1);
        report.remaining_safe_input_tokens = 0;
        self.try_unattended_emergency_recovery(
            &report,
            StoredContextEmergencyTriggerKind::ProviderContextLimit,
            Some(provider_error),
            event_tx,
        )
        .await
    }

    async fn try_unattended_emergency_recovery(
        &mut self,
        report: &ContextPreflightReport,
        trigger_kind: StoredContextEmergencyTriggerKind,
        provider_error: Option<&str>,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<bool> {
        if report.pressure != ContextPressureLevel::Blocked {
            return Ok(false);
        }
        let Some(context) = self.active_turn_context.as_ref() else {
            return Ok(false);
        };
        let Some(authorization) = context.unattended_context.clone() else {
            return Ok(false);
        };
        if context.emergency_attempted || !authorization.is_authorized() {
            return Ok(false);
        }
        if self.provider_output_started() {
            return Ok(false);
        }
        if let Some(context) = self.active_turn_context.as_mut() {
            context.emergency_attempted = true;
        }

        validate_unattended_authorization(&authorization)?;
        let policy = authorization.policy.clone();
        let required_to_target = required_reduction_to_target(report, &policy);
        let service = Arc::new(ContextTransactionService::new());
        let (reasoning_request, protected_count) = self.build_emergency_draft_request(
            &authorization,
            required_to_target,
            trigger_kind,
            false,
        )?;
        let later_operation_enabled = matches!(
            &policy,
            StoredContextEmergencyPolicy::Authorized {
                allow_tool_distillation: true,
                ..
            } | StoredContextEmergencyPolicy::Authorized {
                allow_oldest_range_summary: true,
                ..
            }
        );
        let reasoning_draft = if reasoning_request.is_empty() {
            None
        } else {
            Some(
                self.prepare_emergency_draft(&service, reasoning_request, report)
                    .await?,
            )
        };
        let (draft_id, draft) = match reasoning_draft {
            Some((draft_id, draft))
                if draft.preview.economics.deleted_input_tokens >= required_to_target
                    || !later_operation_enabled =>
            {
                (draft_id, draft)
            }
            Some((draft_id, _)) => {
                let _ = service.cancel_draft(&draft_id);
                let (request, _) = self.build_emergency_draft_request(
                    &authorization,
                    required_to_target,
                    trigger_kind,
                    true,
                )?;
                self.prepare_emergency_draft(&service, request, report)
                    .await?
            }
            None => {
                let (request, _) = self.build_emergency_draft_request(
                    &authorization,
                    required_to_target,
                    trigger_kind,
                    true,
                )?;
                self.prepare_emergency_draft(&service, request, report)
                    .await?
            }
        };
        if draft.preview.economics.deleted_input_tokens < required_to_target {
            return Err(anyhow::anyhow!(
                "authorized unattended context recovery could safely remove {} token(s), below the {} token(s) required for target headroom",
                draft.preview.economics.deleted_input_tokens,
                required_to_target
            ));
        }

        let summary_operations = draft
            .required_operations
            .iter()
            .filter(|operation| matches!(operation, StoredContextOperation::RangeSummary(_)))
            .cloned()
            .collect::<Vec<_>>();
        let mut operations = draft
            .required_operations
            .iter()
            .filter(|operation| {
                matches!(operation, StoredContextOperation::ReasoningSuppression(_))
            })
            .cloned()
            .collect::<Vec<_>>();
        let tool_operations = draft
            .distillation_proposals
            .iter()
            .filter(|proposal| proposal.selected_by_default)
            .map(|proposal| {
                StoredContextOperation::ToolResultDistillation(proposal.operation.clone())
            })
            .collect::<Vec<_>>();
        let transaction_id = format!("context-emergency-{}", uuid::Uuid::new_v4());
        let authorization_record = StoredContextAuthorization::UnattendedEmergency {
            authorization_source: authorization.authorization_source.clone(),
            trigger: Some(emergency_trigger_label(trigger_kind).to_string()),
            scheduled_item_id: authorization.scheduled_item_id.clone(),
        };
        let reasoning_deleted_input_tokens = if operations.is_empty() {
            0
        } else {
            service
                .preview_unattended_emergency_operations(
                    self,
                    &transaction_id,
                    authorization_record.clone(),
                    operations.clone(),
                )?
                .deleted_input_tokens
        };
        let reasoning_and_tools_deleted_input_tokens =
            if reasoning_deleted_input_tokens >= required_to_target {
                reasoning_deleted_input_tokens
            } else {
                operations.extend(tool_operations);
                if operations.is_empty() {
                    0
                } else {
                    service
                        .preview_unattended_emergency_operations(
                            self,
                            &transaction_id,
                            authorization_record.clone(),
                            operations.clone(),
                        )?
                        .deleted_input_tokens
                }
            };
        if reasoning_and_tools_deleted_input_tokens < required_to_target {
            operations.extend(summary_operations);
        }
        let final_economics = service.preview_unattended_emergency_operations(
            self,
            &transaction_id,
            authorization_record.clone(),
            operations.clone(),
        )?;
        if final_economics.deleted_input_tokens < required_to_target {
            return Err(anyhow::anyhow!(
                "authorized unattended context recovery could safely remove {} token(s), below the {} token(s) required for target headroom",
                final_economics.deleted_input_tokens,
                required_to_target
            ));
        }
        let operation_order = operation_order(&operations);
        let policy_turns = match &policy {
            StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns,
                ..
            } => *protected_recent_assistant_turns,
            StoredContextEmergencyPolicy::Block => 0,
        };
        let audit = StoredContextEmergencyAudit {
            authorization_source: authorization.authorization_source.clone(),
            scheduled_item_id: authorization.scheduled_item_id.clone(),
            policy: policy.clone(),
            trigger_kind,
            provider_error: provider_error
                .map(|error| crate::util::truncate_str(error, 512).to_string()),
            context_window: report.context_window,
            safe_input_budget: report.safe_input_budget,
            projected_input_tokens: report.projected_input_tokens,
            required_reduction_to_fit_tokens: report.required_reduction_tokens,
            required_reduction_to_target_tokens: required_to_target,
            achieved_reduction_tokens: 0,
            protected_recent_assistant_turns: policy_turns,
            protected_message_count: protected_count,
            operation_order,
            retry_outcome: StoredContextEmergencyRetryOutcome::Pending,
        };
        let result = service
            .apply_unattended_emergency_operations(
                self,
                &transaction_id,
                authorization_record,
                operations,
                draft.curator_usage,
                audit,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Some(context) = self.active_turn_context.as_mut() {
            context.emergency_transaction_id = Some(transaction_id.clone());
        }
        if let Some(event_tx) = event_tx {
            let _ = event_tx.send(ServerEvent::ContextTransactionApplied {
                id: context_request_id(self),
                draft_id,
                result,
            });
        }
        Ok(true)
    }

    async fn prepare_emergency_draft(
        &self,
        service: &Arc<ContextTransactionService>,
        request: ContextDraftRequest,
        report: &ContextPreflightReport,
    ) -> Result<(String, ContextDraft)> {
        let input = ContextDraftRuntimeInput {
            session_id: self.session.id.clone(),
            messages: self.session.messages.clone(),
            context_view: self.session.context_view.clone(),
            provider: Arc::clone(&self.provider),
            route: self.context_route_identity(),
            model_routes: self.provider.model_routes(),
            estimated_total_request_tokens_before: Some(report.projected_input_tokens),
            active_agent_profile_message_id: self
                .active_transition_message_id()
                .map(str::to_string),
        };
        let draft_id = service
            .prepare_draft_for_session(input, request, false)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let status = service
            .wait_for_draft(&draft_id, std::time::Duration::from_secs(10 * 60))
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let draft = match status {
            ContextDraftStatus::Ready { draft } => *draft,
            ContextDraftStatus::Failed { error, .. } => {
                return Err(anyhow::anyhow!(
                    "authorized unattended context recovery could not prepare a safe transaction: {error}"
                ));
            }
            other => {
                return Err(anyhow::anyhow!(
                    "authorized unattended context recovery ended in unexpected draft state: {other:?}"
                ));
            }
        };
        Ok((draft_id, draft))
    }

    fn build_emergency_draft_request(
        &self,
        authorization: &jcode_session_types::StoredUnattendedContextAuthorization,
        required_to_target: usize,
        trigger_kind: StoredContextEmergencyTriggerKind,
        include_curator_operations: bool,
    ) -> Result<(ContextDraftRequest, usize)> {
        let StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns,
            allow_reasoning_suppression,
            allow_tool_distillation,
            allow_oldest_range_summary,
            ..
        } = &authorization.policy
        else {
            return Err(anyhow::anyhow!("unattended context policy is block"));
        };
        let protected = protected_message_indices(
            &self.session.messages,
            self.active_turn_context
                .as_ref()
                .map(|context| context.transcript_len_before_pending)
                .unwrap_or(self.session.messages.len()),
            *protected_recent_assistant_turns,
        );
        let protected_count = protected.len();

        let reasoning = if *allow_reasoning_suppression {
            let ranges = self
                .session
                .messages
                .iter()
                .enumerate()
                .filter(|(index, message)| {
                    !protected.contains(index)
                        && matches!(message.role, Role::Assistant)
                        && message.content.iter().any(is_replayed_reasoning)
                })
                .map(|(_, message)| ContextMessageRangeSelection {
                    start_message_id: message.id.clone(),
                    end_message_id: message.id.clone(),
                })
                .collect::<Vec<_>>();
            (!ranges.is_empty())
                .then_some(ContextReasoningSelectionRequest::MessageRanges { ranges })
        } else {
            None
        };

        let mut tool_results = Vec::new();
        let reasoning_reduction = reasoning_potential(&self.session.messages, &protected);
        if include_curator_operations && *allow_tool_distillation {
            for (message_index, message) in self.session.messages.iter().enumerate() {
                if protected.contains(&message_index) {
                    continue;
                }
                for (block_ordinal, block) in message.content.iter().enumerate() {
                    if matches!(block, ContentBlock::ToolResult { .. }) {
                        let tokens = estimate_content_block_tokens(block);
                        if tokens >= MIN_EMERGENCY_TOOL_RESULT_TOKENS {
                            tool_results.push(ContextToolResultSelection {
                                message_id: message.id.clone(),
                                block_ordinal,
                            });
                        }
                    }
                }
            }
        }

        let mut summary_ranges = Vec::new();
        if include_curator_operations
            && required_to_target > reasoning_reduction
            && *allow_oldest_range_summary
        {
            let remaining = required_to_target.saturating_sub(reasoning_reduction);
            if let Some(range) = oldest_summary_range(
                &self.session.messages,
                &self.session.context_view,
                &protected,
                remaining.saturating_add(MIN_SUMMARY_REPLACEMENT_ALLOWANCE),
            )? {
                summary_ranges.push(range);
            }
        }
        let request = ContextDraftRequest {
            summary_ranges,
            reasoning,
            tool_results,
            allow_shadowing_active_operations: false,
            curator: Default::default(),
            authorization: StoredContextAuthorization::UnattendedEmergency {
                authorization_source: authorization.authorization_source.clone(),
                trigger: Some(emergency_trigger_label(trigger_kind).to_string()),
                scheduled_item_id: authorization.scheduled_item_id.clone(),
            },
        };
        if request.is_empty() && include_curator_operations {
            return Err(anyhow::anyhow!(
                "authorized unattended policy exposed no eligible safe context operation"
            ));
        }
        Ok((request, protected_count))
    }
}

fn emergency_trigger_label(trigger_kind: StoredContextEmergencyTriggerKind) -> &'static str {
    match trigger_kind {
        StoredContextEmergencyTriggerKind::PreflightLimit => "preflight_limit",
        StoredContextEmergencyTriggerKind::ProviderContextLimit => "provider_context_limit",
    }
}

fn validate_unattended_authorization(
    authorization: &jcode_session_types::StoredUnattendedContextAuthorization,
) -> Result<()> {
    if !authorization.is_authorized() {
        return Err(anyhow::anyhow!(
            "unattended context authorization must carry an authorized policy"
        ));
    }
    crate::context::validate_emergency_policy(&authorization.policy)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let source = authorization.authorization_source.trim();
    if source.is_empty() || source.chars().count() > 512 {
        return Err(anyhow::anyhow!(
            "unattended authorization source must contain 1 through 512 characters"
        ));
    }
    if let Some(item_id) = authorization.scheduled_item_id.as_deref() {
        let item_id = item_id.trim();
        if item_id.is_empty() || item_id.chars().count() > 128 {
            return Err(anyhow::anyhow!(
                "scheduled item authorization ID must contain 1 through 128 characters"
            ));
        }
        if source != format!("scheduled_item:{item_id}") {
            return Err(anyhow::anyhow!(
                "scheduled item authorization source does not match its item ID"
            ));
        }
    }
    Ok(())
}

fn context_request_id(agent: &Agent) -> u64 {
    agent
        .active_turn_context
        .as_ref()
        .and_then(|context| context.request_id)
        .unwrap_or_default()
}

fn required_reduction_to_target(
    report: &ContextPreflightReport,
    policy: &StoredContextEmergencyPolicy,
) -> usize {
    let headroom = match policy {
        StoredContextEmergencyPolicy::Authorized {
            target_headroom_percent,
            ..
        } => usize::from(*target_headroom_percent),
        StoredContextEmergencyPolicy::Block => 0,
    };
    let target_input = report
        .safe_input_budget
        .saturating_mul(100usize.saturating_sub(headroom))
        / 100;
    report.projected_input_tokens.saturating_sub(target_input)
}

fn is_replayed_reasoning(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::Reasoning { .. }
            | ContentBlock::AnthropicThinking { .. }
            | ContentBlock::OpenAIReasoning { .. }
    )
}

fn reasoning_potential(messages: &[StoredMessage], protected: &HashSet<usize>) -> usize {
    messages
        .iter()
        .enumerate()
        .filter(|(index, _)| !protected.contains(index))
        .flat_map(|(_, message)| message.content.iter())
        .filter(|block| is_replayed_reasoning(block))
        .map(estimate_content_block_tokens)
        .sum()
}

fn protected_message_indices(
    messages: &[StoredMessage],
    pending_start: usize,
    protected_recent_assistant_turns: usize,
) -> HashSet<usize> {
    let mut protected = (pending_start..messages.len()).collect::<HashSet<_>>();
    for (index, _) in messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| matches!(message.role, Role::Assistant))
        .take(protected_recent_assistant_turns)
    {
        protected.insert(index);
    }

    let call_positions = messages
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            message.content.iter().filter_map(move |block| match block {
                ContentBlock::ToolUse { id, .. } => Some((id.clone(), index)),
                _ => None,
            })
        })
        .collect::<HashMap<_, _>>();
    if let Some((assistant_index, ids)) =
        messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| {
                let ids = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                (!ids.is_empty()).then_some((index, ids))
            })
    {
        protected.insert(assistant_index);
        for (index, message) in messages.iter().enumerate().skip(assistant_index) {
            if message.content.iter().any(|block| match block {
                ContentBlock::ToolResult { tool_use_id, .. } => ids.contains(tool_use_id.as_str()),
                _ => false,
            }) {
                protected.insert(index);
            }
        }
    }
    for (id, index) in call_positions {
        if protected.iter().any(|protected_index| {
            messages[*protected_index]
                .content
                .iter()
                .any(|block| match block {
                    ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id == &id,
                    _ => false,
                })
        }) {
            protected.insert(index);
        }
    }
    protected
}

fn oldest_summary_range(
    messages: &[StoredMessage],
    state: &jcode_session_types::StoredContextViewState,
    protected: &HashSet<usize>,
    target_source_tokens: usize,
) -> Result<Option<ContextMessageRangeSelection>> {
    let Some(limit) = (0..messages.len()).find(|index| protected.contains(index)) else {
        return Ok(None);
    };
    if limit == 0 {
        return Ok(None);
    }
    let mut accumulated = 0usize;
    let mut requested_end = None;
    for (index, message) in messages.iter().enumerate().take(limit) {
        accumulated = accumulated.saturating_add(
            message
                .content
                .iter()
                .map(estimate_content_block_tokens)
                .sum::<usize>(),
        );
        if accumulated >= target_source_tokens {
            requested_end = Some(index);
            break;
        }
    }
    let Some(requested_end) = requested_end else {
        return Ok(None);
    };
    let closed = close_message_ranges(messages, state, &[(0, requested_end)])
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let range = closed
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("structural closure returned no emergency range"))?;
    if (range.start..=range.end).any(|index| protected.contains(&index)) {
        return Err(anyhow::anyhow!(
            "the minimum structurally closed oldest range intersects protected recent material"
        ));
    }
    Ok(Some(ContextMessageRangeSelection {
        start_message_id: messages[range.start].id.clone(),
        end_message_id: messages[range.end].id.clone(),
    }))
}

fn operation_order(
    operations: &[StoredContextOperation],
) -> Vec<StoredContextEmergencyOperationKind> {
    let mut order = Vec::new();
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
            order.push(kind);
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_session_types::StoredContextViewState;

    fn message(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            id: id.to_string(),
            role,
            content,
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_string(),
            cache_control: None,
        }
    }

    #[test]
    fn protected_material_includes_pending_recent_assistant_and_complete_latest_tool_pair() {
        let messages = vec![
            message("old", Role::User, vec![text("old")]),
            message(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "true"}),
                    thought_signature: Some("provider-state".to_string()),
                }],
            ),
            message(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "result".to_string(),
                    is_error: Some(false),
                }],
            ),
            message(
                "older-reasoning",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "older".to_string(),
                }],
            ),
            message(
                "recent-reasoning",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "recent".to_string(),
                }],
            ),
            message("pending", Role::User, vec![text("pending prompt")]),
        ];
        let protected = protected_message_indices(&messages, 5, 1);
        assert_eq!(protected, HashSet::from([1, 2, 4, 5]));
        assert!(!protected.contains(&3));
    }

    #[test]
    fn oldest_summary_range_uses_structural_closure_and_never_crosses_protected_material() {
        let messages = vec![
            message("old", Role::User, vec![text(&"old ".repeat(300))]),
            message(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "true"}),
                    thought_signature: Some("provider-state".to_string()),
                }],
            ),
            message(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "result".repeat(300),
                    is_error: Some(false),
                }],
            ),
            message("protected", Role::Assistant, vec![text("recent")]),
            message("pending", Role::User, vec![text("pending")]),
        ];
        let protected = HashSet::from([3, 4]);
        let range = oldest_summary_range(
            &messages,
            &StoredContextViewState::default(),
            &protected,
            350,
        )
        .expect("closure succeeds")
        .expect("range exists");
        assert_eq!(range.start_message_id, "old");
        assert_eq!(range.end_message_id, "result");
    }

    #[test]
    fn target_headroom_requires_more_than_the_one_token_fit_boundary() {
        let report = ContextPreflightReport {
            context_revision: 0,
            pressure: ContextPressureLevel::Blocked,
            context_window: 100_000,
            safe_input_budget: 95_000,
            projected_input_tokens: 96_000,
            required_reduction_tokens: 1_000,
            remaining_context_tokens: 4_000,
            remaining_safe_input_tokens: 0,
            semantics: jcode_provider_core::ContextWindowSemantics::InputOnly,
            requested_max_output_tokens: None,
            output_reserve_tokens: 0,
            estimator_margin_tokens: 5_000,
            exact_output_reserve_known: true,
            breakdown: crate::protocol::ContextRequestTokenBreakdown::default(),
        };
        let policy = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 10,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: "test".to_string(),
        };
        assert_eq!(required_reduction_to_target(&report, &policy), 10_500);
    }

    #[test]
    fn replay_reasoning_excludes_transcript_only_traces_and_tool_thought_signatures() {
        assert!(is_replayed_reasoning(&ContentBlock::Reasoning {
            text: "replayed".to_string(),
        }));
        assert!(!is_replayed_reasoning(&ContentBlock::ReasoningTrace {
            text: "history only".to_string(),
        }));
        assert!(!is_replayed_reasoning(&ContentBlock::ToolUse {
            id: "call".to_string(),
            name: "tool".to_string(),
            input: serde_json::json!({}),
            thought_signature: Some("required".to_string()),
        }));
    }

    #[test]
    fn unattended_authorization_requires_bounded_exact_scheduled_provenance() {
        let policy = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 10,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: false,
            authorization_source: "schedule_tool_session:origin".to_string(),
        };
        let valid = jcode_session_types::StoredUnattendedContextAuthorization {
            policy: policy.clone(),
            authorization_source: "scheduled_item:sched-1".to_string(),
            scheduled_item_id: Some("sched-1".to_string()),
        };
        validate_unattended_authorization(&valid).expect("exact provenance is valid");

        let mut malformed = valid.clone();
        malformed.authorization_source = "scheduled_item:other".to_string();
        assert!(validate_unattended_authorization(&malformed).is_err());

        let mut oversized = valid;
        oversized.scheduled_item_id = Some("x".repeat(129));
        oversized.authorization_source = format!(
            "scheduled_item:{}",
            oversized.scheduled_item_id.as_deref().unwrap_or_default()
        );
        assert!(validate_unattended_authorization(&oversized).is_err());

        let blocked = jcode_session_types::StoredUnattendedContextAuthorization {
            policy: StoredContextEmergencyPolicy::Block,
            authorization_source: "session-policy".to_string(),
            scheduled_item_id: None,
        };
        assert!(validate_unattended_authorization(&blocked).is_err());
    }
}
