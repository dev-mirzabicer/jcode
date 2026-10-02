use super::*;
use crate::runtime_lifecycle::RuntimeStopStore;

#[test]
fn reload_and_stop_cannot_both_acquire_the_runtime_transition() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission();
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    let reload = gate.reserve_reload()?;
    assert!(
        gate.begin(&owner, RequestId::new(), review.id, Vec::new())
            .is_err()
    );
    assert!(!owner.status()?.desired_stopped);
    assert!(gate.is_reloading());
    assert!(gate.reserve_reload().is_err());
    drop(reload);
    assert!(gate.accepts_input());
    let barrier = std::sync::Barrier::new(2);
    let (stop, reload) = std::thread::scope(|threads| {
        let stop = threads.spawn(|| {
            barrier.wait();
            gate.begin(&owner, RequestId::new(), review.id, Vec::new())
        });
        let reload = threads.spawn(|| {
            barrier.wait();
            gate.reserve_reload()
        });
        (stop.join().unwrap(), reload.join().unwrap())
    });
    assert_ne!(stop.is_ok(), reload.is_ok());
    if let Ok(operation) = stop {
        assert!(gate.reserve_reload().is_err());
        gate.cancel_wait(&owner, operation.id, operation.revision)?;
    }
    drop(reload);
    assert!(gate.accepts_input());
    Ok(())
}

#[test]
fn final_stop_serializes_with_control_and_seals_later_writes() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission().clone();
    let review = gate.review(&owner, options(StopStrategy::Interrupt), Vec::new())?;
    let operation = gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    std::thread::scope(|threads| -> Result<()> {
        let control_gate = gate.clone();
        let control = threads.spawn(move || {
            control_gate.control_boundary(|| {
                entered.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            })
        });
        ready.recv()?;
        assert!(gate.state.try_lock().is_err());
        let complete = threads.spawn(|| gate.complete(&owner, operation.id, operation.revision));
        assert_eq!(owner.inspect(operation.id)?.phase, ShutdownPhase::Stopping);
        release.send(())?;
        control.join().unwrap()?;
        assert_eq!(complete.join().unwrap()?.phase, ShutdownPhase::Stopped);
        Ok(())
    })?;
    let called = std::sync::atomic::AtomicBool::new(false);
    assert!(
        gate.control_boundary(|| called.store(true, std::sync::atomic::Ordering::SeqCst))
            .is_err()
    );
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    gate.confirm_stopped(&owner, operation.id)?;
    Ok(())
}

#[tokio::test]
async fn interrupted_async_preparation_retains_its_actual_blocking_descendant() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission();
    let permit = gate.independent(
        RuntimeWorkKind::Preparation,
        "blocking-preparation".into(),
        None,
    )?;
    let (ready, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let task = tokio::spawn(prepare(Some(permit), async move {
        spawn_blocking(move || {
            let _ = ready.send(());
            wait.recv_timeout(std::time::Duration::from_secs(5))
        })
        .await??;
        Ok(())
    }));
    entered.await?;
    let review = gate.review(&owner, options(StopStrategy::Interrupt), Vec::new())?;
    gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    gate.interrupt_preparations()?;
    assert!(task.await?.is_err());
    assert_eq!(
        gate.work()?.len(),
        1,
        "Dropping a waiter is not blocking-work quiescence"
    );
    release.send(())?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !gate.work()?.is_empty() {
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn finish_allows_only_the_admitted_preparation_to_publish_and_propagate_blocking_scope()
-> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission().clone();
    let permit = gate.independent(RuntimeWorkKind::Preparation, "admitted".into(), None)?;
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    assert!(!gate.permits_current_work());
    assert!(gate.interrupt_preparations().is_err());
    let worker_gate = gate.clone();
    scope(Some(permit), async move {
        assert!(worker_gate.permits_current_work());
        spawn_blocking(move || {
            assert!(worker_gate.permits_current_work());
            worker_gate
                .causal(RuntimeWorkKind::Preparation, "nested".into(), None)
                .map(drop)
        })
        .await?
    })
    .await?;
    assert!(gate.work()?.is_empty());
    Ok(())
}

fn fixture() -> Result<(tempfile::TempDir, RuntimeStopOwner, RuntimeRegistration)> {
    let root = tempfile::tempdir()?;
    let owner = RuntimeStopStore::new(root.path(), &root.path().join("fixture.sock"))?.claim()?;
    let registration = RuntimeAdmission::register(root.path(), owner.identity())?;
    Ok((root, owner, registration))
}
fn options(strategy: StopStrategy) -> ShutdownOptions {
    ShutdownOptions {
        strategy,
        independent: IndependentTasks::Stop,
        quiescence_timeout_seconds: 5,
        destination: Default::default(),
    }
}

#[tokio::test]
async fn draining_allows_only_real_causal_scopes_not_session_spoofing() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission();
    let primary = gate.independent(
        RuntimeWorkKind::PrimaryTurn,
        "primary".into(),
        Some("session".into()),
    )?;
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    let op = gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    assert!(!gate.accepts_input());
    assert!(
        gate.independent(
            RuntimeWorkKind::PrimaryTurn,
            "new".into(),
            Some("session".into())
        )
        .is_err()
    );
    assert!(
        gate.causal(
            RuntimeWorkKind::Execution,
            "forged".into(),
            Some("session".into())
        )
        .is_err()
    );
    scope(Some(primary.clone()), async {
        let child = gate.causal(
            RuntimeWorkKind::Execution,
            "child".into(),
            Some("other".into()),
        )?;
        assert!(gate.enter_stopping(&owner, op.id, op.revision).is_err());
        scope(Some(child), async {
            let nested = gate.causal(RuntimeWorkKind::Execution, "nested".into(), None)?;
            assert_eq!(gate.work()?.len(), 3);
            drop(nested);
            Ok::<_, anyhow::Error>(())
        })
        .await
    })
    .await?;
    assert_eq!(gate.work()?.len(), 1);
    drop(primary);
    let op = gate.enter_stopping(&owner, op.id, op.revision)?;
    assert_eq!(op.phase, ShutdownPhase::Stopping);
    assert!(gate.cancel_wait(&owner, op.id, op.revision).is_err());
    Ok(())
}

#[tokio::test]
async fn explicit_task_scope_propagation_retains_owner_until_last_waiter_drops() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission().clone();
    let permit = gate.independent(RuntimeWorkKind::PrimaryTurn, "primary".into(), None)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task_gate = gate.clone();
    let task = tokio::spawn(scope(Some(permit), async move {
        rx.await?;
        let child = task_gate.causal(RuntimeWorkKind::Execution, "real-child".into(), None)?;
        drop(child);
        Ok::<_, anyhow::Error>(())
    }));
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    tx.send(()).unwrap();
    task.await??;
    assert!(gate.work()?.is_empty());
    Ok(())
}

#[test]
fn input_append_and_shutdown_begin_share_one_atomic_boundary() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission().clone();
    let owner = Arc::new(owner);
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let input_gate = gate.clone();
    let input = std::thread::spawn(move || {
        input_gate.input_boundary(|| {
            entered.send(()).unwrap();
            wait_release.recv().unwrap();
            42
        })
    });
    wait_entered.recv()?;
    let stopping_gate = gate.clone();
    let stopping_owner = owner.clone();
    let stopping = std::thread::spawn(move || {
        stopping_gate.begin(&stopping_owner, RequestId::new(), review.id, Vec::new())
    });
    release.send(())?;
    assert_eq!(input.join().unwrap(), Some(42));
    let op = stopping.join().unwrap()?;
    assert!(
        gate.input_boundary(|| panic!("Fenced input appended"))
            .is_none()
    );
    gate.cancel_wait(&owner, op.id, op.revision)?;
    assert_eq!(gate.input_boundary(|| 43), Some(43));
    Ok(())
}

#[tokio::test]
async fn interrupt_and_namespace_boundaries_reject_causal_work() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let (_other_root, _other_owner, other) = fixture()?;
    let gate = registration.admission();
    let permit = gate.independent(RuntimeWorkKind::PrimaryTurn, "primary".into(), None)?;
    let foreign =
        other
            .admission()
            .independent(RuntimeWorkKind::PrimaryTurn, "foreign".into(), None)?;
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    let op = gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    scope(Some(foreign), async {
        assert!(
            gate.causal(RuntimeWorkKind::Execution, "foreign-child".into(), None)
                .is_err()
        );
    })
    .await;
    gate.cancel_wait(&owner, op.id, op.revision)?;
    let review = gate.review(&owner, options(StopStrategy::Interrupt), Vec::new())?;
    gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    scope(Some(permit), async {
        assert!(
            gate.causal(RuntimeWorkKind::Execution, "later-effect".into(), None)
                .is_err()
        );
    })
    .await;
    Ok(())
}

#[test]
fn stale_review_keeps_running_but_storage_uncertainty_fences_admission() -> Result<()> {
    let (_root, owner, registration) = fixture()?;
    let gate = registration.admission();
    let review = gate.review(&owner, options(StopStrategy::FinishCurrent), Vec::new())?;
    let permit = gate.independent(RuntimeWorkKind::Preparation, "new".into(), None)?;
    assert!(
        gate.begin(&owner, RequestId::new(), review.id, Vec::new())
            .is_err()
    );
    assert!(gate.accepts_input());
    std::fs::write(owner.store.directory.join("journal.json"), b"corrupt")?;
    assert!(
        gate.begin(&owner, RequestId::new(), review.id, Vec::new())
            .is_err()
    );
    assert!(!gate.accepts_input());
    drop(permit);
    Ok(())
}

#[test]
fn unexpected_registration_loss_does_not_unfence_still_owned_work() -> Result<()> {
    let (root, _owner, registration) = fixture()?;
    let permit =
        registration
            .admission()
            .independent(RuntimeWorkKind::Execution, "live".into(), None)?;
    drop(registration);
    let gate = RuntimeAdmission::for_root(root.path())?.unwrap();
    assert!(!gate.accepts_input());
    assert!(
        gate.causal(RuntimeWorkKind::Execution, "new".into(), None)
            .is_err()
    );
    drop(permit);
    Ok(())
}
