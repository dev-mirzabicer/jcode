use super::*;
use jcode_tool_core::OutputStream;
use jcode_tool_types::{OutputSource, StopCause};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct FixtureTool {
    count: Arc<AtomicUsize>,
    body: String,
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Option<Arc<tokio::sync::Notify>>,
    dropped: Arc<AtomicBool>,
    stream: bool,
}
struct DropMarker(Arc<AtomicBool>);
impl Drop for DropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl Tool for FixtureTool {
    fn execution_policy(
        &self,
        input: &Value,
        _: &ToolContext,
    ) -> Result<jcode_tool_core::ExecutionPolicy> {
        Ok(jcode_tool_core::ExecutionPolicy {
            background: input
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            foreground_timeout: input
                .get("foreground_timeout")
                .and_then(Value::as_u64)
                .map(std::time::Duration::from_millis),
            notify: false,
            wake: false,
            ..Default::default()
        })
    }
    fn name(&self) -> &str {
        "execution_fixture"
    }
    fn description(&self) -> &str {
        "Synthetic execution boundary fixture"
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    async fn execute(&self, input: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        self.count.fetch_add(1, Ordering::SeqCst);
        let _marker = DropMarker(self.dropped.clone());
        if self.stream {
            ctx.invocation
                .capture
                .as_ref()
                .unwrap()
                .write(OutputStream::Stdout, self.body.as_bytes())?;
        }
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        let mut output = ToolOutput::new(if self.stream {
            String::new()
        } else {
            self.body.clone()
        });
        if self.stream {
            output.source =
                OutputSource::Retained(ctx.invocation.capture.as_ref().unwrap().reference()?);
        }
        Ok(output.with_error(input.get("fail").and_then(Value::as_bool).unwrap_or(false)))
    }
}

#[tokio::test]
async fn explicit_background_and_foreground_deadline_return_one_retained_acceptance()
-> anyhow::Result<()> {
    for input in [
        serde_json::json!({"background":true}),
        serde_json::json!({"foreground_timeout":20}),
    ] {
        let (registry, tool, started) = fixture("full background result".into(), true, true).await;
        let ctx = context();
        let id = crate::execution::invocation_id(&ctx);
        let output = registry
            .execute("execution_fixture", input.clone(), ctx.clone())
            .await?;
        started.await?;
        let OutputSource::Acceptance(reference) = &output.source else {
            panic!("Expected acceptance receipt")
        };
        assert!(reference.path.exists());
        assert_eq!(reference.invocation_id, id);
        tool.release.as_ref().unwrap().notify_one();
        let record = crate::execution::wait_for(&id).await?;
        assert_eq!(record.state, crate::execution::RunState::Completed);
        let replay = registry.execute("execution_fixture", input, ctx).await?;
        assert_eq!(replay.output, output.output);
        assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[tokio::test]
async fn producer_declared_error_is_retained_and_reported_as_failure() -> anyhow::Result<()> {
    let (registry, tool, _) = fixture("declared failure body".into(), false, false).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let error = registry
        .execute("execution_fixture", serde_json::json!({"fail":true}), ctx)
        .await
        .unwrap_err();
    let captured = error
        .downcast_ref::<crate::execution::CapturedToolError>()
        .unwrap();
    assert!(captured.output.is_error);
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Failed);
    assert_eq!(
        std::fs::read_to_string(record.output_path.unwrap())?,
        "declared failure body"
    );
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    Ok(())
}
fn context() -> ToolContext {
    ToolContext {
        session_id: uuid::Uuid::new_v4().to_string(),
        message_id: "message".into(),
        tool_call_id: "call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: Some(jcode_agent_runtime::InterruptSignal::new()),
        execution_mode: ToolExecutionMode::AgentTurn,
        invocation: Default::default(),
    }
}

struct WrappedEcho;
#[async_trait]
impl Tool for WrappedEcho {
    fn name(&self) -> &str {
        "wrapped_echo"
    }
    fn description(&self) -> &str {
        "Synthetic producer argument and media fixture"
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{"output_size":{"type":"boolean"},"arguments":{"type":"string"}}})
    }
    async fn execute(&self, input: Value, _: ToolContext) -> anyhow::Result<ToolOutput> {
        anyhow::ensure!(
            input["output_size"] == true && input["arguments"] == "producer-owned",
            "producer input changed"
        );
        Ok(ToolOutput::new("rich member")
            .with_metadata(serde_json::json!({"received":input}))
            .with_image("application/octet-stream", "eA=="))
    }
}

#[tokio::test]
async fn repeated_batches_preserve_wrapped_arguments_media_and_distinct_member_ids()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    registry
        .register("wrapped_echo".into(), Arc::new(WrappedEcho))
        .await;
    let input = serde_json::json!({"tool_calls":[{"tool":"wrapped_echo","intent":"fixture","arguments":{"output_size":true,"arguments":"producer-owned"},"output_size":"small"}]});
    let mut ids = Vec::new();
    for _ in 0..2 {
        let output = registry.execute("batch", input.clone(), context()).await?;
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].data, "eA==");
        let metadata = output.metadata.unwrap();
        assert_eq!(
            metadata["members"][0]["metadata"]["received"]["output_size"],
            true
        );
        ids.push(
            metadata["members"][0]["run_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_ne!(ids[0], ids[1]);
    Ok(())
}

#[tokio::test]
async fn conflicting_batch_presentation_requests_fail_before_members_start() -> anyhow::Result<()> {
    let (registry, tool, _) = fixture("body".into(), false, false).await;
    let input = serde_json::json!({"tool_calls":[{"tool":"execution_fixture","output_size":"small","parameters":{"output_size":"large"}}]});
    assert!(registry.execute("batch", input, context()).await.is_err());
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn capture_allocation_failure_never_starts_a_producer_or_overwrites_existing_data()
-> anyhow::Result<()> {
    let (registry, tool, _) = fixture("must not execute".into(), false, false).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let store = crate::execution::ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    let existing = store.root().join("data").join(&id);
    std::fs::create_dir_all(&existing)?;
    let marker = existing.join("unowned.txt");
    std::fs::write(&marker, b"preserve")?;
    assert!(
        registry
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
            .is_err()
    );
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(marker)?, b"preserve");
    assert_eq!(
        store.inspect(&id)?.unwrap().state,
        crate::execution::RunState::Failed
    );
    Ok(())
}
async fn fixture(
    body: String,
    gate: bool,
    stream: bool,
) -> (
    Registry,
    Arc<FixtureTool>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let (started, receiver) = tokio::sync::oneshot::channel();
    let tool = Arc::new(FixtureTool {
        count: Arc::new(AtomicUsize::new(0)),
        body,
        started: std::sync::Mutex::new(Some(started)),
        release: gate.then(|| Arc::new(tokio::sync::Notify::new())),
        dropped: Arc::new(AtomicBool::new(false)),
        stream,
    });
    registry
        .register("execution_fixture".into(), tool.clone())
        .await;
    (registry, tool, receiver)
}

#[tokio::test]
async fn complete_output_survives_withholding_and_replay_without_new_effects() -> anyhow::Result<()>
{
    let body = format!("{}TAIL", "界".repeat(40_000));
    let (registry, tool, _) = fixture(body.clone(), false, false).await;
    registry.context_budget().write().await.set_budget(1000);
    let ctx = context();
    let input = serde_json::json!({"intent":"capture fixture"});
    let output = registry
        .execute("execution_fixture", input.clone(), ctx.clone())
        .await?;
    assert!(output.withheld.is_some());
    let OutputSource::Retained(reference) = output.source else {
        panic!()
    };
    assert_eq!(std::fs::read_to_string(&reference.path)?, body);
    // A repaired context can receive the saved result without invoking the tool.
    registry.context_budget().write().await.set_budget(0);
    let replay = registry
        .execute("execution_fixture", input.clone(), ctx.clone())
        .await?;
    assert!(replay.withheld.is_none());
    let mut changed_context = ctx;
    changed_context.working_dir = Some(std::env::temp_dir());
    assert!(
        registry
            .execute("execution_fixture", input, changed_context)
            .await
            .is_err()
    );
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    assert!(
        registry
            .execute(
                "execution_fixture",
                serde_json::json!({"accept_large_output":true}),
                context()
            )
            .await
            .is_err()
    );
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn published_definition_keeps_its_matching_producer_until_a_fresh_registry()
-> anyhow::Result<()> {
    let (registry, original, _) = fixture("original".into(), false, false).await;
    registry.definitions(None).await;
    let (_, replacement, _) = fixture("replacement".into(), false, false).await;
    registry
        .register("execution_fixture".into(), replacement.clone())
        .await;
    let output = registry
        .execute("execution_fixture", serde_json::json!({}), context())
        .await?;
    assert!(output.output.starts_with("original"));
    assert_eq!(original.count.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.count.load(Ordering::SeqCst), 0);
    let output = registry
        .clone()
        .execute("execution_fixture", serde_json::json!({}), context())
        .await?;
    assert!(output.output.starts_with("replacement"));
    assert_eq!(replacement.count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn dropping_foreground_wait_stops_actual_owned_task_and_retains_prefix() -> anyhow::Result<()>
{
    let (registry, tool, started) = fixture("partial-prefix".into(), true, true).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let call = tokio::spawn(async move {
        registry
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
    });
    started.await?;
    call.abort();
    let _ = call.await;
    let record = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::execution::wait_for(&id),
    )
    .await??;
    assert_eq!(record.state, crate::execution::RunState::Cancelled);
    assert!(tool.dropped.load(Ordering::SeqCst));
    assert!(std::fs::read_to_string(record.output_path.unwrap())?.starts_with("partial-prefix"));
    Ok(())
}

#[tokio::test]
async fn background_promotion_keeps_identity_and_survives_parent_and_wait_cancellation()
-> anyhow::Result<()> {
    let (registry, tool, started) = fixture("background-body".into(), true, true).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let parent = ctx.graceful_shutdown_signal.clone().unwrap();
    let call = tokio::spawn(async move {
        registry
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
    });
    started.await?;
    assert!(crate::execution::promote(&id).await?);
    call.abort();
    let _ = call.await;
    parent.fire();
    let wait_id = id.clone();
    let wait = tokio::spawn(async move { crate::execution::wait_for(&wait_id).await });
    wait.abort();
    let _ = wait.await;
    assert!(!tool.dropped.load(Ordering::SeqCst));
    tool.release.as_ref().unwrap().notify_one();
    let record = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::execution::wait_for(&id),
    )
    .await??;
    assert_eq!(record.id, id);
    assert!(record.background);
    assert_eq!(record.state, crate::execution::RunState::Completed);
    assert_eq!(
        std::fs::read_to_string(record.output_path.unwrap())?,
        "background-body"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_stop_reaches_a_background_run() -> anyhow::Result<()> {
    let (registry, tool, started) = fixture("retained-before-stop".into(), true, true).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let call = tokio::spawn(async move {
        registry
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
    });
    started.await?;
    assert!(crate::execution::promote(&id).await?);
    assert!(crate::execution::request_stop(
        &id,
        StopCause::HumanCancellation
    )?);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), call)
            .await??
            .is_err()
    );
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Cancelled);
    assert_eq!(record.stop_cause, Some(StopCause::HumanCancellation));
    assert!(tool.dropped.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn compatibility_background_cancel_stops_the_actual_supervised_run() -> anyhow::Result<()> {
    let (registry, tool, started) = fixture("before-controlled-stop".into(), true, true).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let session = ctx.session_id.clone();
    let call = tokio::spawn(async move {
        registry
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
    });
    started.await?;
    assert!(crate::execution::promote(&id).await?);
    let control = crate::execution::background_control(&id)?.unwrap();
    let dir = tempfile::tempdir()?;
    let manager =
        crate::background::BackgroundTaskManager::with_output_dir(dir.path().to_path_buf());
    let task = manager
        .adopt_controlled("execution_fixture", &session, call, control)
        .await?;
    assert!(manager.cancel(&task.task_id).await?);
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Cancelled);
    assert!(tool.dropped.load(Ordering::SeqCst));
    assert_eq!(
        manager
            .status(&task.task_id)
            .await
            .unwrap()
            .error
            .as_deref(),
        Some("Cancelled by user")
    );
    Ok(())
}

#[tokio::test]
async fn bg_controls_and_reads_a_foreground_run_by_durable_identity() -> anyhow::Result<()> {
    let body = format!("first\r\n{}\r\nlast\r\n", "界".repeat(90_000));
    let (registry, tool, started) = fixture(body, true, true).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let owner = registry.clone_with_shared_context_runtime();
    let call = tokio::spawn(async move {
        owner
            .execute("execution_fixture", serde_json::json!({}), ctx)
            .await
    });
    started.await?;
    let status = registry
        .execute(
            "bg",
            serde_json::json!({"action":"status","task_id":id}),
            context(),
        )
        .await?;
    assert_eq!(status.metadata.unwrap()["state"], "running");
    let output = registry
        .execute(
            "bg",
            serde_json::json!({"action":"tail","task_id":id,"tail_lines":1}),
            context(),
        )
        .await?;
    assert!(output.output.starts_with("last\r\n"));
    let wait = registry
        .execute(
            "bg",
            serde_json::json!({"action":"wait","task_id":id,"max_wait_seconds":0}),
            context(),
        )
        .await?;
    assert_eq!(wait.metadata.unwrap()["reason"], "timeout");
    registry
        .execute(
            "bg",
            serde_json::json!({"action":"cancel","task_id":id}),
            context(),
        )
        .await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), call)
            .await??
            .is_err()
    );
    let wait = registry
        .execute(
            "bg",
            serde_json::json!({"action":"wait","task_id":id,"include_output_preview":false}),
            context(),
        )
        .await?;
    assert_eq!(wait.metadata.unwrap()["run"]["state"], "cancelled");
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn native_bash_retains_full_output_and_backgrounds_the_same_command() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    for background in [true, false] {
        let mut ctx = context();
        ctx.working_dir = Some(directory.path().into());
        let id = crate::execution::invocation_id(&ctx);
        let marker = if background { "explicit" } else { "promoted" };
        let input = serde_json::json!({"command":format!("printf x >> {marker}; python3 -c 'import sys; sys.stdout.write(\"z\"*160000+\"TAIL\"); sys.stderr.write(\"ERROR_TAIL\")'; sleep 1; printf FINISHED"),"run_in_background":background,"timeout":if background{None}else{Some(20)},"notify":false,"wake":false});
        let output = registry.execute("bash", input.clone(), ctx.clone()).await?;
        assert!(matches!(output.source, OutputSource::Acceptance(_)));
        let completed = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::execution::wait_for(&id),
        )
        .await??;
        assert_eq!(completed.state, crate::execution::RunState::Completed);
        let path = completed.output_path.unwrap();
        let stdout = std::fs::read_to_string(path.with_file_name("stdout.bin"))?;
        assert!(stdout.contains(&format!("{}TAIL", "z".repeat(160000))));
        assert!(stdout.ends_with("FINISHED"));
        assert_eq!(
            std::fs::read(path.with_file_name("stderr.bin"))?,
            b"ERROR_TAIL"
        );
        let replay = registry.execute("bash", input, ctx).await?;
        assert_eq!(replay.output, output.output);
        assert_eq!(std::fs::read(directory.path().join(marker))?, b"x");
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn captured_bash_keeps_the_existing_stdin_request_workflow() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let (requests, mut input) = tokio::sync::mpsc::unbounded_channel();
    let mut ctx = context();
    ctx.stdin_request_tx = Some(requests);
    let task = tokio::spawn(async move {
        registry.execute("bash",serde_json::json!({"command":"read value; printf 'received:%s' \"$value\"","notify":false,"wake":false}),ctx).await
    });
    let request = match tokio::time::timeout(std::time::Duration::from_secs(10), input.recv()).await
    {
        Ok(Some(request)) => request,
        _ => {
            task.abort();
            anyhow::bail!("Existing native stdin detector did not produce a request");
        }
    };
    request.response_tx.send("synthetic input".into()).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task).await???;
    assert!(result.output.contains("received:synthetic input"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn native_background_status_survives_reload_quiescence_and_manager_recreation()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    let id = crate::execution::invocation_id(&ctx);
    registry.execute("bash",serde_json::json!({"command":"printf x >> effects; sleep 2; printf finished","run_in_background":true,"notify":false,"wake":false}),ctx).await?;
    assert_eq!(
        crate::background::global()
            .abort_live_tasks_for_reload()
            .await?,
        0
    );
    let manager = crate::background::BackgroundTaskManager::with_output_dir(
        directory.path().join("new-manager"),
    );
    let active = manager.status(&id).await.unwrap();
    assert_eq!(active.status, crate::bus::BackgroundTaskStatus::Running);
    assert!(!active.notify && !active.wake);
    let completed = manager
        .wait(&id, Some(std::time::Duration::from_secs(10)), false)
        .await
        .unwrap();
    assert_eq!(
        completed.task.status,
        crate::bus::BackgroundTaskStatus::Completed
    );
    assert!(manager.output(&id).await.unwrap().ends_with("finished"));
    assert_eq!(std::fs::read(directory.path().join("effects"))?, b"x");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn foreground_reload_returns_a_durable_background_receipt_without_stopping_command()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    let stop = jcode_agent_runtime::InterruptSignal::new();
    ctx.graceful_shutdown_signal = Some(stop.clone());
    let id = crate::execution::invocation_id(&ctx);
    let task = tokio::spawn(async move {
        registry.execute("bash",serde_json::json!({"command":"printf x >> effects; printf before; printf ready > ready; sleep 2; printf after","notify":false,"wake":false}),ctx).await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !directory.path().join("ready").exists() {
        if tokio::time::Instant::now() > deadline {
            task.abort();
            anyhow::bail!("Native command did not start");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    stop.fire_with_cause(StopCause::ReloadQuiescence);
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), task).await???;
    assert!(matches!(output.source, OutputSource::Acceptance(_)));
    assert_eq!(
        crate::background::global()
            .abort_live_tasks_for_reload()
            .await?,
        0
    );
    let completed = crate::background::global()
        .wait(&id, Some(std::time::Duration::from_secs(10)), false)
        .await
        .unwrap();
    assert_eq!(
        completed.task.status,
        crate::bus::BackgroundTaskStatus::Completed
    );
    assert!(
        crate::background::global()
            .output(&id)
            .await
            .unwrap()
            .ends_with("after")
    );
    assert_eq!(std::fs::read(directory.path().join("effects"))?, b"x");
    Ok(())
}
