use super::*;
use crate::workspace::RequestId;

#[test]
fn lost_lifecycle_does_not_reclassify_hosted_primaries_as_unmanaged() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let host = lifecycle.primaries.clone();
        assert!(host.accepts_input());
        drop(lifecycle);
        assert!(!host.accepts_input());
        assert!(!host.accepts_prepared_work());
        Ok(())
    })
}

#[test]
fn reviewed_change_and_explicit_force_keep_uncertain_owner_truth() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let held = lifecycle.registration.admission().independent(
            RuntimeWorkKind::Preparation,
            "unsettled".into(),
            None,
        )?;
        let first = begin(&lifecycle, StopStrategy::FinishCurrent, 1).await?;
        let first = phase(&lifecycle, first.id, ShutdownPhase::WaitingForCurrent).await?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::ReviewChange {
                operation: first.id,
                expected_revision: first.revision,
                options: ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 1,
                    destination: Default::default(),
                },
            })
            .await?
        else {
            anyhow::bail!("Review missing");
        };
        let RuntimeResponse::Operation(next) = lifecycle
            .request(RuntimeRequest::Begin {
                request: RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            anyhow::bail!("Operation missing");
        };
        ensure!(
            lifecycle.owner.inspect(first.id)?.phase == ShutdownPhase::Superseded,
            "Old operation remained active"
        );
        let blocked = phase(&lifecycle, next.id, ShutdownPhase::Blocked).await?;
        lifecycle
            .request(RuntimeRequest::Force {
                operation: blocked.id,
                expected_revision: blocked.revision,
            })
            .await?;
        let forced = phase(&lifecycle, next.id, ShutdownPhase::Forced).await?;
        let mut exit = lifecycle.stopped();
        tokio::time::timeout(
            Duration::from_secs(5),
            exit.wait_for(|value| value.is_some()),
        )
        .await??;
        ensure!(
            !forced.remaining.is_empty() && !forced.issues.is_empty(),
            "Force manufactured quiescence"
        );
        ensure!(
            lifecycle
                .stopped()
                .borrow()
                .is_some_and(|exit| exit.forced && exit.operation == next.id),
            "Force did not publish its distinct exit disposition"
        );
        drop(held);
        Ok(())
    })
}

#[test]
fn force_does_not_abandon_an_unpublished_keep_supported_handoff() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let held = lifecycle.registration.admission().independent(
            RuntimeWorkKind::Execution,
            "pending-native-owner".into(),
            None,
        )?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::KeepSupported,
                    quiescence_timeout_seconds: 1,
                    destination: Default::default(),
                },
            })
            .await?
        else {
            anyhow::bail!("Review missing");
        };
        let RuntimeResponse::Operation(operation) = lifecycle
            .request(RuntimeRequest::Begin {
                request: RequestId::new(),
                review: review.id,
            })
            .await?
        else {
            anyhow::bail!("Operation missing");
        };
        let blocked = phase(&lifecycle, operation.id, ShutdownPhase::Blocked).await?;
        lifecycle
            .request(RuntimeRequest::Force {
                operation: blocked.id,
                expected_revision: blocked.revision,
            })
            .await?;
        let blocked = phase(&lifecycle, operation.id, ShutdownPhase::Blocked).await?;
        ensure!(
            blocked.force_requested && lifecycle.stopped().borrow().is_none(),
            "Failed preservation became destructive force"
        );
        drop(held);
        lifecycle
            .request(RuntimeRequest::Retry {
                operation: blocked.id,
                expected_revision: blocked.revision,
            })
            .await?;
        phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        Ok(())
    })
}

#[test]
fn replay_recovers_stopped_publication_without_reexecuting_the_operation() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let review = lifecycle.registration.admission().review(
            &lifecycle.owner,
            ShutdownOptions {
                strategy: StopStrategy::Interrupt,
                independent: IndependentTasks::Stop,
                quiescence_timeout_seconds: 2,
                destination: Default::default(),
            },
            Vec::new(),
        )?;
        let request = RequestId::new();
        let operation = lifecycle.registration.admission().begin(
            &lifecycle.owner,
            request,
            review.id,
            Vec::new(),
        )?;
        let completed = lifecycle.owner.complete(operation.id, operation.revision)?;
        // The durable boundary succeeded, but delivery of the exit signal did
        // not. Retry enters through the actual coordinator request method.
        ensure!(
            lifecycle.stopped().borrow().is_none(),
            "Fixture already signalled exit"
        );
        lifecycle
            .request(RuntimeRequest::Begin {
                request,
                review: review.id,
            })
            .await?;
        let mut stopped = lifecycle.stopped();
        tokio::time::timeout(Duration::from_secs(3), stopped.wait_for(|id| id.is_some())).await??;
        ensure!(
            lifecycle.owner.inspect(operation.id)? == completed,
            "Replay rewrote the original completed operation"
        );
        Ok(())
    })
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
async fn begin(
    lifecycle: &RuntimeLifecycle,
    strategy: StopStrategy,
    seconds: u32,
) -> Result<ShutdownOperation> {
    let RuntimeResponse::Review(review) = lifecycle
        .request(RuntimeRequest::Review {
            options: ShutdownOptions {
                strategy,
                independent: IndependentTasks::Stop,
                quiescence_timeout_seconds: seconds,
                destination: Default::default(),
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
    Ok(operation)
}
async fn phase(
    lifecycle: &RuntimeLifecycle,
    id: OperationId,
    phase: ShutdownPhase,
) -> Result<ShutdownOperation> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let RuntimeResponse::Operation(operation) = lifecycle
                .request(RuntimeRequest::Inspect { operation: id })
                .await?
            else {
                panic!("operation");
            };
            if operation.phase == phase {
                return Ok::<_, anyhow::Error>(operation);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Runtime did not reach the expected phase")?
}

#[test]
fn idle_shutdown_is_runtime_driven_and_not_just_begin_acknowledgement() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let operation = begin(&lifecycle, StopStrategy::FinishCurrent, 3).await?;
        ensure!(
            operation.phase == ShutdownPhase::WaitingForCurrent,
            "Begin pretended to be terminal"
        );
        let complete = phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        ensure!(
            *lifecycle.stopped().borrow()
                == Some(RuntimeExit {
                    operation: complete.id,
                    forced: false,
                    restart: false
                }),
            "No runtime-owned exit signal"
        );
        ensure!(
            lifecycle
                .request(RuntimeRequest::CancelWait {
                    operation: complete.id,
                    expected_revision: complete.revision
                })
                .await
                .is_err(),
            "Cancel crossed the stopping boundary"
        );
        ensure!(
            lifecycle.owner.status()?.desired_stopped,
            "Shutdown did not retain desired-stop"
        );
        Ok(())
    })
}

#[test]
fn finish_has_no_deadline_cancel_restores_admission_and_next_operation_is_not_lost() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let gate = lifecycle.registration.admission();
        let held = gate.independent(RuntimeWorkKind::Preparation, "first".into(), None)?;
        let operation = begin(&lifecycle, StopStrategy::FinishCurrent, 1).await?;
        ensure!(
            lifecycle
                .request(RuntimeRequest::Force {
                    operation: operation.id,
                    expected_revision: operation.revision
                })
                .await
                .is_err(),
            "Force implicitly changed Finish intent"
        );
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let current = phase(&lifecycle, operation.id, ShutdownPhase::WaitingForCurrent).await?;
        ensure!(
            current.revision == operation.revision && !current.force_requested,
            "Stationary work churned revisions or escalated"
        );
        ensure!(
            gate.independent(RuntimeWorkKind::Preparation, "not-admitted".into(), None)
                .is_err(),
            "New independent work entered drain"
        );
        lifecycle
            .request(RuntimeRequest::CancelWait {
                operation: current.id,
                expected_revision: current.revision,
            })
            .await?;
        let second = gate.independent(RuntimeWorkKind::Preparation, "second".into(), None)?;
        drop(held);
        drop(second);
        let operation = begin(&lifecycle, StopStrategy::FinishCurrent, 3).await?;
        phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        Ok(())
    })
}

#[test]
fn incomplete_owner_blocks_timeout_without_force_and_explicit_retry_verifies_it() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let held = lifecycle.registration.admission().independent(
            RuntimeWorkKind::Preparation,
            "retained-control".into(),
            None,
        )?;
        let operation = begin(&lifecycle, StopStrategy::Interrupt, 1).await?;
        let blocked = phase(&lifecycle, operation.id, ShutdownPhase::Blocked).await?;
        ensure!(
            !blocked.force_requested && !blocked.issues.is_empty() && blocked.remaining.len() == 1,
            "Timeout lost actual outstanding work"
        );
        ensure!(
            lifecycle.stopped().borrow().is_none(),
            "Timeout signalled daemon exit"
        );
        drop(held);
        lifecycle
            .request(RuntimeRequest::Retry {
                operation: blocked.id,
                expected_revision: blocked.revision,
            })
            .await?;
        phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        Ok(())
    })
}

#[test]
fn legacy_background_owner_is_joined_and_its_partial_output_survives_interrupt() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let (ready, entered) = tokio::sync::oneshot::channel();
        let task = lifecycle
            .background
            .spawn_with_notify(
                "fixture",
                None,
                "fixture-session",
                false,
                false,
                move |output| async move {
                    tokio::fs::write(&output, b"retained original bytes").await?;
                    let _ = ready.send(());
                    std::future::pending::<Result<crate::background::TaskResult>>().await
                },
            )
            .await;
        entered.await?;
        let operation = begin(&lifecycle, StopStrategy::Interrupt, 5).await?;
        phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        ensure!(
            lifecycle
                .background
                .runtime_owned_work(lifecycle.owner.identity())
                .await
                .is_empty(),
            "Legacy owner was abandoned"
        );
        ensure!(
            tokio::fs::read(lifecycle.background.output_path_for(&task.task_id)).await?
                == b"retained original bytes",
            "Legacy output was lost"
        );
        Ok(())
    })
}

#[test]
fn verified_restart_publishes_a_replacement_exit_without_intentional_stop() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let RuntimeResponse::Review(review) = lifecycle
            .request(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 3,
                    destination: RuntimeDestination::Restart,
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
        let stopped = phase(&lifecycle, operation.id, ShutdownPhase::Stopped).await?;
        let mut exit = lifecycle.stopped();
        tokio::time::timeout(Duration::from_secs(3), exit.wait_for(|exit| exit.is_some()))
            .await??;
        ensure!(
            *exit.borrow()
                == Some(RuntimeExit {
                    operation: stopped.id,
                    forced: false,
                    restart: true
                }),
            "Restart did not publish a replacement exit"
        );
        ensure!(
            !lifecycle.owner.status()?.desired_stopped,
            "A restart recorded an intentional Stop"
        );
        Ok(())
    })
}

#[test]
fn external_signal_quiesces_without_desired_stop_and_reviewed_control_cannot_replace_it()
-> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        lifecycle.begin_external_signal().await?;
        ensure!(
            lifecycle.begin_external_signal().await.is_err(),
            "A second signal opened a second transition"
        );
        let mut exit = lifecycle.stopped();
        tokio::time::timeout(Duration::from_secs(5), exit.wait_for(|exit| exit.is_some()))
            .await??;
        let published = (*exit.borrow()).expect("exit");
        ensure!(
            !published.forced && !published.restart,
            "Unexpected exit kind"
        );
        let operation = lifecycle.owner.inspect(published.operation)?;
        ensure!(
            operation.origin == ShutdownOrigin::ExternalSignal
                && operation.review.options.independent == IndependentTasks::KeepSupported
                && operation.review.options.strategy == StopStrategy::Interrupt,
            "Signal did not use Interrupt with supported-task survival"
        );
        ensure!(
            !lifecycle.owner.status()?.desired_stopped,
            "A signal recorded an intentional Stop; login would not restart the runtime"
        );
        Ok(())
    })
}

#[test]
fn supervision_status_and_recovery_decisions_use_the_live_coordinator() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let lifecycle = fixture(sandbox.root()).await?;
        let RuntimeResponse::Supervision(status) =
            lifecycle.request(RuntimeRequest::Supervision {}).await?
        else {
            panic!("supervision");
        };
        ensure!(
            status.recoveries.is_empty() && !status.runtime.is_empty(),
            "{status:?}"
        );
        let item = lifecycle
            .owner
            .recovery()
            .adopt(
                &[crate::runtime_lifecycle::turns::TurnRecord {
                    schema: 1,
                    session: "session_unpublished".into(),
                    turn: "t".into(),
                    runtime: "previous".into(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                }],
                |_| RecoveryCause::UnexpectedExit,
            )?
            .remove(0);
        // Continue validates that the session can actually run; nothing is
        // recorded when it cannot.
        ensure!(
            lifecycle
                .request(RuntimeRequest::Recover {
                    item: item.id,
                    expected_revision: item.revision,
                    request: RequestId::new(),
                    decision: RecoveryDecision::Continue,
                })
                .await
                .is_err(),
            "Continue accepted an unpublished session"
        );
        let request = RequestId::new();
        let leave = RuntimeRequest::Recover {
            item: item.id,
            expected_revision: item.revision,
            request,
            decision: RecoveryDecision::LeaveStopped,
        };
        let first = lifecycle.request(leave.clone()).await?;
        ensure!(
            leave.matches_response(&first),
            "Uncorrelated recovery reply"
        );
        ensure!(
            lifecycle.request(leave).await? == first,
            "Same request did not replay"
        );
        let RuntimeResponse::Supervision(status) =
            lifecycle.request(RuntimeRequest::Supervision {}).await?
        else {
            panic!("supervision");
        };
        ensure!(
            status.recoveries.len() == 1 && status.recoveries[0].resolved.is_some(),
            "Resolved history missing"
        );
        Ok(())
    })
}
