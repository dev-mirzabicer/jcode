#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Conversation search over the authoritative stored conversation history.

use super::{Tool, ToolContext, ToolOutput};
use crate::context_budget::ContextBudgetTracker;
use crate::message::{Message, Role};
use crate::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Deserialize)]
struct SearchInput {
    /// Search query (keyword search)
    #[serde(default)]
    query: Option<String>,

    /// Get specific turns by range
    #[serde(default)]
    turns: Option<TurnRange>,

    /// Get stats about conversation
    #[serde(default)]
    stats: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TurnRange {
    start: usize,
    end: usize,
}

pub struct ConversationSearchTool {
    context_budget: Arc<RwLock<ContextBudgetTracker>>,
}

impl ConversationSearchTool {
    pub fn new(context_budget: Arc<RwLock<ContextBudgetTracker>>) -> Self {
        Self { context_budget }
    }
}

pub(super) async fn execute_with_context_budget(
    context_budget: &Arc<RwLock<ContextBudgetTracker>>,
    input: Value,
    ctx: ToolContext,
) -> Result<ToolOutput> {
    ConversationSearchTool::new(context_budget.clone())
        .execute(input, ctx)
        .await
}

#[async_trait]
impl Tool for ConversationSearchTool {
    fn name(&self) -> &str {
        "conversation_search"
    }

    fn description(&self) -> &str {
        "Search conversation history."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "turns": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "integer", "description": "Start turn."},
                        "end": {"type": "integer", "description": "End turn."}
                    },
                    "required": ["start", "end"],
                    "description": "Turn range."
                },
                "stats": {
                    "type": "boolean",
                    "description": "Return stats."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SearchInput = serde_json::from_value(input)?;
        let session = load_session(&ctx.session_id);
        if session.is_none() {
            crate::logging::warn(&format!(
                "[tool:conversation_search] failed to load session history for session {}",
                ctx.session_id
            ));
        }
        let session_messages = session.as_ref().map(|session| {
            session
                .messages
                .iter()
                .map(|message| message.to_message())
                .collect::<Vec<_>>()
        });

        let mut output = String::new();

        // Handle stats request
        if params.stats == Some(true) {
            let stats = self.context_budget.read().await.stats();
            let total_stored_messages = session
                .as_ref()
                .map(|session| session.messages.len())
                .unwrap_or_default();
            let context_revision = session
                .as_ref()
                .map(|session| session.context_view.revision)
                .unwrap_or_default();
            let active_context_transactions = session
                .as_ref()
                .map(|session| session.context_view.active_transaction_count())
                .unwrap_or_default();
            let total_context_transactions = session
                .as_ref()
                .map(|session| session.context_view.transactions.len())
                .unwrap_or_default();
            let reverted_context_transactions = session
                .as_ref()
                .map(|session| {
                    session
                        .context_view
                        .transactions
                        .iter()
                        .filter(|transaction| {
                            transaction.latest_status().is_some_and(|status| {
                                status.kind
                                    == jcode_session_types::StoredContextTransactionStatusKind::Reverted
                            })
                        })
                        .count()
                })
                .unwrap_or_default();
            let invalidated_context_transactions = session
                .as_ref()
                .map(|session| {
                    session
                        .context_view
                        .transactions
                        .iter()
                        .filter(|transaction| {
                            transaction.latest_status().is_some_and(|status| {
                                status.kind
                                    == jcode_session_types::StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit
                            })
                        })
                        .count()
                })
                .unwrap_or_default();
            let legacy_summary_present = session
                .as_ref()
                .and_then(|session| session.compaction.as_ref())
                .is_some();
            output.push_str(&format!(
                "## Conversation Stats\n\n\
                 - Total stored messages: {}\n\
                 - Context-view revision: {}\n\
                 - Total context transactions: {}\n\
                 - Active context transactions: {}\n\
                 - Reverted context transactions: {}\n\
                 - Transcript-invalidated context transactions: {}\n\
                 - Legacy summary present: {}\n\
                 - Accounted provider messages: {}\n\
                 - Estimated message tokens: {}\n\
                 - Estimated context tokens: {}\n\
                 - Observed provider tokens: {}\n\
                 - Effective context tokens: {}\n\
                 - Token budget: {}\n\
                 - Context usage: {:.1}%\n",
                total_stored_messages,
                context_revision,
                total_context_transactions,
                active_context_transactions,
                reverted_context_transactions,
                invalidated_context_transactions,
                legacy_summary_present,
                stats.message_count,
                stats.estimated_message_tokens,
                stats.token_estimate,
                stats
                    .observed_input_tokens
                    .map(|tokens| tokens.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                stats.effective_tokens,
                stats.token_budget,
                stats.context_usage * 100.0
            ));
        }

        // Handle keyword search
        if let Some(query) = params.query {
            let results = session_messages
                .as_deref()
                .map(|messages| search_messages(messages, &query))
                .unwrap_or_default();

            if results.is_empty() {
                output.push_str(&format!(
                    "## Search Results\n\nNo results found for '{}'\n",
                    query
                ));
            } else {
                output.push_str(&format!(
                    "## Search Results for '{}'\n\nFound {} matches:\n\n",
                    query,
                    results.len()
                ));

                for result in results.iter().take(10) {
                    let role = match result.role {
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                    };
                    output.push_str(&format!(
                        "**Turn {} ({}):**\n{}\n\n",
                        result.turn, role, result.snippet
                    ));
                }

                if results.len() > 10 {
                    crate::logging::warn(&format!(
                        "[tool:conversation_search] truncating displayed search results for session {} query={} total_results={}",
                        ctx.session_id,
                        query,
                        results.len()
                    ));
                    output.push_str(&format!("... and {} more results\n", results.len() - 10));
                }
            }
        }

        // Handle turn range request
        if let Some(range) = params.turns {
            let turns = session_messages.as_deref().map(|messages| {
                messages
                    .iter()
                    .skip(range.start)
                    .take(range.end.saturating_sub(range.start))
                    .collect::<Vec<_>>()
            });

            if turns.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                output.push_str(&format!(
                    "## Turns {}-{}\n\nNo turns found in that range.\n",
                    range.start, range.end
                ));
            } else if let Some(turns) = turns {
                output.push_str(&format!("## Turns {}-{}\n\n", range.start, range.end));

                for (idx, msg) in turns.iter().enumerate() {
                    let turn_num = range.start + idx;
                    let role = match msg.role {
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                    };

                    output.push_str(&format!("**Turn {} ({}):**\n", turn_num, role));

                    for block in &msg.content {
                        match block {
                            crate::message::ContentBlock::Text { text, .. } => {
                                // Truncate very long messages
                                if text.len() > 1000 {
                                    output.push_str(crate::util::truncate_str(text, 1000));
                                    output.push_str("... (truncated)\n");
                                } else {
                                    output.push_str(text);
                                    output.push('\n');
                                }
                            }
                            crate::message::ContentBlock::ToolUse { name, .. } => {
                                output.push_str(&format!("[Tool call: {}]\n", name));
                            }
                            crate::message::ContentBlock::ToolResult { content, .. } => {
                                let preview = if content.len() > 200 {
                                    format!("{}...", crate::util::truncate_str(content, 200))
                                } else {
                                    content.clone()
                                };
                                output.push_str(&format!("[Tool result: {}]\n", preview));
                            }
                            crate::message::ContentBlock::Reasoning { .. }
                            | crate::message::ContentBlock::ReasoningTrace { .. }
                            | crate::message::ContentBlock::AnthropicThinking { .. }
                            | crate::message::ContentBlock::OpenAIReasoning { .. } => {}
                            crate::message::ContentBlock::Image { .. } => {
                                output.push_str("[Image]\n");
                            }
                            crate::message::ContentBlock::OpenAICompaction { .. } => {
                                output.push_str("[OpenAI native compaction]\n");
                            }
                        }
                    }
                    output.push('\n');
                }
            }
        }

        if output.is_empty() {
            output = "Please provide a 'query' to search, 'turns' range to retrieve, \
                      or 'stats': true to see conversation statistics."
                .to_string();
        }

        Ok(ToolOutput::new(output).with_title("conversation_search"))
    }
}

/// Search result from conversation history
struct SearchResult {
    turn: usize,
    role: Role,
    snippet: String,
}

fn load_session(session_id: &str) -> Option<Session> {
    Session::load(session_id).ok()
}

fn search_messages(messages: &[Message], query: &str) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        let text = message_to_text(msg);
        if text.to_lowercase().contains(&query_lower) {
            let snippet = extract_snippet(&text, &query_lower);
            results.push(SearchResult {
                turn: idx,
                role: msg.role.clone(),
                snippet,
            });
        }
    }

    results
}

fn message_to_text(msg: &Message) -> String {
    msg.content
        .iter()
        .filter_map(|block| match block {
            crate::message::ContentBlock::Text { text, .. } => Some(text.clone()),
            crate::message::ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            crate::message::ContentBlock::OpenAICompaction { .. } => {
                Some("[OpenAI native compaction]".to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_snippet(text: &str, query: &str) -> String {
    if query.is_empty() {
        return text.chars().take(100).collect();
    }

    let mut lower = String::new();
    let mut source_byte_for_lower_byte = Vec::new();
    for (source_byte, character) in text.char_indices() {
        for lowercase_character in character.to_lowercase() {
            let mut encoded = [0u8; 4];
            let lowercase = lowercase_character.encode_utf8(&mut encoded);
            lower.push_str(lowercase);
            source_byte_for_lower_byte.extend(std::iter::repeat_n(source_byte, lowercase.len()));
        }
    }

    if let Some(lower_start) = lower.find(query) {
        let lower_end = lower_start.saturating_add(query.len());
        let match_start = source_byte_for_lower_byte[lower_start];
        let final_character_start = source_byte_for_lower_byte[lower_end - 1];
        let match_end = final_character_start
            + text[final_character_start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or_default();
        let start = text[..match_start]
            .char_indices()
            .rev()
            .nth(49)
            .map(|(offset, _)| offset)
            .unwrap_or(0);
        let end = text[match_end..]
            .char_indices()
            .nth(50)
            .map(|(offset, _)| match_end + offset)
            .unwrap_or(text.len());
        let mut snippet = text[start..end].to_string();
        if start > 0 {
            snippet = format!("...{}", snippet);
        }
        if end < text.len() {
            snippet = format!("{}...", snippet);
        }
        snippet
    } else {
        text.chars().take(100).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_budget::ContextBudgetTracker;
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextStatusEvent, StoredContextTransaction,
        StoredContextTransactionStatusKind,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_test_tool() -> ConversationSearchTool {
        let context_budget = Arc::new(RwLock::new(ContextBudgetTracker::new()));
        ConversationSearchTool::new(context_budget)
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::storage::lock_test_env()
    }

    fn setup_session(messages: Vec<Message>) -> (ToolContext, std::path::PathBuf, Option<String>) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("jcode-test-{}", nonce));
        let _ = std::fs::create_dir_all(base.join("sessions"));

        let previous_home = std::env::var("JCODE_HOME").ok();
        crate::env::set_var("JCODE_HOME", &base);

        let session_id = format!("test-session-{}", nonce);
        let mut session = Session::create_with_id(session_id.clone(), None, None);
        for msg in messages {
            session.add_message(msg.role.clone(), msg.content.clone());
        }
        session.save().unwrap();

        let ctx = ToolContext {
            session_id,
            message_id: "test-message".to_string(),
            tool_call_id: "test-tool-call".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        };

        (ctx, base, previous_home)
    }

    fn restore_env(base: std::path::PathBuf, previous_home: Option<String>) {
        if let Some(prev) = previous_home {
            crate::env::set_var("JCODE_HOME", prev);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_tool_name() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "conversation_search");
    }

    #[tokio::test]
    async fn test_stats() {
        let _guard = env_lock();
        let context_budget = Arc::new(RwLock::new(ContextBudgetTracker::new().with_budget(10_000)));
        {
            let mut tracker = context_budget.write().await;
            tracker.seed_messages(&[Message::user("raw authoritative needle")]);
            tracker.update_observed_input_tokens(7_000);
        }
        let tool = ConversationSearchTool::new(context_budget);
        let (ctx, base, previous_home) =
            setup_session(vec![Message::user("raw authoritative needle")]);
        let mut session = Session::load(&ctx.session_id).expect("load test session");
        session.context_view.revision = 1;
        session
            .context_view
            .transactions
            .push(StoredContextTransaction {
                id: "context-transaction-1".to_string(),
                base_revision: 0,
                created_at: chrono::Utc::now(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
                operations: Vec::new(),
                status_events: vec![StoredContextStatusEvent {
                    revision: 1,
                    timestamp: chrono::Utc::now(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: None,
                }],
                application: None,
                economics: None,
                curator_usage: Vec::new(),
                emergency_audit: None,
            });
        session.save().expect("save context state");
        let input = json!({"stats": true});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("Conversation Stats"));
        assert!(result.output.contains("Total stored messages: 1"));
        assert!(result.output.contains("Context-view revision: 1"));
        assert!(result.output.contains("Total context transactions: 1"));
        assert!(result.output.contains("Active context transactions: 1"));
        assert!(result.output.contains("Reverted context transactions: 0"));
        assert!(
            result
                .output
                .contains("Transcript-invalidated context transactions: 0")
        );
        assert!(result.output.contains("Observed provider tokens: 7000"));
        assert!(result.output.contains("Effective context tokens: 7000"));
        assert!(result.output.contains("Token budget: 10000"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn registry_execution_uses_the_current_clone_context_budget() {
        let _guard = env_lock();
        let template_budget = Arc::new(RwLock::new(ContextBudgetTracker::new().with_budget(1_000)));
        template_budget
            .write()
            .await
            .update_observed_input_tokens(111);

        let mut tools = std::collections::HashMap::new();
        tools.insert(
            "conversation_search".to_string(),
            Arc::new(ConversationSearchTool::new(template_budget.clone())) as Arc<dyn Tool>,
        );
        let template = crate::tool::Registry {
            tools: Arc::new(RwLock::new(tools)),
            skills: Arc::new(RwLock::new(crate::skill::SkillRegistry::default())),
            context_budget: template_budget,
            legacy_compaction: Arc::new(RwLock::new(crate::compaction::CompactionManager::new())),
        };
        let registry = template.clone();
        {
            let context_budget = registry.context_budget();
            let mut tracker = context_budget.write().await;
            tracker.set_budget(10_000);
            tracker.update_observed_input_tokens(7_000);
        }

        let (ctx, base, previous_home) = setup_session(vec![Message::user("clone stats")]);
        let result = registry
            .execute("conversation_search", json!({"stats": true}), ctx)
            .await
            .expect("conversation search should execute");

        assert!(result.output.contains("Observed provider tokens: 7000"));
        assert!(result.output.contains("Token budget: 10000"));
        assert!(!result.output.contains("Observed provider tokens: 111"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn search_uses_raw_authoritative_history_when_context_state_is_active() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) =
            setup_session(vec![Message::user("raw-only-search-needle")]);

        let result = tool
            .execute(json!({"query": "raw-only-search-needle"}), ctx)
            .await
            .unwrap();
        assert!(result.output.contains("Found 1 matches"));
        assert!(result.output.contains("raw-only-search-needle"));
        restore_env(base, previous_home);
    }

    #[test]
    fn snippet_extraction_is_unicode_boundary_safe_and_handles_case_expansion() {
        let box_drawing = "─".repeat(80);
        let text = format!("{box_drawing} Needle {box_drawing}");
        let result = search_messages(&[Message::user(&text)], "needle");
        assert_eq!(result.len(), 1);
        assert!(result[0].snippet.contains("Needle"));

        let expanded_case = search_messages(&[Message::user("İstanbul café")], "i̇stanbul");
        assert_eq!(expanded_case.len(), 1);
        assert!(expanded_case[0].snippet.contains("İstanbul"));
    }

    #[tokio::test]
    async fn test_empty_search() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) = setup_session(Vec::new());
        let input = json!({"query": "nonexistent"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("No results found"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn test_empty_turns() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) = setup_session(Vec::new());
        let input = json!({"turns": {"start": 0, "end": 5}});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("No turns found"));
        restore_env(base, previous_home);
    }
}
