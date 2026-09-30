//! INT-01/WP-04: the request-time gate in the real Agent turn loops.
//!
//! A prefix-bound provider replies with signed thinking bound to the exact
//! request it received. After a recorded system-prompt transition, the next
//! request must not replay thinking bound to the old prompt: the loop
//! suppresses it explicitly, persists that, and only then sends.

use super::*;
use crate::context::reasoning_invalidation_tests::{fixture_invalidations, fixture_request};
use crate::provider::{ContextReasoningBlockKind, EventStream, InvalidReplayedReasoning};
use jcode_message_types::{AnthropicThinkingBinding, ReplayableReasoningBlock};
use jcode_provider_anthropic::binding::analyze_request;
use jcode_session_types::{StoredContextOperation, StoredReasoningInvalidationCause};
use serde_json::json;
use std::sync::Mutex as StdMutex;

#[derive(Clone, Copy)]
enum Reply {
    /// Signed thinking, then a `read` call.
    ThinkThenRead,
    /// Signed thinking, then a final answer.
    ThinkThenAnswer,
}

struct Recorded {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    system: String,
}

#[derive(Default)]
struct State {
    script: std::collections::VecDeque<Reply>,
    requests: Vec<Recorded>,
}

#[derive(Clone, Default)]
struct ThinkingProvider {
    state: Arc<StdMutex<State>>,
}

#[async_trait::async_trait]
impl Provider for ThinkingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        let mut state = self.state.lock().unwrap();
        let index = state.requests.len();
        state.requests.push(Recorded {
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            system: system.to_string(),
        });
        let reply = state
            .script
            .pop_front()
            .unwrap_or_else(|| panic!("request {index} has no scripted reply"));
        // Bound exactly as the Anthropic runtime binds a response's blocks.
        let binding = analyze_request(&fixture_request(messages, tools, system)).binding;
        let mut events = vec![StreamEvent::ReplayableReasoning(
            ReplayableReasoningBlock::AnthropicThinking {
                thinking: format!("thinking {index}"),
                signature: format!("signature-{index}"),
                binding: AnthropicThinkingBinding {
                    model: "claude-sonnet-5-5".to_string(),
                    prefix_digest: binding.prefix_digest,
                    predecessor: binding.last_thinking,
                },
            },
        )];
        match reply {
            Reply::ThinkThenRead => events.extend([
                StreamEvent::ToolUseStart {
                    id: format!("toolu_wp04_{index}"),
                    name: "read".into(),
                },
                StreamEvent::ToolInputDelta(
                    json!({"file_path": "notes.txt", "intent": "read fixture"}).to_string(),
                ),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                },
            ]),
            Reply::ThinkThenAnswer => events.extend([
                StreamEvent::TextDelta("done".to_string()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ]),
        }
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> String {
        "claude-sonnet-5-5".into()
    }

    fn reasoning_replay_kind(&self) -> Option<ContextReasoningBlockKind> {
        Some(ContextReasoningBlockKind::AnthropicThinking)
    }

    fn replayed_reasoning_invalidations(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> Option<Vec<InvalidReplayedReasoning>> {
        Some(fixture_invalidations(messages, tools, system))
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

impl ThinkingProvider {
    fn script(&self, replies: &[Reply]) {
        self.state
            .lock()
            .unwrap()
            .script
            .extend(replies.iter().copied());
    }

    fn request(&self, index: usize) -> (Vec<Message>, Vec<ToolDefinition>, String) {
        let state = self.state.lock().unwrap();
        let recorded = &state.requests[index];
        (
            recorded.messages.clone(),
            recorded.tools.clone(),
            recorded.system.clone(),
        )
    }

    fn request_count(&self) -> usize {
        self.state.lock().unwrap().requests.len()
    }
}

fn replayed_signatures(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::AnthropicThinking { signature, .. } => Some(signature.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_recorded_prompt_transition_suppresses_stale_thinking_before_the_next_request()
-> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let project = tempfile::tempdir()?;
    std::fs::write(project.path().join("notes.txt"), "WP-04 fixture notes\n")?;

    let provider = ThinkingProvider::default();
    let registry = Registry::new(Arc::new(provider.clone()) as Arc<dyn Provider>).await;
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(project.path().display().to_string());
    let mut agent = Agent::new_with_session(
        Arc::new(provider.clone()) as Arc<dyn Provider>,
        registry,
        session,
        None,
    );
    agent.set_system_prompt("WP-04 fixture system prompt");

    // Turn 1: two requests, each producing signed thinking.
    provider.script(&[Reply::ThinkThenRead, Reply::ThinkThenAnswer]);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("first task", Vec::new(), None, tx.clone())
        .await?;
    assert_eq!(provider.request_count(), 2);
    let (second, _, _) = provider.request(1);
    assert_eq!(
        replayed_signatures(&second),
        vec!["signature-0".to_string()],
        "an unchanged prefix replays the first request's thinking"
    );
    assert!(
        agent
            .session
            .context_view
            .active_reasoning_invalidation()
            .is_none()
    );

    // A recorded static-prompt transition between turns.
    agent.set_system_prompt("WP-04 fixture system prompt, revised");
    agent.note_prefix_transition("agent system replacement");

    provider.script(&[Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("second task", Vec::new(), None, tx)
        .await?;

    // The request was sent without the stale thinking, and every block it
    // still replays is valid for its prefix.
    let (third, tools, system) = provider.request(2);
    assert_eq!(system, "WP-04 fixture system prompt, revised");
    assert!(
        replayed_signatures(&third).is_empty(),
        "{:?}",
        replayed_signatures(&third)
    );
    assert!(
        analyze_request(&fixture_request(&third, &tools, &system))
            .invalid()
            .next()
            .is_none()
    );

    // The suppression is explicit, persisted and attributed.
    let managed = agent
        .session
        .context_view
        .active_reasoning_invalidation()
        .expect("managed reasoning invalidation");
    let StoredContextOperation::ReasoningSuppression(suppression) = &managed.operations[0] else {
        panic!("managed set holds suppressions");
    };
    assert_eq!(suppression.targets.len(), 2);
    assert_eq!(
        suppression.selection,
        jcode_session_types::StoredReasoningSelection::Invalidated {
            cause: StoredReasoningInvalidationCause::RequestPrefixChanged {
                recorded_transitions: vec!["agent system replacement".to_string()],
            },
        }
    );
    let reloaded = crate::session::Session::load(&agent.session.id)?;
    assert_eq!(reloaded.context_view, agent.session.context_view);
    assert!(agent.pending_prefix_transitions.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_blocking_loop_applies_the_same_gate() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();

    let provider = ThinkingProvider::default();
    let registry = Registry::new(Arc::new(provider.clone()) as Arc<dyn Provider>).await;
    let mut agent = Agent::new_with_session(
        Arc::new(provider.clone()) as Arc<dyn Provider>,
        registry,
        crate::session::Session::create(None, None),
        None,
    );
    agent.set_system_prompt("blocking fixture prompt");
    provider.script(&[Reply::ThinkThenAnswer]);
    agent.run_once_capture("first").await?;

    agent.set_system_prompt("blocking fixture prompt, revised");
    agent.note_prefix_transition("agent system replacement");
    provider.script(&[Reply::ThinkThenAnswer]);
    agent.run_once_capture("second").await?;

    let (second, _, _) = provider.request(1);
    assert!(replayed_signatures(&second).is_empty());
    assert!(
        agent
            .session
            .context_view
            .active_reasoning_invalidation()
            .is_some()
    );
    Ok(())
}
