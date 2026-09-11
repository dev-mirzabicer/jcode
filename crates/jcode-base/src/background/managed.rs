use super::*;
use crate::execution::control_transport::{ControlOperation, ControlReply, control_in_store};
use crate::execution::{DeliveryState, ExecutionStore, RunState};
use anyhow::{Context, ensure};
use std::io::Read;

fn store() -> Result<ExecutionStore> {
    ExecutionStore::open(&crate::storage::jcode_dir()?)
}

fn projected(store: &ExecutionStore, id: &str) -> Result<Option<TaskStatusFile>> {
    let Some(delivery) = store.background_delivery(id)? else {
        return Ok(None);
    };
    let record = store
        .inspect(id)?
        .context("Delivery points to a missing execution")?;
    let status = if !record.state.terminal() {
        BackgroundTaskStatus::Running
    } else if record.state == RunState::Completed {
        BackgroundTaskStatus::Completed
    } else {
        BackgroundTaskStatus::Failed
    };
    let (started_at, completed_at, duration_secs) = store.execution_times(id)?;
    let error = match record.state {
        RunState::Cancelled | RunState::Interrupted => Some(
            record
                .stop_cause
                .map(|cause| cause.description())
                .unwrap_or("Execution interrupted")
                .to_string(),
        ),
        RunState::Failed
            if record
                .process_exit
                .as_ref()
                .is_some_and(|exit| exit.timed_out) =>
        {
            Some("Command timed out; inspect retained output".into())
        }
        RunState::Failed => Some("Execution failed; inspect the retained output".to_string()),
        _ => None,
    };
    Ok(Some(TaskStatusFile {
        task_id: id.into(),
        tool_name: record.tool,
        display_name: None,
        session_id: record.session_id,
        status,
        exit_code: record
            .process_exit
            .as_ref()
            .and_then(|exit| exit.shell_code()),
        error,
        started_at,
        completed_at,
        duration_secs,
        pid: None,
        #[cfg(unix)]
        process_identity: None,
        owner_pid: None,
        owner_instance: Some(record.owner),
        detached: false,
        notify: delivery.notify,
        wake: delivery.wake,
        progress: record
            .progress
            .as_ref()
            .map(|progress| progress.value.clone()),
        event_history: record
            .progress
            .map(|progress| {
                vec![BackgroundTaskEventRecord {
                    kind: if progress.checkpoint {
                        BackgroundTaskEventKind::Checkpoint
                    } else {
                        BackgroundTaskEventKind::Progress
                    },
                    timestamp: progress.value.updated_at.clone(),
                    message: progress.value.message.clone(),
                    status: None,
                    exit_code: None,
                    progress: Some(progress.value),
                }]
            })
            .unwrap_or_default(),
    }))
}

impl BackgroundTaskManager {
    pub(super) async fn register_managed(
        &self,
        id: &str,
        tool: &str,
        session: &str,
        handle: JoinHandle<Result<jcode_tool_types::ToolOutput>>,
        control: Arc<dyn jcode_tool_core::OwnedExecutionControl>,
        delivery: (bool, bool),
    ) -> Result<BackgroundTaskInfo> {
        let id = id.to_string();
        let tool = tool.to_string();
        let session = session.to_string();
        let saved = id.clone();
        let scope = (tool.clone(), session.clone());
        let (info, created) = tokio::task::spawn_blocking(move || {
            let store = store()?;
            let record = store
                .inspect(&saved)?
                .context("Unknown controlled invocation")?;
            ensure!(
                record.background,
                "Controlled background registration requires committed promotion"
            );
            ensure!(
                record.tool == scope.0 && record.session_id == scope.1,
                "Controlled task does not match its invocation"
            );
            let (_, created) =
                store.register_background_delivery(&saved, delivery.0, delivery.1)?;
            let directory = store.root().join("background");
            crate::storage::ensure_dir(&directory)?;
            ensure!(
                std::fs::symlink_metadata(&directory)?.is_dir(),
                "Background projection directory changed type"
            );
            let status_file = directory.join(format!("{saved}.status.json"));
            crate::storage::write_json_secret(
                &status_file,
                &projected(&store, &saved)?.context("Missing managed status")?,
            )?;
            Ok::<_, anyhow::Error>((
                BackgroundTaskInfo {
                    task_id: saved.clone(),
                    output_file: record.output_path.unwrap_or_else(|| {
                        store.root().join("outputs").join(&saved).join("output.txt")
                    }),
                    status_file,
                },
                created,
            ))
        })
        .await??;
        // The original Registry waiter is not the work owner. It is safe to
        // detach this delivery-only handle after promotion has been committed.
        drop(handle);
        let mut managed = self.managed.write().await;
        if !managed.contains_key(&id) {
            managed.insert(id.clone(), control.clone());
            let manager = Self {
                tasks: self.tasks.clone(),
                output_dir: self.output_dir.clone(),
                managed: self.managed.clone(),
            };
            tokio::spawn(async move {
                if control.wait().await.is_ok() {
                    let _ = manager.publish_managed_completion(&id).await;
                    manager.managed.write().await.remove(&id);
                }
            });
        }
        drop(managed);
        if created {
            Self::publish_task_started_activity(
                &info.task_id,
                &tool,
                None,
                &session,
                delivery.0 || delivery.1,
            );
        }
        Ok(info)
    }

    pub(super) async fn managed_status(&self, id: &str) -> Result<Option<TaskStatusFile>> {
        let id = id.to_string();
        tokio::task::spawn_blocking(move || projected(&store()?, &id)).await?
    }
    pub(super) async fn managed_statuses(&self) -> Result<Vec<TaskStatusFile>> {
        tokio::task::spawn_blocking(move || {
            let store = store()?;
            let mut statuses = Vec::new();
            for id in store.background_delivery_ids()? {
                if let Some(status) = projected(&store, &id)? {
                    statuses.push(status);
                }
            }
            Ok(statuses)
        })
        .await?
    }

    pub async fn publish_managed_completion(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        let completion = tokio::task::spawn_blocking(move || {
            let store = store()?;
            let Some(delivery) = store.background_delivery(&id)? else {
                return Ok(None);
            };
            let Some(status) = projected(&store, &id)? else {
                return Ok(None);
            };
            if status.status == BackgroundTaskStatus::Running {
                return Ok(None);
            }
            let notify = delivery.notify && delivery.notify_state == DeliveryState::Pending;
            let wake = delivery.wake && delivery.wake_state == DeliveryState::Pending;
            if !notify && !wake {
                return Ok(None);
            }
            let record = store
                .inspect(&id)?
                .context("Unknown completed invocation")?;
            let path = record
                .output_path
                .unwrap_or_else(|| store.root().join("receipts").join(format!("{id}.json")));
            let mut bytes = Vec::new();
            if let Ok(file) = std::fs::File::open(&path) {
                file.take(2000).read_to_end(&mut bytes)?;
            }
            let output_preview = String::from_utf8_lossy(&bytes).into_owned();
            Ok::<_, anyhow::Error>(Some(BackgroundTaskCompleted {
                task_id: id,
                tool_name: status.tool_name,
                display_name: status.display_name,
                session_id: status.session_id,
                status: status.status,
                exit_code: status.exit_code,
                output_preview,
                output_file: path,
                duration_secs: status.duration_secs.unwrap_or_default(),
                notify,
                wake,
            }))
        })
        .await??;
        if let Some(completion) = completion {
            Bus::global().publish(BusEvent::BackgroundTaskCompleted(completion));
        }
        Ok(())
    }

    pub async fn retry_managed_delivery(&self) -> Result<()> {
        let ids = tokio::task::spawn_blocking(move || {
            let store = store()?;
            store.reconcile_delivery_attempts()?;
            store.pending_background_deliveries()
        })
        .await??;
        for id in ids {
            self.publish_managed_completion(&id).await?;
        }
        Ok(())
    }

    pub(super) async fn quiesce_managed_for_reload(&self) -> Result<()> {
        let controls: Vec<_> = self
            .managed
            .read()
            .await
            .iter()
            .map(|(id, control)| (id.clone(), control.clone()))
            .collect();
        let mut errors = Vec::new();
        for (id, control) in controls {
            match control.survives_reload().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    errors.push(format!("{id}: {error}"));
                    continue;
                }
            }
            if let Err(error) = control
                .request_stop(jcode_tool_types::StopCause::ReloadQuiescence)
                .await
            {
                errors.push(format!("{id}: {error}"));
                continue;
            }
            match tokio::time::timeout(Duration::from_secs(2), control.wait()).await {
                Ok(Ok(_)) => {
                    self.managed.write().await.remove(&id);
                }
                Ok(Err(error)) => errors.push(format!("{id}: {error}")),
                Err(_) => errors.push(format!("{id}: owned execution is still quiescing")),
            }
        }
        ensure!(
            errors.is_empty(),
            "Reload could not quiesce owned executions: {}",
            errors.join("; ")
        );
        Ok(())
    }

    pub(super) async fn cancel_managed(&self, id: &str) -> Result<bool> {
        let store = store()?;
        let reply = control_in_store(
            &store,
            id,
            ControlOperation::Stop {
                cause: jcode_tool_types::StopCause::HumanCancellation,
            },
        )
        .await?;
        match reply {
            ControlReply::Accepted { changed: false } | ControlReply::Snapshot { .. } => {
                return Ok(false);
            }
            ControlReply::Unavailable { message } => anyhow::bail!("{message}"),
            _ => {}
        }
        match tokio::time::timeout(
            Duration::from_secs(2),
            control_in_store(&store, id, ControlOperation::Wait),
        )
        .await
        {
            Ok(Ok(ControlReply::Snapshot { record })) => Ok(record.state == RunState::Cancelled),
            Ok(Ok(ControlReply::Unavailable { message })) => Err(anyhow::anyhow!(message)),
            Ok(Err(error)) => Err(error),
            _ => anyhow::bail!(
                "Stop was requested, but the execution has not published a terminal outcome"
            ),
        }
    }
}
