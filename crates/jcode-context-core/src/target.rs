use crate::{extend_stable_hash, stable_hash_bytes};
use jcode_message_types::{ContentBlock, Message, Role, cache_relevant_message_value};
use jcode_session_types::{
    StoredContentTarget, StoredContextBlockKind, StoredMessage, StoredMessageRange,
    StoredReasoningSelection, StoredReasoningSuppression,
};
use std::collections::{BTreeSet, HashMap};
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedContentTarget {
    pub message_index: usize,
    pub block_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetResolutionError {
    MessageNotFound {
        message_id: String,
    },
    DuplicateMessageId {
        message_id: String,
    },
    BlockNotFound {
        message_id: String,
        kind: StoredContextBlockKind,
        semantic_id: Option<String>,
        ordinal_hint: usize,
    },
    AmbiguousSemanticTarget {
        message_id: String,
        kind: StoredContextBlockKind,
        semantic_id: String,
    },
    HashMismatch {
        message_id: String,
        block_index: usize,
        expected: u64,
        actual: u64,
    },
    MessageIndexOutOfBounds {
        index: usize,
        message_count: usize,
    },
    BlockIndexOutOfBounds {
        message_index: usize,
        block_index: usize,
        block_count: usize,
    },
}

impl fmt::Display for TargetResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageNotFound { message_id } => {
                write!(formatter, "context target message not found: {message_id}")
            }
            Self::DuplicateMessageId { message_id } => {
                write!(
                    formatter,
                    "context target message ID is ambiguous: {message_id}"
                )
            }
            Self::BlockNotFound {
                message_id,
                kind,
                semantic_id,
                ordinal_hint,
            } => write!(
                formatter,
                "context target block not found in message {message_id}: kind={kind:?}, semantic_id={semantic_id:?}, ordinal_hint={ordinal_hint}"
            ),
            Self::AmbiguousSemanticTarget {
                message_id,
                kind,
                semantic_id,
            } => write!(
                formatter,
                "context target semantic ID is ambiguous in message {message_id}: kind={kind:?}, semantic_id={semantic_id}"
            ),
            Self::HashMismatch {
                message_id,
                block_index,
                expected,
                actual,
            } => write!(
                formatter,
                "context target hash mismatch in message {message_id} block {block_index}: expected {expected:#x}, got {actual:#x}"
            ),
            Self::MessageIndexOutOfBounds {
                index,
                message_count,
            } => write!(
                formatter,
                "message index {index} is outside transcript of {message_count} messages"
            ),
            Self::BlockIndexOutOfBounds {
                message_index,
                block_index,
                block_count,
            } => write!(
                formatter,
                "block index {block_index} is outside message {message_index} with {block_count} blocks"
            ),
        }
    }
}

impl Error for TargetResolutionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRangeResolutionError {
    StartMessageNotFound {
        message_id: String,
    },
    EndMessageNotFound {
        message_id: String,
    },
    DuplicateMessageId {
        message_id: String,
    },
    Reversed {
        start_index: usize,
        end_index: usize,
    },
    MessageCountMismatch {
        expected: usize,
        actual: usize,
    },
    SourceDigestMismatch {
        expected: u64,
        actual: u64,
    },
    StartIndexOutOfBounds {
        index: usize,
        message_count: usize,
    },
    EndIndexOutOfBounds {
        index: usize,
        message_count: usize,
    },
    EmptyTranscript,
}

impl fmt::Display for MessageRangeResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartMessageNotFound { message_id } => {
                write!(
                    formatter,
                    "context range start message not found: {message_id}"
                )
            }
            Self::EndMessageNotFound { message_id } => {
                write!(
                    formatter,
                    "context range end message not found: {message_id}"
                )
            }
            Self::DuplicateMessageId { message_id } => {
                write!(
                    formatter,
                    "context range message ID is ambiguous: {message_id}"
                )
            }
            Self::Reversed {
                start_index,
                end_index,
            } => write!(
                formatter,
                "context range is reversed: start index {start_index}, end index {end_index}"
            ),
            Self::MessageCountMismatch { expected, actual } => write!(
                formatter,
                "context range message count mismatch: expected {expected}, got {actual}"
            ),
            Self::SourceDigestMismatch { expected, actual } => write!(
                formatter,
                "context range source digest mismatch: expected {expected:#x}, got {actual:#x}"
            ),
            Self::StartIndexOutOfBounds {
                index,
                message_count,
            } => write!(
                formatter,
                "context range start index {index} is outside transcript of {message_count} messages"
            ),
            Self::EndIndexOutOfBounds {
                index,
                message_count,
            } => write!(
                formatter,
                "context range end index {index} is outside transcript of {message_count} messages"
            ),
            Self::EmptyTranscript => {
                write!(formatter, "cannot resolve a range in an empty transcript")
            }
        }
    }
}

impl Error for MessageRangeResolutionError {}

/// Reusable stable-ID index for resolving many context targets against one transcript snapshot.
pub struct ContextTargetIndex<'a> {
    messages: &'a [StoredMessage],
    message_indices: HashMap<&'a str, Vec<usize>>,
}

impl<'a> ContextTargetIndex<'a> {
    pub fn new(messages: &'a [StoredMessage]) -> Self {
        let mut message_indices = HashMap::with_capacity(messages.len());
        for (index, message) in messages.iter().enumerate() {
            message_indices
                .entry(message.id.as_str())
                .or_insert_with(Vec::new)
                .push(index);
        }
        Self {
            messages,
            message_indices,
        }
    }

    pub fn resolve_content_target(
        &self,
        target: &StoredContentTarget,
    ) -> Result<ResolvedContentTarget, TargetResolutionError> {
        let message_index = self.resolve_unique_message_id(
            &target.message_id,
            target.stored_index_hint,
            |message_id| TargetResolutionError::MessageNotFound { message_id },
            |message_id| TargetResolutionError::DuplicateMessageId { message_id },
        )?;
        let message = &self.messages[message_index];

        let block_index = if let Some(semantic_id) = target.semantic_id.as_deref() {
            let matches = message
                .content
                .iter()
                .enumerate()
                .filter(|(_, block)| {
                    context_block_kind(block) == target.kind
                        && content_block_semantic_id(block) == Some(semantic_id)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [index] => *index,
                [] => resolve_ordinal_target(message, target)?,
                _ => {
                    return Err(TargetResolutionError::AmbiguousSemanticTarget {
                        message_id: target.message_id.clone(),
                        kind: target.kind,
                        semantic_id: semantic_id.to_string(),
                    });
                }
            }
        } else {
            resolve_ordinal_target(message, target)?
        };

        let actual = content_block_hash(&message.content[block_index]);
        if actual != target.expected_hash {
            return Err(TargetResolutionError::HashMismatch {
                message_id: target.message_id.clone(),
                block_index,
                expected: target.expected_hash,
                actual,
            });
        }

        Ok(ResolvedContentTarget {
            message_index,
            block_index,
        })
    }

    pub fn resolve_message_range(
        &self,
        range: &StoredMessageRange,
    ) -> Result<(usize, usize), MessageRangeResolutionError> {
        if self.messages.is_empty() {
            return Err(MessageRangeResolutionError::EmptyTranscript);
        }
        let start = self.resolve_unique_message_id(
            &range.start_message_id,
            range.start_index_hint,
            |message_id| MessageRangeResolutionError::StartMessageNotFound { message_id },
            |message_id| MessageRangeResolutionError::DuplicateMessageId { message_id },
        )?;
        let end = self.resolve_unique_message_id(
            &range.end_message_id,
            range.end_index_hint,
            |message_id| MessageRangeResolutionError::EndMessageNotFound { message_id },
            |message_id| MessageRangeResolutionError::DuplicateMessageId { message_id },
        )?;
        if start > end {
            return Err(MessageRangeResolutionError::Reversed {
                start_index: start,
                end_index: end,
            });
        }
        let actual_count = end - start + 1;
        if actual_count != range.message_count {
            return Err(MessageRangeResolutionError::MessageCountMismatch {
                expected: range.message_count,
                actual: actual_count,
            });
        }
        let actual_digest = message_range_digest(self.messages, start, end)?;
        if actual_digest != range.source_digest {
            return Err(MessageRangeResolutionError::SourceDigestMismatch {
                expected: range.source_digest,
                actual: actual_digest,
            });
        }
        Ok((start, end))
    }

    fn resolve_unique_message_id<E>(
        &self,
        message_id: &str,
        index_hint: usize,
        not_found: impl FnOnce(String) -> E,
        duplicate: impl FnOnce(String) -> E,
    ) -> Result<usize, E> {
        let matches = self.message_indices.get(message_id);
        match matches.map(Vec::as_slice) {
            Some([index]) => {
                debug_assert_eq!(
                    self.messages.get(*index).map(|message| message.id.as_str()),
                    Some(message_id)
                );
                if *index == index_hint {
                    Ok(index_hint)
                } else {
                    Ok(*index)
                }
            }
            None | Some([]) => Err(not_found(message_id.to_string())),
            Some(_) => Err(duplicate(message_id.to_string())),
        }
    }
}

pub fn context_block_kind(block: &ContentBlock) -> StoredContextBlockKind {
    match block {
        ContentBlock::Text { .. } => StoredContextBlockKind::Text,
        ContentBlock::Reasoning { .. } => StoredContextBlockKind::Reasoning,
        ContentBlock::ReasoningTrace { .. } => StoredContextBlockKind::ReasoningTrace,
        ContentBlock::AnthropicThinking { .. } => StoredContextBlockKind::AnthropicThinking,
        ContentBlock::OpenAIReasoning { .. } => StoredContextBlockKind::OpenAiReasoning,
        ContentBlock::ToolUse { .. } => StoredContextBlockKind::ToolUse,
        ContentBlock::ToolResult { .. } => StoredContextBlockKind::ToolResult,
        ContentBlock::Image { .. } => StoredContextBlockKind::Image,
        ContentBlock::OpenAICompaction { .. } => StoredContextBlockKind::OpenAiCompaction,
    }
}

pub fn content_block_semantic_id(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::OpenAIReasoning { id, .. } | ContentBlock::ToolUse { id, .. } => Some(id),
        ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
        _ => None,
    }
}

pub fn content_block_hash(block: &ContentBlock) -> u64 {
    serde_json::to_vec(block)
        .map(|bytes| stable_hash_bytes(&bytes))
        .unwrap_or_else(|_| stable_hash_bytes(format!("{block:?}").as_bytes()))
}

pub fn build_content_target(
    messages: &[StoredMessage],
    message_index: usize,
    block_index: usize,
) -> Result<StoredContentTarget, TargetResolutionError> {
    let message =
        messages
            .get(message_index)
            .ok_or(TargetResolutionError::MessageIndexOutOfBounds {
                index: message_index,
                message_count: messages.len(),
            })?;
    let block =
        message
            .content
            .get(block_index)
            .ok_or(TargetResolutionError::BlockIndexOutOfBounds {
                message_index,
                block_index,
                block_count: message.content.len(),
            })?;
    Ok(StoredContentTarget {
        message_id: message.id.clone(),
        stored_index_hint: message_index,
        block_ordinal_hint: block_index,
        kind: context_block_kind(block),
        semantic_id: content_block_semantic_id(block).map(ToOwned::to_owned),
        expected_hash: content_block_hash(block),
    })
}

pub fn resolve_content_target(
    messages: &[StoredMessage],
    target: &StoredContentTarget,
) -> Result<ResolvedContentTarget, TargetResolutionError> {
    ContextTargetIndex::new(messages).resolve_content_target(target)
}

pub fn resolve_reasoning_suppression_keep_latest(
    messages: &[StoredMessage],
    protected_recent_assistant_turns: usize,
) -> Result<StoredReasoningSuppression, TargetResolutionError> {
    let assistant_indices = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == Role::Assistant)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let protected_start = assistant_indices
        .len()
        .saturating_sub(protected_recent_assistant_turns);
    let eligible = &assistant_indices[..protected_start];
    build_reasoning_suppression(
        messages,
        eligible.iter().copied(),
        StoredReasoningSelection::KeepLatestAssistantTurns {
            protected_recent_assistant_turns,
        },
    )
}

pub fn resolve_reasoning_suppression_for_ranges(
    messages: &[StoredMessage],
    ranges: &[StoredMessageRange],
) -> Result<StoredReasoningSuppression, MessageRangeOrTargetError> {
    let target_index = ContextTargetIndex::new(messages);
    let mut eligible = BTreeSet::new();
    for range in ranges {
        let (start, end) = target_index
            .resolve_message_range(range)
            .map_err(MessageRangeOrTargetError::Range)?;
        for (index, message) in messages
            .iter()
            .enumerate()
            .take(end.saturating_add(1))
            .skip(start)
        {
            if message.role == Role::Assistant {
                eligible.insert(index);
            }
        }
    }
    build_reasoning_suppression(
        messages,
        eligible,
        StoredReasoningSelection::MessageRanges {
            ranges: ranges.to_vec(),
        },
    )
    .map_err(MessageRangeOrTargetError::Target)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRangeOrTargetError {
    Range(MessageRangeResolutionError),
    Target(TargetResolutionError),
}

impl fmt::Display for MessageRangeOrTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
        }
    }
}

impl Error for MessageRangeOrTargetError {}

fn build_reasoning_suppression(
    messages: &[StoredMessage],
    eligible_message_indices: impl IntoIterator<Item = usize>,
    selection: StoredReasoningSelection,
) -> Result<StoredReasoningSuppression, TargetResolutionError> {
    let mut targets = Vec::new();
    let mut affected_turns = 0usize;
    let mut replay_block_kinds = BTreeSet::new();
    let mut original_token_estimate = 0usize;
    for message_index in eligible_message_indices {
        let Some(message) = messages.get(message_index) else {
            return Err(TargetResolutionError::MessageIndexOutOfBounds {
                index: message_index,
                message_count: messages.len(),
            });
        };
        let mut affected = false;
        for (block_index, block) in message.content.iter().enumerate() {
            let kind = context_block_kind(block);
            if !matches!(
                kind,
                StoredContextBlockKind::Reasoning
                    | StoredContextBlockKind::AnthropicThinking
                    | StoredContextBlockKind::OpenAiReasoning
            ) {
                continue;
            }
            targets.push(build_content_target(messages, message_index, block_index)?);
            replay_block_kinds.insert(kind);
            original_token_estimate =
                original_token_estimate.saturating_add(crate::estimate_content_block_tokens(block));
            affected = true;
        }
        if affected {
            affected_turns += 1;
        }
    }
    Ok(StoredReasoningSuppression {
        selection,
        targets,
        assistant_turns_affected: affected_turns,
        replay_block_kinds: replay_block_kinds.into_iter().collect(),
        original_token_estimate,
        validation_evidence_version: 1,
        validation: Vec::new(),
    })
}

fn resolve_ordinal_target(
    message: &StoredMessage,
    target: &StoredContentTarget,
) -> Result<usize, TargetResolutionError> {
    message
        .content
        .get(target.block_ordinal_hint)
        .filter(|block| context_block_kind(block) == target.kind)
        .map(|_| target.block_ordinal_hint)
        .ok_or_else(|| TargetResolutionError::BlockNotFound {
            message_id: target.message_id.clone(),
            kind: target.kind,
            semantic_id: target.semantic_id.clone(),
            ordinal_hint: target.block_ordinal_hint,
        })
}

pub fn provider_relevant_message_digest(message: &StoredMessage) -> u64 {
    let provider_message: Message = message.to_message();
    let mut digest = stable_hash_bytes(message.id.as_bytes());
    let value = cache_relevant_message_value(&provider_message);
    let value_hash = serde_json::to_vec(&value)
        .map(|bytes| stable_hash_bytes(&bytes))
        .unwrap_or_else(|_| stable_hash_bytes(format!("{value:?}").as_bytes()));
    digest = extend_stable_hash(digest, value_hash);
    digest
}

/// Stable digest of the complete authoritative transcript representation.
///
/// Unlike [`message_range_digest`], this intentionally includes transcript-only
/// reasoning, timestamps, token usage, and display metadata. Draft identity must
/// become stale after *any* authoritative history change, even when that change
/// would not alter provider cache semantics.
pub fn authoritative_transcript_digest(messages: &[StoredMessage]) -> u64 {
    serde_json::to_vec(messages)
        .map(|bytes| stable_hash_bytes(&bytes))
        .unwrap_or_else(|_| stable_hash_bytes(format!("{messages:?}").as_bytes()))
}

pub fn message_range_digest(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
) -> Result<u64, MessageRangeResolutionError> {
    if messages.is_empty() {
        return Err(MessageRangeResolutionError::EmptyTranscript);
    }
    if start > end {
        return Err(MessageRangeResolutionError::Reversed {
            start_index: start,
            end_index: end,
        });
    }
    if start >= messages.len() {
        return Err(MessageRangeResolutionError::StartIndexOutOfBounds {
            index: start,
            message_count: messages.len(),
        });
    }
    if end >= messages.len() {
        return Err(MessageRangeResolutionError::EndIndexOutOfBounds {
            index: end,
            message_count: messages.len(),
        });
    }

    Ok(messages[start..=end]
        .iter()
        .fold(STABLE_RANGE_HASH_SEED, |digest, message| {
            extend_stable_hash(digest, provider_relevant_message_digest(message))
        }))
}

const STABLE_RANGE_HASH_SEED: u64 = 0x8d58_ac26_afe1_2e47;

pub fn build_message_range(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
) -> Result<StoredMessageRange, MessageRangeResolutionError> {
    if messages.is_empty() {
        return Err(MessageRangeResolutionError::EmptyTranscript);
    }
    if start > end {
        return Err(MessageRangeResolutionError::Reversed {
            start_index: start,
            end_index: end,
        });
    }
    let Some(start_message) = messages.get(start) else {
        return Err(MessageRangeResolutionError::StartIndexOutOfBounds {
            index: start,
            message_count: messages.len(),
        });
    };
    let Some(end_message) = messages.get(end) else {
        return Err(MessageRangeResolutionError::EndIndexOutOfBounds {
            index: end,
            message_count: messages.len(),
        });
    };
    Ok(StoredMessageRange {
        start_message_id: start_message.id.clone(),
        end_message_id: end_message.id.clone(),
        start_index_hint: start,
        end_index_hint: end,
        source_digest: message_range_digest(messages, start, end)?,
        message_count: end - start + 1,
    })
}

pub fn resolve_message_range(
    messages: &[StoredMessage],
    range: &StoredMessageRange,
) -> Result<(usize, usize), MessageRangeResolutionError> {
    ContextTargetIndex::new(messages).resolve_message_range(range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::{ContentBlock, Role};

    fn stored_with_role(id: &str, role: Role, blocks: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            id: id.to_string(),
            role,
            content: blocks,
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn stored(id: &str, blocks: Vec<ContentBlock>) -> StoredMessage {
        stored_with_role(id, Role::Assistant, blocks)
    }

    #[test]
    fn semantic_target_survives_block_reordering_but_not_content_change() {
        let result = ContentBlock::ToolResult {
            tool_use_id: "call-1".to_string(),
            content: "original".to_string(),
            is_error: None,
        };
        let mut messages = vec![stored(
            "message-1",
            vec![
                ContentBlock::Reasoning {
                    text: "r".to_string(),
                },
                result,
            ],
        )];
        let target = build_content_target(&messages, 0, 1).expect("target");
        messages[0].content.swap(0, 1);
        assert_eq!(
            resolve_content_target(&messages, &target).expect("semantic resolution"),
            ResolvedContentTarget {
                message_index: 0,
                block_index: 0,
            }
        );

        let ContentBlock::ToolResult { content, .. } = &mut messages[0].content[0] else {
            panic!("expected tool result");
        };
        *content = "changed".to_string();
        assert!(matches!(
            resolve_content_target(&messages, &target),
            Err(TargetResolutionError::HashMismatch { .. })
        ));
    }

    #[test]
    fn indexed_resolution_follows_stable_ids_after_movement_and_rejects_duplicates() {
        let original = stored(
            "stable-id",
            vec![ContentBlock::Reasoning {
                text: "reasoning".to_string(),
            }],
        );
        let target = build_content_target(std::slice::from_ref(&original), 0, 0).expect("target");
        let range = build_message_range(std::slice::from_ref(&original), 0, 0).expect("range");
        let mut moved = vec![
            stored_with_role(
                "inserted",
                Role::User,
                vec![ContentBlock::Text {
                    text: "inserted".to_string(),
                    cache_control: None,
                }],
            ),
            original.clone(),
        ];
        let index = ContextTargetIndex::new(&moved);
        assert_eq!(
            index.resolve_content_target(&target).expect("moved target"),
            ResolvedContentTarget {
                message_index: 1,
                block_index: 0,
            }
        );
        assert_eq!(index.resolve_message_range(&range), Ok((1, 1)));

        moved.push(original);
        let index = ContextTargetIndex::new(&moved);
        assert!(matches!(
            index.resolve_content_target(&target),
            Err(TargetResolutionError::DuplicateMessageId { .. })
        ));
        assert!(matches!(
            index.resolve_message_range(&range),
            Err(MessageRangeResolutionError::DuplicateMessageId { .. })
        ));
    }

    #[test]
    fn range_digest_ignores_trace_only_and_volatile_metadata_but_detects_prompt_changes() {
        let mut messages = vec![stored(
            "message-1",
            vec![ContentBlock::Text {
                text: "prompt".to_string(),
                cache_control: None,
            }],
        )];
        let range = build_message_range(&messages, 0, 0).expect("range");
        messages[0].content.push(ContentBlock::ReasoningTrace {
            text: "history only".to_string(),
        });
        messages[0].tool_duration_ms = Some(9_999);
        assert_eq!(resolve_message_range(&messages, &range), Ok((0, 0)));

        let ContentBlock::Text { text, .. } = &mut messages[0].content[0] else {
            panic!("expected text");
        };
        *text = "changed prompt".to_string();
        assert!(matches!(
            resolve_message_range(&messages, &range),
            Err(MessageRangeResolutionError::SourceDigestMismatch { .. })
        ));
    }

    #[test]
    fn trace_only_suppression_has_no_targets_turns_or_provider_savings() {
        let messages = vec![stored(
            "trace-only",
            vec![ContentBlock::ReasoningTrace {
                text: "history only".repeat(1_000),
            }],
        )];

        let suppression =
            resolve_reasoning_suppression_keep_latest(&messages, 0).expect("suppression");

        assert!(suppression.targets.is_empty());
        assert_eq!(suppression.assistant_turns_affected, 0);
        assert!(suppression.replay_block_kinds.is_empty());
        assert_eq!(suppression.original_token_estimate, 0);
    }

    #[test]
    fn keep_latest_protects_exact_assistant_turns_and_targets_all_replay_kinds() {
        let messages = vec![
            stored(
                "a0",
                vec![
                    ContentBlock::Reasoning {
                        text: "generic".to_string(),
                    },
                    ContentBlock::ReasoningTrace {
                        text: "trace".to_string(),
                    },
                ],
            ),
            stored_with_role(
                "u0",
                Role::User,
                vec![ContentBlock::Text {
                    text: "user".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "a1",
                vec![ContentBlock::AnthropicThinking {
                    thinking: "thinking".to_string(),
                    signature: "signature".to_string(),
                }],
            ),
            stored(
                "a2",
                vec![ContentBlock::OpenAIReasoning {
                    id: "reasoning-2".to_string(),
                    summary: vec!["summary".to_string()],
                    encrypted_content: Some("encrypted".to_string()),
                    status: Some("completed".to_string()),
                }],
            ),
            stored(
                "a3",
                vec![ContentBlock::Reasoning {
                    text: "protected".to_string(),
                }],
            ),
        ];

        let suppression =
            resolve_reasoning_suppression_keep_latest(&messages, 2).expect("suppression");

        assert_eq!(suppression.assistant_turns_affected, 2);
        assert_eq!(suppression.targets.len(), 2);
        assert_eq!(suppression.targets[0].message_id, "a0");
        assert_eq!(
            suppression.targets[0].kind,
            StoredContextBlockKind::Reasoning
        );
        assert_eq!(suppression.targets[1].message_id, "a1");
        assert_eq!(
            suppression.targets[1].kind,
            StoredContextBlockKind::AnthropicThinking
        );
        assert_eq!(
            suppression.replay_block_kinds,
            vec![
                StoredContextBlockKind::Reasoning,
                StoredContextBlockKind::AnthropicThinking,
            ]
        );
        assert!(suppression.original_token_estimate > 0);
    }

    #[test]
    fn manual_ranges_target_only_assistant_replay_blocks_inside_selected_ranges() {
        let messages = vec![
            stored(
                "a0",
                vec![ContentBlock::Reasoning {
                    text: "outside".to_string(),
                }],
            ),
            stored_with_role(
                "u1",
                Role::User,
                vec![ContentBlock::Text {
                    text: "range start".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "a2",
                vec![ContentBlock::OpenAIReasoning {
                    id: "inside-openai".to_string(),
                    summary: Vec::new(),
                    encrypted_content: Some("encrypted".to_string()),
                    status: None,
                }],
            ),
            stored(
                "a3",
                vec![ContentBlock::AnthropicThinking {
                    thinking: "inside anthropic".to_string(),
                    signature: "signature".to_string(),
                }],
            ),
            stored(
                "a4",
                vec![ContentBlock::Reasoning {
                    text: "outside tail".to_string(),
                }],
            ),
        ];
        let ranges = vec![
            build_message_range(&messages, 1, 2).expect("first range"),
            build_message_range(&messages, 3, 3).expect("second range"),
        ];

        let suppression =
            resolve_reasoning_suppression_for_ranges(&messages, &ranges).expect("suppression");

        assert_eq!(suppression.assistant_turns_affected, 2);
        assert_eq!(
            suppression
                .targets
                .iter()
                .map(|target| target.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2", "a3"]
        );
        assert_eq!(
            suppression.selection,
            StoredReasoningSelection::MessageRanges { ranges }
        );
    }

    #[test]
    fn public_range_digest_rejects_invalid_bounds_without_panicking() {
        let messages = vec![stored("m0", Vec::new())];

        assert!(matches!(
            message_range_digest(&[], 0, 0),
            Err(MessageRangeResolutionError::EmptyTranscript)
        ));
        assert!(matches!(
            message_range_digest(&messages, 1, 0),
            Err(MessageRangeResolutionError::Reversed { .. })
        ));
        assert!(matches!(
            message_range_digest(&messages, 1, 1),
            Err(MessageRangeResolutionError::StartIndexOutOfBounds { .. })
        ));
        assert!(matches!(
            message_range_digest(&messages, 0, 1),
            Err(MessageRangeResolutionError::EndIndexOutOfBounds { .. })
        ));
    }

    #[test]
    fn authoritative_digest_tracks_history_only_and_metadata_changes() {
        let mut messages = vec![stored(
            "m1",
            vec![ContentBlock::ReasoningTrace {
                text: "trace one".to_string(),
            }],
        )];
        let original = authoritative_transcript_digest(&messages);

        messages[0].content = vec![ContentBlock::ReasoningTrace {
            text: "trace two".to_string(),
        }];
        assert_ne!(authoritative_transcript_digest(&messages), original);

        let empty = authoritative_transcript_digest(&[]);
        assert_eq!(empty, authoritative_transcript_digest(&[]));
    }
}
