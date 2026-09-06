//! Provider-backed summaries for explicit session transfer.
//!
//! Transfer creates a new authoritative handoff message. It is a session
//! lifecycle operation, not a provider-context transform, and therefore owns no
//! compaction policy, persisted projection state, or automatic trigger.

use crate::instruction::{InstructionRepositoryService, workflow::Workflow};
use crate::message::{ContentBlock, Message, Role};
use crate::provider::Provider;
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::Arc;

const CHARS_PER_TOKEN: usize = 4;
const OUTPUT_RESERVE_TOKENS: usize = 4_000;

/// Generate the readable summary that becomes the transfer child's sole
/// authoritative handoff message. Empty histories produce no handoff.
pub async fn build_transfer_handoff_summary(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
) -> Result<Option<String>> {
    if messages.is_empty() {
        return Ok(None);
    }

    let runtime = crate::instruction::notification::occurrence_runtime(repositories, working_dir)?;
    let task = Workflow::TransferHandoffTask.render_in(&runtime)?;
    let system = Workflow::TransferHandoffSystem.render_in(&runtime)?;
    // Keep the transfer owner's existing output reserve and conservative byte
    // estimator, while respecting any stricter route budget. Complete managed
    // system instructions consume input capacity just like the user prompt.
    let max_prompt_chars = provider
        .context_window()
        .saturating_sub(OUTPUT_RESERVE_TOKENS)
        .min(provider.context_request_budget().safe_input_budget())
        .saturating_mul(CHARS_PER_TOKEN)
        .saturating_sub(system.len());
    let minimum_prompt_chars = task.len().saturating_add(8);
    if max_prompt_chars <= minimum_prompt_chars {
        bail!(
            "provider context window is too small to prepare a safe transfer handoff ({} tokens)",
            provider.context_window()
        );
    }
    let prompt = build_transfer_prompt(&messages, max_prompt_chars, &task);
    let summary = provider.complete_simple(&prompt, &system).await?;
    Ok(Some(summary))
}

fn build_transfer_prompt(messages: &[Message], max_prompt_chars: usize, task: &str) -> String {
    const TRUNCATION_MARKER: &str = "\n\n... [conversation truncated to fit provider input]\n";
    let mut conversation = render_conversation(messages);
    let overhead = task.len().saturating_add(8);
    let conversation_budget = max_prompt_chars.saturating_sub(overhead);
    if conversation.len() > conversation_budget {
        let content_budget = conversation_budget.saturating_sub(TRUNCATION_MARKER.len());
        conversation = truncate_str_boundary(&conversation, content_budget).to_string();
        conversation.push_str(truncate_str_boundary(
            TRUNCATION_MARKER,
            conversation_budget.saturating_sub(conversation.len()),
        ));
    }
    let prompt = format!("{conversation}\n\n---\n\n{task}");
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

    #[derive(Clone, Default)]
    struct RecordingProvider {
        calls: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        window: usize,
        budget: Option<jcode_provider_core::ContextRequestBudget>,
    }

    #[async_trait::async_trait]
    impl Provider for RecordingProvider {
        async fn complete(
            &self,
            messages: &[Message],
            tools: &[crate::message::ToolDefinition],
            system: &str,
            resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            assert!(tools.is_empty());
            assert!(resume_session_id.is_none());
            assert_eq!(messages.len(), 1);
            assert!(matches!(messages[0].role, Role::User));
            let ContentBlock::Text { text, .. } = &messages[0].content[0] else {
                panic!("expected text");
            };
            self.calls
                .lock()
                .unwrap()
                .push((text.clone(), system.to_string()));
            Ok(Box::pin(futures::stream::iter([Ok(
                crate::message::StreamEvent::TextDelta("SUMMARY".into()),
            )])))
        }

        fn name(&self) -> &str {
            "recording"
        }
        fn context_window(&self) -> usize {
            self.window
        }
        fn context_request_budget(&self) -> jcode_provider_core::ContextRequestBudget {
            self.budget
                .unwrap_or_else(|| jcode_provider_core::ContextRequestBudget::unknown(self.window))
        }
        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    fn write_workflow(root: &Path, path: &str, kind: &str, body: &str) {
        let id = Path::new(path).file_stem().unwrap().to_str().unwrap();
        std::fs::write(
            root.join(path),
            format!("---\nid: {id}\nkind: {kind}\ntemplate: handlebars\n---\n{body}"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn transfer_accounts_for_complete_system_and_user_instructions_before_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let repositories = InstructionRepositoryService::from_paths(
            temp.path().join("home"),
            temp.path().join("state"),
        );
        crate::instruction::SystemPromptComposer::from_repository_service(repositories.clone())
            .ensure_global_store()
            .unwrap();
        let root = repositories.global_repository().unwrap().root;
        let task = "TASK".repeat(100);
        let system = "SYSTEM".repeat(200);
        write_workflow(&root, "modules/transfer-handoff-task.md", "module", &task);
        write_workflow(
            &root,
            "system/transfer-handoff-system.md",
            "system",
            &system,
        );
        let provider = RecordingProvider {
            window: 4_500,
            ..Default::default()
        };
        let messages = vec![Message::user(&"界".repeat(10_000))];
        build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
            .await
            .unwrap();
        let calls = provider.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, system);
        assert!(calls[0].0.ends_with(&task));
        assert!(calls[0].0.len() + calls[0].1.len() <= 2_000);
        assert!(calls[0].0.contains("conversation truncated"));
        write_workflow(
            &root,
            "system/transfer-handoff-system.md",
            "system",
            &"S".repeat(2_000),
        );
        assert!(
            build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
                .await
                .is_err()
        );
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
        write_workflow(&root, "system/transfer-handoff-system.md", "system", "S");
        write_workflow(
            &root,
            "modules/transfer-handoff-task.md",
            "module",
            &"T".repeat(2_000),
        );
        assert!(
            build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
                .await
                .is_err()
        );
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
        write_workflow(&root, "modules/transfer-handoff-task.md", "module", &task);
        let restricted = RecordingProvider {
            window: 100_000,
            budget: Some(jcode_provider_core::ContextRequestBudget {
                context_window: 100_000,
                semantics: jcode_provider_core::ContextWindowSemantics::InputPlusOutput,
                requested_max_output_tokens: Some(99_900),
                estimator_margin_tokens: 50,
            }),
            ..Default::default()
        };
        assert!(
            build_transfer_handoff_summary(restricted.fork(), messages, &repositories, None)
                .await
                .is_err()
        );
        assert!(restricted.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn managed_transfer_uses_current_complete_sources_and_blocks_before_provider() {
        let temp = tempfile::tempdir().unwrap();
        let repositories = InstructionRepositoryService::from_paths(
            temp.path().join("home"),
            temp.path().join("state"),
        );
        crate::instruction::SystemPromptComposer::from_repository_service(repositories.clone())
            .ensure_global_store()
            .unwrap();
        let root = repositories.global_repository().unwrap().root;
        let provider = RecordingProvider {
            window: 100_000,
            ..Default::default()
        };
        write_workflow(
            &root,
            "modules/transfer-handoff-task.md",
            "module",
            "OLD-TASK",
        );
        write_workflow(
            &root,
            "system/transfer-handoff-system.md",
            "system",
            "OLD-SYSTEM",
        );
        let messages = vec![Message::user("DATA <&界>")];
        assert_eq!(
            build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
                .await
                .unwrap(),
            Some("SUMMARY".into())
        );
        write_workflow(
            &root,
            "modules/transfer-handoff-task.md",
            "module",
            "NEW-TASK",
        );
        write_workflow(
            &root,
            "system/transfer-handoff-system.md",
            "system",
            "NEW-SYSTEM",
        );
        build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
            .await
            .unwrap();
        let calls = provider.calls.lock().unwrap().clone();
        assert_eq!(
            calls[0],
            (
                "**User:**\nDATA <&界>\n\n\n\n---\n\nOLD-TASK".into(),
                "OLD-SYSTEM".into()
            )
        );
        assert_eq!(
            calls[1],
            (
                "**User:**\nDATA <&界>\n\n\n\n---\n\nNEW-TASK".into(),
                "NEW-SYSTEM".into()
            )
        );
        write_workflow(
            &root,
            "system/transfer-handoff-system.md",
            "system",
            "{{unknown}}",
        );
        assert!(
            build_transfer_handoff_summary(provider.fork(), messages.clone(), &repositories, None)
                .await
                .is_err()
        );
        std::fs::remove_file(root.join("system/transfer-handoff-system.md")).unwrap();
        assert!(
            build_transfer_handoff_summary(provider.fork(), messages, &repositories, None)
                .await
                .is_err()
        );
        assert_eq!(provider.calls.lock().unwrap().len(), 2);
        // An empty history never needs a workflow source or model call.
        assert_eq!(
            build_transfer_handoff_summary(provider.fork(), vec![], &repositories, None)
                .await
                .unwrap(),
            None
        );
    }

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
        let prompt = build_transfer_prompt(&messages, usize::MAX, "SYNTHETIC TASK");
        assert!(!prompt.contains("private reasoning"));
        assert!(prompt.contains("... (truncated)"));
        assert!(prompt.contains("Result for tool-1"));
        assert!(prompt.ends_with("\n\n---\n\nSYNTHETIC TASK"));
    }

    #[test]
    fn transfer_prompt_respects_its_character_budget_including_truncation_marker() {
        let input = "x".repeat(10_000);
        let messages = vec![Message::user(&input)];
        let task = "SYNTHETIC TASK";
        let budget = task.len() + 8 + 120;
        let prompt = build_transfer_prompt(&messages, budget, task);
        assert!(prompt.len() <= budget);
        assert!(prompt.contains("conversation truncated"));
    }
}
