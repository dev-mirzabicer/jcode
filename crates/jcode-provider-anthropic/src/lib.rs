use jcode_message_types::{
    AnthropicThinkingBinding, ContentBlock, Message, Role, TOOL_OUTPUT_MISSING_TEXT,
    ToolDefinition, sanitize_tool_id,
};
use jcode_provider_core::{ANTHROPIC_TOOL_NAME_POLICY, ContextRequestBuilderValidation};
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

pub fn format_messages(messages: &[Message]) -> Vec<ApiMessage> {
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

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };

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
/// provider-authored stand-ins (INT-01 D3, D12). The final tool carries the
/// tools prompt-cache breakpoint.
pub fn format_tools(tools: &[ToolDefinition], cache_ttl_1h: bool) -> Vec<ApiTool> {
    let len = tools.len();
    tools
        .iter()
        .enumerate()
        .map(|(i, tool)| ApiTool {
            name: ANTHROPIC_TOOL_NAME_POLICY.wire_name(&tool.name).to_string(),
            description: tool.description.clone(),
            input_schema: anthropic_input_schema(&tool.input_schema),
            cache_control: if i + 1 == len {
                Some(CacheControlParam::ephemeral(cache_ttl_1h))
            } else {
                None
            },
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
}

impl ApiThinking {
    pub fn block_binding(&self) -> Option<&ApiBlockBinding> {
        match self {
            Self::Adaptive { block_binding, .. } | Self::Enabled { block_binding, .. } => {
                block_binding.as_ref()
            }
        }
    }
}

/// Beta that allows `thinking.block_binding` and adds `input_transformations`
/// to responses.
pub const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

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
    fn ephemeral(cache_ttl_1h: bool) -> Self {
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
}

/// Build the top-level `system`: the OAuth identity blocks (when on OAuth) and
/// the cached static prompt. Nothing per-request belongs here; dynamic context
/// is delivered as persisted transcript content so the prefix never changes.
pub fn build_system_param(system: &str, is_oauth: bool, cache_ttl_1h: bool) -> Option<ApiSystem> {
    let mut blocks = Vec::new();
    if is_oauth {
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: format!("x-anthropic-billing-header: {}", OAUTH_BILLING_HEADER),
            cache_control: None,
        });
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: CLAUDE_CODE_IDENTITY.to_string(),
            cache_control: None,
        });
    }
    if !system.is_empty() {
        blocks.push(ApiSystemBlock {
            block_type: "text",
            text: system.to_string(),
            cache_control: Some(CacheControlParam::ephemeral(cache_ttl_1h)),
        });
    }
    (!blocks.is_empty()).then_some(ApiSystem::Blocks(blocks))
}

pub fn format_messages_with_identity(
    messages: Vec<ApiMessage>,
    _is_oauth: bool,
    cache_ttl_1h: bool,
) -> Vec<ApiMessage> {
    let mut out = messages;

    // Add cache breakpoints for both OAuth and non-OAuth paths
    add_message_cache_breakpoint(&mut out, cache_ttl_1h);

    out
}

/// Add cache_control to messages for conversation caching.
///
/// Strategy: sliding two-marker window
///   - Second-to-last assistant message → READ marker (re-uses cache snapshot from previous turn)
///   - Last assistant message           → WRITE marker (creates new snapshot for the next turn)
///
/// This ensures each turn N+1 reads from turn N's conversation cache, paying only
/// cache_read_input_tokens for the already-cached history instead of full input tokens.
///
/// Budget: system (1) + tools (1) + messages (up to 2) = 4 total, within Anthropic's limit.
pub fn add_message_cache_breakpoint(messages: &mut [ApiMessage], cache_ttl_1h: bool) {
    jcode_logging::info(&format!(
        "Conversation caching: {} messages to process",
        messages.len()
    ));

    if messages.len() < 3 {
        // Need at least: user + assistant + user to be worth caching
        jcode_logging::info("Conversation caching: too few messages, skipping");
        return;
    }

    // Collect indices of up to 2 most recent assistant messages (newest first)
    let mut assistant_indices: Vec<usize> = Vec::with_capacity(2);
    for (i, msg) in messages.iter().enumerate().rev() {
        if msg.role == "assistant" {
            assistant_indices.push(i);
            if assistant_indices.len() == 2 {
                break;
            }
        }
    }

    if assistant_indices.is_empty() {
        jcode_logging::info("Conversation caching: no assistant message found");
        return;
    }

    // Place cache_control on both (newest = WRITE for next turn, older = READ from prev turn)
    let total = assistant_indices.len();
    for (slot, &idx) in assistant_indices.iter().enumerate() {
        let label = if slot == 0 {
            "WRITE (newest)"
        } else {
            "READ (prev-turn)"
        };
        let mut added = false;
        if let Some(msg) = messages.get_mut(idx) {
            for block in msg.content.iter_mut().rev() {
                match block {
                    ApiContentBlock::Text { cache_control, .. }
                    | ApiContentBlock::ToolUse { cache_control, .. } => {
                        *cache_control = Some(CacheControlParam::ephemeral(cache_ttl_1h));
                        added = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        if added {
            jcode_logging::info(&format!(
                "Conversation caching: breakpoint {}/{} at message {} [{}]",
                slot + 1,
                total,
                idx,
                label
            ));
        } else {
            jcode_logging::info(&format!(
                "Conversation caching: no cacheable block in assistant message {} [{}]",
                idx, label
            ));
        }
    }
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
    Image { source: ApiImageSource },
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
mod cache_prefix_invariant_tests {
    //! Deterministic proof that injecting a trailing memory message can never move
    //! the Anthropic prefix-cache breakpoints off the stable assistant prefix.
    //!
    //! Anthropic caching is strict-prefix: a `cache_control` breakpoint caches every
    //! token up to and including the block it sits on. `add_message_cache_breakpoint`
    //! always anchors the two breakpoints on the two most recent *assistant* messages.
    //! Memory is injected by the agent as a trailing *user* message (see
    //! `turn_loops.rs` / `turn_streaming_mpsc.rs`). Therefore the breakpoint anchors,
    //! and every token they cache, are identical with or without the memory suffix.
    //! These tests pin that invariant so a refactor cannot silently break the cache.

    use super::*;
    use jcode_message_types::{ContentBlock, Message, Role};

    fn text_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    /// A realistic warm conversation: user/assistant turns ending on a user message.
    fn base_conversation() -> Vec<Message> {
        vec![
            text_msg(Role::User, "Q1"),
            text_msg(Role::Assistant, "A1"),
            text_msg(Role::User, "Q2"),
            text_msg(Role::Assistant, "A2"),
            text_msg(Role::User, "Q3"),
        ]
    }

    /// Returns the indices of ApiMessages that carry a cache_control breakpoint,
    /// paired with the role of that message.
    fn breakpoint_anchors(messages: &[ApiMessage]) -> Vec<(usize, String)> {
        messages
            .iter()
            .enumerate()
            .filter_map(|(i, msg)| {
                let has_bp = msg.content.iter().any(|block| {
                    matches!(
                        block,
                        ApiContentBlock::Text {
                            cache_control: Some(_),
                            ..
                        } | ApiContentBlock::ToolUse {
                            cache_control: Some(_),
                            ..
                        }
                    )
                });
                has_bp.then(|| (i, msg.role.clone()))
            })
            .collect()
    }

    /// Serialize only the prefix up to and including the last breakpoint. This is the
    /// exact span Anthropic caches; if it is byte-identical across two requests, the
    /// cache is guaranteed to be reused.
    fn cached_prefix_json(messages: &[ApiMessage]) -> String {
        let last_bp = breakpoint_anchors(messages)
            .last()
            .map(|(idx, _)| *idx)
            .expect("expected at least one cache breakpoint");
        serde_json::to_string(&messages[..=last_bp]).expect("serialize cached prefix")
    }

    fn formatted_with_breakpoints(messages: &[Message]) -> Vec<ApiMessage> {
        let mut api = format_messages(messages);
        add_message_cache_breakpoint(&mut api, false);
        api
    }

    #[test]
    fn breakpoints_anchor_on_assistant_messages_only() {
        let api = formatted_with_breakpoints(&base_conversation());
        let anchors = breakpoint_anchors(&api);
        assert!(!anchors.is_empty(), "expected breakpoints to be placed");
        for (idx, role) in &anchors {
            assert_eq!(
                role, "assistant",
                "breakpoint at message {idx} must be on an assistant message, got {role}"
            );
        }
    }

    #[test]
    fn trailing_memory_message_does_not_move_breakpoints() {
        let base = base_conversation();
        let mut with_memory = base.clone();
        with_memory.push(text_msg(
            Role::User,
            "<memory>relevant recall injected for this turn</memory>",
        ));

        let base_api = formatted_with_breakpoints(&base);
        let mem_api = formatted_with_breakpoints(&with_memory);

        let base_anchors = breakpoint_anchors(&base_api);
        let mem_anchors = breakpoint_anchors(&mem_api);

        assert_eq!(
            base_anchors, mem_anchors,
            "memory suffix moved the cache breakpoints: {base_anchors:?} -> {mem_anchors:?}"
        );
    }

    #[test]
    fn cached_prefix_is_byte_identical_with_and_without_memory() {
        let base = base_conversation();
        let mut with_memory = base.clone();
        with_memory.push(text_msg(
            Role::User,
            "<memory>turn-specific recall</memory>",
        ));

        let base_prefix = cached_prefix_json(&formatted_with_breakpoints(&base));
        let mem_prefix = cached_prefix_json(&formatted_with_breakpoints(&with_memory));

        assert_eq!(
            base_prefix, mem_prefix,
            "the cached prefix span differs once memory is appended; cache would be invalidated"
        );
    }

    #[test]
    fn different_memory_each_turn_keeps_identical_cached_prefix() {
        // The memory content changes every turn. Because it is a trailing user message
        // placed *after* the newest assistant breakpoint, the cached prefix must remain
        // identical regardless of what memory is injected.
        let base = base_conversation();
        let cached = cached_prefix_json(&formatted_with_breakpoints(&base));

        for memory in [
            "<memory>recall A</memory>",
            "<memory>completely different recall B with more text</memory>",
            "",
        ] {
            let mut msgs = base.clone();
            if !memory.is_empty() {
                msgs.push(text_msg(Role::User, memory));
            }
            let candidate = cached_prefix_json(&formatted_with_breakpoints(&msgs));
            assert_eq!(
                cached, candidate,
                "memory variant {memory:?} changed the cached prefix span"
            );
        }
    }

    #[test]
    fn format_tools_removes_top_level_combinators_for_anthropic_api() {
        let tool = ToolDefinition {
            name: "custom".to_string(),
            description: "schema compatibility regression".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string"},
                    "nested_union": {
                        "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]
                    }
                },
                "required": ["action"],
                "oneOf": [
                    {"type": "object", "properties": {"label": {"type": "string"}}},
                    {"type": "object", "properties": {"task_id": {"type": "string"}}}
                ],
                "allOf": [
                    {"type": "object", "properties": {"intent": {"type": "string"}}, "required": ["intent"]}
                ]
            }),
        };

        let formatted = format_tools(&[tool], false);
        let schema = &formatted[0].input_schema;
        for keyword in ["oneOf", "anyOf", "allOf"] {
            assert!(
                schema.get(keyword).is_none(),
                "Anthropic rejects top-level {keyword}: {schema}"
            );
        }
        for property in ["action", "nested_union", "label", "task_id", "intent"] {
            assert!(
                schema["properties"].get(property).is_some(),
                "missing merged property {property}: {schema}"
            );
        }
        assert!(
            schema["properties"]["nested_union"].get("anyOf").is_some(),
            "nested combinators remain supported and should not be flattened"
        );
        assert_eq!(schema["required"], json!(["action", "intent"]));
    }
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
