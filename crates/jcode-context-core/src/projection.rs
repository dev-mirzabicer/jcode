use crate::{
    ContextStateValidationError, ContextTargetIndex, MessageRangeResolutionError,
    RangeClosureError, StructuralValidationError, TargetResolutionError, close_message_range,
    estimate_messages_tokens, validate_context_state, validate_projected_structure,
};
use jcode_message_types::{ContentBlock, Message, Role};
use jcode_session_types::{
    StoredContextBlockKind, StoredContextOperation, StoredContextOperationCounts,
    StoredContextPathEvidence, StoredContextViewState, StoredMessage, StoredMessageRange,
    StoredRangeSummary, StoredToolResultDistillation,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContextOperationRef {
    pub transaction_id: String,
    pub operation_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectedMessageSource {
    RawMessage {
        message_id: String,
        stored_index: usize,
    },
    RangeSummary {
        operation: ContextOperationRef,
        source_range: StoredMessageRange,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowedContextOperation {
    pub operation: ContextOperationRef,
    pub target_message_id: String,
    pub covering_summary: ContextOperationRef,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextProjectionDiagnostics {
    pub active_transaction_count: usize,
    pub effective_operation_counts: StoredContextOperationCounts,
    pub shadowed_operations: Vec<ShadowedContextOperation>,
    pub removed_assistant_message_ids: Vec<String>,
    pub raw_provider_token_estimate: usize,
    pub projected_provider_token_estimate: usize,
    pub validation_warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ContextProjection {
    pub messages: Vec<Message>,
    pub sources: Vec<ProjectedMessageSource>,
    pub diagnostics: ContextProjectionDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextProjectionError {
    State(ContextStateValidationError),
    Range {
        operation: ContextOperationRef,
        source: MessageRangeResolutionError,
    },
    Target {
        operation: ContextOperationRef,
        source: TargetResolutionError,
    },
    RangeClosure {
        operation: ContextOperationRef,
        source: RangeClosureError,
    },
    RangeNotStructurallyClosed {
        operation: ContextOperationRef,
        stored: (usize, usize),
        required: (usize, usize),
    },
    OverlappingRangeSummaries {
        first: ContextOperationRef,
        second: ContextOperationRef,
        overlap: (usize, usize),
    },
    EmptyRangeSummary {
        operation: ContextOperationRef,
    },
    InvalidOperationTarget {
        operation: ContextOperationRef,
        expected: &'static str,
        actual: StoredContextBlockKind,
    },
    DuplicateBlockTransform {
        first: ContextOperationRef,
        second: ContextOperationRef,
        message_index: usize,
        block_index: usize,
    },
    DistillationRatioRejected {
        operation: ContextOperationRef,
        original_tokens: usize,
        replacement_tokens: usize,
    },
    DistillationRatioMismatch {
        operation: ContextOperationRef,
        stored_millionths: u32,
        calculated_millionths: Option<u32>,
    },
    Structure(StructuralValidationError),
}

impl fmt::Display for ContextProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => error.fmt(formatter),
            Self::Range { operation, source } => {
                write!(
                    formatter,
                    "{} range is stale: {source}",
                    operation_label(operation)
                )
            }
            Self::Target { operation, source } => {
                write!(
                    formatter,
                    "{} target is stale: {source}",
                    operation_label(operation)
                )
            }
            Self::RangeClosure { operation, source } => write!(
                formatter,
                "{} range closure failed: {source}",
                operation_label(operation)
            ),
            Self::RangeNotStructurallyClosed {
                operation,
                stored,
                required,
            } => write!(
                formatter,
                "{} range {:?} is not structurally closed; required {:?}",
                operation_label(operation),
                stored,
                required
            ),
            Self::OverlappingRangeSummaries {
                first,
                second,
                overlap,
            } => write!(
                formatter,
                "active range summaries {} and {} overlap at {}..={}",
                operation_label(first),
                operation_label(second),
                overlap.0,
                overlap.1
            ),
            Self::EmptyRangeSummary { operation } => {
                write!(
                    formatter,
                    "{} has an empty summary",
                    operation_label(operation)
                )
            }
            Self::InvalidOperationTarget {
                operation,
                expected,
                actual,
            } => write!(
                formatter,
                "{} targets {actual:?}; expected {expected}",
                operation_label(operation)
            ),
            Self::DuplicateBlockTransform {
                first,
                second,
                message_index,
                block_index,
            } => write!(
                formatter,
                "{} and {} both transform message {message_index} block {block_index}",
                operation_label(first),
                operation_label(second)
            ),
            Self::DistillationRatioRejected {
                operation,
                original_tokens,
                replacement_tokens,
            } => write!(
                formatter,
                "{} distillation is not strictly below 20%: {replacement_tokens}/{original_tokens} tokens",
                operation_label(operation)
            ),
            Self::DistillationRatioMismatch {
                operation,
                stored_millionths,
                calculated_millionths,
            } => write!(
                formatter,
                "{} stored distillation ratio {stored_millionths}ppm does not match calculated {calculated_millionths:?}",
                operation_label(operation)
            ),
            Self::Structure(error) => error.fmt(formatter),
        }
    }
}

impl Error for ContextProjectionError {}

fn operation_label(operation: &ContextOperationRef) -> String {
    format!(
        "transaction {} operation {}",
        operation.transaction_id, operation.operation_index
    )
}

struct ResolvedRangeOperation<'a> {
    operation: ContextOperationRef,
    start: usize,
    end: usize,
    summary: &'a StoredRangeSummary,
}

struct ResolvedDistillation<'a> {
    operation: ContextOperationRef,
    distillation: &'a StoredToolResultDistillation,
}

pub fn project_context(
    raw: &[StoredMessage],
    state: &StoredContextViewState,
) -> Result<ContextProjection, ContextProjectionError> {
    validate_context_state(state).map_err(ContextProjectionError::State)?;
    let raw_provider_messages = raw
        .iter()
        .map(StoredMessage::to_message)
        .collect::<Vec<_>>();
    let target_index = ContextTargetIndex::new(raw);
    let mut ranges = Vec::new();
    let mut suppression_targets = Vec::new();
    let mut distillation_targets = Vec::new();

    for transaction in state.active_transactions() {
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            let operation_ref = ContextOperationRef {
                transaction_id: transaction.id.clone(),
                operation_index,
            };
            match operation {
                StoredContextOperation::RangeSummary(summary) => {
                    if summary.summary_text.trim().is_empty() {
                        return Err(ContextProjectionError::EmptyRangeSummary {
                            operation: operation_ref,
                        });
                    }
                    let (start, end) = target_index
                        .resolve_message_range(&summary.source_range)
                        .map_err(|source| ContextProjectionError::Range {
                            operation: operation_ref.clone(),
                            source,
                        })?;
                    let closed =
                        close_message_range(raw, &StoredContextViewState::default(), start, end)
                            .map_err(|source| ContextProjectionError::RangeClosure {
                                operation: operation_ref.clone(),
                                source,
                            })?;
                    if (closed.start, closed.end) != (start, end) {
                        return Err(ContextProjectionError::RangeNotStructurallyClosed {
                            operation: operation_ref,
                            stored: (start, end),
                            required: (closed.start, closed.end),
                        });
                    }
                    ranges.push(ResolvedRangeOperation {
                        operation: operation_ref,
                        start,
                        end,
                        summary,
                    });
                }
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    for target in &suppression.targets {
                        let resolved =
                            target_index
                                .resolve_content_target(target)
                                .map_err(|source| ContextProjectionError::Target {
                                    operation: operation_ref.clone(),
                                    source,
                                })?;
                        if !matches!(
                            target.kind,
                            StoredContextBlockKind::Reasoning
                                | StoredContextBlockKind::AnthropicThinking
                                | StoredContextBlockKind::OpenAiReasoning
                        ) {
                            return Err(ContextProjectionError::InvalidOperationTarget {
                                operation: operation_ref,
                                expected: "a provider-replayed reasoning block",
                                actual: target.kind,
                            });
                        }
                        suppression_targets.push((resolved, operation_ref.clone(), target));
                    }
                }
                StoredContextOperation::ToolResultDistillation(distillation) => {
                    let resolved = target_index
                        .resolve_content_target(&distillation.target)
                        .map_err(|source| ContextProjectionError::Target {
                            operation: operation_ref.clone(),
                            source,
                        })?;
                    if distillation.target.kind != StoredContextBlockKind::ToolResult {
                        return Err(ContextProjectionError::InvalidOperationTarget {
                            operation: operation_ref,
                            expected: "a tool result block",
                            actual: distillation.target.kind,
                        });
                    }
                    if !distillation.is_strictly_below_percent(20) {
                        return Err(ContextProjectionError::DistillationRatioRejected {
                            operation: operation_ref,
                            original_tokens: distillation.original_token_estimate,
                            replacement_tokens: distillation.replacement_token_estimate,
                        });
                    }
                    let calculated = distillation.calculated_replacement_ratio_millionths();
                    if calculated != Some(distillation.replacement_ratio_millionths) {
                        return Err(ContextProjectionError::DistillationRatioMismatch {
                            operation: operation_ref,
                            stored_millionths: distillation.replacement_ratio_millionths,
                            calculated_millionths: calculated,
                        });
                    }
                    distillation_targets.push((
                        resolved,
                        ResolvedDistillation {
                            operation: operation_ref,
                            distillation,
                        },
                    ));
                }
            }
        }
    }

    ranges.sort_by_key(|range| (range.start, range.end));
    for pair in ranges.windows(2) {
        if pair[0].end >= pair[1].start {
            return Err(ContextProjectionError::OverlappingRangeSummaries {
                first: pair[0].operation.clone(),
                second: pair[1].operation.clone(),
                overlap: (pair[1].start, pair[0].end.min(pair[1].end)),
            });
        }
    }

    let mut diagnostics = ContextProjectionDiagnostics {
        active_transaction_count: state.active_transaction_count(),
        raw_provider_token_estimate: estimate_messages_tokens(&raw_provider_messages),
        ..ContextProjectionDiagnostics::default()
    };
    diagnostics.effective_operation_counts.range_summaries = ranges.len();

    let mut suppression_map = HashMap::new();
    for (resolved, operation, target) in suppression_targets {
        if let Some(covering) = range_covering(&ranges, resolved.message_index) {
            diagnostics
                .shadowed_operations
                .push(ShadowedContextOperation {
                    operation,
                    target_message_id: target.message_id.clone(),
                    covering_summary: covering.operation.clone(),
                });
            continue;
        }
        insert_unique_transform(
            &mut suppression_map,
            (resolved.message_index, resolved.block_index),
            operation,
        )?;
    }
    diagnostics
        .effective_operation_counts
        .reasoning_suppressions = suppression_map.values().collect::<HashSet<_>>().len();

    let mut distillation_map: HashMap<(usize, usize), ResolvedDistillation<'_>> = HashMap::new();
    for (resolved, distillation) in distillation_targets {
        if let Some(covering) = range_covering(&ranges, resolved.message_index) {
            diagnostics
                .shadowed_operations
                .push(ShadowedContextOperation {
                    operation: distillation.operation,
                    target_message_id: distillation.distillation.target.message_id.clone(),
                    covering_summary: covering.operation.clone(),
                });
            continue;
        }
        let key = (resolved.message_index, resolved.block_index);
        if let Some(existing) = distillation_map.get(&key) {
            return Err(ContextProjectionError::DuplicateBlockTransform {
                first: existing.operation.clone(),
                second: distillation.operation,
                message_index: key.0,
                block_index: key.1,
            });
        }
        distillation_map.insert(key, distillation);
    }
    diagnostics
        .effective_operation_counts
        .tool_result_distillations = distillation_map.len();

    let mut messages = Vec::new();
    let mut sources = Vec::new();
    let mut raw_index = 0usize;
    let mut range_index = 0usize;
    while raw_index < raw.len() {
        if let Some(range) = ranges.get(range_index)
            && raw_index == range.start
        {
            messages.push(summary_message(raw, range));
            sources.push(ProjectedMessageSource::RangeSummary {
                operation: range.operation.clone(),
                source_range: range.summary.source_range.clone(),
            });
            raw_index = range.end + 1;
            range_index += 1;
            continue;
        }

        let source = &raw[raw_index];
        let mut message = source.to_message();
        let mut projected_content = Vec::with_capacity(message.content.len());
        let mut suppressed_replay_block = false;
        for (block_index, block) in message.content.into_iter().enumerate() {
            let key = (raw_index, block_index);
            if suppression_map.contains_key(&key) {
                suppressed_replay_block = true;
                continue;
            }
            if let Some(distillation) = distillation_map.get(&key) {
                let ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } = block
                else {
                    return Err(ContextProjectionError::InvalidOperationTarget {
                        operation: distillation.operation.clone(),
                        expected: "a tool result block",
                        actual: crate::context_block_kind(&block),
                    });
                };
                projected_content.push(ContentBlock::ToolResult {
                    tool_use_id,
                    content: distillation.distillation.replacement_content.clone(),
                    is_error,
                });
            } else {
                projected_content.push(block);
            }
        }
        message.content = projected_content;

        if message.role == Role::Assistant
            && suppressed_replay_block
            && !message.content.iter().any(provider_replayed_block)
        {
            diagnostics
                .removed_assistant_message_ids
                .push(source.id.clone());
        } else {
            messages.push(message);
            sources.push(ProjectedMessageSource::RawMessage {
                message_id: source.id.clone(),
                stored_index: raw_index,
            });
        }
        raw_index += 1;
    }

    let validation =
        validate_projected_structure(raw, &messages).map_err(ContextProjectionError::Structure)?;
    diagnostics.validation_warnings = validation.warnings;
    diagnostics.projected_provider_token_estimate = estimate_messages_tokens(&messages);

    debug_assert_eq!(messages.len(), sources.len());
    Ok(ContextProjection {
        messages,
        sources,
        diagnostics,
    })
}

fn insert_unique_transform(
    transforms: &mut HashMap<(usize, usize), ContextOperationRef>,
    key: (usize, usize),
    operation: ContextOperationRef,
) -> Result<(), ContextProjectionError> {
    if let Some(existing) = transforms.insert(key, operation.clone()) {
        return Err(ContextProjectionError::DuplicateBlockTransform {
            first: existing,
            second: operation,
            message_index: key.0,
            block_index: key.1,
        });
    }
    Ok(())
}

fn range_covering<'ranges, 'summary>(
    ranges: &'ranges [ResolvedRangeOperation<'summary>],
    message_index: usize,
) -> Option<&'ranges ResolvedRangeOperation<'summary>> {
    ranges
        .iter()
        .find(|range| message_index >= range.start && message_index <= range.end)
}

fn provider_replayed_block(block: &ContentBlock) -> bool {
    !matches!(block, ContentBlock::ReasoningTrace { .. })
}

fn summary_message(raw: &[StoredMessage], range: &ResolvedRangeOperation<'_>) -> Message {
    let mut text = format!(
        "## Selected Conversation Summary\n\nCovered historical range: stored messages {} through {} ({} messages)\n\n{}",
        range.start + 1,
        range.end + 1,
        range.end - range.start + 1,
        range.summary.summary_text.trim()
    );
    if let Some(evidence) = range.summary.file_evidence.as_ref() {
        if !range.summary.file_change_digest.trim().is_empty() {
            text.push_str("\n\n### Curator file-change digest\n");
            text.push_str(range.summary.file_change_digest.trim());
        }
        text.push_str("\n\n### Harness-generated file evidence");
        append_file_evidence_category(&mut text, "Files changed", &evidence.changed);
        append_file_evidence_category(
            &mut text,
            "Files read or inspected",
            &evidence.read_or_inspected,
        );
        append_file_evidence_category(
            &mut text,
            "Paths searched or browsed",
            &evidence.searched_or_browsed,
        );
    } else if !range.summary.file_change_digest.trim().is_empty() {
        text.push_str("\n\n### Files changed in this range\n");
        text.push_str(range.summary.file_change_digest.trim());
    } else if !range.summary.changed_files.is_empty() {
        text.push_str("\n\n### Files changed in this range\n");
        for path in &range.summary.changed_files {
            text.push_str("\n- ");
            text.push_str(path);
        }
    }
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
        timestamp: raw.get(range.start).and_then(|message| message.timestamp),
        tool_duration_ms: None,
    }
}

fn append_file_evidence_category(
    text: &mut String,
    label: &str,
    evidence: &StoredContextPathEvidence,
) {
    let completeness = if evidence.complete {
        "complete"
    } else {
        "incomplete"
    };
    text.push_str(&format!("\n\n#### {label} · {completeness}"));
    if evidence.paths.is_empty() {
        text.push_str("\n- None observed by supported structured tools.");
    } else {
        for path in &evidence.paths {
            text.push_str("\n- ");
            text.push_str(path);
        }
    }
    for warning in &evidence.warnings {
        text.push_str("\n- Evidence warning: ");
        text.push_str(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analyze_cache_prefix, build_content_target, build_message_range,
        resolve_reasoning_suppression_keep_latest,
    };
    use chrono::Utc;
    use jcode_session_types::{
        StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextStatusEvent,
        StoredContextTransaction, StoredContextTransactionStatusKind, StoredRangeSummary,
        StoredReasoningSelection, StoredReasoningSuppression, StoredToolResultDistillation,
    };

    fn stored(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
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

    fn text(id: &str, role: Role, text: &str) -> StoredMessage {
        stored(
            id,
            role,
            vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        )
    }

    fn generator() -> StoredContextArtifactGenerator {
        StoredContextArtifactGenerator {
            provider: "provider".to_string(),
            model: "model".to_string(),
            route: "route".to_string(),
            prompt_version: "v1".to_string(),
            effort: None,
            role: None,
            selection_source: None,
            transaction_instructions: None,
            task_instructions: None,
        }
    }

    fn summary(raw: &[StoredMessage], start: usize, end: usize, text: &str) -> StoredRangeSummary {
        StoredRangeSummary {
            source_range: build_message_range(raw, start, end).expect("range"),
            summary_text: text.to_string(),
            file_change_digest: String::new(),
            changed_files: Vec::new(),
            change_evidence_complete: false,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: Some(generator()),
            source_token_estimate: 1_000,
            replacement_token_estimate: 100,
            warnings: Vec::new(),
            created_at: Utc::now(),
            legacy_coverage: None,
        }
    }

    fn applied_state(operations: Vec<StoredContextOperation>) -> StoredContextViewState {
        StoredContextViewState {
            revision: 1,
            transactions: vec![StoredContextTransaction {
                id: "tx".to_string(),
                base_revision: 0,
                created_at: Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations,
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
            }],
            ..StoredContextViewState::default()
        }
    }

    fn suppression(
        raw: &[StoredMessage],
        targets: &[(usize, usize)],
    ) -> StoredReasoningSuppression {
        let targets = targets
            .iter()
            .map(|(message_index, block_index)| {
                build_content_target(raw, *message_index, *block_index).expect("target")
            })
            .collect::<Vec<_>>();
        let mut replay_block_kinds = targets.iter().map(|target| target.kind).collect::<Vec<_>>();
        replay_block_kinds.sort();
        replay_block_kinds.dedup();
        StoredReasoningSuppression {
            selection: StoredReasoningSelection::MessageRanges { ranges: Vec::new() },
            assistant_turns_affected: targets
                .iter()
                .map(|target| target.message_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            original_token_estimate: targets.len().saturating_mul(10),
            targets,
            replay_block_kinds,
            validation_evidence_version: 1,
            validation: Vec::new(),
        }
    }

    fn distillation(
        raw: &[StoredMessage],
        message_index: usize,
        block_index: usize,
        replacement_content: &str,
        original_tokens: usize,
        replacement_tokens: usize,
    ) -> StoredToolResultDistillation {
        let target = build_content_target(raw, message_index, block_index).expect("result target");
        let tool_call_id = target.semantic_id.clone().expect("tool result semantic ID");
        StoredToolResultDistillation {
            target,
            tool_name: "tool".to_string(),
            tool_call_id,
            replacement_content: replacement_content.to_string(),
            original_token_estimate: original_tokens,
            replacement_token_estimate: replacement_tokens,
            replacement_ratio_millionths: if original_tokens == 0 {
                0
            } else {
                u32::try_from(
                    (replacement_tokens as u128).saturating_mul(1_000_000)
                        / original_tokens as u128,
                )
                .unwrap_or(u32::MAX)
            },
            preservation_rationale: "fixture".to_string(),
            uncertainties: Vec::new(),
            generator: generator(),
            created_at: Utc::now(),
        }
    }

    fn project_context(
        raw: &[StoredMessage],
        state: &StoredContextViewState,
    ) -> Result<ContextProjection, ContextProjectionError> {
        let before = serde_json::to_vec(raw).expect("serialize raw before projection");
        let result = super::project_context(raw, state);
        assert_eq!(
            serde_json::to_vec(raw).expect("serialize raw after projection"),
            before,
            "projection must never mutate the authoritative transcript"
        );
        result
    }

    fn fixture() -> Vec<StoredMessage> {
        vec![
            text("m0", Role::User, "stable"),
            stored(
                "m1",
                Role::Assistant,
                vec![
                    ContentBlock::Reasoning {
                        text: "generic reasoning".to_string(),
                    },
                    ContentBlock::Text {
                        text: "answer one".to_string(),
                        cache_control: None,
                    },
                ],
            ),
            text("m2", Role::User, "range one tail"),
            stored(
                "m3",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command":"tests"}),
                    thought_signature: Some("gemini-signature".to_string()),
                }],
            ),
            stored(
                "m4",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: "large output ".repeat(1_000),
                    is_error: Some(true),
                }],
            ),
            stored(
                "m5",
                Role::Assistant,
                vec![
                    ContentBlock::OpenAIReasoning {
                        id: "reasoning-5".to_string(),
                        summary: vec!["reason".to_string()],
                        encrypted_content: Some("encrypted".to_string()),
                        status: Some("completed".to_string()),
                    },
                    ContentBlock::Text {
                        text: "answer five".to_string(),
                        cache_control: None,
                    },
                ],
            ),
            text("m6", Role::User, "range two"),
            text("m7", Role::Assistant, "range two answer"),
            text("m8", Role::User, "current"),
        ]
    }

    #[test]
    fn combined_projection_is_atomic_deterministic_and_keeps_raw_bytes_unchanged() {
        let raw = fixture();
        let before = serde_json::to_vec(&raw).expect("serialize raw");
        let reasoning_target = build_content_target(&raw, 5, 0).expect("reasoning target");
        let result_target = build_content_target(&raw, 4, 0).expect("result target");
        let operations = vec![
            StoredContextOperation::RangeSummary(summary(&raw, 1, 2, "summary one")),
            StoredContextOperation::RangeSummary(summary(&raw, 6, 7, "summary two")),
            StoredContextOperation::ReasoningSuppression(
                jcode_session_types::StoredReasoningSuppression {
                    selection:
                        jcode_session_types::StoredReasoningSelection::KeepLatestAssistantTurns {
                            protected_recent_assistant_turns: 1,
                        },
                    targets: vec![reasoning_target],
                    assistant_turns_affected: 1,
                    replay_block_kinds: vec![StoredContextBlockKind::OpenAiReasoning],
                    original_token_estimate: 10,
                    validation_evidence_version: 1,
                    validation: Vec::new(),
                },
            ),
            StoredContextOperation::ToolResultDistillation(StoredToolResultDistillation {
                target: result_target,
                tool_name: "bash".to_string(),
                tool_call_id: "call".to_string(),
                replacement_content: "tests failed: exact error retained".to_string(),
                original_token_estimate: 1_000,
                replacement_token_estimate: 100,
                replacement_ratio_millionths: 100_000,
                preservation_rationale: "fixture".to_string(),
                uncertainties: Vec::new(),
                generator: generator(),
                created_at: Utc::now(),
            }),
        ];
        let state = applied_state(operations);
        let projection = project_context(&raw, &state).expect("projection");
        let second = project_context(&raw, &state).expect("deterministic projection");
        assert_eq!(
            serde_json::to_vec(&projection.messages).expect("serialize projection"),
            serde_json::to_vec(&second.messages).expect("serialize projection")
        );
        assert_eq!(serde_json::to_vec(&raw).expect("serialize raw"), before);
        assert_eq!(projection.messages.len(), 7);
        assert_eq!(projection.sources.len(), projection.messages.len());
        assert_eq!(
            projection.diagnostics.effective_operation_counts,
            StoredContextOperationCounts {
                range_summaries: 2,
                reasoning_suppressions: 1,
                tool_result_distillations: 1,
            }
        );
        let ContentBlock::ToolResult {
            content, is_error, ..
        } = &projection.messages[3].content[0]
        else {
            panic!("expected distilled tool result");
        };
        assert_eq!(content, "tests failed: exact error retained");
        assert_eq!(*is_error, Some(true));
        assert!(matches!(
            projection.messages[4].content.as_slice(),
            [ContentBlock::Text { .. }]
        ));
    }

    #[test]
    fn range_summary_wrapper_separates_curator_digest_from_structured_evidence_and_keeps_legacy_fallback()
     {
        let raw = fixture();
        let mut structured = summary(&raw, 1, 2, "structured summary");
        structured.file_change_digest = "Curator-authored changed-file findings.".to_string();
        structured.file_evidence = Some(jcode_session_types::StoredContextFileEvidence {
            changed: StoredContextPathEvidence {
                paths: vec!["src/lib.rs".to_string()],
                complete: true,
                warnings: Vec::new(),
            },
            read_or_inspected: StoredContextPathEvidence {
                paths: vec!["src/parser.rs".to_string()],
                complete: true,
                warnings: Vec::new(),
            },
            searched_or_browsed: StoredContextPathEvidence {
                paths: Vec::new(),
                complete: false,
                warnings: vec!["shell search scope may be incomplete".to_string()],
            },
        });
        let projection = project_context(
            &raw,
            &applied_state(vec![StoredContextOperation::RangeSummary(structured)]),
        )
        .expect("structured evidence projection");
        let structured_text = projection
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } if text.contains("structured summary") => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .expect("structured summary text");
        for expected in [
            "### Curator file-change digest",
            "### Harness-generated file evidence",
            "#### Files changed · complete",
            "src/lib.rs",
            "#### Files read or inspected · complete",
            "src/parser.rs",
            "#### Paths searched or browsed · incomplete",
            "None observed by supported structured tools.",
            "Evidence warning: shell search scope may be incomplete",
        ] {
            assert!(
                structured_text.contains(expected),
                "missing {expected}: {structured_text}"
            );
        }

        let mut legacy = summary(&raw, 1, 2, "legacy summary");
        legacy.changed_files = vec!["legacy/path.rs".to_string()];
        legacy.change_evidence_complete = true;
        let projection = project_context(
            &raw,
            &applied_state(vec![StoredContextOperation::RangeSummary(legacy)]),
        )
        .expect("legacy projection");
        let legacy_text = projection
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } if text.contains("legacy summary") => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .expect("legacy summary text");
        assert!(legacy_text.contains("### Files changed in this range"));
        assert!(legacy_text.contains("legacy/path.rs"));
        assert!(!legacy_text.contains("Harness-generated file evidence"));
    }

    #[test]
    fn revert_and_reapply_use_status_history_without_reconstructing_source() {
        let raw = fixture();
        let mut state = applied_state(vec![StoredContextOperation::RangeSummary(summary(
            &raw, 1, 2, "summary",
        ))]);
        let applied = project_context(&raw, &state).expect("applied");
        state.revision = 2;
        state.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 2,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reverted,
                reason: None,
            });
        let reverted = project_context(&raw, &state).expect("reverted");
        assert_eq!(reverted.messages.len(), raw.len());
        state.revision = 3;
        state.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 3,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reapplied,
                reason: None,
            });
        let reapplied = project_context(&raw, &state).expect("reapplied");
        assert_eq!(
            serde_json::to_vec(&applied.messages).unwrap(),
            serde_json::to_vec(&reapplied.messages).unwrap()
        );
    }

    #[test]
    fn future_appends_do_not_expand_explicit_reasoning_targets() {
        let mut raw = fixture();
        let suppression = resolve_reasoning_suppression_keep_latest(&raw, 1).expect("suppression");
        let original_targets = suppression.targets.clone();
        let state = applied_state(vec![StoredContextOperation::ReasoningSuppression(
            suppression,
        )]);
        raw.push(stored(
            "m9",
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "new reasoning".to_string(),
            }],
        ));
        let projection = project_context(&raw, &state).expect("projection after append");
        assert_eq!(state.transactions[0].operations.len(), 1);
        let StoredContextOperation::ReasoningSuppression(stored) =
            &state.transactions[0].operations[0]
        else {
            panic!("expected suppression");
        };
        assert_eq!(stored.targets, original_targets);
        assert!(projection.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Reasoning { text } if text == "new reasoning")
            })
        }));
    }

    #[test]
    fn overlapping_active_ranges_and_exact_twenty_percent_distillation_are_rejected() {
        let raw = fixture();
        let overlap = applied_state(vec![
            StoredContextOperation::RangeSummary(summary(&raw, 0, 2, "one")),
            StoredContextOperation::RangeSummary(summary(&raw, 2, 2, "two")),
        ]);
        assert!(matches!(
            project_context(&raw, &overlap),
            Err(ContextProjectionError::OverlappingRangeSummaries { .. })
        ));

        let target = build_content_target(&raw, 4, 0).expect("target");
        let ratio = applied_state(vec![StoredContextOperation::ToolResultDistillation(
            StoredToolResultDistillation {
                target,
                tool_name: "bash".to_string(),
                tool_call_id: "call".to_string(),
                replacement_content: "replacement".to_string(),
                original_token_estimate: 100,
                replacement_token_estimate: 20,
                replacement_ratio_millionths: 200_000,
                preservation_rationale: "fixture".to_string(),
                uncertainties: Vec::new(),
                generator: generator(),
                created_at: Utc::now(),
            },
        )]);
        assert!(matches!(
            project_context(&raw, &ratio),
            Err(ContextProjectionError::DistillationRatioRejected { .. })
        ));
    }

    #[test]
    fn projection_cache_analysis_reports_earliest_changed_item() {
        let raw = fixture();
        let state = applied_state(vec![StoredContextOperation::RangeSummary(summary(
            &raw, 1, 2, "summary",
        ))]);
        let projection = project_context(&raw, &state).expect("projection");
        let old = raw
            .iter()
            .map(StoredMessage::to_message)
            .collect::<Vec<_>>();
        let analysis = analyze_cache_prefix(&old, &projection.messages);
        assert_eq!(analysis.unchanged_prefix_items, 1);
        assert_eq!(analysis.earliest_changed_provider_item, Some(1));
    }

    #[test]
    fn default_projection_matches_raw_provider_conversion_including_trace_only_messages() {
        let raw = vec![
            stored(
                "trace-only",
                Role::Assistant,
                vec![ContentBlock::ReasoningTrace {
                    text: "history only".to_string(),
                }],
            ),
            stored("empty-assistant", Role::Assistant, Vec::new()),
            text("user", Role::User, "prompt"),
        ];
        let expected = raw
            .iter()
            .map(StoredMessage::to_message)
            .collect::<Vec<_>>();

        let projection =
            project_context(&raw, &StoredContextViewState::default()).expect("projection");

        assert_eq!(
            serde_json::to_vec(&projection.messages).expect("projected JSON"),
            serde_json::to_vec(&expected).expect("expected JSON")
        );
        assert!(
            projection
                .diagnostics
                .removed_assistant_message_ids
                .is_empty()
        );
        assert_eq!(projection.sources.len(), raw.len());
    }

    #[test]
    fn reasoning_suppression_removes_complete_replay_blocks_and_preserves_other_content() {
        let raw = vec![
            stored(
                "generic",
                Role::Assistant,
                vec![
                    ContentBlock::Reasoning {
                        text: "generic reasoning".to_string(),
                    },
                    ContentBlock::Text {
                        text: "answer".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call".to_string(),
                        name: "tool".to_string(),
                        input: serde_json::json!({"arg": true}),
                        thought_signature: Some("gemini-signature".to_string()),
                    },
                ],
            ),
            stored(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: "result".to_string(),
                    is_error: None,
                }],
            ),
            stored(
                "anthropic",
                Role::Assistant,
                vec![
                    ContentBlock::AnthropicThinking {
                        thinking: "thinking".to_string(),
                        signature: "signed".to_string(),
                    },
                    ContentBlock::Text {
                        text: "anthropic answer".to_string(),
                        cache_control: None,
                    },
                ],
            ),
            stored(
                "openai",
                Role::Assistant,
                vec![
                    ContentBlock::OpenAIReasoning {
                        id: "reasoning-item".to_string(),
                        summary: vec!["summary".to_string()],
                        encrypted_content: Some("encrypted".to_string()),
                        status: Some("completed".to_string()),
                    },
                    ContentBlock::Text {
                        text: "openai answer".to_string(),
                        cache_control: None,
                    },
                ],
            ),
            stored(
                "reasoning-only",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "remove entire message".to_string(),
                }],
            ),
        ];
        let state = applied_state(vec![StoredContextOperation::ReasoningSuppression(
            suppression(&raw, &[(0, 0), (2, 0), (3, 0), (4, 0)]),
        )]);

        let projection = project_context(&raw, &state).expect("projection");

        assert!(matches!(
            projection.messages[0].content.as_slice(),
            [
                ContentBlock::Text { text, .. },
                ContentBlock::ToolUse {
                    thought_signature: Some(signature),
                    ..
                }
            ] if text == "answer" && signature == "gemini-signature"
        ));
        assert!(matches!(
            projection.messages[2].content.as_slice(),
            [ContentBlock::Text { text, .. }] if text == "anthropic answer"
        ));
        assert!(matches!(
            projection.messages[3].content.as_slice(),
            [ContentBlock::Text { text, .. }] if text == "openai answer"
        ));
        assert_eq!(
            projection.diagnostics.removed_assistant_message_ids,
            vec!["reasoning-only"]
        );
        assert!(!projection.sources.iter().any(|source| matches!(
            source,
            ProjectedMessageSource::RawMessage { message_id, .. }
                if message_id == "reasoning-only"
        )));
    }

    #[test]
    fn distillation_changes_only_result_content_and_keeps_empty_result_structure() {
        let raw = vec![
            stored(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call-id".to_string(),
                    name: "tool".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: Some("signature".to_string()),
                }],
            ),
            stored(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call-id".to_string(),
                    content: "large output".repeat(100),
                    is_error: Some(true),
                }],
            ),
        ];
        let before = serde_json::to_vec(&raw).expect("raw before");
        let state = applied_state(vec![StoredContextOperation::ToolResultDistillation(
            distillation(&raw, 1, 0, "", 100, 0),
        )]);

        let projection = project_context(&raw, &state).expect("projection");

        assert_eq!(serde_json::to_vec(&raw).expect("raw after"), before);
        assert!(matches!(
            projection.messages[1].content.as_slice(),
            [ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: Some(true),
            }] if tool_use_id == "call-id" && content.is_empty()
        ));
        assert!(matches!(
            projection.messages[0].content.as_slice(),
            [ContentBlock::ToolUse {
                thought_signature: Some(signature),
                ..
            }] if signature == "signature"
        ));
    }

    #[test]
    fn stale_hash_rejects_projection_without_mutating_raw_messages() {
        let mut raw = vec![stored(
            "reasoning",
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "original".to_string(),
            }],
        )];
        let state = applied_state(vec![StoredContextOperation::ReasoningSuppression(
            suppression(&raw, &[(0, 0)]),
        )]);
        let ContentBlock::Reasoning { text } = &mut raw[0].content[0] else {
            panic!("reasoning");
        };
        *text = "changed".to_string();
        let before = serde_json::to_vec(&raw).expect("raw before failed projection");

        assert!(matches!(
            project_context(&raw, &state),
            Err(ContextProjectionError::Target {
                source: TargetResolutionError::HashMismatch { .. },
                ..
            })
        ));
        assert_eq!(
            serde_json::to_vec(&raw).expect("raw after failed projection"),
            before
        );
    }

    #[test]
    fn distillation_inside_summary_is_shadowed_and_duplicate_effective_distillations_reject() {
        let raw = fixture();
        let shadowed_state = applied_state(vec![
            StoredContextOperation::RangeSummary(summary(&raw, 3, 4, "tool summary")),
            StoredContextOperation::ToolResultDistillation(distillation(
                &raw, 4, 0, "short", 100, 5,
            )),
        ]);

        let shadowed = project_context(&raw, &shadowed_state).expect("shadowed projection");
        assert_eq!(
            shadowed
                .diagnostics
                .effective_operation_counts
                .tool_result_distillations,
            0
        );
        assert_eq!(shadowed.diagnostics.shadowed_operations.len(), 1);
        assert_eq!(
            shadowed.diagnostics.shadowed_operations[0].target_message_id,
            "m4"
        );

        let duplicate = applied_state(vec![
            StoredContextOperation::ToolResultDistillation(distillation(
                &raw, 4, 0, "first", 100, 5,
            )),
            StoredContextOperation::ToolResultDistillation(distillation(
                &raw, 4, 0, "second", 100, 5,
            )),
        ]);
        assert!(matches!(
            project_context(&raw, &duplicate),
            Err(ContextProjectionError::DuplicateBlockTransform { .. })
        ));
    }

    #[test]
    fn adjacent_prefix_and_suffix_summaries_keep_chronological_source_mapping() {
        let raw = vec![
            text("m0", Role::User, "zero"),
            text("m1", Role::Assistant, "one"),
            text("m2", Role::User, "two"),
            stored(
                "m3",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "suffix reasoning".to_string(),
                }],
            ),
            stored(
                "m4",
                Role::User,
                vec![ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "image".to_string(),
                }],
            ),
        ];
        let before = serde_json::to_vec(&raw).expect("raw before");
        let state = applied_state(vec![
            StoredContextOperation::RangeSummary(summary(&raw, 0, 0, "prefix")),
            StoredContextOperation::RangeSummary(summary(&raw, 1, 1, "adjacent")),
            StoredContextOperation::RangeSummary(summary(&raw, 3, 4, "suffix")),
        ]);

        let projection = project_context(&raw, &state).expect("projection");

        assert_eq!(serde_json::to_vec(&raw).expect("raw after"), before);
        assert_eq!(projection.messages.len(), 4);
        assert!(matches!(
            projection.sources.as_slice(),
            [
                ProjectedMessageSource::RangeSummary { source_range: first, .. },
                ProjectedMessageSource::RangeSummary { source_range: second, .. },
                ProjectedMessageSource::RawMessage { message_id, .. },
                ProjectedMessageSource::RangeSummary { source_range: third, .. },
            ] if first.start_message_id == "m0"
                && second.start_message_id == "m1"
                && message_id == "m2"
                && third.start_message_id == "m3"
                && third.end_message_id == "m4"
        ));
    }
}
