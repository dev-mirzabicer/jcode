//! INT-01/WP-03 R10: tool results always lead a merged user message.
//!
//! Delivered context (a system reminder) and a safe-boundary input are
//! persisted as separate user messages after the tool results. Same-role
//! merging joins them into one Anthropic user message, whose tool results
//! must still come first.

use super::*;
use jcode_message_types::{ContentBlock, Message, Role};

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message {
        role,
        content,
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

fn tool_use(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.to_string(),
        name: "read".to_string(),
        input: json!({"file_path": "a.txt"}),
        thought_signature: None,
    }
}

fn tool_result(id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: format!("result {id}"),
        is_error: None,
    }
}

fn kinds(message: &ApiMessage) -> Vec<&'static str> {
    message
        .content
        .iter()
        .map(|block| match block {
            ApiContentBlock::ToolResult { .. } => "tool_result",
            ApiContentBlock::Text { .. } => "text",
            _ => "other",
        })
        .collect()
}

#[test]
fn parallel_results_lead_a_user_message_that_also_carries_delivered_context() {
    let formatted = format_messages(&[
        message(Role::User, vec![text("prompt")]),
        message(
            Role::Assistant,
            vec![tool_use("toolu_a"), tool_use("toolu_b")],
        ),
        // Stored order after an injected input and a reminder landed between
        // two parallel results: result, input, reminder, result.
        message(Role::User, vec![tool_result("toolu_a")]),
        message(Role::User, vec![text("injected input")]),
        message(
            Role::User,
            vec![text(
                "<system-reminder>\n# System Reminder\n\nsynthetic\n</system-reminder>",
            )],
        ),
        message(Role::User, vec![tool_result("toolu_b")]),
    ]);

    assert_eq!(formatted.len(), 3);
    assert_eq!(
        kinds(&formatted[2]),
        vec!["tool_result", "tool_result", "text", "text"]
    );
    let texts: Vec<&str> = formatted[2]
        .content
        .iter()
        .filter_map(|block| match block {
            ApiContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts[0], "injected input");
    assert!(texts[1].starts_with("<system-reminder>"));
}

#[test]
fn a_message_that_already_leads_with_results_is_unchanged() {
    let formatted = format_messages(&[
        message(Role::User, vec![text("prompt")]),
        message(Role::Assistant, vec![tool_use("toolu_a")]),
        message(Role::User, vec![tool_result("toolu_a")]),
        message(
            Role::User,
            vec![text("<system-reminder>\nnudge\n</system-reminder>")],
        ),
    ]);
    assert_eq!(kinds(&formatted[2]), vec!["tool_result", "text"]);
}
