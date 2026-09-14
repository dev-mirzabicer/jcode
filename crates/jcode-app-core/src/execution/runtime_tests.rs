use super::*;
use jcode_tool_core::ToolExecutionMode;

#[tokio::test]
async fn closed_delivery_channel_still_reads_the_actual_owner_outcome() -> Result<()> {
    use jcode_tool_core::OwnedExecutionControl;
    let mut fixture = start().await?;
    let original = LIVE
        .lock()
        .unwrap()
        .get(&(fixture.store.root().to_path_buf(), fixture.id.clone()))
        .unwrap()
        .clone();
    let (sender, result) = tokio::sync::watch::channel(None);
    drop(sender);
    let control = super::super::BackgroundControl {
        run: Arc::new(LiveRun {
            owns_execution: AtomicBool::new(false),
            store: original.store.clone(),
            runtime: original.runtime.clone(),
            invocation: original.invocation.clone(),
            stop: original.stop.clone(),
            background: AtomicBool::new(true),
            promoted: Default::default(),
            ready: original.ready.clone(),
            delivery: tokio::sync::Mutex::new(()),
            commands: original.commands.clone(),
            result,
        }),
    };
    let waiting = tokio::time::timeout(std::time::Duration::from_millis(50), control.wait()).await;
    assert!(
        waiting.is_err(),
        "A closed delivery channel cannot make live work terminal"
    );
    assert_eq!(
        fixture.store.inspect(&fixture.id)?.unwrap().state,
        RunState::Running
    );
    fixture.release.notify_one();
    (&mut fixture.task).await??;
    assert_eq!(
        fixture.store.inspect(&fixture.id)?.unwrap().state,
        RunState::Completed
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), control.wait()).await??;
    assert_eq!(outcome, RunState::Completed);
    Ok(())
}

struct Fixture {
    store: ExecutionStore,
    id: String,
    task: tokio::task::JoinHandle<Result<ToolOutput>>,
    release: Arc<tokio::sync::Notify>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.release.notify_one();
        self.task.abort();
    }
}
async fn start() -> Result<Fixture> {
    let ctx = ToolContext {
        session_id: uuid::Uuid::new_v4().to_string(),
        message_id: "message".into(),
        tool_call_id: "call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: Default::default(),
    };
    let invocation = super::super::invocation(&ctx, "fixture", serde_json::json!({}));
    let id = invocation.id();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(async move {
        super::super::execute(
            invocation,
            ctx,
            NonZeroUsize::new(100).unwrap(),
            Box::new(move |_| {
                Box::pin(async move {
                    let _ = started.send(());
                    gate.notified().await;
                    Ok(ToolOutput::new("retained result"))
                })
            }),
        )
        .await
    });
    ready.await?;
    let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    Ok(Fixture {
        store,
        id,
        task,
        release,
    })
}

#[tokio::test]
async fn authenticated_socket_stops_owned_work_without_loading_output() -> Result<()> {
    let fixture = start().await?;
    let inspected =
        control_in_store(&fixture.store, &fixture.id, ControlOperation::Inspect).await?;
    let ControlReply::Snapshot { record } = inspected else {
        panic!()
    };
    assert_eq!(record.state, RunState::Running);
    let endpoint = fixture.store.runtime_endpoint(&record.owner)?.unwrap();
    assert!(!format!("{endpoint:?}").contains(endpoint.transport_key()));
    assert!(matches!(
        control_in_store(
            &fixture.store,
            &fixture.id,
            ControlOperation::Stop {
                cause: StopCause::HumanCancellation
            }
        )
        .await?,
        ControlReply::Accepted { changed: true }
    ));
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        control_in_store(&fixture.store, &fixture.id, ControlOperation::Wait),
    )
    .await??;
    let ControlReply::Snapshot { record } = reply else {
        panic!()
    };
    assert_eq!(record.state, RunState::Cancelled);
    assert_eq!(record.stop_cause, Some(StopCause::HumanCancellation));
    Ok(())
}

#[tokio::test]
async fn invalid_credentials_and_protocol_cannot_mutate_a_run() -> Result<()> {
    let fixture = start().await?;
    let record = fixture.store.inspect(&fixture.id)?.unwrap();
    let endpoint = fixture.store.runtime_endpoint(&record.owner)?.unwrap();
    let mut bad = RuntimeEndpoint::new(
        endpoint.id.clone(),
        endpoint.endpoint.clone(),
        endpoint.lease_path.clone(),
        "0".repeat(64),
    );
    assert!(
        exchange(
            &bad,
            &fixture.id,
            ControlOperation::Stop {
                cause: StopCause::HumanCancellation
            }
        )
        .await
        .is_err()
    );
    bad.protocol_version = 0;
    assert!(
        exchange(&bad, &fixture.id, ControlOperation::Background)
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .inspect(&fixture.id)?
            .unwrap()
            .stop_cause
            .is_none()
    );
    fixture.release.notify_one();
    let reply = control_in_store(&fixture.store, &fixture.id, ControlOperation::Wait).await?;
    assert!(matches!(reply,ControlReply::Snapshot{record} if record.state==RunState::Completed));
    Ok(())
}

#[tokio::test]
async fn dropping_a_remote_wait_does_not_stop_the_run() -> Result<()> {
    let fixture = start().await?;
    assert!(matches!(
        control_in_store(&fixture.store, &fixture.id, ControlOperation::Background).await?,
        ControlReply::Accepted { changed: true }
    ));
    let store = fixture.store.clone();
    let id = fixture.id.clone();
    let wait =
        tokio::spawn(async move { control_in_store(&store, &id, ControlOperation::Wait).await });
    tokio::task::yield_now().await;
    wait.abort();
    let _ = wait.await;
    assert!(
        fixture
            .store
            .inspect(&fixture.id)?
            .unwrap()
            .stop_cause
            .is_none()
    );
    fixture.release.notify_one();
    let ControlReply::Snapshot { record } =
        control_in_store(&fixture.store, &fixture.id, ControlOperation::Wait).await?
    else {
        panic!()
    };
    assert_eq!(record.state, RunState::Completed);
    Ok(())
}

#[tokio::test]
async fn same_runtime_replay_without_a_live_producer_is_unavailable_not_self_waiting() -> Result<()>
{
    let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    let runtime = ensure_running(&store).await?;
    let ctx = ToolContext {
        session_id: uuid::Uuid::new_v4().to_string(),
        message_id: "message".into(),
        tool_call_id: "call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: Default::default(),
    };
    let invocation = super::super::invocation(&ctx, "fixture", serde_json::json!({}));
    let PreparedInvocation::New(record) = store.prepare(&invocation, &runtime.endpoint.id)? else {
        panic!()
    };
    store.start(&record.id, &runtime.endpoint.id)?;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::execute(
            invocation,
            ctx,
            NonZeroUsize::new(100).unwrap(),
            Box::new(move |_| {
                Box::pin(async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::new("must not execute"))
                })
            }),
        ),
    )
    .await?;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Running);
    Ok(())
}

#[tokio::test]
async fn owner_process_fixture() -> Result<()> {
    let Some(path) = std::env::var_os("JCODE_EXECUTION_OWNER_FIXTURE") else {
        return Ok(());
    };
    let mut fixture = start().await?;
    crate::storage::write_json_secret(
        std::path::Path::new(&path),
        &serde_json::json!({"run_id":fixture.id}),
    )?;
    let result = (&mut fixture.task).await?;
    ensure!(
        result.is_err(),
        "Cross-process fixture should end through explicit Stop"
    );
    Ok(())
}

#[tokio::test]
async fn another_process_controls_only_the_registered_execution() -> Result<()> {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = tempfile::tempdir()?;
    let marker = directory.path().join("ready.json");
    let mut child = Child(
        std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "execution::runtime::tests::owner_process_fixture",
                "--nocapture",
            ])
            .env("JCODE_HOME", directory.path())
            .env("JCODE_RUNTIME_DIR", directory.path().join("runtime"))
            .env("JCODE_EXECUTION_OWNER_FIXTURE", &marker)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()?,
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !marker.exists() {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Owner process did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let marker: serde_json::Value = crate::storage::read_json(&marker)?;
    let id = marker["run_id"]
        .as_str()
        .context("Missing fixture identity")?;
    let store = ExecutionStore::open(directory.path())?;
    let record = store.inspect(id)?.unwrap();
    let endpoint = store.runtime_endpoint(&record.owner)?.unwrap();
    assert_eq!(endpoint.process_id, child.0.id());
    assert_ne!(endpoint.process_id, std::process::id());
    assert!(matches!(
        control_in_store(
            &store,
            id,
            ControlOperation::Stop {
                cause: StopCause::HumanCancellation
            }
        )
        .await?,
        ControlReply::Accepted { changed: true }
    ));
    let ControlReply::Snapshot { record } = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        control_in_store(&store, id, ControlOperation::Wait),
    )
    .await??
    else {
        panic!()
    };
    assert_eq!(record.state, RunState::Cancelled);
    loop {
        if let Some(status) = child.0.try_wait()? {
            assert!(status.success());
            break;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Owner process did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!endpoint.has_live_lease()?);
    assert!(matches!(
        control_in_store(
            &store,
            id,
            ControlOperation::Stop {
                cause: StopCause::HumanCancellation
            }
        )
        .await?,
        ControlReply::Accepted { changed: false }
    ));
    Ok(())
}

#[tokio::test]
async fn ending_a_listener_releases_its_unreferenced_runtime_lease() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = ExecutionStore::open(directory.path())?;
    let runtime = ensure_running(&store).await?;
    let endpoint = runtime.endpoint.clone();
    assert!(endpoint.has_live_lease()?);
    runtime.listener.abort();
    while !runtime.listener.is_finished() {
        tokio::task::yield_now().await;
    }
    drop(runtime);
    assert!(!endpoint.has_live_lease()?);
    let replacement = ensure_running(&store).await?;
    assert_ne!(replacement.endpoint.id, endpoint.id);
    Ok(())
}

#[tokio::test]
async fn unsupported_force_does_not_cancel_otherwise_running_work() -> Result<()> {
    let mut fixture = start().await?;
    let session = fixture.store.inspect(&fixture.id)?.unwrap().session_id;
    assert!(
        crate::execution::inspection::inspect(
            fixture.store.root().parent().unwrap(),
            &session,
            jcode_tool_types::execution::ExecutionRequest::ForceStop {
                run_id: fixture.id.clone()
            }
        )
        .await
        .is_err()
    );
    let current = fixture.store.inspect(&fixture.id)?.unwrap();
    assert_eq!(current.state, RunState::Running);
    assert!(current.stop_cause.is_none());
    fixture.release.notify_one();
    (&mut fixture.task).await??;
    assert_eq!(
        fixture.store.inspect(&fixture.id)?.unwrap().state,
        RunState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn human_background_promotion_releases_foreground_caller_without_restarting_work()
-> Result<()> {
    let mut fixture = start().await?;
    let live = LIVE
        .lock()
        .unwrap()
        .get(&(fixture.store.root().to_path_buf(), fixture.id.clone()))
        .unwrap()
        .clone();
    // The Registry publishes readiness after real preflight. This synthetic
    // producer uses the same execution owner without a provider/tool dependency.
    live.ready.mark();
    let reply = control_in_store(&fixture.store, &fixture.id, ControlOperation::Background).await?;
    assert!(matches!(reply, ControlReply::Accepted { changed: true }));
    let receipt =
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut fixture.task).await???;
    assert!(matches!(receipt.source, OutputSource::Acceptance(_)));
    assert_eq!(
        fixture.store.inspect(&fixture.id)?.unwrap().state,
        RunState::Running
    );
    fixture.release.notify_one();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        control_in_store(&fixture.store, &fixture.id, ControlOperation::Wait),
    )
    .await??;
    assert!(matches!(result,ControlReply::Snapshot{record} if record.state==RunState::Completed));
    assert!(
        fixture
            .store
            .result(
                &fixture.store.inspect(&fixture.id)?.unwrap(),
                NonZeroUsize::new(1000).unwrap()
            )?
            .output
            .contains("retained result")
    );
    Ok(())
}
