//! Arrivals through the real turn loops: held input waits through a declared
//! stop and arrives first, oldest first, with its notes when an awaited item
//! ends the stop; Mirza's dispositions keep or revoke a stop; arrivals at a
//! safe point are delivered or held by the same rule in both loops; a held
//! urgent message does not interrupt. Content is synthetic; notes are
//! rendered from the managed resources at test time, and no test judges
//! wording.
use super::*;
use crate::instruction::notification::Notification;
use crate::primary_input::PrimaryInputStore;
use crate::provider::EventStream;
use crate::session_work::SessionWorkStore;
use jcode_session_types::{ContextDeliveryChannel, PrimaryInputDelivery, PrimaryInputEnvelope};
use jcode_session_work_types::{
    ArrivalClass, ArrivalRoute, DeclarationState, HumanDisposition, ItemAlias, TurnOutcome,
    WithdrawCause,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// One scripted provider reply. `arrive` inputs are accepted into the inbox
/// and decided while the request is open, as if a client submitted them
/// during the turn.
struct Reply {
    events: Vec<StreamEvent>,
    arrive: Vec<PrimaryInputEnvelope>,
}

#[derive(Clone, Default)]
struct Script {
    replies: Arc<StdMutex<VecDeque<Reply>>>,
    requests: Arc<StdMutex<Vec<Vec<Message>>>>,
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
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.requests.lock().unwrap().push(messages.to_vec());
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or(Reply {
            events: text("unexpected extra request"),
            arrive: Vec::new(),
        });
        for input in reply.arrive {
            let session = input.session.clone();
            PrimaryInputStore::current().accept(input.clone())?;
            if crate::session_work::delivery::decided_now(
                &input,
                crate::session_work::arrivals::Activity::Working,
            ) {
                crate::session_work::delivery::arrive(
                    &session,
                    &input,
                    crate::session_work::arrivals::Activity::Working,
                )?;
            }
        }
        Ok(Box::pin(futures::stream::iter(
            reply.events.into_iter().map(Ok),
        )))
    }
    fn name(&self) -> &str {
        "arrival-script"
    }
    fn model(&self) -> String {
        "arrival-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn text(body: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(body.into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]
}

fn say(body: &str) -> Reply {
    Reply {
        events: text(body),
        arrive: Vec::new(),
    }
}

fn say_while(body: &str, arrive: Vec<PrimaryInputEnvelope>) -> Reply {
    Reply {
        events: text(body),
        arrive,
    }
}

fn declare(id: &str, mut input: serde_json::Value) -> Reply {
    input["intent"] = json!("synthetic call");
    Reply {
        events: vec![
            StreamEvent::ToolUseStart {
                id: id.into(),
                name: "declare_stop".into(),
            },
            StreamEvent::ToolInputDelta(input.to_string()),
            StreamEvent::ToolUseEnd,
            StreamEvent::MessageEnd {
                stop_reason: Some("tool_use".into()),
            },
        ],
        arrive: Vec::new(),
    }
}

fn wait_on(id: &str, condition: &str) -> Reply {
    declare(
        id,
        json!({"action": "wait", "condition": condition, "summary": "synthetic wait"}),
    )
}

/// Fields drop in order: the feature and variables are restored before the
/// directories go, and the environment lock is released last.
struct Env {
    _feature: crate::config::feature_override::ScopedFeatureOverride,
    _vars: Vec<super::tests::AgentTestEnvRestore>,
    project: tempfile::TempDir,
    _home: tempfile::TempDir,
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
            _home: home,
            _vars: vars,
            _feature: crate::config::feature_override::ScopedFeatureOverride::session_work(true),
        }
    }

    fn session(&self) -> crate::session::Session {
        let mut session = crate::session::Session::create(None, None);
        session.working_dir = Some(self.project.path().to_string_lossy().into_owned());
        assert!(
            crate::session_work::activate_new_session(
                &mut session,
                crate::session_work::NewSessionWork::Primary,
                &crate::instruction::InstructionRepositoryService::new(),
            )
            .unwrap()
        );
        session.save().unwrap();
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
}

async fn agent(script: &Script, session: crate::session::Session) -> Agent {
    let shared: Arc<dyn Provider> = Arc::new(script.clone());
    let registry = Registry::new(Arc::clone(&shared)).await;
    Agent::new_with_session(shared, registry, session, None)
}

/// A message from Mirza, optionally with a disposition.
fn message(session: &str, body: &str, delivery: PrimaryInputDelivery) -> PrimaryInputEnvelope {
    let mut input = PrimaryInputEnvelope::new(session.into(), body.into(), delivery);
    input.origin = Some(jcode_session_types::StoredMessageOrigin::Human);
    input
}

/// A background task's result, as its completion delivers it.
fn finished(session: &str, task: &str, wake: bool) -> PrimaryInputEnvelope {
    let mut input = PrimaryInputEnvelope::new(
        session.into(),
        format!("synthetic result of {task}"),
        PrimaryInputDelivery::SafeBoundary,
    );
    input.display_role = Some(crate::session::StoredDisplayRole::BackgroundTask);
    input.arrival = Some(ArrivalClass::ItemFinished {
        item: crate::session_work::background::source(task, "bash", &format!("job {task}"), wake),
    });
    input
}

/// Allocate background task `task`'s item as its start would.
fn started(session: &str, task: &str, wake: bool) -> ItemAlias {
    crate::session_work::background::start(session, task, "bash", &format!("job {task}"), wake)
        .unwrap()
        .unwrap()
        .alias
}

/// Accept and decide an input while the session is idle, as a client
/// submission would.
fn submit_idle(input: &PrimaryInputEnvelope) -> jcode_session_work_types::ArrivalRecord {
    PrimaryInputStore::current().accept(input.clone()).unwrap();
    crate::session_work::delivery::arrive(
        &input.session,
        input,
        crate::session_work::arrivals::Activity::Idle,
    )
    .unwrap()
}

/// Run the input the idle delivery worker would start a turn with.
async fn wake(agent: &mut Agent) -> Result<Option<PrimaryInputEnvelope>> {
    let session = agent.session_id().to_string();
    let pending = PrimaryInputStore::current().pending(&session)?;
    let Some(next) = crate::server::idle_deliverable(&session, pending)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_primary_input(next.clone(), tx).await?;
    Ok(Some(next))
}

/// The model-visible text of the stored transcript after `from`, in order:
/// arrival notes and user content.
fn arrivals_after(agent: &Agent, from: usize) -> Vec<String> {
    agent.session.messages[from..]
        .iter()
        .filter_map(|message| {
            if let Some((channel, body)) = message.context_delivery() {
                return (channel == ContextDeliveryChannel::SessionWork).then(|| body.to_string());
            }
            if message.role != Role::User {
                return None;
            }
            message.content.iter().find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

fn clock(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

fn record(session: &str, input: &PrimaryInputEnvelope) -> jcode_session_work_types::ArrivalRecord {
    SessionWorkStore::new()
        .arrival(session, &input.id.to_string())
        .unwrap()
        .unwrap()
}

fn outcomes(session: &str) -> Vec<TurnOutcome> {
    SessionWorkStore::new()
        .turn_outcomes(session)
        .unwrap()
        .into_iter()
        .map(|(_, outcome)| outcome)
        .collect()
}

/// Accept an input and run the turn it starts, as the delivery worker does.
async fn run_input(agent: &mut Agent, input: PrimaryInputEnvelope) -> Result<()> {
    PrimaryInputStore::current().accept(input.clone())?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_primary_input(input, tx).await
}

/// Declare a wait on `condition` and end the turn on it.
async fn wait_for(agent: &mut Agent, script: &Script, condition: &str) -> Result<()> {
    script.push(vec![wait_on("w", condition), say("Waiting.")]);
    let session = agent.session_id().to_string();
    run_input(
        agent,
        message(&session, "go", PrimaryInputDelivery::NextTurn),
    )
    .await?;
    assert!(matches!(
        outcomes(&session).last(),
        Some(TurnOutcome::Waiting { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn held_arrivals_wait_through_a_stop_and_arrive_first_when_an_awaited_item_ends_it()
-> Result<()> {
    let env = Env::new();
    let session = env.session();
    let id = session.id.clone();
    env.workflow(&id, "- [>] build: Build it\n");
    let t1 = started(&id, "run-a", true);
    let t2 = started(&id, "run-b", true);
    let script = Script::new(Vec::new());
    let mut agent = agent(&script, session).await;
    wait_for(&mut agent, &script, &t1.to_string()).await?;
    let before = agent.session.messages.len();

    // Not awaited: held, with the held diagnostic on its receipt.
    let other = finished(&id, "run-b", true);
    let held = submit_idle(&other);
    assert!(held.is_held());
    let receipt = PrimaryInputStore::current().inspect(&id, other.id)?;
    assert_eq!(
        receipt.issue.as_deref(),
        Some(format!("Held: the session is waiting on {t1}.").as_str())
    );
    // Mirza's message queues by default.
    let note = message(
        &id,
        "synthetic note from Mirza",
        PrimaryInputDelivery::NextTurn,
    );
    assert!(submit_idle(&note).is_held());
    // Nothing starts a turn while everything is held.
    assert!(wake(&mut agent).await?.is_none());
    assert_eq!(script.count(), 2);

    // The awaited item ends the stop and wakes the session.
    let awaited = finished(&id, "run-a", true);
    let ended = submit_idle(&awaited);
    assert_eq!(ended.route, ArrivalRoute::EndsStop { stop: 1 });
    script.push(vec![say("Back to work.")]);
    let woke = wake(&mut agent).await?.expect("the awaited arrival wakes");
    assert_eq!(woke.id, awaited.id);

    // Held input first, oldest first, each with its note; then the waking
    // input with the end of the stop.
    let held_at = record(&id, &other).arrived_at;
    let note_at = record(&id, &note).arrived_at;
    assert_eq!(
        arrivals_after(&agent, before),
        [
            Notification::SessionWorkHeldItemWaiting {
                alias: &t2.to_string(),
                label: "job run-b",
                event: "finished",
                time: &clock(held_at),
                condition: &t1.to_string(),
            }
            .render(None)?,
            other.content.clone(),
            Notification::SessionWorkHeldMessageWaiting {
                time: &clock(note_at),
                condition: &t1.to_string(),
            }
            .render(None)?,
            note.content.clone(),
            Notification::SessionWorkStopEnded {
                alias: &t1.to_string(),
                event: "finished",
            }
            .render(None)?,
            awaited.content.clone(),
        ]
    );
    // Every input is committed once; the held diagnostic is gone.
    for input in [&other, &note, &awaited] {
        let receipt = PrimaryInputStore::current().inspect(&id, input.id)?;
        assert_eq!(
            receipt.state,
            jcode_session_types::PrimaryInputState::Committed
        );
    }
    assert_eq!(
        SessionWorkStore::new().stops(&id)?[0].state,
        DeclarationState::Withdrawn {
            cause: WithdrawCause::AwaitedArrival
        }
    );
    assert!(PrimaryInputStore::current().pending(&id)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn send_and_keep_delivers_under_the_stop_and_revoke_and_send_withdraws_it() -> Result<()> {
    let env = Env::new();
    let session = env.session();
    let id = session.id.clone();
    env.workflow(&id, "- [>] build: Build it\n");
    let t1 = started(&id, "run-a", true);
    let script = Script::new(Vec::new());
    let mut agent = agent(&script, session).await;
    wait_for(&mut agent, &script, &t1.to_string()).await?;

    // Queued, then sent with the stop kept.
    let queued = message(&id, "synthetic question", PrimaryInputDelivery::NextTurn);
    assert!(submit_idle(&queued).is_held());
    let sent = SessionWorkStore::new().redispose(
        &id,
        &queued.id.to_string(),
        HumanDisposition::SendKeepStop,
        chrono::Utc::now(),
    )?;
    assert_eq!(sent.route, ArrivalRoute::KeepsStop { stop: 1 });
    PrimaryInputStore::current().defer(&id, queued.id, None)?;
    let before = agent.session.messages.len();
    script.push(vec![say("Answered in text.")]);
    let woke = wake(&mut agent)
        .await?
        .expect("a kept-stop message is delivered");
    assert_eq!(woke.id, queued.id);
    assert_eq!(
        arrivals_after(&agent, before),
        [
            Notification::SessionWorkMessageKeptStop.render(None)?,
            queued.content.clone()
        ]
    );
    // The stop is still in place.
    assert_eq!(
        SessionWorkStore::new().current_stop(&id)?.unwrap().number,
        1
    );

    // Revoke and send, decided as it arrives.
    let mut revoking = message(
        &id,
        "synthetic change of plan",
        PrimaryInputDelivery::NextTurn,
    );
    revoking.arrival = Some(ArrivalClass::Human {
        disposition: HumanDisposition::RevokeAndSend,
    });
    assert_eq!(
        submit_idle(&revoking).route,
        ArrivalRoute::RevokesStop { stop: 1 }
    );
    assert!(SessionWorkStore::new().current_stop(&id)?.is_none());
    assert_eq!(
        SessionWorkStore::new().stops(&id)?[0].state,
        DeclarationState::Withdrawn {
            cause: WithdrawCause::Revoked
        }
    );
    let before = agent.session.messages.len();
    script.push(vec![say("Changing course.")]);
    wake(&mut agent)
        .await?
        .expect("revoke and send is delivered");
    assert_eq!(
        arrivals_after(&agent, before),
        [
            Notification::SessionWorkMessageRevokedStop.render(None)?,
            revoking.content.clone()
        ]
    );
    Ok(())
}

#[tokio::test]
async fn at_a_safe_point_arrivals_are_delivered_while_working_and_held_under_a_declaration()
-> Result<()> {
    for blocking in [false, true] {
        let env = Env::new();
        let session = env.session();
        let id = session.id.clone();
        env.workflow(&id, "- [>] build: Build it\n");
        let t1 = started(&id, "run-a", true);
        started(&id, "run-b", true);
        // While working, a finished background task joins the turn at its
        // next safe point.
        let working = finished(&id, "run-b", true);
        // During the declaring turn, another arrives: it is held.
        let declaring = message(&id, "synthetic aside", PrimaryInputDelivery::SafeBoundary);
        let script = Script::new(vec![
            say_while("Working.", vec![working.clone()]),
            wait_on("w", &t1.to_string()),
            say_while("Waiting.", vec![declaring.clone()]),
        ]);
        let mut agent = agent(&script, session).await;
        if blocking {
            agent.run_once("go").await?;
        } else {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            agent
                .run_once_streaming_mpsc("go", Vec::new(), None, tx)
                .await?;
        }
        assert_eq!(script.count(), 3, "blocking={blocking}");
        assert_eq!(record(&id, &working).route, ArrivalRoute::Deliver);
        assert_eq!(
            PrimaryInputStore::current().inspect(&id, working.id)?.state,
            jcode_session_types::PrimaryInputState::Committed
        );
        assert!(record(&id, &declaring).is_held(), "blocking={blocking}");
        assert_eq!(
            PrimaryInputStore::current()
                .inspect(&id, declaring.id)?
                .state,
            jcode_session_types::PrimaryInputState::Accepted
        );
        assert!(matches!(
            outcomes(&id).last(),
            Some(TurnOutcome::Waiting { .. })
        ));
    }
    Ok(())
}

#[tokio::test]
async fn a_held_urgent_message_does_not_interrupt_and_hold_policy_waits_for_the_next_turn()
-> Result<()> {
    let env = Env::new();
    let session = env.session();
    let id = session.id.clone();
    env.workflow(&id, "- [x] build: Build it\n");
    let script = Script::new(vec![
        declare("c", json!({"action": "complete", "summary": "Built"})),
        say("Done."),
    ]);
    let mut agent = agent(&script, session).await;
    run_input(
        &mut agent,
        message(&id, "go", PrimaryInputDelivery::NextTurn),
    )
    .await?;
    assert_eq!(outcomes(&id), [TurnOutcome::Completed { declared: true }]);

    // After completion a hold-policy result is held for the next turn and a
    // wake-policy one wakes the session.
    let t1 = started(&id, "run-hold", false);
    let quiet = finished(&id, "run-hold", false);
    assert!(submit_idle(&quiet).is_held());
    assert!(wake(&mut agent).await?.is_none());

    // Mirza's message starts the next turn, and the held result arrives
    // first with its note.
    let next = message(&id, "next question", PrimaryInputDelivery::NextTurn);
    assert_eq!(submit_idle(&next).route, ArrivalRoute::Deliver);
    let before = agent.session.messages.len();
    script.push(vec![say("Answer.")]);
    wake(&mut agent).await?.expect("Mirza's message wakes");
    let quiet_at = record(&id, &quiet).arrived_at;
    assert_eq!(
        arrivals_after(&agent, before),
        [
            Notification::SessionWorkHeldItemStopped {
                alias: &t1.to_string(),
                label: "job run-hold",
                event: "finished",
                time: &clock(quiet_at),
            }
            .render(None)?,
            quiet.content.clone(),
            next.content.clone(),
        ]
    );

    // An urgent message queued under a declared stop does not interrupt.
    env.workflow(&id, "- [x] build: Build it\n- [>] more: More\n");
    let t2 = started(&id, "run-c", true);
    wait_for(&mut agent, &script, &t2.to_string()).await?;
    let mut urgent = message(&id, "urgent aside", PrimaryInputDelivery::SafeBoundary);
    urgent.urgent = true;
    assert!(submit_idle(&urgent).is_held());
    assert!(!agent.has_urgent_interrupt());
    Ok(())
}
