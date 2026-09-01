#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{
    NotifySessionContext, clone_split_session, create_transfer_child_session,
    handle_notify_session, handle_rename_session, handle_resume_all_sessions, handle_set_feature,
};
use crate::agent::Agent;
use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::protocol::{FeatureToggle, ServerEvent};
use crate::provider::{EventStream, Provider};
use crate::server::{ClientConnectionInfo, SwarmMember};
use crate::tool::Registry;
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use jcode_session_types::{StoredContextEmergencyPolicy, StoredUnattendedContextAuthorization};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, mpsc};

fn scheduled_authorization(item_id: &str) -> StoredUnattendedContextAuthorization {
    StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 10,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: "schedule_tool_session:origin".to_string(),
        },
        authorization_source: format!("scheduled_item:{item_id}"),
        scheduled_item_id: Some(item_id.to_string()),
    }
}
use tokio::time::{Duration, timeout};

#[allow(clippy::type_complexity)]
fn empty_swarm_status_state() -> (
    Arc<RwLock<HashMap<String, std::collections::HashSet<String>>>>,
    Arc<RwLock<std::collections::VecDeque<crate::server::SwarmEvent>>>,
    Arc<std::sync::atomic::AtomicU64>,
    tokio::sync::broadcast::Sender<crate::server::SwarmEvent>,
) {
    let (swarm_event_tx, _) = tokio::sync::broadcast::channel(16);
    (
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(std::collections::VecDeque::new())),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        swarm_event_tx,
    )
}

struct MockProvider;

#[derive(Clone, Default)]
struct StreamingMockProvider {
    responses: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
}

impl StreamingMockProvider {
    fn queue_response(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "mock provider complete should not be called in client_actions tests"
        ))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

#[async_trait]
impl Provider for StreamingMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let stream = stream! {
            for event in events {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn clone_split_session_uses_persisted_session_state() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let mut parent = crate::session::Session::create_with_id(
        "session_parent_split_test".to_string(),
        None,
        None,
    );
    parent.working_dir = Some("/tmp/jcode-split-test".to_string());
    parent.model = Some("gpt-test".to_string());
    parent.provider_key = Some("provider-test".to_string());
    parent.route_api_method = Some("route-test".to_string());
    parent.reasoning_effort = Some("high".to_string());
    parent.subagent_model = Some("subagent-test".to_string());
    parent.improve_mode = Some(crate::session::SessionImproveMode::ImproveRun);
    parent.autoreview_enabled = Some(true);
    parent.autojudge_enabled = Some(true);
    parent.provider_session_id = Some("must-not-transfer".to_string());
    parent.startup_context_block = Some(jcode_session_types::StoredStartupContextBlock {
        kind: jcode_session_types::StoredStartupContextBlockKind::PlanStorage,
        message: "synthetic inherited block".to_string(),
        blocked_at: chrono::Utc::now(),
    });
    parent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "hello from parent".to_string(),
            cache_control: None,
        }],
    );
    parent.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });
    parent.save().expect("save parent");
    let migrated_parent = crate::session::Session::load(&parent.id).expect("migrate parent");
    assert!(migrated_parent.compaction.is_none());
    assert_eq!(migrated_parent.context_view.active_transaction_count(), 1);

    let (child_id, _child_name) = clone_split_session(&parent.id).expect("clone split");
    let child = crate::session::Session::load(&child_id).expect("load child");

    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(
        child.messages.len(),
        parent.messages.len() + 1,
        "fork should inherit the transcript plus one fork notice"
    );
    assert_eq!(
        child.messages[0].content_preview(),
        parent.messages[0].content_preview()
    );
    let fork_notice = child.messages.last().expect("fork notice message");
    assert_eq!(
        fork_notice.display_role,
        Some(crate::session::StoredDisplayRole::System),
        "fork notice must be hidden from the visible transcript"
    );
    let fork_notice_text = fork_notice.content_preview();
    assert!(
        fork_notice_text.contains("forked") && fork_notice_text.contains(parent.id.as_str()),
        "fork notice should mention the parent session: {fork_notice_text}"
    );
    assert!(child.compaction.is_none());
    assert_eq!(child.context_view, migrated_parent.context_view);
    assert_eq!(
        child.startup_context_block,
        migrated_parent.startup_context_block
    );
    assert_eq!(child.working_dir, parent.working_dir);
    assert_eq!(child.model, parent.model);
    assert_eq!(child.provider_key, parent.provider_key);
    assert_eq!(child.route_api_method, parent.route_api_method);
    assert_eq!(child.reasoning_effort, parent.reasoning_effort);
    assert_eq!(child.subagent_model, parent.subagent_model);
    assert_eq!(child.improve_mode, parent.improve_mode);
    assert_eq!(child.autoreview_enabled, parent.autoreview_enabled);
    assert_eq!(child.autojudge_enabled, parent.autojudge_enabled);
    assert!(child.provider_session_id.is_none());
    assert_eq!(child.status, crate::session::SessionStatus::Closed);
    assert_ne!(child.id, parent.id);

    let parent_context_before = migrated_parent.context_view.clone();
    let mut mutated_child = child.clone();
    let next_revision = mutated_child.context_view.revision + 1;
    mutated_child.context_view.revision = next_revision;
    mutated_child.context_view.transactions[0]
        .status_events
        .push(jcode_session_types::StoredContextStatusEvent {
            revision: next_revision,
            timestamp: chrono::Utc::now(),
            kind: jcode_session_types::StoredContextTransactionStatusKind::Reverted,
            reason: Some("child-only revert".to_string()),
        });
    mutated_child.save().expect("save child-only mutation");
    let parent_after_child_mutation =
        crate::session::Session::load(&parent.id).expect("reload independent parent");
    assert_eq!(
        parent_after_child_mutation.context_view,
        parent_context_before
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn transfer_child_uses_authoritative_handoff_instead_of_invalid_legacy_compaction() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("create project");
    std::fs::write(project.join("required.txt"), "transfer startup snapshot")
        .expect("write startup file");
    let startup = crate::startup_context::StartupContext::new();
    let active = startup.resolve_project(&project).expect("resolve project");
    let preview = startup.preview_selection(
        &active,
        [crate::startup_context::StartupSelectionInput::new(
            "required.txt",
        )],
    );
    startup
        .save_project_plan(&active, 0, &preview)
        .expect("save startup plan");

    let mut parent = crate::session::Session::create_with_id(
        "session_parent_transfer_test".to_string(),
        None,
        None,
    );
    parent.working_dir = Some(project.to_string_lossy().into_owned());
    parent.model = Some("gpt-test".to_string());
    parent.provider_key = Some("provider-test".to_string());
    parent.route_api_method = Some("route-test".to_string());
    parent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "raw parent transcript must not be copied".to_string(),
            cache_control: None,
        }],
    );
    parent.save().expect("save transfer parent");
    let parent_before = serde_json::to_vec(&crate::session::Session::load(&parent.id).unwrap())
        .expect("serialize parent");
    let (child_id, _) = create_transfer_child_session(
        &parent.id,
        &parent,
        Some("Readable transfer summary".to_string()),
    )
    .expect("create transfer child");
    let child = crate::session::Session::load(&child_id).expect("load transfer child");

    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(
        child.startup_context.as_ref().unwrap().batches[0]
            .files
            .len(),
        1
    );
    assert_eq!(child.messages.len(), 4);
    assert!(child.compaction.is_none());
    assert_eq!(child.context_view, Default::default());
    assert_eq!(child.provider_key, parent.provider_key);
    assert_eq!(child.route_api_method, parent.route_api_method);
    assert!(child.provider_session_id.is_none());
    let handoff = child.messages.last().expect("authoritative handoff");
    assert_eq!(
        handoff.display_role,
        Some(crate::session::StoredDisplayRole::System)
    );
    let handoff_text = handoff.content_preview();
    assert!(handoff_text.contains("Readable transfer summary"));
    assert!(handoff_text.contains(parent.id.as_str()));
    assert!(!handoff_text.contains("raw parent transcript must not be copied"));
    assert!(child.messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "transfer startup snapshot"))
    }));
    assert_eq!(
        serde_json::to_vec(&crate::session::Session::load(&parent.id).unwrap())
            .expect("serialize parent"),
        parent_before
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn transfer_capture_failure_leaves_no_child_session_or_todo_sidecar() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("create project");
    let required = project.join("required.txt");
    std::fs::write(&required, "will disappear").expect("write startup file");
    let startup = crate::startup_context::StartupContext::new();
    let active = startup.resolve_project(&project).expect("resolve project");
    let preview = startup.preview_selection(
        &active,
        [crate::startup_context::StartupSelectionInput::new(
            "required.txt",
        )],
    );
    startup
        .save_project_plan(&active, 0, &preview)
        .expect("save startup plan");
    std::fs::remove_file(required).expect("remove required file");

    let mut parent = crate::session::Session::create_with_id(
        "session_parent_transfer_failure".to_string(),
        None,
        None,
    );
    parent.working_dir = Some(project.to_string_lossy().into_owned());
    parent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "parent".to_string(),
            cache_control: None,
        }],
    );
    parent.save().expect("save parent");
    let sessions_dir = temp.path().join("sessions");
    let before = std::fs::read_dir(&sessions_dir)
        .expect("sessions dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::HashSet<_>>();

    assert!(
        create_transfer_child_session(&parent.id, &parent, Some("handoff".to_string())).is_err()
    );
    let after = std::fs::read_dir(&sessions_dir)
        .expect("sessions dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::HashSet<_>>();
    for file_name in after.difference(&before) {
        let path = sessions_dir.join(file_name);
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).expect("read concurrently created session snapshot"),
        )
        .expect("decode concurrently created session snapshot");
        assert_ne!(
            value["parent_id"].as_str(),
            Some(parent.id.as_str()),
            "rejected transfer left an unpublished child snapshot at {}",
            path.display()
        );
    }

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn enabling_swarm_does_not_auto_elect_coordinator() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let (member_event_tx, _member_event_rx) = mpsc::unbounded_channel();
    let now = Instant::now();
    let session_id = "session_test_swarm_toggle";
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        crate::server::SwarmMember {
            session_id: session_id.to_string(),
            event_tx: member_event_tx,
            event_txs: HashMap::new(),
            working_dir: Some(PathBuf::from("/tmp/jcode-passive-swarm")),
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("duck".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let mut swarm_enabled = false;

    handle_set_feature(
        42,
        FeatureToggle::Swarm,
        true,
        &agent,
        session_id,
        &Some("duck".to_string()),
        &mut swarm_enabled,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &channel_subscriptions,
        &channel_subscriptions_by_session,
        &swarm_plans,
        &client_event_tx,
    )
    .await;

    assert!(swarm_enabled);
    assert!(swarm_coordinators.read().await.is_empty());
    assert_eq!(
        swarm_members
            .read()
            .await
            .get(session_id)
            .and_then(|member| member.swarm_id.clone())
            .as_deref(),
        Some("session:session_test_swarm_toggle")
    );
    assert_eq!(
        swarm_members
            .read()
            .await
            .get(session_id)
            .map(|member| member.role.as_str()),
        Some("agent")
    );

    let events: Vec<_> = std::iter::from_fn(|| client_event_rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id: 42 }))
    );
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            ServerEvent::Notification { message, .. }
                if message == "You are the coordinator for this swarm."
        )
    }));
}

#[tokio::test]
async fn remote_memory_enable_is_rejected_when_globally_disabled() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_MEMORY_ENABLED");
    crate::env::set_var("JCODE_MEMORY_ENABLED", "false");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let mut swarm_enabled = false;
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_set_feature(
        43,
        FeatureToggle::Memory,
        true,
        &agent,
        "session_memory_disabled",
        &None,
        &mut swarm_enabled,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &channel_subscriptions,
        &channel_subscriptions_by_session,
        &swarm_plans,
        &client_event_tx,
    )
    .await;

    assert!(!agent.lock().await.memory_enabled());
    assert!(matches!(
        client_event_rx.try_recv(),
        Ok(ServerEvent::Error { id: 43, .. })
    ));

    if let Some(previous) = previous {
        crate::env::set_var("JCODE_MEMORY_ENABLED", previous);
    } else {
        crate::env::remove_var("JCODE_MEMORY_ENABLED");
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn rename_session_event_uses_agent_session_id_even_when_client_id_is_stale() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let agent_session_id = agent.lock().await.session_id().to_string();
    let stale_client_session_id = "session_stale_client_id";
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let now = Instant::now();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        stale_client_session_id.to_string(),
        SwarmMember {
            session_id: stale_client_session_id.to_string(),
            event_tx: member_event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("stale".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_rename_session(
        99,
        Some("Release planning".to_string()),
        &agent,
        stale_client_session_id,
        &swarm_members,
        &client_event_tx,
    )
    .await;

    let rename_event = timeout(Duration::from_secs(2), member_event_rx.recv())
        .await
        .expect("rename event should arrive")
        .expect("member event channel should stay open");
    match rename_event {
        ServerEvent::SessionRenamed {
            session_id,
            title,
            display_title,
        } => {
            assert_eq!(session_id, agent_session_id);
            assert_eq!(title.as_deref(), Some("Release planning"));
            assert_eq!(display_title, "Release planning");
        }
        other => panic!("expected SessionRenamed, got {other:?}"),
    }

    let client_events: Vec<_> = std::iter::from_fn(|| client_event_rx.try_recv().ok()).collect();
    assert!(
        client_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 99))
    );
    let loaded = crate::session::Session::load(&agent_session_id).expect("renamed session saved");
    assert_eq!(loaded.custom_title.as_deref(), Some("Release planning"));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn notify_session_runs_scheduled_task_immediately_for_idle_live_session() {
    let provider = Arc::new(StreamingMockProvider::default());
    provider.queue_response(vec![
        StreamEvent::TextDelta("Working on scheduled task.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_dyn.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider_dyn, registry)));
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::<String, Arc<Mutex<Agent>>>::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "client-1".to_string(),
        ClientConnectionInfo {
            client_id: "client-1".to_string(),
            session_id: session_id.clone(),
            client_instance_id: None,
            debug_client_id: Some("debug-1".to_string()),
            connected_at: Instant::now(),
            last_seen: Instant::now(),
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        SwarmMember {
            session_id: session_id.clone(),
            event_tx: member_event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("otter".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let (swarms_by_id, event_history, event_counter, swarm_event_tx) = empty_swarm_status_state();
    handle_notify_session(
        77,
        session_id.clone(),
        "[Scheduled task]\nTask: Follow up".to_string(),
        Some(scheduled_authorization("sched-live")),
        NotifySessionContext {
            sessions: &sessions,
            soft_interrupt_queues: &soft_interrupt_queues,
            client_connections: &client_connections,
            swarm_members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            swarm_event_tx: &swarm_event_tx,
            client_event_tx: &client_event_tx,
        },
    )
    .await;

    let streamed_event = timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::TextDelta { text })
                    if text.contains("Working on scheduled task.") =>
                {
                    return text;
                }
                Some(_) => continue,
                None => panic!("live member stream closed before scheduled task ran"),
            }
        }
    })
    .await
    .expect("scheduled task should start streaming promptly");
    assert!(streamed_event.contains("Working on scheduled task."));

    let client_events: Vec<_> = std::iter::from_fn(|| client_event_rx.try_recv().ok()).collect();
    assert!(
        client_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 77))
    );

    let guard = agent.lock().await;
    assert!(guard.messages().iter().any(|message| {
        message.role == Role::User
            && message.display_role == Some(crate::session::StoredDisplayRole::System)
            && message
                .content_preview()
                .contains("[Scheduled task] Task: Follow up")
    }));
    assert!(guard.messages().iter().any(|message| {
        message.role == Role::Assistant
            && message
                .content_preview()
                .contains("Working on scheduled task.")
    }));
}

#[tokio::test]
async fn notify_session_queues_soft_interrupt_when_live_session_is_busy() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let session_id = agent.lock().await.session_id().to_string();
    let queue = agent.lock().await.soft_interrupt_queue();

    let sessions = Arc::new(RwLock::new(HashMap::<String, Arc<Mutex<Agent>>>::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        queue.clone(),
    )])));
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "client-1".to_string(),
        ClientConnectionInfo {
            client_id: "client-1".to_string(),
            session_id: session_id.clone(),
            client_instance_id: None,
            debug_client_id: Some("debug-1".to_string()),
            connected_at: Instant::now(),
            last_seen: Instant::now(),
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        SwarmMember {
            session_id: session_id.clone(),
            event_tx: member_event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "running".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("otter".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let _busy_guard = agent.lock().await;

    let (swarms_by_id, event_history, event_counter, swarm_event_tx) = empty_swarm_status_state();
    handle_notify_session(
        88,
        session_id.clone(),
        "[Scheduled task]\nTask: Follow up while busy".to_string(),
        Some(scheduled_authorization("sched-busy")),
        NotifySessionContext {
            sessions: &sessions,
            soft_interrupt_queues: &soft_interrupt_queues,
            client_connections: &client_connections,
            swarm_members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            swarm_event_tx: &swarm_event_tx,
            client_event_tx: &client_event_tx,
        },
    )
    .await;

    let member_event = timeout(Duration::from_secs(2), member_event_rx.recv())
        .await
        .expect("notification should arrive promptly")
        .expect("live member should receive notification");
    match member_event {
        ServerEvent::Notification {
            from_session,
            from_name,
            message,
            ..
        } => {
            assert_eq!(from_session, "schedule");
            assert_eq!(from_name.as_deref(), Some("scheduled task"));
            assert!(message.contains("Task: Follow up while busy"));
        }
        other => panic!("expected notification event, got {other:?}"),
    }

    let queued = queue.lock().unwrap();
    assert_eq!(
        queued.len(),
        1,
        "scheduled task should queue as soft interrupt"
    );
    assert!(queued[0].content.contains("Task: Follow up while busy"));
    assert_eq!(
        queued[0].unattended_context,
        Some(scheduled_authorization("sched-busy"))
    );
    drop(queued);

    let client_events: Vec<_> = std::iter::from_fn(|| client_event_rx.try_recv().ok()).collect();
    assert!(
        client_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 88))
    );
}

/// Build a live SwarmMember with a real client attachment so the resume-all
/// sweep treats it as live. Returns the member and the receiver for events
/// fanned out to that attachment.
fn live_member(session_id: &str) -> (SwarmMember, mpsc::UnboundedReceiver<ServerEvent>) {
    let (attach_tx, attach_rx) = mpsc::unbounded_channel();
    let member = SwarmMember {
        session_id: session_id.to_string(),
        event_tx: mpsc::unbounded_channel().0,
        event_txs: HashMap::from([("client-1".to_string(), attach_tx)]),
        working_dir: None,
        swarm_id: None,
        swarm_enabled: false,
        status: "ready".to_string(),
        detail: None,
        task_label: None,
        friendly_name: Some("otter".to_string()),
        report_back_to_session_id: None,
        latest_completion_report: None,
        role: "agent".to_string(),
        joined_at: Instant::now(),
        last_status_change: Instant::now(),
        is_headless: false,
        output_tail: None,
        todo_progress: None,
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    };
    (member, attach_rx)
}

#[tokio::test]
async fn resume_all_continues_interrupted_idle_live_session() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = Arc::new(StreamingMockProvider::default());
    provider.queue_response(vec![
        StreamEvent::TextDelta("Continuing where I left off.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_dyn.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider_dyn, registry)));
    let session_id = {
        let mut guard = agent.lock().await;
        // Leave the session with a pending user turn the assistant never answered
        // (simulating a turn that errored / was interrupted mid-generation).
        guard.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "please keep going on the refactor".to_string(),
                cache_control: None,
            }],
        );
        guard.session_id().to_string()
    };

    let sessions = Arc::new(RwLock::new(HashMap::<String, Arc<Mutex<Agent>>>::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let (member, mut attach_rx) = live_member(&session_id);
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), member)])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let (swarms_by_id, event_history, event_counter, swarm_event_tx) = empty_swarm_status_state();
    handle_resume_all_sessions(
        91,
        &sessions,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &client_event_tx,
    )
    .await;

    // The session should resume and stream the continuation.
    let streamed = timeout(Duration::from_secs(2), async {
        loop {
            match attach_rx.recv().await {
                Some(ServerEvent::TextDelta { text })
                    if text.contains("Continuing where I left off.") =>
                {
                    return text;
                }
                Some(_) => continue,
                None => panic!("live attachment closed before continuation streamed"),
            }
        }
    })
    .await
    .expect("interrupted session should resume promptly");
    assert!(streamed.contains("Continuing where I left off."));

    // The requesting client receives a summary describing one resumed session.
    let result = timeout(Duration::from_secs(2), async {
        loop {
            match client_event_rx.recv().await {
                Some(event @ ServerEvent::ResumeAllResult { .. }) => return event,
                Some(_) => continue,
                None => panic!("client channel closed before resume-all result"),
            }
        }
    })
    .await
    .expect("resume-all result should be emitted");
    match result {
        ServerEvent::ResumeAllResult {
            id,
            resumed,
            skipped,
            ..
        } => {
            assert_eq!(id, 91);
            assert_eq!(resumed, 1);
            assert_eq!(skipped, 0);
        }
        other => panic!("expected ResumeAllResult, got {other:?}"),
    }

    if let Some(home) = prev_home {
        crate::env::set_var("JCODE_HOME", home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn resume_all_skips_session_with_completed_turn() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let session_id = {
        let mut guard = agent.lock().await;
        // A completed turn: last visible message is from the assistant.
        guard.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "do the thing".to_string(),
                cache_control: None,
            }],
        );
        guard.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "done".to_string(),
                cache_control: None,
            }],
        );
        guard.session_id().to_string()
    };

    let sessions = Arc::new(RwLock::new(HashMap::<String, Arc<Mutex<Agent>>>::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let (member, _attach_rx) = live_member(&session_id);
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), member)])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let (swarms_by_id, event_history, event_counter, swarm_event_tx) = empty_swarm_status_state();
    handle_resume_all_sessions(
        92,
        &sessions,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &client_event_tx,
    )
    .await;

    let result = timeout(Duration::from_secs(2), async {
        loop {
            match client_event_rx.recv().await {
                Some(event @ ServerEvent::ResumeAllResult { .. }) => return event,
                Some(_) => continue,
                None => panic!("client channel closed before resume-all result"),
            }
        }
    })
    .await
    .expect("resume-all result should be emitted");
    match result {
        ServerEvent::ResumeAllResult {
            id,
            resumed,
            skipped,
            ..
        } => {
            assert_eq!(id, 92);
            assert_eq!(resumed, 0);
            assert_eq!(skipped, 1);
        }
        other => panic!("expected ResumeAllResult, got {other:?}"),
    }

    if let Some(home) = prev_home {
        crate::env::set_var("JCODE_HOME", home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}
