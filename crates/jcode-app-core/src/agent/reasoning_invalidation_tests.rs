//! INT-01/WP-04: the request-time gate in the real Agent turn loops.
//!
//! A prefix-bound provider replies with signed thinking bound to the exact
//! request it received. After a recorded system-prompt transition, the next
//! request must not replay thinking bound to the old prompt: the loop
//! suppresses it explicitly, persists that, and only then sends.

use super::*;
use crate::context::reasoning_invalidation_tests::{
    fixture_invalidations_for, fixture_request, fixture_request_for,
};
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
    /// Whether the model takes operator notices as system messages and tool
    /// changes inside them (Opus 5.5), or neither.
    inline: bool,
    state: Arc<StdMutex<State>>,
}

impl ThinkingProvider {
    fn caps(&self) -> jcode_provider_core::AnthropicConversationCaps {
        jcode_provider_core::AnthropicConversationCaps {
            system_messages: self.inline,
            inline_tool_changes: self.inline,
        }
    }
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
        let binding =
            analyze_request(&fixture_request_for(messages, tools, system, self.caps())).binding;
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
        Some(fixture_invalidations_for(
            messages,
            tools,
            system,
            self.caps(),
        ))
    }

    fn renders_operator_notices(&self) -> bool {
        self.inline
    }

    fn renders_tool_changes(&self) -> bool {
        self.inline
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

/// A registry tool that is never called; registering it changes membership.
struct FixtureTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl crate::tool::Tool for FixtureTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "fixture tool"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}})
    }
    fn decode_input(&self, _input: &serde_json::Value) -> Result<()> {
        Ok(())
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tool::ToolContext,
    ) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new("ok"))
    }
}

fn tool_names(tools: &[ToolDefinition]) -> Vec<String> {
    tools.iter().map(|tool| tool.name.clone()).collect()
}

async fn thinking_agent(provider: &ThinkingProvider, project: &std::path::Path) -> Agent {
    let registry = Registry::new(Arc::new(provider.clone()) as Arc<dyn Provider>).await;
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(project.display().to_string());
    let mut agent = Agent::new_with_session(
        Arc::new(provider.clone()) as Arc<dyn Provider>,
        registry,
        session,
        None,
    );
    agent.set_system_prompt("WP-05 fixture system prompt");
    agent
}

/// A context transition changes history, not tools (INT-01/WP-05): the next
/// request keeps the tool bytes and every earlier Claude thinking block. A
/// registry change is announced at the next request (INT-01/WP-06, D15); on a
/// provider whose `tools` array carries it, that is a recorded tool-set
/// transition, named as the cause of the thinking it invalidates.
#[tokio::test]
async fn a_context_transition_keeps_the_tool_set_and_earlier_thinking() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let project = tempfile::tempdir()?;
    std::fs::write(project.path().join("notes.txt"), "WP-05 fixture notes\n")?;
    let provider = ThinkingProvider::default();
    let mut agent = thinking_agent(&provider, project.path()).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    provider.script(&[Reply::ThinkThenRead, Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("first task", Vec::new(), None, tx.clone())
        .await?;
    let (_, frozen_tools, _) = provider.request(1);

    // The transition hook every context apply, revert and reapply runs, then
    // the next request.
    agent.after_provider_context_changed("context transaction", "fixture revision", true)?;
    provider.script(&[Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("second task", Vec::new(), None, tx.clone())
        .await?;
    let (third, tools, system) = provider.request(2);
    assert_eq!(
        serde_json::to_value(&tools)?,
        serde_json::to_value(&frozen_tools)?,
        "the tool bytes are kept"
    );
    assert_eq!(
        replayed_signatures(&third),
        vec!["signature-0", "signature-1"],
        "every earlier thinking block is replayed"
    );
    assert!(
        analyze_request(&fixture_request(&third, &tools, &system))
            .invalid()
            .next()
            .is_none()
    );
    assert!(
        agent
            .session
            .context_view
            .active_reasoning_invalidation()
            .is_none(),
        "no thinking was invalidated"
    );

    // The registry gains a tool. The next request announces it once; this
    // provider's array must carry it, so the thinking bound to the old set
    // is suppressed explicitly and the cause names the transition.
    let since = std::time::Instant::now();
    agent
        .registry
        .register(
            "fixture_extra".to_string(),
            Arc::new(FixtureTool {
                name: "fixture_extra",
            }),
        )
        .await;
    provider.script(&[Reply::ThinkThenAnswer, Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("third task", Vec::new(), None, tx.clone())
        .await?;
    let (fourth, tools, _) = provider.request(3);
    assert_eq!(
        tool_names(&tools).last().map(String::as_str),
        Some("fixture_extra")
    );
    assert_eq!(
        serde_json::to_value(&tools[..frozen_tools.len()])?,
        serde_json::to_value(&frozen_tools)?,
        "first-sent tools keep their bytes and order"
    );
    assert!(replayed_signatures(&fourth).is_empty());
    assert_eq!(
        crate::tool::tool_set_notice_count(&agent.session.messages),
        1
    );
    let managed = agent
        .session
        .context_view
        .active_reasoning_invalidation()
        .expect("managed reasoning invalidation");
    let StoredContextOperation::ReasoningSuppression(suppression) = &managed.operations[0] else {
        panic!("managed set holds suppressions");
    };
    assert_eq!(
        suppression.selection,
        jcode_session_types::StoredReasoningSelection::Invalidated {
            cause: StoredReasoningInvalidationCause::RequestPrefixChanged {
                recorded_transitions: vec![crate::tool::TOOL_SET_TRANSITION.to_string()],
            },
        }
    );
    assert!(
        crate::cache_invalidation::recorded_since(since)
            .iter()
            .any(|entry| entry.source == crate::tool::TOOL_SET_TRANSITION),
        "the tool-set change is journaled"
    );

    // Once announced, the set is stable: the next request replays the
    // thinking produced after the transition and announces nothing.
    agent
        .run_once_streaming_mpsc("fourth task", Vec::new(), None, tx)
        .await?;
    let (fifth, later_tools, _) = provider.request(4);
    assert_eq!(
        serde_json::to_value(&later_tools)?,
        serde_json::to_value(&tools)?
    );
    assert_eq!(replayed_signatures(&fifth), vec!["signature-3"]);
    assert_eq!(
        crate::tool::tool_set_notice_count(&agent.session.messages),
        1
    );
    Ok(())
}

/// The same registry change on a model that takes tool changes inside a
/// message (INT-01/WP-06, D15): the `tools` array keeps its first-sent bytes,
/// the change rides in the notice, and every earlier thinking block stays
/// valid under the production binding rule.
#[tokio::test]
async fn an_in_message_tool_change_keeps_the_array_and_earlier_thinking() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let project = tempfile::tempdir()?;
    std::fs::write(project.path().join("notes.txt"), "WP-06 fixture notes\n")?;
    let provider = ThinkingProvider {
        inline: true,
        ..Default::default()
    };
    let caps = provider.caps();
    let mut agent = thinking_agent(&provider, project.path()).await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    provider.script(&[Reply::ThinkThenRead, Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("first task", Vec::new(), None, tx.clone())
        .await?;
    let (_, frozen_tools, _) = provider.request(1);

    let since = std::time::Instant::now();
    agent
        .registry
        .register(
            "mcp__fixture__probe".to_string(),
            Arc::new(FixtureTool {
                name: "mcp__fixture__probe",
            }),
        )
        .await;
    provider.script(&[Reply::ThinkThenAnswer, Reply::ThinkThenAnswer]);
    agent
        .run_once_streaming_mpsc("second task", Vec::new(), None, tx.clone())
        .await?;
    let (third, tools, system) = provider.request(2);
    assert_eq!(
        serde_json::to_value(&tools)?,
        serde_json::to_value(&frozen_tools)?,
        "the array is as first advertised"
    );
    let changes: Vec<_> = third
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::OperatorNotice { tool_changes, .. } => Some(tool_changes.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(matches!(
        changes.as_slice(),
        [jcode_message_types::ToolSetChange::Added { definition }]
            if definition.name == "mcp__fixture__probe"
    ));
    assert_eq!(
        replayed_signatures(&third),
        vec!["signature-0", "signature-1"]
    );
    assert!(
        analyze_request(&fixture_request_for(&third, &tools, &system, caps))
            .invalid()
            .next()
            .is_none()
    );
    assert!(
        agent
            .session
            .context_view
            .active_reasoning_invalidation()
            .is_none()
    );
    assert!(
        !crate::cache_invalidation::recorded_since(since)
            .iter()
            .any(|entry| entry.source == crate::tool::TOOL_SET_TRANSITION),
        "no tool-set transition: the array did not change"
    );

    // The thinking produced after the notice is bound to it and replays.
    agent
        .run_once_streaming_mpsc("third task", Vec::new(), None, tx)
        .await?;
    let (fourth, tools, system) = provider.request(3);
    assert_eq!(
        replayed_signatures(&fourth),
        vec!["signature-0", "signature-1", "signature-2"]
    );
    assert!(
        analyze_request(&fixture_request_for(&fourth, &tools, &system, caps))
            .invalid()
            .next()
            .is_none()
    );
    assert_eq!(
        crate::tool::tool_set_notice_count(&agent.session.messages),
        1
    );
    Ok(())
}
