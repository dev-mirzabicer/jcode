use super::*;

#[derive(Clone, Default)]
struct DurableInputProvider {
    snapshots: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
}
#[async_trait]
impl Provider for DurableInputProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.snapshots.lock().unwrap().push(messages.to_vec());
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("synthetic completion".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }
    fn name(&self) -> &str {
        "durable-input-fixture"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn durable_primary_input_detached_replay_and_busy_boundary() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let recorder = Arc::new(DurableInputProvider::default());
        let provider: Arc<dyn Provider> = recorder.clone();
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_owned();
        agent.lock().await.startup_context_session_mut().save()?;
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(session.clone(), agent.clone())])));
        let status = status_fixture(&session);
        let input = jcode_session_types::PrimaryInputEnvelope {
            id: crate::workspace::RequestId::new(), session: session.clone(), delivery: jcode_session_types::PrimaryInputDelivery::SafeBoundary,
            content: "durable synthetic input".into(), images: vec![("image/png".into(), "ZmFrZQ==".into())], display_role: None,
            origin: Some(jcode_session_types::StoredMessageOrigin::Human), system_reminder: None, unattended_context: None, urgent: false,
        };
        let accepted = crate::server::live_turn::submit_primary_input(&host, input.clone(), status.clone()).await?;
        assert_eq!(accepted.state, jcode_session_types::PrimaryInputState::Accepted);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let receipt = crate::primary_input::PrimaryInputStore::current().inspect(&session, input.id)?;
                anyhow::ensure!(receipt.state != jcode_session_types::PrimaryInputState::Failed, "delivery failed: {receipt:?}");
                if receipt.state == jcode_session_types::PrimaryInputState::Committed && host.processing(&session).is_none() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|error| anyhow::anyhow!("first delivery deadline: {error}; receipt={:?}; processing={:?}; calls={}", crate::primary_input::PrimaryInputStore::current().inspect(&session, input.id), host.processing(&session), recorder.snapshots.lock().unwrap().len()))??;
        let committed = crate::server::live_turn::submit_primary_input(&host, input.clone(), status.clone()).await?;
        assert_eq!(committed.state, jcode_session_types::PrimaryInputState::Committed);
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 1);
        assert!(recorder.snapshots.lock().unwrap()[0].iter().flat_map(|message| &message.content).any(|block| matches!(block, ContentBlock::Image { media_type, data } if media_type == "image/png" && data == "ZmFrZQ==")));
        let mut conflict = input.clone(); conflict.content.push('!');
        assert!(crate::server::live_turn::submit_primary_input(&host, conflict, status.clone()).await.is_err());
        // Reserve a real host turn while accepting another input. It remains
        // durable even though no observer or client owns its delivery.
        let (release, wait) = tokio::sync::oneshot::channel();
        host.start(host.admit(&session, 44, agent.clone())?, move |_| async move { wait.await?; Ok(None) }, |_| async {});
        let mut busy = input.clone(); busy.id = crate::workspace::RequestId::new(); busy.content = "queued durable input".into();
        crate::server::live_turn::submit_primary_input(&host, busy.clone(), status.clone()).await?;
        assert_eq!(crate::primary_input::PrimaryInputStore::current().inspect(&session, busy.id)?.state, jcode_session_types::PrimaryInputState::Accepted);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if crate::primary_input::PrimaryInputStore::current().inspect(&session, busy.id)?.state == jcode_session_types::PrimaryInputState::Committed && host.processing(&session).is_none() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 2);
        let saved = Session::load(&session)?;
        assert_eq!(saved.primary_inputs.len(), 2);
        assert_eq!(saved.messages.iter().flat_map(|m| &m.content).filter(|block| matches!(block,ContentBlock::Text{text,..} if text == "durable synthetic input")).count(), 1);
        host.shutdown().await
    })
}

#[tokio::test]
async fn primary_stdin_survives_observer_loss_and_rejects_foreign_or_duplicate_answers()
-> Result<()> {
    let host = Arc::new(crate::primary::PrimaryHost::default());
    let session = "stdin-primary-fixture";
    let status = status_fixture(session);
    let input = host.stdin(session, || {
        crate::server::primary_stdin::PrimaryStdin::new(session.into(), status.members.clone())
    });
    let (events, mut observer) = mpsc::unbounded_channel();
    crate::server::register_session_event_sender(&status.members, session, "old", events.into())
        .await;
    let (response, answer) = tokio::sync::oneshot::channel();
    input.sender().send(crate::tool::StdinInputRequest {
        request_id: "fixture-input".into(),
        prompt: "synthetic input prompt".into(),
        is_password: true,
        response_tx: response,
    })?;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), observer.recv()).await?,
        Some(ServerEvent::StdinRequest {
            is_password: true,
            ..
        })
    ));
    crate::server::unregister_session_event_sender(&status.members, session, "old").await;
    drop(observer);
    assert_eq!(host.pending_stdin(session).len(), 1);
    let (events, mut observer) = mpsc::unbounded_channel();
    let events: crate::client_delivery::ClientEventSender = events.into();
    crate::server::client_actions::handle_stdin_response(
        3,
        "fixture-input".into(),
        "not accepted".into(),
        "other-primary",
        &host,
        &events,
    )
    .await;
    assert!(matches!(
        observer.recv().await,
        Some(ServerEvent::Error { id: 3, .. })
    ));
    // The pending authority lives in the retained service even after the old
    // observer disappears. No input body is placed in its discovery event.
    assert_eq!(input.pending().len(), 1);
    crate::server::client_actions::handle_stdin_response(
        4,
        "fixture-input".into(),
        "synthetic private answer".into(),
        session,
        &host,
        &events,
    )
    .await;
    assert!(matches!(
        observer.recv().await,
        Some(ServerEvent::Done { id: 4 })
    ));
    assert_eq!(answer.await?, "synthetic private answer");
    assert!(input.respond("fixture-input", "duplicate".into()).is_err());
    assert!(input.pending().is_empty());
    host.shutdown().await?;
    Ok(())
}

#[test]
fn stopped_snapshot_retains_partial_output_without_fabricating_source() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let gate = GatedClientProvider::default();
        let provider: Arc<dyn Provider> = Arc::new(gate);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let status = status_fixture(&session);
        let (tx, mut rx) = mpsc::unbounded_channel();
        crate::server::register_session_event_sender(
            &status.members,
            &session,
            "observer",
            tx.into(),
        )
        .await;
        assert!(
            crate::server::live_turn::run_live_turn_if_idle(
                &session,
                "fixture input",
                None,
                &host,
                status
            )
            .await
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = rx.recv().await {
                if matches!(event, ServerEvent::TextDelta { .. }) {
                    return;
                }
            }
            panic!("provider closed before its prefix");
        })
        .await?;
        assert!(host.stop(&session).await?);
        let snapshot = host.presentation(&session).snapshot().await?;
        let canonical = snapshot
            .session
            .messages
            .iter()
            .filter(|message| message.role == crate::message::Role::Assistant)
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        let replay = snapshot
            .events
            .iter()
            .filter_map(|event| match event {
                ServerEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(format!("{canonical}{replay}"), "fixture prefix ");
        assert!(!snapshot.processing);
        assert!(
            snapshot
                .events
                .iter()
                .any(|event| matches!(event, ServerEvent::Interrupted))
        );
        let seen = host
            .presentation(&session)
            .recovery_events(&snapshot.session, Some(&snapshot.cursor));
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, ServerEvent::Done { .. } | ServerEvent::Interrupted))
        );
        host.shutdown().await
    })
}

#[test]
fn busy_snapshot_replays_prefix_once_then_continues_after_its_cursor() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let gate=GatedClientProvider::default();
        let provider:Arc<dyn Provider>=Arc::new(gate.clone());
        let registry=Registry::new(provider.clone()).await;
        let agent=Arc::new(Mutex::new(Agent::new(provider.clone(),registry)));
        let session=agent.lock().await.session_id().to_string();
        let host=Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(session.clone(),agent.clone())])));
        let status=status_fixture(&session);
        let members=status.members.clone();
        let (observer,mut events,_)=crate::client_delivery::ClientEventSender::bounded_client();
        observer.enable_primary_stream();
        observer.retarget(&session);
        crate::server::register_session_event_sender(&members,&session,"observer",observer.clone()).await;
        assert!(crate::server::live_turn::run_live_turn_if_idle(&session,"fixture input",None,&host,status).await);
        let prefix=tokio::time::timeout(Duration::from_secs(10),async {
            loop { let event=events.recv().await.unwrap(); if matches!(&event.event,ServerEvent::TextDelta{text} if text=="fixture prefix ") {break event;} }
        }).await?;
        let (stream,mut reader)=crate::transport::stream_pair()?;
        let (_,writer)=stream.into_split();
        let writer=Arc::new(Mutex::new(writer));
        crate::server::client_state::handle_get_history(41,&session,true,&agent,&crate::server::startup_context::test_coordinator(),&provider,&host,&Arc::new(RwLock::new(HashMap::new())),&Arc::new(RwLock::new(1)),&writer,"fixture","",None,Some(&observer)).await?;
        assert!(!prefix.is_current(),"queued prefix is already represented by the snapshot replay");
        drop(writer);
        let mut bytes=Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader,&mut bytes).await?;
        let frames=String::from_utf8(bytes)?.lines().map(serde_json::from_str::<serde_json::Value>).collect::<std::result::Result<Vec<_>,_>>()?;
        assert_eq!(frames[0]["type"],"history");
        assert!(frames[0]["messages"].as_array().unwrap().iter().any(|message|message["content"].as_str().is_some_and(|content|content.contains("fixture input"))));
        assert_eq!(frames[0]["primary_stream"]["phase"],"snapshot");
        assert_eq!(frames[0]["primary_stream"]["replay_events"].as_u64().unwrap() as usize,frames.len()-1);
        let text=frames.iter().filter(|frame|frame["type"]=="text_delta").filter_map(|frame|frame["text"].as_str()).collect::<String>();
        assert_eq!(text,"fixture prefix ");
        assert!(frames.iter().all(|frame|frame["type"]!="primary_checkpoint"));
        gate.release.notify_one();
        let suffix=tokio::time::timeout(Duration::from_secs(10),async {
            loop { let event=events.recv().await.unwrap(); if matches!(&event.event,ServerEvent::TextDelta{text} if text=="fixture suffix") {break event;} }
        }).await?;
        assert!(suffix.is_current());
        let live:serde_json::Value=serde_json::from_str(&suffix.json)?;
        assert!(live["primary_stream"]["cursor"]["sequence"].as_u64().unwrap()>frames[0]["primary_stream"]["cursor"]["sequence"].as_u64().unwrap());
        host.wait_idle(&session).await?;
        let final_view=host.presentation(&session).snapshot().await?;
        assert!(!final_view.processing);
        assert!(matches!(&final_view.events[..],[ServerEvent::Done{id:0}]));
        assert!(final_view.session.messages.iter().flat_map(|message|&message.content).any(|block|matches!(block,ContentBlock::Text{text,..} if text=="fixture prefix fixture suffix")));
        host.shutdown().await
    })
}

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
        let agent = Arc::new(Mutex::new(Agent::new(provider.clone(),registry)));
        let session = agent.lock().await.session_id().to_string();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(session.clone(),agent.clone())])));
        let status = status_fixture(&session);
        let members = status.members.clone();
        let (observer,_receiver,disconnected) = crate::client_delivery::ClientEventSender::bounded_client();
        observer.enable_primary_stream();
        observer.retarget(&session);
        crate::server::register_session_event_sender(&members,&session,"slow",observer.clone()).await;
        assert!(crate::server::live_turn::run_live_turn_if_idle(&session,"fixture input",None,&host,status).await);
        tokio::time::timeout(Duration::from_secs(5),control.entered.notified()).await?;
        let (stream,_peer)=crate::transport::stream_pair()?;
        let (_,writer)=stream.into_split();
        let writer=Arc::new(Mutex::new(writer));
        let held_writer=writer.lock().await;
        let startup=crate::server::startup_context::test_coordinator();
        let connections=Arc::new(RwLock::new(HashMap::new()));
        let count=Arc::new(RwLock::new(1));
        let snapshot=crate::server::client_state::handle_get_history(77,&session,true,&agent,&startup,&provider,&host,&connections,&count,&writer,"fixture","",None,Some(&observer));
        tokio::pin!(snapshot);
        assert!(tokio::time::timeout(Duration::from_millis(10),snapshot.as_mut()).await.is_err());
        for id in 0..1024 { if observer.send(ServerEvent::Done{id}).is_err() { break; } }
        assert!(disconnected.is_cancelled());
        assert!(tokio::time::timeout(Duration::from_secs(1),snapshot.as_mut()).await?.is_err());
        drop(held_writer);
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
    let output = crate::server::primary_output::PrimaryOutput::new(
        session.into(),
        Arc::new(crate::primary::presentation::Presentation::new(session)),
        status.members.clone(),
        Some(origin.into()),
    );
    let stream = output.tx.clone();
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

#[test]
fn notify_session_terminal_race_starts_only_unconsumed_detached_input() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let gate = GatedClientProvider::default();
        let provider: Arc<dyn Provider> = Arc::new(gate.clone());
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_owned();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let status = status_fixture(&session);
        let admission = host.admit(&session, 9, agent.clone())?;
        let (at_terminal, terminal) = tokio::sync::oneshot::channel();
        let (release, finish) = tokio::sync::oneshot::channel();
        host.start(
            admission,
            |_| async { Ok(None) },
            move |_| async move {
                let _ = at_terminal.send(());
                let _ = finish.await;
            },
        );
        terminal.await?;
        let (tx, mut rx) = crate::client_delivery::local_event_channel();
        crate::server::client_actions::handle_notify_session(
            81,
            session.clone(),
            "terminal notification fixture".into(),
            None,
            crate::server::client_actions::NotifySessionContext {
                sessions: &host,
                swarm_members: &status.members,
                swarms_by_id: &status.swarms_by_id,
                event_history: &status.event_history,
                event_counter: &status.event_counter,
                swarm_event_tx: &status.event_tx,
                client_event_tx: &tx,
            },
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerEvent::Done { id: 81 })
        ));
        assert_eq!(
            crate::primary_input::PrimaryInputStore::current()
                .pending(&session)?
                .len(),
            1
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(30), gate.entered.notified()).await?;
        gate.release.notify_one();
        host.wait_idle(&session).await?;
        let guard = agent.lock().await;
        assert_eq!(
            guard
                .messages()
                .iter()
                .filter(|message| message
                    .content_preview()
                    .contains("terminal notification fixture"))
                .count(),
            1
        );
        assert!(
            crate::primary_input::PrimaryInputStore::current()
                .pending(&session)?
                .is_empty()
        );
        drop(guard);
        crate::server::client_actions::handle_notify_session(
            82,
            "missing-notification-target".into(),
            "must not be accepted".into(),
            None,
            crate::server::client_actions::NotifySessionContext {
                sessions: &host,
                swarm_members: &status.members,
                swarms_by_id: &status.swarms_by_id,
                event_history: &status.event_history,
                event_counter: &status.event_counter,
                swarm_event_tx: &status.event_tx,
                client_event_tx: &tx,
            },
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerEvent::Error { id: 82, .. })
        ));
        host.shutdown().await
    })
}

#[test]
fn primary_stop_waits_for_owned_foreground_terminal_publication() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_owned();
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let admission = host.admit(&session, 71, agent.clone())?;
        let mut ctx = crate::tool::ToolContext {
            session_id: session.clone(),
            message_id: "seal-fixture".into(),
            tool_call_id: "slow-seal".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: Some(admission.agent.graceful_shutdown_signal()),
            execution_mode: jcode_tool_core::ToolExecutionMode::AgentTurn,
            invocation: Default::default(),
        };
        ctx.invocation.policy.cooperative_stop = true;
        let invocation = crate::execution::invocation(&ctx, "seal-fixture", serde_json::json!({}));
        let run = invocation.id();
        let (ready, started) = tokio::sync::oneshot::channel();
        let (cancelled, cancel) = tokio::sync::oneshot::channel();
        let (release, finish) = tokio::sync::oneshot::channel();
        let (done, mut completed) = tokio::sync::oneshot::channel();
        host.start(
            admission,
            move |agent| async move {
                let _guard = agent;
                crate::execution::execute(
                    invocation,
                    ctx,
                    std::num::NonZeroUsize::new(4096).unwrap(),
                    Box::new(move |ctx| {
                        Box::pin(async move {
                            let _ = ready.send(());
                            ctx.graceful_shutdown_signal
                                .as_ref()
                                .unwrap()
                                .notified()
                                .await;
                            let _ = cancelled.send(());
                            let _ = finish.await;
                            Ok(crate::tool::ToolOutput::new("retained partial fixture"))
                        })
                    }),
                )
                .await?;
                Ok(None)
            },
            move |outcome| async move {
                let _ = done.send(outcome.interrupted);
            },
        );
        started.await?;
        let owner = host.clone();
        let target = session.clone();
        let stop = tokio::spawn(async move { owner.stop(&target).await });
        cancel.await?;
        let premature = tokio::time::timeout(Duration::from_secs(1), &mut completed).await;
        release.send(()).unwrap();
        stop.await??;
        let settled_store = crate::execution::ExecutionStore::open(&crate::storage::jcode_dir()?)?;
        crate::execution::await_terminal(
            &settled_store,
            &run,
            &jcode_agent_runtime::InterruptSignal::new(),
        )
        .await?;
        assert!(
            premature.is_err(),
            "primary completion was emitted before its foreground result was sealed"
        );
        assert!(completed.await?);
        let store = crate::execution::ExecutionStore::open(&crate::storage::jcode_dir()?)?;
        let record = store.inspect(&run)?.unwrap();
        assert_eq!(record.state, crate::execution::RunState::Cancelled);
        assert!(record.result_path.is_some());
        host.shutdown().await
    })
}
