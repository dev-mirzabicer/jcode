use super::*;
use crate::execution::command_handoff::CommandRequest;
use crate::runtime_lifecycle::RuntimeStopStore;
use crate::workspace::{RequestId, runtime::*};
use jcode_tool_core::{CaptureMode, ExecutionPolicy, InvocationContext, ToolExecutionMode};
use std::process::{Child, Command, Stdio};

fn context(cwd: &std::path::Path, background: bool) -> ToolContext {
    ToolContext {
        session_id: format!("shutdown-{}", uuid::Uuid::new_v4()),
        message_id: "message".into(),
        tool_call_id: "command".into(),
        working_dir: Some(cwd.into()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: InvocationContext {
            output_target: NonZeroUsize::new(2000),
            policy: ExecutionPolicy {
                capture: CaptureMode::NativeCommand,
                background,
                manual_ready: true,
                cooperative_stop: true,
                notify: true,
                wake: true,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}
async fn wait_path(path: &std::path::Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Fixture did not reach its recorded checkpoint")
}
fn stop_store(root: &std::path::Path) -> Result<RuntimeStopStore> {
    RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &root.join("shutdown-fixture.sock"),
    )
}

/// A native subprocess fixture, not an Agent or a model call. It actually exits
/// after the production handoff, so survival cannot rely on a live parent lease.
#[tokio::test]
async fn parent_fixture() -> Result<()> {
    let Ok(mode) = std::env::var("JCODE_WP08_NATIVE_PARENT") else {
        return Ok(());
    };
    let root = crate::storage::jcode_dir()?;
    let work = PathBuf::from(std::env::var("JCODE_WP08_NATIVE_CWD")?);
    let lifecycle = crate::server::shutdown::RuntimeLifecycle::new(
        &root,
        &root.join("shutdown-fixture.sock"),
        Arc::new(crate::primary::PrimaryHost::default()),
        crate::background::global().clone(),
    )
    .await?;
    let ctx = context(&work, mode == "background");
    let command = "printf x >> effects; printf before; printf ready > ready; while [ ! -f release ]; do sleep 0.05; done; printf after".to_owned();
    let invocation = invocation(&ctx, "bash", serde_json::json!({"command":command}));
    let id = invocation.id();
    std::fs::write(
        work.join("invocation.json"),
        serde_json::to_vec(&invocation)?,
    )?;
    let request = CommandRequest {
        command,
        working_dir: work.clone(),
        timeout_ms: Some(45_000),
        background: mode == "background",
        notify: true,
        wake: true,
        title: None,
        storage: StorageConfig::default(),
    };
    let task = tokio::spawn(execute(
        invocation,
        ctx,
        NonZeroUsize::new(2000).unwrap(),
        Box::new(move |ctx| Box::pin(command_worker::launch(request, ctx))),
    ));
    wait_path(&work.join("ready")).await?;
    let RuntimeResponse::Review(review) = lifecycle
        .request(RuntimeRequest::Review {
            options: ShutdownOptions {
                strategy: StopStrategy::Interrupt,
                independent: IndependentTasks::KeepSupported,
                quiescence_timeout_seconds: 10,
            },
        })
        .await?
    else {
        panic!("review");
    };
    let RuntimeResponse::Operation(operation) = lifecycle
        .request(RuntimeRequest::Begin {
            request: RequestId::new(),
            review: review.id,
        })
        .await?
    else {
        panic!("operation");
    };
    let output = task.await??;
    ensure!(
        matches!(output.source, OutputSource::Acceptance(_)),
        "Original waiter did not receive its same-run acceptance"
    );
    let mut stopped = lifecycle.stopped();
    tokio::time::timeout(Duration::from_secs(12), stopped.wait_for(|id| id.is_some())).await??;
    let RuntimeResponse::Operation(completed) = lifecycle
        .request(RuntimeRequest::Inspect {
            operation: operation.id,
        })
        .await?
    else {
        panic!("operation");
    };
    ensure!(
        completed.phase == ShutdownPhase::Stopped
            && completed.preserved.iter().any(|work| work.id == id),
        "Coordinator lost the surviving command"
    );
    let store = ExecutionStore::open(&root)?;
    let parent = store
        .command_ownership(&id)?
        .context("Native transfer missing")?
        .parent;
    std::fs::write(work.join("parent-owner"), parent)?;
    std::fs::write(work.join("parent-done"), b"verified-handoff")?;
    // Exiting only this isolated fixture proves all its in-process ownership
    // (including BackgroundManager controls) is gone, not just a closed socket.
    std::process::exit(0)
}

async fn wait_child(child: &mut Child) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "Fixture runtime exited with {status}");
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Fixture runtime did not finish handoff")?
}

async fn survival_case(
    root: &std::path::Path,
    background: bool,
    legacy: Option<&std::path::Path>,
) -> Result<()> {
    let work = tempfile::tempdir()?;
    #[cfg(target_os = "macos")]
    let (workspace, location) = crate::workspace::test_support::registered_checkout(
        &crate::storage::durable_state_dir(),
        work.path(),
    );
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(path) = legacy {
        command.env("JCODE_WP08_LEGACY_WORKER", path);
    } else {
        command.env_remove("JCODE_WP08_LEGACY_WORKER");
    }
    let mut parent = command
        .args([
            "--exact",
            "execution::shutdown::tests::parent_fixture",
            "--nocapture",
        ])
        .env(
            "JCODE_WP08_NATIVE_PARENT",
            if background {
                "background"
            } else {
                "foreground"
            },
        )
        .env("JCODE_WP08_NATIVE_CWD", work.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    let outcome = async {
        wait_child(&mut parent).await?;
        ensure!(work.path().join("parent-done").exists(), "No durable native handoff proof");
        let invocation: Invocation = serde_json::from_slice(&std::fs::read(work.path().join("invocation.json"))?)?;
        let id = invocation.id();
        let store = ExecutionStore::open(root)?;
        let original = std::fs::read_to_string(work.path().join("parent-owner"))?;
        ensure!(!store.runtime_endpoint(&original)?.context("Original owner missing")?.has_live_lease()?, "Original runtime lease is still live");
        let before = store.inspect(&id)?.context("Survivor missing")?;
        if legacy.is_some() { ensure!(store.runtime_endpoint(&before.owner)?.context("Legacy endpoint missing")?.protocol_version == 2, "Fixture did not exercise a v2 predecessor worker"); }
        ensure!(before.background && !before.state.terminal() && before.stop_cause.is_none(), "Worker was stopped instead of preserved");
        let lifecycle = stop_store(root)?;
        #[cfg(target_os = "macos")]
        {
            let (_, report) = crate::workspace::test_support::closeout_work(&workspace, location, root, &crate::storage::durable_state_dir()).await;
            ensure!(report.findings.iter().any(|finding| finding.kind == crate::workspace::CloseoutWorkKind::PhysicalLease), "Preserved worker lost its original checkout lifetime lease");
        }
        ensure!(lifecycle.require_automatic_start().is_err(), "Completion must not undo intentional Stop");
        lifecycle.authorize_start()?;
        let owner = lifecycle.claim()?;
        let owned = OwnedExecutions::bind(root, &owner).await?;
        ensure!(owned.inventory().await?.iter().any(|run| run.id == id && run.supported_survivor), "Start did not recover the original native identity");
        ensure!(owned.preserve(&id, Duration::from_secs(2)).await?.is_some(), "Idempotent survivor review lost work");
        let legacy_stop = legacy.is_some() && background;
        if legacy_stop {
            owned.stop(&id, false, Duration::from_secs(10)).await?;
        } else {
            std::fs::write(work.path().join("release"), b"release")?;
        }
        let reply = tokio::time::timeout(Duration::from_secs(10), runtime::control_in_store(&store, &id, ControlOperation::Wait)).await??;
        ensure!(matches!(reply, ControlReply::Snapshot { ref record } if record.state == if legacy_stop { RunState::Cancelled } else { RunState::Completed }), "Survivor did not reach its actual terminal state");
        let record = store.inspect(&id)?.context("Terminal survivor missing")?;
        let output = store.result(&record, NonZeroUsize::new(2000).unwrap())?;
        ensure!(output.output.contains("before") && (legacy_stop || output.output.contains("after")), "Survivor lost output");
        if legacy_stop { ensure!(record.stop_cause == Some(StopCause::HumanCancellation), "Old transport did not receive its supported cancellation cause"); }
        ensure!(std::fs::read(work.path().join("effects"))? == b"x", "Native work ran more than once");
        let mut ctx = context(work.path(), background);
        ctx.session_id = invocation.session_id.clone(); ctx.message_id = invocation.message_id.clone(); ctx.tool_call_id = invocation.call_path[0].clone();
        if !legacy_stop { execute(invocation, ctx, NonZeroUsize::new(2000).unwrap(), Box::new(|_| Box::pin(async { anyhow::bail!("Producer replayed") }))).await?; }
        ensure!(std::fs::read(work.path().join("effects"))? == b"x", "Transport replay repeated effects");
        runtime::end_test_listener(&owned.runtime).await;
        Ok::<_, anyhow::Error>(())
    }.await;
    // Cleanup is attempted before returning even when a check fails. All targets
    // are captured fixture handles or the exact invocation written before spawn.
    let cleanup = async {
        if parent.try_wait()?.is_none() {
            parent.kill()?;
        }
        parent.wait()?;
        if let Ok(bytes) = std::fs::read(work.path().join("invocation.json")) {
            let input: Invocation = serde_json::from_slice(&bytes)?;
            let store = ExecutionStore::open(root)?;
            if let Some(record) = store.inspect(&input.id())?
                && !record.state.terminal()
            {
                runtime::control_in_store(
                    &store,
                    &record.id,
                    ControlOperation::Stop {
                        cause: StopCause::HumanCancellation,
                    },
                )
                .await?;
                tokio::time::timeout(
                    Duration::from_secs(15),
                    runtime::control_in_store(&store, &record.id, ControlOperation::Wait),
                )
                .await??;
            }
            if let Some(record) = store.inspect(&input.id())? {
                let endpoint = store
                    .runtime_endpoint(&record.owner)?
                    .context("Fixture worker ownership metadata disappeared")?;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while endpoint.has_live_lease()? {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await??;
            }
        }
        // Promotion also registers a process-global completion observer. Its
        // durable work is terminal above, but it may not have removed its map
        // entry before this test's Tokio runtime and temporary store disappear.
        // Settle through the existing owner while the receipt is still readable.
        ensure!(
            crate::background::global()
                .abort_live_tasks_for_reload()
                .await?
                == 0,
            "Fixture left live legacy background work"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    std::fs::write(
        root.join("shutdown-fixture-cleanup.json"),
        serde_json::to_vec(
            &serde_json::json!({"background":background,"checks":outcome.as_ref().err().map(ToString::to_string),"cleanup":cleanup.as_ref().err().map(ToString::to_string)}),
        )?,
    )?;
    cleanup?;
    outcome
}

#[test]
fn native_foreground_handoff_survives_parent_exit_and_explicit_start() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(survival_case(sandbox.root(), false, None))
}
#[test]
fn native_background_handoff_survives_parent_exit_and_explicit_start() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(survival_case(sandbox.root(), true, None))
}

#[test]
fn unsupported_preservation_does_not_cancel_and_timeout_is_not_terminal() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = stop_store(sandbox.root())?;
        let owner = lifecycle.claim()?;
        let owned = OwnedExecutions::bind(sandbox.root(), &owner).await?;
        let other = RuntimeStopStore::new(
            &crate::storage::durable_state_dir(),
            &sandbox.root().join("other-runtime.sock"),
        )?
        .claim()?;
        ensure!(
            OwnedExecutions::bind(sandbox.root(), &other).await.is_err(),
            "Execution endpoint was silently rebound to another namespace"
        );
        let mut ctx = context(sandbox.root(), false);
        ctx.invocation.policy.capture = CaptureMode::Complete;
        ctx.invocation.policy.cooperative_stop = false;
        let input = invocation(&ctx, "synthetic-owned-work", serde_json::json!({}));
        let id = input.id();
        let (ready, prepared) = oneshot::channel();
        let task = tokio::spawn(execute(
            input,
            ctx,
            NonZeroUsize::new(2000).unwrap(),
            Box::new(move |ctx| {
                Box::pin(async move {
                    ctx.invocation
                        .capture
                        .as_ref()
                        .unwrap()
                        .write(OutputStream::Text, b"retained partial")?;
                    ctx.invocation.ready.as_ref().unwrap().mark();
                    let _ = ready.send(());
                    std::future::pending::<Result<ToolOutput>>().await
                })
            }),
        ));
        prepared.await?;
        let outcome = async {
            ensure!(
                owned.preserve(&id, Duration::from_secs(1)).await.is_err(),
                "Unsupported work advertised survival"
            );
            ensure!(
                owned.store.inspect(&id)?.unwrap().stop_cause.is_none(),
                "Preserve failure destructively cancelled work"
            );
            ensure!(
                owned.stop(&id, true, Duration::from_secs(1)).await.is_err(),
                "Force without native ownership was accepted"
            );
            ensure!(
                owned.store.inspect(&id)?.unwrap().stop_cause.is_none(),
                "Unsupported Force cancelled work"
            );
            ensure!(
                owned
                    .stop(&id, false, Duration::from_millis(5))
                    .await
                    .is_err(),
                "Timeout falsely reported quiescence"
            );
            ensure!(
                !owned.store.inspect(&id)?.unwrap().state.terminal(),
                "Stop acknowledgement became terminal too early"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        owned.stop(&id, false, Duration::from_secs(5)).await?;
        ensure!(task.await?.is_err(), "Stopped producer was successful");
        ensure!(
            owned.store.inspect(&id)?.unwrap().state == RunState::Cancelled,
            "Stop did not retain terminal cancellation"
        );
        runtime::end_test_listener(&owned.runtime).await;
        outcome
    })
}

#[test]
fn damaged_native_handoff_blocks_preservation_without_destructive_fallback() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let owner = stop_store(sandbox.root())?.claim()?;
        let owned = OwnedExecutions::bind(sandbox.root(), &owner).await?;
        let work = tempfile::tempdir()?;
        let ctx = context(work.path(), false);
        let command =
            "printf x >> effects; printf before; printf ready > ready; sleep 45".to_owned();
        let input = invocation(&ctx, "bash", serde_json::json!({"command":command}));
        let id = input.id();
        let request = CommandRequest {
            command,
            working_dir: work.path().into(),
            timeout_ms: Some(45_000),
            background: false,
            notify: false,
            wake: false,
            title: None,
            storage: StorageConfig::default(),
        };
        let task = tokio::spawn(execute(
            input,
            ctx,
            NonZeroUsize::new(2000).unwrap(),
            Box::new(move |ctx| Box::pin(command_worker::launch(request, ctx))),
        ));
        wait_path(&work.path().join("ready")).await?;
        let payload = owned
            .store
            .root()
            .join("commands")
            .join(format!("{id}.json"));
        let original = std::fs::read(&payload)?;
        let outcome = async {
            std::fs::write(&payload, b"damaged fixture transfer")?;
            ensure!(
                owned.preserve(&id, Duration::from_secs(1)).await.is_err(),
                "Damaged handoff was accepted"
            );
            let record = owned
                .store
                .inspect(&id)?
                .context("Native record disappeared")?;
            ensure!(
                !record.background && record.stop_cause.is_none() && !record.state.terminal(),
                "Failed preservation cancelled or falsely handed off native work"
            );
            ensure!(
                std::fs::read(work.path().join("effects"))? == b"x",
                "Preservation repeated command effects"
            );
            std::fs::write(&payload, &original)?;
            ensure!(
                owned.preserve(&id, Duration::from_secs(5)).await?.is_some(),
                "Repaired handoff could not preserve the same command"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        std::fs::write(&payload, original)?;
        owned.stop(&id, false, Duration::from_secs(10)).await?;
        let original_reply = task.await?;
        if outcome.is_ok() {
            ensure!(
                matches!(original_reply?.source, OutputSource::Acceptance(_)),
                "Original foreground did not receive acceptance"
            );
        }
        ensure!(
            owned.store.inspect(&id)?.unwrap().stop_cause == Some(StopCause::RuntimeShutdown),
            "Explicit runtime stop lost its cause"
        );
        ensure!(
            crate::background::global()
                .abort_live_tasks_for_reload()
                .await?
                == 0,
            "Fixture left live legacy background work"
        );
        runtime::end_test_listener(&owned.runtime).await;
        outcome
    })
}

#[test]
#[ignore = "requires an explicit immutable predecessor binary and isolated state"]
fn actual_predecessor_worker_survives_and_accepts_compatible_stop() -> Result<()> {
    ensure!(
        std::env::var_os("JCODE_TEST_STATE_ROOT").is_some(),
        "Use isolated test launcher"
    );
    let binary = PathBuf::from(
        std::env::var_os("JCODE_WP08_PREDECESSOR_BINARY")
            .context("Set the exact predecessor binary")?,
    )
    .canonicalize()?;
    let version = Command::new(&binary).arg("--version").output()?;
    ensure!(version.status.success(), "Predecessor is not executable");
    println!(
        "WP08_PREDECESSOR_WORKER {} {}",
        binary.display(),
        String::from_utf8_lossy(&version.stdout)
    );
    for background in [false, true] {
        let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        tokio::runtime::Runtime::new()?.block_on(survival_case(
            sandbox.root(),
            background,
            Some(&binary),
        ))?;
        println!(
            "WP08_PREDECESSOR_CLEANUP {}",
            std::fs::read_to_string(sandbox.root().join("shutdown-fixture-cleanup.json"))?
        );
    }
    Ok(())
}
