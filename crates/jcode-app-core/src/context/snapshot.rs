use crate::context::provider_validation::validate_projected_messages;
use crate::message::ContentBlock;
use crate::protocol::{
    CONTEXT_MESSAGE_DETAIL_MAX_CHARS, CONTEXT_SNAPSHOT_MAX_PAGE_SIZE, ContextEditorBlock,
    ContextEditorMessage, ContextEditorSnapshot, ContextMessageDetail, ContextMessageDetailFormat,
    ContextOperationBadge, ContextOperationBadgeKind, ContextSummaryCoverage, ContextTextChunk,
};
use crate::provider::{
    ContextProjectionOperationKind, ContextProjectionValidationOperation,
    ContextProjectionValidationStatus, ContextReasoningBlockKind, Provider,
};
use jcode_context_core::{
    ContextTargetIndex, ProjectedMessageSource, authoritative_transcript_digest,
    content_block_semantic_id, context_block_kind, estimate_content_block_tokens,
    estimate_message_tokens, project_context,
};
use jcode_session_types::{
    StoredContextBlockKind, StoredContextOperation, StoredContextViewState, StoredMessage,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

const MESSAGE_PREVIEW_MAX_CHARS: usize = 320;

pub struct ContextSnapshotInput<'a> {
    pub session_id: &'a str,
    pub messages: &'a [StoredMessage],
    pub context_view: &'a StoredContextViewState,
    pub processing: bool,
    pub provider: &'a dyn Provider,
    pub route: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSnapshotError(pub String);

impl fmt::Display for ContextSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ContextSnapshotError {}

pub fn build_context_editor_snapshot(
    input: ContextSnapshotInput<'_>,
) -> Result<ContextEditorSnapshot, ContextSnapshotError> {
    let projection = project_context(input.messages, input.context_view)
        .map_err(|error| ContextSnapshotError(error.to_string()))?;
    let target_index = ContextTargetIndex::new(input.messages);
    let mut message_badges = vec![Vec::<ContextOperationBadge>::new(); input.messages.len()];
    let mut block_badges = input
        .messages
        .iter()
        .map(|message| vec![Vec::<ContextOperationBadge>::new(); message.content.len()])
        .collect::<Vec<_>>();
    let mut summary_coverage = vec![None; input.messages.len()];

    for transaction in input.context_view.active_transactions() {
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            let (kind, message_indices, block_target, summary_bounds) = match operation {
                StoredContextOperation::RangeSummary(summary) => {
                    let (start, end) = target_index
                        .resolve_message_range(&summary.source_range)
                        .map_err(|error| ContextSnapshotError(error.to_string()))?;
                    (
                        ContextOperationBadgeKind::RangeSummary,
                        (start..=end).collect::<Vec<_>>(),
                        None,
                        Some((start, end)),
                    )
                }
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    let mut indices = BTreeSet::new();
                    let mut targets = Vec::new();
                    for target in &suppression.targets {
                        let resolved = target_index
                            .resolve_content_target(target)
                            .map_err(|error| ContextSnapshotError(error.to_string()))?;
                        indices.insert(resolved.message_index);
                        targets.push((resolved.message_index, resolved.block_index));
                    }
                    (
                        ContextOperationBadgeKind::ReasoningSuppression,
                        indices.into_iter().collect(),
                        Some(targets),
                        None,
                    )
                }
                StoredContextOperation::ToolResultDistillation(distillation) => {
                    let resolved = target_index
                        .resolve_content_target(&distillation.target)
                        .map_err(|error| ContextSnapshotError(error.to_string()))?;
                    (
                        ContextOperationBadgeKind::ToolResultDistillation,
                        vec![resolved.message_index],
                        Some(vec![(resolved.message_index, resolved.block_index)]),
                        None,
                    )
                }
            };
            let badge = ContextOperationBadge {
                transaction_id: transaction.id.clone(),
                operation_index,
                kind,
            };
            let coverage = summary_bounds.map(|(start, end)| ContextSummaryCoverage {
                transaction_id: transaction.id.clone(),
                operation_index,
                start_message_id: input.messages[start].id.clone(),
                end_message_id: input.messages[end].id.clone(),
                start_stored_index: start,
                end_stored_index: end,
                message_count: end.saturating_sub(start).saturating_add(1),
            });
            for message_index in message_indices {
                message_badges[message_index].push(badge.clone());
                if let Some(coverage) = coverage.as_ref() {
                    summary_coverage[message_index] = Some(coverage.clone());
                }
            }
            if let Some(targets) = block_target {
                for (message_index, block_index) in targets {
                    block_badges[message_index][block_index].push(badge.clone());
                }
            }
        }
    }

    let removable =
        provider_removable_reasoning_kinds(input.provider, &projection.messages, input.messages);
    let mut projected_tokens_by_raw_index = BTreeMap::new();
    for (message, source) in projection.messages.iter().zip(&projection.sources) {
        if let ProjectedMessageSource::RawMessage { stored_index, .. } = source {
            projected_tokens_by_raw_index.insert(*stored_index, estimate_message_tokens(message));
        }
    }

    let messages: Vec<ContextEditorMessage> = input
        .messages
        .iter()
        .enumerate()
        .map(|(stored_index, message)| {
            let mut tool_group_ids = BTreeSet::new();
            let blocks = message
                .content
                .iter()
                .enumerate()
                .map(|(ordinal, block)| {
                    let kind = context_block_kind(block);
                    let (tool_name, tool_use_id, tool_result_is_error, has_thought_signature) =
                        match block {
                            ContentBlock::ToolUse {
                                id,
                                name,
                                thought_signature,
                                ..
                            } => {
                                tool_group_ids.insert(id.clone());
                                (
                                    Some(name.clone()),
                                    Some(id.clone()),
                                    false,
                                    thought_signature.is_some(),
                                )
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                is_error,
                                ..
                            } => {
                                tool_group_ids.insert(tool_use_id.clone());
                                (
                                    None,
                                    Some(tool_use_id.clone()),
                                    is_error.unwrap_or(false),
                                    false,
                                )
                            }
                            _ => (None, None, false, false),
                        };
                    ContextEditorBlock {
                        ordinal,
                        kind,
                        semantic_id: content_block_semantic_id(block).map(ToOwned::to_owned),
                        estimated_provider_tokens: estimate_content_block_tokens(block),
                        tool_name,
                        tool_use_id,
                        tool_result_is_error,
                        has_image_payload: matches!(block, ContentBlock::Image { .. }),
                        has_tool_thought_signature: has_thought_signature,
                        provider_removable_reasoning: removable.contains(&kind),
                        active_operations: block_badges[stored_index][ordinal].clone(),
                    }
                })
                .collect::<Vec<_>>();
            let removable_reasoning_kinds = blocks
                .iter()
                .filter(|block| block.provider_removable_reasoning)
                .map(|block| block.kind)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            ContextEditorMessage {
                message_id: message.id.clone(),
                stored_index,
                role: message.role.clone(),
                display_role: message.display_role,
                timestamp: message.timestamp,
                raw_provider_tokens: estimate_message_tokens(&message.to_message()),
                projected_provider_tokens: projected_tokens_by_raw_index
                    .get(&stored_index)
                    .copied()
                    .unwrap_or_default(),
                preview: bounded_message_preview(message),
                blocks,
                tool_group_ids: tool_group_ids.into_iter().collect(),
                summary_coverage: summary_coverage[stored_index].clone(),
                active_operations: message_badges[stored_index].clone(),
                removable_reasoning_kinds,
                active_agent_profile: false,
            }
        })
        .collect();

    Ok(ContextEditorSnapshot {
        session_id: input.session_id.to_string(),
        context_revision: input.context_view.revision,
        raw_message_count: input.messages.len(),
        transcript_digest: authoritative_transcript_digest(input.messages),
        processing: input.processing,
        provider_name: input.provider.name().to_string(),
        provider_display_name: input.provider.display_name(),
        model: input.provider.model(),
        route: input.route.to_string(),
        context_window: input.provider.context_window(),
        projected_request_tokens: projection.diagnostics.projected_provider_token_estimate,
        message_page_start: 0,
        message_page_end: messages.len(),
        next_message_page_start: None,
        messages,
        active_transactions: input
            .context_view
            .active_transactions()
            .map(crate::context::summarize_context_transaction)
            .collect(),
        emergency_policy: input.context_view.emergency_policy.clone(),
        curator_route: None,
        curator_unavailable_reason: None,
        curator_default: Default::default(),
        curator_route_options: Vec::new(),
    })
}

pub fn paginate_context_editor_snapshot(
    mut snapshot: ContextEditorSnapshot,
    page_start: usize,
    page_size: usize,
) -> Result<ContextEditorSnapshot, ContextSnapshotError> {
    if page_size == 0 || page_size > CONTEXT_SNAPSHOT_MAX_PAGE_SIZE {
        return Err(ContextSnapshotError(format!(
            "context snapshot page size must be between 1 and {CONTEXT_SNAPSHOT_MAX_PAGE_SIZE}"
        )));
    }
    if page_start > snapshot.messages.len() {
        return Err(ContextSnapshotError(format!(
            "context snapshot page start {page_start} exceeds {} messages",
            snapshot.messages.len()
        )));
    }
    let page_end = page_start
        .saturating_add(page_size)
        .min(snapshot.messages.len());
    let messages = snapshot.messages.drain(page_start..page_end).collect();
    snapshot.messages = messages;
    snapshot.message_page_start = page_start;
    snapshot.message_page_end = page_end;
    snapshot.next_message_page_start = (page_end < snapshot.raw_message_count).then_some(page_end);
    Ok(snapshot)
}

pub struct ContextMessageDetailInput<'a> {
    pub session_id: &'a str,
    pub messages: &'a [StoredMessage],
    pub context_view: &'a StoredContextViewState,
    pub expected_context_revision: u64,
    pub expected_transcript_digest: u64,
    pub message_id: &'a str,
    pub block_ordinal: usize,
    pub start_char: usize,
    pub max_chars: usize,
}

pub fn build_context_message_detail(
    input: ContextMessageDetailInput<'_>,
) -> Result<ContextMessageDetail, ContextSnapshotError> {
    if input.max_chars == 0 || input.max_chars > CONTEXT_MESSAGE_DETAIL_MAX_CHARS {
        return Err(ContextSnapshotError(format!(
            "context detail chunk size must be between 1 and {CONTEXT_MESSAGE_DETAIL_MAX_CHARS}"
        )));
    }
    if input.context_view.revision != input.expected_context_revision {
        return Err(ContextSnapshotError(format!(
            "context revision changed from {} to {}",
            input.expected_context_revision, input.context_view.revision
        )));
    }
    let transcript_digest = authoritative_transcript_digest(input.messages);
    if transcript_digest != input.expected_transcript_digest {
        return Err(ContextSnapshotError(format!(
            "authoritative transcript digest changed from {} to {transcript_digest}",
            input.expected_transcript_digest
        )));
    }
    let mut matches = input
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.id == input.message_id);
    let Some((stored_index, message)) = matches.next() else {
        return Err(ContextSnapshotError(format!(
            "stored message not found: {}",
            input.message_id
        )));
    };
    if matches.next().is_some() {
        return Err(ContextSnapshotError(format!(
            "stored message ID is ambiguous: {}",
            input.message_id
        )));
    }
    let block = message.content.get(input.block_ordinal).ok_or_else(|| {
        ContextSnapshotError(format!(
            "stored message {} has no block at ordinal {}",
            input.message_id, input.block_ordinal
        ))
    })?;
    let block_kind = context_block_kind(block);
    let semantic_id = content_block_semantic_id(block).map(str::to_string);
    let mut format = ContextMessageDetailFormat::Text;
    let mut text = String::new();
    let mut tool_name = None;
    let mut tool_use_id = None;
    let mut tool_result_is_error = None;
    let mut provider_status = None;
    let mut image_media_type = None;
    let mut image_encoded_bytes = None;
    let mut opaque_signature_present = false;
    let mut encrypted_state_present = false;

    match block {
        ContentBlock::Text { text: value, .. }
        | ContentBlock::Reasoning { text: value }
        | ContentBlock::ReasoningTrace { text: value } => text = value.clone(),
        ContentBlock::AnthropicThinking {
            thinking,
            signature,
        } => {
            text = thinking.clone();
            opaque_signature_present = !signature.is_empty();
        }
        ContentBlock::OpenAIReasoning {
            summary,
            encrypted_content,
            status,
            ..
        } => {
            text = summary.join("\n");
            provider_status = status.clone();
            encrypted_state_present = encrypted_content
                .as_deref()
                .is_some_and(|content| !content.is_empty());
        }
        ContentBlock::ToolUse {
            id,
            name,
            input,
            thought_signature,
        } => {
            format = ContextMessageDetailFormat::Json;
            text = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
            tool_name = Some(name.clone());
            tool_use_id = Some(id.clone());
            opaque_signature_present = thought_signature
                .as_deref()
                .is_some_and(|signature| !signature.is_empty());
        }
        ContentBlock::ToolResult {
            tool_use_id: id,
            content,
            is_error,
        } => {
            text = content.clone();
            tool_use_id = Some(id.clone());
            tool_result_is_error = Some(is_error.unwrap_or(false));
        }
        ContentBlock::Image { media_type, data } => {
            format = ContextMessageDetailFormat::MetadataOnly;
            image_media_type = Some(media_type.clone());
            image_encoded_bytes = Some(data.len());
        }
        ContentBlock::OpenAICompaction { encrypted_content } => {
            format = ContextMessageDetailFormat::MetadataOnly;
            encrypted_state_present = !encrypted_content.is_empty();
        }
    }

    let content = context_text_chunk(&text, input.start_char, input.max_chars)?;
    Ok(ContextMessageDetail {
        session_id: input.session_id.to_string(),
        context_revision: input.context_view.revision,
        transcript_digest,
        message_id: message.id.clone(),
        stored_index,
        role: message.role.clone(),
        display_role: message.display_role,
        timestamp: message.timestamp,
        block_ordinal: input.block_ordinal,
        block_kind,
        format,
        content,
        semantic_id,
        tool_name,
        tool_use_id,
        tool_result_is_error,
        provider_status,
        image_media_type,
        image_encoded_bytes,
        opaque_signature_present,
        encrypted_state_present,
    })
}

fn context_text_chunk(
    text: &str,
    start_char: usize,
    max_chars: usize,
) -> Result<ContextTextChunk, ContextSnapshotError> {
    let total_chars = text.chars().count();
    if start_char > total_chars {
        return Err(ContextSnapshotError(format!(
            "context detail start {start_char} exceeds {total_chars} characters"
        )));
    }
    let end_char = start_char.saturating_add(max_chars).min(total_chars);
    let chunk = text
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect();
    Ok(ContextTextChunk {
        start_char,
        end_char,
        total_chars,
        text: chunk,
        next_start_char: (end_char < total_chars).then_some(end_char),
    })
}

fn provider_removable_reasoning_kinds(
    provider: &dyn Provider,
    projected_messages: &[crate::message::Message],
    raw_messages: &[StoredMessage],
) -> BTreeSet<StoredContextBlockKind> {
    let kinds = raw_messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Reasoning { .. } => Some((
                StoredContextBlockKind::Reasoning,
                ContextReasoningBlockKind::GenericReasoning,
            )),
            ContentBlock::AnthropicThinking { .. } => Some((
                StoredContextBlockKind::AnthropicThinking,
                ContextReasoningBlockKind::AnthropicThinking,
            )),
            ContentBlock::OpenAIReasoning { .. } => Some((
                StoredContextBlockKind::OpenAiReasoning,
                ContextReasoningBlockKind::OpenAiReasoning,
            )),
            ContentBlock::ReasoningTrace { .. } => Some((
                StoredContextBlockKind::ReasoningTrace,
                ContextReasoningBlockKind::ReasoningTrace,
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let operations = kinds
        .iter()
        .enumerate()
        .map(|(index, (_, kind))| ContextProjectionValidationOperation {
            id: format!("snapshot-reasoning-{index}"),
            kind: ContextProjectionOperationKind::ReasoningSuppression { block_kind: *kind },
        })
        .collect::<Vec<_>>();
    if operations.is_empty() {
        return BTreeSet::new();
    }
    let report = validate_projected_messages(provider, projected_messages, &operations);
    if report.builder_status != ContextProjectionValidationStatus::Supported {
        return BTreeSet::new();
    }
    report
        .findings
        .iter()
        .filter(|finding| {
            finding.status == ContextProjectionValidationStatus::Supported
                && finding.operation_id.is_some()
        })
        .filter_map(|finding| {
            let id = finding.operation_id.as_deref()?;
            let index = id
                .strip_prefix("snapshot-reasoning-")?
                .parse::<usize>()
                .ok()?;
            kinds.iter().nth(index).map(|(stored, _)| *stored)
        })
        .collect()
}

fn bounded_message_preview(message: &StoredMessage) -> String {
    let preview = message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => non_empty(text).map(ToOwned::to_owned),
            ContentBlock::ToolUse { name, .. } => Some(format!("[tool: {name}]")),
            ContentBlock::ToolResult { content, .. } => {
                non_empty(content).map(|content| format!("[result: {content}]"))
            }
            ContentBlock::Reasoning { text } => {
                non_empty(text).map(|text| format!("[reasoning: {text}]"))
            }
            ContentBlock::ReasoningTrace { text } => {
                non_empty(text).map(|text| format!("[reasoning trace: {text}]"))
            }
            ContentBlock::AnthropicThinking { thinking, .. } => {
                non_empty(thinking).map(|text| format!("[thinking: {text}]"))
            }
            ContentBlock::OpenAIReasoning { summary, .. } => {
                let summary = summary.join(" ");
                non_empty(&summary).map(|text| format!("[reasoning: {text}]"))
            }
            ContentBlock::Image { media_type, data } => Some(format!(
                "[image: {media_type}, {} encoded bytes]",
                data.len()
            )),
            ContentBlock::OpenAICompaction { .. } => {
                Some("[legacy OpenAI compaction state]".to_string())
            }
        })
        .unwrap_or_else(|| "(empty)".to_string())
        .replace('\n', " ");
    truncate_chars(&preview, MESSAGE_PREVIEW_MAX_CHARS)
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role, StreamEvent, ToolDefinition};
    use crate::provider::EventStream;
    use crate::provider::{
        ContextProviderFamily, ContextProviderValidationIdentity, ContextRequestBuilderValidation,
        context_projection_validation_report,
    };
    use crate::session::Session;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Utc;
    use jcode_session_types::{
        StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextStatusEvent,
        StoredContextTransaction, StoredContextTransactionStatusKind, StoredRangeSummary,
        StoredReasoningSelection, StoredReasoningSuppression, StoredToolResultDistillation,
    };
    use std::sync::Arc;

    struct SnapshotProvider;

    #[derive(Clone)]
    struct ReasoningSnapshotProvider {
        family: ContextProviderFamily,
        supported_reasoning: Option<ContextReasoningBlockKind>,
        builder_supported: bool,
    }

    #[async_trait]
    impl Provider for SnapshotProvider {
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
            "snapshot"
        }

        fn display_name(&self) -> String {
            "Snapshot Provider".to_string()
        }

        fn model(&self) -> String {
            "snapshot-model".to_string()
        }

        fn context_window(&self) -> usize {
            100_000
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> crate::provider::ContextProjectionValidationReport {
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: ContextProviderFamily::OpenRouterCompatible,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "snapshot-test-v1".to_string(),
                },
                operations,
                Some(ContextReasoningBlockKind::GenericReasoning),
                Ok(ContextRequestBuilderValidation::new(messages.len())),
            )
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
    }

    #[async_trait]
    impl Provider for ReasoningSnapshotProvider {
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
            "reasoning-snapshot"
        }

        fn model(&self) -> String {
            "reasoning-snapshot-model".to_string()
        }

        fn context_window(&self) -> usize {
            100_000
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> crate::provider::ContextProjectionValidationReport {
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: self.family,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "reasoning-snapshot-test-v1".to_string(),
                },
                operations,
                self.supported_reasoning,
                if self.builder_supported {
                    Ok(ContextRequestBuilderValidation::new(messages.len()))
                } else {
                    Err("production request builder rejected the projection".to_string())
                },
            )
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    fn stored(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            origin: None,
            id: id.to_string(),
            role,
            content,
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn transaction(
        id: &str,
        revision: u64,
        operations: Vec<StoredContextOperation>,
    ) -> StoredContextTransaction {
        StoredContextTransaction {
            id: id.to_string(),
            base_revision: revision.saturating_sub(1),
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations,
            status_events: vec![StoredContextStatusEvent {
                revision,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        }
    }

    fn phase17_measurement_messages() -> Vec<StoredMessage> {
        (0usize..10_000)
            .map(|index| {
                let content = if index % 20 == 0 {
                    vec![ContentBlock::ToolUse {
                        id: format!("phase17-tool-{}", index / 20),
                        name: "read".to_string(),
                        input: serde_json::json!({
                            "file_path": format!("src/phase17/file-{}.rs", index / 20)
                        }),
                        thought_signature: None,
                    }]
                } else if index % 20 == 1 {
                    let pair_index = index / 20;
                    let repetitions = if pair_index % 50 == 0 { 4_096 } else { 128 };
                    vec![ContentBlock::ToolResult {
                        tool_use_id: format!("phase17-tool-{pair_index}"),
                        content: format!("phase17 exact tool result {pair_index} ")
                            .repeat(repetitions),
                        is_error: Some(false),
                    }]
                } else if index % 1_000 == 510 {
                    vec![
                        ContentBlock::Image {
                            media_type: "image/png".to_string(),
                            data: "phase17-image-payload".repeat(2_048),
                        },
                        ContentBlock::Text {
                            text: format!("phase17 image label {index}"),
                            cache_control: None,
                        },
                    ]
                } else if index % 25 == 2 {
                    vec![
                        ContentBlock::Reasoning {
                            text: format!("phase17 replayed reasoning {index} ").repeat(12),
                        },
                        ContentBlock::Text {
                            text: format!("phase17 assistant conclusion {index}"),
                            cache_control: None,
                        },
                    ]
                } else {
                    vec![ContentBlock::Text {
                        text: format!(
                            "phase17 representative message {index} with stable searchable content"
                        ),
                        cache_control: None,
                    }]
                };
                stored(
                    &format!("phase17-message-{index}"),
                    if index % 20 == 0 || index % 25 == 2 {
                        Role::Assistant
                    } else if index % 20 == 1 || index.is_multiple_of(2) {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    content,
                )
            })
            .collect()
    }

    fn phase17_summary_operation(
        messages: &[StoredMessage],
        start: usize,
        end: usize,
        label: &str,
    ) -> StoredContextOperation {
        let source_range = jcode_context_core::build_message_range(messages, start, end)
            .expect("representative summary range");
        let source_token_estimate = messages[start..=end]
            .iter()
            .map(|message| estimate_message_tokens(&message.to_message()))
            .sum::<usize>();
        StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range,
            summary_text: format!(
                "Representative lossless summary for {label}; exact decisions, paths, results, and unresolved work are preserved."
            ),
            file_change_digest: String::new(),
            changed_files: Vec::new(),
            change_evidence_complete: false,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: None,
            source_token_estimate,
            replacement_token_estimate: 32,
            warnings: Vec::new(),
            created_at: Utc::now(),
            legacy_coverage: None,
        })
    }

    fn phase17_measurement_state(messages: &[StoredMessage]) -> StoredContextViewState {
        let mut transactions = [
            (100, 499),
            (2_100, 2_499),
            (4_100, 4_499),
            (6_100, 6_499),
            (8_100, 8_499),
        ]
        .into_iter()
        .enumerate()
        .map(|(offset, (start, end))| {
            transaction(
                &format!("phase17-summary-{}", offset + 1),
                (offset + 1) as u64,
                vec![phase17_summary_operation(
                    messages,
                    start,
                    end,
                    &format!("messages {start} through {end}"),
                )],
            )
        })
        .collect::<Vec<_>>();

        let reasoning_ranges = [1_002, 3_002, 5_002, 7_002, 9_002]
            .into_iter()
            .map(|index| {
                jcode_context_core::build_message_range(messages, index, index)
                    .expect("representative reasoning range")
            })
            .collect::<Vec<_>>();
        let suppression = jcode_context_core::resolve_reasoning_suppression_for_ranges(
            messages,
            &reasoning_ranges,
        )
        .expect("representative reasoning suppression");
        transactions.push(transaction(
            "phase17-reasoning",
            6,
            vec![StoredContextOperation::ReasoningSuppression(suppression)],
        ));

        let target = jcode_context_core::build_content_target(messages, 9_001, 0)
            .expect("representative tool-result target");
        let original_token_estimate = estimate_content_block_tokens(&messages[9_001].content[0]);
        let replacement_content =
            "Representative distilled result retaining the exact finding and source path.";
        let replacement_token_estimate = estimate_content_block_tokens(&ContentBlock::ToolResult {
            tool_use_id: "phase17-tool-450".to_string(),
            content: replacement_content.to_string(),
            is_error: Some(false),
        });
        let replacement_ratio_millionths = u32::try_from(
            (replacement_token_estimate as u128).saturating_mul(1_000_000)
                / original_token_estimate as u128,
        )
        .unwrap_or(u32::MAX);
        transactions.push(transaction(
            "phase17-distillation",
            7,
            vec![StoredContextOperation::ToolResultDistillation(
                StoredToolResultDistillation {
                    target,
                    tool_name: "read".to_string(),
                    tool_call_id: "phase17-tool-450".to_string(),
                    replacement_content: replacement_content.to_string(),
                    original_token_estimate,
                    replacement_token_estimate,
                    replacement_ratio_millionths,
                    preservation_rationale:
                        "The replacement retains the exact result required by later work."
                            .to_string(),
                    uncertainties: Vec::new(),
                    generator: StoredContextArtifactGenerator {
                        provider: "phase17-measurement".to_string(),
                        model: "phase17-measurement".to_string(),
                        route: "synthetic".to_string(),
                        prompt_version: "phase17-measurement-v1".to_string(),
                        effort: None,
                        role: None,
                        selection_source: None,
                        transaction_instructions: None,
                        task_instructions: None,
                    },
                    created_at: Utc::now(),
                },
            )],
        ));

        StoredContextViewState {
            revision: 7,
            transactions,
            ..StoredContextViewState::default()
        }
    }

    fn phase17_revised_measurement_state(
        messages: &[StoredMessage],
        state: &StoredContextViewState,
    ) -> StoredContextViewState {
        let mut revised = state.clone();
        revised.revision = 8;
        revised.transactions.push(transaction(
            "phase17-rebuild-summary",
            8,
            vec![phase17_summary_operation(
                messages,
                9_200,
                9_399,
                "messages 9200 through 9399",
            )],
        ));
        revised
    }

    fn phase17_measurement_session(
        messages: &[StoredMessage],
        state: &StoredContextViewState,
    ) -> Session {
        let mut session = Session::create_with_id(
            "phase17-representative-performance".to_string(),
            None,
            Some("Phase 17 representative performance".to_string()),
        );
        session.replace_messages(messages.to_vec());
        session.context_view = state.clone();
        session
    }

    fn phase17_duration_stats_with_setup<T>(
        mut setup: impl FnMut() -> T,
        mut operation: impl FnMut(&mut T),
    ) -> serde_json::Value {
        const WARMUPS: usize = 2;
        const SAMPLES: usize = 7;
        for _ in 0..WARMUPS {
            let mut state = setup();
            operation(&mut state);
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let mut state = setup();
            let started = std::time::Instant::now();
            operation(&mut state);
            samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
        samples.sort_unstable();
        serde_json::json!({
            "warmups": WARMUPS,
            "samples": SAMPLES,
            "minimum_ns": samples[0],
            "median_ns": samples[SAMPLES / 2],
            "maximum_ns": samples[SAMPLES - 1],
        })
    }

    #[test]
    #[ignore = "manual Phase 17 representative large-session measurement; run explicitly with --ignored --nocapture"]
    fn phase17_representative_large_session_projection_snapshot_cache_and_memory() {
        let messages = phase17_measurement_messages();
        let state = phase17_measurement_state(&messages);
        let revised_state = phase17_revised_measurement_state(&messages, &state);
        jcode_context_core::validate_context_state(&state)
            .expect("representative state must be strictly valid");
        jcode_context_core::validate_context_state(&revised_state)
            .expect("representative revised state must be strictly valid");

        let projection = phase17_duration_stats_with_setup(
            || (),
            |_| {
                let projection = jcode_context_core::project_context(&messages, &state)
                    .expect("representative projection");
                std::hint::black_box(projection.messages.len());
            },
        );
        let first_session_cache = phase17_duration_stats_with_setup(
            || phase17_measurement_session(&messages, &state),
            |session| {
                let count = session
                    .projected_provider_messages()
                    .expect("initial projected cache")
                    .len();
                std::hint::black_box(count);
            },
        );

        let mut reused_session = phase17_measurement_session(&messages, &state);
        reused_session
            .projected_provider_messages()
            .expect("populate reusable projected cache");
        let unchanged_cache_reuse = phase17_duration_stats_with_setup(
            || (),
            |_| {
                let count = reused_session
                    .projected_provider_messages()
                    .expect("reuse projected cache")
                    .len();
                std::hint::black_box(count);
            },
        );

        let append_fast_path = phase17_duration_stats_with_setup(
            || {
                let mut session = phase17_measurement_session(&messages, &state);
                session
                    .projected_provider_messages()
                    .expect("populate projected cache before append");
                session.append_stored_message(stored(
                    "phase17-appended-message",
                    Role::User,
                    vec![ContentBlock::Text {
                        text: "phase17 appended fast-path message".to_string(),
                        cache_control: None,
                    }],
                ));
                session
            },
            |session| {
                let count = session
                    .projected_provider_messages()
                    .expect("append projected cache")
                    .len();
                std::hint::black_box(count);
            },
        );

        let revision_rebuild = phase17_duration_stats_with_setup(
            || {
                let mut session = phase17_measurement_session(&messages, &state);
                session
                    .projected_provider_messages()
                    .expect("populate projected cache before revision");
                session.context_view = revised_state.clone();
                session
            },
            |session| {
                let count = session
                    .projected_provider_messages()
                    .expect("context-revision projected cache rebuild")
                    .len();
                std::hint::black_box(count);
            },
        );

        let snapshot_construction = phase17_duration_stats_with_setup(
            || (),
            |_| {
                let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
                    session_id: "phase17-representative-performance",
                    messages: &messages,
                    context_view: &state,
                    processing: false,
                    provider: &SnapshotProvider,
                    route: "synthetic",
                })
                .expect("representative Context Editor snapshot");
                std::hint::black_box(snapshot.messages.len());
            },
        );
        let representative_snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "phase17-representative-performance",
            messages: &messages,
            context_view: &state,
            processing: false,
            provider: &SnapshotProvider,
            route: "synthetic",
        })
        .expect("representative Context Editor snapshot for payload measurements");
        let snapshot_pagination = phase17_duration_stats_with_setup(
            || Some(representative_snapshot.clone()),
            |snapshot| {
                let page = paginate_context_editor_snapshot(
                    snapshot
                        .take()
                        .expect("representative snapshot setup must be present"),
                    4_200,
                    crate::protocol::CONTEXT_SNAPSHOT_MAX_PAGE_SIZE,
                )
                .expect("representative maximum snapshot page");
                std::hint::black_box(page.messages.len());
            },
        );
        let page = paginate_context_editor_snapshot(
            representative_snapshot.clone(),
            4_200,
            crate::protocol::CONTEXT_SNAPSHOT_MAX_PAGE_SIZE,
        )
        .expect("representative maximum snapshot page");
        assert_eq!(page.messages.len(), 1_000);
        assert!(
            page.messages
                .iter()
                .any(|message| message.summary_coverage.is_some())
        );
        assert!(
            page.messages.iter().all(|message| {
                message.preview.chars().count() <= MESSAGE_PREVIEW_MAX_CHARS + 1
            })
        );
        assert!(
            page.messages
                .iter()
                .flat_map(|message| &message.blocks)
                .any(|block| block.has_image_payload)
        );
        let page_json = serde_json::to_vec(&page).expect("serialize representative page");
        assert!(!String::from_utf8_lossy(&page_json).contains("phase17-image-payload"));

        let mut memory_session = phase17_measurement_session(&messages, &state);
        let memory_before_projection = memory_session.memory_profile_snapshot();
        memory_session
            .projected_provider_messages()
            .expect("populate projected cache for memory profile");
        let memory_after_projection = memory_session.memory_profile_snapshot();
        memory_session.release_provider_messages_cache();
        let memory_after_cache_release = memory_session.memory_profile_snapshot();
        assert_eq!(
            memory_after_cache_release.projected_provider_cache_message_count,
            0
        );
        assert_eq!(
            memory_after_cache_release.projected_provider_cache_json_bytes,
            0
        );

        let report = serde_json::json!({
            "identity": {
                "git_hash": jcode_build_meta::GIT_HASH,
                "version": jcode_build_meta::VERSION,
                "root_package_version": jcode_build_meta::PKG_VERSION,
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
            },
            "fixture": {
                "authoritative_messages": messages.len(),
                "tool_pairs": 500,
                "active_transactions": state.active_transaction_count(),
                "active_range_summaries": 5,
                "active_reasoning_suppressions": 1,
                "active_tool_distillations": 1,
                "image_messages": 10,
                "context_state_json_bytes": serde_json::to_vec(&state).expect("serialize context state").len(),
                "full_snapshot_json_bytes": serde_json::to_vec(&representative_snapshot).expect("serialize full snapshot").len(),
                "paged_snapshot_json_bytes": page_json.len(),
            },
            "timings": {
                "pure_projection": projection,
                "first_session_projected_cache": first_session_cache,
                "unchanged_cache_reuse": unchanged_cache_reuse,
                "one_message_append_fast_path": append_fast_path,
                "context_revision_cache_rebuild": revision_rebuild,
                "full_context_editor_snapshot": snapshot_construction,
                "maximum_page_pagination": snapshot_pagination,
            },
            "memory_profile": {
                "before_projection": memory_before_projection,
                "after_projection": memory_after_projection,
                "after_cache_release": memory_after_cache_release,
            },
        });
        println!(
            "PHASE17_CONTEXT_PERFORMANCE={}",
            serde_json::to_string_pretty(&report).expect("serialize performance report")
        );
    }

    #[test]
    fn snapshot_is_bounded_image_safe_and_provider_aware() {
        let messages = vec![stored(
            "m1",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "replayed".to_string(),
                },
                ContentBlock::ReasoningTrace {
                    text: "history only".to_string(),
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "A".repeat(20_000),
                },
            ],
        )];
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session",
            messages: &messages,
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("snapshot");

        assert_eq!(snapshot.raw_message_count, 1);
        assert!(snapshot.messages[0].preview.len() < 2_000);
        assert!(!snapshot.messages[0].preview.contains(&"A".repeat(100)));
        assert!(
            snapshot.messages[0]
                .removable_reasoning_kinds
                .contains(&StoredContextBlockKind::Reasoning)
        );
        assert!(
            !snapshot.messages[0]
                .removable_reasoning_kinds
                .contains(&StoredContextBlockKind::ReasoningTrace)
        );
        assert!(snapshot.messages[0].blocks[2].has_image_payload);
    }

    #[test]
    fn snapshot_reports_active_block_operation_badges() {
        let messages = vec![
            stored(
                "call",
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "src/lib.rs"}),
                    thought_signature: None,
                }],
            ),
            stored(
                "result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    content: "long output".repeat(100),
                    is_error: Some(false),
                }],
            ),
        ];
        let target = jcode_context_core::build_content_target(&messages, 1, 0).expect("target");
        let original = estimate_content_block_tokens(&messages[1].content[0]);
        let replacement = ContentBlock::ToolResult {
            tool_use_id: "tool-1".to_string(),
            content: "short".to_string(),
            is_error: Some(false),
        };
        let replacement_tokens = estimate_content_block_tokens(&replacement);
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![StoredContextTransaction {
                id: "tx".to_string(),
                base_revision: 0,
                created_at: Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations: vec![StoredContextOperation::ToolResultDistillation(
                    StoredToolResultDistillation {
                        target,
                        tool_name: "read".to_string(),
                        tool_call_id: "tool-1".to_string(),
                        replacement_content: "short".to_string(),
                        original_token_estimate: original,
                        replacement_token_estimate: replacement_tokens,
                        replacement_ratio_millionths: ((replacement_tokens as u128 * 1_000_000)
                            / original as u128)
                            as u32,
                        preservation_rationale: "test".to_string(),
                        uncertainties: Vec::new(),
                        generator: StoredContextArtifactGenerator {
                            provider: "test".to_string(),
                            model: "test".to_string(),
                            route: "test".to_string(),
                            prompt_version: "test".to_string(),
                            effort: None,
                            role: None,
                            selection_source: None,
                            transaction_instructions: None,
                            task_instructions: None,
                        },
                        created_at: Utc::now(),
                    },
                )],
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
        };

        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session",
            messages: &messages,
            context_view: &state,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("snapshot");
        assert_eq!(snapshot.messages[1].blocks[0].active_operations.len(), 1);
        assert_eq!(snapshot.active_transactions.len(), 1);
    }

    #[test]
    fn transcript_digest_changes_without_context_revision_and_revision_changes_without_transcript()
    {
        let messages = vec![stored(
            "m1",
            Role::User,
            vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        )];
        let first = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session",
            messages: &messages,
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("snapshot");
        let mut changed = messages.clone();
        changed[0].content = vec![ContentBlock::Text {
            text: "changed".to_string(),
            cache_control: None,
        }];
        let second = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session",
            messages: &changed,
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("snapshot");
        assert_ne!(first.transcript_digest, second.transcript_digest);
        assert_eq!(first.context_revision, second.context_revision);

        let revised_state = StoredContextViewState {
            revision: 7,
            ..StoredContextViewState::default()
        };
        let revised = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session",
            messages: &messages,
            context_view: &revised_state,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("snapshot");
        assert_eq!(first.transcript_digest, revised.transcript_digest);
        assert_ne!(first.context_revision, revised.context_revision);
    }

    #[test]
    fn empty_unicode_large_output_and_image_snapshots_remain_bounded_and_serializable() {
        let empty = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "empty",
            messages: &[],
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("empty snapshot");
        assert!(empty.messages.is_empty());
        assert_eq!(empty.raw_message_count, 0);

        let image_data = "BASE64_SECRET_IMAGE".repeat(2_000);
        let messages = vec![
            stored(
                "unicode",
                Role::User,
                vec![ContentBlock::Text {
                    text: format!("{} tail", "🧪é漢字".repeat(200)),
                    cache_control: None,
                }],
            ),
            stored(
                "large-result",
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "large-call".to_string(),
                    content: "large output ".repeat(50_000),
                    is_error: Some(false),
                }],
            ),
            stored(
                "image",
                Role::User,
                vec![ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: image_data.clone(),
                }],
            ),
        ];
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "bounded",
            messages: &messages,
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("bounded snapshot");
        assert!(snapshot.messages[0].preview.chars().count() <= MESSAGE_PREVIEW_MAX_CHARS + 1);
        assert!(snapshot.messages[0].preview.ends_with('…'));
        assert!(snapshot.messages[1].preview.chars().count() <= MESSAGE_PREVIEW_MAX_CHARS + 1);
        assert!(snapshot.messages[2].preview.contains("encoded bytes"));
        let encoded = serde_json::to_string(&snapshot).expect("snapshot JSON");
        assert!(!encoded.contains(&image_data));
        assert!(encoded.len() < 20_000);
    }

    #[test]
    fn summarized_rows_keep_stable_identity_coverage_and_zero_projected_raw_tokens() {
        let messages = vec![
            stored(
                "first",
                Role::User,
                vec![ContentBlock::Text {
                    text: "first source".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "second",
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "second source".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "suffix",
                Role::User,
                vec![ContentBlock::Text {
                    text: "suffix remains".to_string(),
                    cache_control: None,
                }],
            ),
        ];
        let source_range =
            jcode_context_core::build_message_range(&messages, 0, 1).expect("summary source range");
        let state = StoredContextViewState {
            revision: 1,
            transactions: vec![transaction(
                "summary-tx",
                1,
                vec![StoredContextOperation::RangeSummary(StoredRangeSummary {
                    source_range,
                    summary_text: "The first two messages were summarized.".to_string(),
                    file_change_digest: String::new(),
                    changed_files: Vec::new(),
                    change_evidence_complete: true,
                    file_evidence: None,
                    boundary_expansions: Vec::new(),
                    generator: None,
                    source_token_estimate: 20,
                    replacement_token_estimate: 10,
                    warnings: Vec::new(),
                    created_at: Utc::now(),
                    legacy_coverage: None,
                })],
            )],
            ..StoredContextViewState::default()
        };
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "summary",
            messages: &messages,
            context_view: &state,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("summary snapshot");
        assert_eq!(
            snapshot
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "suffix"]
        );
        for row in &snapshot.messages[..2] {
            let coverage = row.summary_coverage.as_ref().expect("summary coverage");
            assert_eq!(coverage.start_message_id, "first");
            assert_eq!(coverage.end_message_id, "second");
            assert_eq!(coverage.start_stored_index, 0);
            assert_eq!(coverage.end_stored_index, 1);
            assert_eq!(coverage.message_count, 2);
            assert_eq!(row.projected_provider_tokens, 0);
            assert!(row.raw_provider_tokens > 0);
        }
        assert!(snapshot.messages[2].summary_coverage.is_none());
        assert!(snapshot.messages[2].projected_provider_tokens > 0);

        let page = paginate_context_editor_snapshot(snapshot, 1, 1).expect("interior page");
        assert_eq!(page.message_page_start, 1);
        assert_eq!(page.message_page_end, 2);
        assert_eq!(page.messages.len(), 1);
        let coverage = page.messages[0]
            .summary_coverage
            .as_ref()
            .expect("interior coverage remains authoritative");
        assert_eq!(coverage.start_stored_index, 0);
        assert_eq!(coverage.end_stored_index, 1);
        assert_eq!(coverage.start_message_id, "first");
        assert_eq!(coverage.end_message_id, "second");

        let mut lifecycle = state;
        lifecycle.revision = 2;
        lifecycle.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 2,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reverted,
                reason: None,
            });
        let reverted = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "summary",
            messages: &messages,
            context_view: &lifecycle,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("reverted snapshot");
        assert!(
            reverted
                .messages
                .iter()
                .all(|message| message.summary_coverage.is_none())
        );

        lifecycle.revision = 3;
        lifecycle.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 3,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reapplied,
                reason: None,
            });
        let reapplied = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "summary",
            messages: &messages,
            context_view: &lifecycle,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("reapplied snapshot");
        assert!(reapplied.messages[..2].iter().all(|message| {
            message
                .summary_coverage
                .as_ref()
                .is_some_and(|coverage| coverage.transaction_id == "summary-tx")
        }));

        lifecycle.revision = 4;
        lifecycle.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 4,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
                reason: Some("source changed".to_string()),
            });
        let invalidated = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "summary",
            messages: &messages,
            context_view: &lifecycle,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("invalidated snapshot");
        assert!(
            invalidated
                .messages
                .iter()
                .all(|message| message.summary_coverage.is_none())
        );
    }

    #[test]
    fn multiple_active_block_operations_produce_distinct_message_and_block_badges() {
        let messages = vec![stored(
            "reasoning",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "first reasoning".repeat(20),
                },
                ContentBlock::Reasoning {
                    text: "second reasoning".repeat(20),
                },
                ContentBlock::Text {
                    text: "visible".to_string(),
                    cache_control: None,
                },
            ],
        )];
        let suppression = |block_index: usize| {
            StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
                selection: StoredReasoningSelection::MessageRanges { ranges: Vec::new() },
                targets: vec![
                    jcode_context_core::build_content_target(&messages, 0, block_index)
                        .expect("reasoning target"),
                ],
                assistant_turns_affected: 1,
                replay_block_kinds: vec![StoredContextBlockKind::Reasoning],
                original_token_estimate: estimate_content_block_tokens(
                    &messages[0].content[block_index],
                ),
                validation_evidence_version: 1,
                validation: Vec::new(),
            })
        };
        let state = StoredContextViewState {
            revision: 2,
            transactions: vec![
                transaction("first-suppression", 1, vec![suppression(0)]),
                transaction("second-suppression", 2, vec![suppression(1)]),
            ],
            ..StoredContextViewState::default()
        };
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "badges",
            messages: &messages,
            context_view: &state,
            processing: false,
            provider: &SnapshotProvider,
            route: "test-route",
        })
        .expect("multiple badges snapshot");
        assert_eq!(snapshot.messages[0].active_operations.len(), 2);
        assert_eq!(snapshot.messages[0].blocks[0].active_operations.len(), 1);
        assert_eq!(snapshot.messages[0].blocks[1].active_operations.len(), 1);
        assert_ne!(
            snapshot.messages[0].blocks[0].active_operations[0].transaction_id,
            snapshot.messages[0].blocks[1].active_operations[0].transaction_id
        );
    }

    #[test]
    fn reasoning_removability_tracks_provider_family_and_fails_closed_on_builder_rejection() {
        let messages = vec![stored(
            "all-reasoning",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "generic".to_string(),
                },
                ContentBlock::ReasoningTrace {
                    text: "trace".to_string(),
                },
                ContentBlock::AnthropicThinking {
                    thinking: "anthropic".to_string(),
                    signature: "signed".to_string(),
                },
                ContentBlock::OpenAIReasoning {
                    id: "reasoning-id".to_string(),
                    summary: vec!["openai".to_string()],
                    encrypted_content: Some("opaque".to_string()),
                    status: Some("completed".to_string()),
                },
                ContentBlock::Text {
                    text: "visible".to_string(),
                    cache_control: None,
                },
            ],
        )];
        let cases = [
            (
                ContextProviderFamily::OpenRouterCompatible,
                ContextReasoningBlockKind::GenericReasoning,
                StoredContextBlockKind::Reasoning,
            ),
            (
                ContextProviderFamily::Anthropic,
                ContextReasoningBlockKind::AnthropicThinking,
                StoredContextBlockKind::AnthropicThinking,
            ),
            (
                ContextProviderFamily::OpenAiResponses,
                ContextReasoningBlockKind::OpenAiReasoning,
                StoredContextBlockKind::OpenAiReasoning,
            ),
        ];
        for (family, supported_reasoning, expected) in cases {
            let provider = ReasoningSnapshotProvider {
                family,
                supported_reasoning: Some(supported_reasoning),
                builder_supported: true,
            };
            let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
                session_id: "reasoning",
                messages: &messages,
                context_view: &StoredContextViewState::default(),
                processing: false,
                provider: &provider,
                route: "test-route",
            })
            .expect("provider-aware snapshot");
            assert_eq!(
                snapshot.messages[0].removable_reasoning_kinds,
                vec![expected]
            );
            assert!(
                !snapshot.messages[0]
                    .removable_reasoning_kinds
                    .contains(&StoredContextBlockKind::ReasoningTrace)
            );
        }

        let rejected = ReasoningSnapshotProvider {
            family: ContextProviderFamily::OpenRouterCompatible,
            supported_reasoning: Some(ContextReasoningBlockKind::GenericReasoning),
            builder_supported: false,
        };
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "rejected",
            messages: &messages,
            context_view: &StoredContextViewState::default(),
            processing: false,
            provider: &rejected,
            route: "test-route",
        })
        .expect("fail-closed snapshot");
        assert!(snapshot.messages[0].removable_reasoning_kinds.is_empty());
        assert!(
            snapshot.messages[0]
                .blocks
                .iter()
                .all(|block| !block.provider_removable_reasoning)
        );
    }

    #[test]
    fn snapshot_pagination_preserves_global_identity_and_enforces_bounds() {
        let messages = vec![
            stored(
                "message-1",
                Role::User,
                vec![ContentBlock::Text {
                    text: "one".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "message-2",
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "two".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "message-3",
                Role::User,
                vec![ContentBlock::Text {
                    text: "three".to_string(),
                    cache_control: None,
                }],
            ),
        ];
        let context_view = StoredContextViewState {
            revision: 7,
            ..StoredContextViewState::default()
        };
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "session-page",
            messages: &messages,
            context_view: &context_view,
            processing: false,
            provider: &SnapshotProvider,
            route: "fixture-route",
        })
        .expect("build canonical snapshot");
        let digest = snapshot.transcript_digest;

        let first =
            paginate_context_editor_snapshot(snapshot.clone(), 0, 2).expect("first bounded page");
        assert_eq!(first.context_revision, 7);
        assert_eq!(first.transcript_digest, digest);
        assert_eq!(first.raw_message_count, 3);
        assert_eq!(first.message_page_start, 0);
        assert_eq!(first.message_page_end, 2);
        assert_eq!(first.next_message_page_start, Some(2));
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["message-1", "message-2"]
        );

        let final_page =
            paginate_context_editor_snapshot(snapshot.clone(), 2, 1).expect("final bounded page");
        assert_eq!(final_page.message_page_start, 2);
        assert_eq!(final_page.message_page_end, 3);
        assert_eq!(final_page.next_message_page_start, None);
        assert_eq!(final_page.messages[0].message_id, "message-3");

        let empty_tail = paginate_context_editor_snapshot(snapshot.clone(), 3, 1)
            .expect("page start at raw length is a valid empty tail");
        assert!(empty_tail.messages.is_empty());
        assert_eq!(empty_tail.message_page_start, 3);
        assert_eq!(empty_tail.message_page_end, 3);
        assert_eq!(empty_tail.next_message_page_start, None);

        assert!(paginate_context_editor_snapshot(snapshot.clone(), 4, 1).is_err());
        assert!(paginate_context_editor_snapshot(snapshot.clone(), 0, 0).is_err());
        assert!(
            paginate_context_editor_snapshot(snapshot, 0, CONTEXT_SNAPSHOT_MAX_PAGE_SIZE + 1)
                .is_err()
        );
    }

    #[test]
    fn lazy_detail_chunks_unicode_and_never_exposes_opaque_provider_state() {
        let messages = vec![stored(
            "message-detail",
            Role::Assistant,
            vec![
                ContentBlock::Text {
                    text: "A🙂éZ".to_string(),
                    cache_control: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "BASE64_SECRET".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path":"src/lib.rs"}),
                    thought_signature: Some("THOUGHT_SIGNATURE_SECRET".to_string()),
                },
                ContentBlock::OpenAIReasoning {
                    id: "reasoning-1".to_string(),
                    summary: vec!["safe summary".to_string()],
                    encrypted_content: Some("ENCRYPTED_REASONING_SECRET".to_string()),
                    status: Some("completed".to_string()),
                },
                ContentBlock::OpenAICompaction {
                    encrypted_content: "ENCRYPTED_COMPACTION_SECRET".to_string(),
                },
            ],
        )];
        let context_view = StoredContextViewState {
            revision: 9,
            ..StoredContextViewState::default()
        };
        let digest = authoritative_transcript_digest(&messages);
        let detail = |block_ordinal, start_char, max_chars| {
            build_context_message_detail(ContextMessageDetailInput {
                session_id: "session-detail",
                messages: &messages,
                context_view: &context_view,
                expected_context_revision: 9,
                expected_transcript_digest: digest,
                message_id: "message-detail",
                block_ordinal,
                start_char,
                max_chars,
            })
        };

        let unicode = detail(0, 1, 2).expect("Unicode detail chunk");
        assert_eq!(unicode.content.text, "🙂é");
        assert_eq!(unicode.content.start_char, 1);
        assert_eq!(unicode.content.end_char, 3);
        assert_eq!(unicode.content.total_chars, 4);
        assert_eq!(unicode.content.next_start_char, Some(3));

        let image = detail(1, 0, 16).expect("image metadata detail");
        assert_eq!(image.format, ContextMessageDetailFormat::MetadataOnly);
        assert_eq!(image.content.text, "");
        assert_eq!(image.image_media_type.as_deref(), Some("image/png"));
        assert_eq!(image.image_encoded_bytes, Some("BASE64_SECRET".len()));
        assert!(
            !serde_json::to_string(&image)
                .expect("serialize image detail")
                .contains("BASE64_SECRET")
        );

        let tool = detail(2, 0, 1_024).expect("tool detail");
        assert_eq!(tool.format, ContextMessageDetailFormat::Json);
        assert!(tool.opaque_signature_present);
        assert!(tool.content.text.contains("src/lib.rs"));
        assert!(!tool.content.text.contains("THOUGHT_SIGNATURE_SECRET"));

        let reasoning = detail(3, 0, 1_024).expect("OpenAI reasoning detail");
        assert_eq!(reasoning.content.text, "safe summary");
        assert!(reasoning.encrypted_state_present);
        assert!(
            !serde_json::to_string(&reasoning)
                .expect("serialize reasoning detail")
                .contains("ENCRYPTED_REASONING_SECRET")
        );

        let compaction = detail(4, 0, 16).expect("legacy compaction metadata detail");
        assert_eq!(compaction.format, ContextMessageDetailFormat::MetadataOnly);
        assert!(compaction.encrypted_state_present);
        assert!(
            !serde_json::to_string(&compaction)
                .expect("serialize compaction detail")
                .contains("ENCRYPTED_COMPACTION_SECRET")
        );

        assert!(detail(0, 5, 1).is_err());
        assert!(detail(0, 0, 0).is_err());
        assert!(detail(0, 0, CONTEXT_MESSAGE_DETAIL_MAX_CHARS + 1).is_err());

        let stale_revision = build_context_message_detail(ContextMessageDetailInput {
            session_id: "session-detail",
            messages: &messages,
            context_view: &context_view,
            expected_context_revision: 8,
            expected_transcript_digest: digest,
            message_id: "message-detail",
            block_ordinal: 0,
            start_char: 0,
            max_chars: 16,
        })
        .expect_err("revision mismatch must be stale");
        assert!(stale_revision.to_string().contains("revision changed"));

        let stale_digest = build_context_message_detail(ContextMessageDetailInput {
            session_id: "session-detail",
            messages: &messages,
            context_view: &context_view,
            expected_context_revision: 9,
            expected_transcript_digest: digest.wrapping_add(1),
            message_id: "message-detail",
            block_ordinal: 0,
            start_char: 0,
            max_chars: 16,
        })
        .expect_err("digest mismatch must be stale");
        assert!(stale_digest.to_string().contains("digest changed"));

        let missing_message = build_context_message_detail(ContextMessageDetailInput {
            session_id: "session-detail",
            messages: &messages,
            context_view: &context_view,
            expected_context_revision: 9,
            expected_transcript_digest: digest,
            message_id: "missing-message",
            block_ordinal: 0,
            start_char: 0,
            max_chars: 16,
        })
        .expect_err("missing stable ID must fail");
        assert!(missing_message.to_string().contains("not found"));

        let missing_block = detail(99, 0, 16).expect_err("missing block must fail");
        assert!(missing_block.to_string().contains("no block"));

        let ambiguous_messages = vec![messages[0].clone(), messages[0].clone()];
        let ambiguous_digest = authoritative_transcript_digest(&ambiguous_messages);
        let ambiguous = build_context_message_detail(ContextMessageDetailInput {
            session_id: "session-detail",
            messages: &ambiguous_messages,
            context_view: &context_view,
            expected_context_revision: 9,
            expected_transcript_digest: ambiguous_digest,
            message_id: "message-detail",
            block_ordinal: 0,
            start_char: 0,
            max_chars: 16,
        })
        .expect_err("ambiguous stable ID must fail");
        assert!(ambiguous.to_string().contains("ambiguous"));
    }

    #[test]
    fn startup_context_messages_are_native_authoritative_snapshot_and_detail_source() {
        let mut messages = vec![
            stored(
                "startup-control",
                Role::User,
                vec![ContentBlock::Text {
                    text: "SYNTHETIC_STARTUP_CONTROL".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "startup-file",
                Role::User,
                vec![
                    ContentBlock::Text {
                        text: "synthetic file metadata".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "exact startup body: İstanbul 日本語 🧪".to_string(),
                        cache_control: None,
                    },
                ],
            ),
            stored(
                "startup-stale",
                Role::User,
                vec![ContentBlock::Text {
                    text: "SYNTHETIC_STALE_MARKER".to_string(),
                    cache_control: None,
                }],
            ),
        ];
        for message in &mut messages {
            message.display_role = Some(jcode_session_types::StoredDisplayRole::System);
            message.timestamp = None;
        }
        let context_view = StoredContextViewState::default();
        let provider = SnapshotProvider;
        let snapshot = build_context_editor_snapshot(ContextSnapshotInput {
            session_id: "startup-snapshot-session",
            messages: &messages,
            context_view: &context_view,
            processing: false,
            provider: &provider,
            route: "snapshot-route",
        })
        .expect("Startup Context snapshot");

        assert_eq!(snapshot.raw_message_count, 3);
        assert_eq!(
            snapshot
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["startup-control", "startup-file", "startup-stale"]
        );
        assert!(snapshot.messages.iter().all(|message| {
            message.display_role == Some(jcode_session_types::StoredDisplayRole::System)
        }));

        let detail = build_context_message_detail(ContextMessageDetailInput {
            session_id: "startup-snapshot-session",
            messages: &messages,
            context_view: &context_view,
            expected_context_revision: snapshot.context_revision,
            expected_transcript_digest: snapshot.transcript_digest,
            message_id: "startup-file",
            block_ordinal: 1,
            start_char: 0,
            max_chars: CONTEXT_MESSAGE_DETAIL_MAX_CHARS,
        })
        .expect("exact Startup Context file detail");
        assert_eq!(
            detail.content.text,
            "exact startup body: İstanbul 日本語 🧪"
        );
        assert_eq!(detail.message_id, "startup-file");
        assert_eq!(detail.block_ordinal, 1);
    }
}
