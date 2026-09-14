use super::{App, context_protocol::ContextProtocolState};
use crate::protocol::{Request, ServerEvent};
use crate::tui::{backend::RemoteConnection, task_monitor::TaskMonitor};
use crossterm::event::{KeyCode, KeyModifiers};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct TaskUi {
    pub monitor: Option<RefCell<TaskMonitor>>,
    receivers: HashMap<u64, oneshot::Receiver<ServerEvent>>,
    pub child: Option<ChildEditor>,
    owned: HashSet<u64>,
    completion_receipts: HashSet<u64>,
}
pub(super) struct ChildEditor {
    pub target: String,
    pub protocol: ContextProtocolState,
    pub pending: HashMap<
        u64,
        (
            crate::protocol::ContextRequestKind,
            Option<String>,
            Option<String>,
        ),
    >,
}
impl App {
    pub(super) fn handle_task_command(&mut self, command: &str) -> bool {
        if command != "/tasks" {
            return false;
        }
        self.task_ui.monitor = Some(RefCell::new(TaskMonitor::new(
            self.remote_session_id
                .clone()
                .unwrap_or_else(|| self.session.id.clone()),
            self.remote_session_id.is_some(),
        )));
        self.force_full_redraw = true;
        true
    }
    pub(super) fn handle_task_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let handled = self
            .task_ui
            .monitor
            .as_ref()
            .is_some_and(|monitor| monitor.borrow_mut().key(code, modifiers));
        if handled {
            self.open_requested_child_context();
            self.force_full_redraw = true;
        }
        handled
    }
    pub(super) fn open_requested_child_context(&mut self) {
        let target = self
            .task_ui
            .monitor
            .as_ref()
            .and_then(|monitor| monitor.borrow_mut().child_context.take());
        if let Some(target) = target {
            let primary = std::mem::take(&mut self.context_protocol);
            self.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
            if let Some(editor) = &self.context_editor_overlay {
                editor.borrow_mut().set_child_target(&target);
            }
            let protocol = std::mem::replace(&mut self.context_protocol, primary);
            self.task_ui.child = Some(ChildEditor {
                target,
                protocol,
                pending: HashMap::new(),
            });
        }
    }
    pub(super) fn reconnect_task_monitor(&mut self, session: &str) {
        if let Some(monitor) = &self.task_ui.monitor {
            monitor.borrow_mut().reconnect(session);
        }
        self.task_ui.owned.clear();
        self.task_ui.completion_receipts.clear();
        if self.task_ui.child.is_some() {
            self.swap_child_protocol();
            let pending = self
                .task_ui
                .child
                .as_mut()
                .map(|child| std::mem::take(&mut child.pending))
                .unwrap_or_default();
            for (id, (kind, draft, transaction)) in pending {
                self.context_protocol.accept_rejection(id,kind,draft,transaction,crate::protocol::ContextServiceError::Runtime("Connection changed. Inspect current context before retrying; no mutation was resent.".into()));
            }
            self.sync_context_editor_from_protocol();
            self.swap_child_protocol();
            self.context_editor_actions.clear();
            self.context_editor_actions.push_back(
                crate::tui::context_editor::ContextEditorAction::LoadSnapshot {
                    page_start: 0,
                    page_size: 250,
                },
            );
        }
    }
    pub(super) fn swap_child_protocol(&mut self) {
        if let Some(child) = &mut self.task_ui.child {
            std::mem::swap(&mut self.context_protocol, &mut child.protocol);
        }
    }
    pub(super) fn fail_child_context_request(&mut self, id: u64, message: String) {
        if let Some((kind, draft, transaction)) = self
            .task_ui
            .child
            .as_mut()
            .and_then(|child| child.pending.remove(&id))
        {
            self.context_protocol.accept_rejection(
                id,
                kind,
                draft,
                transaction,
                crate::protocol::ContextServiceError::Runtime(message),
            );
            self.sync_context_editor_from_protocol();
        }
    }
    pub(super) fn reduce_task_event(
        &mut self,
        event: ServerEvent,
    ) -> Result<bool, Box<ServerEvent>> {
        if let ServerEvent::Done { id } = &event
            && self.task_ui.completion_receipts.remove(id)
        {
            return Ok(false);
        }
        let id = match &event {
            ServerEvent::TaskMonitorCapabilities { id, .. }
            | ServerEvent::TaskMonitorResponse { id, .. }
            | ServerEvent::ExecutionResponse { id, .. }
            | ServerEvent::OutputCleanupResponse { id, .. }
            | ServerEvent::Error { id, .. } => *id,
            ServerEvent::ChildContextResponse { id, child_id, .. } => {
                let id = *id;
                if self.task_ui.child.as_ref().is_none_or(|child| {
                    child.target != *child_id || !child.pending.contains_key(&id)
                }) {
                    return Ok(false);
                }
                let ServerEvent::ChildContextResponse { event, .. } = event else {
                    unreachable!()
                };
                let terminal = !matches!(
                    &*event,
                    ServerEvent::ContextDraftProgress { .. }
                        | ServerEvent::ContextDraftApplying { .. }
                );
                self.swap_child_protocol();
                let accepted = if let ServerEvent::Error { message, .. } = *event {
                    self.fail_child_context_request(id, message);
                    true
                } else {
                    self.reduce_context_server_event_inner(*event, true)
                        .unwrap_or(false)
                };
                self.swap_child_protocol();
                if terminal && let Some(child) = &mut self.task_ui.child {
                    child.pending.remove(&id);
                }
                return Ok(accepted);
            }
            _ => return Err(Box::new(event)),
        };
        let owned = self.task_ui.owned.remove(&id);
        let Some(monitor) = &self.task_ui.monitor else {
            return if owned {
                Ok(false)
            } else {
                Err(Box::new(event))
            };
        };
        if !monitor.borrow().accepts_id(id) {
            return if owned {
                Ok(false)
            } else {
                Err(Box::new(event))
            };
        }
        Ok(monitor.borrow_mut().accept(id, event))
    }
    pub(super) fn dispatch_local_task_requests(&mut self) -> bool {
        let mut changed = false;
        let ids = self.task_ui.receivers.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let result = self.task_ui.receivers.get_mut(&id).unwrap().try_recv();
            let event = match result {
                Ok(event) => event,
                Err(oneshot::error::TryRecvError::Empty) => continue,
                Err(oneshot::error::TryRecvError::Closed) => ServerEvent::Error {
                    id,
                    message:
                        "Task inspection worker disconnected. Refresh to inspect current state."
                            .into(),
                    retry_after_secs: None,
                },
            };
            self.task_ui.receivers.remove(&id);
            changed |= self.reduce_task_event(event).unwrap_or(false);
        }
        self.open_requested_child_context();
        if let Some(monitor) = &self.task_ui.monitor {
            monitor.borrow_mut().tick();
        }
        while self
            .task_ui
            .monitor
            .as_ref()
            .is_some_and(|m| !m.borrow().queued.is_empty())
        {
            let id = self.next_local_context_request_id();
            let request = self
                .task_ui
                .monitor
                .as_ref()
                .unwrap()
                .borrow_mut()
                .reserve(id)
                .unwrap();
            self.task_ui.owned.insert(id);
            if matches!(
                &request,
                Request::Execution { .. } | Request::OutputCleanup { .. }
            ) {
                self.task_ui.completion_receipts.insert(id);
            }
            self.task_ui.completion_receipts.remove(&id);
            let session = self
                .task_ui
                .monitor
                .as_ref()
                .unwrap()
                .borrow()
                .session
                .clone();
            let (tx, rx) = oneshot::channel();
            self.task_ui.receivers.insert(id, rx);
            tokio::spawn(async move {
                let event = local_request(session, request)
                    .await
                    .unwrap_or_else(|error| ServerEvent::Error {
                        id,
                        message: format!("Task monitor: {error:#}"),
                        retry_after_secs: None,
                    });
                let _ = tx.send(event);
            });
            changed = true;
        }
        changed
    }
    pub(super) async fn dispatch_remote_task_requests(&mut self, remote: &mut RemoteConnection) {
        self.open_requested_child_context();
        if let Some(monitor) = &self.task_ui.monitor {
            monitor.borrow_mut().tick();
        }
        while self
            .task_ui
            .monitor
            .as_ref()
            .is_some_and(|m| !m.borrow().queued.is_empty())
        {
            let id = remote.reserve_context_request_id();
            let request = self
                .task_ui
                .monitor
                .as_ref()
                .unwrap()
                .borrow_mut()
                .reserve(id)
                .unwrap();
            self.task_ui.owned.insert(id);
            if matches!(
                &request,
                Request::Execution { .. } | Request::OutputCleanup { .. }
            ) {
                self.task_ui.completion_receipts.insert(id);
            }
            if let Err(error) = remote.send_reserved_task_request(request).await {
                let _ = self.reduce_task_event(ServerEvent::Error {
                    id,
                    message: format!("Task transport unavailable: {error}"),
                    retry_after_secs: None,
                });
                break;
            }
        }
    }
}
async fn local_request(session: String, request: Request) -> anyhow::Result<ServerEvent> {
    let root = crate::storage::jcode_dir()?;
    Ok(match request {
        Request::TaskMonitor { id, request } => ServerEvent::TaskMonitorResponse {
            id,
            response: crate::execution::task_monitor::inspect(&root, &session, request).await?,
        },
        Request::Execution { id, request } => ServerEvent::ExecutionResponse {
            id,
            response: crate::execution::inspection::inspect(&root, &session, request).await?,
        },
        Request::TaskMonitorProbe { id } => ServerEvent::TaskMonitorCapabilities {
            id,
            version: 1,
            child_context: true,
        },
        Request::OutputCleanup { id, request } => {
            let response = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                use jcode_tool_types::cleanup::{CleanupRequest, CleanupResponse};
                let store = crate::execution::ExecutionStore::open(&root)?;
                let now = chrono::Utc::now().timestamp();
                Ok(match request {
                    CleanupRequest::Status => CleanupResponse::Status {
                        status: store.retention_status()?,
                    },
                    CleanupRequest::Review { selection } => CleanupResponse::Review {
                        review: store.review_output_cleanup(&session, selection, now)?,
                    },
                    CleanupRequest::Confirm {
                        review_id,
                        confirmation_id,
                    } => CleanupResponse::Outcome {
                        outcome: store.confirm_output_cleanup(
                            &session,
                            &review_id,
                            &confirmation_id,
                            now,
                        )?,
                    },
                })
            })
            .await??;
            ServerEvent::OutputCleanupResponse { id, response }
        }
        _ => anyhow::bail!("Unsupported local task request"),
    })
}
