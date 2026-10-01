use jcode_message_types::{
    AnthropicThinkingBinding, ContentBlock, Message, Role, TOOL_OUTPUT_MISSING_TEXT,
    ToolDefinition, sanitize_tool_id,
};
use jcode_provider_core::{
    ANTHROPIC_TOOL_NAME_POLICY, AnthropicConversationCaps, ContextRequestBuilderValidation,
};
use serde::Serialize;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// Claude Code billing attribution text observed in the official CLI's system
/// prompt blocks.
pub const OAUTH_BILLING_HEADER: &str = "cc_version=2.1.280; cc_entrypoint=sdk-cli; cch=33f85;";

const CLAUDE_CODE_IDENTITY: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

/// Minimal user turn appended when a formatted conversation would otherwise end
/// on an assistant message, which Anthropic rejects on non-prefill models.
pub(crate) const CONTINUATION_USER_TURN: &str = "Continue.";

pub mod binding;
mod cache_breakpoints;

pub use cache_breakpoints::{
    INTERMEDIATE_AFTER_POSITIONS, LOOKBACK_POSITIONS, place_cache_breakpoints,
};

/// Format messages for a model with no mid-conversation operator channel:
/// every operator notice is user text, exactly as it was stored.
pub fn format_messages(messages: &[Message]) -> Vec<ApiMessage> {
    format_messages_for(messages, AnthropicConversationCaps::default())
}

/// Format messages for a model with the given mid-conversation capabilities.
///
/// An operator notice (`ContentBlock::OperatorNotice`, INT-01/WP-06 D17)
/// becomes a `role: "system"` message carrying its body when the model
/// accepts one and the documented placement holds: it follows a user
/// message, and it is the last message or an assistant message follows it
/// (a run of notices counts as one). Anywhere else it is the stored user
/// text, merged with the user content around it. The decision is a pure
/// function of the messages, so a rendering changes only when a notice that
/// never got a reply is followed by a new user message, which invalidates no
/// reasoning: nothing was produced after the notice.
///
/// Tool changes a notice announces go into its system message when the model
/// accepts them there. When the notice is user text, they move to a system
/// message of their own before the next assistant message, the first position
/// where the placement rules hold.
pub fn format_messages_for(
    messages: &[Message],
    caps: AnthropicConversationCaps,
) -> Vec<ApiMessage> {
    use std::collections::HashSet;

    // Pre-pass: drop duplicate tool_results for the same tool_use_id.
    //
    // Anthropic rejects the whole request (400 "unexpected `tool_use_id` found
    // in `tool_result` blocks") when a tool_use_id appears twice, because after
    // same-role merging only the first result lines up with the tool_use in the
    // preceding assistant message. Duplicates are produced by the missing
    // tool-output repair racing a still-running tool: the repair inserts a
    // synthetic placeholder result, then the real result lands moments later,
    // and the conversation is permanently unsendable. Prefer the real output
    // over the synthetic placeholder, and otherwise keep the first occurrence.
    let messages = &dedupe_tool_results(messages);

    // First pass: collect all tool_use IDs and tool_result IDs
    let mut tool_use_ids: HashSet<String> = HashSet::new();
    let mut tool_result_ids: HashSet<String> = HashSet::new();

    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_use_ids.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    tool_result_ids.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }

    // Find dangling tool_uses (no matching tool_result)
    let dangling: HashSet<_> = tool_use_ids.difference(&tool_result_ids).cloned().collect();
    if !dangling.is_empty() {
        jcode_logging::info(&format!(
            "[anthropic] Repairing {} dangling tool_use(s) by injecting synthetic tool_results",
            dangling.len()
        ));
    }

    // Second pass: build messages, injecting synthetic tool_results after assistant messages
    // that have dangling tool_uses
    let mut result: Vec<ApiMessage> = Vec::new();

    // Operator notice entries of `result`, by index, with their user form.
    let mut notices: std::collections::HashMap<usize, OperatorForms> =
        std::collections::HashMap::new();
    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };

        if let (
            Role::User,
            [
                ContentBlock::OperatorNotice {
                    text,
                    body,
                    tool_changes,
                },
            ],
        ) = (&msg.role, msg.content.as_slice())
            && caps.system_messages
        {
            let changes = if caps.inline_tool_changes {
                tool_changes.iter().map(tool_change_block).collect()
            } else {
                Vec::new()
            };
            notices.insert(
                result.len(),
                OperatorForms {
                    user_text: text.clone(),
                    changes: changes.clone(),
                },
            );
            let mut content = vec![ApiContentBlock::Text {
                text: body.clone(),
                cache_control: None,
            }];
            content.extend(changes);
            result.push(ApiMessage {
                role: "system".to_string(),
                content,
            });
            continue;
        }

        let content = format_content_blocks(&msg.content);

        if !content.is_empty() {
            result.push(ApiMessage {
                role: role.to_string(),
                content,
            });
        }

        // If this is an assistant message with dangling tool_uses, inject synthetic results
        if matches!(msg.role, Role::Assistant) {
            let mut synthetic_results: Vec<ApiContentBlock> = Vec::new();
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, .. } = block
                    && dangling.contains(id)
                {
                    synthetic_results.push(ApiContentBlock::ToolResult {
                        tool_use_id: sanitize_tool_id(id),
                        content: ToolResultContent::Text(
                            "[Session interrupted before tool execution completed]".to_string(),
                        ),
                        is_error: true,
                        cache_control: None,
                    });
                }
            }
            if !synthetic_results.is_empty() {
                result.push(ApiMessage {
                    role: "user".to_string(),
                    content: synthetic_results,
                });
            }
        }
    }

    let result = place_operator_messages(result, &notices);

    // Third pass: merge consecutive messages of the same role
    // Anthropic API requires strictly alternating user/assistant messages
    let pre_merge_count = result.len();
    let mut merged: Vec<ApiMessage> = Vec::new();
    for msg in result {
        if let Some(last) = merged.last_mut()
            && last.role == msg.role
        {
            last.content.extend(msg.content);
            continue;
        }
        merged.push(msg);
    }

    if merged.len() != pre_merge_count {
        jcode_logging::info(&format!(
            "[anthropic] Merged {} consecutive same-role messages",
            pre_merge_count - merged.len()
        ));
    }

    // Anthropic requires every tool_result answering the previous assistant
    // turn to lead the user message. Merging consecutive user messages puts
    // delivered context (a system reminder or an injected input) beside tool
    // results, and a result after a text block makes the API report the
    // tool_use as missing its result. Stable partition: results first, every
    // other block keeps its order.
    for msg in merged.iter_mut().filter(|m| m.role == "user") {
        let first_non_result = msg
            .content
            .iter()
            .position(|b| !matches!(b, ApiContentBlock::ToolResult { .. }));
        let needs_reorder = first_non_result.is_some_and(|start| {
            msg.content[start..]
                .iter()
                .any(|b| matches!(b, ApiContentBlock::ToolResult { .. }))
        });
        if needs_reorder {
            let (results, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut msg.content)
                .into_iter()
                .partition(|b| matches!(b, ApiContentBlock::ToolResult { .. }));
            msg.content = results;
            msg.content.extend(rest);
        }
    }

    // Current Claude models reject a request that ends on an assistant turn
    // (no prefill). Every turn now starts with persisted user-role content,
    // including a reload resume, whose continuation is its delivered content,
    // so this is unreachable. It remains a guard against history damage and
    // is logged as a defect: the appended turn is not in the transcript, so
    // the next request would not be an append of this one.
    if merged.last().is_some_and(|last| last.role == "assistant") {
        jcode_logging::error(
            "[anthropic] INV-1 defect: conversation ended with an assistant message; \
             appending an unpersisted continuation user turn to avoid a prefill rejection (400)",
        );
        merged.push(ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: CONTINUATION_USER_TURN.to_string(),
                cache_control: None,
            }],
        });
    }

    // Validate: check each assistant message with tool_use has matching tool_result in next user message
    for (i, msg) in merged.iter().enumerate() {
        if msg.role == "assistant" {
            let tool_uses: Vec<&String> = msg
                .content
                .iter()
                .filter_map(|b| {
                    if let ApiContentBlock::ToolUse { id, .. } = b {
                        Some(id)
                    } else {
                        None
                    }
                })
                .collect();

            if !tool_uses.is_empty() {
                // Check next message
                if let Some(next) = merged.get(i + 1) {
                    if next.role != "user" {
                        jcode_logging::warn(&format!(
                            "[anthropic] Message {} has tool_use but next message is {} (should be user)",
                            i, next.role
                        ));
                    } else {
                        let tool_results: std::collections::HashSet<&String> = next
                            .content
                            .iter()
                            .filter_map(|b| {
                                if let ApiContentBlock::ToolResult { tool_use_id, .. } = b {
                                    Some(tool_use_id)
                                } else {
                                    None
                                }
                            })
                            .collect();

                        for tu_id in &tool_uses {
                            if !tool_results.contains(*tu_id) {
                                jcode_logging::warn(&format!(
                                    "[anthropic] Message {} has tool_use {} but no matching tool_result in message {}",
                                    i,
                                    tu_id,
                                    i + 1
                                ));
                            }
                        }
                    }
                } else {
                    jcode_logging::warn(&format!(
                        "[anthropic] Message {} has tool_use but no next message",
                        i
                    ));
                }
            }
        }
    }

    merged
}

/// The user form of an operator notice rendered as a system message, and
/// the tool-change blocks it carries.
struct OperatorForms {
    user_text: String,
    changes: Vec<ApiContentBlock>,
}

/// Keep each run of system messages where the placement rules hold; turn the
/// others into user text, moving their tool changes to the next valid
/// position (before the next assistant message, or the end).
fn place_operator_messages(
    entries: Vec<ApiMessage>,
    notices: &std::collections::HashMap<usize, OperatorForms>,
) -> Vec<ApiMessage> {
    if notices.is_empty() {
        return entries;
    }
    let mut placed: Vec<ApiMessage> = Vec::with_capacity(entries.len());
    let mut pending: Vec<ApiContentBlock> = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        if entries[index].role != "system" {
            if entries[index].role == "assistant" && !pending.is_empty() {
                placed.push(ApiMessage {
                    role: "system".to_string(),
                    content: std::mem::take(&mut pending),
                });
            }
            placed.push(entries[index].clone());
            index += 1;
            continue;
        }
        let end = entries[index..]
            .iter()
            .position(|entry| entry.role != "system")
            .map_or(entries.len(), |offset| index + offset);
        let follows_user = placed.last().is_some_and(|last| last.role == "user");
        let before_assistant = entries.get(end).is_none_or(|next| next.role == "assistant");
        if follows_user && before_assistant {
            placed.extend(entries[index..end].iter().cloned());
        } else {
            for position in index..end {
                let forms = &notices[&position];
                placed.push(ApiMessage {
                    role: "user".to_string(),
                    content: vec![ApiContentBlock::Text {
                        text: forms.user_text.clone(),
                        cache_control: None,
                    }],
                });
                pending.extend(forms.changes.iter().cloned());
            }
        }
        index = end;
    }
    if !pending.is_empty() {
        placed.push(ApiMessage {
            role: "system".to_string(),
            content: pending,
        });
    }
    placed
}

/// A tool-set change as a mid-conversation tool-change block: additions and
/// redefinitions by value (`inline-tools-2026-09-15`), removals by name.
fn tool_change_block(change: &jcode_message_types::ToolSetChange) -> ApiContentBlock {
    use jcode_message_types::ToolSetChange;
    match change {
        ToolSetChange::Added { definition } | ToolSetChange::Redefined { definition } => {
            ApiContentBlock::ToolAddition {
                tool: ApiToolChangeTarget::ToolDefinition {
                    definition: format_tools(std::slice::from_ref(definition))
                        .pop()
                        .expect("one definition formats to one tool"),
                },
                cache_control: None,
            }
        }
        ToolSetChange::Removed { name } => ApiContentBlock::ToolRemoval {
            tool: ApiToolChangeTarget::ToolReference {
                name: ANTHROPIC_TOOL_NAME_POLICY.wire_name(name).to_string(),
            },
        },
    }
}

/// Returns true when a tool_result body is one of the synthetic placeholders
/// injected by the missing tool-output repair paths rather than real output.
fn is_placeholder_tool_result(content: &str, is_error: Option<bool>) -> bool {
    is_error.unwrap_or(false)
        && (content.contains(TOOL_OUTPUT_MISSING_TEXT)
            || content.contains("[Session interrupted before tool execution completed]"))
}

/// Remove duplicate `tool_result` blocks so each `tool_use_id` is answered
/// exactly once, preferring real output over a synthetic placeholder.
/// Messages left with no content at all are dropped by the caller's
/// `!content.is_empty()` guard.
fn dedupe_tool_results(messages: &[Message]) -> Vec<Message> {
    use std::collections::HashMap;

    // Winner position per tool_use_id: the first real result if one exists,
    // otherwise the first occurrence at all.
    let mut winner: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut winner_is_real: HashMap<&str, bool> = HashMap::new();
    let mut duplicate_seen = false;

    for (mi, msg) in messages.iter().enumerate() {
        for (bi, block) in msg.content.iter().enumerate() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            let real = !is_placeholder_tool_result(content, *is_error);
            match winner_is_real.get(tool_use_id.as_str()) {
                None => {
                    winner.insert(tool_use_id, (mi, bi));
                    winner_is_real.insert(tool_use_id, real);
                }
                Some(false) if real => {
                    // Upgrade a placeholder winner to the real output.
                    winner.insert(tool_use_id, (mi, bi));
                    winner_is_real.insert(tool_use_id, true);
                    duplicate_seen = true;
                }
                Some(_) => duplicate_seen = true,
            }
        }
    }

    if !duplicate_seen {
        return messages.to_vec();
    }

    let dropped = std::cell::Cell::new(0usize);
    let out: Vec<Message> = messages
        .iter()
        .enumerate()
        .map(|(mi, msg)| {
            let mut msg = msg.clone();
            let mut bi = 0usize;
            msg.content.retain(|block| {
                let index = bi;
                bi += 1;
                let ContentBlock::ToolResult { tool_use_id, .. } = block else {
                    return true;
                };
                let keep = winner.get(tool_use_id.as_str()) == Some(&(mi, index));
                if !keep {
                    dropped.set(dropped.get() + 1);
                }
                keep
            });
            msg
        })
        .collect();

    if dropped.get() > 0 {
        jcode_logging::warn(&format!(
            "[anthropic] Dropped {} duplicate tool_result block(s); each tool_use_id may be \
             answered only once",
            dropped.get()
        ));
    }
    out
}

/// Convert our ContentBlock to Anthropic API format
pub fn format_content_blocks(blocks: &[ContentBlock]) -> Vec<ApiContentBlock> {
    let mut result: Vec<ApiContentBlock> = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => {
                // A text block that immediately follows an image-bearing tool_result is the
                // "[Attached image associated with the preceding tool result: ...]" label
                // emitted alongside image tool outputs. The Anthropic API requires every
                // tool_result for a parallel tool-call turn to be contiguous in the next user
                // message; a sibling text block wedged between tool_results makes the API
                // report later tool_use ids as missing their tool_result. Fold the label into
                // the tool_result's content blocks so the tool_results stay contiguous.
                if let Some(ApiContentBlock::ToolResult {
                    content: ToolResultContent::Blocks(blocks),
                    ..
                }) = result.last_mut()
                    && blocks
                        .iter()
                        .any(|b| matches!(b, ToolResultContentBlock::Image { .. }))
                {
                    blocks.push(ToolResultContentBlock::Text { text: text.clone() });
                } else {
                    result.push(ApiContentBlock::Text {
                        text: text.clone(),
                        cache_control: None,
                    });
                }
            }
            // Replayed byte-exact and in stored order. A block stored before
            // jcode recorded bindings may have been spliced or reordered by
            // the old accumulators, so it is never replayed (INT-01 DESIGN
            // §4.2); see `unbound_thinking_block_count`.
            ContentBlock::AnthropicThinking {
                thinking,
                signature,
                binding: Some(binding),
            } => {
                result.push(ApiContentBlock::Thinking {
                    thinking: thinking.clone(),
                    signature: signature.clone(),
                    binding: Some(binding.clone()),
                });
            }
            ContentBlock::AnthropicThinking { binding: None, .. } => {}
            // Operator notices outside a standalone notice message are user
            // text, exactly as stored.
            ContentBlock::OperatorNotice { text, .. } => result.push(ApiContentBlock::Text {
                text: text.clone(),
                cache_control: None,
            }),
            ContentBlock::AnthropicRedactedThinking { data, binding } => {
                result.push(ApiContentBlock::RedactedThinking {
                    data: data.clone(),
                    binding: binding.clone(),
                });
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                result.push(ApiContentBlock::ToolUse {
                    id: sanitize_tool_id(id),
                    name: ANTHROPIC_TOOL_NAME_POLICY.wire_name(name).to_string(),
                    input: if input.is_object() {
                        input.clone()
                    } else {
                        serde_json::json!({})
                    },
                    cache_control: None,
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                result.push(ApiContentBlock::ToolResult {
                    tool_use_id: sanitize_tool_id(tool_use_id),
                    content: ToolResultContent::Text(content.clone()),
                    is_error: is_error.unwrap_or(false),
                    cache_control: None,
                });
            }
            ContentBlock::Image { media_type, data } => {
                let img_block = ToolResultContentBlock::Image {
                    source: ApiImageSource {
                        kind: "base64".to_string(),
                        media_type: media_type.clone(),
                        data: data.clone(),
                    },
                };
                if let Some(ApiContentBlock::ToolResult { content, .. }) = result.last_mut() {
                    match content {
                        ToolResultContent::Text(text) => {
                            let text_block = ToolResultContentBlock::Text {
                                text: std::mem::take(text),
                            };
                            *content = ToolResultContent::Blocks(vec![text_block, img_block]);
                        }
                        ToolResultContent::Blocks(blocks) => {
                            blocks.push(img_block);
                        }
                    }
                } else {
                    result.push(ApiContentBlock::Image {
                        source: ApiImageSource {
                            kind: "base64".to_string(),
                            media_type: media_type.clone(),
                            data: data.clone(),
                        },
                        cache_control: None,
                    });
                }
            }
            _ => {}
        }
    }
    result
}

/// Number of stored Anthropic thinking blocks the formatter does not replay
/// because they carry no binding record.
pub fn unbound_thinking_block_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::AnthropicThinking { binding: None, .. }))
        .count()
}

/// Validate projected provider messages through the production Anthropic formatter.
///
/// This validates formatter output rather than duplicating the wire serializer. It rejects
/// unsigned thinking and verifies strict tool-result placement after normal merging,
/// deduplication, image association, and dangling-result repair have run.
pub fn validate_projected_messages(
    messages: &[Message],
) -> Result<ContextRequestBuilderValidation, String> {
    use std::collections::HashSet;

    let formatted = format_messages(messages);
    if formatted.is_empty() {
        return Err(
            "Projected history normalizes to no Anthropic request messages; the request would not contain a valid conversation turn."
                .to_string(),
        );
    }

    for pair in formatted.windows(2) {
        if pair[0].role == pair[1].role {
            return Err(format!(
                "Anthropic formatter left consecutive '{}' messages instead of strict role alternation.",
                pair[0].role
            ));
        }
    }

    let mut seen_tool_use_ids = HashSet::new();
    for (message_index, message) in formatted.iter().enumerate() {
        for block in &message.content {
            // Empty thinking text is valid (the default `display: "omitted"`
            // returns it); the signature or redacted payload is what replays.
            let unsigned = match block {
                ApiContentBlock::Thinking { signature, .. } => signature.trim().is_empty(),
                ApiContentBlock::RedactedThinking { data, .. } => data.trim().is_empty(),
                _ => false,
            };
            if unsigned {
                return Err(format!(
                    "Anthropic message {message_index} contains a thinking block without its signed payload."
                ));
            }
        }

        if message.role != "assistant" {
            continue;
        }
        let tool_use_ids: Vec<&str> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ApiContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        if tool_use_ids.is_empty() {
            continue;
        }
        for tool_use_id in &tool_use_ids {
            if !seen_tool_use_ids.insert(*tool_use_id) {
                return Err(format!(
                    "Anthropic projected history contains duplicate normalized tool_use id '{tool_use_id}'."
                ));
            }
        }

        let next = formatted.get(message_index + 1).ok_or_else(|| {
            format!(
                "Anthropic assistant message {message_index} contains tool calls but no following user tool-result message."
            )
        })?;
        if next.role != "user" {
            return Err(format!(
                "Anthropic assistant message {message_index} contains tool calls but the following message has role '{}'.",
                next.role
            ));
        }

        let expected: HashSet<&str> = tool_use_ids.iter().copied().collect();
        let contiguous_results: Vec<&str> = next
            .content
            .iter()
            .take_while(|block| matches!(block, ApiContentBlock::ToolResult { .. }))
            .filter_map(|block| match block {
                ApiContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        let actual: HashSet<&str> = contiguous_results.iter().copied().collect();
        if contiguous_results.len() != expected.len() || actual != expected {
            return Err(format!(
                "Anthropic assistant message {message_index} does not have one contiguous leading tool_result block for every tool_use in the following user message."
            ));
        }
    }

    Ok(ContextRequestBuilderValidation::new(formatted.len()))
}

/// Normalize a tool schema for Anthropic's `input_schema`.
///
/// Anthropic accepts JSON Schema combinators inside object properties but
/// rejects `oneOf`/`anyOf`/`allOf` at the top level, and requires an object
/// schema with a `properties` map. The subset and the rewrites live in
/// `jcode-schema-dialect` so every provider shares one implementation and one
/// set of regression tests.
///
/// Widening a top-level combiner loses the per-branch constraint, which is
/// intended: runtime tool deserialization remains the authority on which
/// combination is actually valid.
pub fn anthropic_input_schema(schema: &Value) -> Value {
    jcode_schema_dialect::normalize(schema, &jcode_schema_dialect::registry::ANTHROPIC)
}

/// Convert registry tool definitions to Anthropic tools.
///
/// OAuth and API-key requests carry the same tools: every registry tool, under
/// its registry name (or its [`ANTHROPIC_TOOL_NAME_POLICY`] entry), with its
/// registry description and the Anthropic dialect of its schema. There are no
/// provider-authored stand-ins (INT-01 D3, D12). Prompt-cache markers are
/// placed on the complete request by [`place_cache_breakpoints`].
pub fn format_tools(tools: &[ToolDefinition]) -> Vec<ApiTool> {
    tools
        .iter()
        .map(|tool| ApiTool {
            name: ANTHROPIC_TOOL_NAME_POLICY.wire_name(&tool.name).to_string(),
            description: tool.description.clone(),
            input_schema: anthropic_input_schema(&tool.input_schema),
            cache_control: None,
        })
        .collect()
}

/// Tool-choice policy for every request that carries tools.
///
/// jcode's `batch` tool is the parallelism mechanism for every provider, so
/// native parallel tool calls are disabled, matching GPT's
/// `parallel_tool_calls: false` (INT-01 D1). It is sent identically on every
/// request with tools because a changed `tool_choice` invalidates the cached
/// message prefix.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ApiToolChoice {
    #[serde(rename = "type")]
    kind: &'static str,
    disable_parallel_tool_use: bool,
}

impl ApiToolChoice {
    pub fn for_tools(tools: &[ApiTool]) -> Option<Self> {
        (!tools.is_empty()).then_some(Self {
            kind: "auto",
            disable_parallel_tool_use: true,
        })
    }
}

#[derive(Serialize, Clone)]
pub struct ApiRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<ApiSystem>,
    pub messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ApiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ApiToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ApiMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ApiThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<ApiOutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub stream: bool,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiThinking {
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        block_binding: Option<ApiBlockBinding>,
    },
    Enabled {
        budget_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        block_binding: Option<ApiBlockBinding>,
    },
    /// Thinking off, for models whose thinking is on unless disabled
    /// (`ThinkingOff::Disabled`). It takes no other field.
    Disabled,
}

impl ApiThinking {
    pub fn block_binding(&self) -> Option<&ApiBlockBinding> {
        match self {
            Self::Adaptive { block_binding, .. } | Self::Enabled { block_binding, .. } => {
                block_binding.as_ref()
            }
            Self::Disabled => None,
        }
    }

    /// Whether the model thinks under this parameter.
    pub fn thinks(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// Beta that allows `thinking.block_binding` and adds `input_transformations`
/// to responses.
pub const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

/// Beta for mid-conversation tool changes by reference: a `role: "system"`
/// message carrying `tool_addition` (of a tool declared with
/// `defer_loading: true`) or `tool_removal` blocks.
pub const MID_CONVERSATION_TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";

/// Beta for tool changes by value: a `tool_addition` carrying a complete
/// definition, which adds a tool or replaces a same-name one from that message
/// on. It also covers changes by reference.
pub const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// `thinking.block_binding`: what the API does with a replayed thinking block
/// whose conversation prefix changed. Requires
/// [`THINKING_BINDING_CONTROLS_BETA`].
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiBlockBinding {
    pub prefix_mismatch_behavior: PrefixMismatchBehavior,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrefixMismatchBehavior {
    /// Reject the request with a 400.
    Error,
    /// Drop the first mismatched block and every later thinking block, and
    /// report them in `input_transformations`.
    DropBlock,
}

#[derive(Serialize, Clone)]
pub struct ApiOutputConfig {
    pub effort: String,
}

#[derive(Serialize, Clone)]
pub struct ApiMetadata {
    pub user_id: String,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum ApiSystem {
    Blocks(Vec<ApiSystemBlock>),
}

/// Cache control for prompt caching
#[derive(Serialize, Clone)]
pub struct CacheControlParam {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<&'static str>,
}

impl CacheControlParam {
    pub(crate) fn ephemeral(cache_ttl_1h: bool) -> Self {
        if cache_ttl_1h {
            Self::ephemeral_1h()
        } else {
            Self {
                kind: "ephemeral",
                ttl: None,
            }
        }
    }

    fn ephemeral_1h() -> Self {
        Self {
            kind: "ephemeral",
            ttl: Some("1h"),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct ApiSystemBlock {
    #[serde(rename = "type")]
    pub block_type: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlParam>,
    /// The OAuth client billing header. The API neither binds thinking to it
    /// nor caches it (measured, INT-01/WP-06 probe G6.6), so
    /// [`binding`] digests represent it by a fixed text and a client-version
    /// sync invalidates nothing. Never sent.
    #[serde(skip)]
    pub client_billing: bool,
}

/// Build the top-level `system`: the OAuth identity blocks (when on OAuth) and
/// the static prompt. Nothing per-request belongs here; dynamic context is
/// delivered as persisted transcript content so the prefix never changes.
/// [`place_cache_breakpoints`] marks its last block.
pub fn build_system_param(system: &str, is_oauth: bool) -> Option<ApiSystem> {
    let mut blocks = Vec::new();
    if is_oauth {
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: format!("x-anthropic-billing-header: {}", OAUTH_BILLING_HEADER),
            cache_control: None,
            client_billing: true,
        });
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: CLAUDE_CODE_IDENTITY.to_string(),
            cache_control: None,
            client_billing: false,
        });
    }
    if !system.is_empty() {
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: system.to_string(),
            cache_control: None,
            client_billing: false,
        });
    }
    (!blocks.is_empty()).then_some(ApiSystem::Blocks(blocks))
}

#[derive(Serialize, Clone)]
pub struct ApiMessage {
    pub role: String,
    pub content: Vec<ApiContentBlock>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum ApiContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlParam>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlParam>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlParam>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
        /// jcode's record of where the block was produced. Never sent.
        #[serde(skip)]
        binding: Option<AnthropicThinkingBinding>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        data: String,
        /// jcode's record of where the block was produced. Never sent.
        #[serde(skip)]
        binding: AnthropicThinkingBinding,
    },
    #[serde(rename = "image")]
    Image {
        source: ApiImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlParam>,
    },
    /// A tool offered from this `role: "system"` message on.
    #[serde(rename = "tool_addition")]
    ToolAddition {
        tool: ApiToolChangeTarget,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControlParam>,
    },
    /// A tool withdrawn from this `role: "system"` message on. Its definition
    /// stays in `tools`.
    #[serde(rename = "tool_removal")]
    ToolRemoval { tool: ApiToolChangeTarget },
}

/// The tool a mid-conversation tool change names.
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiToolChangeTarget {
    /// A tool by name.
    ToolReference { name: String },
    /// A complete tool definition (`inline-tools-2026-09-15`).
    ToolDefinition { definition: ApiTool },
}

impl ApiContentBlock {
    /// Whether the API accepts a prompt-cache breakpoint on this block.
    /// Thinking blocks do not take one.
    pub fn accepts_cache_control(&self) -> bool {
        self.cache_control_ref().is_some()
    }

    fn cache_control_ref(&self) -> Option<&Option<CacheControlParam>> {
        match self {
            Self::Text { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::ToolAddition { cache_control, .. } => Some(cache_control),
            Self::Thinking { .. } | Self::RedactedThinking { .. } | Self::ToolRemoval { .. } => {
                None
            }
        }
    }

    /// The block's breakpoint slot, when it accepts one.
    pub(crate) fn cache_control_mut(&mut self) -> Option<&mut Option<CacheControlParam>> {
        match self {
            Self::Text { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::ToolAddition { cache_control, .. } => Some(cache_control),
            Self::Thinking { .. } | Self::RedactedThinking { .. } | Self::ToolRemoval { .. } => {
                None
            }
        }
    }

    /// Whether this block carries a prompt-cache breakpoint.
    pub fn has_cache_control(&self) -> bool {
        self.cache_control_ref().is_some_and(Option::is_some)
    }
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ToolResultContentBlock>),
}

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum ToolResultContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ApiImageSource },
}

#[derive(Serialize, Clone)]
pub struct ApiImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Serialize, Clone)]
pub struct ApiTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlParam>,
}

#[cfg(test)]
#[path = "tool_surface_tests.rs"]
mod tool_surface_tests;

#[cfg(test)]
#[path = "trailing_assistant_repair_tests.rs"]
mod trailing_assistant_repair_tests;

#[cfg(test)]
#[path = "tool_results_first_tests.rs"]
mod tool_results_first_tests;

#[cfg(test)]
#[path = "duplicate_tool_result_tests.rs"]
mod duplicate_tool_result_tests;

#[cfg(test)]
#[path = "wedge_fixture_check.rs"]
mod wedge_fixture_check;

#[cfg(test)]
#[path = "context_validation_tests.rs"]
mod context_validation_tests;

#[cfg(test)]
#[path = "operator_messages_tests.rs"]
mod operator_messages_tests;
