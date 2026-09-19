use super::*;

#[tokio::test]
async fn subscribe_preserves_cwd_and_history_when_idle_and_busy() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir()?;
    let original = root.path().join("original");
    let reported = root.path().join("other-client-directory");
    std::fs::create_dir(&original)?;
    std::fs::create_dir(&reported)?;
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::new_with_initial_working_dir(
        provider,
        registry.clone(),
        original.to_str(),
    )));
    let (session_id, before) = {
        let mut agent = agent.lock().await;
        agent.startup_context_session_mut().save()?;
        (
            agent.session_id().to_string(),
            serde_json::to_vec(agent.messages())?,
        )
    };
    let members = Arc::new(RwLock::new(HashMap::new()));
    let swarms = Arc::new(RwLock::new(HashMap::new()));
    let channels = Arc::new(RwLock::new(HashMap::new()));
    let session_channels = Arc::new(RwLock::new(HashMap::new()));
    let plans = Arc::new(RwLock::new(HashMap::new()));
    let coordinators = Arc::new(RwLock::new(HashMap::new()));
    let history = Arc::new(RwLock::new(VecDeque::new()));
    let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_events, _) = broadcast::channel(16);
    let (events, mut received) = mpsc::unbounded_channel();
    let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let mut selfdev = false;
    let home = dirs::home_dir().expect("isolated test home");
    for (reported, busy) in [
        (&reported, false),
        (&reported, true),
        (&home, false),
        (&home, true),
        (&original, false),
    ] {
        let guard = if busy { Some(agent.lock().await) } else { None };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_subscribe(
                7,
                Some(reported.to_string_lossy().into_owned()),
                None,
                false,
                &mut selfdev,
                &session_id,
                "location-client",
                &None,
                &agent,
                &registry,
                false,
                &members,
                &swarms,
                &channels,
                &session_channels,
                &plans,
                &coordinators,
                &events.clone().into(),
                &pool,
                &history,
                &counter,
                &swarm_events,
            ),
        )
        .await?;
        drop(guard);
        tokio::task::yield_now().await;
        let agent = agent.lock().await;
        assert_eq!(agent.working_dir(), original.to_str());
        assert_eq!(serde_json::to_vec(agent.messages())?, before);
        assert_eq!(
            members.read().await[&session_id].working_dir.as_deref(),
            Some(original.as_path())
        );
        assert!(
            std::iter::from_fn(|| received.try_recv().ok())
                .any(|event| matches!(event, ServerEvent::Done { id: 7 }))
        );
    }
    Ok(())
}
