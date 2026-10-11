//! The stop protocol through both real turn loops, driven by scripted
//! provider events: implicit and declared completion, corrections and their
//! counters, the tool gate, nonvoluntary ends, a restart in the middle of a
//! correction, the session tool surface and its frozen guidance. Text is
//! synthetic or rendered from the managed resources at test time; no test
//! judges wording.
use super::*;
use crate::instruction::notification::Notification;
use crate::provider::EventStream;
use crate::session_work::SessionWorkStore;
use jcode_session_types::ContextDeliveryChannel;
use jcode_session_work_types::{
    DeclarationState, ExhaustedCounter, FailureClass, SessionWorkRole, TurnOutcome,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

enum Reply {
    Events(Vec<StreamEvent>),
    /// The provider rejects the request.
    Fail,
    /// The turn is interrupted while this request is open, then the events
    /// arrive.
    Interrupt(jcode_tool_types::StopCause, Vec<StreamEvent>),
}

#[derive(Clone, Default)]
struct Script {
    replies: Arc<StdMutex<VecDeque<Reply>>>,
    requests: Arc<StdMutex<Vec<(Vec<Message>, Vec<ToolDefinition>)>>>,
    signal: Arc<StdMutex<Option<jcode_agent_runtime::InterruptSignal>>>,
    /// Render operator deliveries natively, like a runtime with an operator
    /// channel.
    operator: bool,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: Arc::new(StdMutex::new(replies.into())),
            ..Self::default()
        }
    }
    fn push(&self, replies: Vec<Reply>) {
        self.replies.lock().unwrap().extend(replies);
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn tools(&self, index: usize) -> Vec<ToolDefinition> {
        self.requests.lock().unwrap()[index].1.clone()
    }
    fn messages(&self, index: usize) -> Vec<Message> {
        self.requests.lock().unwrap()[index].0.clone()
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.requests
            .lock()
            .unwrap()
            .push((messages.to_vec(), tools.to_vec()));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Reply::Events(text("unexpected extra request")));
        let events = match reply {
            Reply::Events(events) => events,
            Reply::Fail => anyhow::bail!("synthetic provider rejection"),
            Reply::Interrupt(cause, events) => {
                if let Some(signal) = self.signal.lock().unwrap().as_ref() {
                    signal.fire_with_cause(cause);
                }
                events
            }
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
    fn name(&self) -> &str {
        "stop-protocol-script"
    }
    fn renders_operator_notices(&self) -> bool {
        self.operator
    }
    fn model(&self) -> String {
        "stop-protocol-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn text(body: &str) -> Vec<StreamEvent> {
    text_with(body, "end_turn")
}

fn text_with(body: &str, stop_reason: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(body.into()),
        StreamEvent::MessageEnd {
            stop_reason: Some(stop_reason.into()),
        },
    ]
}

fn call(id: &str, name: &str, mut input: serde_json::Value) -> Vec<StreamEvent> {
    input["intent"] = json!("synthetic call");
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: name.into(),
        },
        StreamEvent::ToolInputDelta(input.to_string()),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".into()),
        },
    ]
}

fn declare(id: &str, input: serde_json::Value) -> Reply {
    Reply::Events(call(id, "declare_stop", input))
}

fn say(body: &str) -> Reply {
    Reply::Events(text(body))
}

#[derive(Clone, Copy, Debug)]
enum Loop {
    Streaming,
    Blocking,
}

const LOOPS: [Loop; 2] = [Loop::Streaming, Loop::Blocking];

/// Fields drop in order: the feature and variables are restored before the
/// directories go, and the environment lock is released last.
struct Env {
    _feature: crate::config::feature_override::ScopedFeatureOverride,
    _vars: Vec<super::tests::AgentTestEnvRestore>,
    project: tempfile::TempDir,
    home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let vars = vec![
            super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path()),
            super::tests::AgentTestEnvRestore::set_path(
                "JCODE_RUNTIME_DIR",
                &home.path().join("runtime"),
            ),
        ];
        Self {
            _lock: lock,
            project: tempfile::tempdir().unwrap(),
            home,
            _vars: vars,
            _feature: crate::config::feature_override::ScopedFeatureOverride::session_work(true),
        }
    }

    fn session(&self, kind: crate::session_work::NewSessionWork<'_>) -> crate::session::Session {
        let mut session = crate::session::Session::create(None, None);
        session.working_dir = Some(self.project.path().to_string_lossy().into_owned());
        assert!(
            crate::session_work::activate_new_session(
                &mut session,
                kind,
                &crate::instruction::InstructionRepositoryService::new(),
            )
            .unwrap()
        );
        session
    }

    fn workflow(&self, session: &str, text: &str) {
        crate::session_work::SessionWorkSurface::new()
            .unwrap()
            .write_workflow(
                session,
                &uuid::Uuid::new_v4().to_string(),
                text,
                chrono::Utc::now(),
            )
            .unwrap();
    }

    fn file(&self, name: &str) -> String {
        let path = self.project.path().join(name);
        std::fs::write(&path, "synthetic file\n").unwrap();
        path.to_string_lossy().into_owned()
    }
}

async fn agent(script: &Script, session: crate::session::Session) -> Agent {
    let shared: Arc<dyn Provider> = Arc::new(script.clone());
    let registry = Registry::new(Arc::clone(&shared)).await;
    let agent = Agent::new_with_session(shared, registry, session, None);
    *script.signal.lock().unwrap() = Some(agent.graceful_shutdown_signal());
    agent
}

async fn turn(agent: &mut Agent, mode: Loop, prompt: &str) -> Result<()> {
    match mode {
        Loop::Streaming => {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            agent
                .run_once_streaming_mpsc(prompt, Vec::new(), None, tx)
                .await
        }
        Loop::Blocking => agent.run_once(prompt).await,
    }
}

fn outcomes(session: &str) -> Vec<TurnOutcome> {
    SessionWorkStore::new()
        .turn_outcomes(session)
        .unwrap()
        .into_iter()
        .map(|(_, outcome)| outcome)
        .collect()
}

fn corrections(agent: &Agent) -> Vec<String> {
    agent
        .session
        .messages
        .iter()
        .filter_map(|message| {
            message
                .context_delivery()
                .filter(|(channel, _)| *channel == ContextDeliveryChannel::StopCorrection)
                .map(|(_, body)| body.to_string())
        })
        .collect()
}

/// Every tool result in the stored transcript, by tool call.
fn results(agent: &Agent) -> Vec<(String, String, bool)> {
    agent
        .session
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((
                tool_use_id.clone(),
                content.clone(),
                is_error.unwrap_or(false),
            )),
            _ => None,
        })
        .collect()
}

/// A tool's own output: without the error prefix and the retained-output
/// footer the execution layer adds to every result.
fn result_of(agent: &Agent, id: &str) -> (String, bool) {
    let (content, error) = results(agent)
        .into_iter()
        .find(|(call, _, _)| call == id)
        .map(|(_, content, error)| (content, error))
        .unwrap_or_else(|| panic!("no result for {id}"));
    let body = content
        .split("\n[Retained output:")
        .next()
        .unwrap_or_default();
    let body = body.strip_prefix("Error: ").unwrap_or(body);
    (body.to_string(), error)
}

/// Every tool use in the transcript is answered by a result.
fn every_call_answered(agent: &Agent) {
    let answered: Vec<String> = results(agent).into_iter().map(|(id, _, _)| id).collect();
    for block in agent.session.messages.iter().flat_map(|m| m.content.iter()) {
        if let ContentBlock::ToolUse { id, .. } = block {
            assert!(answered.contains(id), "tool call {id} has no result");
        }
    }
}

/// Each request extends the previous one: earlier content is never rewritten.
fn append_only(script: &Script) {
    for index in 1..script.count() {
        let before = serde_json::to_value(script.messages(index - 1)).unwrap();
        let after = serde_json::to_value(script.messages(index)).unwrap();
        let (before, after) = (before.as_array().unwrap(), after.as_array().unwrap());
        assert!(after.len() > before.len(), "request {index} did not grow");
        // A request's newest user message may carry a timestamp the next one
        // keeps; compare everything but the volatile last message.
        assert_eq!(
            &after[..before.len() - 1],
            &before[..before.len() - 1],
            "request {index} rewrote earlier content"
        );
        assert_eq!(
            serde_json::to_string(&script.tools(index)).unwrap(),
            serde_json::to_string(&script.tools(index - 1)).unwrap(),
            "request {index} changed the tool set"
        );
    }
}

fn rendered(notification: Notification<'_>) -> String {
    notification.render(None).unwrap()
}

#[tokio::test]
async fn without_a_workflow_a_turn_completes_implicitly() -> Result<()> {
    for mode in LOOPS {
        let env = Env::new();
        let session = env.session(crate::session_work::NewSessionWork::Primary);
        let id = session.id.clone();
        let script = Script::new(vec![say("hello")]);
        let mut agent = agent(&script, session).await;
        turn(&mut agent, mode, "hi").await?;
        assert_eq!(script.count(), 1, "{mode:?}");
        assert_eq!(outcomes(&id), [TurnOutcome::Completed { declared: false }]);
        assert!(corrections(&agent).is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn an_undeclared_stop_is_corrected_until_its_counter_runs_out() -> Result<()> {
    for mode in LOOPS {
        let env = Env::new();
        let session = env.session(crate::session_work::NewSessionWork::Primary);
        let id = session.id.clone();
        env.workflow(&id, "- [>] build: Build it\n- [ ] check: Check it\n");
        let script = Script::new(vec![say("one"), say("two"), say("three")]);
        let mut agent = agent(&script, session).await;
        turn(&mut agent, mode, "go").await?;
        assert_eq!(script.count(), 3, "{mode:?}: two corrections, then the end");
        let expected = rendered(Notification::SessionWorkCorrectionMissing {
            modules: "build (active), check (pending)",
        });
        assert_eq!(corrections(&agent), [expected.clone(), expected]);
        // Corrections ask for operator authority and are appended.
        assert!(
            agent
                .session
                .messages
                .iter()
                .filter(|message| message
                    .context_delivery()
                    .is_some_and(|(channel, _)| channel == ContextDeliveryChannel::StopCorrection))
                .all(|message| message.operator_delivery().is_some())
        );
        append_only(&script);
        assert_eq!(
            outcomes(&id),
            [TurnOutcome::StoppedWithoutDeclaration {
                cause: ExhaustedCounter::Missing
            }]
        );
        let store = SessionWorkStore::new();
        assert!(store.open_turn(&id)?.is_none(), "the turn is settled");
    }
    Ok(())
}

#[tokio::test]
async fn a_declared_completion_takes_the_final_message_and_later_turns_end_implicitly() -> Result<()>
{
    for mode in LOOPS {
        let env = Env::new();
        let session = env.session(crate::session_work::NewSessionWork::Primary);
        let id = session.id.clone();
        env.workflow(&id, "- [x] build: Build it\n");
        let script = Script::new(vec![
            declare("d1", json!({"action": "complete", "summary": "Built"})),
            say("All built."),
        ]);
        let mut agent = agent(&script, session).await;
        turn(&mut agent, mode, "go").await?;
        assert_eq!(script.count(), 2, "{mode:?}");
        assert_eq!(
            result_of(&agent, "d1"),
            (
                rendered(Notification::SessionWorkStopCompleteDeclared),
                false
            )
        );
        let stop = SessionWorkStore::new().latest_completion(&id)?.unwrap();
        assert_eq!(stop.final_text.as_deref(), Some("All built."));
        assert_eq!(stop.workflow_revision, Some(1));
        // A follow-up question needs no declaration.
        script.push(vec![say("Because.")]);
        turn(&mut agent, mode, "why?").await?;
        assert_eq!(
            outcomes(&id),
            [
                TurnOutcome::Completed { declared: true },
                TurnOutcome::Completed { declared: false }
            ]
        );
        assert!(corrections(&agent).is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn completion_with_an_unfinished_workflow_is_refused_at_the_call() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    let id = session.id.clone();
    env.workflow(&id, "- [x] a: A\n- [?] b: B\n");
    let script = Script::new(vec![
        declare("d1", json!({"action": "complete"})),
        declare(
            "d2",
            json!({"action": "wait", "condition": "t9", "summary": "x"}),
        ),
        declare(
            "d3",
            json!({"action": "wait", "condition": "any(t1", "summary": "x"}),
        ),
        say("stopping"),
        say("stopping"),
        say("stopping"),
    ]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "go").await?;
    let (refused, error) = result_of(&agent, "d1");
    assert!(error);
    assert_eq!(
        refused,
        rendered(Notification::SessionWorkStopRefusedWorkflow {
            modules: "b (undetermined)"
        })
    );
    assert!(result_of(&agent, "d2").1 && result_of(&agent, "d3").1);
    assert!(SessionWorkStore::new().stops(&id)?.is_empty());
    every_call_answered(&agent);
    Ok(())
}

#[tokio::test]
async fn the_gate_warns_once_then_refuses_and_batch_never_runs() -> Result<()> {
    for mode in LOOPS {
        let env = Env::new();
        let session = env.session(crate::session_work::NewSessionWork::Primary);
        let id = session.id.clone();
        env.workflow(&id, "- [x] a: A\n");
        let file = env.file("notes.txt");
        let written = env.project.path().join("batch-wrote-this.txt");
        let script = Script::new(vec![
            declare("d1", json!({"action": "complete"})),
            Reply::Events(call("r1", "read", json!({"file_path": file}))),
            Reply::Events(call("r2", "read", json!({"file_path": file}))),
            Reply::Events(call(
                "b1",
                "batch",
                json!({"tool_calls": [{"tool": "write", "file_path": written, "content": "x"}]}),
            )),
            Reply::Events(call("r3", "read", json!({"file_path": file}))),
        ]);
        let mut agent = agent(&script, session).await;
        turn(&mut agent, mode, "go").await?;
        assert_eq!(
            script.count(),
            5,
            "{mode:?}: the third refusal ends the turn"
        );
        let warned = rendered(Notification::SessionWorkGateWarned);
        let (first, error) = result_of(&agent, "r1");
        assert!(!error && first.ends_with(&warned), "{mode:?}: {first}");
        assert_eq!(
            result_of(&agent, "r2"),
            (rendered(Notification::SessionWorkGateRefused), true)
        );
        assert_eq!(
            result_of(&agent, "b1"),
            (rendered(Notification::SessionWorkGateRefusedBatch), true)
        );
        assert!(!written.exists(), "no part of a refused batch runs");
        assert_eq!(
            result_of(&agent, "r3"),
            (rendered(Notification::SessionWorkGateRefused), true)
        );
        every_call_answered(&agent);
        // The declaration stays and still holds, so the session is complete.
        assert_eq!(outcomes(&id), [TurnOutcome::Completed { declared: true }]);
        let stop = SessionWorkStore::new().latest_completion(&id)?.unwrap();
        assert_eq!(stop.state, DeclarationState::Effective);
    }
    Ok(())
}

#[tokio::test]
async fn withdraw_returns_the_warned_call_and_a_later_stop_needs_a_declaration() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    let id = session.id.clone();
    env.workflow(&id, "- [x] a: A\n");
    let file = env.file("notes.txt");
    let script = Script::new(vec![
        declare("d1", json!({"action": "complete"})),
        Reply::Events(call("r1", "read", json!({"file_path": file}))),
        declare("w1", json!({"action": "withdraw"})),
        Reply::Events(call("r2", "read", json!({"file_path": file}))),
        Reply::Events(call("r3", "read", json!({"file_path": file}))),
        say("done for now"),
        say("still done"),
        say("done"),
    ]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Blocking, "go").await?;
    assert_eq!(
        result_of(&agent, "w1"),
        (rendered(Notification::SessionWorkStopWithdrawn), false)
    );
    assert!(!result_of(&agent, "r2").1 && !result_of(&agent, "r3").1);
    // The workflow is finished but was never declared complete: corrections
    // until the missing counter runs out.
    assert_eq!(script.count(), 8);
    assert_eq!(corrections(&agent).len(), 2);
    assert_eq!(
        outcomes(&id),
        [TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Missing
        }]
    );
    Ok(())
}

#[tokio::test]
async fn a_declaration_that_stops_holding_is_corrected_and_the_agent_recovers() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    let id = session.id.clone();
    env.workflow(&id, "- [x] a: A\n");
    let workflow = crate::session_work::SessionWorkSurface::new()?.workflow_path(&id)?;
    let workflow = workflow.to_string_lossy().into_owned();
    let script = Script::new(vec![
        declare("d1", json!({"action": "complete"})),
        // The warned call reopens the workflow before the final message.
        Reply::Events(call(
            "w1",
            "write",
            json!({"file_path": workflow, "content": "- [x] a: A\n- [ ] b: B\n"}),
        )),
        say("done"),
        Reply::Events(call(
            "w2",
            "write",
            json!({"file_path": workflow, "content": "- [x] a: A\n- [x] b: B\n"}),
        )),
        declare("d2", json!({"action": "complete"})),
        say("really done"),
    ]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "go").await?;
    assert_eq!(
        corrections(&agent),
        [rendered(Notification::SessionWorkStopRefusedWorkflow {
            modules: "b (pending)"
        })]
    );
    let stops = SessionWorkStore::new().stops(&id)?;
    assert!(matches!(
        stops[0].state,
        DeclarationState::Invalidated { .. }
    ));
    assert_eq!(stops[1].state, DeclarationState::Effective);
    assert_eq!(outcomes(&id), [TurnOutcome::Completed { declared: true }]);
    Ok(())
}

#[tokio::test]
async fn nonvoluntary_ends_are_recorded_and_never_corrected() -> Result<()> {
    for mode in LOOPS {
        let env = Env::new();
        let session = env.session(crate::session_work::NewSessionWork::Primary);
        let id = session.id.clone();
        env.workflow(&id, "- [>] a: A\n");
        let script = Script::new(vec![
            Reply::Events(text_with("I can't help with that.", "refusal")),
            Reply::Fail,
            Reply::Interrupt(
                jcode_tool_types::StopCause::HumanCancellation,
                text("partial"),
            ),
        ]);
        let mut agent = agent(&script, session).await;
        turn(&mut agent, mode, "one").await?;
        assert!(turn(&mut agent, mode, "two").await.is_err());
        turn(&mut agent, mode, "three").await?;
        agent.graceful_shutdown_signal().reset();
        assert_eq!(script.count(), 3, "{mode:?}");
        assert!(corrections(&agent).is_empty(), "{mode:?}");
        assert_eq!(
            outcomes(&id),
            [
                TurnOutcome::ProviderRefusal,
                TurnOutcome::Failed {
                    class: FailureClass::Provider
                },
                TurnOutcome::InterruptedByUser,
            ],
            "{mode:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn stop_revokes_a_declaration_made_in_its_turn_and_a_failure_keeps_one() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    let id = session.id.clone();
    env.workflow(&id, "- [x] a: A\n");
    let script = Script::new(vec![
        declare("d1", json!({"action": "complete"})),
        Reply::Interrupt(jcode_tool_types::StopCause::HumanCancellation, text("x")),
        declare("d2", json!({"action": "complete"})),
        Reply::Fail,
    ]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "one").await?;
    agent.graceful_shutdown_signal().reset();
    let store = SessionWorkStore::new();
    assert_eq!(
        store.stops(&id)?[0].state,
        DeclarationState::Withdrawn {
            cause: jcode_session_work_types::WithdrawCause::Revoked
        }
    );
    assert!(turn(&mut agent, Loop::Streaming, "two").await.is_err());
    assert_eq!(store.stops(&id)?[1].state, DeclarationState::Effective);
    assert_eq!(
        outcomes(&id),
        [
            TurnOutcome::InterruptedByUser,
            TurnOutcome::Failed {
                class: FailureClass::Provider
            }
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_restart_in_the_middle_of_a_correction_keeps_its_counter() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    let id = session.id.clone();
    env.workflow(&id, "- [>] a: A\n");
    let script = Script::new(vec![
        say("one"),
        Reply::Interrupt(jcode_tool_types::StopCause::ReloadQuiescence, text("two")),
    ]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "go").await?;
    assert_eq!(corrections(&agent).len(), 1);
    assert_eq!(outcomes(&id), [TurnOutcome::InterruptedByReload]);
    let (turn_number, counters) = SessionWorkStore::new().open_turn(&id)?.unwrap();
    assert_eq!(counters.corrections(ExhaustedCounter::Missing), 1);
    drop(agent);

    // A new process restores the session and continues the same turn.
    let restored = crate::session::Session::load(&id)?;
    let script = Script::new(vec![say("three"), say("four")]);
    let mut agent = self::agent(&script, restored).await;
    turn(&mut agent, Loop::Streaming, "continue").await?;
    assert_eq!(script.count(), 2, "one correction was left");
    assert_eq!(corrections(&agent).len(), 2);
    let turns: Vec<u32> = SessionWorkStore::new()
        .turn_outcomes(&id)?
        .into_iter()
        .map(|(turn, _)| turn)
        .collect();
    assert_eq!(turns, [turn_number, turn_number]);
    assert_eq!(
        outcomes(&id)[1],
        TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Missing
        }
    );
    Ok(())
}

#[tokio::test]
async fn activated_sessions_get_the_tool_in_their_role_with_frozen_guidance() -> Result<()> {
    let env = Env::new();
    let primary = env.session(crate::session_work::NewSessionWork::Primary);
    let script = Script::new(vec![say("a"), say("b")]);
    let mut agent = agent(&script, primary).await;
    turn(&mut agent, Loop::Streaming, "one").await?;
    let tools = script.tools(0);
    let tool = |name: &str| tools.iter().find(|tool| tool.name == name).cloned();
    let declare = tool("declare_stop").expect("primaries get declare_stop");
    let composer = crate::instruction::SystemPromptComposer::new();
    let guidance = composer.declare_stop_tool_guidance(SessionWorkRole::Primary, None)?;
    assert_eq!(declare.description, guidance);
    let wake = rendered(Notification::SessionWorkBackgroundWake);
    for name in ["bash", "bg"] {
        let schema = tool(name).unwrap().input_schema;
        assert_eq!(schema["properties"]["wake"]["description"], json!(wake));
    }
    // Editing the managed source does not reach a session that froze it.
    let source = env.home.path().join("instructions/tools/declare-stop.md");
    let original = std::fs::read_to_string(&source)?;
    std::fs::write(
        &source,
        original.replace("Declare", "SYNTHETIC EDIT. Declare"),
    )?;
    turn(&mut agent, Loop::Streaming, "two").await?;
    assert_eq!(
        serde_json::to_string(&script.tools(1))?,
        serde_json::to_string(&tools)?
    );
    let saved = crate::session::Session::load(&agent.session.id)?;
    assert_eq!(
        saved
            .frozen_tool_guidance("declare_stop")
            .and_then(|guidance| guidance.description),
        Some(guidance)
    );

    // A child gets completion only.
    let child = env.session(crate::session_work::NewSessionWork::Child {
        preset: "fixture",
        template: None,
    });
    let script = Script::new(vec![say("c")]);
    let mut agent = self::agent(&script, child).await;
    turn(&mut agent, Loop::Streaming, "child").await?;
    let declare = script
        .tools(0)
        .into_iter()
        .find(|tool| tool.name == "declare_stop")
        .unwrap();
    assert_eq!(
        declare.input_schema["properties"]["action"]["enum"],
        json!(["complete"])
    );
    assert_eq!(
        declare.description,
        composer.declare_stop_tool_guidance(SessionWorkRole::Child, None)?
    );
    Ok(())
}

#[tokio::test]
async fn sessions_without_session_work_keep_their_tools_and_loops() -> Result<()> {
    let env = Env::new();
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(env.project.path().to_string_lossy().into_owned());
    let script = Script::new(vec![say("plain")]);
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "hi").await?;
    let tools = script.tools(0);
    assert!(tools.iter().all(|tool| tool.name != "declare_stop"));
    let bash = tools.iter().find(|tool| tool.name == "bash").unwrap();
    assert_ne!(
        bash.input_schema["properties"]["wake"]["description"],
        json!(rendered(Notification::SessionWorkBackgroundWake))
    );
    // Nothing of session work was created for it.
    assert!(!SessionWorkStore::new().exists()?);
    Ok(())
}

#[tokio::test]
async fn corrections_reach_an_operator_channel_as_operator_notices() -> Result<()> {
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    env.workflow(&session.id, "- [>] a: A\n");
    let mut script = Script::new(vec![say("one"), say("two"), say("three")]);
    script.operator = true;
    let mut agent = agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "go").await?;
    let expected = rendered(Notification::SessionWorkCorrectionMissing {
        modules: "a (active)",
    });
    let notices: Vec<String> = script
        .messages(2)
        .into_iter()
        .flat_map(|message| message.content)
        .filter_map(|block| match block {
            ContentBlock::OperatorNotice { body, .. } => Some(body),
            _ => None,
        })
        .filter(|body| body.trim() == expected.trim())
        .collect();
    assert_eq!(notices.len(), 2, "both corrections are operator notices");
    drop(agent);
    drop(env);
    // A runtime without an operator channel gets the stored text.
    let env = Env::new();
    let session = env.session(crate::session_work::NewSessionWork::Primary);
    env.workflow(&session.id, "- [>] a: A\n");
    let script = Script::new(vec![say("one"), say("two"), say("three")]);
    let mut agent = self::agent(&script, session).await;
    turn(&mut agent, Loop::Streaming, "go").await?;
    assert!(
        script
            .messages(2)
            .iter()
            .flat_map(|message| &message.content)
            .all(|block| !matches!(block, ContentBlock::OperatorNotice { .. }))
    );
    Ok(())
}
