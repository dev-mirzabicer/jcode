use super::*;
use crate::bus::{BackgroundTaskProgressKind, BackgroundTaskProgressSource, BusEvent};
use anyhow::anyhow;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

#[test]
fn managed_snapshot_and_reload_records_survive_manager_recreation_without_body_reads() -> Result<()>
{
    use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, RunState};
    use jcode_tool_core::{OutputCapture, OutputStream};
    let _lock = crate::storage::lock_test_env();
    let root = tempdir()?;
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(old) = self.0.take() {
                crate::env::set_var("JCODE_HOME", old)
            } else {
                crate::env::remove_var("JCODE_HOME")
            }
        }
    }
    let _restore = Restore(std::env::var_os("JCODE_HOME"));
    crate::env::set_var("JCODE_HOME", root.path());
    let store = ExecutionStore::open(root.path())?;
    let invocation = Invocation {
        session_id: "snapshot-session".into(),
        message_id: "message".into(),
        call_path: vec!["call".into()],
        tool: "snapshot-fixture".into(),
        input: serde_json::json!({}),
        working_dir: None,
        received_result_digest: None,
    };
    let PreparedInvocation::New(record) = store.prepare(&invocation, "fixture")? else {
        panic!()
    };
    store.start(&record.id, "fixture")?;
    store.promote(&record.id, "fixture")?;
    store.register_background_delivery(&record.id, false, false)?;
    let capture = Capture::create(store.clone(), record.clone(), Default::default())?;
    capture.write(OutputStream::Text, b"retained source")?;
    let path = capture.reference()?.path;
    let moved = path.with_extension("offline");
    std::fs::rename(&path, &moved)?;
    let manager = BackgroundTaskManager::with_output_dir(root.path().join("empty-legacy"));
    let summary = manager.running_snapshot_including_managed(root.path())?;
    assert_eq!(summary.count, 1);
    assert_eq!(summary.labels, ["snapshot-fixture"]);
    let records = manager.persisted_background_tasks_for_session("snapshot-session")?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id, record.id);
    assert!(
        manager
            .persisted_background_tasks_for_session("other")?
            .is_empty()
    );
    std::fs::rename(&moved, &path)?;
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference()?);
    capture.seal(output, RunState::Completed)?;
    assert_eq!(
        manager
            .running_snapshot_including_managed(root.path())?
            .count,
        0
    );
    assert!(
        manager
            .persisted_background_tasks_for_session("snapshot-session")?
            .is_empty()
    );
    Ok(())
}

struct DroppedSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DroppedSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

async fn adopted_inner_fixture(reload: bool) -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _lifetime = DroppedSignal(Some(dropped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
        Ok(jcode_tool_types::ToolOutput::new("unreachable"))
    });
    let cleanup = handle.abort_handle();
    started_rx.await?;
    let info = manager
        .adopt_with_options("synthetic", None, "adoption", false, false, handle)
        .await;
    if reload {
        manager.abort_live_tasks_for_reload().await?;
    } else {
        manager.cancel(&info.task_id).await?;
    }
    let actually_stopped = dropped_rx.try_recv().is_ok();
    // Even the red regression leaves no detached fixture behind.
    cleanup.abort();
    assert!(
        actually_stopped,
        "terminal cancellation must wait until the adopted inner future is dropped"
    );
    assert!(!manager.is_live_task(&info.task_id));
    Ok(())
}

#[tokio::test]
async fn adopted_inner_cancel_waits_for_actual_work_to_stop() -> Result<()> {
    for _ in 0..3 {
        adopted_inner_fixture(false).await?;
    }
    Ok(())
}

#[tokio::test]
async fn adopted_inner_reload_waits_for_actual_work_to_stop() -> Result<()> {
    for _ in 0..3 {
        adopted_inner_fixture(true).await?;
    }
    Ok(())
}

#[tokio::test]
async fn adopted_uninterruptible_work_remains_owned_after_stop_timeout() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let handle = tokio::task::spawn_blocking(move || {
        let _ = ready_tx.send(());
        let _ = release_rx.recv();
        Ok(jcode_tool_types::ToolOutput::new(
            "finished after explicit release",
        ))
    });
    ready_rx.await?;
    let info = manager
        .adopt_with_options("synthetic", None, "uninterruptible", false, false, handle)
        .await;
    let result = manager.cancel(&info.task_id).await;
    let still_owned = manager.is_live_task(&info.task_id);
    let status = manager.status(&info.task_id).await;
    // Release before assertions so even a failing test cannot strand a worker.
    release_tx.send(())?;
    assert!(
        result.is_err(),
        "stop acknowledgement is not proof of quiescence"
    );
    assert!(still_owned);
    assert_eq!(status.unwrap().status, BackgroundTaskStatus::Running);
    for _ in 0..200 {
        if !manager.is_live_task(&info.task_id) {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        manager.status(&info.task_id).await.unwrap().status,
        BackgroundTaskStatus::Completed
    );
    Ok(())
}

#[tokio::test]
async fn spawn_with_notify_emits_started_ui_activity() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut bus_rx = Bus::global().subscribe();

    let info = manager
        .spawn_with_notify(
            "bash",
            Some("checks".to_string()),
            "session-started",
            true,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(10)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for UI activity event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::UiActivity(activity) = event
            && activity.session_id.as_deref() == Some("session-started")
            && activity.message.contains(&info.task_id)
        {
            assert_eq!(activity.kind, crate::bus::UiActivityKind::Background);
            assert!(activity.message.contains("Background task started"));
            assert!(activity.message.contains("checks"));
            assert_eq!(
                activity.status_notice.as_deref(),
                Some("Background task started · checks")
            );
            return Ok(());
        }
    }

    Err(anyhow!(
        "started UI activity event for task {} not received",
        info.task_id
    ))
}

#[tokio::test]
async fn update_delivery_applies_to_running_task_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-test",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_millis(25)).await;
                tokio::fs::write(&output_path, "hello").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let updated = manager
        .update_delivery(&info.task_id, true, true)
        .await
        .map_err(|err| anyhow!("update delivery should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert!(updated.notify);
    assert!(updated.wake);

    for _ in 0..40 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            assert!(status.notify);
            assert!(status.wake);
            assert_eq!(status.status, BackgroundTaskStatus::Completed);
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!("background task did not complete in time"))
}

#[tokio::test]
async fn update_progress_persists_status_and_emits_bus_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(50)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(42.0),
        message: Some("Running checks".to_string()),
        current: Some(21),
        total: Some(50),
        unit: Some("tests".to_string()),
        eta_seconds: Some(8),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let mut bus_rx = Bus::global().subscribe();
    let updated = manager
        .update_progress(&info.task_id, progress.clone())
        .await
        .map_err(|err| anyhow!("update progress should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(updated.progress, Some(progress.clone().normalize()));

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for progress event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::BackgroundTaskProgress(event) = event
            && event.task_id == info.task_id
        {
            assert_eq!(event.session_id, "session-progress");
            assert_eq!(event.progress, progress.normalize());
            return Ok(());
        }
    }

    Err(anyhow!(
        "progress event for task {} not received",
        info.task_id
    ))
}

#[tokio::test]
async fn wait_returns_when_task_finishes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-finish",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_millis(25)).await;
                tokio::fs::write(&output_path, "done").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let wait_result = manager
        .wait(&info.task_id, None, true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Finished);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Completed);
    assert_eq!(wait_result.task.exit_code, Some(0));
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_progress_checkpoint() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(25.0),
        message: Some("checkpoint".to_string()),
        current: Some(1),
        total: Some(4),
        unit: Some("steps".to_string()),
        eta_seconds: Some(3),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let waiter = manager.wait(&info.task_id, Some(Duration::from_secs(2)), true);
    let updater = async {
        sleep(Duration::from_millis(25)).await;
        manager
            .update_progress(&info.task_id, progress.clone())
            .await
            .map_err(|err| anyhow!("progress update should succeed: {err}"))?
            .ok_or_else(|| anyhow!("task should exist"))?;
        Result::<()>::Ok(())
    };
    let (wait_result, updater_result) = tokio::join!(waiter, updater);
    updater_result?;
    let wait_result = wait_result.ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Progress);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    assert_eq!(wait_result.task.progress, Some(progress.normalize()));
    assert!(wait_result.progress_event.is_some());
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_timeout() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-timeout",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(250)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;

    let wait_result = manager
        .wait(&info.task_id, Some(Duration::from_millis(25)), true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Timeout);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    Ok(())
}

fn running_status_fixture(task_id: &str, session_id: &str) -> TaskStatusFile {
    TaskStatusFile {
        task_id: task_id.to_string(),
        tool_name: "swarm".to_string(),
        display_name: None,
        session_id: session_id.to_string(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        error: None,
        started_at: Utc::now().to_rfc3339(),
        completed_at: None,
        duration_secs: None,
        pid: None,
        #[cfg(unix)]
        process_identity: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: false,
        wake: false,
        progress: None,
        event_history: Vec::new(),
    }
}

async fn write_status_fixture(manager: &BackgroundTaskManager, status: &TaskStatusFile) {
    let path = manager.status_path_for(&status.task_id);
    let json = serde_json::to_string_pretty(status).expect("serialize status fixture");
    tokio::fs::write(&path, json).await.expect("write fixture");
}

#[tokio::test]
async fn tasks_map_prunes_entry_after_natural_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-prune",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(10)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;
    assert!(
        manager.is_live_task(&info.task_id),
        "task should be live right after spawn"
    );

    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running && !manager.is_live_task(&info.task_id) {
            // Pruned only after the status file was finalized, so the live
            // map never claims a task whose status file is already terminal.
            let (running_count, labels, _) = manager.running_snapshot();
            assert_eq!(running_count, 0, "snapshot should not count finished tasks");
            assert!(labels.is_empty());
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!(
        "task {} was not pruned from the live map after completion",
        info.task_id
    ))
}

#[tokio::test]
async fn reconcile_marks_orphan_from_reloaded_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Same PID, different instance token: exactly what an exec-based server
    // reload leaves behind.
    let mut orphan = running_status_fixture("orphan1aaaa", "session-orphan");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);

    let status = manager
        .status("orphan1aaaa")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("orphaned") && error.contains("reloaded"),
        "error should explain the reload orphaning, got: {error}"
    );
    assert!(status.completed_at.is_some());
    Ok(())
}

#[tokio::test]
async fn reconcile_marks_orphan_from_dead_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A child process that has already exited and been reaped gives us a PID
    // that is provably not running.
    let mut child = std::process::Command::new("true")
        .spawn()
        .map_err(|err| anyhow!("spawn child: {err}"))?;
    let dead_pid = child.id();
    child.wait().map_err(|err| anyhow!("wait child: {err}"))?;

    let mut orphan = running_status_fixture("orphan2bbbb", "session-orphan-dead");
    orphan.owner_pid = Some(dead_pid);
    orphan.owner_instance = Some("some-dead-instance".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);
    let status = manager
        .status("orphan2bbbb")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    Ok(())
}

#[tokio::test]
async fn reconcile_leaves_non_orphans_alone() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Owned by this exact process image: could still be bootstrapping.
    let mut own = running_status_fixture("keep1aaaa", "session-keep");
    own.owner_pid = Some(std::process::id());
    own.owner_instance = Some(model::process_instance_token().to_string());
    write_status_fixture(&manager, &own).await;

    // Legacy file without owner metadata: no safe liveness signal, leave it.
    let legacy = running_status_fixture("keep2bbbb", "session-keep");
    write_status_fixture(&manager, &legacy).await;

    // Owned by a live foreign process (PID 1 is always alive on Unix).
    let mut foreign = running_status_fixture("keep3cccc", "session-keep");
    foreign.owner_pid = Some(1);
    foreign.owner_instance = Some("init-instance".to_string());
    write_status_fixture(&manager, &foreign).await;

    // Detached with a live pid: reconciled by the detached path, not this one.
    let mut detached = running_status_fixture("keep4dddd", "session-keep");
    detached.detached = true;
    detached.pid = Some(std::process::id());
    write_status_fixture(&manager, &detached).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 0);

    for task_id in ["keep1aaaa", "keep2bbbb", "keep3cccc", "keep4dddd"] {
        let status = manager
            .status(task_id)
            .await
            .ok_or_else(|| anyhow!("status for {task_id} should exist"))?;
        assert_eq!(
            status.status,
            BackgroundTaskStatus::Running,
            "{task_id} should not be reconciled"
        );
    }
    Ok(())
}

#[tokio::test]
async fn status_read_self_heals_orphaned_task() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut orphan = running_status_fixture("orphan3cccc", "session-orphan-read");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    // A plain status read (used by bg status / bg wait) heals the phantom
    // without waiting for the startup sweep.
    let status = manager
        .status("orphan3cccc")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);

    // And wait() returns immediately instead of blocking to timeout.
    let wait_result = manager
        .wait("orphan3cccc", Some(Duration::from_secs(5)), false)
        .await
        .ok_or_else(|| anyhow!("wait should find the task"))?;
    assert_eq!(
        wait_result.reason,
        BackgroundTaskWaitReason::AlreadyFinished
    );
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_finalizes_running_tasks() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A long-running task that would otherwise vanish across an exec reload.
    let info = manager
        .spawn_with_notify(
            "selfdev-build",
            Some("selfdev build".to_string()),
            "session-reload-abort",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await;
    assert!(manager.is_live_task(&info.task_id));

    let aborted = manager.abort_live_tasks_for_reload().await?;
    assert_eq!(aborted, 1);
    assert!(
        !manager.is_live_task(&info.task_id),
        "live map should be drained"
    );

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("Interrupted by server reload"),
        "error should explain the reload interruption, got: {error}"
    );
    assert!(status.completed_at.is_some());

    // Idempotent: a second sweep finds nothing.
    assert_eq!(manager.abort_live_tasks_for_reload().await?, 0);
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_keeps_naturally_finished_status() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-reload-finished",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(0))) },
        )
        .await;

    // Let the task finish and write its terminal status.
    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        manager.abort_live_tasks_for_reload().await?,
        0,
        "a naturally completed task should not be counted as finalized"
    );

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(
        status.status,
        BackgroundTaskStatus::Completed,
        "a task that finished before the sweep must keep its real status"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn detached_stop_rejects_legacy_pid_without_identity_and_stops_verified_owned_group()
-> Result<()> {
    let directory = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(directory.path().into());
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30").process_group(0).kill_on_drop(true);
    let mut child = command.spawn()?;
    let pid = child.id().unwrap();
    let info = manager.reserve_task_info();
    manager
        .register_detached_task(
            &info,
            "fixture",
            None,
            "detached-identity",
            pid,
            &Utc::now().to_rfc3339(),
            false,
            false,
        )
        .await;
    let original: TaskStatusFile = crate::storage::read_json(&info.status_file)?;
    assert!(original.process_identity.is_some());
    let mut legacy = original.clone();
    legacy.process_identity = None;
    crate::storage::write_json_secret(&info.status_file, &legacy)?;
    let error = manager
        .cancel(&info.task_id)
        .await
        .expect_err("Unverified PID must not be signalled");
    assert!(error.to_string().contains("verified process identity"));
    assert!(child.try_wait()?.is_none());
    let stored: TaskStatusFile = crate::storage::read_json(&info.status_file)?;
    assert_eq!(stored.status, BackgroundTaskStatus::Running);
    crate::storage::write_json_secret(&info.output_file, &"captured-prefix")?;
    crate::storage::write_json_secret(&info.status_file, &original)?;
    assert!(manager.cancel(&info.task_id).await?);
    child.wait().await?;
    assert!(!crate::platform::process_group_has_live_members(pid).await?);
    assert_eq!(
        manager
            .status(&info.task_id)
            .await
            .unwrap()
            .error
            .as_deref(),
        Some("Cancelled by user")
    );
    Ok(())
}
