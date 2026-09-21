use super::*;
use anyhow::{Result, anyhow};

#[tokio::test]
async fn handle_clear_session_replaces_runtime_handles_and_updates_shutdown_registration()
-> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("test home");
    let previous_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());
    let project = home.path().join("project");
    std::fs::create_dir(&project)?;
    std::fs::write(project.join("required.txt"), "clear startup snapshot")?;
    let startup = crate::startup_context::StartupContext::new();
    let active = startup.resolve_project(&project)?;
    let preview = startup.preview_selection(
        &active,
        [crate::startup_context::StartupSelectionInput::new(
            "required.txt",
        )],
    );
    startup.save_project_plan(&active, 0, &preview)?;

    let old_session_id = "session_before_clear";
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry.clone(),
        old_session_id,
        Vec::new(),
    )));
    agent
        .lock()
        .await
        .set_working_dir_for_pending_context(Some(project.to_string_lossy().into_owned()));
    {
        let mut guard = agent.lock().await;
        guard
            .activate_primary_instructions(crate::instruction::AgentSelection::Default)
            .expect("activate clear source agent");
    }
    let active_before = agent
        .lock()
        .await
        .active_agent()
        .cloned()
        .expect("active clear source agent");
    let common_path = home.path().join("instructions/system/common.md");
    let common = crate::instruction::InstructionDocument {
        id: crate::instruction::InstructionId::parse("common").expect("id"),
        kind: crate::instruction::InstructionKind::System,
        scope: crate::instruction::InstructionScope::Global,
        template_mode: crate::instruction::TemplateMode::Plain,
        metadata: crate::instruction::InstructionMetadata::default(),
        body: "CLEAR_CURRENT_SOURCE".to_string(),
        path: std::path::PathBuf::from("system/common.md"),
    };
    std::fs::write(common_path, common.to_markdown()?).expect("edit clear source");

    let old_queue = {
        let guard = agent.lock().await;
        guard.soft_interrupt_queue()
    };
    let old_background_signal = {
        let guard = agent.lock().await;
        guard.background_tool_signal()
    };
    let old_cancel_signal = {
        let guard = agent.lock().await;
        guard.graceful_shutdown_signal()
    };

    let sessions = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
        old_session_id.to_string(),
        Arc::clone(&agent),
    )])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::from([(
        old_session_id.to_string(),
        old_cancel_signal.clone(),
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::from([(
        old_session_id.to_string(),
        old_queue.clone(),
    )])));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_clear".to_string(),
        ClientConnectionInfo {
            client_id: "conn_clear".to_string(),
            session_id: old_session_id.to_string(),
            client_instance_id: None,
            debug_client_id: Some("debug_clear".to_string()),
            connected_at: now,
            last_seen: now,
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        old_session_id.to_string(),
        test_swarm_member(old_session_id, "ready"),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm-test".to_string(),
        HashSet::from([old_session_id.to_string()]),
    )])));
    let _file_touch = FileTouchService::new();
    let _channel_subscriptions = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let _channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::from([(
        "swarm-test".to_string(),
        VersionedPlan {
            items: Vec::new(),
            version: 1,
            participants: HashSet::from([old_session_id.to_string()]),
            task_progress: HashMap::new(),
            mode: "deep".to_string(),
            node_meta: HashMap::new(),
        },
    )])));
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let _context_transactions = crate::context::ContextTransactionService::new();
    let instruction_repositories = crate::instruction::InstructionRepositoryService::new();

    let mut client_session_id = old_session_id.to_string();
    let source = agent.clone();
    let source_before = serde_json::to_vec(source.lock().await.messages())?;
    let agent = handle_clear_session(
        7,
        None,
        &mut client_session_id,
        "conn_clear",
        &source,
        &instruction_repositories,
        &sessions,
        &crate::server::startup_context::test_coordinator(),
        &Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        &shutdown_signals,
        &soft_interrupt_queues,
        &client_connections,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &client_event_tx.clone().into(),
    )
    .await;
    assert_eq!(source.lock().await.session_id(), old_session_id);
    assert_eq!(
        serde_json::to_vec(source.lock().await.messages())?,
        source_before
    );
    assert!(!Arc::ptr_eq(&source, &agent));
    assert!(!Arc::ptr_eq(
        &source.lock().await.provider_handle(),
        &agent.lock().await.provider_handle()
    ));

    assert_ne!(client_session_id, old_session_id);
    let cleared = crate::session::Session::load(&client_session_id)?;
    assert_eq!(cleared.active_agent(), Some(&active_before));
    assert!(
        cleared
            .system_prompt_text()
            .expect("cleared system prompt")
            .contains("CLEAR_CURRENT_SOURCE")
    );
    assert!(swarm_members.read().await.contains_key(old_session_id));
    assert!(swarm_members.read().await.contains_key(&client_session_id));
    assert!(
        swarm_plans.read().await["swarm-test"]
            .participants
            .contains(old_session_id)
    );
    assert!(
        !swarm_plans.read().await["swarm-test"]
            .participants
            .contains(&client_session_id)
    );

    old_queue
        .lock()
        .map_err(|_| anyhow!("old queue lock"))?
        .push(jcode_agent_runtime::SoftInterruptMessage {
            content: "stale queued message".to_string(),
            images: Vec::new(),
            urgent: false,
            source: jcode_agent_runtime::SoftInterruptSource::User,
            unattended_context: None,
        });
    old_background_signal.fire();
    old_cancel_signal.fire();

    let (new_queue, new_background_signal, new_cancel_signal) = {
        let guard = agent.lock().await;
        (
            guard.soft_interrupt_queue(),
            guard.background_tool_signal(),
            guard.graceful_shutdown_signal(),
        )
    };

    assert!(!Arc::ptr_eq(&old_queue, &new_queue));
    assert!(!new_background_signal.is_set());
    assert!(!new_cancel_signal.is_set());
    assert!(!agent.lock().await.has_soft_interrupts());
    let startup_receipt = agent
        .lock()
        .await
        .startup_context_session()
        .startup_context
        .clone()
        .expect("clear should recapture Startup Context");
    assert_eq!(startup_receipt.batches[0].files.len(), 1);

    let queue_map = soft_interrupt_queues.read().await;
    assert!(queue_map.contains_key(old_session_id));
    assert!(queue_map.contains_key(&client_session_id));
    drop(queue_map);

    let signals = shutdown_signals.read().await;
    assert!(signals.contains_key(old_session_id));
    let registered_signal = signals
        .get(&client_session_id)
        .ok_or_else(|| anyhow!("new session should have shutdown signal"))?
        .clone();
    drop(signals);
    registered_signal.fire();
    assert!(new_cancel_signal.is_set());

    let first = client_event_rx
        .recv()
        .await
        .ok_or_else(|| anyhow!("session id event"))?;
    assert!(matches!(first, ServerEvent::SessionId { .. }));
    let second = client_event_rx
        .recv()
        .await
        .ok_or_else(|| anyhow!("done event"))?;
    assert!(matches!(second, ServerEvent::Done { id: 7 }));
    if let Some(previous_home) = previous_home {
        crate::env::set_var("JCODE_HOME", previous_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    Ok(())
}
