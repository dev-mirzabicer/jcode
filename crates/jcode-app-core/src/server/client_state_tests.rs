use super::handle_get_history;
use super::handle_get_model_catalog;
use super::session_activity_snapshot;
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::server::ClientConnectionInfo;
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::io::BufRead as _;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, RwLock, mpsc};

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "mock provider complete should not be called in client_state tests"
        ))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    fn model(&self) -> String {
        "mock-model".to_string()
    }
}

#[tokio::test]
async fn session_activity_snapshot_prefers_live_tool_name_for_target_session() {
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        (
            "conn-idle".to_string(),
            ClientConnectionInfo {
                client_id: "conn-idle".to_string(),
                session_id: "other-session".to_string(),
                client_instance_id: None,
                debug_client_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: true,
                current_tool_name: Some("bash".to_string()),
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
        (
            "conn-target".to_string(),
            ClientConnectionInfo {
                client_id: "conn-target".to_string(),
                session_id: "target-session".to_string(),
                client_instance_id: None,
                debug_client_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: true,
                current_tool_name: Some("batch".to_string()),
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
    ])));

    let snapshot = session_activity_snapshot(&client_connections, "target-session", false)
        .await
        .expect("activity snapshot");

    assert!(snapshot.is_processing);
    assert_eq!(snapshot.current_tool_name.as_deref(), Some("batch"));
}

#[tokio::test]
async fn session_activity_snapshot_uses_fallback_when_no_live_connection_is_marked_busy() {
    let client_connections = Arc::new(RwLock::new(HashMap::<String, ClientConnectionInfo>::new()));

    let snapshot = session_activity_snapshot(&client_connections, "target-session", true)
        .await
        .expect("fallback snapshot");

    assert!(snapshot.is_processing);
    assert_eq!(snapshot.current_tool_name, None);
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "test intentionally keeps the agent busy lock held to exercise persisted-history fallback"
)]
async fn handle_get_history_falls_back_to_persisted_snapshot_when_agent_is_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("create temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    let session_id = "session_busy_history_fallback";
    let mut session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("busy fallback".to_string()),
    );
    session.model = Some("mock-model".to_string());
    session.append_stored_message(crate::session::StoredMessage {
        origin: None,
        id: "msg-busy-fallback".to_string(),
        role: crate::message::Role::User,
        content: vec![crate::message::ContentBlock::Text {
            text: "persisted fallback history".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });
    session.save().expect("save session");

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::empty();
    let mut live_session = session.clone();
    live_session.title = Some("live agent".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider.clone(),
        registry,
        live_session,
        None,
    )));
    let busy_guard = agent.lock().await;

    let sessions = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
        session_id.to_string(),
        Arc::clone(&agent),
    )])));
    let client_connections = Arc::new(RwLock::new(HashMap::<String, ClientConnectionInfo>::new()));
    let client_count = Arc::new(RwLock::new(1usize));

    let (stream_a, mut stream_b) = crate::transport::stream_pair().expect("stream pair");
    let (_reader_a, writer_a) = stream_a.into_split();
    let writer = Arc::new(Mutex::new(writer_a));
    let startup_context = crate::server::startup_context::test_coordinator();

    handle_get_history(
        42,
        session_id,
        true,
        &agent,
        &startup_context,
        &provider,
        &sessions,
        &client_connections,
        &client_count,
        &writer,
        "server-name",
        "🔥",
        None,
    )
    .await
    .expect("history should be written from persisted fallback");

    drop(busy_guard);
    drop(writer);

    let mut bytes = Vec::new();
    stream_b
        .read_to_end(&mut bytes)
        .await
        .expect("read history event bytes");
    let mut cursor = std::io::Cursor::new(bytes);
    let mut line = String::new();
    cursor.read_line(&mut line).expect("read first line");
    let event: crate::protocol::ServerEvent =
        serde_json::from_str(line.trim()).expect("decode history event");

    match event {
        crate::protocol::ServerEvent::History {
            id,
            session_id: returned_session_id,
            messages,
            activity,
            ..
        } => {
            assert_eq!(id, 42);
            assert_eq!(returned_session_id, session_id);
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "persisted fallback history");
            let activity = activity.expect("fallback activity snapshot");
            assert!(activity.is_processing);
        }
        other => panic!("expected history event, got {:?}", other),
    }

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "test intentionally keeps the agent busy lock held to exercise model-catalog fallback"
)]
async fn handle_get_model_catalog_does_not_wait_for_busy_agent_lock() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("create temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    let session_id = "session_busy_model_catalog_fallback";
    let mut session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("busy model catalog".to_string()),
    );
    session.model = Some("persisted-model".to_string());
    session.save().expect("save session");

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider.clone(),
        Registry::empty(),
        session.clone(),
        None,
    )));
    let busy_guard = agent.lock().await;

    let (stream_a, mut stream_b) = crate::transport::stream_pair().expect("stream pair");
    let (_reader_a, writer_a) = stream_a.into_split();
    let writer = Arc::new(Mutex::new(writer_a));
    let startup_context = crate::server::startup_context::test_coordinator();

    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_get_model_catalog(43, session_id, &agent, &startup_context, &provider, &writer),
    )
    .await
    .expect("model catalog must not wait for busy agent mutex")
    .expect("model catalog fallback should write history event");

    drop(busy_guard);
    drop(writer);

    let mut bytes = Vec::new();
    stream_b
        .read_to_end(&mut bytes)
        .await
        .expect("read model catalog event bytes");
    let mut cursor = std::io::Cursor::new(bytes);
    let mut line = String::new();
    cursor.read_line(&mut line).expect("read first line");
    let event: crate::protocol::ServerEvent =
        serde_json::from_str(line.trim()).expect("decode model catalog event");

    match event {
        crate::protocol::ServerEvent::History {
            id,
            session_id: returned_session_id,
            provider_name,
            provider_model,
            ..
        } => {
            assert_eq!(id, 43);
            assert_eq!(returned_session_id, session_id);
            assert_eq!(provider_name.as_deref(), Some("mock"));
            assert_eq!(provider_model.as_deref(), Some("persisted-model"));
        }
        other => panic!("expected history event, got {:?}", other),
    }

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

struct ReloadHistoryEnvGuard {
    prev_home: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
}

impl ReloadHistoryEnvGuard {
    fn new(home: &std::path::Path, runtime: &std::path::Path) -> Self {
        let prev_home = std::env::var_os("JCODE_HOME");
        let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_HOME", home);
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime);
        Self {
            prev_home,
            prev_runtime,
        }
    }
}

impl Drop for ReloadHistoryEnvGuard {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        if let Some(prev_runtime) = self.prev_runtime.take() {
            crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
    }
}

/// History never infers a continuation from transcript shape or session
/// status. An unexpected exit is reported as its durable recovery item, which
/// only an explicit decision or a new human message resolves.
#[cfg(unix)]
#[test]
fn history_reports_crash_recovery_without_inferring_continuation() -> Result<()> {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::TempDir::new()?;
    let runtime = tempfile::TempDir::new()?;
    let _guard = ReloadHistoryEnvGuard::new(home.path(), runtime.path());
    let session_id = "session_history_crash_recovery";
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.status = crate::session::SessionStatus::Crashed { message: None };
    session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "work that was interrupted".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;
    crate::server::write_reload_state(
        "reload-history-test",
        "hash",
        crate::server::ReloadPhase::Starting,
        Some(session_id.to_string()),
    );
    // Crashed status, a final user message and an active reload marker used
    // to make History ask the client to continue. Nothing is inferred now.
    assert!(super::unresolved_runtime_recovery(session_id).is_none());
    assert_eq!(super::history_recovery(session_id), (None, None));

    let store = crate::runtime_lifecycle::RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &crate::server::socket_path(),
    )?;
    let crashed = store.claim()?;
    crashed.turns().begin(session_id)?;
    drop(crashed);
    let next = store.claim()?;
    let items = next.reconcile_interrupted_turns(|_, _| {
        anyhow::bail!("an unexpected exit is never a planned transition")
    })?;
    assert_eq!(items.len(), 1);
    // A legacy reload intent for the same session must not let a client
    // continue a turn that now needs an explicit decision.
    let directive = crate::protocol::ReloadRecoverySnapshot {
        reconnect_notice: None,
        continuation_message: "legacy continuation".to_string(),
    };
    super::super::reload_recovery::persist_intent(
        "legacy-reload",
        session_id,
        super::super::reload_recovery::ReloadRecoveryRole::Initiator,
        directive.clone(),
        "legacy",
    )?;
    let (attached, recovery) = super::history_recovery(session_id);
    assert!(attached.is_none() && recovery.is_some());
    let reported = super::unresolved_runtime_recovery(session_id)
        .expect("unresolved recovery is reported to clients");
    assert_eq!(reported.id, items[0].id);
    assert_eq!(
        reported.cause,
        crate::workspace::runtime::RecoveryCause::UnexpectedExit
    );
    next.recovery()
        .supersede(session_id, crate::workspace::RequestId::new())?;
    assert!(super::unresolved_runtime_recovery(session_id).is_none());
    // Without a pending decision a legacy intent keeps its attach semantics;
    // a runtime-owned intent is delivered by the runtime, never by a client.
    assert_eq!(
        super::history_recovery(session_id),
        (Some(directive.clone()), None)
    );
    super::super::reload_recovery::persist_runtime_intent(
        "runtime-reload",
        session_id,
        super::super::reload_recovery::ReloadRecoveryRole::InterruptedPeer,
        directive,
        "verified",
    )?;
    assert_eq!(super::history_recovery(session_id), (None, None));
    Ok(())
}
