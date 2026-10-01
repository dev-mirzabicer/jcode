//! INT-01/WP-06 R22 (D15): a session's tool set across restarts, through the
//! real agent loop and both production tool builders.
//!
//! Each restart loads the persisted session into a fresh `Agent` with a fresh
//! registry, as a reloaded server does. The scenario runs once for a provider
//! whose `tools` array must carry changes (OpenAI, Sonnet 5) and once for one
//! that takes them inside a message (Opus 5.5).

use super::*;
use crate::provider::EventStream;
use jcode_message_types::ToolSetChange;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

#[derive(Clone)]
enum Reply {
    Text,
    Call(&'static str),
}

struct Recorded {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
}

#[derive(Default)]
struct RecorderState {
    script: VecDeque<Reply>,
    requests: Vec<Recorded>,
}

#[derive(Clone, Default)]
struct RecordingProvider {
    /// Whether the runtime takes tool changes inside a message.
    inline: bool,
    state: Arc<StdMutex<RecorderState>>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _system: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        let mut state = self.state.lock().unwrap();
        let index = state.requests.len();
        state.requests.push(Recorded {
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        });
        let reply = state.script.pop_front().unwrap_or(Reply::Text);
        let events = match reply {
            Reply::Text => vec![
                StreamEvent::TextDelta("ok".to_string()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ],
            Reply::Call(name) => vec![
                StreamEvent::ToolUseStart {
                    id: format!("toolu_wp06_{index}"),
                    name: name.into(),
                },
                StreamEvent::ToolInputDelta(json!({"intent": "fixture call"}).to_string()),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                },
            ],
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "wp06-recording"
    }

    fn model(&self) -> String {
        "wp06-recording-model".into()
    }

    fn renders_operator_notices(&self) -> bool {
        true
    }

    fn renders_tool_changes(&self) -> bool {
        self.inline
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

struct FixtureTool {
    name: &'static str,
    description: &'static str,
    property: &'static str,
}

#[async_trait::async_trait]
impl crate::tool::Tool for FixtureTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {self.property: {"type": "string"}}})
    }
    fn decode_input(&self, _input: &Value) -> Result<()> {
        Ok(())
    }
    async fn execute(
        &self,
        _input: Value,
        _ctx: crate::tool::ToolContext,
    ) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new("fixture output"))
    }
}

const PROBE: &str = "fixture_probe";
const MCP: &str = "mcp__fixture__query";

fn probe(description: &'static str, property: &'static str) -> FixtureTool {
    FixtureTool {
        name: PROBE,
        description,
        property,
    }
}

fn mcp() -> FixtureTool {
    FixtureTool {
        name: MCP,
        description: "Query the fixture server",
        property: "q",
    }
}

/// A server start: the persisted session in a fresh Agent whose fresh
/// registry holds `tools`.
async fn restart(
    provider: &RecordingProvider,
    session_id: &str,
    tools: Vec<FixtureTool>,
    mcp_connecting: bool,
) -> Result<Agent> {
    let session = crate::session::Session::load(session_id)?;
    let provider: Arc<dyn Provider> = Arc::new(provider.clone());
    let registry = Registry::new(Arc::clone(&provider)).await;
    for tool in tools {
        registry
            .register(tool.name.to_string(), Arc::new(tool))
            .await;
    }
    registry.set_mcp_connecting(mcp_connecting);
    Ok(Agent::new_with_session(provider, registry, session, None))
}

async fn turn(agent: &mut Agent, message: &str) -> Result<()> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc(message, Vec::new(), None, tx)
        .await
}

/// The request's tools exactly as each production builder sends them.
fn built_tools(request: &Recorded) -> (Value, Value) {
    (
        serde_json::to_value(jcode_provider_anthropic::format_tools(&request.tools)).unwrap(),
        Value::Array(jcode_provider_openai::build_tools(&request.tools)),
    )
}

fn tool<'a>(request: &'a Recorded, name: &str) -> Option<&'a ToolDefinition> {
    request.tools.iter().find(|tool| tool.name == name)
}

/// The tool changes the session's notices announced, one entry per notice.
fn announced(agent: &Agent) -> Vec<Vec<ToolSetChange>> {
    agent
        .session
        .messages
        .iter()
        .filter_map(crate::session::StoredMessage::operator_delivery)
        .filter(|delivery| !delivery.tool_changes.is_empty())
        .map(|delivery| delivery.tool_changes.to_vec())
        .collect()
}

fn last_tool_result(request: &Recorded) -> String {
    request
        .messages
        .iter()
        .rev()
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a tool result")
}

async fn restarts_keep_the_tool_set_and_announce_each_change_once(inline: bool) -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let provider = RecordingProvider {
        inline,
        ..Default::default()
    };
    let request = |index: usize| {
        let mut state = provider.state.lock().unwrap();
        let recorded = state.requests.remove(index);
        state.requests.insert(
            index,
            Recorded {
                messages: recorded.messages.clone(),
                tools: recorded.tools.clone(),
            },
        );
        recorded
    };
    let journaled = |since: std::time::Instant| {
        crate::cache_invalidation::recorded_since(since)
            .iter()
            .filter(|entry| entry.source == crate::tool::TOOL_SET_TRANSITION)
            .count()
    };

    // The first request freezes the set.
    let mut session = crate::session::Session::create(None, None);
    let session_id = session.id.clone();
    session.save()?;
    let mut agent = restart(
        &provider,
        &session_id,
        vec![probe("Probe v1", "x"), mcp()],
        false,
    )
    .await?;
    turn(&mut agent, "first").await?;
    let frozen = built_tools(&request(0));
    assert!(tool(&request(0), PROBE).is_some() && tool(&request(0), MCP).is_some());

    // Two restarts with an unchanged registry: the same bytes on both
    // builders, nothing announced.
    for prompt in ["second", "third"] {
        let since = std::time::Instant::now();
        agent = restart(
            &provider,
            &session_id,
            vec![probe("Probe v1", "x"), mcp()],
            false,
        )
        .await?;
        turn(&mut agent, prompt).await?;
        assert_eq!(journaled(since), 0);
    }
    assert_eq!(built_tools(&request(2)), frozen);
    assert!(announced(&agent).is_empty());

    // The MCP server has not reconnected yet: its tool stays, nothing is
    // announced, and a call says why it cannot run.
    agent = restart(&provider, &session_id, vec![probe("Probe v1", "x")], true).await?;
    provider
        .state
        .lock()
        .unwrap()
        .script
        .push_back(Reply::Call(MCP));
    turn(&mut agent, "fourth").await?;
    assert_eq!(built_tools(&request(4)), frozen);
    assert!(announced(&agent).is_empty());
    assert!(
        last_tool_result(&request(4)).contains("reconnecting"),
        "{}",
        last_tool_result(&request(4))
    );

    // A binary whose tool description changed: one notice; the first-sent
    // bytes stay in every array.
    let since = std::time::Instant::now();
    agent = restart(
        &provider,
        &session_id,
        vec![probe("Probe v2", "x"), mcp()],
        false,
    )
    .await?;
    turn(&mut agent, "fifth").await?;
    assert_eq!(built_tools(&request(5)), frozen);
    assert_eq!(journaled(since), 0);
    let notices = announced(&agent);
    assert_eq!(notices.len(), 1);
    assert!(matches!(
        notices[0].as_slice(),
        [ToolSetChange::Redefined { definition }] if definition.description == "Probe v2"
    ));

    // A changed input schema: one more notice. An array provider's tools
    // follow, as a recorded transition; in-message changes leave the array.
    let since = std::time::Instant::now();
    agent = restart(
        &provider,
        &session_id,
        vec![probe("Probe v2", "y"), mcp()],
        false,
    )
    .await?;
    turn(&mut agent, "sixth").await?;
    assert_eq!(announced(&agent).len(), 2);
    if inline {
        assert_eq!(built_tools(&request(6)), frozen);
        assert_eq!(journaled(since), 0);
    } else {
        let sent = tool(&request(6), PROBE).unwrap().clone();
        assert!(sent.input_schema["properties"].get("y").is_some());
        assert_eq!(journaled(since), 1);
    }

    // The probe is gone and a new tool arrived: one notice with both. The
    // removed definition stays in every array, and a call to it is refused.
    let since = std::time::Instant::now();
    agent = restart(
        &provider,
        &session_id,
        vec![
            mcp(),
            FixtureTool {
                name: "fixture_new",
                description: "A new tool",
                property: "z",
            },
        ],
        false,
    )
    .await?;
    provider
        .state
        .lock()
        .unwrap()
        .script
        .push_back(Reply::Call(PROBE));
    turn(&mut agent, "seventh").await?;
    let notices = announced(&agent);
    assert_eq!(notices.len(), 3);
    assert_eq!(notices[2].len(), 2);
    assert!(tool(&request(7), PROBE).is_some());
    assert_eq!(tool(&request(7), "fixture_new").is_some(), !inline);
    assert_eq!(journaled(since), usize::from(!inline));
    assert!(
        last_tool_result(&request(8)).contains("no longer available"),
        "{}",
        last_tool_result(&request(8))
    );

    // What the Opus 5.5 builder sends for the last notice: a system message
    // carrying the removal by name and the addition by value.
    if inline {
        let wire = serde_json::to_value(jcode_provider_anthropic::format_messages_for(
            &request(8).messages,
            jcode_provider_core::anthropic_conversation_caps("claude-opus-5-5"),
        ))?;
        let kinds: Vec<&str> = wire
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .flat_map(|message| message["content"].as_array().unwrap())
            .filter_map(|block| block["type"].as_str())
            .filter(|kind| *kind != "text")
            .collect();
        assert_eq!(
            kinds,
            vec![
                "tool_addition",
                "tool_addition",
                "tool_removal",
                "tool_addition"
            ]
        );
    }

    // Settled: another restart announces nothing, and the persisted record
    // reproduces the same arrays.
    let before = built_tools(&request(8));
    agent = restart(
        &provider,
        &session_id,
        vec![
            mcp(),
            FixtureTool {
                name: "fixture_new",
                description: "A new tool",
                property: "z",
            },
        ],
        false,
    )
    .await?;
    turn(&mut agent, "eighth").await?;
    assert_eq!(built_tools(&request(9)), before);
    assert_eq!(announced(&agent).len(), 3);
    Ok(())
}

#[tokio::test]
async fn a_restarted_session_keeps_its_tools_on_a_provider_whose_array_carries_changes()
-> Result<()> {
    restarts_keep_the_tool_set_and_announce_each_change_once(false).await
}

#[tokio::test]
async fn a_restarted_session_keeps_its_tools_on_a_provider_with_in_message_changes() -> Result<()> {
    restarts_keep_the_tool_set_and_announce_each_change_once(true).await
}

/// A provider that takes tool changes inside a message sees only the notices
/// its history holds. A rewind that cuts a notice off must not undo the
/// change for the model: the next request announces it again, and neither
/// the record nor the array changes.
#[tokio::test]
async fn a_change_whose_notice_was_rewound_is_announced_again() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    let provider = RecordingProvider {
        inline: true,
        ..Default::default()
    };
    let notice_changes = |index: usize| -> Vec<ToolSetChange> {
        provider.state.lock().unwrap().requests[index]
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::OperatorNotice { tool_changes, .. } => Some(tool_changes.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    };
    let tools = |index: usize| provider.state.lock().unwrap().requests[index].tools.clone();

    let mut session = crate::session::Session::create(None, None);
    let session_id = session.id.clone();
    session.save()?;
    let mut agent = restart(&provider, &session_id, Vec::new(), false).await?;
    turn(&mut agent, "first").await?;
    let kept = agent.session.messages.len();

    agent
        .registry
        .register(MCP.to_string(), Arc::new(mcp()))
        .await;
    turn(&mut agent, "second").await?;
    assert_eq!(notice_changes(1).len(), 1);
    let record = agent.session.tool_set.clone();

    agent
        .rewind_to_message(kept)
        .map_err(|error| anyhow::anyhow!(error))?;
    assert_eq!(
        crate::tool::tool_set_notice_count(&agent.session.messages),
        0
    );
    turn(&mut agent, "after the rewind").await?;
    assert!(matches!(
        notice_changes(2).as_slice(),
        [ToolSetChange::Added { definition }] if definition.name == MCP
    ));
    assert_eq!(tools(2), tools(0), "the array is as first advertised");
    assert_eq!(agent.session.tool_set, record, "the record did not change");

    // In view again: nothing more is announced.
    turn(&mut agent, "next").await?;
    assert_eq!(notice_changes(3).len(), 1);
    assert_eq!(
        crate::tool::tool_set_notice_count(&agent.session.messages),
        1
    );
    Ok(())
}
