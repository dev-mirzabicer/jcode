use crate::{
    ContextStateValidationError, ContextTargetIndex, MessageRangeResolutionError,
    build_message_range, validate_context_state,
};
use jcode_message_types::ContentBlock;
use jcode_session_types::{
    StoredContextOperation, StoredContextViewState, StoredMessage, StoredMessageRange,
    StoredRangeBoundaryExpansion, StoredRangeBoundaryExpansionReason,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedMessageRange {
    pub requested_start: usize,
    pub requested_end: usize,
    pub start: usize,
    pub end: usize,
    pub expansions: Vec<StoredRangeBoundaryExpansion>,
}

impl ClosedMessageRange {
    pub fn was_expanded(&self) -> bool {
        self.start != self.requested_start || self.end != self.requested_end
    }

    pub fn to_stored_range(
        &self,
        messages: &[StoredMessage],
    ) -> Result<StoredMessageRange, MessageRangeResolutionError> {
        build_message_range(messages, self.start, self.end)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RangeClosureError {
    ContextState(ContextStateValidationError),
    EmptyTranscript,
    StartOutOfBounds {
        index: usize,
        message_count: usize,
    },
    EndOutOfBounds {
        index: usize,
        message_count: usize,
    },
    Reversed {
        start: usize,
        end: usize,
    },
    ActiveSummaryStale {
        transaction_id: String,
        source: MessageRangeResolutionError,
    },
    ClosedRangesOverlap {
        first_requested: (usize, usize),
        second_requested: (usize, usize),
        overlap: (usize, usize),
    },
}

impl fmt::Display for RangeClosureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContextState(error) => error.fmt(formatter),
            Self::EmptyTranscript => {
                write!(formatter, "cannot close a range in an empty transcript")
            }
            Self::StartOutOfBounds {
                index,
                message_count,
            } => write!(
                formatter,
                "range start {index} is outside transcript of {message_count} messages"
            ),
            Self::EndOutOfBounds {
                index,
                message_count,
            } => write!(
                formatter,
                "range end {index} is outside transcript of {message_count} messages"
            ),
            Self::Reversed { start, end } => {
                write!(formatter, "range is reversed: start {start}, end {end}")
            }
            Self::ActiveSummaryStale {
                transaction_id,
                source,
            } => write!(
                formatter,
                "active summary in transaction {transaction_id} is stale: {source}"
            ),
            Self::ClosedRangesOverlap {
                first_requested,
                second_requested,
                overlap,
            } => write!(
                formatter,
                "closed ranges overlap at {}..={}: requested {:?} and {:?}",
                overlap.0, overlap.1, first_requested, second_requested
            ),
        }
    }
}

impl Error for RangeClosureError {}

#[derive(Clone)]
struct ActiveSummaryBoundary {
    transaction_id: String,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct ToolIndex {
    calls: HashMap<String, Vec<usize>>,
    results: HashMap<String, Vec<usize>>,
    calls_by_message: HashMap<usize, Vec<String>>,
}

impl ToolIndex {
    fn build(messages: &[StoredMessage]) -> Self {
        let mut index = Self::default();
        for (message_index, message) in messages.iter().enumerate() {
            for block in &message.content {
                match block {
                    ContentBlock::ToolUse { id, .. } => {
                        index
                            .calls
                            .entry(id.clone())
                            .or_default()
                            .push(message_index);
                        index
                            .calls_by_message
                            .entry(message_index)
                            .or_default()
                            .push(id.clone());
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        index
                            .results
                            .entry(tool_use_id.clone())
                            .or_default()
                            .push(message_index);
                    }
                    _ => {}
                }
            }
        }
        index
    }
}

pub fn close_message_range(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    requested_start: usize,
    requested_end: usize,
) -> Result<ClosedMessageRange, RangeClosureError> {
    validate_context_state(state).map_err(RangeClosureError::ContextState)?;
    validate_requested_range(messages, requested_start, requested_end)?;
    let tool_index = ToolIndex::build(messages);
    let target_index = ContextTargetIndex::new(messages);
    let active_summaries = active_summary_boundaries(&target_index, state)?;
    let mut start = requested_start;
    let mut end = requested_end;
    let mut expansion_reasons = BTreeMap::new();

    loop {
        let before = (start, end);

        for summary in &active_summaries {
            if intervals_intersect(start, end, summary.start, summary.end)
                && (start > summary.start || end < summary.end)
            {
                extend_interval(
                    messages,
                    &mut start,
                    &mut end,
                    summary.start,
                    summary.end,
                    StoredRangeBoundaryExpansionReason::ExistingSummaryBoundary {
                        transaction_id: summary.transaction_id.clone(),
                    },
                    &mut expansion_reasons,
                );
            }
        }

        let mut included_call_messages = tool_index
            .calls_by_message
            .keys()
            .copied()
            .filter(|index| *index >= start && *index <= end)
            .collect::<Vec<_>>();
        included_call_messages.sort_unstable();
        for call_message in included_call_messages {
            let Some(tool_ids) = tool_index.calls_by_message.get(&call_message) else {
                continue;
            };
            if tool_ids.len() > 1 {
                for tool_id in tool_ids {
                    for result_index in tool_index
                        .results
                        .get(tool_id)
                        .into_iter()
                        .flatten()
                        .copied()
                        .collect::<Vec<_>>()
                    {
                        extend_interval(
                            messages,
                            &mut start,
                            &mut end,
                            call_message,
                            result_index,
                            StoredRangeBoundaryExpansionReason::ParallelToolGroup,
                            &mut expansion_reasons,
                        );
                    }
                }
            }
        }

        let tool_ids = tool_index
            .calls
            .keys()
            .chain(tool_index.results.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let tool_pair_scan_interval = (start, end);
        for tool_id in tool_ids {
            let call_indices = tool_index.calls.get(&tool_id).cloned().unwrap_or_default();
            let result_indices = tool_index
                .results
                .get(&tool_id)
                .cloned()
                .unwrap_or_default();
            if call_indices.is_empty() || result_indices.is_empty() {
                continue;
            }
            let group_is_touched = call_indices.iter().chain(&result_indices).any(|index| {
                *index >= tool_pair_scan_interval.0 && *index <= tool_pair_scan_interval.1
            });
            if !group_is_touched {
                continue;
            }
            let group_start = call_indices
                .iter()
                .chain(&result_indices)
                .copied()
                .min()
                .unwrap_or(start);
            let group_end = call_indices
                .iter()
                .chain(&result_indices)
                .copied()
                .max()
                .unwrap_or(end);
            extend_interval(
                messages,
                &mut start,
                &mut end,
                group_start,
                group_end,
                StoredRangeBoundaryExpansionReason::ToolPair {
                    tool_use_id: tool_id,
                },
                &mut expansion_reasons,
            );
        }

        if before == (start, end) {
            break;
        }
    }

    Ok(ClosedMessageRange {
        requested_start,
        requested_end,
        start,
        end,
        expansions: expansion_reasons
            .into_iter()
            .filter_map(|(stored_index_hint, reason)| {
                messages
                    .get(stored_index_hint)
                    .map(|message| StoredRangeBoundaryExpansion {
                        message_id: message.id.clone(),
                        stored_index_hint,
                        reason,
                    })
            })
            .collect(),
    })
}

pub fn close_message_ranges(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    requested: &[(usize, usize)],
) -> Result<Vec<ClosedMessageRange>, RangeClosureError> {
    let mut ranges = requested
        .iter()
        .map(|(start, end)| close_message_range(messages, state, *start, *end))
        .collect::<Result<Vec<_>, _>>()?;
    ranges.sort_by_key(|range| (range.start, range.end));
    for pair in ranges.windows(2) {
        let first = &pair[0];
        let second = &pair[1];
        if first.end >= second.start {
            return Err(RangeClosureError::ClosedRangesOverlap {
                first_requested: (first.requested_start, first.requested_end),
                second_requested: (second.requested_start, second.requested_end),
                overlap: (second.start, first.end.min(second.end)),
            });
        }
    }
    Ok(ranges)
}

fn validate_requested_range(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
) -> Result<(), RangeClosureError> {
    if messages.is_empty() {
        return Err(RangeClosureError::EmptyTranscript);
    }
    if start > end {
        return Err(RangeClosureError::Reversed { start, end });
    }
    if start >= messages.len() {
        return Err(RangeClosureError::StartOutOfBounds {
            index: start,
            message_count: messages.len(),
        });
    }
    if end >= messages.len() {
        return Err(RangeClosureError::EndOutOfBounds {
            index: end,
            message_count: messages.len(),
        });
    }
    Ok(())
}

fn active_summary_boundaries(
    target_index: &ContextTargetIndex<'_>,
    state: &StoredContextViewState,
) -> Result<Vec<ActiveSummaryBoundary>, RangeClosureError> {
    let mut summaries = Vec::new();
    for transaction in state.active_transactions() {
        for operation in &transaction.operations {
            let StoredContextOperation::RangeSummary(summary) = operation else {
                continue;
            };
            let (start, end) = target_index
                .resolve_message_range(&summary.source_range)
                .map_err(|source| RangeClosureError::ActiveSummaryStale {
                    transaction_id: transaction.id.clone(),
                    source,
                })?;
            summaries.push(ActiveSummaryBoundary {
                transaction_id: transaction.id.clone(),
                start,
                end,
            });
        }
    }
    Ok(summaries)
}

fn extend_interval(
    messages: &[StoredMessage],
    start: &mut usize,
    end: &mut usize,
    include_start: usize,
    include_end: usize,
    reason: StoredRangeBoundaryExpansionReason,
    expansion_reasons: &mut BTreeMap<usize, StoredRangeBoundaryExpansionReason>,
) {
    let (include_start, include_end) = if include_start <= include_end {
        (include_start, include_end)
    } else {
        (include_end, include_start)
    };
    if include_start < *start {
        for index in include_start..*start {
            if index < messages.len() {
                expansion_reasons
                    .entry(index)
                    .or_insert_with(|| reason.clone());
            }
        }
        *start = include_start;
    }
    if include_end > *end {
        for index in (*end + 1)..=include_end {
            if index < messages.len() {
                expansion_reasons
                    .entry(index)
                    .or_insert_with(|| reason.clone());
            }
        }
        *end = include_end;
    }
}

fn intervals_intersect(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start <= b_end && b_start <= a_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use jcode_message_types::Role;
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextStatusEvent, StoredContextTransaction,
        StoredContextTransactionStatusKind, StoredRangeSummary,
    };

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

    fn text(id: &str) -> StoredMessage {
        message(
            id,
            Role::User,
            vec![ContentBlock::Text {
                text: id.to_string(),
                cache_control: None,
            }],
        )
    }

    #[test]
    fn closure_expands_parallel_tool_call_and_result_group() {
        let messages = vec![
            text("m0"),
            message(
                "m1",
                Role::Assistant,
                vec![
                    ContentBlock::ToolUse {
                        id: "a".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: Some("signature".to_string()),
                    },
                    ContentBlock::ToolUse {
                        id: "b".to_string(),
                        name: "grep".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    },
                ],
            ),
            text("m2"),
            message(
                "m3",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "a".to_string(),
                    content: "a result".to_string(),
                    is_error: None,
                }],
            ),
            message(
                "m4",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "b".to_string(),
                    content: "b result".to_string(),
                    is_error: None,
                }],
            ),
        ];
        let closed = close_message_range(&messages, &StoredContextViewState::default(), 3, 3)
            .expect("close range");
        assert_eq!((closed.start, closed.end), (1, 4));
        assert!(closed.expansions.iter().any(|expansion| {
            expansion.reason == StoredRangeBoundaryExpansionReason::ParallelToolGroup
        }));
    }

    #[test]
    fn closure_handles_result_before_call_without_assuming_order() {
        let messages = vec![
            message(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: "result".to_string(),
                    is_error: None,
                }],
            ),
            text("middle"),
            message(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "call".to_string(),
                    name: "tool".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                }],
            ),
        ];
        let closed = close_message_range(&messages, &StoredContextViewState::default(), 0, 0)
            .expect("close range");
        assert_eq!((closed.start, closed.end), (0, 2));
    }

    #[test]
    fn closure_expands_across_existing_summary_boundary() {
        let messages = vec![text("m0"), text("m1"), text("m2"), text("m3")];
        let range = build_message_range(&messages, 1, 2).expect("stored range");
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![StoredContextTransaction {
                id: "tx".to_string(),
                base_revision: 0,
                created_at: Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations: vec![StoredContextOperation::RangeSummary(StoredRangeSummary {
                    source_range: range,
                    summary_text: "summary".to_string(),
                    file_change_digest: String::new(),
                    changed_files: Vec::new(),
                    change_evidence_complete: false,
                    boundary_expansions: Vec::new(),
                    generator: None,
                    source_token_estimate: 0,
                    replacement_token_estimate: 0,
                    warnings: Vec::new(),
                    created_at: Utc::now(),
                    legacy_coverage: None,
                })],
                status_events: vec![StoredContextStatusEvent {
                    revision: 1,
                    timestamp: Utc::now(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: None,
                }],
                application: None,
                economics: None,
                curator_usage: Vec::new(),
            }],
            ..StoredContextViewState::default()
        };
        let closed = close_message_range(&messages, &state, 2, 3).expect("close range");
        assert_eq!((closed.start, closed.end), (1, 3));
        assert!(matches!(
            closed.expansions[0].reason,
            StoredRangeBoundaryExpansionReason::ExistingSummaryBoundary { .. }
        ));
    }

    #[test]
    fn closure_rejects_malformed_context_state_before_using_active_boundaries() {
        let messages = vec![text("m0")];
        let state = StoredContextViewState {
            transactions: vec![StoredContextTransaction {
                id: "missing-status".to_string(),
                base_revision: 0,
                created_at: Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations: Vec::new(),
                status_events: Vec::new(),
                application: None,
                economics: None,
                curator_usage: Vec::new(),
            }],
            ..StoredContextViewState::default()
        };

        assert!(matches!(
            close_message_range(&messages, &state, 0, 0),
            Err(RangeClosureError::ContextState(
                ContextStateValidationError::MissingStatusHistory { .. }
            ))
        ));
    }

    #[test]
    fn missing_tool_result_does_not_invent_a_boundary() {
        let messages = vec![message(
            "call",
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "missing".to_string(),
                name: "tool".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
        )];
        let closed = close_message_range(&messages, &StoredContextViewState::default(), 0, 0)
            .expect("close range");
        assert_eq!((closed.start, closed.end), (0, 0));
        assert!(closed.expansions.is_empty());
    }

    #[test]
    fn multiple_disjoint_and_adjacent_ranges_remain_distinct() {
        let messages = vec![text("m0"), text("m1"), text("m2"), text("m3")];

        let disjoint = close_message_ranges(
            &messages,
            &StoredContextViewState::default(),
            &[(0, 0), (2, 3)],
        )
        .expect("disjoint ranges");
        assert_eq!(
            disjoint
                .iter()
                .map(|range| (range.start, range.end))
                .collect::<Vec<_>>(),
            vec![(0, 0), (2, 3)]
        );

        let adjacent = close_message_ranges(
            &messages,
            &StoredContextViewState::default(),
            &[(0, 0), (1, 1)],
        )
        .expect("adjacent ranges");
        assert_eq!(
            adjacent
                .iter()
                .map(|range| (range.start, range.end))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1)]
        );
    }

    #[test]
    fn tool_expansion_that_makes_closed_ranges_overlap_is_rejected() {
        let messages = vec![
            text("m0"),
            message(
                "calls",
                Role::Assistant,
                vec![
                    ContentBlock::ToolUse {
                        id: "a".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "b".to_string(),
                        name: "grep".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    },
                ],
            ),
            text("between"),
            message(
                "result-a",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "a".to_string(),
                    content: "a".to_string(),
                    is_error: None,
                }],
            ),
            message(
                "result-b",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "b".to_string(),
                    content: "b".to_string(),
                    is_error: None,
                }],
            ),
        ];

        assert!(matches!(
            close_message_ranges(
                &messages,
                &StoredContextViewState::default(),
                &[(2, 2), (3, 3)],
            ),
            Err(RangeClosureError::ClosedRangesOverlap { .. })
        ));
    }

    #[test]
    fn image_and_thought_signature_stay_with_their_logical_tool_pair() {
        let messages = vec![
            message(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "image-call".to_string(),
                    name: "screenshot".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: Some("gemini-thought-signature".to_string()),
                }],
            ),
            message(
                "result",
                Role::User,
                vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "image-call".to_string(),
                        content: "captured".to_string(),
                        is_error: None,
                    },
                    ContentBlock::Image {
                        media_type: "image/png".to_string(),
                        data: "base64".to_string(),
                    },
                ],
            ),
            text("tail"),
        ];

        let closed = close_message_range(&messages, &StoredContextViewState::default(), 1, 1)
            .expect("closed tool result");
        assert_eq!((closed.start, closed.end), (0, 1));
        assert!(matches!(
            messages[0].content.as_slice(),
            [ContentBlock::ToolUse {
                thought_signature: Some(signature),
                ..
            }] if signature == "gemini-thought-signature"
        ));
        assert!(matches!(
            messages[1].content.as_slice(),
            [ContentBlock::ToolResult { .. }, ContentBlock::Image { .. }]
        ));
    }

    #[test]
    fn closure_expansion_provenance_is_deterministic() {
        let messages = vec![
            message(
                "calls",
                Role::Assistant,
                vec![
                    ContentBlock::ToolUse {
                        id: "z-call".to_string(),
                        name: "z".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "a-call".to_string(),
                        name: "a".to_string(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    },
                ],
            ),
            text("middle"),
            message(
                "result-z",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "z-call".to_string(),
                    content: "z".to_string(),
                    is_error: None,
                }],
            ),
            message(
                "result-a",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "a-call".to_string(),
                    content: "a".to_string(),
                    is_error: None,
                }],
            ),
        ];

        let expected = close_message_range(&messages, &StoredContextViewState::default(), 2, 2)
            .expect("first closure");
        for _ in 0..32 {
            assert_eq!(
                close_message_range(&messages, &StoredContextViewState::default(), 2, 2)
                    .expect("repeat closure"),
                expected
            );
        }
    }
}
