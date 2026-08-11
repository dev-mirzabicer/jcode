use super::{format_messages, validate_projected_messages};
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

fn thinking_tool_fixture(include_thinking: bool) -> Vec<Message> {
    let mut assistant_content = Vec::new();
    if include_thinking {
        assistant_content.push(ContentBlock::AnthropicThinking {
            thinking: "I will inspect both files.".to_string(),
            signature: "signed-thinking-state".to_string(),
        });
    }
    assistant_content.extend([
        ContentBlock::ToolUse {
            id: "call_read".to_string(),
            name: "read".to_string(),
            input: json!({"file_path":"src/lib.rs"}),
            thought_signature: None,
        },
        ContentBlock::ToolUse {
            id: "call_image".to_string(),
            name: "read".to_string(),
            input: json!({"file_path":"diagram.png"}),
            thought_signature: None,
        },
    ]);

    vec![
        message(Role::Assistant, assistant_content),
        message(
            Role::User,
            vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_read".to_string(),
                    content: "source".to_string(),
                    is_error: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "aW1hZ2U=".to_string(),
                },
                ContentBlock::Text {
                    text: "[Attached image associated with the preceding tool result: diagram.png]"
                        .to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_image".to_string(),
                    content: "image metadata".to_string(),
                    is_error: None,
                },
            ],
        ),
        Message::assistant_text("Both results are valid."),
        Message::user("Continue with the implementation."),
    ]
}

#[test]
fn retained_and_suppressed_signed_thinking_both_validate_around_parallel_tools() {
    let retained = thinking_tool_fixture(true);
    let retained_validation =
        validate_projected_messages(&retained, false).expect("retained thinking must validate");
    let retained_json = serde_json::to_value(format_messages(&retained, false)).unwrap();
    assert_eq!(retained_validation.normalized_item_count, 4);
    assert!(retained_json.to_string().contains("signed-thinking-state"));

    let suppressed = thinking_tool_fixture(false);
    let suppressed_validation = validate_projected_messages(&suppressed, false)
        .expect("complete thinking-block suppression must keep the tool turn valid");
    let suppressed_json = serde_json::to_value(format_messages(&suppressed, false)).unwrap();
    assert_eq!(suppressed_validation.normalized_item_count, 4);
    assert!(!suppressed_json.to_string().contains("thinking"));
    assert_eq!(
        suppressed_json[1]["content"][0]["type"],
        json!("tool_result")
    );
    assert_eq!(
        suppressed_json[1]["content"][1]["type"],
        json!("tool_result")
    );
    assert_eq!(
        suppressed_json[1]["content"][0]["content"][1]["type"],
        json!("image")
    );
}

#[test]
fn thinking_without_signature_is_rejected_with_precise_diagnostic() {
    let messages = vec![
        message(
            Role::Assistant,
            vec![
                ContentBlock::AnthropicThinking {
                    thinking: "unsigned".to_string(),
                    signature: String::new(),
                },
                ContentBlock::Text {
                    text: "answer".to_string(),
                    cache_control: None,
                },
            ],
        ),
        Message::user("Continue."),
    ];

    let error = validate_projected_messages(&messages, false).unwrap_err();
    assert!(error.contains("without a complete non-empty signature"));
}

#[test]
fn selected_summary_replacing_complete_thinking_and_tool_range_validates() {
    let projected = vec![
        Message::user(
            "## Selected Conversation Summary\n\nThe assistant inspected src/lib.rs and diagram.png; both reads succeeded.",
        ),
        Message::assistant_text("I will continue from the preserved result."),
        Message::user("Implement the next change."),
    ];

    let validation = validate_projected_messages(&projected, true)
        .expect("summary replacing the complete range must remain sendable");
    assert_eq!(validation.normalized_item_count, 3);
}

#[test]
fn normalized_tool_id_collisions_are_rejected() {
    let messages = vec![
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
        message(
            Role::User,
            vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call.a".to_string(),
                    content: "a".to_string(),
                    is_error: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_a".to_string(),
                    content: "b".to_string(),
                    is_error: None,
                },
            ],
        ),
    ];

    let error = validate_projected_messages(&messages, false).unwrap_err();
    assert!(error.contains("duplicate normalized tool_use id 'call_a'"));
}
