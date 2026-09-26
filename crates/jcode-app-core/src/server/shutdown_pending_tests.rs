//! Real SQLite publication contention at the native invocation boundary.
use super::*;
use crate::execution::{ExecutionStore, RunState, StorageConfig};
use crate::tool::ToolContext;
use std::num::NonZeroUsize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct WriterLock(tokio::process::Child);
impl WriterLock {
    async fn acquire(root: &Path) -> Result<Self> {
        let mut child = tokio::process::Command::new("python3")
            .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); print('locked',flush=True); sys.stdin.read(1); c.rollback()"])
            .arg(root.join("execution/index.sqlite"))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .kill_on_drop(true).spawn()?;
        let mut reader = BufReader::new(child.stdout.take().context("Writer stdout")?);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await??;
        ensure!(
            line.trim() == "locked",
            "Fixture writer did not acquire SQLite: {line}"
        );
        Ok(Self(child))
    }
    async fn release(mut self) -> Result<()> {
        self.0
            .stdin
            .as_mut()
            .context("Writer stdin")?
            .write_all(b"x")
            .await?;
        ensure!(
            tokio::time::timeout(Duration::from_secs(5), self.0.wait())
                .await??
                .success(),
            "Writer release failed"
        );
        Ok(())
    }
}

async fn wait_for(mut predicate: impl FnMut() -> Result<bool>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate()? {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

fn pending_publication(background: bool, failed: bool) -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = RuntimeLifecycle::new(
            sandbox.root(),
            &sandbox.root().join("runtime.sock"),
            Arc::new(PrimaryHost::default()),
            BackgroundTaskManager::with_output_dir(sandbox.root().join("background")),
        )
        .await?;
        let store = ExecutionStore::open(sandbox.root())?;
        let work = tempfile::tempdir()?;
        let writer = WriterLock::acquire(sandbox.root()).await?;
        let ctx = ToolContext {
            session_id: "pending-native-fixture".into(),
            message_id: "pending-foreground".into(),
            tool_call_id: "native".into(),
            working_dir: Some(work.path().into()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
            invocation: jcode_tool_core::InvocationContext {
                policy: jcode_tool_core::ExecutionPolicy {
                    capture: jcode_tool_core::CaptureMode::NativeCommand,
                    background,
                    manual_ready: true,
                    cooperative_stop: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let command = if background { "printf x >> effect; printf before; while [ ! -f release ]; do sleep 0.05; done; printf native-result" } else { "printf x >> effect; printf native-result" }.to_string();
        let invocation =
            crate::execution::invocation(&ctx, "bash", serde_json::json!({"command":command}));
        let id = invocation.id();
        let request = crate::execution::command_handoff::CommandRequest {
            command,
            working_dir: work.path().into(),
            timeout_ms: Some(20_000),
            background,
            notify: false,
            wake: false,
            title: None,
            storage: StorageConfig::default(),
        };
        let producer = tokio::spawn(crate::execution::execute(
            invocation,
            ctx,
            NonZeroUsize::new(2000).unwrap(),
            Box::new(move |ctx| Box::pin(async move {
                if failed { anyhow::bail!("Synthetic native preparation failed before launch"); }
                crate::execution::command_worker::launch(request, ctx).await
            })),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if lifecycle
                    .work()
                    .await?
                    .iter()
                    .any(|item| item.id == id && item.supported_survivor)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        ensure!(
            store.inspect(&id)?.is_none(),
            "Fixture did not hold row publication"
        );
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::FinishCurrent,
                    independent: IndependentTasks::KeepSupported,
                    quiescence_timeout_seconds: 2,
                },
            })
            .await?
        else {
            anyhow::bail!("Review missing")
        };
        let RuntimeResponse::Operation(operation) = lifecycle
            .request(RuntimeRequest::Begin {
                request: crate::workspace::RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            anyhow::bail!("Operation missing")
        };
        // Let the production driver observe admitted work while WAL readers can
        // proceed but the supervisor cannot publish its row.
        tokio::time::sleep(Duration::from_millis(300)).await;
        ensure!(
            store.inspect(&id)?.is_none(),
            "Publication escaped the real writer lock"
        );
        writer.release().await?;
        let outcome = async {
            if !background || failed {
                wait_for(|| Ok(store.inspect(&id)?.is_some_and(|r|r.state.terminal()))).await?;
            }
            let mut stopped = lifecycle.stopped();
            tokio::time::timeout(Duration::from_secs(3), async {
                while stopped.borrow_and_update().is_none() {
                    stopped.changed().await?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("FinishCurrent stranded after native row publication")??;
            let record = lifecycle.owner.inspect(operation.id)?;
            ensure!(
                record.phase == ShutdownPhase::Stopped
                    && record.remaining.is_empty()
                    && (if background && !failed {record.preserved.len()==1 && record.preserved[0].id==id} else {record.preserved.is_empty()})
                    && record.issues.is_empty(),
                "Incorrect terminal shutdown: {record:?}"
            );
            if failed {
                ensure!(!work.path().join("effect").exists() && store.inspect(&id)?.unwrap().state==RunState::Failed, "Failed preparation started effects or fabricated success");
            } else {
                if background {
                    wait_for(||Ok(work.path().join("effect").exists())).await?;
                    ensure!(store.inspect(&id)?.unwrap().state==RunState::Running, "Background survivor ended before explicit release");
                }
                ensure!(std::fs::read(work.path().join("effect"))?==b"x", "Native effect replayed");
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        std::fs::write(work.path().join("release"),b"release")?;
        wait_for(||Ok(store.inspect(&id)?.is_some_and(|r|r.state.terminal()))).await?;
        let reply = tokio::time::timeout(Duration::from_secs(10), producer).await??;
        if let Err(error) = &outcome {
            eprintln!("Expected-behavior failure: {error:#}");
        }
        // Cleanup remains reviewed even on the red implementation.
        if lifecycle.stopped.borrow().is_none() {
            let current = lifecycle.owner.inspect(operation.id)?;
            let RuntimeResponse::Review(review) = lifecycle
                .request(RuntimeRequest::ReviewChange {
                    operation: current.id,
                    expected_revision: current.revision,
                    options: ShutdownOptions {
                        strategy: StopStrategy::Interrupt,
                        independent: IndependentTasks::Stop,
                        quiescence_timeout_seconds: 5,
                    },
                })
                .await?
            else {
                anyhow::bail!("Cleanup review missing")
            };
            lifecycle
                .request(RuntimeRequest::Begin {
                    request: crate::workspace::RequestId::new(),
                    review: review.id,
                })
                .await?;
            let mut stopped = lifecycle.stopped();
            tokio::time::timeout(Duration::from_secs(10), async {
                while stopped.borrow_and_update().is_none() {
                    stopped.changed().await?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await??;
        }
        if !failed {
            let output=store.result(&store.inspect(&id)?.unwrap(), NonZeroUsize::new(2000).unwrap())?;
            ensure!(output.output.contains("native-result"), "Original output unavailable");
            if background {ensure!(matches!(reply?.source,jcode_tool_types::OutputSource::Acceptance(_)), "Missing same-run background receipt");} else {reply?;}
        } else {ensure!(reply.is_err(), "Failed preparation returned success");}
        if let Some(transfer)=store.command_ownership(&id)? && let Some(worker)=transfer.worker {
            wait_for(||Ok(!store.runtime_endpoint(&worker)?.context("Worker identity missing")?.has_live_lease()?)).await?;
        }
        ensure!(crate::background::global().abort_live_tasks_for_reload().await?==0,"Fixture left legacy work");
        ensure!(store.list_all(None, 2)?.len() == 1, "Preparation produced another invocation");
        let stopped_store = RuntimeStopStore::new(
            &crate::storage::durable_state_dir(), &sandbox.root().join("runtime.sock"),
        )?;
        drop(lifecycle);
        wait_for(|| Ok(!stopped_store.owner_is_live()?)).await?;
        outcome
    })
}

#[test]
fn pending_foreground_publication_finishes_without_another_control() -> Result<()> {
    pending_publication(false, false)
}
#[test]
fn pending_background_publication_survives_without_another_control() -> Result<()> {
    pending_publication(true, false)
}
#[test]
fn failed_native_preparation_does_not_strand_finish() -> Result<()> {
    pending_publication(true, true)
}

async fn fixture(root: &Path) -> Result<Arc<RuntimeLifecycle>> {
    RuntimeLifecycle::new(
        root,
        &root.join("runtime.sock"),
        Arc::new(PrimaryHost::default()),
        BackgroundTaskManager::with_output_dir(root.join("background")),
    )
    .await
}
async fn finish_review(lifecycle: &RuntimeLifecycle) -> Result<ShutdownOperation> {
    let RuntimeResponse::Review(review) = lifecycle
        .request(RuntimeRequest::Review {
            options: ShutdownOptions {
                strategy: StopStrategy::FinishCurrent,
                independent: IndependentTasks::Stop,
                quiescence_timeout_seconds: 2,
            },
        })
        .await?
    else {
        anyhow::bail!("Review missing")
    };
    let RuntimeResponse::Operation(operation) = lifecycle
        .request(RuntimeRequest::Begin {
            request: crate::workspace::RequestId::new(),
            review: review.id,
        })
        .await?
    else {
        anyhow::bail!("Operation missing")
    };
    Ok(operation)
}
async fn await_stopped(lifecycle: &RuntimeLifecycle) -> Result<()> {
    let mut receiver = lifecycle.stopped();
    tokio::time::timeout(Duration::from_secs(10), async {
        while receiver.borrow_and_update().is_none() {
            receiver.changed().await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

fn observation_failure(cancel: bool) -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let store = ExecutionStore::open(sandbox.root())?;
        let ctx = ToolContext {
            session_id: "observation-fixture".into(),
            message_id: "held".into(),
            tool_call_id: "producer".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let invocation = crate::execution::invocation(&ctx, "fixture", serde_json::json!({}));
        let id = invocation.id();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, proceed) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(crate::execution::execute(
            invocation,
            ctx,
            NonZeroUsize::new(2000).unwrap(),
            Box::new(move |_| {
                Box::pin(async move {
                    let _ = entered.send(());
                    proceed.await?;
                    Ok(jcode_tool_types::ToolOutput::new("retained fixture result"))
                })
            }),
        ));
        ready.await?;
        let owner = store.inspect(&id)?.context("Producer missing")?.owner;
        let path = store
            .root()
            .join("runtimes")
            .join(format!("{owner}.namespace.json"));
        let original = std::fs::read(&path)?;
        let operation = finish_review(&lifecycle).await?;
        std::fs::write(&path, b"invalid owned fixture namespace")?;
        let outcome = async {
            wait_for(|| Ok(!lifecycle.owner.inspect(operation.id)?.issues.is_empty())).await?;
            let observed = lifecycle.owner.inspect(operation.id)?;
            ensure!(
                observed.phase == ShutdownPhase::WaitingForCurrent && !observed.cancellation_closed,
                "Observation failure closed waiting cancellation"
            );
            match lifecycle
                .request(RuntimeRequest::Inspect {
                    operation: operation.id,
                })
                .await
            {
                Ok(RuntimeResponse::Operation(op)) => {
                    ensure!(!op.issues.is_empty(), "Silent failure")
                }
                Err(_) => {} // A failure report itself may currently be pending.
                other => anyhow::bail!("Unexpected observation reply: {other:?}"),
            }
            if cancel {
                lifecycle
                    .request(RuntimeRequest::CancelWait {
                        operation: observed.id,
                        expected_revision: observed.revision,
                    })
                    .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        std::fs::write(path, original)?;
        let _ = release.send(());
        producer.await??;
        if outcome.is_ok() && !cancel {
            await_stopped(&lifecycle).await?;
            let final_state = lifecycle.owner.inspect(operation.id)?;
            ensure!(
                final_state.issues.is_empty() && final_state.remaining.is_empty(),
                "Recovered observation retained stale issues"
            );
        }
        if outcome.is_ok() && cancel {
            ensure!(
                lifecycle.owner.inspect(operation.id)?.phase == ShutdownPhase::Cancelled
                    && lifecycle.primaries.accepts_input(),
                "Cancel did not restore admission"
            );
        }
        outcome
    })
}
#[test]
fn observation_error_is_visible_and_automatically_recovers() -> Result<()> {
    observation_failure(false)
}
#[test]
fn observation_error_preserves_waiting_cancellation() -> Result<()> {
    observation_failure(true)
}

struct RestoreJournal(std::path::PathBuf, std::path::PathBuf);
impl Drop for RestoreJournal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
        let _ = std::fs::rename(&self.1, &self.0);
    }
}
#[test]
fn journal_failure_is_visible_and_reporting_recovers_without_new_control() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let held = lifecycle.registration.admission().independent(
            RuntimeWorkKind::Preparation,
            "journal-fixture".into(),
            None,
        )?;
        let operation = finish_review(&lifecycle).await?;
        let path = crate::storage::durable_state_dir()
            .join("runtime-lifecycle")
            .join(lifecycle.owner.namespace())
            .join("journal.json");
        let backup = path.with_extension("fixture-original");
        std::fs::rename(&path, &backup)?;
        let restore = RestoreJournal(path.clone(), backup);
        std::fs::create_dir(&path)?;
        drop(held);
        let outcome = async {
            wait_for(|| Ok(lifecycle.driver_fault.lock().unwrap().is_some()))
                .await
                .context("Journal fault was not surfaced")?;
            ensure!(
                lifecycle
                    .request(RuntimeRequest::Inspect {
                        operation: operation.id
                    })
                    .await
                    .is_err(),
                "Journal failure looked like successful healthy waiting"
            );
            ensure!(
                lifecycle.stopped.borrow().is_none(),
                "Failed checkpoint reported exit"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        drop(restore);
        if outcome.is_ok() {
            await_stopped(&lifecycle)
                .await
                .context("Journal recovery stranded the driver")?;
            ensure!(
                lifecycle.owner.inspect(operation.id)?.issues.is_empty(),
                "Recovered journal left errors"
            );
        }
        outcome
    })
}

#[test]
fn an_earlier_driver_failure_cannot_overwrite_a_new_retry() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let held = lifecycle.registration.admission().independent(
            RuntimeWorkKind::Preparation,
            "retry-fixture".into(),
            None,
        )?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 1,
                },
            })
            .await?
        else {
            anyhow::bail!("Review missing")
        };
        let RuntimeResponse::Operation(operation) = lifecycle
            .request(RuntimeRequest::Begin {
                request: crate::workspace::RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            anyhow::bail!("Operation missing")
        };
        wait_for(|| Ok(lifecycle.owner.inspect(operation.id)?.phase == ShutdownPhase::Blocked))
            .await?;
        let old_epoch = lifecycle
            .control_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let blocked = lifecycle.owner.inspect(operation.id)?;
        lifecycle
            .request(RuntimeRequest::Retry {
                operation: blocked.id,
                expected_revision: blocked.revision,
            })
            .await?;
        ensure!(
            lifecycle
                .report_driver_failure(Some(blocked.id), old_epoch, "synthetic prior observation")
                .await?,
            "New retry lost its progression"
        );
        ensure!(
            lifecycle.owner.inspect(blocked.id)?.phase == ShutdownPhase::Stopping,
            "Old failure blocked a later reviewed attempt"
        );
        drop(held);
        await_stopped(&lifecycle).await
    })
}
