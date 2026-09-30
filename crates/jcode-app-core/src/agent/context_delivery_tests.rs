//! INT-01/WP-03 acceptance: every request a scripted session sends is
//! append-only on both production request builders (R08, R09).
//!
//! A recording provider drives the real Agent turn loops through reminders,
//! a batch nudge, a safe-boundary input, a skill activation (the one declared
//! transition), a reload mid-turn and two reload resumes. Each recorded
//! request is then formatted exactly as the Anthropic and OpenAI runtimes
//! format it, and consecutive requests are compared.

use super::*;
use crate::provider::EventStream;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Read,
    Fail,
}

struct Recorded {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    system: String,
}

#[derive(Default)]
struct RecorderState {
    script: VecDeque<Reply>,
    requests: Vec<Recorded>,
    /// Accepted as a safe-boundary input while the request with this index
    /// is in flight.
    inject_during: Option<(usize, jcode_session_types::PrimaryInputEnvelope)>,
    next_tool_id: usize,
}

#[derive(Clone, Default)]
struct RecordingProvider {
    state: Arc<StdMutex<RecorderState>>,
}

impl RecordingProvider {
    fn script(&self, replies: &[Reply]) {
        self.state
            .lock()
            .unwrap()
            .script
            .extend(replies.iter().cloned());
    }

    fn record(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> Result<EventStream> {
        let mut state = self.state.lock().unwrap();
        let index = state.requests.len();
        state.requests.push(Recorded {
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            system: system.to_string(),
        });
        if let Some((at, _)) = &state.inject_during
            && *at == index
        {
            let (_, input) = state.inject_during.take().unwrap();
            crate::primary_input::PrimaryInputStore::current().accept(input)?;
        }
        let reply = state
            .script
            .pop_front()
            .unwrap_or_else(|| panic!("request {index} has no scripted reply"));
        let events = match reply {
            Reply::Text(text) => vec![
                StreamEvent::TextDelta(text.to_string()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ],
            Reply::Read => {
                state.next_tool_id += 1;
                vec![
                    StreamEvent::ToolUseStart {
                        id: format!("toolu_wp03_{}", state.next_tool_id),
                        name: "read".into(),
                    },
                    StreamEvent::ToolInputDelta(
                        json!({"file_path": "notes.txt", "intent": "read fixture"}).to_string(),
                    ),
                    StreamEvent::ToolUseEnd,
                    StreamEvent::MessageEnd {
                        stop_reason: Some("tool_use".into()),
                    },
                ]
            }
            Reply::Fail => anyhow::bail!("synthetic process exit before the provider replied"),
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.record(messages, tools, system)
    }

    fn name(&self) -> &str {
        "wp03-recording"
    }

    fn model(&self) -> String {
        "wp03-recording-model".into()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// One provider's view of a request: its top-level system, its tools and its
/// flattened (role, block) sequence. Consecutive same-role messages merge on
/// Anthropic, so blocks rather than messages are the append unit.
struct View {
    system: Value,
    tools: Value,
    blocks: Vec<Value>,
}

fn strip_cache_control(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip_cache_control);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_cache_control),
        _ => {}
    }
}

fn anthropic_view(request: &Recorded) -> View {
    let system = jcode_provider_anthropic::build_system_param(&request.system, true);
    let mut system = serde_json::to_value(system).unwrap();
    let mut tools =
        serde_json::to_value(jcode_provider_anthropic::format_tools(&request.tools)).unwrap();
    strip_cache_control(&mut system);
    strip_cache_control(&mut tools);
    let mut blocks = Vec::new();
    for message in jcode_provider_anthropic::format_messages(&request.messages) {
        let mut message = serde_json::to_value(message).unwrap();
        strip_cache_control(&mut message);
        let role = message["role"].clone();
        for block in message["content"].as_array().unwrap() {
            blocks.push(json!({"role": role, "block": block}));
        }
    }
    View {
        system,
        tools,
        blocks,
    }
}

fn openai_view(request: &Recorded) -> View {
    View {
        system: Value::String(request.system.clone()),
        tools: Value::Array(jcode_provider_openai::build_tools(&request.tools)),
        blocks: jcode_provider_openai::build_responses_input(&request.messages),
    }
}

/// Every consecutive pair must keep system and tools byte-identical (except
/// across the declared transitions) and extend the block sequence.
fn append_only_failures(label: &str, views: &[View], transitions: &[usize]) -> Vec<String> {
    let mut failures = Vec::new();
    for (index, pair) in views.windows(2).enumerate() {
        let current = index + 1;
        let (before, after) = (&pair[0], &pair[1]);
        if before.system != after.system && !transitions.contains(&current) {
            failures.push(format!("{label}: system changed at request {current}"));
        }
        if before.tools != after.tools {
            failures.push(format!("{label}: tools changed at request {current}"));
        }
        let first_difference = before
            .blocks
            .iter()
            .zip(&after.blocks)
            .position(|(left, right)| left != right);
        if let Some(position) = first_difference {
            failures.push(format!(
                "{label}: request {current} rewrote history block {position} of {}",
                before.blocks.len()
            ));
        } else if after.blocks.len() < before.blocks.len() {
            failures.push(format!(
                "{label}: request {current} removed {} trailing history block(s)",
                before.blocks.len() - after.blocks.len()
            ));
        }
    }
    failures
}

/// Deliveries of `text` on `channel`, recognized structurally.
fn delivered_count(
    session: &crate::session::Session,
    channel: jcode_session_types::ContextDeliveryChannel,
    text: &str,
) -> usize {
    session
        .messages
        .iter()
        .filter_map(crate::session::StoredMessage::context_delivery)
        .filter(|(delivered, body)| *delivered == channel && body.contains(text))
        .count()
}

async fn fixture_agent(provider: &RecordingProvider, session: crate::session::Session) -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(provider.clone());
    let registry = Registry::new(Arc::clone(&provider)).await;
    Agent::new_with_session(provider, registry, session, None)
}

async fn streaming_turn(agent: &mut Agent, message: &str, reminder: Option<&str>) -> Result<()> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc(message, Vec::new(), reminder.map(str::to_string), tx)
        .await
}

#[tokio::test]
async fn scripted_session_is_append_only_on_both_production_builders() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let project = tempfile::tempdir()?;
    std::fs::write(project.path().join("notes.txt"), "WP-03 fixture notes\n")?;
    let skill_dir = project.path().join(".jcode/skills/wp03-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: wp03-skill\ndescription: Synthetic skill\n---\nWP03_SKILL_BODY\n",
    )?;

    let provider = RecordingProvider::default();
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(project.path().to_string_lossy().to_string());
    let session_id = session.id.clone();
    let mut agent = fixture_agent(&provider, session).await;
    let mut transitions = Vec::new();

    // Turn with a reminder and two tool rounds.
    provider.script(&[Reply::Read, Reply::Read, Reply::Text("one")]);
    streaming_turn(&mut agent, "first prompt", Some("REMINDER_ALPHA")).await?;
    // Turn without a reminder.
    provider.script(&[Reply::Read, Reply::Text("two")]);
    streaming_turn(&mut agent, "second prompt", None).await?;
    // Blocking loop: four sequential single-tool rounds trigger the batch nudge.
    provider.script(&[
        Reply::Read,
        Reply::Read,
        Reply::Read,
        Reply::Read,
        Reply::Read,
        Reply::Text("three"),
    ]);
    agent.run_once_capture("third prompt").await?;
    // A safe-boundary input with its own reminder arrives mid-turn.
    let mut background = jcode_session_types::PrimaryInputEnvelope::new(
        session_id.clone(),
        "BACKGROUND_TASK_NOTICE".into(),
        jcode_session_types::PrimaryInputDelivery::SafeBoundary,
    );
    background.display_role = Some(crate::session::StoredDisplayRole::BackgroundTask);
    background.system_reminder = Some("REMINDER_BACKGROUND".into());
    {
        let mut state = provider.state.lock().unwrap();
        state.inject_during = Some((state.requests.len(), background));
    }
    provider.script(&[Reply::Read, Reply::Read, Reply::Text("four")]);
    streaming_turn(&mut agent, "fourth prompt", Some("REMINDER_BETA")).await?;
    // The one declared transition: a skill activation changes the static prompt
    // and is recorded as a documented cache invalidation.
    transitions.push(provider.state.lock().unwrap().requests.len());
    let before_activation = std::time::Instant::now();
    agent.activate_skill("wp03-skill")?;
    assert_eq!(
        crate::cache_invalidation::most_recent_since(before_activation).map(|entry| entry.source),
        Some("skill activation")
    );
    provider.script(&[Reply::Read, Reply::Text("five")]);
    streaming_turn(&mut agent, "fifth prompt", Some("REMINDER_BETA")).await?;
    // The process dies mid-turn after a tool result was persisted.
    provider.script(&[Reply::Read, Reply::Fail]);
    assert!(
        streaming_turn(&mut agent, "sixth prompt", None)
            .await
            .is_err()
    );
    agent.session.save()?;
    drop(agent);

    // Reload: a fresh Agent resumes from durable state.
    let mut agent = fixture_agent(&provider, crate::session::Session::load(&session_id)?).await;
    provider.script(&[Reply::Read, Reply::Text("resumed")]);
    streaming_turn(&mut agent, "", Some("REMINDER_CONTINUE")).await?;
    provider.script(&[Reply::Read, Reply::Read, Reply::Text("seven")]);
    streaming_turn(&mut agent, "seventh prompt", Some("REMINDER_ALPHA")).await?;
    agent.session.save()?;
    drop(agent);

    // A second, clean reload and resume with the same continuation text.
    let mut agent = fixture_agent(&provider, crate::session::Session::load(&session_id)?).await;
    provider.script(&[Reply::Text("resumed again")]);
    streaming_turn(&mut agent, "", Some("REMINDER_CONTINUE")).await?;
    provider.script(&[Reply::Read, Reply::Read, Reply::Read, Reply::Text("eight")]);
    streaming_turn(&mut agent, "eighth prompt", Some("REMINDER_GAMMA")).await?;
    provider.script(&[Reply::Read, Reply::Read, Reply::Text("nine")]);
    agent.run_once_capture("ninth prompt").await?;

    let state = provider.state.lock().unwrap();
    assert!(state.script.is_empty(), "every scripted reply was consumed");
    let requests = &state.requests;
    assert!(
        requests.len() >= 30,
        "scripted session sent {} requests",
        requests.len()
    );

    let mut failures = Vec::new();
    let anthropic: Vec<View> = requests.iter().map(anthropic_view).collect();
    let openai: Vec<View> = requests.iter().map(openai_view).collect();
    failures.extend(append_only_failures("anthropic", &anthropic, &transitions));
    failures.extend(append_only_failures("openai", &openai, &transitions));

    // The declared transition really changed the static prompt, on both.
    for (label, views) in [("anthropic", &anthropic), ("openai", &openai)] {
        let at = transitions[0];
        if views[at].system == views[at - 1].system
            || !views[at].system.to_string().contains("WP03_SKILL_BODY")
        {
            failures.push(format!(
                "{label}: skill activation did not reach the static prompt"
            ));
        }
    }

    // Each occurrence is delivered exactly once, persisted and structurally
    // identified.
    use jcode_session_types::ContextDeliveryChannel::{BatchNudge, TurnReminder};
    let durable = crate::session::Session::load(&session_id)?;
    for (channel, text, expected) in [
        (TurnReminder, "REMINDER_ALPHA", 2),
        (TurnReminder, "REMINDER_BETA", 2),
        (TurnReminder, "REMINDER_BACKGROUND", 1),
        (TurnReminder, "REMINDER_CONTINUE", 2),
        (TurnReminder, "REMINDER_GAMMA", 1),
        (BatchNudge, "", 1),
    ] {
        let delivered = delivered_count(&durable, channel, text);
        if delivered != expected {
            failures.push(format!(
                "{channel:?} {text} delivered {delivered} time(s), expected {expected}"
            ));
        }
    }
    // A reload resume has its continuation as its content: no empty prompt.
    let empty_prompts = durable
        .messages
        .iter()
        .filter(|message| {
            message.role == Role::User
                && message.content.iter().all(
                    |block| matches!(block, ContentBlock::Text { text, .. } if text.trim().is_empty()),
                )
        })
        .count();
    if empty_prompts != 0 {
        failures.push(format!("{empty_prompts} empty user prompt(s) were stored"));
    }
    assert!(
        failures.is_empty(),
        "append-only failures:\n{}",
        failures.join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn one_boundary_group_delivers_its_shared_reminder_once_after_its_inputs() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let provider = RecordingProvider::default();
    let mut agent = fixture_agent(&provider, crate::session::Session::create(None, None)).await;
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "earlier prompt".into(),
            cache_control: None,
        }],
    );
    agent.session.save()?;
    let store = crate::primary_input::PrimaryInputStore::current();
    for content in ["FIRST_NOTICE", "SECOND_NOTICE"] {
        let mut input = jcode_session_types::PrimaryInputEnvelope::new(
            agent.session_id().into(),
            content.into(),
            jcode_session_types::PrimaryInputDelivery::SafeBoundary,
        );
        input.display_role = Some(crate::session::StoredDisplayRole::BackgroundTask);
        input.system_reminder = Some("SHARED_REMINDER".into());
        store.accept(input)?;
    }
    assert_eq!(agent.inject_primary_inputs()?.len(), 2);

    let durable = crate::session::Session::load(agent.session_id())?;
    let tail: Vec<String> = durable.messages[durable.messages.len() - 3..]
        .iter()
        .map(|message| match message.context_delivery() {
            Some((_, body)) => format!("delivery:{body}"),
            None => crate::session::StoredMessage::content_preview(message),
        })
        .collect();
    assert_eq!(
        tail,
        [
            "FIRST_NOTICE".to_string(),
            "SECOND_NOTICE".to_string(),
            "delivery:# System Reminder\n\nSHARED_REMINDER".to_string(),
        ]
    );
    Ok(())
}
