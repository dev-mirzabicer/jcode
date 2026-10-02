use super::*;
use std::sync::{Arc, Barrier};

#[test]
#[cfg(unix)]
fn dangling_control_directory_is_not_an_empty_stopped_runtime() -> Result<()> {
    let (root, store) = fixture()?;
    std::fs::create_dir_all(store.directory.parent().unwrap())?;
    std::os::unix::fs::symlink(root.path().join("missing"), &store.directory)?;
    assert!(store.status().is_err());
    assert!(store.owner_is_live().is_err());
    assert!(store.require_automatic_start().is_err());
    assert!(
        store
            .accepted_request(RequestId::new(), ReviewId::new())
            .is_err()
    );
    assert!(!root.path().join("missing").exists());
    let other = tempfile::tempdir()?;
    let parent_alias = RuntimeStopStore::new(other.path(), &other.path().join("runtime.sock"))?;
    std::os::unix::fs::symlink(
        other.path().join("missing"),
        parent_alias.directory.parent().unwrap(),
    )?;
    assert!(parent_alias.status().is_err());
    assert!(parent_alias.owner_is_live().is_err());
    Ok(())
}

#[test]
fn offline_inspection_preserves_receipts_and_reports_actual_ownership() -> Result<()> {
    let (_root, store) = fixture()?;
    assert!(!store.owner_is_live()?);
    let request = RequestId::new();
    let owner = store.claim()?;
    assert!(store.owner_is_live()?);
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let operation = owner.begin(request, review.id, Vec::new())?;
    let complete = owner.complete(operation.id, operation.revision)?;
    let bytes = std::fs::read(store.directory.join("journal.json"))?;
    assert_eq!(
        store.accepted_request(request, review.id)?,
        Some(complete.clone())
    );
    assert!(store.accepted_request(request, ReviewId::new()).is_err());
    drop(owner);
    assert!(!store.owner_is_live()?);
    assert_eq!(store.inspect(operation.id)?, complete);
    assert_eq!(std::fs::read(store.directory.join("journal.json"))?, bytes);
    assert_eq!(store.status()?.namespace, store.namespace());
    Ok(())
}

#[test]
fn reviewed_replacement_preserves_history_and_never_reopens_cancel_after_stop() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let original = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        vec![work("held", false)],
    )?;
    let first = owner.begin(RequestId::new(), original.id, vec![work("held", false)])?;
    let changed = owner.review_change(
        first.id,
        first.revision,
        options(StopStrategy::Interrupt, IndependentTasks::KeepSupported),
        vec![work("held", false)],
    )?;
    let request = RequestId::new();
    let second = owner.begin(request, changed.id, vec![work("held", false)])?;
    assert_eq!(owner.inspect(first.id)?.phase, ShutdownPhase::Superseded);
    assert_eq!(owner.inspect(first.id)?.review, original);
    assert_eq!(owner.begin(request, changed.id, Vec::new())?, second);
    assert!(owner.cancel_wait(first.id, first.revision).is_err());
    let blocked = owner.block(
        second.id,
        second.revision,
        vec!["fixture still held".into()],
    )?;
    let finish = owner.review_change(
        second.id,
        blocked.revision,
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        vec![work("held", false)],
    )?;
    let third = owner.begin(RequestId::new(), finish.id, vec![work("held", false)])?;
    assert!(third.cancellation_closed);
    assert!(owner.cancel_wait(third.id, third.revision).is_err());
    assert!(
        owner
            .update(second.id, blocked.revision, |_| Ok(()))
            .is_err()
    );
    Ok(())
}

#[test]
fn changed_revision_refuses_replacement_and_force_retains_uncertainty() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        vec![work("held", false)],
    )?;
    let first = owner.begin(RequestId::new(), review.id, vec![work("held", false)])?;
    let stale = owner.review_change(
        first.id,
        first.revision,
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        vec![work("held", false)],
    )?;
    let stopping = owner.enter_stopping(first.id, first.revision)?;
    assert!(
        owner
            .begin(RequestId::new(), stale.id, vec![work("held", false)])
            .is_err()
    );
    let blocked = owner.block(
        first.id,
        stopping.revision,
        vec!["checkpoint unavailable".into()],
    )?;
    assert!(owner.force_exit(first.id, blocked.revision).is_err());
    let retry = owner.retry(first.id, blocked.revision, true)?;
    let blocked = owner.block(
        first.id,
        retry.revision,
        vec!["checkpoint still unavailable".into()],
    )?;
    let forced = owner.force_exit(first.id, blocked.revision)?;
    assert_eq!(forced.phase, ShutdownPhase::Forced);
    assert_eq!(forced.remaining, vec![work("held", false)]);
    assert!(!forced.issues.is_empty());
    assert!(owner.confirm_stopped(forced.id).is_err());
    owner.confirm_forced(forced.id)?;
    assert!(store.require_automatic_start().is_err());
    drop(owner);
    store.authorize_start()?;
    assert_eq!(store.claim()?.inspect(forced.id)?, forced);
    Ok(())
}

#[test]
fn stopped_confirmation_is_same_owner_and_does_not_rewrite_history() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let operation = owner.begin(RequestId::new(), review.id, Vec::new())?;
    assert!(owner.confirm_stopped(operation.id).is_err());
    let completed = owner.complete(operation.id, operation.revision)?;
    let bytes = std::fs::read(store.directory.join("journal.json"))?;
    owner.confirm_stopped(operation.id)?;
    owner.confirm_stopped(operation.id)?;
    assert_eq!(owner.inspect(operation.id)?, completed);
    assert_eq!(std::fs::read(store.directory.join("journal.json"))?, bytes);
    drop(owner);
    store.authorize_start()?;
    let replacement = store.claim()?;
    assert!(replacement.confirm_stopped(operation.id).is_err());
    Ok(())
}

fn fixture() -> Result<(tempfile::TempDir, RuntimeStopStore)> {
    let root = tempfile::tempdir()?;
    let store = RuntimeStopStore::new(root.path(), &root.path().join("runtime.sock"))?;
    Ok((root, store))
}
fn options(strategy: StopStrategy, independent: IndependentTasks) -> ShutdownOptions {
    ShutdownOptions {
        strategy,
        independent,
        quiescence_timeout_seconds: 30,
        destination: Default::default(),
    }
}
fn work(id: &str, survivor: bool) -> RuntimeWork {
    RuntimeWork {
        id: id.into(),
        owner: "original-owner".into(),
        session: Some("session".into()),
        kind: RuntimeWorkKind::Execution,
        supported_survivor: survivor,
    }
}

#[test]
fn reviewed_intent_is_durable_idempotent_and_blocks_automatic_start() -> Result<()> {
    let (_root, store) = fixture()?;
    assert!(!store.status()?.desired_stopped);
    assert!(
        !store.directory.exists(),
        "Read-only absence is not initialization"
    );
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        vec![work("one", false)],
    )?;
    let request = RequestId::new();
    let op = owner.begin(request, review.id, vec![work("one", false)])?;
    assert_eq!(op.phase, ShutdownPhase::WaitingForCurrent);
    assert_eq!(owner.begin(request, review.id, Vec::new())?, op);
    assert!(owner.begin(request, ReviewId::new(), Vec::new()).is_err());
    assert_eq!(store.status()?.operation, Some(op.clone()));
    assert!(store.require_automatic_start().is_err());
    assert!(
        store.authorize_start().is_err(),
        "Start cannot cancel another live owner"
    );
    let cancelled = owner.cancel_wait(op.id, op.revision)?;
    assert_eq!(owner.cancel_wait(op.id, op.revision)?, cancelled);
    assert!(!store.status()?.desired_stopped);
    assert!(
        owner
            .begin(RequestId::new(), review.id, Vec::new())
            .is_err(),
        "A consumed review cannot authorize another shutdown"
    );
    Ok(())
}

#[test]
fn stale_work_and_runtime_review_do_not_acquire_authority() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        vec![work("old", false)],
    )?;
    assert!(
        owner
            .begin(RequestId::new(), review.id, vec![work("new", false)])
            .is_err()
    );
    let mut changed = work("old", false);
    changed.owner = "replacement".into();
    assert!(
        owner
            .begin(RequestId::new(), review.id, vec![changed])
            .is_err()
    );
    assert!(!store.status()?.desired_stopped);
    drop(owner);
    let owner = store.claim()?;
    assert!(
        owner
            .begin(RequestId::new(), review.id, Vec::new())
            .is_err()
    );
    Ok(())
}

#[test]
fn completed_review_work_can_drop_out_but_cancel_ends_at_stopping() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        vec![work("done", false)],
    )?;
    let op = owner.begin(RequestId::new(), review.id, Vec::new())?;
    assert!(owner.retry(op.id, op.revision, true).is_err());
    let op = owner.enter_stopping(op.id, op.revision)?;
    assert!(owner.cancel_wait(op.id, op.revision).is_err());
    let op = owner.complete(op.id, op.revision)?;
    assert_eq!(op.phase, ShutdownPhase::Stopped);
    assert!(store.require_automatic_start().is_err());
    drop(owner);
    assert!(store.claim().is_err());
    assert!(!store.authorize_start()?.desired_stopped);
    store.claim()?;
    Ok(())
}

#[test]
fn blocked_and_survivor_receipts_never_manufacture_quiescence() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let task = work("native", true);
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::KeepSupported),
        vec![task.clone()],
    )?;
    let op = owner.begin(RequestId::new(), review.id, vec![task.clone()])?;
    assert!(owner.complete(op.id, op.revision).is_err());
    assert!(
        owner
            .observe(
                op.id,
                op.revision,
                Vec::new(),
                vec![work("mcp", false)],
                Vec::new()
            )
            .is_err()
    );
    let op = owner.block(op.id, op.revision, vec!["synthetic handoff failure".into()])?;
    assert!(owner.cancel_wait(op.id, op.revision).is_err());
    assert!(owner.complete(op.id, op.revision).is_err());
    assert!(!op.force_requested);
    let op = owner.retry(op.id, op.revision, true)?;
    assert!(op.force_requested);
    let op = owner.observe(
        op.id,
        op.revision,
        Vec::new(),
        vec![task.clone()],
        Vec::new(),
    )?;
    let op = owner.complete(op.id, op.revision)?;
    assert_eq!(op.preserved, vec![task]);
    assert_eq!(
        op.review.work, op.preserved,
        "Original review remains immutable"
    );
    Ok(())
}

#[test]
fn stop_all_and_duplicate_work_cannot_claim_preservation() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    assert!(
        owner
            .review(
                options(StopStrategy::Interrupt, IndependentTasks::Stop),
                vec![work("dup", false), work("dup", false)]
            )
            .is_err()
    );
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let op = owner.begin(RequestId::new(), review.id, Vec::new())?;
    assert!(
        owner
            .observe(
                op.id,
                op.revision,
                Vec::new(),
                vec![work("native", true)],
                Vec::new()
            )
            .is_err()
    );
    assert_eq!(owner.inspect(op.id)?, op);
    Ok(())
}

#[test]
fn cancel_and_final_drain_are_one_revision_checked_transition() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = Arc::new(store.claim()?);
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let op = owner.begin(RequestId::new(), review.id, Vec::new())?;
    let barrier = Arc::new(Barrier::new(3));
    let cancel_owner = owner.clone();
    let cancel_barrier = barrier.clone();
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_owner.cancel_wait(op.id, op.revision)
    });
    let stopping_owner = owner.clone();
    let stopping_barrier = barrier.clone();
    let stopping = std::thread::spawn(move || {
        stopping_barrier.wait();
        stopping_owner.enter_stopping(op.id, op.revision)
    });
    barrier.wait();
    let cancelled = cancel.join().unwrap().is_ok();
    let stopped = stopping.join().unwrap().is_ok();
    assert_ne!(cancelled, stopped);
    assert_eq!(store.status()?.desired_stopped, stopped);
    Ok(())
}

#[test]
fn explicit_start_preserves_uncertain_receipt_without_replaying_old_review() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        vec![work("uncertain", false)],
    )?;
    let request = RequestId::new();
    let op = owner.begin(request, review.id, review.work.clone())?;
    drop(owner);
    assert!(store.claim().is_err());
    let status = store.authorize_start()?;
    let interrupted = status.operation.unwrap();
    assert_eq!(interrupted.phase, ShutdownPhase::Interrupted);
    assert_eq!(interrupted.remaining, op.remaining);
    assert!(!interrupted.issues.is_empty());
    let owner = store.claim()?;
    assert_eq!(owner.begin(request, review.id, Vec::new())?, interrupted);
    assert!(owner.retry(op.id, interrupted.revision, true).is_err());
    Ok(())
}

#[test]
fn missing_corrupt_unknown_and_replaced_storage_fail_closed() -> Result<()> {
    for damage in ["missing", "corrupt", "schema", "namespace", "marker"] {
        let (_root, store) = fixture()?;
        drop(store.claim()?);
        let path = store.directory.join("journal.json");
        match damage {
            "missing" => std::fs::remove_file(&path)?,
            "corrupt" => std::fs::write(&path, b"bad")?,
            "marker" => std::fs::write(store.directory.join("journal.lock"), b"bad")?,
            _ => {
                let mut data: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
                if damage == "schema" {
                    data["schema"] = 999.into();
                } else {
                    data["namespace"] = "different".into();
                }
                std::fs::write(&path, serde_json::to_vec(&data)?)?;
            }
        }
        assert!(store.status().is_err(), "{damage}");
        assert!(store.require_automatic_start().is_err(), "{damage}");
        assert!(store.authorize_start().is_err(), "{damage}");
    }
    Ok(())
}

#[test]
fn persistence_failure_does_not_change_last_committed_intent() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let op = owner.begin(RequestId::new(), review.id, Vec::new())?;
    let path = store.directory.join("journal.json");
    let before = std::fs::read(&path)?;
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    assert!(owner.cancel_wait(op.id, op.revision).is_err());
    std::fs::remove_dir(&path)?;
    std::fs::write(&path, before)?;
    assert_eq!(owner.inspect(op.id)?, op);
    assert!(store.status()?.desired_stopped);
    Ok(())
}

#[test]
fn owner_process_fixture() -> Result<()> {
    let Some(root) = std::env::var_os("JCODE_WP08_OWNER_FIXTURE") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let store = RuntimeStopStore::new(&root, &root.join("runtime.sock"))?;
    assert!(store.claim().is_err());
    assert!(store.authorize_start().is_err());
    Ok(())
}

#[test]
fn kernel_owner_excludes_independent_process_and_releases_on_drop() -> Result<()> {
    let (root, store) = fixture()?;
    let owner = store.claim()?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "runtime_lifecycle::tests::owner_process_fixture",
            "--nocapture",
        ])
        .env("JCODE_WP08_OWNER_FIXTURE", root.path())
        .status()?;
    assert!(status.success());
    drop(owner);
    store.claim()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_regular_control_files_reject_symlink_aliases() -> Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (root, store) = fixture()?;
    drop(store.claim()?);
    for entry in ["journal.lock", "owner.lock", "journal.json"] {
        let path = store.directory.join(entry);
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o077, 0);
        let saved = root.path().join(entry);
        std::fs::rename(&path, &saved)?;
        symlink(&saved, &path)?;
        assert!(store.claim().is_err());
        std::fs::remove_file(&path)?;
        std::fs::rename(&saved, &path)?;
    }
    Ok(())
}

fn restart_options() -> ShutdownOptions {
    ShutdownOptions {
        destination: RuntimeDestination::Restart,
        ..options(StopStrategy::Interrupt, IndependentTasks::Stop)
    }
}

/// Drive one operation of `owner` to a verified Stopped receipt.
fn stop_verified(owner: &RuntimeStopOwner, options: ShutdownOptions) -> Result<ShutdownOperation> {
    let review = owner.review(options, Vec::new())?;
    let begun = owner.begin(RequestId::new(), review.id, Vec::new())?;
    let stopped = owner.complete(begun.id, begun.revision)?;
    assert_eq!(stopped.phase, ShutdownPhase::Stopped);
    Ok(stopped)
}

#[test]
fn verified_restart_keeps_desired_running_and_hands_interrupted_turns_to_continuation() -> Result<()>
{
    let (_root, store) = fixture()?;
    let old = store.claim()?;
    let record = old.turns().begin("session_restart")?;
    let stopped = stop_verified(&old, restart_options())?;
    assert!(!stopped.records_intentional_stop());
    assert!(!store.status()?.desired_stopped, "restart is not a Stop");
    old.confirm_stopped(stopped.id)?;
    drop(old);
    store.require_automatic_start()?;

    let next = store.claim()?;
    let mut planned = Vec::new();
    let recovered = next.reconcile_interrupted_turns(|record, transition| {
        planned.push((record.clone(), transition));
        Ok(())
    })?;
    assert!(recovered.is_empty(), "a verified restart is not a crash");
    assert_eq!(
        planned,
        vec![(
            record,
            PlannedTransition::Restart {
                operation: stopped.id
            }
        )]
    );
    assert!(
        next.turns().leftovers()?.is_empty(),
        "classified records settle only after the continuation intent persisted"
    );
    Ok(())
}

#[test]
fn reload_handoff_receipt_classifies_only_its_own_incarnation() -> Result<()> {
    let (_root, store) = fixture()?;
    let old = store.claim()?;
    old.turns().begin("session_reloaded")?;
    old.record_reload_handoff("reload-1")?;
    drop(old);
    let middle = store.claim()?;
    // A turn of the replacement that then crashes is not covered by the old receipt.
    middle.turns().begin("session_crashed")?;
    let mut planned = Vec::new();
    middle.reconcile_interrupted_turns(|record, transition| {
        planned.push((record.session.clone(), transition));
        Ok(())
    })?;
    assert_eq!(
        planned,
        vec![(
            "session_reloaded".to_string(),
            PlannedTransition::Reload {
                reload: "reload-1".into()
            }
        )]
    );
    drop(middle);
    let last = store.claim()?;
    let items = last.reconcile_interrupted_turns(|_, _| {
        anyhow::bail!("no planned transition covers a crash")
    })?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].session, "session_crashed");
    assert_eq!(items[0].cause, RecoveryCause::UnexpectedExit);
    Ok(())
}

#[test]
fn failed_continuation_persistence_retains_the_turn_record_for_retry() -> Result<()> {
    let (_root, store) = fixture()?;
    let old = store.claim()?;
    old.turns().begin("session_retry")?;
    old.record_reload_handoff("reload-2")?;
    drop(old);
    let next = store.claim()?;
    assert!(
        next.reconcile_interrupted_turns(|_, _| anyhow::bail!("disk full"))
            .is_err()
    );
    assert_eq!(next.turns().leftovers()?.len(), 1, "evidence kept");
    let mut retried = 0;
    next.reconcile_interrupted_turns(|_, _| {
        retried += 1;
        Ok(())
    })?;
    assert_eq!(retried, 1);
    assert!(next.turns().leftovers()?.is_empty());
    Ok(())
}

#[test]
fn external_signal_and_forced_exits_become_selectable_recovery() -> Result<()> {
    let (_root, store) = fixture()?;
    let signalled = store.claim()?;
    signalled.turns().begin("session_signal")?;
    let review = signalled.review(
        ShutdownOptions {
            independent: IndependentTasks::KeepSupported,
            ..options(StopStrategy::Interrupt, IndependentTasks::Stop)
        },
        Vec::new(),
    )?;
    let operation = signalled.begin_external_signal(RequestId::new(), review.id, Vec::new())?;
    assert_eq!(operation.origin, ShutdownOrigin::ExternalSignal);
    assert!(
        !store.status()?.desired_stopped,
        "termination by the system is not an intentional Stop"
    );
    let stopped = signalled.complete(operation.id, operation.revision)?;
    signalled.confirm_stopped(stopped.id)?;
    drop(signalled);
    store.require_automatic_start()?;

    let next = store.claim()?;
    let items = next.reconcile_interrupted_turns(|_, _| {
        anyhow::bail!("a signal is not a planned transition")
    })?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].cause, RecoveryCause::ExternalSignal);
    // Startup classification is idempotent across a crash before settlement.
    assert!(
        next.reconcile_interrupted_turns(|_, _| unreachable!())?
            .is_empty()
    );
    assert_eq!(next.recovery().list()?.len(), 1);
    assert_eq!(store.recoveries()?.len(), 1, "offline inspection sees it");
    Ok(())
}

#[test]
fn reviewed_change_cannot_rewrite_an_external_signal_exit() -> Result<()> {
    let (_root, store) = fixture()?;
    let owner = store.claim()?;
    let review = owner.review(
        options(StopStrategy::FinishCurrent, IndependentTasks::Stop),
        Vec::new(),
    )?;
    assert!(
        owner
            .begin_external_signal(RequestId::new(), review.id, Vec::new())
            .is_err(),
        "an external exit is a fresh interrupting stop"
    );
    let review = owner.review(
        options(StopStrategy::Interrupt, IndependentTasks::Stop),
        Vec::new(),
    )?;
    let operation = owner.begin_external_signal(RequestId::new(), review.id, Vec::new())?;
    let blocked = owner.block(operation.id, operation.revision, vec!["owner busy".into()])?;
    assert!(
        owner
            .review_change(
                blocked.id,
                blocked.revision,
                options(StopStrategy::Interrupt, IndependentTasks::Stop),
                Vec::new()
            )
            .is_err()
    );
    Ok(())
}
