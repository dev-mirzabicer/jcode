use crate::{ContextOperationRef, ContextProjection, ContextProjectionError, project_context};
use chrono::{DateTime, Utc};
use jcode_session_types::{
    StoredContextStatusEvent, StoredContextTransactionStatusKind, StoredContextViewState,
    StoredMessage,
};
use std::error::Error;
use std::fmt;

/// Result of reconciling persisted context transactions after an explicit transcript edit.
///
/// The caller supplies the edited authoritative transcript. This function never mutates it,
/// never retargets an operation, and invalidates only active transactions whose exact persisted
/// sources no longer resolve. Every invalidation produced by one edit shares one new context
/// revision so the edit remains one provider-view transition.
#[derive(Debug)]
pub struct ContextTranscriptReconciliation {
    pub state: StoredContextViewState,
    pub projection: ContextProjection,
    pub invalidated_transaction_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTranscriptReconciliationError {
    RevisionOverflow,
    Projection(ContextProjectionError),
}

impl fmt::Display for ContextTranscriptReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionOverflow => formatter.write_str(
                "context revision overflow while reconciling an explicit transcript edit",
            ),
            Self::Projection(error) => error.fmt(formatter),
        }
    }
}

impl Error for ContextTranscriptReconciliationError {}

fn stale_source_operation(error: &ContextProjectionError) -> Option<&ContextOperationRef> {
    match error {
        ContextProjectionError::Range { operation, .. }
        | ContextProjectionError::Target { operation, .. }
        | ContextProjectionError::RangeClosure { operation, .. }
        | ContextProjectionError::RangeNotStructurallyClosed { operation, .. } => Some(operation),
        ContextProjectionError::State(_)
        | ContextProjectionError::OverlappingRangeSummaries { .. }
        | ContextProjectionError::EmptyRangeSummary { .. }
        | ContextProjectionError::InvalidOperationTarget { .. }
        | ContextProjectionError::DuplicateBlockTransform { .. }
        | ContextProjectionError::DistillationRatioRejected { .. }
        | ContextProjectionError::DistillationRatioMismatch { .. }
        | ContextProjectionError::Structure(_) => None,
    }
}

pub fn reconcile_context_after_transcript_edit(
    messages: &[StoredMessage],
    current: &StoredContextViewState,
    timestamp: DateTime<Utc>,
    reason: impl Into<String>,
) -> Result<ContextTranscriptReconciliation, ContextTranscriptReconciliationError> {
    let reason = reason.into();
    let mut state = current.clone();
    let mut invalidated_transaction_ids = Vec::new();
    let mut invalidation_revision = None;

    loop {
        match project_context(messages, &state) {
            Ok(projection) => {
                return Ok(ContextTranscriptReconciliation {
                    state,
                    projection,
                    invalidated_transaction_ids,
                });
            }
            Err(error) => {
                let Some(operation) = stale_source_operation(&error) else {
                    return Err(ContextTranscriptReconciliationError::Projection(error));
                };
                let Some(transaction) = state.transactions.iter_mut().find(|transaction| {
                    transaction.id == operation.transaction_id && transaction.is_active()
                }) else {
                    return Err(ContextTranscriptReconciliationError::Projection(error));
                };

                let revision = match invalidation_revision {
                    Some(revision) => revision,
                    None => {
                        let revision = state
                            .revision
                            .checked_add(1)
                            .ok_or(ContextTranscriptReconciliationError::RevisionOverflow)?;
                        state.revision = revision;
                        invalidation_revision = Some(revision);
                        revision
                    }
                };
                let transaction_id = transaction.id.clone();
                transaction.status_events.push(StoredContextStatusEvent {
                    revision,
                    timestamp,
                    kind: StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
                    reason: Some(reason.clone()),
                });
                invalidated_transaction_ids.push(transaction_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_content_target;
    use jcode_message_types::{ContentBlock, Role};
    use jcode_session_types::{
        StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextOperation,
        StoredContextTransaction, StoredReasoningSelection, StoredReasoningSuppression,
    };

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-20T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn message(id: &str, role: Role, reasoning: &str, text: &str) -> StoredMessage {
        StoredMessage {
            id: id.to_string(),
            role,
            content: vec![
                ContentBlock::Reasoning {
                    text: reasoning.to_string(),
                },
                ContentBlock::Text {
                    text: text.to_string(),
                    cache_control: None,
                },
            ],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn suppression(messages: &[StoredMessage], message_index: usize) -> StoredContextOperation {
        let target = build_content_target(messages, message_index, 0).expect("reasoning target");
        StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
            selection: StoredReasoningSelection::MessageRanges { ranges: Vec::new() },
            assistant_turns_affected: 1,
            original_token_estimate: 10,
            replay_block_kinds: vec![target.kind],
            targets: vec![target],
            validation_evidence_version: 1,
            validation: Vec::new(),
        })
    }

    fn transaction(
        id: &str,
        base_revision: u64,
        applied_revision: u64,
        operation: StoredContextOperation,
    ) -> StoredContextTransaction {
        StoredContextTransaction {
            id: id.to_string(),
            base_revision,
            created_at: timestamp(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: vec![operation],
            status_events: vec![StoredContextStatusEvent {
                revision: applied_revision,
                timestamp: timestamp(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        }
    }

    #[test]
    fn transcript_edit_invalidates_only_stale_transactions_at_one_revision() {
        let raw = vec![
            message("m0", Role::Assistant, "old zero", "text zero"),
            message("m1", Role::Assistant, "old one", "text one"),
            message("m2", Role::Assistant, "old two", "text two"),
        ];
        let state = StoredContextViewState {
            revision: 3,
            transactions: vec![
                transaction("keep", 0, 1, suppression(&raw, 0)),
                transaction("drop-one", 1, 2, suppression(&raw, 1)),
                transaction("drop-two", 2, 3, suppression(&raw, 2)),
            ],
            ..StoredContextViewState::default()
        };
        let raw_before = serde_json::to_vec(&raw).expect("raw serialization");

        let reconciled = reconcile_context_after_transcript_edit(
            &raw[..1],
            &state,
            timestamp(),
            "rewind removed exact source material",
        )
        .expect("reconciliation");

        assert_eq!(reconciled.state.revision, 4);
        assert_eq!(
            reconciled.invalidated_transaction_ids,
            vec!["drop-one".to_string(), "drop-two".to_string()]
        );
        assert!(reconciled.state.transactions[0].is_active());
        for transaction in &reconciled.state.transactions[1..] {
            let status = transaction.latest_status().expect("status");
            assert_eq!(status.revision, 4);
            assert_eq!(
                status.kind,
                StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit
            );
            assert_eq!(
                status.reason.as_deref(),
                Some("rewind removed exact source material")
            );
        }
        assert_eq!(reconciled.projection.messages.len(), 1);
        assert_eq!(
            serde_json::to_vec(&raw).expect("raw serialization"),
            raw_before
        );
    }

    #[test]
    fn valid_transcript_edit_keeps_context_state_byte_identical() {
        let raw = vec![message(
            "m0",
            Role::Assistant,
            "historical reasoning",
            "retained text",
        )];
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![transaction("keep", 0, 1, suppression(&raw, 0))],
            ..StoredContextViewState::default()
        };

        let reconciled =
            reconcile_context_after_transcript_edit(&raw, &state, timestamp(), "no source changed")
                .expect("reconciliation");

        assert_eq!(reconciled.state, state);
        assert!(reconciled.invalidated_transaction_ids.is_empty());
    }

    #[test]
    fn non_source_projection_errors_are_not_misreported_as_transcript_invalidation() {
        use jcode_session_types::StoredToolResultDistillation;

        let raw = vec![
            StoredMessage {
                id: "call-message".to_string(),
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "tool".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            },
            StoredMessage {
                id: "result-message".to_string(),
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "large result".repeat(100),
                    is_error: Some(false),
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            },
        ];
        let target = build_content_target(&raw, 1, 0).expect("tool result target");
        let operation =
            StoredContextOperation::ToolResultDistillation(StoredToolResultDistillation {
                target,
                tool_name: "tool".to_string(),
                tool_call_id: "call-1".to_string(),
                replacement_content: "replacement".to_string(),
                original_token_estimate: 100,
                replacement_token_estimate: 20,
                replacement_ratio_millionths: 200_000,
                preservation_rationale: "fixture".to_string(),
                uncertainties: Vec::new(),
                generator: StoredContextArtifactGenerator {
                    provider: "provider".to_string(),
                    model: "model".to_string(),
                    route: "route".to_string(),
                    prompt_version: "v1".to_string(),
                    effort: None,
                },
                created_at: timestamp(),
            });
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![transaction("invalid-ratio", 0, 1, operation)],
            ..StoredContextViewState::default()
        };

        let error =
            reconcile_context_after_transcript_edit(&raw, &state, timestamp(), "transcript edit")
                .expect_err("ratio error must remain authoritative");

        assert!(matches!(
            error,
            ContextTranscriptReconciliationError::Projection(
                ContextProjectionError::DistillationRatioRejected { .. }
            )
        ));
        assert!(state.transactions[0].is_active());
    }
}
