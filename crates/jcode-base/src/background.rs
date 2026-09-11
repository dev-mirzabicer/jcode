//! Background task execution manager
//!
//! Allows tools to run in the background and notify the agent when complete.
//! Uses file-based storage for crash resilience + event channel for real-time notifications.

use crate::bus::{
    BackgroundTaskCompleted, BackgroundTaskProgress, BackgroundTaskProgressEvent,
    BackgroundTaskProgressSource, BackgroundTaskStatus, Bus, BusEvent,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant as TokioInstant, MissedTickBehavior};

mod model;

pub use model::{
    BackgroundCleanupResult, BackgroundTaskEventKind, BackgroundTaskEventRecord,
    BackgroundTaskInfo, BackgroundTaskWaitReason, BackgroundTaskWaitResult,
    RunningBackgroundProgress, TaskResult, TaskStatusFile, format_progress_display,
    format_progress_summary, render_progress_bar,
};

/// One identity per process image, shared by execution and background owners.
pub fn runtime_instance_id() -> &'static str {
    model::process_instance_token()
}
use model::{
    EXIT_MARKER_PREFIX, RunningTask, normalize_delivery, progress_equivalent,
    progress_event_record, progress_wait_reason, push_task_event, task_dir, terminal_event_record,
};

struct AbortAdoptedOnDrop(tokio::task::AbortHandle);

struct Adoption {
    tool_name: String,
    display_name: Option<String>,
    session_id: String,
    notify: bool,
    wake: bool,
}

impl Drop for AbortAdoptedOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Manages background task execution
pub struct BackgroundTaskManager {
    tasks: Arc<RwLock<HashMap<String, RunningTask>>>,
    output_dir: PathBuf,
}

impl BackgroundTaskManager {
    /// Create a manager rooted at a specific output directory.
    ///
    /// Primarily for tests; production code should use [`global`].
    pub fn with_output_dir(output_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&output_dir).ok();
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            output_dir,
        }
    }

    /// Create a new background task manager
    pub fn new() -> Self {
        let output_dir = task_dir();
        Self::with_output_dir(output_dir)
    }

    /// Generate a short, unique task ID
    fn generate_task_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        const TASK_ID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // Use last 6 digits of timestamp + 4 random chars
        let rand_part: String = (0..4)
            .map(|_| {
                let idx = (rand::random::<u8>() as usize) % TASK_ID_ALPHABET.len();
                TASK_ID_ALPHABET[idx] as char
            })
            .collect();
        format!(
            "{}{}",
            &timestamp.to_string()[timestamp.to_string().len().saturating_sub(6)..],
            rand_part
        )
    }

    pub fn output_path_for(&self, task_id: &str) -> PathBuf {
        self.output_dir.join(format!("{}.output", task_id))
    }

    pub fn status_path_for(&self, task_id: &str) -> PathBuf {
        self.output_dir.join(format!("{}.status.json", task_id))
    }

    fn publish_task_started_activity(
        task_id: &str,
        tool_name: &str,
        display_name: Option<&str>,
        session_id: &str,
        notify: bool,
    ) {
        if !notify {
            return;
        }
        let label = crate::message::background_task_display_label(tool_name, display_name);
        let safe_label = label.replace('`', "'");
        Bus::global().publish(BusEvent::UiActivity(crate::bus::UiActivity::background(
            Some(session_id.to_string()),
            format!(
                "**Background task started** `{}` · `{}`\n\nJcode is running this in the background. Progress, checkpoints, and completion will appear here.",
                task_id, safe_label
            ),
            Some(format!("Background task started · {}", label)),
        )));
    }

    fn status_duration_secs(started_at: &str, completed_at: DateTime<Utc>) -> Option<f64> {
        DateTime::parse_from_rfc3339(started_at)
            .ok()
            .and_then(|started| (completed_at - started.with_timezone(&Utc)).to_std().ok())
            .map(|duration| duration.as_secs_f64())
    }

    fn parse_exit_code_from_output(output: &str) -> Option<i32> {
        output.lines().rev().find_map(|line| {
            let trimmed = line.trim();
            let suffix = trimmed.strip_prefix(EXIT_MARKER_PREFIX)?;
            let suffix = suffix.strip_suffix(" ---")?;
            suffix.trim().parse::<i32>().ok()
        })
    }

    async fn read_status_file(&self, path: &std::path::Path) -> Option<TaskStatusFile> {
        let content = fs::read_to_string(path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    async fn write_status_file(&self, path: &std::path::Path, status: &TaskStatusFile) {
        if let Ok(json) = serde_json::to_string_pretty(status) {
            let _ = fs::write(path, json).await;
        }
    }

    async fn finalize_detached_status_if_needed(
        &self,
        mut status: TaskStatusFile,
        status_path: &std::path::Path,
    ) -> TaskStatusFile {
        if status.status != BackgroundTaskStatus::Running || !status.detached {
            return status;
        }

        let Some(pid) = status.pid else {
            return status;
        };

        let reaped_exit = crate::platform::try_reap_child_process(pid).ok().flatten();

        if reaped_exit.is_none() && crate::platform::is_process_running(pid) {
            return status;
        }

        let output_path = self.output_path_for(&status.task_id);
        let output = fs::read_to_string(&output_path).await.unwrap_or_default();
        let exit_code = reaped_exit.or_else(|| Self::parse_exit_code_from_output(&output));
        let completed_at = Utc::now();
        let duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
        let final_status = if matches!(exit_code, Some(0)) {
            BackgroundTaskStatus::Completed
        } else {
            BackgroundTaskStatus::Failed
        };
        let final_error = if matches!(final_status, BackgroundTaskStatus::Failed) {
            Some(match exit_code {
                Some(code) => format!("Command exited with code {}", code),
                None => "Detached command exited without a readable exit code".to_string(),
            })
        } else {
            None
        };

        status.status = final_status.clone();
        status.exit_code = exit_code;
        status.error = final_error.clone();
        status.completed_at = Some(completed_at.to_rfc3339());
        status.duration_secs = duration_secs;
        status.pid = Some(pid);
        push_task_event(
            &mut status,
            terminal_event_record(final_status.clone(), exit_code, final_error.as_deref()),
        );

        self.write_status_file(status_path, &status).await;

        let output_preview = if output.len() > 500 {
            format!("{}...", crate::util::truncate_str(&output, 500))
        } else {
            output
        };
        Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
            task_id: status.task_id.clone(),
            tool_name: status.tool_name.clone(),
            display_name: status.display_name.clone(),
            session_id: status.session_id.clone(),
            status: final_status,
            exit_code,
            output_preview,
            output_file: output_path,
            duration_secs: duration_secs.unwrap_or_default(),
            notify: status.notify,
            wake: status.wake,
        }));

        status
    }

    /// True when a non-detached `Running` status file provably belongs to a
    /// process image that no longer exists, so no future can ever finalize it.
    ///
    /// Rules, deliberately conservative because the task dir is shared by
    /// every jcode process on the machine:
    /// - Terminal, detached, or pid-bearing files are never orphans here
    ///   (detached reconciliation is `finalize_detached_status_if_needed`).
    /// - Files owned by this exact process image are never orphans: the
    ///   initial status file is written before the task lands in the live
    ///   map, so "Running + not in map + my instance" can simply mean the
    ///   task is still bootstrapping.
    /// - Files owned by this PID but a different instance token are orphans:
    ///   an exec-based reload replaced the process image, so the owning
    ///   future is gone even though the PID matches.
    /// - Files owned by another PID are orphans only once that process is
    ///   dead.
    /// - Files without owner metadata (written by older builds) are left
    ///   alone; only the explicit startup sweep in
    ///   [`Self::reconcile_orphaned_tasks`] handles those.
    fn status_is_reconcilable_orphan(status: &TaskStatusFile) -> bool {
        if status.status != BackgroundTaskStatus::Running || status.detached || status.pid.is_some()
        {
            return false;
        }
        let Some(owner_pid) = status.owner_pid else {
            return false;
        };
        if status.owner_instance.as_deref() == Some(model::process_instance_token()) {
            return false;
        }
        if owner_pid == std::process::id() {
            return true;
        }
        !crate::platform::is_process_running(owner_pid)
    }

    /// Finalize an orphaned non-detached `Running` status file as `Failed`.
    ///
    /// The owning process's task future died with the process (crash or
    /// exec-based server reload), so without this the file reads `Running`
    /// forever: `bg list`/`bg status` show a phantom task and `bg wait`
    /// blocks until its timeout.
    async fn finalize_orphaned_status_if_needed(
        &self,
        mut status: TaskStatusFile,
        status_path: &std::path::Path,
    ) -> TaskStatusFile {
        if !Self::status_is_reconcilable_orphan(&status) {
            return status;
        }
        // Belt and braces: never rewrite a task this process is executing.
        if self.is_live_task(&status.task_id) {
            return status;
        }

        let completed_at = Utc::now();
        let duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
        let error =
            "Task orphaned: the owning server process exited (reloaded or crashed) before the task finished"
                .to_string();
        status.status = BackgroundTaskStatus::Failed;
        status.exit_code = None;
        status.error = Some(error.clone());
        status.completed_at = Some(completed_at.to_rfc3339());
        status.duration_secs = duration_secs;
        push_task_event(
            &mut status,
            terminal_event_record(BackgroundTaskStatus::Failed, None, Some(&error)),
        );
        self.write_status_file(status_path, &status).await;

        let output_path = self.output_path_for(&status.task_id);
        let output = fs::read_to_string(&output_path).await.unwrap_or_default();
        let output_preview = if output.len() > 500 {
            format!("{}...", crate::util::truncate_str(&output, 500))
        } else {
            output
        };
        Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
            task_id: status.task_id.clone(),
            tool_name: status.tool_name.clone(),
            display_name: status.display_name.clone(),
            session_id: status.session_id.clone(),
            status: BackgroundTaskStatus::Failed,
            exit_code: None,
            output_preview,
            output_file: output_path,
            duration_secs: duration_secs.unwrap_or_default(),
            notify: status.notify,
            wake: status.wake,
        }));

        status
    }

    /// Startup/reload sweep: mark orphaned non-detached `Running` status
    /// files as `Failed` with a "server reloaded" note.
    ///
    /// Only owner-tagged files are considered, using the liveness rules of
    /// [`Self::status_is_reconcilable_orphan`]. Files without owner metadata
    /// (written by older builds, or by processes that legitimately still run
    /// them) are left untouched: the task dir is shared machine-wide, so
    /// without owner metadata there is no safe way to distinguish a phantom
    /// from another live process's task. Returns how many files were
    /// reconciled.
    pub async fn reconcile_orphaned_tasks(&self) -> usize {
        let mut reconciled = 0;
        let Ok(mut entries) = fs::read_dir(&self.output_dir).await else {
            return reconciled;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(status) = self.read_status_file(&path).await else {
                continue;
            };
            if !Self::status_is_reconcilable_orphan(&status) {
                continue;
            }
            if self.tasks.read().await.contains_key(&status.task_id) {
                continue;
            }
            self.finalize_orphaned_status_if_needed(status, &path).await;
            reconciled += 1;
        }
        reconciled
    }

    pub fn reserve_task_info(&self) -> BackgroundTaskInfo {
        let task_id = Self::generate_task_id();
        let output_file = self.output_path_for(&task_id);
        let status_file = self.status_path_for(&task_id);
        BackgroundTaskInfo {
            task_id,
            output_file,
            status_file,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Detached task registration mirrors persisted status fields and existing call sites"
    )]
    pub async fn register_detached_task(
        &self,
        info: &BackgroundTaskInfo,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        pid: u32,
        started_at: &str,
        notify: bool,
        wake: bool,
    ) {
        let (notify, wake) = normalize_delivery(notify, wake);
        let status = TaskStatusFile {
            task_id: info.task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name,
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: started_at.to_string(),
            completed_at: None,
            duration_secs: None,
            pid: Some(pid),
            // Detached processes outlive this server, so no in-process owner:
            // reconciliation must never clobber them.
            owner_pid: None,
            owner_instance: None,
            detached: true,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
        };
        self.write_status_file(&info.status_file, &status).await;
        Self::publish_task_started_activity(
            &info.task_id,
            tool_name,
            status.display_name.as_deref(),
            session_id,
            notify,
        );
    }

    /// Spawn a background task
    ///
    /// The `execute_fn` receives the output file path and should write output there.
    /// It returns a TaskResult with exit code and optional error.
    pub async fn spawn<F, Fut>(
        &self,
        tool_name: &str,
        session_id: &str,
        execute_fn: F,
    ) -> BackgroundTaskInfo
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<TaskResult>> + Send,
    {
        self.spawn_with_notify(tool_name, None, session_id, true, false, execute_fn)
            .await
    }

    /// Spawn a background task with explicit notify flag
    pub async fn spawn_with_notify<F, Fut>(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        execute_fn: F,
    ) -> BackgroundTaskInfo
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<TaskResult>> + Send,
    {
        let (notify, wake) = normalize_delivery(notify, wake);
        let task_id = Self::generate_task_id();
        let output_path = self.output_dir.join(format!("{}.output", task_id));
        let status_path = self.output_dir.join(format!("{}.status.json", task_id));
        let started_at_rfc3339 = chrono::Utc::now().to_rfc3339();

        // Write initial status file
        let initial_status = TaskStatusFile {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: display_name.clone(),
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: started_at_rfc3339.clone(),
            completed_at: None,
            duration_secs: None,
            pid: None,
            owner_pid: Some(std::process::id()),
            owner_instance: Some(model::process_instance_token().to_string()),
            detached: false,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&initial_status) {
            let _ = std::fs::write(&status_path, json);
        }
        Self::publish_task_started_activity(
            &task_id,
            tool_name,
            display_name.as_deref(),
            session_id,
            notify,
        );

        let output_path_clone = output_path.clone();
        let status_path_clone = status_path.clone();
        let task_id_clone = task_id.clone();
        let tool_name_owned = tool_name.to_string();
        let display_name_owned = display_name.clone();
        let session_id_owned = session_id.to_string();
        let started_at = Instant::now();
        let started_at_rfc3339_for_task = started_at_rfc3339.clone();
        let (delivery_flags_tx, delivery_flags_rx) = watch::channel((notify, wake));
        let tasks_for_prune = Arc::clone(&self.tasks);
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn the background task
        let handle = tokio::spawn(async move {
            let result = execute_fn(output_path_clone.clone()).await;

            let duration_secs = started_at.elapsed().as_secs_f64();
            let (status, exit_code, error) = match &result {
                Ok(task_result) => {
                    let status = task_result.status.clone().unwrap_or_else(|| {
                        if task_result.error.is_some() {
                            BackgroundTaskStatus::Failed
                        } else {
                            BackgroundTaskStatus::Completed
                        }
                    });
                    (status, task_result.exit_code, task_result.error.clone())
                }
                Err(e) => (BackgroundTaskStatus::Failed, None, Some(e.to_string())),
            };

            let (notify_flag, wake_flag) = *delivery_flags_rx.borrow();
            let prior_status = tokio::fs::read_to_string(&status_path_clone)
                .await
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let prior_progress = prior_status
                .as_ref()
                .and_then(|status| status.progress.clone());
            let prior_event_history = prior_status
                .map(|status| status.event_history)
                .unwrap_or_default();

            // Update status file
            let mut final_status = TaskStatusFile {
                task_id: task_id_clone.clone(),
                tool_name: tool_name_owned.clone(),
                display_name: display_name_owned.clone(),
                session_id: session_id_owned.clone(),
                status: status.clone(),
                exit_code,
                error: error.clone(),
                started_at: started_at_rfc3339_for_task,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(duration_secs),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_progress,
                event_history: prior_event_history,
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(status.clone(), exit_code, error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                let _ = tokio::fs::write(&status_path_clone, json).await;
            }

            // Drop this task from the live map now that its terminal status is
            // persisted. Order matters: pruning only after the status-file
            // write keeps "in the live map while the status file says Running"
            // equivalent to "a task future is actually executing", which the
            // run_plan duplicate-driver guard and self-dev build reconciliation
            // rely on. Awaiting registration first means a task that finishes
            // instantly cannot race the insert below and leave a permanent
            // phantom entry in the map.
            let _ = registered_rx.await;
            tasks_for_prune.write().await.remove(&task_id_clone);

            // Read output preview for notification
            let output_preview = tokio::fs::read_to_string(&output_path_clone)
                .await
                .map(|s| {
                    if s.len() > 500 {
                        format!("{}...", crate::util::truncate_str(&s, 500))
                    } else {
                        s
                    }
                })
                .unwrap_or_default();

            // Publish completion event to the bus
            Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
                task_id: task_id_clone,
                tool_name: tool_name_owned,
                display_name: display_name_owned,
                session_id: session_id_owned,
                status,
                exit_code,
                output_preview,
                output_file: output_path_clone,
                duration_secs,
                notify: notify_flag,
                wake: wake_flag,
            }));

            result
        });

        // Track the running task
        let running_task = RunningTask {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name,
            status_path: status_path.clone(),
            delivery_flags: delivery_flags_tx,
            handle: Some(handle),
            adopted_abort: None,
            stop_cause: Arc::new(std::sync::Mutex::new(None)),
            execution_control: None,
        };

        self.tasks
            .write()
            .await
            .insert(task_id.clone(), running_task);
        let _ = registered_tx.send(());

        BackgroundTaskInfo {
            task_id,
            output_file: output_path,
            status_file: status_path,
        }
    }

    /// Adopt an already-spawned task as a background task.
    /// Used when the user moves a currently-executing tool to background via Alt+B.
    /// The `handle` is an already-running tokio task; we just register it for tracking
    /// and wire up completion notifications.
    pub async fn adopt(
        &self,
        tool_name: &str,
        session_id: &str,
        handle: JoinHandle<Result<jcode_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        self.adopt_with_options(tool_name, None, session_id, true, false, handle)
            .await
    }

    /// Adopt an already-spawned task as a background task, with explicit display
    /// name and delivery flags. Used both for user-initiated handoff (Alt+B) and
    /// for promoting a foreground command that exceeded its timeout but is still
    /// running, so it keeps running and surfaces as a background-task card.
    pub async fn adopt_with_options(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        handle: JoinHandle<Result<jcode_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        self.adopt_impl(
            Adoption {
                tool_name: tool_name.into(),
                display_name,
                session_id: session_id.into(),
                notify,
                wake,
            },
            handle,
            None,
        )
        .await
    }

    pub async fn adopt_controlled(
        &self,
        tool_name: &str,
        session_id: &str,
        handle: JoinHandle<Result<jcode_tool_types::ToolOutput>>,
        control: Arc<dyn jcode_tool_core::OwnedExecutionControl>,
    ) -> BackgroundTaskInfo {
        self.adopt_impl(
            Adoption {
                tool_name: tool_name.into(),
                display_name: None,
                session_id: session_id.into(),
                notify: true,
                wake: false,
            },
            handle,
            Some(control),
        )
        .await
    }

    async fn adopt_impl(
        &self,
        options: Adoption,
        handle: JoinHandle<Result<jcode_tool_types::ToolOutput>>,
        execution_control: Option<Arc<dyn jcode_tool_core::OwnedExecutionControl>>,
    ) -> BackgroundTaskInfo {
        let Adoption {
            tool_name,
            display_name,
            session_id,
            notify,
            wake,
        } = options;
        let tool_name = tool_name.as_str();
        let session_id = session_id.as_str();
        let (notify, wake) = normalize_delivery(notify, wake);
        let task_id = Self::generate_task_id();
        let output_path = self.output_dir.join(format!("{}.output", task_id));
        let status_path = self.output_dir.join(format!("{}.status.json", task_id));

        let initial_status = TaskStatusFile {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: display_name.clone(),
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_secs: None,
            pid: None,
            owner_pid: Some(std::process::id()),
            owner_instance: Some(model::process_instance_token().to_string()),
            detached: false,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&initial_status) {
            let _ = std::fs::write(&status_path, json);
        }
        Self::publish_task_started_activity(
            &task_id,
            tool_name,
            display_name.as_deref(),
            session_id,
            notify,
        );

        let output_path_clone = output_path.clone();
        let status_path_clone = status_path.clone();
        let task_id_clone = task_id.clone();
        let tool_name_owned = tool_name.to_string();
        let session_id_owned = session_id.to_string();
        let started_at = Instant::now();
        let started_at_rfc3339 = initial_status.started_at.clone();
        let display_name_owned = initial_status.display_name.clone();
        let (delivery_flags_tx, delivery_flags_rx) = watch::channel((notify, wake));
        let tasks_for_prune = Arc::clone(&self.tasks);
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();

        let adopted_abort = handle.abort_handle();
        let stop_cause = Arc::new(std::sync::Mutex::new(None::<jcode_tool_types::StopCause>));
        let wrapper_stop_cause = Arc::clone(&stop_cause);
        let abort_on_drop = AbortAdoptedOnDrop(adopted_abort.clone());
        let wrapper_control = execution_control.clone();
        let wrapper_handle = tokio::spawn(async move {
            let _abort_on_drop = abort_on_drop;
            let tool_result = handle.await;
            let duration_secs = started_at.elapsed().as_secs_f64();

            let (mut status, exit_code, mut error, output_text) = match tool_result {
                Ok(Ok(output)) => (
                    BackgroundTaskStatus::Completed,
                    Some(0),
                    None,
                    output.output,
                ),
                Ok(Err(e)) => (
                    BackgroundTaskStatus::Failed,
                    None,
                    Some(e.to_string()),
                    e.to_string(),
                ),
                Err(e) if e.is_cancelled() => {
                    let cause = wrapper_stop_cause
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .unwrap_or(jcode_tool_types::StopCause::OwnerCrash);
                    (
                        BackgroundTaskStatus::Failed,
                        None,
                        Some(cause.description().to_string()),
                        cause.description().to_string(),
                    )
                }
                Err(e) => (
                    BackgroundTaskStatus::Failed,
                    None,
                    Some(e.to_string()),
                    format!("Task panicked: {}", e),
                ),
            };

            if let Some(control) = wrapper_control {
                match control.wait().await {
                    Ok(jcode_tool_types::RunState::Cancelled) => {
                        status = BackgroundTaskStatus::Failed;
                        let cause = wrapper_stop_cause
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .unwrap_or(jcode_tool_types::StopCause::HumanCancellation);
                        error = Some(cause.description().to_string());
                    }
                    Ok(jcode_tool_types::RunState::Interrupted) => {
                        status = BackgroundTaskStatus::Failed;
                        error = Some(
                            jcode_tool_types::StopCause::ReloadQuiescence
                                .description()
                                .to_string(),
                        );
                    }
                    Err(failure) => {
                        status = BackgroundTaskStatus::Failed;
                        error = Some(format!("Execution outcome unavailable: {failure:#}"));
                    }
                    _ => {}
                }
            }

            if let Ok(mut file) = File::create(&output_path_clone).await {
                let _ = file.write_all(output_text.as_bytes()).await;
            }

            let (notify_flag, wake_flag) = *delivery_flags_rx.borrow();
            let prior_status = tokio::fs::read_to_string(&status_path_clone)
                .await
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let prior_progress = prior_status
                .as_ref()
                .and_then(|status| status.progress.clone());
            let prior_event_history = prior_status
                .map(|status| status.event_history)
                .unwrap_or_default();

            let mut final_status = TaskStatusFile {
                task_id: task_id_clone.clone(),
                tool_name: tool_name_owned.clone(),
                display_name: display_name_owned.clone(),
                session_id: session_id_owned.clone(),
                status: status.clone(),
                exit_code,
                error: error.clone(),
                started_at: started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(duration_secs),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_progress,
                event_history: prior_event_history,
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(status.clone(), exit_code, error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                let _ = tokio::fs::write(&status_path_clone, json).await;
            }

            // Prune the live-map entry only after the terminal status file is
            // persisted (and after registration below, so instant completions
            // cannot race the insert and leave a phantom entry).
            let _ = registered_rx.await;
            tasks_for_prune.write().await.remove(&task_id_clone);

            let output_preview = if output_text.len() > 500 {
                format!("{}...", crate::util::truncate_str(&output_text, 500))
            } else {
                output_text
            };

            Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
                task_id: task_id_clone,
                tool_name: tool_name_owned,
                display_name: display_name_owned,
                session_id: session_id_owned,
                status: status.clone(),
                exit_code,
                output_preview,
                output_file: output_path_clone,
                duration_secs,
                notify: notify_flag,
                wake: wake_flag,
            }));

            Ok(TaskResult {
                exit_code,
                error,
                status: Some(status),
            })
        });

        let running_task = RunningTask {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: None,
            status_path: status_path.clone(),
            delivery_flags: delivery_flags_tx,
            handle: Some(wrapper_handle),
            adopted_abort: Some(adopted_abort),
            stop_cause,
            execution_control,
        };

        self.tasks
            .write()
            .await
            .insert(task_id.clone(), running_task);
        let _ = registered_tx.send(());

        BackgroundTaskInfo {
            task_id,
            output_file: output_path,
            status_file: status_path,
        }
    }

    /// List all tasks (both running and completed from disk)
    pub async fn list(&self) -> Vec<TaskStatusFile> {
        let mut results = Vec::new();

        // Read all status files from disk
        if let Ok(mut entries) = fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false)
                    && let Some(status) = self.read_status_file(&path).await
                {
                    let reconciled = self.finalize_detached_status_if_needed(status, &path).await;
                    let reconciled = self
                        .finalize_orphaned_status_if_needed(reconciled, &path)
                        .await;
                    results.push(reconciled);
                }
            }
        }

        // Sort by task_id (which includes timestamp)
        results.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        results
    }

    /// Get status of a specific task
    pub async fn status(&self, task_id: &str) -> Option<TaskStatusFile> {
        let status_path = self.status_path_for(task_id);
        let status = self.read_status_file(&status_path).await?;
        let status = self
            .finalize_detached_status_if_needed(status, &status_path)
            .await;
        Some(
            self.finalize_orphaned_status_if_needed(status, &status_path)
                .await,
        )
    }

    /// Best-effort synchronous check for whether a task is still live in this process.
    pub fn is_live_task(&self, task_id: &str) -> bool {
        let Ok(tasks) = self.tasks.try_read() else {
            return false;
        };
        tasks.contains_key(task_id)
    }

    /// Get full output of a task
    pub async fn output(&self, task_id: &str) -> Option<String> {
        let output_path = self.output_path_for(task_id);
        fs::read_to_string(&output_path).await.ok()
    }

    /// Wait for a task to finish, emit progress, or reach an optional caller deadline.
    ///
    /// This combines bus-driven wakeups with light periodic status reconciliation so
    /// detached tasks, missed broadcasts, and crash or reload edges are still observed
    /// without active polling by the agent. A `None` deadline waits until a terminal or
    /// requested progress event, however long that takes.
    pub async fn wait(
        &self,
        task_id: &str,
        max_wait: Option<Duration>,
        return_on_progress: bool,
    ) -> Option<BackgroundTaskWaitResult> {
        let mut bus_rx = Bus::global().subscribe();
        let initial = self.status(task_id).await?;
        if initial.status != BackgroundTaskStatus::Running {
            return Some(BackgroundTaskWaitResult {
                reason: BackgroundTaskWaitReason::AlreadyFinished,
                task: initial,
                progress_event: None,
                event_record: None,
            });
        }
        if max_wait.map(|duration| duration.is_zero()).unwrap_or(false) {
            return Some(BackgroundTaskWaitResult {
                reason: BackgroundTaskWaitReason::Timeout,
                task: initial,
                progress_event: None,
                event_record: None,
            });
        }

        let mut last_progress = initial.progress.clone();
        let timeout =
            max_wait.map(|duration| tokio::time::sleep_until(TokioInstant::now() + duration));
        tokio::pin!(timeout);
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut bus_open = true;

        loop {
            tokio::select! {
                _ = async {
                    match timeout.as_mut().as_pin_mut() {
                        Some(timeout) => timeout.await,
                        None => std::future::pending().await,
                    }
                }, if max_wait.is_some() => {
                    let task = self.status(task_id).await?;
                    let reason = if task.status == BackgroundTaskStatus::Running {
                        BackgroundTaskWaitReason::Timeout
                    } else {
                        BackgroundTaskWaitReason::Finished
                    };
                    return Some(BackgroundTaskWaitResult {
                        reason,
                        task,
                        progress_event: None,
                        event_record: None,
                    });
                }
                _ = poll.tick() => {
                    let task = self.status(task_id).await?;
                    if task.status != BackgroundTaskStatus::Running {
                        return Some(BackgroundTaskWaitResult {
                            reason: BackgroundTaskWaitReason::Finished,
                            task,
                            progress_event: None,
                            event_record: None,
                        });
                    }
                    if return_on_progress && task.progress != last_progress {
                        let event_record = task.event_history.last().cloned();
                        return Some(BackgroundTaskWaitResult {
                            reason: progress_wait_reason(event_record.as_ref()),
                            progress_event: None,
                            task,
                            event_record,
                        });
                    }
                    last_progress = task.progress.clone();
                }
                event = bus_rx.recv(), if bus_open => {
                    match event {
                        Ok(BusEvent::BackgroundTaskCompleted(event)) if event.task_id == task_id => {
                            let task = self.status(task_id).await?;
                            return Some(BackgroundTaskWaitResult {
                                reason: BackgroundTaskWaitReason::Finished,
                                task,
                                progress_event: None,
                                event_record: None,
                            });
                        }
                        Ok(BusEvent::BackgroundTaskProgress(event)) if event.task_id == task_id => {
                            if return_on_progress {
                                let task = self.status(task_id).await?;
                                let event_record = task.event_history.last().cloned();
                                return Some(BackgroundTaskWaitResult {
                                    reason: progress_wait_reason(event_record.as_ref()),
                                    task,
                                    progress_event: Some(event),
                                    event_record,
                                });
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let task = self.status(task_id).await?;
                            if task.status != BackgroundTaskStatus::Running {
                                return Some(BackgroundTaskWaitResult {
                                    reason: BackgroundTaskWaitReason::Finished,
                                    task,
                                    progress_event: None,
                                    event_record: None,
                                });
                            }
                            if return_on_progress && task.progress != last_progress {
                                let event_record = task.event_history.last().cloned();
                                return Some(BackgroundTaskWaitResult {
                                    reason: progress_wait_reason(event_record.as_ref()),
                                    progress_event: None,
                                    task,
                                    event_record,
                                });
                            }
                            last_progress = task.progress.clone();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            let task = self.status(task_id).await?;
                            if task.status != BackgroundTaskStatus::Running {
                                return Some(BackgroundTaskWaitResult {
                                    reason: BackgroundTaskWaitReason::Finished,
                                    task,
                                    progress_event: None,
                                    event_record: None,
                                });
                            }
                            bus_open = false;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Update progress for an existing background task.
    pub async fn update_progress(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Progress)
            .await
    }

    /// Record an explicit checkpoint for an existing background task.
    pub async fn update_checkpoint(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Checkpoint)
            .await
    }

    async fn update_progress_with_event_kind(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
        event_kind: BackgroundTaskEventKind,
    ) -> Result<Option<TaskStatusFile>> {
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };

        let progress = progress.normalize();
        if let Some(existing) = status.progress.as_ref() {
            if progress_equivalent(existing, &progress) {
                return Ok(Some(status));
            }

            let existing_is_more_determinate = existing.percent.is_some()
                || matches!((existing.current, existing.total), (_, Some(total)) if total > 0);
            let new_is_less_determinate = progress.percent.is_none()
                && !matches!((progress.current, progress.total), (_, Some(total)) if total > 0);
            if existing_is_more_determinate
                && new_is_less_determinate
                && matches!(progress.source, BackgroundTaskProgressSource::ParsedOutput)
            {
                return Ok(Some(status));
            }
        }

        status.progress = Some(progress.clone());
        push_task_event(
            &mut status,
            progress_event_record(event_kind, progress.clone()),
        );
        self.write_status_file(&status_path, &status).await;

        Bus::global().publish(BusEvent::BackgroundTaskProgress(
            BackgroundTaskProgressEvent {
                task_id: status.task_id.clone(),
                tool_name: status.tool_name.clone(),
                display_name: status.display_name.clone(),
                session_id: status.session_id.clone(),
                progress,
            },
        ));

        Ok(Some(status))
    }

    /// Update delivery behavior for an existing background task.
    ///
    /// This supports retroactively enabling notify/wake after the task was already started.
    pub async fn update_delivery(
        &self,
        task_id: &str,
        notify: bool,
        wake: bool,
    ) -> Result<Option<TaskStatusFile>> {
        let (notify, wake) = normalize_delivery(notify, wake);
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };
        status.notify = notify;
        status.wake = wake;
        let event_status = status.status.clone();
        let event_exit_code = status.exit_code;
        let event_progress = status.progress.clone();
        push_task_event(
            &mut status,
            BackgroundTaskEventRecord {
                kind: BackgroundTaskEventKind::DeliveryUpdated,
                timestamp: Utc::now().to_rfc3339(),
                message: Some(format!("notify={}, wake={}", notify, wake)),
                status: Some(event_status),
                exit_code: event_exit_code,
                progress: event_progress,
            },
        );
        self.write_status_file(&status_path, &status).await;

        if let Some(task) = self.tasks.read().await.get(task_id) {
            let _ = task.delivery_flags.send((notify, wake));
        }

        Ok(Some(status))
    }

    /// A stop operation owns the join independently of its caller's wait.
    /// The live entry remains present while work is quiescing or cannot stop.
    async fn stop_live_task(
        &self,
        task_id: &str,
        cause: jcode_tool_types::StopCause,
    ) -> Result<Option<bool>> {
        let (mut handle, abort, control) = {
            let mut tasks = self.tasks.write().await;
            let Some(task) = tasks.get_mut(task_id) else {
                return Ok(None);
            };
            let handle = task
                .handle
                .take()
                .ok_or_else(|| anyhow::anyhow!("Stop is already in progress for {task_id}"))?;
            task.stop_cause
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_or_insert(cause);
            let abort = task
                .adopted_abort
                .clone()
                .unwrap_or_else(|| handle.abort_handle());
            (handle, abort, task.execution_control.clone())
        };
        let manager = Self {
            tasks: Arc::clone(&self.tasks),
            output_dir: self.output_dir.clone(),
        };
        let id = task_id.to_string();
        // Dropping a cancel/wait caller does not detach the real work from its
        // stop owner. This supervisor completes publication or restores the join.
        tokio::spawn(async move {
            if let Some(control)=&control {
                if let Err(error)=control.request_stop(cause) {
                    if let Some(task)=manager.tasks.write().await.get_mut(&id) {task.handle=Some(handle);}
                    return Err(error);
                }
            } else {abort.abort();}
            let joined = match tokio::time::timeout(Duration::from_secs(2), &mut handle).await {
                Ok(result) => result,
                Err(_) => {
                    if let Some(task) = manager.tasks.write().await.get_mut(&id) {
                        task.handle = Some(handle);
                    }
                    anyhow::bail!("Stop requested for {id}, but owned work has not quiesced; it remains active");
                }
            };
            let path = manager.status_path_for(&id);
            let mut status = manager.read_status_file(&path).await
                .ok_or_else(|| anyhow::anyhow!("Owned work stopped, but its status receipt is unavailable: {id}"))?;
            if status.status == BackgroundTaskStatus::Running {
                let (state, exit_code, error) = match joined {
                    Ok(Ok(result)) => (result.status.unwrap_or_else(|| if result.error.is_some() { BackgroundTaskStatus::Failed } else { BackgroundTaskStatus::Completed }), result.exit_code, result.error),
                    Ok(Err(error)) => (BackgroundTaskStatus::Failed, None, Some(error.to_string())),
                    Err(error) if error.is_cancelled() => (BackgroundTaskStatus::Failed, None, Some(cause.description().to_string())),
                    Err(error) => (BackgroundTaskStatus::Failed, None, Some(format!("Task panicked: {error}"))),
                };
                status.status = state;
                status.exit_code = exit_code;
                status.error = error;
                let completed = Utc::now();
                status.completed_at = Some(completed.to_rfc3339());
                status.duration_secs = Self::status_duration_secs(&status.started_at, completed);
                let event = terminal_event_record(status.status.clone(), status.exit_code, status.error.as_deref());
                push_task_event(&mut status, event);
            }
            tokio::task::spawn_blocking(move || crate::storage::write_json_secret(&path, &status)).await?
                .map_err(|error| anyhow::anyhow!("Owned work stopped, but terminal receipt persistence failed: {error:#}"))?;
            manager.tasks.write().await.remove(&id);
            Ok(Some(true))
        }).await?
    }

    /// Cancel a running task
    pub async fn cancel(&self, task_id: &str) -> Result<bool> {
        self.cancel_with_grace(task_id, std::time::Duration::from_millis(400))
            .await
    }

    /// Cancel a running task, allowing detached processes a configurable grace period
    /// between TERM and KILL on Unix.
    pub async fn cancel_with_grace(
        &self,
        task_id: &str,
        _graceful_timeout: std::time::Duration,
    ) -> Result<bool> {
        if let Some(stopped) = self
            .stop_live_task(task_id, jcode_tool_types::StopCause::HumanCancellation)
            .await?
        {
            Ok(stopped)
        } else {
            let status_path = self.status_path_for(task_id);
            let Some(mut status) = self.read_status_file(&status_path).await else {
                return Ok(false);
            };
            status = self
                .finalize_detached_status_if_needed(status, &status_path)
                .await;
            if status.status != BackgroundTaskStatus::Running || !status.detached {
                return Ok(false);
            }

            let Some(pid) = status.pid else {
                return Ok(false);
            };

            #[cfg(unix)]
            {
                let _ = crate::platform::signal_detached_process_group(pid, libc::SIGTERM);
                tokio::time::sleep(_graceful_timeout).await;
                if crate::platform::is_process_running(pid) {
                    let _ = crate::platform::signal_detached_process_group(pid, libc::SIGKILL);
                }
            }
            #[cfg(windows)]
            {
                let _ = crate::platform::signal_detached_process_group(pid, 0);
            }

            let completed_at = Utc::now();
            status.status = BackgroundTaskStatus::Failed;
            status.exit_code = None;
            status.error = Some("Cancelled by user".to_string());
            status.completed_at = Some(completed_at.to_rfc3339());
            status.duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
            let event_status = status.status.clone();
            let event_exit_code = status.exit_code;
            let event_error = status.error.clone();
            push_task_event(
                &mut status,
                terminal_event_record(event_status, event_exit_code, event_error.as_deref()),
            );
            self.write_status_file(&status_path, &status).await;
            Ok(true)
        }
    }

    /// Abort every live in-process task before an exec-based server reload.
    ///
    /// `exec` replaces the process image without running destructors, so
    /// without this the spawned task futures simply vanish: their
    /// `kill_on_drop` children (e.g. cargo builds) are never killed and keep
    /// running orphaned, and their status files stay `Running` until the next
    /// process's reconcile sweep happens to notice. Aborting the handles here
    /// drops the futures (killing children) and persisting a terminal
    /// `Failed` status makes the interruption deterministic and immediately
    /// visible to `bg wait`/`bg status` and self-dev queue reconciliation.
    ///
    /// Returns the number of tasks finalized.
    pub async fn abort_live_tasks_for_reload(&self) -> Result<usize> {
        let ids: Vec<String> = self.tasks.read().await.keys().cloned().collect();
        let mut stopped = 0;
        let mut failures = Vec::new();
        for id in ids {
            match self
                .stop_live_task(&id, jcode_tool_types::StopCause::ReloadQuiescence)
                .await
            {
                Ok(Some(true)) => stopped += 1,
                Ok(_) => {}
                Err(error) => failures.push(format!("{id}: {error:#}")),
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "Reload cannot replace the runtime while owned work is not quiescent: {}",
            failures.join("; ")
        );
        Ok(stopped)
    }

    /// Clean up old task files (older than specified hours)
    pub async fn cleanup(&self, max_age_hours: u64) -> Result<usize> {
        Ok(self
            .cleanup_filtered(max_age_hours, &std::collections::HashSet::new(), false)
            .await?
            .removed_files)
    }

    /// Clean up old task files, skipping running tasks and optionally filtering by status.
    pub async fn cleanup_filtered(
        &self,
        max_age_hours: u64,
        status_filter: &std::collections::HashSet<&str>,
        dry_run: bool,
    ) -> Result<BackgroundCleanupResult> {
        let mut result = BackgroundCleanupResult {
            matched_files: 0,
            removed_files: 0,
            skipped_running_files: 0,
        };
        let cutoff =
            std::time::SystemTime::now() - std::time::Duration::from_secs(max_age_hours * 3600);

        if let Ok(mut entries) = fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Ok(metadata) = fs::metadata(&path).await else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                if modified >= cutoff {
                    continue;
                }

                let mut associated_status = None;
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    associated_status = self.read_status_file(&path).await;
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("output")
                    && let Some(task_id) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    associated_status = self.status(task_id).await;
                }

                if let Some(status) = associated_status.as_ref() {
                    if status.status == BackgroundTaskStatus::Running {
                        result.skipped_running_files += 1;
                        continue;
                    }
                    let status_label = match status.status {
                        BackgroundTaskStatus::Running => "running",
                        BackgroundTaskStatus::Completed => "completed",
                        BackgroundTaskStatus::Superseded => "superseded",
                        BackgroundTaskStatus::Failed => "failed",
                    };
                    if !status_filter.is_empty() && !status_filter.contains(status_label) {
                        continue;
                    }
                } else if !status_filter.is_empty() {
                    continue;
                }

                result.matched_files += 1;
                if !dry_run {
                    let _ = fs::remove_file(&path).await;
                    result.removed_files += 1;
                }
            }
        }

        if dry_run {
            result.removed_files = result.matched_files;
        }

        Ok(result)
    }

    /// Best-effort synchronous snapshot of currently running tasks.
    /// This avoids async calls in render paths.
    pub fn running_snapshot(&self) -> (usize, Vec<String>, Option<RunningBackgroundProgress>) {
        let Ok(tasks) = self.tasks.try_read() else {
            return (0, Vec::new(), None);
        };

        let mut rows: Vec<RunningBackgroundProgress> = Vec::new();
        for task in tasks.values() {
            let status = std::fs::read_to_string(&task.status_path)
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let progress = status.as_ref().and_then(|status| status.progress.clone());
            let label = status
                .as_ref()
                .and_then(|status| status.display_name.clone())
                .or_else(|| task.display_name.clone())
                .unwrap_or_else(|| task.tool_name.clone());

            rows.push(RunningBackgroundProgress {
                task_id: task.task_id.clone(),
                tool_name: task.tool_name.clone(),
                label,
                detail: progress.map(|progress| format_progress_display(&progress, 10)),
            });
        }

        rows.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        let latest = rows.iter().find(|row| row.detail.is_some()).cloned();

        (
            tasks.len(),
            rows.iter().map(|row| row.label.clone()).collect(),
            latest,
        )
    }

    /// Best-effort synchronous lookup of detached tasks that are still running
    /// for a specific session.
    ///
    /// This is primarily used during self-dev reload recovery, where the new
    /// process needs to remind the agent that a previous `bash` command was
    /// persisted into the background instead of being interrupted.
    pub fn persisted_detached_running_tasks_for_session(
        &self,
        session_id: &str,
    ) -> Vec<TaskStatusFile> {
        let mut matches = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.output_dir) else {
            return matches;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(status) = serde_json::from_str::<TaskStatusFile>(&content) else {
                continue;
            };

            if status.session_id != session_id
                || status.status != BackgroundTaskStatus::Running
                || !status.detached
            {
                continue;
            }

            let Some(pid) = status.pid else {
                continue;
            };

            if crate::platform::is_process_running(pid) {
                matches.push(status);
            }
        }

        matches.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        matches
    }
}

impl Default for BackgroundTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global singleton for background task manager
static BACKGROUND_MANAGER: std::sync::OnceLock<BackgroundTaskManager> = std::sync::OnceLock::new();

/// Get the global background task manager
pub fn global() -> &'static BackgroundTaskManager {
    BACKGROUND_MANAGER.get_or_init(BackgroundTaskManager::new)
}

#[cfg(test)]
mod tests;
