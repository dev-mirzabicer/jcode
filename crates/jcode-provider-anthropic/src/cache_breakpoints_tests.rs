//! Prompt-cache breakpoint placement (INT-01 DESIGN §6, R12).
//!
//! These replace the tests of the former placement, which anchored two
//! markers on the two newest assistant messages and one on the last tool.

use super::*;
use crate::{
    ApiRequest, ApiSystemBlock, ApiTool, ApiToolChoice, build_system_param, format_messages,
};
use jcode_message_types::{AnthropicThinkingBinding, ContentBlock, Message, Role};
use serde_json::{Value, json};

fn tool(name: &str) -> ApiTool {
    ApiTool {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: json!({"type": "object", "properties": {}}),
        cache_control: None,
    }
}

fn request(system: Option<ApiSystem>, messages: &[Message]) -> ApiRequest {
    let tools = vec![tool("bash"), tool("read")];
    ApiRequest {
        model: "claude-opus-5-5".to_string(),
        max_tokens: 1024,
        system,
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

fn placed(messages: &[Message]) -> ApiRequest {
    let mut request = request(build_system_param("static prompt", true), messages);
    place_cache_breakpoints(&mut request, true);
    request
}

fn thinking(tag: &str) -> ContentBlock {
    ContentBlock::AnthropicThinking {
        thinking: format!("thinking {tag}"),
        signature: format!("sig-{tag}"),
        binding: Some(AnthropicThinkingBinding {
            model: "claude-opus-5-5".to_string(),
            prefix_digest: "anthropic-prefix-v1:test".to_string(),
            predecessor: None,
        }),
    }
}

fn tool_use(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.to_string(),
        name: "bash".to_string(),
        input: json!({"command": "true"}),
        thought_signature: None,
    }
}

/// An assistant response that thinks, then calls one tool.
fn assistant_tool_call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![thinking(id), tool_use(id)],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn user_blocks(texts: &[&str]) -> Message {
    Message {
        role: Role::User,
        content: texts
            .iter()
            .map(|text| ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            })
            .collect(),
        timestamp: None,
        tool_duration_ms: None,
    }
}

/// `(message, block)` of every marked message block.
fn marked_blocks(request: &ApiRequest) -> Vec<(usize, usize)> {
    request
        .messages
        .iter()
        .enumerate()
        .flat_map(|(message, m)| {
            m.content
                .iter()
                .enumerate()
                .filter(|(_, block)| block.has_cache_control())
                .map(move |(block, _)| (message, block))
        })
        .collect()
}

fn marker_count(request: &ApiRequest) -> usize {
    let value = serde_json::to_value(request).expect("serialize");
    count_markers(&value)
}

fn count_markers(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(count_markers).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(count_markers).sum(),
        _ => 0,
    }
}

/// The request serialized through `(message, block)`, markers stripped: the
/// exact span a breakpoint on that block caches.
fn span_through(request: &ApiRequest, message: usize, block: usize) -> String {
    let mut span = request.clone();
    span.messages.truncate(message + 1);
    span.messages[message].content.truncate(block + 1);
    let mut value = serde_json::to_value(&span).expect("serialize");
    strip_markers(&mut value);
    for field in ["model", "max_tokens", "stream"] {
        value.as_object_mut().expect("object").remove(field);
    }
    value.to_string()
}

fn strip_markers(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip_markers);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_markers),
        _ => {}
    }
}

/// A scripted agent session: each request is the previous one plus the
/// model's response and what the next request appends.
fn scripted_session() -> Vec<Vec<Message>> {
    let mut history = vec![Message::user("start the task")];
    let mut requests = vec![history.clone()];
    for step in 0..6 {
        let id = format!("call_{step}");
        history.push(assistant_tool_call(&id));
        history.push(Message::tool_result(&id, "ok", false));
        if step == 2 {
            // A mid-turn delivery beside the tool result.
            history.push(user_blocks(&[
                "<system-reminder>\nnote\n</system-reminder>",
            ]));
        }
        requests.push(history.clone());
    }
    history.push(Message::assistant_text("done"));
    history.push(Message::user("next turn"));
    requests.push(history);
    requests
}

#[test]
fn the_static_prefix_marker_sits_on_the_last_system_block() {
    let request = placed(&[Message::user("hi")]);
    let Some(ApiSystem::Blocks(blocks)) = &request.system else {
        panic!("system blocks");
    };
    let marked: Vec<bool> = blocks.iter().map(|b| b.cache_control.is_some()).collect();
    assert_eq!(
        marked,
        vec![false, false, true],
        "identity blocks, then the prompt"
    );
    assert!(
        request
            .tools
            .iter()
            .flatten()
            .all(|tool| tool.cache_control.is_none()),
        "tools render before system, so the system marker already covers them"
    );
}

#[test]
fn without_a_system_the_last_tool_carries_the_static_prefix_marker() {
    let mut request = request(None, &[Message::user("hi")]);
    place_cache_breakpoints(&mut request, false);
    let marked: Vec<&str> = request
        .tools
        .iter()
        .flatten()
        .filter(|tool| tool.cache_control.is_some())
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(marked, vec!["read"]);
}

#[test]
fn a_first_request_writes_only_its_newest_block() {
    let request = placed(&[Message::user("hi")]);
    assert_eq!(marked_blocks(&request), vec![(0, 0)]);
    assert_eq!(marker_count(&request), 2, "system and newest");
}

#[test]
fn each_request_reads_exactly_where_the_previous_one_wrote() {
    let requests: Vec<ApiRequest> = scripted_session().iter().map(|m| placed(m)).collect();
    for pair in requests.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        let written = *marked_blocks(previous).last().expect("newest marker");
        assert!(
            marked_blocks(current).contains(&written),
            "the next request marks the block the previous one wrote: {written:?} in {:?}",
            marked_blocks(current)
        );
        assert_eq!(
            span_through(previous, written.0, written.1),
            span_through(current, written.0, written.1),
            "the span the previous request cached is a byte-identical prefix of the next"
        );
        let newest = *marked_blocks(current).last().expect("newest marker");
        let last_message = current.messages.len() - 1;
        assert_eq!(
            newest.0, last_message,
            "the newest marker is in the newest message"
        );
        assert!(marker_count(current) <= 4);
    }
}

/// `user, assistant(thinking, tool_use), user(tool_result, texts...)`: the
/// thinking block is position 1, the tool call 2, the result 3.
fn thinking_then_long_result(texts: usize) -> Vec<Message> {
    let labels: Vec<String> = (0..texts).map(|i| format!("note {i}")).collect();
    let mut result = Message::tool_result("a", "ok", false);
    result
        .content
        .extend(labels.iter().map(|text| ContentBlock::Text {
            text: text.clone(),
            cache_control: None,
        }));
    vec![Message::user("question"), assistant_tool_call("a"), result]
}

#[test]
fn a_marker_never_lands_on_a_thinking_block() {
    // Newest at position 16: the intermediate target, position 1, is the
    // thinking block. Nothing markable lies between it and the previous
    // marker, so no intermediate is placed.
    let request = placed(&thinking_then_long_result(13));
    assert_eq!(marked_blocks(&request), vec![(0, 0), (2, 13)]);

    // Newest at position 17: the target, position 2, is the tool call.
    let request = placed(&thinking_then_long_result(14));
    assert_eq!(marked_blocks(&request), vec![(0, 0), (1, 1), (2, 14)]);

    for messages in scripted_session() {
        let request = placed(&messages);
        for (message, block) in marked_blocks(&request) {
            assert!(
                request.messages[message].content[block].accepts_cache_control(),
                "marker on a block that cannot take one at {message}.{block}"
            );
        }
    }
}

#[test]
fn a_long_append_gets_an_intermediate_marker_within_the_lookback() {
    let texts: Vec<String> = (0..24).map(|i| format!("file {i}")).collect();
    let many: Vec<&str> = texts.iter().map(String::as_str).collect();
    let messages = vec![
        Message::user("start"),
        Message::assistant_text("ok"),
        user_blocks(&many),
    ];
    let request = placed(&messages);
    let marked = marked_blocks(&request);
    assert_eq!(
        marked.len(),
        3,
        "previous, intermediate, newest: {marked:?}"
    );
    assert_eq!(marked[0], (0, 0));
    assert_eq!(marked[2], (2, 23));
    assert_eq!(
        marked[1],
        (2, 23 - INTERMEDIATE_AFTER_POSITIONS),
        "the intermediate sits {INTERMEDIATE_AFTER_POSITIONS} positions before the newest"
    );
    assert_eq!(marker_count(&request), 4, "the API maximum");
}

#[test]
fn parallel_tool_runs_count_as_one_lookback_position() {
    let ids: Vec<String> = (0..30).map(|i| format!("p{i}")).collect();
    let messages = vec![
        Message::user("fan out"),
        Message {
            role: Role::Assistant,
            content: ids.iter().map(|id| tool_use(id)).collect(),
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: "ok".to_string(),
                    is_error: None,
                })
                .collect(),
            timestamp: None,
            tool_duration_ms: None,
        },
    ];
    let request = placed(&messages);
    assert_eq!(
        marked_blocks(&request),
        vec![(0, 0), (2, 29)],
        "two run positions after the previous marker need no intermediate"
    );
}

#[test]
fn placement_replaces_existing_markers_and_is_idempotent() {
    let messages = scripted_session().pop().expect("requests");
    let mut request = request(build_system_param("static prompt", false), &messages);
    if let Some(tool) = request.tools.as_mut().and_then(|tools| tools.first_mut()) {
        tool.cache_control = Some(CacheControlParam::ephemeral(false));
    }
    if let Some(ApiContentBlock::Text { cache_control, .. }) =
        request.messages[0].content.first_mut()
    {
        *cache_control = Some(CacheControlParam::ephemeral(false));
    }
    place_cache_breakpoints(&mut request, true);
    let once = serde_json::to_string(&request).expect("serialize");
    place_cache_breakpoints(&mut request, true);
    assert_eq!(once, serde_json::to_string(&request).expect("serialize"));
    assert!(
        request
            .tools
            .iter()
            .flatten()
            .all(|t| t.cache_control.is_none())
    );
    assert!(!marked_blocks(&request).contains(&(0, 0)));
}

#[test]
fn every_marker_carries_the_same_ttl() {
    let messages = scripted_session().pop().expect("requests");
    for (ttl_1h, expected) in [(true, Some("1h")), (false, None)] {
        let mut request = request(build_system_param("static prompt", true), &messages);
        place_cache_breakpoints(&mut request, ttl_1h);
        let value = serde_json::to_value(&request).expect("serialize");
        let mut ttls = Vec::new();
        collect_ttls(&value, &mut ttls);
        assert!(!ttls.is_empty());
        assert!(
            ttls.iter().all(|ttl| ttl.as_deref() == expected),
            "ttl {expected:?}: {ttls:?}"
        );
    }
}

fn collect_ttls(value: &Value, out: &mut Vec<Option<String>>) {
    match value {
        Value::Object(map) => {
            if let Some(marker) = map.get("cache_control") {
                out.push(marker.get("ttl").and_then(Value::as_str).map(String::from));
            }
            map.values().for_each(|v| collect_ttls(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_ttls(v, out)),
        _ => {}
    }
}

#[test]
fn a_system_block_marker_is_the_only_one_on_system() {
    let request = placed(&[Message::user("hi")]);
    let blocks = match &request.system {
        Some(ApiSystem::Blocks(blocks)) => blocks.clone(),
        None => Vec::<ApiSystemBlock>::new(),
    };
    assert_eq!(
        blocks.iter().filter(|b| b.cache_control.is_some()).count(),
        1
    );
}
