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

#[tokio::test]
async fn provider_supplied_result_is_retained_without_invoking_the_native_producer()
-> anyhow::Result<()> {
    let (registry, tool, _) = fixture("must not execute".into(), false, false).await;
    let text = format!("{}SDK_TAIL", "λ".repeat(600_000));
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let output = registry
        .retain_provider_result(
            "execution_fixture",
            serde_json::json!({}),
            ctx.clone(),
            ToolOutput::new(&text),
        )
        .await?;
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(std::fs::read_to_string(record.output_path.unwrap())?, text);
    let replay = registry
        .retain_provider_result(
            "execution_fixture",
            serde_json::json!({}),
            ctx,
            ToolOutput::new(&text),
        )
        .await?;
    assert_eq!(replay.output, output.output);
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn read_tool_resumes_the_retained_presentation_point_without_a_source_copy()
-> anyhow::Result<()> {
    let (registry, _, _) = fixture(format!("{}TAIL", "α".repeat(2000)), false, false).await;
    let ctx = context();
    let session = ctx.session_id.clone();
    let output = registry
        .execute(
            "execution_fixture",
            serde_json::json!({"output_size":100}),
            ctx.clone(),
        )
        .await?;
    let OutputSource::Retained(reference) = output.source else {
        panic!()
    };
    let mut read_ctx = ctx;
    read_ctx.tool_call_id = "read-remainder".into();
    let page=registry.execute("read",serde_json::json!({"file_path":reference.path,"read_point":reference.continuation,"output_size":10_000}),read_ctx).await?;
    let OutputSource::ReadPage(page_ref) = page.source else {
        panic!()
    };
    assert_eq!(page_ref.start_byte, 200);
    assert!(page.output.contains("TAIL"));
    let store = crate::execution::ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    let records = store.list(&session, None, 100)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.output_path.is_some())
            .count(),
        1
    );
    Ok(())
}

#[cfg(feature = "pdf")]
fn two_page_pdf(first: &str, second: &str) -> Vec<u8> {
    let stream = |text: &str| format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
    let a = stream(first);
    let b = stream(second);
    let objects=[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{a}\nendstream",a.len()),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{b}\nendstream",b.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in &offsets[1..] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[cfg(feature = "pdf")]
#[tokio::test]
async fn pdf_pages_are_retained_completely_and_continue_without_archiving_the_original_binary()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("document.pdf");
    let first = format!("{}PAGE_ONE_TAIL", "A".repeat(25_000));
    let second = format!("{}PAGE_TWO_TAIL", "B".repeat(25_000));
    let original = two_page_pdf(&first, &second);
    std::fs::write(&path, &original)?;
    let output = registry
        .execute(
            "read",
            serde_json::json!({"file_path":path,"output_size":100}),
            context(),
        )
        .await?;
    assert_eq!(output.metadata.as_ref().unwrap()["pages"], 2);
    let OutputSource::Retained(reference) = output.source else {
        panic!()
    };
    let complete = std::fs::read_to_string(&reference.path)?;
    assert!(complete.contains(&first) && complete.contains(&second));
    let continued=registry.execute("read",serde_json::json!({"file_path":reference.path,"read_point":reference.continuation,"output_size":100_000}),context()).await?;
    assert!(continued.output.contains("PAGE_TWO_TAIL"));
    assert_eq!(std::fs::read(&path)?, original);
    let selected = registry
        .execute(
            "read",
            serde_json::json!({"file_path":path,"start_line":1,"end_line":2}),
            context(),
        )
        .await?;
    assert_eq!(
        selected.metadata.as_ref().unwrap()["line_selection"]["max_lines"],
        2
    );
    assert!(!selected.output.contains("PAGE_ONE_TAIL"));
    for part in std::fs::read_dir(reference.path.parent().unwrap())? {
        let part = part?;
        if part.file_type()?.is_file() {
            assert!(!std::fs::read(part.path())?.starts_with(b"%PDF-"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn atomic_image_read_retains_exact_pixels_and_reports_oversized_source_without_reading_it()
-> anyhow::Result<()> {
    use base64::Engine;
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("pixel.png");
    let bytes=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")?;
    std::fs::write(&path, &bytes)?;
    let output = registry
        .execute("read", serde_json::json!({"file_path":path}), context())
        .await?;
    assert_eq!(output.images.len(), 1);
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(&output.images[0].data)?,
        bytes
    );
    let large = directory.path().join("large.png");
    std::fs::File::create(&large)?.set_len(20 * 1024 * 1024 + 1)?;
    let error = registry
        .execute("read", serde_json::json!({"file_path":large}), context())
        .await
        .unwrap_err();
    let output = &error
        .downcast_ref::<crate::execution::CapturedToolError>()
        .unwrap()
        .output;
    assert!(output.images.is_empty() && output.is_error);
    assert_eq!(
        output.metadata.as_ref().unwrap()["source_bytes"],
        20 * 1024 * 1024 + 1
    );
    assert_eq!(std::fs::metadata(&large)?.len(), 20 * 1024 * 1024 + 1);
    Ok(())
}

#[tokio::test]
async fn mutation_receipts_retain_all_changed_lines_and_whitespace_before_presentation()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    let body = (0..100)
        .map(|index| format!("  CHANGED_{index:03}  \n"))
        .collect::<String>();
    for tool in ["write", "edit", "multiedit", "patch", "apply_patch"] {
        let name = format!("{tool}.txt");
        let path = directory.path().join(&name);
        if matches!(tool, "edit" | "multiedit") {
            std::fs::write(&path, "old\n")?;
        }
        let input = match tool {
            "write" => serde_json::json!({"file_path":name,"content":body,"output_size":100}),
            "edit" => {
                serde_json::json!({"file_path":name,"old_string":"old\n","new_string":body,"output_size":100})
            }
            "multiedit" => {
                serde_json::json!({"file_path":name,"edits":[{"old_string":"old\n","new_string":body}],"output_size":100})
            }
            "patch" => {
                serde_json::json!({"patch_text":format!("--- /dev/null\n+++ b/{name}\n@@ -0,0 +1,100 @@\n{}",body.lines().map(|line|format!("+{line}\n")).collect::<String>()),"output_size":100})
            }
            _ => {
                serde_json::json!({"patch_text":format!("*** Begin Patch\n*** Add File: {name}\n{}*** End Patch\n",body.lines().map(|line|format!("+{line}\n")).collect::<String>()),"output_size":100})
            }
        };
        let mut ctx = context();
        ctx.working_dir = Some(directory.path().into());
        let output = registry.execute(tool, input, ctx).await?;
        let OutputSource::Retained(reference) = output.source else {
            panic!()
        };
        let receipt = std::fs::read_to_string(reference.path)?;
        assert!(
            receipt.contains("100+   CHANGED_099  \n"),
            "{tool}: final changed line missing"
        );
        assert_eq!(std::fs::read_to_string(&path)?, body, "{tool}");
    }
    Ok(())
}

#[tokio::test]
async fn unified_patch_receipt_uses_actual_whole_file_line_numbers() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    std::fs::write(directory.path().join("source"), "first\nsecond\nthird\n")?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    let output=registry.execute("patch",serde_json::json!({"patch_text":"--- a/source\n+++ b/source\n@@ -3,1 +3,1 @@\n-third\n+changed\n"}),ctx).await?;
    assert!(output.output.contains("3- third\n3+ changed\n"));
    Ok(())
}

#[tokio::test]
async fn partial_mutation_failure_retains_prior_receipts_and_does_not_rollback_effects()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    std::fs::write(directory.path().join("blocker"), "not a directory")?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    let id = crate::execution::invocation_id(&ctx);
    let error=registry.execute("apply_patch",serde_json::json!({"patch_text":"*** Begin Patch\n*** Add File: first.txt\n+completed-first\n*** Add File: blocker/child\n+cannot-write\n*** Add File: unstarted.txt\n+must-not-start\n*** End Patch\n"}),ctx).await.expect_err("Second write must fail");
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Failed);
    let receipt = std::fs::read_to_string(record.output_path.unwrap())?;
    assert!(
        receipt.contains("first.txt") && receipt.contains("completed-first"),
        "Earlier successful receipt was lost: {error}"
    );
    assert!(receipt.contains("blocker/child"));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("first.txt"))?,
        "completed-first\n"
    );
    assert!(!directory.path().join("unstarted.txt").exists());
    Ok(())
}

#[tokio::test]
async fn multi_edit_and_unified_patch_report_partial_failure_structurally() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    std::fs::write(directory.path().join("source"), "original\n")?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    let result=registry.execute("multiedit",serde_json::json!({"file_path":"source","edits":[{"old_string":"original","new_string":"changed"},{"old_string":"missing","new_string":"unused"}]}),ctx).await;
    assert!(
        result.is_err(),
        "Partial edit must not be a completed success"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("source"))?,
        "changed\n"
    );
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    assert!(registry.execute("patch",serde_json::json!({"patch_text":"--- a/missing\n+++ b/missing\n@@ -1,1 +1,1 @@\n-old\n+new\n"}),ctx).await.is_err());
    Ok(())
}

#[tokio::test]
async fn same_target_patch_move_does_not_delete_the_written_source() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("nested"))?;
    std::fs::write(directory.path().join("source"), "old\n")?;
    let mut ctx = context();
    ctx.working_dir = Some(directory.path().into());
    registry.execute("apply_patch",serde_json::json!({"patch_text":"*** Begin Patch\n*** Update File: source\n*** Move to: nested/../source\n@@\n-old\n+new\n*** End Patch\n"}),ctx).await?;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("source"))?,
        "new\n"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn native_command_progress_and_checkpoint_keep_raw_bytes_and_wake_only_waiters()
-> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let mut bus = crate::bus::Bus::global().subscribe();
    registry.execute("bash",serde_json::json!({"command":"sleep 1; printf 'JCODE_PRO'; sleep 0.1; printf 'GRESS {\"percent\":25,\"message\":\"first\"}\n'; sleep 1; printf 'JCODE_CHECKPOINT {\"message\":\"checkpoint reached\"}\n'; sleep 2; printf done","run_in_background":true,"notify":false,"wake":false}),ctx).await?;
    let first=registry.execute("bg",serde_json::json!({"action":"wait","task_id":id,"return_on_progress":true,"max_wait_seconds":10}),context()).await?;
    assert_eq!(first.metadata.as_ref().unwrap()["reason"], "progress");
    assert_eq!(
        first.metadata.as_ref().unwrap()["run"]["progress"]["value"]["percent"],
        25.0
    );
    let second=registry.execute("bg",serde_json::json!({"action":"wait","task_id":id,"return_on_progress":true,"max_wait_seconds":10}),context()).await?;
    assert_eq!(second.metadata.as_ref().unwrap()["reason"], "checkpoint");
    let status = crate::background::global().status(&id).await.unwrap();
    assert!(status.progress.is_some());
    assert!(matches!(
        status.event_history.last().unwrap().kind,
        crate::background::BackgroundTaskEventKind::Checkpoint
    ));
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Completed);
    let raw = std::fs::read_to_string(record.output_path.unwrap().with_file_name("stdout.bin"))?;
    assert!(
        raw.contains("JCODE_PROGRESS") && raw.contains("JCODE_CHECKPOINT") && raw.ends_with("done")
    );
    let mut progress_seen = false;
    while let Ok(event) = bus.try_recv() {
        if let crate::bus::BusEvent::BackgroundTaskProgress(event) = event
            && event.task_id == id
        {
            progress_seen = true;
        }
    }
    assert!(progress_seen);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn native_nonzero_command_exit_is_not_a_completed_success() -> anyhow::Result<()> {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let ctx = context();
    let id = crate::execution::invocation_id(&ctx);
    let error = registry
        .execute(
            "bash",
            serde_json::json!({"command":"printf failed-command; exit 7"}),
            ctx,
        )
        .await
        .expect_err("Nonzero exit must fail");
    assert!(
        error
            .downcast_ref::<crate::execution::CapturedToolError>()
            .is_some()
    );
    let record = crate::execution::wait_for(&id).await?;
    assert_eq!(record.state, crate::execution::RunState::Failed);
    assert!(std::fs::read_to_string(record.output_path.unwrap())?.contains("failed-command"));
    Ok(())
}
