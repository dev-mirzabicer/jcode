use super::*;
use crate::runtime_lifecycle::RuntimeStopStore;

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
