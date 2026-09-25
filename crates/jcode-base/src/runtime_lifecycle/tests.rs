use super::*;
use std::sync::{Arc, Barrier};

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
