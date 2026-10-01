//! Operator notices in the Anthropic formatter (INT-01/WP-06, D17).

use super::*;
use jcode_message_types::ToolSetChange;
use serde_json::{Value, json};

const BOTH: AnthropicConversationCaps = AnthropicConversationCaps {
    system_messages: true,
    inline_tool_changes: true,
};
const SYSTEM_ONLY: AnthropicConversationCaps = AnthropicConversationCaps {
    system_messages: true,
    inline_tool_changes: false,
};

fn notice(body: &str) -> Message {
    notice_with(body, Vec::new())
}

fn notice_with(body: &str, tool_changes: Vec<ToolSetChange>) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::OperatorNotice {
            text: format!("<system-reminder>\n{body}\n</system-reminder>"),
            body: body.to_string(),
            tool_changes,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

/// The delivery exactly as stored (and as a provider without an operator
/// channel receives it).
fn stored(body: &str) -> Message {
    Message::user(&format!("<system-reminder>\n{body}\n</system-reminder>"))
}

fn wire(messages: &[Message], caps: AnthropicConversationCaps) -> Value {
    serde_json::to_value(format_messages_for(messages, caps)).unwrap()
}

fn roles(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect()
}

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
    }
}

#[test]
fn a_notice_after_user_content_is_a_system_message_with_its_body() {
    let messages = [Message::user("do it"), notice("note")];
    let value = wire(&messages, BOTH);
    assert_eq!(roles(&value), vec!["user", "system"]);
    assert_eq!(
        value[1]["content"],
        json!([{"type": "text", "text": "note"}])
    );
    // Followed by the reply, it stays one.
    let messages = [
        Message::user("do it"),
        notice("note"),
        Message::assistant_text("done"),
        Message::user("next"),
    ];
    assert_eq!(
        roles(&wire(&messages, BOTH)),
        vec!["user", "system", "assistant", "user"]
    );
}

#[test]
fn without_an_operator_channel_the_bytes_are_the_stored_delivery() {
    let with_notice = [
        Message::user("do it"),
        notice("note"),
        Message::assistant_text("ok"),
    ];
    let stored_form = [
        Message::user("do it"),
        stored("note"),
        Message::assistant_text("ok"),
    ];
    assert_eq!(
        wire(&with_notice, AnthropicConversationCaps::default()),
        wire(&stored_form, BOTH),
        "a model without system messages, and every stored delivery, render as today"
    );
    assert_eq!(
        serde_json::to_value(format_messages(&with_notice)).unwrap(),
        wire(&stored_form, AnthropicConversationCaps::default())
    );
}

#[test]
fn placement_rules_decide_the_form() {
    // After an assistant message (a reload resume): user text.
    let resume = [
        Message::user("task"),
        Message::assistant_text("working"),
        notice("resume"),
    ];
    let value = wire(&resume, BOTH);
    assert_eq!(roles(&value), vec!["user", "assistant", "user"]);
    assert_eq!(
        value[2]["content"][0]["text"],
        "<system-reminder>\nresume\n</system-reminder>"
    );
    // First in the conversation: user text.
    assert_eq!(roles(&wire(&[notice("first")], BOTH)), vec!["user"]);
    // Followed by user content: user text, merged with it.
    let followed = [Message::user("a"), notice("n"), Message::user("b")];
    let value = wire(&followed, BOTH);
    assert_eq!(roles(&value), vec!["user"]);
    assert_eq!(value[0]["content"].as_array().unwrap().len(), 3);
    // Two notices in a row after user content: one system message.
    let two = [Message::user("a"), notice("one"), notice("two")];
    let value = wire(&two, BOTH);
    assert_eq!(roles(&value), vec!["user", "system"]);
    assert_eq!(
        value[1]["content"],
        json!([{"type": "text", "text": "one"}, {"type": "text", "text": "two"}])
    );
}

#[test]
fn a_rendering_flips_only_for_an_unanswered_notice_and_only_after_the_last_reply() {
    // The request after a delivery failed, then a new prompt arrived: the
    // notice was last, with no reply, and now precedes user content.
    let mut history = vec![
        Message::user("first"),
        Message::assistant_text("reply"),
        Message::user("second"),
        notice("note"),
    ];
    let failed = wire(&history, BOTH);
    history.push(Message::user("third"));
    let next = wire(&history, BOTH);
    assert_eq!(roles(&failed), vec!["user", "assistant", "user", "system"]);
    assert_eq!(roles(&next), vec!["user", "assistant", "user"]);
    // Everything up to and including the last reply is unchanged, so no
    // thinking produced before the notice is affected.
    assert_eq!(failed[0], next[0]);
    assert_eq!(failed[1], next[1]);
}

#[test]
fn tool_changes_ride_in_the_system_message_by_value_and_by_name() {
    let changes = vec![
        ToolSetChange::Added {
            definition: tool("probe_echo", "Echo text"),
        },
        ToolSetChange::Redefined {
            definition: tool("bash", "Run a command (revised)"),
        },
        ToolSetChange::Removed {
            name: "read".to_string(),
        },
    ];
    let messages = [
        Message::user("go"),
        notice_with("tools changed", changes.clone()),
    ];
    let value = wire(&messages, BOTH);
    assert_eq!(roles(&value), vec!["user", "system"]);
    let content = &value[1]["content"];
    assert_eq!(content[0], json!({"type": "text", "text": "tools changed"}));
    assert_eq!(content[1]["type"], "tool_addition");
    assert_eq!(content[1]["tool"]["type"], "tool_definition");
    assert_eq!(content[1]["tool"]["definition"]["name"], "probe_echo");
    assert_eq!(
        content[1]["tool"]["definition"]["input_schema"],
        anthropic_input_schema(&tool("probe_echo", "").input_schema)
    );
    assert_eq!(
        content[2]["tool"]["definition"]["description"],
        "Run a command (revised)"
    );
    assert_eq!(
        content[3],
        json!({"type": "tool_removal", "tool": {"type": "tool_reference", "name": "read"}})
    );

    // A model with system messages but no tool changes: the text only.
    let value = wire(&messages, SYSTEM_ONLY);
    assert_eq!(
        value[1]["content"],
        json!([{"type": "text", "text": "tools changed"}])
    );

    // A notice in user form: its changes move before the next reply.
    let resumed = [
        Message::user("task"),
        Message::assistant_text("working"),
        notice_with("tools changed", changes),
        Message::user("continue"),
        Message::assistant_text("ok"),
        Message::user("more"),
    ];
    let value = wire(&resumed, BOTH);
    assert_eq!(
        roles(&value),
        vec!["user", "assistant", "user", "system", "assistant", "user"]
    );
    let moved = value[3]["content"].as_array().unwrap();
    assert_eq!(moved.len(), 3);
    assert!(moved.iter().all(|block| block["type"] != "text"));
}

#[test]
fn thinking_produced_after_a_system_message_is_bound_to_it() {
    let mut messages = vec![Message::user("go"), notice("note")];
    let request = |messages: &[Message]| ApiRequest {
        model: "claude-opus-5-5".to_string(),
        max_tokens: 1024,
        system: build_system_param("sys", false),
        messages: format_messages_for(messages, BOTH),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    };
    let binding = binding::analyze_request(&request(&messages)).binding;
    messages.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::AnthropicThinking {
                thinking: "t".to_string(),
                signature: "sig".to_string(),
                binding: Some(jcode_message_types::AnthropicThinkingBinding {
                    model: "claude-opus-5-5".to_string(),
                    prefix_digest: binding.prefix_digest,
                    predecessor: binding.last_thinking,
                }),
            },
            ContentBlock::Text {
                text: "done".to_string(),
                cache_control: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });
    messages.push(Message::user("next"));
    let report = binding::analyze_request(&request(&messages));
    assert!(report.invalid().next().is_none(), "{:?}", report.replayed);
    // The same history in the user form is a different prefix.
    let mut user_form = request(&messages);
    user_form.messages = format_messages(&messages);
    assert!(
        binding::analyze_request(&user_form)
            .invalid()
            .next()
            .is_some()
    );
}

#[test]
fn the_cache_reads_where_a_request_that_ended_on_a_notice_wrote() {
    let mut request = ApiRequest {
        model: "claude-opus-5-5".to_string(),
        max_tokens: 1024,
        system: build_system_param("sys", false),
        messages: format_messages_for(
            &[
                Message::user("go"),
                notice("note"),
                Message::assistant_text("done"),
                Message::user("next"),
            ],
            BOTH,
        ),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    };
    place_cache_breakpoints(&mut request, true);
    assert!(
        request.messages[1].content[0].has_cache_control(),
        "the previous request's newest block was the notice"
    );
    assert!(request.messages[3].content[0].has_cache_control());
}
