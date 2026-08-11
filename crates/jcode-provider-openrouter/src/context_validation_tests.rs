use super::{build_chat_messages, validate_projected_messages};
use jcode_message_types::{ContentBlock, Message, Role};
use serde_json::json;

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message {
        role,
        content,
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn projected_tool_turn() -> Vec<Message> {
    vec![
        Message::user("Read Cargo.toml."),
        message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call_read".to_string(),
                name: "read".to_string(),
                input: json!({"file_path":"Cargo.toml"}),
                thought_signature: None,
            }],
        ),
        Message::tool_result("call_read", "[package]", false),
        Message::assistant_text("The file is valid."),
        Message::user("Continue."),
    ]
}

#[test]
fn suppressed_generic_reasoning_uses_only_required_formatter_placeholder() {
    let projected = projected_tool_turn();
    let validation = validate_projected_messages(&projected, true, true, true)
        .expect("projected tool turn must validate");
    let formatted = build_chat_messages(&projected, "", true, true, true);
    let assistant = formatted
        .iter()
        .find(|message| message.get("tool_calls").is_some())
        .expect("assistant tool-call message");

    assert_eq!(assistant["reasoning_content"], json!(" "));
    assert_eq!(validation.formatter_placeholder_count, 1);
    assert!(validation.normalization_notes[0].contains("must not be counted"));
}

#[test]
fn strict_openai_compatible_shape_omits_reasoning_field_and_keeps_pairing() {
    let projected = projected_tool_turn();
    let validation = validate_projected_messages(&projected, false, false, true)
        .expect("strict OpenAI-compatible route must validate without non-standard fields");
    let formatted = build_chat_messages(&projected, "", false, false, true);
    let call_index = formatted
        .iter()
        .position(|message| message.get("tool_calls").is_some())
        .expect("assistant tool call");

    assert!(formatted[call_index].get("reasoning_content").is_none());
    assert_eq!(formatted[call_index + 1]["role"], json!("tool"));
    assert_eq!(
        formatted[call_index + 1]["tool_call_id"],
        json!("call_read")
    );
    assert_eq!(validation.formatter_placeholder_count, 0);
}

#[test]
fn reasoning_only_interrupted_artifact_is_dropped_without_bare_assistant_message() {
    let projected = vec![
        Message::user("Start."),
        message(
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "interrupted".to_string(),
            }],
        ),
        Message::user("Resume."),
    ];

    validate_projected_messages(&projected, false, false, false)
        .expect("reasoning-only artifact must normalize safely");
    let formatted = build_chat_messages(&projected, "", false, false, false);
    assert!(formatted.iter().all(|message| {
        message["role"] != json!("assistant")
            || message.get("content").is_some()
            || message.get("tool_calls").is_some()
    }));
}

#[test]
fn delayed_tool_result_normalization_remains_immediately_paired() {
    let projected = vec![
        Message::user("Run the check."),
        Message::tool_result("call_check", "ok", false),
        message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call_check".to_string(),
                name: "bash".to_string(),
                input: json!({"command":"true"}),
                thought_signature: None,
            }],
        ),
        Message::user("Continue."),
    ];

    validate_projected_messages(&projected, true, false, false)
        .expect("production formatter must preserve delayed-result repair");
    let formatted = build_chat_messages(&projected, "", true, false, false);
    let call_index = formatted
        .iter()
        .position(|message| message.get("tool_calls").is_some())
        .expect("assistant tool call");
    assert_eq!(formatted[call_index + 1]["role"], json!("tool"));
    assert_eq!(
        formatted[call_index + 1]["tool_call_id"],
        json!("call_check")
    );
}

#[test]
fn projected_history_rejects_native_compaction_state() {
    let projected = vec![message(
        Role::User,
        vec![ContentBlock::OpenAICompaction {
            encrypted_content: "legacy-state".to_string(),
        }],
    )];

    let error = validate_projected_messages(&projected, true, true, true).unwrap_err();
    assert!(error.contains("OpenAI-native compaction state"));
}

#[test]
fn normalized_tool_id_collisions_are_rejected() {
    let projected = vec![
        message(
            Role::Assistant,
            vec![
                ContentBlock::ToolUse {
                    id: "call.a".to_string(),
                    name: "read".to_string(),
                    input: json!({"file_path":"a"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "call_a".to_string(),
                    name: "read".to_string(),
                    input: json!({"file_path":"b"}),
                    thought_signature: None,
                },
            ],
        ),
        Message::tool_result("call.a", "a", false),
        Message::tool_result("call_a", "b", false),
    ];

    let error = validate_projected_messages(&projected, true, true, true).unwrap_err();
    assert!(error.contains("duplicate normalized tool-call id 'call_a'"));
}
