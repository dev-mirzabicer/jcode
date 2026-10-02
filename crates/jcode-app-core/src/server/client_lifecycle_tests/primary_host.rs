use super::*;

#[test]
#[cfg(unix)]
fn cold_primary_input_is_durable_while_fenced_and_restored_once_after_cancel() -> Result<()> {
    use crate::runtime_lifecycle::admission::RuntimeAdmission;
    use crate::workspace::runtime::*;
    use jcode_session_types::{PrimaryInputDelivery, PrimaryInputEnvelope, PrimaryInputState};
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let root = crate::storage::jcode_dir()?;
        let recorder = Arc::new(DurableInputProvider::default());
        let provider: Arc<dyn Provider> = recorder.clone();
        let registry = Registry::new(provider.clone()).await;
        let (mut agent, _) = Agent::new_with_startup_context(
            provider.clone(),
            registry,
            None,
            crate::agent::StartupContextActivation::primary(
                crate::agent::StartupContextCaller::HarnessApi,
            ),
        )?;
        agent.startup_context_session_mut().save()?;
        let session = agent.session_id().to_owned();
        let source = serde_json::to_vec(agent.messages())?;
        let system = agent.startup_context_session().system_prompt.clone();
        drop(agent);
        let host = Arc::new(crate::primary::PrimaryHost::default());
        let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
        let repositories = Arc::new(crate::instruction::InstructionRepositoryService::new());
        host.configure_input_restore(
            provider.clone(),
            Arc::new(tokio::sync::OnceCell::new_with(Some(pool.clone()))),
            repositories.clone(),
        );
        let lifecycle = crate::server::shutdown::RuntimeLifecycle::new(
            &root,
            &root.join("cold-input.sock"),
            host.clone(),
            crate::background::global().clone(),
        )
        .await?;
        let gate = RuntimeAdmission::for_root(&root)?.context("runtime gate")?;
        let held = gate.independent(RuntimeWorkKind::Preparation, "held-fixture".into(), None)?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::FinishCurrent,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 5,
                    destination: Default::default(),
                },
            })
            .await?
        else {
            panic!()
        };
        let RuntimeResponse::Operation(operation) = lifecycle
            .request(RuntimeRequest::Begin {
                request: crate::workspace::RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            panic!()
        };
        let plain = PrimaryInputEnvelope {
            id: crate::workspace::RequestId::new(),
            session: session.clone(),
            delivery: PrimaryInputDelivery::NextTurn,
            content: "cold plain input".into(),
            images: vec![],
            display_role: None,
            origin: Some(jcode_session_types::StoredMessageOrigin::Human),
            system_reminder: None,
            unattended_context: None,
            urgent: false,
            activate_skill: None,
            observe_startup_context: None,
            client_request_digest: None,
        };
        let status = status_fixture(&session);
        let (server_stream, client_stream) = crate::transport::stream_pair()?;
        let (global_tx, _) = broadcast::channel(8);
        let (debug_tx, _) = broadcast::channel(8);
        let (swarm_tx, _) = broadcast::channel(8);
        let server = tokio::spawn(handle_client(
            server_stream,
            host.clone(),
            global_tx,
            provider.clone(),
            Arc::new(crate::context::ContextTransactionService::new()),
            crate::server::startup_context::test_coordinator(),
            Arc::new(RwLock::new(false)),
            Arc::new(RwLock::new(String::new())),
            Arc::new(RwLock::new(1)),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            FileTouchService::new(),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(ClientDebugState::default())),
            debug_tx,
            Arc::new(RwLock::new(std::collections::VecDeque::new())),
            Arc::new(AtomicU64::new(0)),
            swarm_tx,
            "cold-input".into(),
            String::new(),
            pool.clone(),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(HashMap::new())),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
        ));
        let (read, mut write) = client_stream.into_split();
        let mut read = BufReader::new(read);
        async fn accept_socket(
            read: &mut BufReader<crate::transport::ReadHalf>,
            write: &mut crate::transport::WriteHalf,
            request: Request,
        ) -> Result<jcode_session_types::PrimaryInputReceipt> {
            let id = request.id();
            write
                .write_all((serde_json::to_string(&request)? + "\n").as_bytes())
                .await?;
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), read.read_line(&mut line)).await??;
            match serde_json::from_str::<ServerEvent>(&line)? {
                ServerEvent::PrimaryInputReceipt { id: reply, receipt } if reply == id => {
                    Ok(receipt)
                }
                other => anyhow::bail!("Unexpected pre-Subscribe reply: {other:?}"),
            }
        }
        let receipt = accept_socket(
            &mut read,
            &mut write,
            Request::PrimaryInput {
                id: 1,
                input: Box::new(plain.clone()),
            },
        )
        .await?;
        assert_eq!(receipt.state, PrimaryInputState::Accepted);
        let mut client_input = plain.clone();
        client_input.id = crate::workspace::RequestId::new();
        client_input.content = "cold client input".into();
        let client = crate::protocol::PrimaryClientInput {
            input: client_input.clone(),
            queued_messages: None,
            retry_of: None,
            is_system: false,
            retry_attempts: 0,
            auto_retry: false,
        };
        let receipt = accept_socket(
            &mut read,
            &mut write,
            Request::PrimaryClientInput {
                id: 2,
                request: Box::new(client.clone()),
            },
        )
        .await?;
        assert_eq!(receipt.state, PrimaryInputState::Accepted);
        drop(write);
        server.await??;
        assert!(host.read().await.is_empty());
        assert!(
            host.restore(&session, &provider, &pool, &repositories)
                .await
                .is_err()
        );
        assert!(recorder.snapshots.lock().unwrap().is_empty());
        assert_eq!(
            serde_json::to_vec(&Session::load(&session)?.messages)?,
            source
        );
        assert_eq!(
            crate::primary_input::PrimaryInputStore::current().original(&session, plain.id)?,
            Some(plain.clone())
        );
        let RuntimeResponse::Operation(current) = lifecycle
            .request(RuntimeRequest::Inspect {
                operation: operation.id,
            })
            .await?
        else {
            panic!()
        };
        lifecycle
            .request(RuntimeRequest::CancelWait {
                operation: current.id,
                expected_revision: current.revision,
            })
            .await?;
        drop(held);
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let store = crate::primary_input::PrimaryInputStore::current();
                if [plain.id, client_input.id].iter().all(|id| {
                    store
                        .inspect(&session, *id)
                        .is_ok_and(|r| r.state == PrimaryInputState::Committed)
                }) && host.processing(&session).is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 2);
        let mut after_system = Session::load(&session)?.system_prompt;
        assert!(
            after_system
                .as_ref()
                .and_then(|value| value.first_provider_dispatch_at)
                .is_some()
        );
        assert!(
            system
                .as_ref()
                .and_then(|value| value.first_provider_dispatch_at)
                .is_none()
        );
        after_system.as_mut().unwrap().first_provider_dispatch_at = None;
        assert_eq!(after_system, system);
        assert_eq!(
            crate::server::live_turn::submit_primary_input(&host, plain, status.clone())
                .await?
                .state,
            PrimaryInputState::Committed
        );
        assert_eq!(
            crate::server::live_turn::submit_client_input(&host, client, status)
                .await?
                .state,
            PrimaryInputState::Committed
        );
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 2);
        host.shutdown().await?;
        Ok(())
    })
}

#[test]
#[cfg(unix)]
fn runtime_shutdown_fence_retains_input_and_allows_only_admitted_work() -> Result<()> {
    use crate::runtime_lifecycle::admission::RuntimeAdmission;
    use crate::workspace::runtime::*;
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let root = crate::storage::jcode_dir()?;
        let recorder = Arc::new(DurableInputProvider::default());
        let provider: Arc<dyn Provider> = recorder.clone();
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry.clone())));
        let session = agent.lock().await.session_id().to_owned();
        agent.lock().await.startup_context_session_mut().save()?;
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let status = status_fixture(&session);
        let lifecycle = crate::server::shutdown::RuntimeLifecycle::new(
            &root,
            &root.join("shutdown-fixture.sock"),
            host.clone(),
            crate::background::global().clone(),
        )
        .await?;
        let gate = RuntimeAdmission::for_root(&root)?.context("Runtime gate missing")?;
        // Keep one genuinely admitted preparation outstanding while the first
        // primary finishes, so Cancel is still the valid human action.
        let held = gate.independent(
            RuntimeWorkKind::Preparation,
            "fixture-preparation".into(),
            None,
        )?;
        let initial = jcode_session_types::PrimaryInputEnvelope {
            id: crate::workspace::RequestId::new(),
            session: session.clone(),
            delivery: jcode_session_types::PrimaryInputDelivery::NextTurn,
            content: "already admitted input".into(),
            images: Vec::new(),
            display_role: None,
            origin: Some(jcode_session_types::StoredMessageOrigin::Human),
            system_reminder: None,
            unattended_context: None,
            urgent: false,
            activate_skill: None,
            observe_startup_context: None,
            client_request_digest: None,
        };
        let admission = host.admit(&session, 99, agent.clone())?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::FinishCurrent,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 10,
                    destination: Default::default(),
                },
            })
            .await?
        else {
            panic!("review");
        };
        let RuntimeResponse::Operation(op) = lifecycle
            .request(RuntimeRequest::Begin {
                request: crate::workspace::RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            panic!("operation");
        };
        let mut later = initial.clone();
        later.id = crate::workspace::RequestId::new();
        later.content = "deferred input".into();
        later.delivery = jcode_session_types::PrimaryInputDelivery::SafeBoundary;
        later.urgent = true;
        crate::server::live_turn::submit_primary_input(&host, later.clone(), status.clone())
            .await?;
        assert!(!host.accepts_input());
        let workdir = tempfile::tempdir()?;
        let file = workdir.path().join("admitted-effect.txt");
        let context = crate::tool::ToolContext {
            session_id: session.clone(),
            message_id: "admitted-effect".into(),
            tool_call_id: "write".into(),
            working_dir: Some(workdir.path().to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
            invocation: Default::default(),
        };
        // Same Session text does not grant causal authority to another caller.
        let mut denied = context.clone();
        denied.tool_call_id = "denied".into();
        let denied_file = workdir.path().join("not-admitted.txt");
        assert!(
            registry
                .execute(
                    "write",
                    serde_json::json!({"file_path":denied_file,"content":"must not exist"}),
                    denied
                )
                .await
                .is_err()
        );
        assert!(!denied_file.exists());
        let producer_registry = registry.clone();
        let producer_file = file.clone();
        let (completed, result) = tokio::sync::oneshot::channel();
        host.start(
            admission,
            move |mut agent| async move {
                assert!(!agent.has_urgent_interrupt());
                producer_registry
                    .execute(
                        "write",
                        serde_json::json!({"file_path":producer_file,"content":"one effect"}),
                        context,
                    )
                    .await?;
                agent.run_primary_input_capture(initial).await.map(Some)
            },
            move |outcome| async move {
                let _ = completed.send(outcome.result);
            },
        );
        tokio::time::timeout(Duration::from_secs(20), result).await???;
        host.wait_idle(&session).await?;
        assert_eq!(std::fs::read_to_string(&file)?, "one effect");
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 1);
        assert_eq!(
            crate::primary_input::PrimaryInputStore::current()
                .inspect(&session, later.id)?
                .state,
            jcode_session_types::PrimaryInputState::Accepted
        );
        let current = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let RuntimeResponse::Operation(current) = lifecycle
                    .request(RuntimeRequest::Inspect { operation: op.id })
                    .await?
                    && current
                        .remaining
                        .iter()
                        .all(|work| work.kind == RuntimeWorkKind::Preparation)
                {
                    return Ok::<_, anyhow::Error>(current);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let mut retiring = host
            .input_drain(&session)
            .context("Fixture drain already occupied")?;
        retiring.defer_for_runtime();
        lifecycle
            .request(RuntimeRequest::CancelWait {
                operation: current.id,
                expected_revision: current.revision,
            })
            .await?;
        drop(held);
        // Cancel saw the old registration; only its proper retirement can kick
        // this pending input. No direct delivery call repairs the test.
        drop(retiring);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if crate::primary_input::PrimaryInputStore::current()
                    .inspect(&session, later.id)?
                    .state
                    == jcode_session_types::PrimaryInputState::Committed
                    && host.processing(&session).is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 2);
        host.shutdown().await?;
        assert!(gate.work()?.is_empty());
        Ok(())
    })
}

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
            origin: Some(jcode_session_types::StoredMessageOrigin::Human), system_reminder: None, unattended_context: None, urgent: false, activate_skill: None, observe_startup_context: None, client_request_digest: None,
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
        crate::server::client_state::handle_get_history(41,&session,true,&agent,&crate::server::startup_context::test_coordinator(),&provider,&host,&Arc::new(RwLock::new(HashMap::new())),&Arc::new(RwLock::new(1)),&writer,"fixture","",Some(&observer)).await?;
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
        let snapshot=crate::server::client_state::handle_get_history(77,&session,true,&agent,&startup,&provider,&host,&connections,&count,&writer,"fixture","",Some(&observer));
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

/// Journals of a runtime incarnation bound to `host`, plus one unresolved
/// unexpected-exit recovery item for `session` from a previous incarnation.
fn bind_recovery_fixture(
    host: &crate::primary::PrimaryHost,
    session: &str,
) -> Result<(
    crate::runtime_lifecycle::RuntimeStopOwner,
    crate::workspace::runtime::RecoveryItem,
)> {
    let store = crate::runtime_lifecycle::RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &crate::server::socket_path(),
    )?;
    let owner = store.claim()?;
    host.bind_runtime_journals(crate::primary::RuntimeJournals {
        turns: owner.turns(),
        recovery: owner.recovery(),
    })?;
    let record = crate::runtime_lifecycle::turns::TurnRecord {
        schema: 1,
        session: session.into(),
        turn: "previous-turn".into(),
        runtime: "previous-incarnation".into(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let item = owner
        .recovery()
        .adopt(&[record], |_| {
            crate::workspace::runtime::RecoveryCause::UnexpectedExit
        })?
        .remove(0);
    Ok((owner, item))
}

async fn wait_committed(
    session: &str,
    input: crate::workspace::RequestId,
    host: &crate::primary::PrimaryHost,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let receipt =
                crate::primary_input::PrimaryInputStore::current().inspect(session, input)?;
            anyhow::ensure!(
                receipt.state != jcode_session_types::PrimaryInputState::Failed,
                "delivery failed: {receipt:?}"
            );
            if receipt.state == jcode_session_types::PrimaryInputState::Committed
                && host.processing(session).is_none()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[test]
fn unresolved_crash_recovery_defers_automatic_wakes_until_human_input_supersedes() -> Result<()> {
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
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let (owner, item) = bind_recovery_fixture(&host, &session)?;
        let status = status_fixture(&session);
        let mut wake = jcode_session_types::PrimaryInputEnvelope::new(
            session.clone(),
            "background task finished".into(),
            jcode_session_types::PrimaryInputDelivery::SafeBoundary,
        );
        wake.display_role = Some(jcode_session_types::StoredDisplayRole::BackgroundTask);
        crate::server::live_turn::submit_primary_input(&host, wake.clone(), status.clone()).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            crate::primary_input::PrimaryInputStore::current()
                .inspect(&session, wake.id)?
                .state,
            jcode_session_types::PrimaryInputState::Accepted,
            "an automatic wake must not bypass the pending recovery decision"
        );
        assert!(
            recorder.snapshots.lock().unwrap().is_empty(),
            "no inference before a decision"
        );

        let mut human = jcode_session_types::PrimaryInputEnvelope::new(
            session.clone(),
            "human follow-up".into(),
            jcode_session_types::PrimaryInputDelivery::NextTurn,
        );
        human.origin = Some(jcode_session_types::StoredMessageOrigin::Human);
        crate::server::live_turn::submit_primary_input(&host, human.clone(), status.clone())
            .await?;
        wait_committed(&session, human.id, &host).await?;
        wait_committed(&session, wake.id, &host).await?;
        let resolved = owner.recovery().inspect(item.id)?;
        assert_eq!(
            resolved.resolved.map(|resolved| resolved.resolution),
            Some(
                crate::workspace::runtime::RecoveryResolution::SupersededByInput {
                    input: human.id
                }
            )
        );
        // The human message went first; the deferred wake followed it, either
        // at that turn's safe boundary or as its own next turn.
        let calls = recorder.snapshots.lock().unwrap().clone();
        assert!((1..=2).contains(&calls.len()));
        let text = |messages: &Vec<Message>| {
            messages
                .iter()
                .flat_map(|m| &m.content)
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            text(&calls[0]).contains("human follow-up")
                && !text(&calls[0]).contains("background task finished")
        );
        // A trusted decision after supersession cannot inject a stale continuation.
        assert!(
            crate::server::supervision::decide(
                &host,
                item.id,
                item.revision,
                crate::workspace::RequestId::new(),
                crate::workspace::runtime::RecoveryDecision::Continue,
            )
            .await
            .is_err()
        );
        assert_eq!(recorder.snapshots.lock().unwrap().len(), calls.len());
        host.shutdown().await
    })
}

#[test]
fn selected_continue_delivers_one_turn_and_replays_only_by_request() -> Result<()> {
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
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let (owner, item) = bind_recovery_fixture(&host, &session)?;
        host.configure_input_delivery(status_fixture(&session));
        let request = crate::workspace::RequestId::new();
        let decide = |request| {
            crate::server::supervision::decide(
                &host,
                item.id,
                item.revision,
                request,
                crate::workspace::runtime::RecoveryDecision::Continue,
            )
        };
        let first = decide(request).await?;
        let Some(crate::workspace::runtime::RecoveryResolution::Continued { input }) = first
            .resolved
            .as_ref()
            .map(|resolved| resolved.resolution.clone())
        else {
            panic!("continue was not recorded: {first:?}");
        };
        wait_committed(&session, input, &host).await?;
        // Two clients, or one retrying an uncertain reply: same request replays
        // the decision; another request cannot decide again.
        assert_eq!(decide(request).await?, first);
        assert!(decide(crate::workspace::RequestId::new()).await.is_err());
        let calls = recorder.snapshots.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "exactly one continuation turn");
        let saved = Session::load(&session)?;
        assert_eq!(saved.primary_inputs.len(), 1);
        assert!(owner.recovery().unresolved(&session)?.is_empty());
        host.shutdown().await
    })
}

#[test]
fn leave_stopped_needs_no_inference_and_releases_deferred_wakes() -> Result<()> {
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
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let (_owner, item) = bind_recovery_fixture(&host, &session)?;
        let status = status_fixture(&session);
        let mut wake = jcode_session_types::PrimaryInputEnvelope::new(
            session.clone(),
            "scheduled wake".into(),
            jcode_session_types::PrimaryInputDelivery::SafeBoundary,
        );
        wake.display_role = Some(jcode_session_types::StoredDisplayRole::BackgroundTask);
        crate::server::live_turn::submit_primary_input(&host, wake.clone(), status.clone()).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(recorder.snapshots.lock().unwrap().is_empty());
        let left = crate::server::supervision::decide(
            &host,
            item.id,
            item.revision,
            crate::workspace::RequestId::new(),
            crate::workspace::runtime::RecoveryDecision::LeaveStopped,
        )
        .await?;
        assert_eq!(
            left.resolved.map(|resolved| resolved.resolution),
            Some(crate::workspace::runtime::RecoveryResolution::LeftStopped {})
        );
        // The interrupted turn is not continued; ordinary deferred input resumes.
        wait_committed(&session, wake.id, &host).await?;
        assert_eq!(recorder.snapshots.lock().unwrap().len(), 1);
        host.shutdown().await
    })
}

#[test]
fn only_planned_transitions_retain_interrupted_turn_records() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(DurableInputProvider::default());
        let registry = Registry::new(provider.clone()).await;
        let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
        let session = agent.lock().await.session_id().to_owned();
        agent.lock().await.startup_context_session_mut().save()?;
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let (owner, _item) = bind_recovery_fixture(&host, &session)?;
        let pending = || async {
            let admission = host.admit(&session, 7, agent.clone())?;
            host.start(
                admission,
                |_| std::future::pending::<Result<Option<String>>>(),
                |_| async {},
            );
            anyhow::ensure!(
                owner.turns().own_records()?.len() == 1,
                "admission is durable before work"
            );
            Ok::<_, anyhow::Error>(())
        };
        // A natural or human-stopped turn settles its record.
        pending().await?;
        host.stop(&session).await?;
        assert!(
            owner.turns().own_records()?.is_empty(),
            "human Stop is not crash evidence"
        );
        // A planned transition leaves the exact record for the next incarnation.
        host.retain_interrupted_turns(true);
        pending().await?;
        host.interrupt_runtime_with_cause(jcode_tool_types::StopCause::ReloadQuiescence)
            .await?;
        let retained = host.retained_turn_records()?;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].session, session);
        // If the transition fails, this incarnation takes the records back.
        host.retain_interrupted_turns(false);
        assert_eq!(host.settle_retained_turn_records()?, vec![session.clone()]);
        assert!(owner.turns().own_records()?.is_empty());
        host.shutdown().await
    })
}

/// A reload that interrupted work and then could not replace the runtime keeps
/// serving: it continues exactly the interrupted turn locally, once, through
/// durable input, settles its record and creates no crash recovery.
#[test]
fn failed_reload_after_interruption_continues_the_turn_locally_once() -> Result<()> {
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
        let host = Arc::new(crate::primary::PrimaryHost::new(HashMap::from([(
            session.clone(),
            agent.clone(),
        )])));
        let (owner, _) = bind_recovery_fixture(&host, &session)?;
        // Only the fixture's prior-incarnation item exists; resolve it so the
        // session is eligible for planned continuation.
        for item in owner.recovery().unresolved(&session)? {
            owner.recovery().resolve(
                item.id,
                item.revision,
                crate::workspace::RequestId::new(),
                crate::workspace::runtime::RecoveryResolution::LeftStopped {},
            )?;
        }
        host.configure_input_delivery(status_fixture(&session));
        host.retain_interrupted_turns(true);
        let admission = host.admit(&session, 9, agent.clone())?;
        host.start(
            admission,
            |_| std::future::pending::<Result<Option<String>>>(),
            |_| async {},
        );
        host.interrupt_runtime_with_cause(jcode_tool_types::StopCause::ReloadQuiescence)
            .await?;
        assert_eq!(host.retained_turn_records()?.len(), 1);
        let signal = crate::server::ReloadSignal {
            hash: "fixture".into(),
            triggering_session: None,
            prefer_selfdev_binary: false,
            request_id: "reload-failed-fixture".into(),
        };
        // An intent written before runtime-owned continuation has no
        // verified evidence; the runtime never wakes its session itself.
        crate::server::reload_recovery::persist_intent(
            "legacy-reload",
            "session_legacy_fixture",
            crate::server::reload_recovery::ReloadRecoveryRole::Initiator,
            crate::protocol::ReloadRecoverySnapshot {
                reconnect_notice: None,
                continuation_message: "legacy".into(),
            },
            "legacy",
        )?;
        crate::server::reload::fail_reload_for_test(
            &signal,
            &host,
            &anyhow::anyhow!("checkpoint failed"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if !recorder.snapshots.lock().unwrap().is_empty()
                    && host.processing(&session).is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let calls = recorder.snapshots.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "exactly one local continuation");
        let text = calls[0]
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("interrupted by a server reload"), "{text}");
        assert!(
            owner.turns().own_records()?.is_empty(),
            "settled after the failed reload"
        );
        assert!(
            owner.recovery().unresolved(&session)?.is_empty(),
            "a failed reload is not a crash"
        );
        let pending = crate::server::reload_recovery::pending_records()?;
        assert_eq!(
            pending.len(),
            1,
            "runtime intent retired, legacy intent untouched"
        );
        assert_eq!(pending[0].session_id, "session_legacy_fixture");
        host.shutdown().await
    })
}
