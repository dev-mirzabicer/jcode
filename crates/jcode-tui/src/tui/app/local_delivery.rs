use super::{App, DisplayMessage, ProcessingStatus};
use crate::bus::BackgroundTaskCompleted;
use crate::execution::{DeliveryAttempt, DeliveryChannel, ExecutionStore};
use crate::message::{
    ContentBlock, Message, Role, background_task_status_notice,
    format_background_task_notification_markdown,
};
use crate::session::StoredDisplayRole;
use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct Prepared {
    task: BackgroundTaskCompleted,
    notify: Option<DeliveryAttempt>,
    wake: Option<DeliveryAttempt>,
    displayed: bool,
}
fn acknowledge(attempt: Option<DeliveryAttempt>, delivered: bool) {
    if let Some(attempt) = attempt {
        let finish = move || {
            if attempt.finish(delivered).is_err() {
                crate::logging::warn("Local background delivery acknowledgement failed");
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(finish);
        } else {
            finish();
        }
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        acknowledge(self.notify.take(), false);
        acknowledge(self.wake.take(), false);
    }
}
struct SummaryResult {
    root: std::path::PathBuf,
    result: anyhow::Result<crate::background::RunningBackgroundSnapshot>,
}

pub(super) struct LocalDeliveryState {
    pending: HashSet<String>,
    ready: VecDeque<Prepared>,
    sender: tokio::sync::mpsc::UnboundedSender<(String, anyhow::Result<Prepared>)>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<(String, anyhow::Result<Prepared>)>,
    next_scan: Instant,
    scanning: Arc<AtomicBool>,
    summary: crate::background::RunningBackgroundSnapshot,
    summary_root: Option<std::path::PathBuf>,
    summary_receiver: Option<tokio::sync::oneshot::Receiver<SummaryResult>>,
    next_summary: Instant,
    summary_error: Option<String>,
    summary_notice: Option<String>,
}
impl Default for LocalDeliveryState {
    fn default() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            pending: HashSet::new(),
            ready: VecDeque::new(),
            sender,
            receiver,
            next_scan: Instant::now(),
            scanning: Arc::new(AtomicBool::new(false)),
            summary: Default::default(),
            summary_root: None,
            summary_receiver: None,
            next_summary: Instant::now(),
            summary_error: None,
            summary_notice: None,
        }
    }
}
impl LocalDeliveryState {
    pub(super) fn background_snapshot(&self) -> crate::background::RunningBackgroundSnapshot {
        if crate::storage::jcode_dir().ok().as_ref() != self.summary_root.as_ref() {
            return Default::default();
        }
        let mut summary = self.summary.clone();
        if let Some(error) = &self.summary_error {
            summary.progress = Some(crate::background::RunningBackgroundProgress {
                task_id: String::new(),
                tool_name: "background".into(),
                label: "Last known background activity".into(),
                detail: Some(error.clone()),
            });
        }
        summary
    }

    fn refresh_summary(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &mut self.summary_receiver {
            match receiver.try_recv() {
                Ok(SummaryResult { root, result }) => {
                    self.summary_receiver = None;
                    if crate::storage::jcode_dir().ok().as_ref() == Some(&root) {
                        match result {
                            Ok(summary) => {
                                self.summary = summary;
                                self.summary_root = Some(root);
                                self.summary_error = None;
                                changed = true;
                            }
                            Err(error) => {
                                let message = format!(
                                    "Background summary unavailable; retaining last known metadata: {error:#}"
                                );
                                if self.summary_error.as_ref() != Some(&message) {
                                    self.summary_notice = Some(message.clone());
                                }
                                self.summary_error = Some(message);
                                changed = true;
                            }
                        }
                    }
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.summary_receiver = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.summary_receiver.is_none() && Instant::now() >= self.next_summary {
            self.next_summary = Instant::now() + Duration::from_millis(500);
            if let (Ok(root), Ok(runtime)) = (
                crate::storage::jcode_dir(),
                tokio::runtime::Handle::try_current(),
            ) {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                self.summary_receiver = Some(receiver);
                runtime.spawn_blocking(move || {
                    let result =
                        crate::background::global().running_snapshot_including_managed(&root);
                    let _ = sender.send(SummaryResult { root, result });
                });
            }
        }
        changed
    }
    pub(super) fn submit(&mut self, task: BackgroundTaskCompleted) {
        if !self.pending.insert(task.task_id.clone()) {
            return;
        }
        let id = task.task_id.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = (|| {
                let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
                let record = store
                    .inspect(&id)?
                    .ok_or_else(|| anyhow::anyhow!("Unknown completion invocation"))?;
                anyhow::ensure!(
                    record.session_id == task.session_id,
                    "Completion recipient changed"
                );
                let mut prepared = Prepared {
                    task,
                    notify: None,
                    wake: None,
                    displayed: false,
                };
                if prepared.task.notify {
                    prepared.notify = store.begin_delivery(&id, DeliveryChannel::Notify)?;
                }
                if prepared.task.wake {
                    prepared.wake = store.begin_delivery(&id, DeliveryChannel::Wake)?;
                }
                Ok(prepared)
            })();
            let _ = sender.send((id, result));
        });
    }
}

pub(super) fn drain(app: &mut App) -> bool {
    let summary_changed = app.local_delivery.refresh_summary();
    if let Some(notice) = app.local_delivery.summary_notice.take() {
        app.set_status_notice(notice);
    }
    if Instant::now() >= app.local_delivery.next_scan
        && !app.local_delivery.scanning.swap(true, Ordering::SeqCst)
    {
        app.local_delivery.next_scan = Instant::now() + Duration::from_secs(5);
        let scanning = app.local_delivery.scanning.clone();
        tokio::spawn(async move {
            let _ = crate::background::global().retry_managed_delivery().await;
            scanning.store(false, Ordering::SeqCst);
        });
    }
    while let Ok((id, result)) = app.local_delivery.receiver.try_recv() {
        match result {
            Ok(prepared) if prepared.notify.is_some() || prepared.wake.is_some() => {
                app.local_delivery.ready.push_back(prepared)
            }
            Ok(_) => {
                app.local_delivery.pending.remove(&id);
            }
            Err(error) => {
                app.local_delivery.pending.remove(&id);
                app.set_status_notice(format!("Background delivery remains pending: {error}"));
            }
        }
    }
    let mut redraw = summary_changed;
    let count = app.local_delivery.ready.len();
    for _ in 0..count {
        let mut prepared = app.local_delivery.ready.pop_front().unwrap();
        if prepared.task.session_id != app.session.id {
            app.local_delivery.pending.remove(&prepared.task.task_id);
            continue;
        }
        let notification = format_background_task_notification_markdown(
            &prepared.task,
            app.session.working_dir.as_deref().map(std::path::Path::new),
        );
        if !prepared.displayed {
            if prepared.notify.is_some() {
                app.push_display_message(DisplayMessage::background_task(notification.clone()));
                app.set_status_notice(background_task_status_notice(&prepared.task));
                acknowledge(prepared.notify.take(), true);
                redraw = true;
            }
            prepared.displayed = true;
        }
        if app.is_processing {
            app.local_delivery.ready.push_back(prepared);
            continue;
        }
        let content = vec![ContentBlock::Text {
            text: notification,
            cache_control: None,
        }];
        let mut session = app.session.clone();
        session.add_message_with_display_role(
            Role::User,
            content.clone(),
            Some(StoredDisplayRole::BackgroundTask),
        );
        if let Err(error) = session.save() {
            app.set_status_notice(format!(
                "Background completion was not added to history: {error}"
            ));
            app.local_delivery.pending.remove(&prepared.task.task_id);
            continue;
        }
        app.session = session;
        app.add_provider_message(Message {
            role: Role::User,
            content,
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        });
        if prepared.wake.is_some() {
            app.pending_turn = true;
            app.is_processing = true;
            app.status = ProcessingStatus::Sending;
            app.processing_started.get_or_insert_with(Instant::now);
            app.visible_turn_started = Some(Instant::now());
        }
        acknowledge(prepared.wake.take(), true);
        app.local_delivery.pending.remove(&prepared.task.task_id);
        redraw = true;
    }
    redraw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_background_summary_refreshes_managed_metadata_without_render_io() -> anyhow::Result<()>
    {
        let _guard = crate::storage::lock_test_env();
        let root = tempfile::tempdir()?;
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
        let invocation = crate::execution::Invocation {
            session_id: "summary-session".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "summary-fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let crate::execution::PreparedInvocation::New(mut record) =
            store.prepare(&invocation, "fixture")?
        else {
            panic!()
        };
        store.start(&record.id, "fixture")?;
        store.promote(&record.id, "fixture")?;
        store.register_background_delivery(&record.id, false, false)?;
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(async {
                let mut state = LocalDeliveryState::default();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !state.refresh_summary() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?;
                let snapshot = state.background_snapshot();
                assert_eq!(snapshot.count, 1);
                assert_eq!(snapshot.labels, ["summary-fixture"]);
                state.next_summary = Instant::now() + Duration::from_secs(60);
                let (sender, receiver) = tokio::sync::oneshot::channel();
                state.summary_receiver = Some(receiver);
                sender
                    .send(SummaryResult {
                        root: root.path().join("other"),
                        result: Ok(crate::background::RunningBackgroundSnapshot {
                            count: 99,
                            labels: Vec::new(),
                            progress: None,
                        }),
                    })
                    .ok();
                state.refresh_summary();
                assert_eq!(
                    state.background_snapshot().count,
                    1,
                    "A stale namespace reply cannot replace the cached view"
                );
                let (sender, receiver) = tokio::sync::oneshot::channel();
                state.summary_receiver = Some(receiver);
                sender
                    .send(SummaryResult {
                        root: root.path().into(),
                        result: Err(anyhow::anyhow!("fixture unavailable")),
                    })
                    .ok();
                state.refresh_summary();
                assert!(state.summary_notice.is_some());
                assert_eq!(state.background_snapshot().count, 1);
                assert!(
                    state
                        .background_snapshot()
                        .progress
                        .unwrap()
                        .label
                        .contains("Last known")
                );
                record.state = crate::execution::RunState::Completed;
                store.finish(&record)?;
                state.next_summary = Instant::now();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !state.refresh_summary() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?;
                assert_eq!(state.background_snapshot().count, 0);
                assert!(state.summary_error.is_none());
                Ok(())
            })
    }
    use crate::execution::{DeliveryState, Invocation, PreparedInvocation, RunState};
    fn completion(app: &App, store: &ExecutionStore) -> anyhow::Result<BackgroundTaskCompleted> {
        let input = Invocation {
            session_id: app.session.id.clone(),
            message_id: "message".into(),
            call_path: vec![crate::id::new_id("delivery-test")],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(mut record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        store.promote(&record.id, "owner")?;
        store.register_background_delivery(&record.id, true, true)?;
        record.state = RunState::Completed;
        store.finish(&record)?;
        Ok(BackgroundTaskCompleted {
            task_id: record.id,
            tool_name: "fixture".into(),
            display_name: None,
            session_id: app.session.id.clone(),
            status: crate::bus::BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            output_preview: "synthetic result".into(),
            output_file: store.root().join("fixture.output"),
            duration_secs: 1.0,
            notify: true,
            wake: true,
        })
    }
    #[test]
    fn local_managed_delivery_waits_for_idle_and_never_replays_into_another_session()
    -> anyhow::Result<()> {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir()?;
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            for switch in [false, true] {
                let mut app = tokio::task::block_in_place(crate::tui::app::tests::create_test_app);
                app.local_delivery.next_scan = Instant::now() + Duration::from_secs(60);
                let store = ExecutionStore::open(home.path())?;
                let event = completion(&app, &store)?;
                let id = event.task_id.clone();
                let messages = app.session.messages.len();
                app.is_processing = true;
                app.local_delivery.submit(event.clone());
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.local_delivery.ready.is_empty() {
                    drain(&mut app);
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "Local completion was not prepared"
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert_eq!(app.session.messages.len(), messages);
                if switch {
                    app.session.id = crate::id::new_id("delivery-test");
                }
                app.is_processing = false;
                drain(&mut app);
                if switch {
                    assert_eq!(app.session.messages.len(), messages);
                    assert!(!app.pending_turn);
                } else {
                    assert_eq!(app.session.messages.len(), messages + 1);
                    assert!(app.pending_turn);
                }
                loop {
                    let receipt = store.background_delivery(&id)?.unwrap();
                    let expected = if switch {
                        DeliveryState::Pending
                    } else {
                        DeliveryState::Delivered
                    };
                    if receipt.wake_state == expected {
                        break;
                    }
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "Local delivery acknowledgement did not settle"
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if !switch {
                    app.local_delivery.submit(event);
                    while app.local_delivery.pending.contains(&id) {
                        drain(&mut app);
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "Duplicate receipt did not settle"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    assert_eq!(app.session.messages.len(), messages + 1);
                }
            }
            Ok::<_, anyhow::Error>(())
        });
        if let Some(value) = previous {
            crate::env::set_var("JCODE_HOME", value);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        result
    }
}
