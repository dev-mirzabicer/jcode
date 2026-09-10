use super::*;

#[tokio::test]
async fn swarm_retirement_await_and_mutation_files_do_not_expire_migrate_or_resume() {
    use crate::server::{await_members_state as awaits, swarm_mutation_state as mutations};
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _env = configure_test_env(&root);
    let _off = ScopedEnvVar::set("JCODE_SWARM_ENABLED", "false");
    let awaited: awaits::PersistedAwaitMembersState = serde_json::from_value(serde_json::json!({
        "key":"retained", "session_id":"fixture", "swarm_id":"retained", "target_status":["completed"],
        "requested_ids":[], "created_at_unix_ms":0, "deadline_unix_ms":1, "background":true,
        "notify":true, "wake":true
    })).unwrap();
    let mutation = mutations::PersistedSwarmMutationState {
        key:"retained".into(), action:"spawn".into(), session_id:"fixture".into(),
        created_at_unix_ms:0, final_response:None,
    };
    let mut fixtures = Vec::new();
    for (directory, bytes) in [
        ("jcode-await-members", serde_json::to_vec(&awaited).unwrap()),
        ("jcode-swarm-mutations", serde_json::to_vec(&mutation).unwrap()),
    ] {
        let path = crate::server::durable_state::state_path(directory, "retained");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        fixtures.push((path, bytes));
    }
    assert!(awaits::load_state("retained").is_none());
    assert!(awaits::pending_await_members_for_session("fixture").is_empty());
    assert!(mutations::load_state("retained").is_none());
    awaits::save_state(&awaited);
    mutations::save_state(&mutation);
    let provider = Arc::new(StreamingMockProvider::default());
    let server = Server::new(provider.clone());
    crate::server::comm_await::resume_background_awaits(
        &server.swarm_state.members, &server.swarm_state.swarms_by_id,
        &server.swarm_event_tx, &server.await_members_runtime,
    ).await;
    server.recover_headless_sessions_on_startup().await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    assert!(mutations::begin_or_replay(&server.swarm_mutation_runtime, "retained", "spawn", "fixture", 1, &tx).await.is_none());
    assert_retired(rx.try_recv().unwrap());
    assert!(server.sessions.read().await.is_empty());
    assert!(provider.requests.lock().unwrap().is_empty());
    for (path, bytes) in fixtures {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

fn retirement_requests() -> Vec<crate::protocol::Request> {
    [
        "comm_share",
        "comm_read",
        "comm_message",
        "comm_list",
        "comm_list_channels",
        "comm_channel_members",
        "comm_propose_plan",
        "comm_approve_plan",
        "comm_reject_plan",
        "comm_seed_graph",
        "comm_expand_node",
        "comm_complete_node",
        "comm_inject_gap",
        "comm_spawn",
        "comm_list_models",
        "comm_stop",
        "comm_assign_role",
        "comm_summary",
        "comm_status",
        "comm_report",
        "comm_read_context",
        "comm_resync_plan",
        "comm_plan_status",
        "comm_assign_task",
        "comm_assign_next",
        "comm_task_control",
        "comm_subscribe_channel",
        "comm_unsubscribe_channel",
        "comm_await_members",
    ]
    .into_iter()
    .enumerate()
    .map(|(ordinal, kind)| {
        // Unknown fields are ignored by the wire decoder. Supply the union of
        // required fields so every coordination variant reaches real dispatch.
        serde_json::from_value(serde_json::json!({
            "type": kind, "id": ordinal as u64 + 100, "session_id": "fixture",
            "key": "key", "value": "value", "from_session": "fixture", "message": "task",
            "channel": "channel", "items": [], "proposer_session": "fixture",
            "nodes": [], "children": [], "node_id": "node", "gate_id": "gate",
            "artifact_json": "{}", "target_session": "fixture", "role": "coordinator",
            "action": "retry", "task_id": "task", "target_status": ["completed"],
            "label": "fixture", "initial_message": "must not execute"
        }))
        .unwrap()
    })
    .collect()
}

async fn retirement_connection(
    server: &Server,
) -> (
    crate::transport::Stream,
    tokio::task::JoinHandle<Result<()>>,
) {
    let (server_stream, client) = crate::transport::Stream::pair().unwrap();
    let task = tokio::spawn(
        crate::server::client_lifecycle::handle_client_with_instruction_repositories(
            server_stream,
            server.sessions.clone(),
            server.event_tx.clone(),
            server.provider.clone(),
            server.context_transactions.clone(),
            server.startup_context.clone(),
            server.instruction_repositories.clone(),
            server.is_processing.clone(),
            server.session_id.clone(),
            server.client_count.clone(),
            server.client_connections.clone(),
            server.swarm_state.members.clone(),
            server.swarm_state.swarms_by_id.clone(),
            server.shared_context.clone(),
            server.swarm_state.plans.clone(),
            server.swarm_state.coordinators.clone(),
            server.file_touch.clone(),
            server.channel_subscriptions.clone(),
            server.channel_subscriptions_by_session.clone(),
            server.client_debug_state.clone(),
            server.client_debug_response_tx.clone(),
            server.event_history.clone(),
            server.event_counter.clone(),
            server.swarm_event_tx.clone(),
            "retirement-test".into(),
            "test".into(),
            crate::server::get_shared_mcp_pool(&server.mcp_pool).await,
            server.shutdown_signals.clone(),
            server.soft_interrupt_queues.clone(),
            server.await_members_runtime.clone(),
            server.swarm_mutation_runtime.clone(),
        ),
    );
    (client, task)
}

async fn retirement_terminal(
    reader: &mut tokio::io::BufReader<crate::transport::ReadHalf>,
    id: u64,
) -> ServerEvent {
    use tokio::io::AsyncBufReadExt;
    timeout(Duration::from_secs(10), async {
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0, "connection closed before {id}");
            let event: ServerEvent = serde_json::from_str(&line).unwrap();
            if matches!(&event, ServerEvent::Error {id: value, ..} | ServerEvent::Done {id: value} | ServerEvent::Pong {id: value} if *value == id) {
                return event;
            }
        }
    }).await.expect("retirement request must not wait for model work")
}

fn assert_retired(event: ServerEvent) {
    match event {
        ServerEvent::Error { message, .. } => assert_eq!(message, crate::config::SWARM_UNAVAILABLE),
        other => panic!("expected globally unavailable error, got {other:?}"),
    }
}

#[tokio::test]
async fn swarm_retirement_all_first_request_protocols_reject_without_sessions_or_models() {
    use tokio::io::AsyncWriteExt;
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _env = configure_test_env(&root);
    let _off = ScopedEnvVar::set("JCODE_SWARM_ENABLED", "false");
    let provider = Arc::new(StreamingMockProvider::default());
    let server = Server::new(provider.clone());
    for request in retirement_requests() {
        assert!(request.is_swarm_request());
        let (client, task) = retirement_connection(&server).await;
        let (reader, mut writer) = client.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        writer
            .write_all((serde_json::to_string(&request).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
        assert_retired(retirement_terminal(&mut reader, request.id()).await);
        drop(writer);
        task.await.unwrap().unwrap();
        assert!(server.sessions.read().await.is_empty());
        assert!(server.client_connections.read().await.is_empty());
    }
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(server.swarm_state.members.read().await.is_empty());
    assert!(server.swarm_state.plans.read().await.is_empty());
}

#[tokio::test]
async fn swarm_retirement_attached_protocols_and_busy_enable_reject_preserving_presence() {
    use tokio::io::AsyncWriteExt;
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _env = configure_test_env(&root);
    let _off = ScopedEnvVar::set("JCODE_SWARM_ENABLED", "false");
    let provider = Arc::new(StreamingMockProvider::default());
    let server = Server::new(provider.clone());
    let (client, task) = retirement_connection(&server).await;
    let (reader, mut writer) = client.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let subscribe = serde_json::json!({"type":"subscribe", "id":1, "working_dir":root.path()});
    writer
        .write_all((subscribe.to_string() + "\n").as_bytes())
        .await
        .unwrap();
    assert!(matches!(
        retirement_terminal(&mut reader, 1).await,
        ServerEvent::Done { .. }
    ));
    assert_eq!(server.sessions.read().await.len(), 1);
    assert_eq!(server.swarm_state.members.read().await.len(), 1);
    assert!(server.swarm_state.swarms_by_id.read().await.is_empty());
    for member in server.swarm_state.members.read().await.values() {
        assert!(!member.swarm_enabled && member.swarm_id.is_none());
        assert!(!member.event_txs.is_empty());
    }
    let agent = server
        .sessions
        .read()
        .await
        .values()
        .next()
        .unwrap()
        .clone();
    let held = agent.lock().await;
    for request in retirement_requests() {
        writer
            .write_all((serde_json::to_string(&request).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
        assert_retired(retirement_terminal(&mut reader, request.id()).await);
    }
    writer
        .write_all(
            b"{\"type\":\"set_feature\",\"id\":500,\"feature\":\"swarm\",\"enabled\":true}\n",
        )
        .await
        .unwrap();
    assert_retired(retirement_terminal(&mut reader, 500).await);
    drop(held);
    writer
        .write_all(b"{\"type\":\"ping\",\"id\":501}\n")
        .await
        .unwrap();
    assert!(matches!(
        retirement_terminal(&mut reader, 501).await,
        ServerEvent::Pong { .. }
    ));
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(server.sessions.read().await.len(), 1);
    drop(writer);
    drop(reader);
    timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn swarm_retirement_internal_workers_and_async_debug_reject_before_agent_lock() {
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _env = configure_test_env(&root);
    let _off = ScopedEnvVar::set("JCODE_SWARM_ENABLED", "false");
    let provider = Arc::new(StreamingMockProvider::default());
    let agent = Arc::new(Mutex::new(Agent::new_with_disabled_startup_context(
        provider.clone(),
        Registry::empty(),
        None,
    )));
    let held = agent.lock().await;
    let jobs = Arc::new(RwLock::new(HashMap::new()));
    let result = timeout(
        Duration::from_secs(1),
        crate::server::swarm::run_swarm_task(agent.clone(), "fixture", "general", "task"),
    )
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        crate::config::SWARM_UNAVAILABLE
    );
    let result = timeout(
        Duration::from_secs(1),
        crate::server::swarm::run_swarm_message(agent.clone(), "task"),
    )
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        crate::config::SWARM_UNAVAILABLE
    );
    let result = timeout(
        Duration::from_secs(1),
        crate::server::debug_jobs::maybe_start_async_debug_job(
            agent.clone(),
            "swarm_message_async:task",
            jobs.clone(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        crate::config::SWARM_UNAVAILABLE
    );
    assert!(jobs.read().await.is_empty());
    assert!(provider.requests.lock().unwrap().is_empty());
    drop(held);
}
