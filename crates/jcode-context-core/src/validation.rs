use jcode_message_types::{ContentBlock, Message};
use jcode_session_types::{
    STORED_CONTEXT_VIEW_SCHEMA_VERSION, StoredContextTransactionStatusKind, StoredContextViewState,
    StoredMessage,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralValidation {
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextStateValidationError {
    UnsupportedSchema {
        found: u32,
        supported: u32,
    },
    DuplicateTransactionId {
        transaction_id: String,
    },
    BaseRevisionInFuture {
        transaction_id: String,
        base_revision: u64,
        state_revision: u64,
    },
    MissingStatusHistory {
        transaction_id: String,
    },
    StatusRevisionNotAfterBase {
        transaction_id: String,
        base_revision: u64,
        status_revision: u64,
    },
    StatusRevisionInFuture {
        transaction_id: String,
        status_revision: u64,
        state_revision: u64,
    },
    NonMonotonicStatusRevision {
        transaction_id: String,
        previous_revision: u64,
        revision: u64,
    },
    InvalidStatusTransition {
        transaction_id: String,
        previous: Option<StoredContextTransactionStatusKind>,
        next: StoredContextTransactionStatusKind,
    },
}

impl fmt::Display for ContextStateValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { found, supported } => write!(
                formatter,
                "unsupported context-view schema {found}; this build supports schema {supported}"
            ),
            Self::DuplicateTransactionId { transaction_id } => {
                write!(
                    formatter,
                    "duplicate context transaction ID: {transaction_id}"
                )
            }
            Self::BaseRevisionInFuture {
                transaction_id,
                base_revision,
                state_revision,
            } => write!(
                formatter,
                "transaction {transaction_id} base revision {base_revision} exceeds state revision {state_revision}"
            ),
            Self::MissingStatusHistory { transaction_id } => {
                write!(
                    formatter,
                    "transaction {transaction_id} has no status history"
                )
            }
            Self::StatusRevisionNotAfterBase {
                transaction_id,
                base_revision,
                status_revision,
            } => write!(
                formatter,
                "transaction {transaction_id} status revision {status_revision} does not follow base revision {base_revision}"
            ),
            Self::StatusRevisionInFuture {
                transaction_id,
                status_revision,
                state_revision,
            } => write!(
                formatter,
                "transaction {transaction_id} status revision {status_revision} exceeds state revision {state_revision}"
            ),
            Self::NonMonotonicStatusRevision {
                transaction_id,
                previous_revision,
                revision,
            } => write!(
                formatter,
                "transaction {transaction_id} status revisions are not strictly increasing: {previous_revision} then {revision}"
            ),
            Self::InvalidStatusTransition {
                transaction_id,
                previous,
                next,
            } => write!(
                formatter,
                "transaction {transaction_id} has invalid status transition {previous:?} -> {next:?}"
            ),
        }
    }
}

impl Error for ContextStateValidationError {}

pub fn validate_context_state(
    state: &StoredContextViewState,
) -> Result<(), ContextStateValidationError> {
    if state.schema_version != STORED_CONTEXT_VIEW_SCHEMA_VERSION {
        return Err(ContextStateValidationError::UnsupportedSchema {
            found: state.schema_version,
            supported: STORED_CONTEXT_VIEW_SCHEMA_VERSION,
        });
    }
    let mut transaction_ids = HashSet::new();
    for transaction in &state.transactions {
        if !transaction_ids.insert(transaction.id.as_str()) {
            return Err(ContextStateValidationError::DuplicateTransactionId {
                transaction_id: transaction.id.clone(),
            });
        }
        if transaction.base_revision > state.revision {
            return Err(ContextStateValidationError::BaseRevisionInFuture {
                transaction_id: transaction.id.clone(),
                base_revision: transaction.base_revision,
                state_revision: state.revision,
            });
        }
        if transaction.status_events.is_empty() {
            return Err(ContextStateValidationError::MissingStatusHistory {
                transaction_id: transaction.id.clone(),
            });
        }
        let mut previous_revision = None;
        let mut previous_status = None;
        for status in &transaction.status_events {
            if status.revision <= transaction.base_revision {
                return Err(ContextStateValidationError::StatusRevisionNotAfterBase {
                    transaction_id: transaction.id.clone(),
                    base_revision: transaction.base_revision,
                    status_revision: status.revision,
                });
            }
            if status.revision > state.revision {
                return Err(ContextStateValidationError::StatusRevisionInFuture {
                    transaction_id: transaction.id.clone(),
                    status_revision: status.revision,
                    state_revision: state.revision,
                });
            }
            if let Some(previous) = previous_revision
                && status.revision <= previous
            {
                return Err(ContextStateValidationError::NonMonotonicStatusRevision {
                    transaction_id: transaction.id.clone(),
                    previous_revision: previous,
                    revision: status.revision,
                });
            }
            if !valid_status_transition(previous_status, status.kind) {
                return Err(ContextStateValidationError::InvalidStatusTransition {
                    transaction_id: transaction.id.clone(),
                    previous: previous_status,
                    next: status.kind,
                });
            }
            previous_revision = Some(status.revision);
            previous_status = Some(status.kind);
        }
    }
    Ok(())
}

fn valid_status_transition(
    previous: Option<StoredContextTransactionStatusKind>,
    next: StoredContextTransactionStatusKind,
) -> bool {
    matches!(
        (previous, next),
        (None, StoredContextTransactionStatusKind::Applied)
            | (
                Some(
                    StoredContextTransactionStatusKind::Applied
                        | StoredContextTransactionStatusKind::Reapplied,
                ),
                StoredContextTransactionStatusKind::Reverted
                    | StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
            )
            | (
                Some(
                    StoredContextTransactionStatusKind::Reverted
                        | StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
                ),
                StoredContextTransactionStatusKind::Reapplied,
            )
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuralValidationError {
    ResultLostItsCall { tool_use_id: String },
    CallLostItsResult { tool_use_id: String },
    DuplicateProjectedCall { tool_use_id: String, count: usize },
    DuplicateProjectedResult { tool_use_id: String, count: usize },
    ToolThoughtSignatureChanged { tool_use_id: String },
}

impl fmt::Display for StructuralValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultLostItsCall { tool_use_id } => {
                write!(formatter, "tool result lost its call: {tool_use_id}")
            }
            Self::CallLostItsResult { tool_use_id } => {
                write!(formatter, "tool call lost its result: {tool_use_id}")
            }
            Self::DuplicateProjectedCall { tool_use_id, count } => write!(
                formatter,
                "projected context contains {count} calls for tool ID {tool_use_id}"
            ),
            Self::DuplicateProjectedResult { tool_use_id, count } => write!(
                formatter,
                "projected context contains {count} results for tool ID {tool_use_id}"
            ),
            Self::ToolThoughtSignatureChanged { tool_use_id } => write!(
                formatter,
                "tool thought signature changed during projection for {tool_use_id}"
            ),
        }
    }
}

impl Error for StructuralValidationError {}

#[derive(Default)]
struct ToolShape {
    calls: HashMap<String, usize>,
    results: HashMap<String, usize>,
    thought_signatures: HashMap<String, Option<String>>,
}

impl ToolShape {
    fn from_messages(messages: &[Message]) -> Self {
        let mut shape = Self::default();
        for message in messages {
            for block in &message.content {
                match block {
                    ContentBlock::ToolUse {
                        id,
                        thought_signature,
                        ..
                    } => {
                        *shape.calls.entry(id.clone()).or_default() += 1;
                        shape
                            .thought_signatures
                            .entry(id.clone())
                            .or_insert_with(|| thought_signature.clone());
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        *shape.results.entry(tool_use_id.clone()).or_default() += 1;
                    }
                    _ => {}
                }
            }
        }
        shape
    }
}

pub fn validate_projected_structure(
    raw: &[StoredMessage],
    projected: &[Message],
) -> Result<StructuralValidation, StructuralValidationError> {
    let raw_messages = raw
        .iter()
        .map(StoredMessage::to_message)
        .collect::<Vec<_>>();
    let raw_shape = ToolShape::from_messages(&raw_messages);
    let projected_shape = ToolShape::from_messages(projected);
    let mut warnings = Vec::new();
    let tool_ids = raw_shape
        .calls
        .keys()
        .chain(raw_shape.results.keys())
        .chain(projected_shape.calls.keys())
        .chain(projected_shape.results.keys())
        .cloned()
        .collect::<HashSet<_>>();

    for tool_use_id in tool_ids {
        let raw_calls = raw_shape
            .calls
            .get(&tool_use_id)
            .copied()
            .unwrap_or_default();
        let raw_results = raw_shape
            .results
            .get(&tool_use_id)
            .copied()
            .unwrap_or_default();
        let projected_calls = projected_shape
            .calls
            .get(&tool_use_id)
            .copied()
            .unwrap_or_default();
        let projected_results = projected_shape
            .results
            .get(&tool_use_id)
            .copied()
            .unwrap_or_default();

        if projected_calls > 1 && raw_calls <= 1 {
            return Err(StructuralValidationError::DuplicateProjectedCall {
                tool_use_id,
                count: projected_calls,
            });
        }
        if projected_results > 1 && raw_results <= 1 {
            return Err(StructuralValidationError::DuplicateProjectedResult {
                tool_use_id,
                count: projected_results,
            });
        }
        if raw_calls > 0 && projected_calls == 0 && projected_results > 0 {
            return Err(StructuralValidationError::ResultLostItsCall { tool_use_id });
        }
        if raw_results > 0 && projected_results == 0 && projected_calls > 0 {
            return Err(StructuralValidationError::CallLostItsResult { tool_use_id });
        }
        if raw_calls == 0 && projected_results > 0 {
            warnings.push(format!(
                "inherited tool result without a stored call: {tool_use_id}"
            ));
        }
        if raw_results == 0 && projected_calls > 0 {
            warnings.push(format!(
                "inherited tool call without a stored result: {tool_use_id}"
            ));
        }

        if projected_calls > 0
            && raw_shape.thought_signatures.get(&tool_use_id)
                != projected_shape.thought_signatures.get(&tool_use_id)
        {
            return Err(StructuralValidationError::ToolThoughtSignatureChanged { tool_use_id });
        }
    }

    Ok(StructuralValidation { warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use jcode_message_types::Role;
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextStatusEvent, StoredContextTransaction,
    };

    fn stored(role: Role, content: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            id: format!("m-{}", content.len()),
            role,
            content,
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    #[test]
    fn state_rejects_duplicate_transaction_ids_and_invalid_transitions() {
        let transaction = StoredContextTransaction {
            id: "duplicate".to_string(),
            base_revision: 0,
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: Vec::new(),
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        };
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![transaction.clone(), transaction],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&state),
            Err(ContextStateValidationError::DuplicateTransactionId { .. })
        ));

        let invalid_transition = StoredContextViewState {
            revision: 2,
            transactions: vec![StoredContextTransaction {
                id: "invalid-transition".to_string(),
                base_revision: 0,
                created_at: Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations: Vec::new(),
                status_events: vec![
                    StoredContextStatusEvent {
                        revision: 1,
                        timestamp: Utc::now(),
                        kind: StoredContextTransactionStatusKind::Applied,
                        reason: None,
                    },
                    StoredContextStatusEvent {
                        revision: 2,
                        timestamp: Utc::now(),
                        kind: StoredContextTransactionStatusKind::Applied,
                        reason: None,
                    },
                ],
                application: None,
                economics: None,
                curator_usage: Vec::new(),
                emergency_audit: None,
            }],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&invalid_transition),
            Err(ContextStateValidationError::InvalidStatusTransition { .. })
        ));
    }

    #[test]
    fn state_folds_applied_reverted_reapplied_and_invalidated_statuses() {
        let transaction = StoredContextTransaction {
            id: "history".to_string(),
            base_revision: 0,
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: Vec::new(),
            status_events: vec![
                StoredContextStatusEvent {
                    revision: 1,
                    timestamp: Utc::now(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: None,
                },
                StoredContextStatusEvent {
                    revision: 2,
                    timestamp: Utc::now(),
                    kind: StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
                    reason: Some("rewind".to_string()),
                },
                StoredContextStatusEvent {
                    revision: 3,
                    timestamp: Utc::now(),
                    kind: StoredContextTransactionStatusKind::Reapplied,
                    reason: None,
                },
                StoredContextStatusEvent {
                    revision: 4,
                    timestamp: Utc::now(),
                    kind: StoredContextTransactionStatusKind::Reverted,
                    reason: None,
                },
            ],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        };
        let state = StoredContextViewState {
            revision: 4,
            transactions: vec![transaction],
            ..StoredContextViewState::default()
        };

        validate_context_state(&state).expect("valid status history");
        assert_eq!(state.active_transaction_count(), 0);
        assert_eq!(
            state.transactions[0]
                .latest_status()
                .map(|event| event.kind),
            Some(StoredContextTransactionStatusKind::Reverted)
        );
    }

    #[test]
    fn state_rejects_missing_future_and_non_monotonic_status_revisions() {
        let base = StoredContextTransaction {
            id: "tx".to_string(),
            base_revision: 0,
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: Vec::new(),
            status_events: Vec::new(),
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        };

        let missing = StoredContextViewState {
            transactions: vec![base.clone()],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&missing),
            Err(ContextStateValidationError::MissingStatusHistory { .. })
        ));

        let mut not_after_base = base.clone();
        not_after_base.base_revision = 1;
        not_after_base.status_events = vec![StoredContextStatusEvent {
            revision: 1,
            timestamp: Utc::now(),
            kind: StoredContextTransactionStatusKind::Applied,
            reason: None,
        }];
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![not_after_base],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&state),
            Err(ContextStateValidationError::StatusRevisionNotAfterBase { .. })
        ));

        let mut future = base.clone();
        future.status_events = vec![StoredContextStatusEvent {
            revision: 2,
            timestamp: Utc::now(),
            kind: StoredContextTransactionStatusKind::Applied,
            reason: None,
        }];
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![future],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&state),
            Err(ContextStateValidationError::StatusRevisionInFuture { .. })
        ));

        let mut non_monotonic = base;
        non_monotonic.status_events = vec![
            StoredContextStatusEvent {
                revision: 2,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            },
            StoredContextStatusEvent {
                revision: 1,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reverted,
                reason: None,
            },
        ];
        let state = StoredContextViewState {
            revision: 2,
            transactions: vec![non_monotonic],
            ..StoredContextViewState::default()
        };
        assert!(matches!(
            validate_context_state(&state),
            Err(ContextStateValidationError::NonMonotonicStatusRevision { .. })
        ));
    }

    #[test]
    fn projection_may_remove_a_complete_tool_pair_but_not_one_side() {
        let raw = vec![
            stored(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call".to_string(),
                    name: "tool".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: Some("signature".to_string()),
                }],
            ),
            stored(
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: "result".to_string(),
                    is_error: None,
                }],
            ),
        ];
        assert!(validate_projected_structure(&raw, &[]).is_ok());
        assert!(matches!(
            validate_projected_structure(&raw, &[raw[0].to_message()]),
            Err(StructuralValidationError::CallLostItsResult { .. })
        ));
    }
}
