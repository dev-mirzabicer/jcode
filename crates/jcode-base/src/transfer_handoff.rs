//! Provider-backed summaries for explicit session transfer.
//!
//! Transfer creates a new authoritative handoff message. It is a session
//! lifecycle operation, not a provider-context transform, and therefore owns no
//! compaction policy, persisted projection state, or automatic trigger.

use crate::message::{ContentBlock, Message, Role};
use crate::provider::Provider;
use anyhow::{Result, bail};
use std::sync::Arc;

const CHARS_PER_TOKEN: usize = 4;
const OUTPUT_RESERVE_TOKENS: usize = 4_000;
const TRANSFER_PROMPT: &str = r#"Summarize this conversation as a self-contained handoff for a new session.

Write in natural language with these sections:
- **Context:** What is being worked on and why
- **What was done:** Key actions, files changed, decisions, and rejected alternatives
- **Current state:** What works, what is broken, validation performed, and unresolved failures
- **Next steps:** Exact work that should continue
- **User preferences:** Requirements, constraints, and workflow decisions that remain binding

Preserve operationally relevant paths, identifiers, values, commands, observed results, and exact error strings. Never claim unverified work passed and never omit a known unresolved failure. Be concise without replacing precise technical facts with vague prose."#;

/// Generate the readable summary that becomes the transfer child's sole
/// authoritative handoff message. Empty histories produce no handoff.
pub async fn build_transfer_handoff_summary(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
) -> Result<Option<String>> {
    if messages.is_empty() {
        return Ok(None);
    }

    let max_prompt_chars = provider
        .context_window()
        .saturating_sub(OUTPUT_RESERVE_TOKENS)
        .saturating_mul(CHARS_PER_TOKEN);
    let minimum_prompt_chars = TRANSFER_PROMPT.len().saturating_add(8);
    if max_prompt_chars <= minimum_prompt_chars {
        bail!(
            "provider context window is too small to prepare a safe transfer handoff ({} tokens)",
            provider.context_window()
        );
    }
    let prompt = build_transfer_prompt(&messages, max_prompt_chars);
    let summary = provider
        .complete_simple(
            &prompt,
            "You prepare precise, self-contained technical session handoffs.",
        )
        .await?;
    Ok(Some(summary))
}

fn build_transfer_prompt(messages: &[Message], max_prompt_chars: usize) -> String {
    const TRUNCATION_MARKER: &str = "\n\n... [conversation truncated to fit provider input]\n";
    let mut conversation = render_conversation(messages);
    let overhead = TRANSFER_PROMPT.len().saturating_add(8);
    let conversation_budget = max_prompt_chars.saturating_sub(overhead);
    if conversation.len() > conversation_budget {
        let content_budget = conversation_budget.saturating_sub(TRUNCATION_MARKER.len());
        conversation = truncate_str_boundary(&conversation, content_budget).to_string();
        conversation.push_str(truncate_str_boundary(
            TRUNCATION_MARKER,
            conversation_budget.saturating_sub(conversation.len()),
        ));
    }
    let prompt = format!("{conversation}\n\n---\n\n{TRANSFER_PROMPT}");
    debug_assert!(prompt.len() <= max_prompt_chars || max_prompt_chars <= overhead);
    prompt
}

fn render_conversation(messages: &[Message]) -> String {
    let mut rendered = String::new();
    for message in messages {
        let role = match message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        rendered.push_str("**");
        rendered.push_str(role);
        rendered.push_str(":**\n");
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    rendered.push_str(text);
                    rendered.push('\n');
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    rendered.push_str(&format!("[Tool {id}: {name} - {input}]\n"));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let content = if content.len() > 500 {
                        format!("{}... (truncated)", truncate_str_boundary(content, 500))
                    } else {
                        content.clone()
                    };
                    let error_label = is_error
                        .filter(|is_error| *is_error)
                        .map(|_| " error")
                        .unwrap_or_default();
                    rendered.push_str(&format!(
                        "[Result for {tool_use_id}{error_label}: {content}]\n"
                    ));
                }
                ContentBlock::Image { .. } => rendered.push_str("[Image]\n"),
                ContentBlock::OpenAICompaction { .. } => {
                    rendered.push_str("[Historical OpenAI compaction state]\n");
                }
                ContentBlock::Reasoning { .. }
                | ContentBlock::ReasoningTrace { .. }
                | ContentBlock::AnthropicThinking { .. }
                | ContentBlock::OpenAIReasoning { .. } => {}
            }
        }
        rendered.push('\n');
    }
    rendered
}

fn truncate_str_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_prompt_omits_reasoning_and_bounds_tool_results_utf8_safely() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "private reasoning".to_string(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    content: "界".repeat(600),
                    is_error: Some(false),
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let prompt = build_transfer_prompt(&messages, usize::MAX);
        assert!(!prompt.contains("private reasoning"));
        assert!(prompt.contains("... (truncated)"));
        assert!(prompt.contains("Result for tool-1"));
        assert!(prompt.contains("self-contained handoff"));
    }

    #[test]
    fn transfer_prompt_respects_its_character_budget_including_truncation_marker() {
        let input = "x".repeat(10_000);
        let messages = vec![Message::user(&input)];
        let budget = TRANSFER_PROMPT.len() + 8 + 120;
        let prompt = build_transfer_prompt(&messages, budget);
        assert!(prompt.len() <= budget);
        assert!(prompt.contains("conversation truncated"));
    }
}
