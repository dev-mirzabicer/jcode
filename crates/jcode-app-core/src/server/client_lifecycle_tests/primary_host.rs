use super::*;

#[derive(Clone, Default)]
struct GatedClientProvider {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for GatedClientProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        let control = self.clone();
        Ok(Box::pin(async_stream::stream! {
            yield Ok(StreamEvent::TextDelta("fixture prefix ".into()));
            control.entered.notify_one();
            control.release.notified().await;
            yield Ok(StreamEvent::TextDelta("fixture suffix".into()));
            yield Ok(StreamEvent::MessageEnd { stop_reason: None });
        }))
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn slow_client_overflow_does_not_stop_the_primary() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let control = GatedClientProvider::default();
        let provider: Arc<dyn Provider> = Arc::new(control.clone());
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider,registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(session.clone(),agent.clone())])));
        let status = status_fixture(&session);
        let members = status.members.clone();
        let (observer,_receiver,disconnected) = crate::client_delivery::ClientEventSender::bounded_client();
        observer.retarget(&session);
        crate::server::register_session_event_sender(&members,&session,"slow",observer.clone()).await;
        assert!(crate::server::live_turn::run_live_turn_if_idle(&session,"fixture input",None,&host,status).await);
        tokio::time::timeout(Duration::from_secs(5),control.entered.notified()).await?;
        for id in 0..1024 { if observer.send(ServerEvent::Done{id}).is_err() { break; } }
        assert!(disconnected.is_cancelled());
        assert!(host.processing(&session).is_some());
        control.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10),host.wait_idle(&session)).await??;
        let stored = Session::load(&session)?;
        assert!(stored.messages.iter().flat_map(|m| &m.content).any(|block| matches!(block,ContentBlock::Text{text,..} if text == "fixture prefix fixture suffix")));
        assert_eq!(members.read().await[&session].status,"ready");
        host.shutdown().await
    })
}

#[test]
fn resume_all_rejected_admission_preserves_recovery_intent() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    tokio::runtime::Runtime::new()?.block_on(async {
        let recorder = Arc::new(RecordingImmediateProvider::default());
        let provider: Arc<dyn Provider> = recorder.clone();
        let registry = Registry::new(provider.clone()).await;
        let mut agent = Agent::new(provider, registry);
        agent.add_message(
            crate::message::Role::User,
            vec![ContentBlock::Text {
                text: "pending fixture input".into(),
                cache_control: None,
            }],
        );
        let session = agent.session_id().to_string();
        let agent = Arc::new(Mutex::new(agent));
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        crate::server::reload_recovery::persist_intent(
            "fixture-resume-race",
            &session,
            crate::server::reload_recovery::ReloadRecoveryRole::InterruptedPeer,
            crate::tool::selfdev::ReloadRecoveryDirective {
                reconnect_notice: None,
                continuation_message: "fixture continuation".into(),
            },
            "fixture",
        )?;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (finish, release) = tokio::sync::oneshot::channel();
        host.start(
            host.admit(&session, 1, agent)?,
            |_| async { Ok(None) },
            move |_| async move {
                let _ = entered.send(());
                let _ = release.await;
            },
        );
        ready.await?;
        let status = status_fixture(&session);
        let (tx, mut rx) = mpsc::unbounded_channel();
        crate::server::client_actions::handle_resume_all_sessions(
            42,
            &host,
            &status.members,
            &status.swarms_by_id,
            &status.event_history,
            &status.event_counter,
            &status.event_tx,
            &tx.clone().into(),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerEvent::ResumeAllResult {
                resumed: 0,
                skipped: 1,
                ..
            })
        ));
        assert!(crate::server::reload_recovery::has_pending_for_session(
            &session
        ));
        assert!(recorder.snapshots.lock().unwrap().is_empty());
        finish.send(()).unwrap();
        host.wait_idle(&session).await?;
        host.shutdown().await
    })
}

#[tokio::test]
async fn departed_origin_cannot_receive_detached_primary_events() -> Result<()> {
    let session = "session_detached_origin";
    let status = status_fixture(session);
    let (origin, mut received) = mpsc::unbounded_channel();
    crate::server::register_session_event_sender(
        &status.members,
        session,
        "client",
        origin.clone().into(),
    )
    .await;
    assert_eq!(
        crate::server::fanout_session_event(&status.members, session, ServerEvent::Done { id: 1 })
            .await,
        1
    );
    assert!(matches!(
        received.recv().await,
        Some(ServerEvent::Done { id: 1 })
    ));
    crate::server::unregister_session_event_sender(&status.members, session, "client").await;
    assert_eq!(
        crate::server::fanout_session_event(&status.members, session, ServerEvent::Done { id: 2 })
            .await,
        0
    );
    let stream = crate::server::state::session_event_fanout_sender_with_fallback(
        session.into(),
        status.members.clone(),
        origin.into(),
    );
    stream.send(ServerEvent::Done { id: 3 })?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), received.recv())
            .await
            .is_err()
    );
    Ok(())
}

#[test]
fn stop_reserved_primary_prevents_dispatch() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::default());
        let admission = host.admit(&session, 42, agent)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (stopped, ()) = tokio::join!(biased;
            host.stop(&session),
            async {
                host.start(admission,
                    |_| async { panic!("stopped reservation must never dispatch") },
                    |outcome| async move { let _ = tx.send((outcome.interrupted,outcome.result.is_err())); },
                );
            }
        );
        assert!(stopped?);
        assert_eq!(rx.await?, (true,true));
        assert!(host.processing(&session).is_none());
        host.shutdown().await
    })
}

#[test]
fn restore_primary_preserves_busy_source_and_target_snapshots() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(RecordingImmediateProvider::default());
        let registry = Registry::new(provider.clone()).await;
        let source = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
        let source_id = source.lock().await.session_id().to_string();
        let target_provider = provider.fork_for_new_session();
        let registry = Registry::new(target_provider.clone()).await;
        let (mut target, _) = Agent::new_with_startup_context(
            target_provider,
            registry,
            None,
            crate::agent::StartupContextActivation::primary(
                crate::agent::StartupContextCaller::HarnessApi,
            ),
        )?;
        target.startup_context_session_mut().save()?;
        let target_id = target.session_id().to_string();
        let target_before = serde_json::to_vec(target.messages())?;
        let system_before = serde_json::to_vec(&target.startup_context_session().system_prompt)?;
        drop(target);
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            source_id.clone(),
            source.clone(),
        )])));
        let source_guard = source.lock().await;
        let source_before = serde_json::to_vec(source_guard.messages())?;
        let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
        tokio::time::timeout(
            Duration::from_secs(10),
            host.restore(
                &target_id,
                &provider,
                &pool,
                &crate::instruction::InstructionRepositoryService::new(),
            ),
        )
        .await??;
        assert_eq!(source_guard.session_id(), source_id);
        assert_eq!(serde_json::to_vec(source_guard.messages())?, source_before);
        let target = host.read().await[&target_id].clone();
        let target = target.lock().await;
        assert_eq!(serde_json::to_vec(target.messages())?, target_before);
        assert_eq!(
            serde_json::to_vec(&target.startup_context_session().system_prompt)?,
            system_before
        );
        assert!(host.read().await.contains_key(&source_id));
        assert!(!Arc::ptr_eq(
            &source_guard.provider_handle(),
            &target.provider_handle()
        ));
        Ok(())
    })
}

fn status_fixture(session: &str) -> crate::server::live_turn::LiveTurnSwarmContext {
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let members = Arc::new(RwLock::new(HashMap::from([(
        session.to_string(),
        SwarmMember {
            session_id: session.to_string(),
            event_tx: tx.into(),
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".into(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".into(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: Default::default(),
        },
    )])));
    crate::server::live_turn::LiveTurnSwarmContext::new(
        &members,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(std::collections::VecDeque::new())),
        &Arc::new(AtomicU64::new(0)),
        &broadcast::channel(8).0,
    )
}

#[test]
fn detached_host_completes_real_agent_turn_without_client_receiver() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(FanoutStreamProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let status = status_fixture(&session);
        let members = status.members.clone();
        assert!(
            crate::server::live_turn::run_live_turn_if_idle(
                &session,
                "detached fixture",
                None,
                &host,
                status
            )
            .await
        );
        assert!(host.processing(&session).is_some());
        assert!(host.admit(&session, 7, agent.clone()).is_err());
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle(&session)).await??;
        assert_eq!(members.read().await[&session].status, "ready");
        assert!(
            members.read().await[&session]
                .latest_completion_report
                .as_ref()
                .unwrap()
                .contains("after attach")
        );
        let loaded = Session::load(&session)?;
        assert!(
            loaded
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| matches!(b,ContentBlock::Text{text,..} if text.contains("after attach")))
        );
        assert!(host.read().await.contains_key(&session));
        host.shutdown().await
    })
}

#[test]
fn detached_host_stop_reaches_real_stream_and_clears_only_its_turn() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(NeverEndingStreamProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let status = status_fixture(&session);
        let members = status.members.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        crate::server::register_session_event_sender(&members, &session, "observer", tx.into())
            .await;
        assert!(
            crate::server::live_turn::run_live_turn_if_idle(
                &session,
                "stream until stopped",
                None,
                &host,
                status
            )
            .await
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(rx.recv().await, Some(ServerEvent::TextDelta { .. })) {}
        })
        .await?;
        drop(rx);
        assert!(host.stop(&session).await?);
        assert!(host.processing(&session).is_none());
        assert_eq!(members.read().await[&session].status, "stopped");
        assert!(!agent.lock().await.graceful_shutdown_signal().is_set());
        assert!(crate::turn_cancel_registry::active_turn_signals(&session).is_empty());
        let next = host.admit(&session, 19, agent.clone())?;
        drop(next);
        assert!(host.processing(&session).is_none());
        assert!(!host.stop(&session).await?);
        host.shutdown().await
    })
}

#[test]
fn primary_admission_and_kernel_owner_exclude_second_writer() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_string();
        let first = Arc::new(crate::primary::PrimaryHost::default());
        let second = Arc::new(crate::primary::PrimaryHost::default());
        let admission = first.admit(&session, 1, agent.clone())?;
        assert_eq!(first.processing(&session), Some(1));
        assert!(first.admit(&session, 2, agent.clone()).is_err());
        drop(admission);
        assert!(first.processing(&session).is_none());
        assert!(second.admit(&session, 3, agent.clone()).is_err());
        drop(first);
        let admitted = second.admit(&session, 4, agent)?;
        drop(admitted);
        Ok(())
    })
}
