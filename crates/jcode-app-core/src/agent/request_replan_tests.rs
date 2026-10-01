//! INT-01/WP-06 R19 and R20 in the real Agent turn loops: a runtime that
//! hands a request back is answered with a newly planned request, and
//! reasoning the provider reports or rejects is suppressed, persisted, before
//! anything is sent again.

use super::*;
use crate::context::reasoning_invalidation_tests::{fixture_invalidations, fixture_request};
use crate::provider::{ContextReasoningBlockKind, EventStream, InvalidReplayedReasoning};
use jcode_message_types::{AnthropicThinkingBinding, ReplayableReasoningBlock};
use jcode_provider_anthropic::binding::{analyze_request, stored_thinking_fingerprint};
use jcode_provider_core::{ProviderRequestContext, ProviderRequestReplan};
use jcode_session_types::{StoredContextOperation, StoredReasoningInvalidationCause};
use std::sync::Mutex as StdMutex;

#[derive(Clone)]
enum Step {
    /// Signed thinking bound to the request, then an answer.
    Answer,
    /// As `Answer`, after reporting every replayed block from the given one
    /// on as dropped.
    DropFromThenAnswer(usize),
    /// The stream ends with this replan before any output.
    HandBack(Handback),
    /// `complete` itself returns this replan.
    RefuseToSend(Handback),
}

#[derive(Clone)]
enum Handback {
    /// The runtime moved to this model.
    ModelFallback(&'static str),
    /// The provider rejected every replayed block from the given one on.
    RejectFrom(usize),
    Invalid,
}

#[derive(Default)]
struct State {
    script: std::collections::VecDeque<Step>,
    /// Replayed thinking signatures and the model of each request.
    requests: Vec<(Vec<String>, String)>,
    model: String,
    caller_replans: Vec<bool>,
}

#[derive(Clone)]
struct ScriptedProvider {
    state: Arc<StdMutex<State>>,
}

impl ScriptedProvider {
    fn new(script: Vec<Step>) -> Self {
        Self {
            state: Arc::new(StdMutex::new(State {
                script: script.into(),
                model: "claude-sonnet-5-5".to_string(),
                ..State::default()
            })),
        }
    }

    fn requests(&self) -> Vec<(Vec<String>, String)> {
        self.state.lock().unwrap().requests.clone()
    }

    fn replan(&self, handback: &Handback, ids: &[String]) -> ProviderRequestReplan {
        match handback {
            Handback::ModelFallback(to) => {
                let mut state = self.state.lock().unwrap();
                let from = std::mem::replace(&mut state.model, to.to_string());
                ProviderRequestReplan::ModelFallback {
                    from,
                    to: to.to_string(),
                    cause: "unavailable".to_string(),
                }
            }
            Handback::RejectFrom(start) => ProviderRequestReplan::ReasoningRejected {
                block_ids: ids[*start..].to_vec(),
                reason: "prefix_binding_mismatch".to_string(),
            },
            Handback::Invalid => ProviderRequestReplan::ReplayedReasoningInvalid { blocks: 1 },
        }
    }
}

fn signatures(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::AnthropicThinking { signature, .. } => Some(signature.clone()),
            _ => None,
        })
        .collect()
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.complete_split_with_context(
            messages,
            tools,
            system,
            None,
            ProviderRequestContext::default(),
        )
        .await
    }

    async fn complete_split_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        _: Option<&str>,
        context: ProviderRequestContext,
    ) -> Result<EventStream> {
        let ids: Vec<String> = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(stored_thinking_fingerprint)
            .collect();
        let (step, index) = {
            let mut state = self.state.lock().unwrap();
            let index = state.requests.len();
            let model = state.model.clone();
            state.requests.push((signatures(messages), model));
            state.caller_replans.push(context.caller_replans);
            (
                state
                    .script
                    .pop_front()
                    .unwrap_or_else(|| panic!("request {index} has no scripted step")),
                index,
            )
        };
        let binding = analyze_request(&fixture_request(messages, tools, system)).binding;
        let answer = |predecessor: Option<String>| {
            vec![
                Ok(StreamEvent::ReplayableReasoning(
                    ReplayableReasoningBlock::AnthropicThinking {
                        thinking: format!("thinking {index}"),
                        signature: format!("signature-{index}"),
                        binding: AnthropicThinkingBinding {
                            model: "claude-sonnet-5-5".to_string(),
                            prefix_digest: binding.prefix_digest.clone(),
                            predecessor,
                        },
                    },
                )),
                Ok(StreamEvent::TextDelta("done".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ]
        };
        let events: Vec<Result<StreamEvent>> = match step {
            Step::Answer => answer(binding.last_thinking.clone()),
            Step::DropFromThenAnswer(start) => {
                let mut events = vec![Ok(StreamEvent::ProviderDroppedReasoning {
                    block_ids: ids[start..].to_vec(),
                    reason: "prefix_binding_mismatch".to_string(),
                })];
                // As the runtime chains it: to the last block the API kept.
                events.extend(answer(start.checked_sub(1).map(|kept| ids[kept].clone())));
                events
            }
            Step::HandBack(handback) => vec![Err(self.replan(&handback, &ids).into())],
            Step::RefuseToSend(handback) => return Err(self.replan(&handback, &ids).into()),
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> String {
        self.state.lock().unwrap().model.clone()
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

    fn replayed_reasoning_block_id(&self, block: &ContentBlock) -> Option<String> {
        stored_thinking_fingerprint(block)
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

struct Fixture {
    agent: Agent,
    provider: ScriptedProvider,
    /// Fields drop in order: the environment is restored, then the home
    /// removed, and only then the lock released.
    _guards: (
        super::tests::AgentTestEnvRestore,
        super::tests::AgentTestEnvRestore,
        tempfile::TempDir,
        std::sync::MutexGuard<'static, ()>,
    ),
}

async fn fixture(script: Vec<Step>) -> Result<Fixture> {
    let lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let home_guard = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let runtime_guard = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let provider = ScriptedProvider::new(script);
    let registry = Registry::new(Arc::new(provider.clone()) as Arc<dyn Provider>).await;
    let mut agent = Agent::new_with_session(
        Arc::new(provider.clone()) as Arc<dyn Provider>,
        registry,
        crate::session::Session::create(None, None),
        None,
    );
    agent.set_system_prompt("replan fixture prompt");
    Ok(Fixture {
        agent,
        provider,
        _guards: (runtime_guard, home_guard, home, lock),
    })
}

/// Run one turn through the streaming loop (`streaming`) or the blocking one.
async fn turn(agent: &mut Agent, streaming: bool, prompt: &str) -> Result<()> {
    if streaming {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        agent
            .run_once_streaming_mpsc(prompt, Vec::new(), None, tx)
            .await
    } else {
        agent.run_once_capture(prompt).await.map(|_| ())
    }
}

fn managed_causes(agent: &Agent) -> Vec<(usize, StoredReasoningInvalidationCause)> {
    let Some(managed) = agent.session.context_view.active_reasoning_invalidation() else {
        return Vec::new();
    };
    managed
        .operations
        .iter()
        .map(|operation| {
            let StoredContextOperation::ReasoningSuppression(suppression) = operation else {
                panic!("managed set holds suppressions");
            };
            let jcode_session_types::StoredReasoningSelection::Invalidated { cause } =
                &suppression.selection
            else {
                panic!("managed suppressions carry a cause");
            };
            (suppression.targets.len(), cause.clone())
        })
        .collect()
}

#[tokio::test]
async fn a_provider_reported_drop_is_persisted_and_not_sent_again() -> Result<()> {
    for streaming in [true, false] {
        let mut test = fixture(vec![
            Step::Answer,
            Step::Answer,
            // The third request replays two blocks; the provider drops both.
            Step::DropFromThenAnswer(0),
            Step::Answer,
        ])
        .await?;
        for prompt in ["one", "two", "three", "four"] {
            turn(&mut test.agent, streaming, prompt).await?;
        }
        let requests = test.provider.requests();
        assert_eq!(
            requests[2].0,
            vec!["signature-0".to_string(), "signature-1".to_string()]
        );
        assert_eq!(
            requests[3].0,
            vec!["signature-2".to_string()],
            "streaming {streaming}: the dropped run is suppressed before the next request"
        );
        assert_eq!(
            managed_causes(&test.agent),
            vec![(
                2,
                StoredReasoningInvalidationCause::ProviderReported {
                    reason: "prefix_binding_mismatch".to_string()
                }
            )]
        );
        let reloaded = crate::session::Session::load(&test.agent.session.id)?;
        assert_eq!(reloaded.context_view, test.agent.session.context_view);
        assert!(test.agent.provider_reported_reasoning.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn rejected_reasoning_is_suppressed_and_the_request_is_sent_once_more() -> Result<()> {
    for streaming in [true, false] {
        for refuse in [false, true] {
            let reject = Handback::RejectFrom(1);
            let mut test = fixture(vec![
                Step::Answer,
                Step::Answer,
                if refuse {
                    Step::RefuseToSend(reject)
                } else {
                    Step::HandBack(reject)
                },
                Step::Answer,
            ])
            .await?;
            for prompt in ["one", "two", "three"] {
                turn(&mut test.agent, streaming, prompt).await?;
            }
            let requests = test.provider.requests();
            assert_eq!(
                requests.len(),
                4,
                "the rejected request is planned again once"
            );
            assert_eq!(requests[2].0.len(), 2);
            assert_eq!(
                requests[3].0,
                vec!["signature-0".to_string()],
                "streaming {streaming} refuse {refuse}: the named run is gone, the block before it stays"
            );
            assert_eq!(
                managed_causes(&test.agent),
                vec![(
                    1,
                    StoredReasoningInvalidationCause::ProviderReported {
                        reason: "prefix_binding_mismatch".to_string()
                    }
                )]
            );
            assert!(
                test.provider
                    .state
                    .lock()
                    .unwrap()
                    .caller_replans
                    .iter()
                    .all(|replans| *replans),
                "the loops tell the runtime they plan again"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_model_fallback_is_adopted_recorded_and_planned_again() -> Result<()> {
    for streaming in [true, false] {
        let mut test = fixture(vec![
            Step::Answer,
            Step::HandBack(Handback::ModelFallback("claude-opus-5-5")),
            Step::Answer,
        ])
        .await?;
        turn(&mut test.agent, streaming, "one").await?;
        let since = std::time::Instant::now();
        turn(&mut test.agent, streaming, "two").await?;
        let requests = test.provider.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].1, "claude-sonnet-5-5");
        assert_eq!(requests[2].1, "claude-opus-5-5");
        assert_eq!(
            test.agent.session.model.as_deref(),
            Some("claude-opus-5-5"),
            "the session follows the runtime's model"
        );
        assert!(
            crate::cache_invalidation::recorded_since(since)
                .iter()
                .any(|record| record.source == "provider model fallback"),
            "streaming {streaming}: the fallback is a recorded provider-model transition"
        );
        assert_eq!(test.agent.provider_replans, 0);
    }
    Ok(())
}

#[tokio::test]
async fn a_request_handed_back_without_end_fails_instead_of_looping() -> Result<()> {
    let mut test = fixture(vec![Step::RefuseToSend(Handback::Invalid); 12]).await?;
    let error = turn(&mut test.agent, true, "one")
        .await
        .expect_err("the turn fails");
    assert!(
        ProviderRequestReplan::of(&error).is_some(),
        "the last replan is the error: {error:#}"
    );
    assert_eq!(test.provider.requests().len(), 5);
    Ok(())
}
