//! Prefix binding of Anthropic signed reasoning (INT-01 DESIGN §4.2).
//!
//! Anthropic binds each thinking block's signature to the conversation that
//! produced it. Per Anthropic's preserved-thinking documentation (re-verified
//! 2026-09-29), the bound prefix is:
//!
//! - the top-level `system` prompt,
//! - the tool set, bound as a name-sorted set,
//! - every message before the block,
//!
//! plus a chain to the previous thinking block. Earlier thinking blocks are not
//! part of the prefix. That is why a leading run of thinking blocks may be
//! removed while a block in the middle may not. Changing request parameters
//! outside `system`/`tools`/`messages`, or adding, moving and removing
//! `cache_control` markers, does not invalidate a block.
//!
//! jcode records, for every block it captures, a digest of that prefix as the
//! runtime sent it and the fingerprint of the thinking block that preceded it.
//! [`analyze_request`] then decides from the request alone whether
//! each replayed block would still be accepted, without asking the provider.
//! The digest is taken over the formatted wire request, so it sees exactly what
//! the provider sees.

use crate::{ApiContentBlock, ApiMessage, ApiRequest, ApiSystem, ApiTool};
use jcode_message_types::AnthropicThinkingBinding;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Scheme label carried in every recorded digest. A digest produced by any
/// other scheme never matches, so a future change to the canonical form fails
/// closed instead of comparing incompatible values.
pub const PREFIX_DIGEST_SCHEME: &str = "anthropic-prefix-v1";

const SECTION_SYSTEM: u8 = b'S';
const SECTION_TOOL: u8 = b'T';
const SECTION_MESSAGE: u8 = b'M';

/// Where a newly produced block sits in the conversation the runtime sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestBinding {
    /// Digest of the complete request prefix: the new assistant turn follows
    /// every message in the request.
    pub prefix_digest: String,
    /// Fingerprint of the last thinking block the request carried.
    pub last_thinking: Option<String>,
}

/// Fingerprint of a thinking block's opaque payload: the signature of a
/// `thinking` block, or the data of a `redacted_thinking` block.
pub fn thinking_fingerprint(kind: ThinkingPayload, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(match kind {
        ThinkingPayload::Signature => b"thinking\0".as_slice(),
        ThinkingPayload::RedactedData => b"redacted_thinking\0".as_slice(),
    });
    hasher.update(payload.as_bytes());
    hex::encode(hasher.finalize())
}

/// Which opaque payload a fingerprint was taken over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingPayload {
    Signature,
    RedactedData,
}

fn thinking_block_fingerprint(block: &ApiContentBlock) -> Option<String> {
    match block {
        ApiContentBlock::Thinking { signature, .. } => {
            Some(thinking_fingerprint(ThinkingPayload::Signature, signature))
        }
        ApiContentBlock::RedactedThinking { data, .. } => {
            Some(thinking_fingerprint(ThinkingPayload::RedactedData, data))
        }
        _ => None,
    }
}

fn thinking_block_binding(block: &ApiContentBlock) -> Option<Option<&AnthropicThinkingBinding>> {
    match block {
        ApiContentBlock::Thinking { binding, .. } => Some(binding.as_ref()),
        ApiContentBlock::RedactedThinking { binding, .. } => Some(Some(binding)),
        _ => None,
    }
}

/// Rolling digest over `system`, the name-sorted tools, and then one message
/// at a time, so the prefix before every message can be read in one pass.
pub struct PrefixDigester {
    hasher: Sha256,
}

impl PrefixDigester {
    pub fn new(system: Option<&ApiSystem>, tools: Option<&[ApiTool]>) -> Self {
        let mut digester = Self {
            hasher: Sha256::new(),
        };
        digester.hasher.update(PREFIX_DIGEST_SCHEME.as_bytes());
        digester.hasher.update(b"\n");

        let system_blocks = system
            .map(|system| match system {
                ApiSystem::Blocks(blocks) => blocks
                    .iter()
                    .map(|block| without_cache_control(to_value(block)))
                    .collect::<Vec<_>>(),
            })
            .unwrap_or_default();
        for block in &system_blocks {
            digester.push_section(SECTION_SYSTEM, block);
        }

        let mut tools: Vec<Value> = tools
            .unwrap_or_default()
            .iter()
            .map(|tool| without_cache_control(to_value(tool)))
            .collect();
        tools.sort_by(|left, right| tool_name(left).cmp(tool_name(right)));
        for tool in &tools {
            digester.push_section(SECTION_TOOL, tool);
        }
        digester
    }

    /// Add one message: its role and every non-thinking block, without cache
    /// markers.
    pub fn push_message(&mut self, message: &ApiMessage) {
        let content: Vec<Value> = message
            .content
            .iter()
            .filter(|block| thinking_block_fingerprint(block).is_none())
            .map(|block| without_cache_control(to_value(block)))
            .collect();
        let value = serde_json::json!({ "role": message.role, "content": content });
        self.push_section(SECTION_MESSAGE, &value);
    }

    /// The digest of everything pushed so far.
    pub fn digest(&self) -> String {
        format!(
            "{PREFIX_DIGEST_SCHEME}:{}",
            hex::encode(self.hasher.clone().finalize())
        )
    }

    fn push_section(&mut self, tag: u8, value: &Value) {
        let mut bytes = Vec::new();
        write_canonical(value, &mut bytes);
        self.hasher.update([tag]);
        self.hasher.update((bytes.len() as u64).to_le_bytes());
        self.hasher.update(&bytes);
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or_default()
}

/// Remove the top-level `cache_control` marker of a system block, tool or
/// content block. Only that level carries markers; nested schema properties
/// or tool input with the same key are content and stay.
fn without_cache_control(mut value: Value) -> Value {
    if let Value::Object(map) = &mut value {
        map.remove("cache_control");
    }
    value
}

/// Serialize a JSON value with object keys sorted, so the digest does not
/// depend on map iteration or field order.
fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_scalar(&Value::String(key.clone()), out);
                out.push(b':');
                write_canonical(&map[key], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        scalar => write_scalar(scalar, out),
    }
}

fn write_scalar(value: &Value, out: &mut Vec<u8>) {
    // serde_json's scalar encoding (string escaping, number formatting) is
    // deterministic for a given value.
    out.extend_from_slice(value.to_string().as_bytes());
}

/// Whether a replayed thinking block would still be accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayValidity {
    Valid,
    /// Something in `system`, the tool set or an earlier message changed
    /// since the block was produced.
    PrefixChanged,
    /// A thinking block between this one and its recorded predecessor is no
    /// longer replayed (only a leading run may be removed).
    ChainBroken,
    /// An earlier replayed block is invalid. Anthropic drops the first
    /// mismatched block and every thinking block after it.
    FollowsInvalid,
}

impl ReplayValidity {
    /// Stable label for logs and diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::PrefixChanged => "prefix_changed",
            Self::ChainBroken => "chain_broken",
            Self::FollowsInvalid => "follows_invalid",
        }
    }
}

/// One replayed thinking block and its validity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedThinking {
    /// Wire path of the block, in the form Anthropic uses in
    /// `input_transformations` (`messages.<i>.content.<j>`).
    pub path: String,
    /// The model that produced the block.
    pub model: String,
    pub validity: ReplayValidity,
}

/// What a request's thinking bindings say: the binding for the blocks its
/// response will produce, and the validity of every block it replays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestBindingReport {
    pub binding: RequestBinding,
    /// Every replayed thinking block, in request order.
    pub replayed: Vec<ReplayedThinking>,
}

impl RequestBindingReport {
    /// Replayed blocks that would not be accepted.
    pub fn invalid(&self) -> impl Iterator<Item = &ReplayedThinking> {
        self.replayed
            .iter()
            .filter(|block| block.validity != ReplayValidity::Valid)
    }
}

/// Bind a request in one pass over its prefix.
///
/// A replayed block is valid when the digest of the request prefix before its
/// assistant message equals the digest recorded when it was produced, and
/// either it is the first thinking block replayed or the thinking block
/// replayed before it is the one it recorded as its predecessor. Once a block
/// is invalid, every later block is too.
pub fn analyze_request(request: &ApiRequest) -> RequestBindingReport {
    let mut digester = PrefixDigester::new(request.system.as_ref(), request.tools.as_deref());
    let mut previous: Option<String> = None;
    let mut invalid_seen = false;
    let mut replayed = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
        let mut prefix: Option<String> = None;
        for (block_index, block) in message.content.iter().enumerate() {
            let Some(binding) = thinking_block_binding(block) else {
                continue;
            };
            let prefix = prefix.get_or_insert_with(|| digester.digest());
            let validity = match binding {
                _ if invalid_seen => ReplayValidity::FollowsInvalid,
                // The formatter never replays an unbound block; one that
                // reaches a request cannot be shown to match its prefix.
                None => ReplayValidity::PrefixChanged,
                Some(binding) if binding.prefix_digest != *prefix => ReplayValidity::PrefixChanged,
                Some(binding) if previous.is_some() && binding.predecessor != previous => {
                    ReplayValidity::ChainBroken
                }
                Some(_) => ReplayValidity::Valid,
            };
            invalid_seen |= validity != ReplayValidity::Valid;
            replayed.push(ReplayedThinking {
                path: format!("messages.{message_index}.content.{block_index}"),
                model: binding.map(|b| b.model.clone()).unwrap_or_default(),
                validity,
            });
            previous = thinking_block_fingerprint(block);
        }
        digester.push_message(message);
    }
    RequestBindingReport {
        binding: RequestBinding {
            prefix_digest: digester.digest(),
            last_thinking: previous,
        },
        replayed,
    }
}

/// Fingerprint of a stored replayable thinking block, equal to the one
/// [`analyze_request`] uses for the same block on the wire. `None` for every
/// other block, including thinking stored without a binding record, which the
/// formatter never replays.
pub fn stored_thinking_fingerprint(block: &jcode_message_types::ContentBlock) -> Option<String> {
    match block {
        jcode_message_types::ContentBlock::AnthropicThinking {
            signature,
            binding: Some(_),
            ..
        } => Some(thinking_fingerprint(ThinkingPayload::Signature, signature)),
        jcode_message_types::ContentBlock::AnthropicRedactedThinking { data, .. } => {
            Some(thinking_fingerprint(ThinkingPayload::RedactedData, data))
        }
        _ => None,
    }
}

/// One replayed thinking block that must be suppressed before the request is
/// sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressedReplay {
    /// [`thinking_fingerprint`] of the block.
    pub fingerprint: String,
    /// Wire path, as in [`ReplayedThinking::path`].
    pub path: String,
    /// `PrefixChanged` or `ChainBroken`; never `Valid` or `FollowsInvalid`.
    pub validity: ReplayValidity,
}

/// The replayed thinking blocks jcode must suppress so that every block it
/// still sends is valid.
///
/// This is [`analyze_request`]'s rule applied as if each invalid block were
/// already removed. A block is kept when the prefix before it matches its
/// recorded digest and it chains to the last kept block (or no block was kept
/// before it: removing a leading run is allowed). Removing a block never
/// changes a later block's prefix digest, because thinking is not part of the
/// prefix, so one pass decides every block.
pub fn blocks_to_suppress(request: &ApiRequest) -> Vec<SuppressedReplay> {
    let mut digester = PrefixDigester::new(request.system.as_ref(), request.tools.as_deref());
    let mut last_kept: Option<String> = None;
    let mut suppressed = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
        let mut prefix: Option<String> = None;
        for (block_index, block) in message.content.iter().enumerate() {
            let Some(binding) = thinking_block_binding(block) else {
                continue;
            };
            let Some(fingerprint) = thinking_block_fingerprint(block) else {
                continue;
            };
            let prefix = prefix.get_or_insert_with(|| digester.digest());
            let validity = match binding {
                None => ReplayValidity::PrefixChanged,
                Some(binding) if binding.prefix_digest != *prefix => ReplayValidity::PrefixChanged,
                Some(binding) if last_kept.is_some() && binding.predecessor != last_kept => {
                    ReplayValidity::ChainBroken
                }
                Some(_) => ReplayValidity::Valid,
            };
            if validity == ReplayValidity::Valid {
                last_kept = Some(fingerprint);
            } else {
                suppressed.push(SuppressedReplay {
                    fingerprint,
                    path: format!("messages.{message_index}.content.{block_index}"),
                    validity,
                });
            }
        }
        digester.push_message(message);
    }
    suppressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiSystemBlock, ApiToolChoice, CacheControlParam, ToolResultContent, format_messages,
    };
    use jcode_message_types::{ContentBlock, Message, Role};
    use serde_json::json;

    fn tool(name: &str) -> ApiTool {
        ApiTool {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
            cache_control: None,
        }
    }

    fn system(text: &str) -> ApiSystem {
        ApiSystem::Blocks(vec![ApiSystemBlock {
            block_type: "text",
            text: text.to_string(),
            cache_control: None,
        }])
    }

    fn request(system_text: &str, tools: Vec<ApiTool>, messages: &[Message]) -> ApiRequest {
        ApiRequest {
            model: "claude-opus-5-5".to_string(),
            max_tokens: 1024,
            system: Some(system(system_text)),
            messages: format_messages(messages),
            tool_choice: ApiToolChoice::for_tools(&tools),
            tools: Some(tools),
            metadata: None,
            thinking: None,
            output_config: None,
            temperature: None,
            service_tier: None,
            stream: true,
        }
    }

    fn message(role: Role, content: Vec<ContentBlock>) -> Message {
        Message {
            role,
            content,
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    /// The assistant turn a response to `request` produced, with one signed
    /// thinking block per signature, bound the way the runtime binds them.
    fn produced_turn(request: &ApiRequest, signatures: &[&str], tool_id: &str) -> Message {
        let binding = analyze_request(request).binding;
        let mut predecessor = binding.last_thinking.clone();
        let mut content = Vec::new();
        for signature in signatures {
            content.push(ContentBlock::AnthropicThinking {
                thinking: format!("thought {signature}"),
                signature: signature.to_string(),
                binding: Some(AnthropicThinkingBinding {
                    model: "claude-opus-5-5".to_string(),
                    prefix_digest: binding.prefix_digest.clone(),
                    predecessor: predecessor.clone(),
                }),
            });
            predecessor = Some(thinking_fingerprint(ThinkingPayload::Signature, signature));
        }
        content.push(ContentBlock::ToolUse {
            id: tool_id.to_string(),
            name: "bash".to_string(),
            input: json!({"command": "true"}),
            thought_signature: None,
        });
        message(Role::Assistant, content)
    }

    fn tool_result(tool_id: &str) -> Message {
        Message::tool_result(tool_id, "ok", false)
    }

    /// A three-request conversation: user, then two tool rounds, each
    /// producing thinking bound to the request that produced it.
    fn conversation() -> Vec<Message> {
        let tools = vec![tool("bash"), tool("read")];
        let mut messages = vec![Message::user("start")];
        let first = produced_turn(&request("sys", tools.clone(), &messages), &["sig-a"], "t1");
        messages.push(first);
        messages.push(tool_result("t1"));
        let second = produced_turn(
            &request("sys", tools.clone(), &messages),
            &["sig-b", "sig-c"],
            "t2",
        );
        messages.push(second);
        messages.push(tool_result("t2"));
        messages
    }

    fn validities(report: &RequestBindingReport) -> Vec<ReplayValidity> {
        report.replayed.iter().map(|block| block.validity).collect()
    }

    #[test]
    fn identical_requests_produce_identical_digests_under_the_scheme_label() {
        let messages = [Message::user("hello")];
        let first = analyze_request(&request("sys", vec![tool("bash")], &messages));
        let second = analyze_request(&request("sys", vec![tool("bash")], &messages));
        assert_eq!(first, second);
        assert!(
            first
                .binding
                .prefix_digest
                .starts_with("anthropic-prefix-v1:")
        );
    }

    #[test]
    fn cache_markers_are_excluded_from_the_digest() {
        let messages = conversation();
        let plain = request("sys", vec![tool("bash"), tool("read")], &messages);
        let mut marked = plain.clone();
        if let Some(ApiSystem::Blocks(blocks)) = marked.system.as_mut() {
            blocks[0].cache_control = Some(CacheControlParam::ephemeral(true));
        }
        if let Some(tools) = marked.tools.as_mut() {
            tools[1].cache_control = Some(CacheControlParam::ephemeral(false));
        }
        crate::add_message_cache_breakpoint(&mut marked.messages, true);
        assert_ne!(
            serde_json::to_string(&plain).unwrap(),
            serde_json::to_string(&marked).unwrap(),
            "the fixture must actually carry markers"
        );
        assert_eq!(analyze_request(&plain), analyze_request(&marked));
    }

    #[test]
    fn tools_are_bound_as_a_name_sorted_set() {
        let messages = [Message::user("hello")];
        let ordered = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        let reordered =
            analyze_request(&request("sys", vec![tool("read"), tool("bash")], &messages));
        assert_eq!(ordered.binding, reordered.binding);

        let mut changed_tool = tool("read");
        changed_tool.description.push_str(" (changed)");
        let changed = analyze_request(&request("sys", vec![tool("bash"), changed_tool], &messages));
        assert_ne!(ordered.binding.prefix_digest, changed.binding.prefix_digest);
        let added = analyze_request(&request(
            "sys",
            vec![tool("bash"), tool("read"), tool("grep")],
            &messages,
        ));
        assert_ne!(ordered.binding.prefix_digest, added.binding.prefix_digest);
    }

    #[test]
    fn object_key_order_does_not_change_the_canonical_form() {
        let mut first = Vec::new();
        let mut second = Vec::new();
        write_canonical(
            &json!({"b": 1, "a": {"y": [true, null], "x": "é\n"}}),
            &mut first,
        );
        let mut map = serde_json::Map::new();
        map.insert("a".to_string(), json!({"x": "é\n", "y": [true, null]}));
        map.insert("b".to_string(), json!(1));
        write_canonical(&Value::Object(map), &mut second);
        assert_eq!(first, second);
        assert_eq!(
            String::from_utf8(first).unwrap(),
            r#"{"a":{"x":"é\n","y":[true,null]},"b":1}"#
        );
    }

    #[test]
    fn nested_cache_control_keys_are_content_and_stay_bound() {
        let messages = [Message::user("hello")];
        let base = tool("bash");
        let mut nested = tool("bash");
        nested.input_schema["properties"]["cache_control"] = json!({"type": "string"});
        assert_ne!(
            analyze_request(&request("sys", vec![base], &messages))
                .binding
                .prefix_digest,
            analyze_request(&request("sys", vec![nested], &messages))
                .binding
                .prefix_digest,
        );
    }

    #[test]
    fn unchanged_append_only_history_replays_every_block_as_valid() {
        let messages = conversation();
        let report = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        assert_eq!(
            validities(&report),
            vec![ReplayValidity::Valid; 3],
            "{:?}",
            report.replayed
        );
        assert_eq!(report.replayed[0].path, "messages.1.content.0");
        assert_eq!(report.replayed[2].path, "messages.3.content.1");
        assert_eq!(
            report.binding.last_thinking,
            Some(thinking_fingerprint(ThinkingPayload::Signature, "sig-c"))
        );
    }

    #[test]
    fn an_edited_earlier_message_invalidates_every_later_block() {
        let mut messages = conversation();
        messages[0] = Message::user("start (edited)");
        let report = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        assert_eq!(
            validities(&report),
            vec![
                ReplayValidity::PrefixChanged,
                ReplayValidity::FollowsInvalid,
                ReplayValidity::FollowsInvalid,
            ]
        );
    }

    #[test]
    fn a_changed_system_prompt_or_tool_set_invalidates_replayed_blocks() {
        let messages = conversation();
        let system_changed = analyze_request(&request(
            "sys + reminder",
            vec![tool("bash"), tool("read")],
            &messages,
        ));
        assert_eq!(
            system_changed.replayed[0].validity,
            ReplayValidity::PrefixChanged
        );
        let tools_changed = analyze_request(&request("sys", vec![tool("bash")], &messages));
        assert_eq!(
            tools_changed.replayed[0].validity,
            ReplayValidity::PrefixChanged
        );
    }

    #[test]
    fn a_change_after_a_block_leaves_it_valid_and_invalidates_only_later_blocks() {
        let mut messages = conversation();
        // Edit the tool result that follows the first turn: the first block's
        // prefix is untouched, the second turn's prefix is not.
        messages[2] = Message::tool_result("t1", "ok (edited)", false);
        let report = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        assert_eq!(
            validities(&report),
            vec![
                ReplayValidity::Valid,
                ReplayValidity::PrefixChanged,
                ReplayValidity::FollowsInvalid,
            ]
        );
    }

    #[test]
    fn removing_a_leading_run_of_thinking_keeps_later_blocks_valid() {
        let mut messages = conversation();
        messages[1]
            .content
            .retain(|block| !matches!(block, ContentBlock::AnthropicThinking { .. }));
        let report = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        assert_eq!(validities(&report), vec![ReplayValidity::Valid; 2]);
    }

    #[test]
    fn removing_a_thinking_block_from_the_middle_breaks_the_chain() {
        let mut messages = conversation();
        // Keep sig-a and sig-c, drop sig-b between them.
        messages[3].content.retain(|block| {
            !matches!(block, ContentBlock::AnthropicThinking { signature, .. } if signature == "sig-b")
        });
        let report = analyze_request(&request("sys", vec![tool("bash"), tool("read")], &messages));
        assert_eq!(
            validities(&report),
            vec![ReplayValidity::Valid, ReplayValidity::ChainBroken]
        );
    }

    #[test]
    fn redacted_blocks_chain_by_their_data() {
        let tools = vec![tool("bash")];
        let mut messages = vec![Message::user("start")];
        let first = request("sys", tools.clone(), &messages);
        let binding = analyze_request(&first).binding;
        messages.push(message(
            Role::Assistant,
            vec![
                ContentBlock::AnthropicRedactedThinking {
                    data: "opaque".to_string(),
                    binding: AnthropicThinkingBinding {
                        model: "claude-opus-5-5".to_string(),
                        prefix_digest: binding.prefix_digest.clone(),
                        predecessor: None,
                    },
                },
                ContentBlock::AnthropicThinking {
                    thinking: String::new(),
                    signature: "after-redacted".to_string(),
                    binding: Some(AnthropicThinkingBinding {
                        model: "claude-opus-5-5".to_string(),
                        prefix_digest: binding.prefix_digest,
                        predecessor: Some(thinking_fingerprint(
                            ThinkingPayload::RedactedData,
                            "opaque",
                        )),
                    }),
                },
                ContentBlock::Text {
                    text: "done".to_string(),
                    cache_control: None,
                },
            ],
        ));
        messages.push(Message::user("next"));
        let report = analyze_request(&request("sys", tools, &messages));
        assert_eq!(validities(&report), vec![ReplayValidity::Valid; 2]);
        assert_ne!(
            thinking_fingerprint(ThinkingPayload::RedactedData, "x"),
            thinking_fingerprint(ThinkingPayload::Signature, "x"),
        );
    }

    #[test]
    fn thinking_blocks_are_not_part_of_the_bound_prefix() {
        let messages = conversation();
        let mut stripped = messages.clone();
        for message in &mut stripped {
            message.content.retain(|block| {
                !matches!(
                    block,
                    ContentBlock::AnthropicThinking { .. }
                        | ContentBlock::AnthropicRedactedThinking { .. }
                )
            });
        }
        let tools = vec![tool("bash"), tool("read")];
        assert_eq!(
            analyze_request(&request("sys", tools.clone(), &messages))
                .binding
                .prefix_digest,
            analyze_request(&request("sys", tools, &stripped))
                .binding
                .prefix_digest,
        );
    }

    fn suppressed_signatures(request: &ApiRequest, messages: &[Message]) -> Vec<String> {
        let by_fingerprint: std::collections::HashMap<String, String> = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::AnthropicThinking { signature, .. } => {
                    stored_thinking_fingerprint(block).map(|f| (f, signature.clone()))
                }
                _ => None,
            })
            .collect();
        blocks_to_suppress(request)
            .into_iter()
            .map(|block| by_fingerprint[&block.fingerprint].clone())
            .collect()
    }

    /// Suppress the planned blocks from the stored history and re-analyze:
    /// every block still replayed must be valid.
    fn assert_plan_leaves_only_valid_blocks(
        system: &str,
        tools: Vec<ApiTool>,
        messages: &[Message],
    ) {
        let plan = blocks_to_suppress(&request(system, tools.clone(), messages));
        let suppressed: std::collections::HashSet<String> =
            plan.iter().map(|block| block.fingerprint.clone()).collect();
        let mut kept = messages.to_vec();
        for message in &mut kept {
            message.content.retain(|block| {
                stored_thinking_fingerprint(block)
                    .is_none_or(|fingerprint| !suppressed.contains(&fingerprint))
            });
        }
        let report = analyze_request(&request(system, tools, &kept));
        assert!(
            report.invalid().next().is_none(),
            "plan {plan:?} left {:?}",
            report.replayed
        );
    }

    #[test]
    fn an_unchanged_history_suppresses_nothing() {
        let messages = conversation();
        let tools = vec![tool("bash"), tool("read")];
        assert!(blocks_to_suppress(&request("sys", tools, &messages)).is_empty());
    }

    #[test]
    fn an_edit_suppresses_the_blocks_after_it_and_keeps_the_ones_before() {
        let tools = vec![tool("bash"), tool("read")];
        let mut messages = conversation();
        messages[2] = Message::tool_result("t1", "ok (summarized)", false);
        let request = request("sys", tools.clone(), &messages);
        assert_eq!(
            suppressed_signatures(&request, &messages),
            vec!["sig-b".to_string(), "sig-c".to_string()]
        );
        let plan = blocks_to_suppress(&request);
        assert_eq!(plan[0].validity, ReplayValidity::PrefixChanged);
        assert_eq!(plan[0].path, "messages.3.content.0");
        assert_eq!(plan[1].validity, ReplayValidity::PrefixChanged);
        assert_plan_leaves_only_valid_blocks("sys", tools, &messages);
    }

    #[test]
    fn a_suppressed_middle_block_breaks_the_chain_of_the_blocks_after_it() {
        let tools = vec![tool("bash"), tool("read")];
        let mut messages = conversation();
        messages[3].content.retain(|block| {
            !matches!(block, ContentBlock::AnthropicThinking { signature, .. } if signature == "sig-b")
        });
        let request = request("sys", tools.clone(), &messages);
        assert_eq!(
            suppressed_signatures(&request, &messages),
            vec!["sig-c".to_string()]
        );
        assert_eq!(
            blocks_to_suppress(&request)[0].validity,
            ReplayValidity::ChainBroken
        );
        assert_plan_leaves_only_valid_blocks("sys", tools, &messages);
    }

    #[test]
    fn a_suppressed_leading_run_keeps_the_later_blocks() {
        let tools = vec![tool("bash"), tool("read")];
        let mut messages = conversation();
        messages[1]
            .content
            .retain(|block| !matches!(block, ContentBlock::AnthropicThinking { .. }));
        assert!(blocks_to_suppress(&request("sys", tools, &messages)).is_empty());
    }

    #[test]
    fn a_changed_system_prompt_or_tool_set_suppresses_every_block() {
        let messages = conversation();
        let system_changed = request("sys (skill)", vec![tool("bash"), tool("read")], &messages);
        assert_eq!(
            suppressed_signatures(&system_changed, &messages),
            vec!["sig-a", "sig-b", "sig-c"]
        );
        let tools_changed = request("sys", vec![tool("bash")], &messages);
        assert_eq!(blocks_to_suppress(&tools_changed).len(), 3);
        assert_plan_leaves_only_valid_blocks(
            "sys (skill)",
            vec![tool("bash"), tool("read")],
            &messages,
        );
    }

    #[test]
    fn an_invalid_first_block_is_suppressed_and_a_valid_later_block_is_kept() {
        // The first turn's block no longer matches (an edit inside the first
        // user message would do it, but so does a block recorded under another
        // digest); suppressing it leaves the next turn's block first, and its
        // own prefix still matches.
        let tools = vec![tool("bash"), tool("read")];
        let mut messages = conversation();
        if let ContentBlock::AnthropicThinking {
            binding: Some(binding),
            ..
        } = &mut messages[1].content[0]
        {
            binding.prefix_digest = format!("{PREFIX_DIGEST_SCHEME}:other");
        }
        let request = request("sys", tools.clone(), &messages);
        assert_eq!(suppressed_signatures(&request, &messages), vec!["sig-a"]);
        assert_plan_leaves_only_valid_blocks("sys", tools, &messages);
    }

    #[test]
    fn stored_and_wire_fingerprints_agree_and_skip_unbound_thinking() {
        let messages = conversation();
        let request = request("sys", vec![tool("bash"), tool("read")], &messages);
        let wire: Vec<String> = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(thinking_block_fingerprint)
            .collect();
        let stored: Vec<String> = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(stored_thinking_fingerprint)
            .collect();
        assert_eq!(wire, stored);
        assert_eq!(
            stored_thinking_fingerprint(&ContentBlock::AnthropicThinking {
                thinking: "old".to_string(),
                signature: "sig".to_string(),
                binding: None,
            }),
            None
        );
    }

    #[test]
    fn tool_result_content_is_bound() {
        let messages = conversation();
        let mut request = request("sys", vec![tool("bash"), tool("read")], &messages);
        let before = analyze_request(&request).binding.prefix_digest;
        if let Some(ApiContentBlock::ToolResult { content, .. }) =
            request.messages[2].content.first_mut()
        {
            *content = ToolResultContent::Text("different".to_string());
        }
        assert_ne!(before, analyze_request(&request).binding.prefix_digest);
    }
}
