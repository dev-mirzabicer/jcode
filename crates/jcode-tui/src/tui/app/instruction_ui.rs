use super::App;
use crate::instruction::inspection::{InspectionContext, InspectionWorker};
use crate::protocol::{InstructionInspectionReply, InstructionInspectionRequest};
use crate::tui::instruction_manager::editing::local_recovery::{
    LocalRecoveryRequest, LocalRecoveryRow, LocalSnapshot, RecoveryStore,
};
use crate::tui::{backend::RemoteConnection, instruction_manager::InstructionManager};
use crossterm::event::{KeyCode, KeyModifiers};
use std::cell::RefCell;
enum LocalRecoveryOutput {
    Exported(String),
    Archived(String),
    Listed(Vec<LocalRecoveryRow>),
    Loaded(Box<LocalSnapshot>),
}

#[derive(Default)]
pub(super) struct InstructionUi {
    pub manager: Option<RefCell<InstructionManager>>,
    worker: InspectionWorker,
    receiver: Option<(
        u64,
        tokio::sync::oneshot::Receiver<InstructionInspectionReply>,
    )>,
    next_id: u64,
    management: crate::instruction::management::InstructionManagementWorker,
    management_receiver: Option<(
        u64,
        tokio::sync::oneshot::Receiver<crate::protocol::InstructionManagementReply>,
    )>,
    recovery: Option<RecoveryStore>,
    recovery_generation: u64,
    had_local_intent: bool,
    recovery_redraw: bool,
    local_recovery_receiver: Option<(
        String,
        tokio::sync::oneshot::Receiver<Result<LocalRecoveryOutput, String>>,
    )>,
}

impl App {
    pub(super) fn handle_instruction_command(&mut self, command: &str) -> bool {
        let kind = match command {
            "/instructions" | "/prompts" => None,
            "/model-roster" => Some("model-roster"),
            "/agent instructions" => Some("agent"),
            "/skills instructions" => Some("skill"),
            "/swarm-prompt inspect" => Some("tool-guidance"),
            _ => return false,
        };
        let session = self
            .remote_session_id
            .clone()
            .unwrap_or_else(|| self.session.id.clone());
        let retained_editing = self.instruction_ui.manager.as_ref().and_then(|existing| {
            let mut existing = existing.borrow_mut();
            (existing.session == session && !existing.visible)
                .then(|| std::mem::take(&mut existing.editing))
        });
        let mut manager = InstructionManager::new(session, kind == Some("model-roster"));
        if let Some(editing) = retained_editing {
            manager.editing = editing;
        }
        manager.filter.kind = kind.map(str::to_string);
        manager.queued = Some(InstructionInspectionRequest::Open {
            filter: manager.filter.clone(),
        });
        self.instruction_ui.manager = Some(RefCell::new(manager));
        self.input.clear();
        self.cursor_pos = 0;
        true
    }

    pub(super) fn instruction_manager_visible(&self) -> bool {
        self.instruction_ui
            .manager
            .as_ref()
            .is_some_and(|manager| manager.borrow().visible)
    }

    pub(super) fn handle_instruction_paste(&mut self, text: &str) -> bool {
        let Some(manager) = &self.instruction_ui.manager else {
            return false;
        };
        manager.borrow_mut().paste(text)
    }

    pub(super) fn handle_instruction_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        self.instruction_ui
            .manager
            .as_ref()
            .is_some_and(|manager| manager.borrow_mut().key(code, modifiers))
    }

    pub(super) fn reconnect_instruction_manager(&mut self, session: &str) {
        if let Some(manager) = &self.instruction_ui.manager {
            let mut manager = manager.borrow_mut();
            if manager.session == session {
                manager.suspend_editing_connection(&self.remote_client_instance_id);
            } else {
                if let Some(store) = &self.instruction_ui.recovery {
                    store.schedule(
                        &manager.session,
                        LocalSnapshot::capture(&manager, &self.remote_client_instance_id),
                    );
                }
                manager.editing = Default::default();
            }
            if manager.visible {
                manager.refresh(session);
            }
        }
    }

    pub(super) fn accept_instruction_reply(
        &mut self,
        id: u64,
        reply: InstructionInspectionReply,
    ) -> bool {
        self.instruction_ui
            .manager
            .as_ref()
            .is_some_and(|manager| manager.borrow_mut().accept(id, reply))
    }

    pub(super) fn dispatch_local_instruction_request(&mut self) -> bool {
        self.prepare_instruction_recovery();
        let mut changed =
            self.take_instruction_recovery_redraw() | self.dispatch_local_instruction_management();
        if let Some((id, receiver)) = self.instruction_ui.receiver.as_mut() {
            match receiver.try_recv() {
                Ok(reply) => {
                    let id = *id;
                    self.instruction_ui.receiver = None;
                    changed |= self.accept_instruction_reply(id, reply);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.instruction_ui.receiver = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let Some(manager) = &self.instruction_ui.manager else {
            return changed;
        };
        if manager.borrow().queued.is_none() {
            return changed;
        }
        self.instruction_ui.next_id = self.instruction_ui.next_id.wrapping_add(1).max(1);
        let id = self.instruction_ui.next_id;
        let Some(request) = manager.borrow_mut().reserve(id) else {
            return changed;
        };
        let context = InspectionContext::from_session(
            &self.session,
            self.provider.as_ref(),
            self.session.is_canary,
        );
        let session_id = context.session_id.clone();
        let receiver = self.instruction_ui.worker.submit(
            crate::instruction::InstructionRepositoryService::new(),
            session_id,
            move || Ok(context),
            request,
        );
        self.instruction_ui.receiver = Some((id, receiver));
        true
    }

    pub(super) async fn dispatch_remote_instruction_request(
        &mut self,
        remote: &mut RemoteConnection,
    ) {
        self.prepare_instruction_recovery();
        self.dispatch_remote_instruction_management(remote).await;
        let Some(manager) = &self.instruction_ui.manager else {
            return;
        };
        if manager.borrow().queued.is_none() {
            return;
        }
        let id = remote.reserve_context_request_id();
        let request = manager.borrow_mut().reserve(id);
        let Some(request) = request else {
            return;
        };
        if let Err(error) = remote.send_instruction_inspection(id, request).await
            && let Some(manager) = &self.instruction_ui.manager
        {
            let mut manager = manager.borrow_mut();
            if manager
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == id)
            {
                manager.pending = None;
                manager.status =
                    format!("Instruction transport unavailable: {error}. Reconnect or R to retry.");
            }
        }
    }

    pub(super) fn instruction_debug(&self) -> serde_json::Value {
        self.instruction_ui.manager.as_ref().map_or_else(
            || serde_json::json!({"visible":false}),
            |manager| manager.borrow().debug(),
        )
    }

    pub(super) fn accept_instruction_management_reply(
        &mut self,
        id: u64,
        reply: crate::protocol::InstructionManagementReply,
    ) -> bool {
        let closed = matches!(
            reply.result,
            crate::protocol::InstructionManagementResult::Closed
                | crate::protocol::InstructionManagementResult::Discarded
        );
        let Some(manager) = &self.instruction_ui.manager else {
            return false;
        };
        let mut manager = manager.borrow_mut();
        let accepted = manager.accept_management(id, reply);
        if accepted && closed && manager.visible {
            let session = manager.session.clone();
            manager.refresh(&session);
        }
        accepted
    }

    fn dispatch_local_instruction_management(&mut self) -> bool {
        let mut changed = false;
        if let Some((id, receiver)) = self.instruction_ui.management_receiver.as_mut() {
            match receiver.try_recv() {
                Ok(reply) => {
                    let id = *id;
                    self.instruction_ui.management_receiver = None;
                    changed |= self.accept_instruction_management_reply(id, reply);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.instruction_ui.management_receiver = None;
                    if let Some(manager) = &self.instruction_ui.manager {
                        let mut manager = manager.borrow_mut();
                        manager.editing.pending = None;
                        manager.editing.status = "Management worker stopped. Draft retained; recover its receipt before retrying.".into();
                    }
                    changed = true;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if !self.prepare_instruction_recovery() {
            return changed;
        }
        let Some(manager) = &self.instruction_ui.manager else {
            return changed;
        };
        if manager.borrow().editing.queued.is_none() {
            return changed;
        }
        self.instruction_ui.next_id = self.instruction_ui.next_id.wrapping_add(1).max(1);
        let id = self.instruction_ui.next_id;
        let session = manager.borrow().session.clone();
        let Some(request) = manager.borrow_mut().editing.reserve(id, &session) else {
            return changed;
        };
        let context = InspectionContext::from_session(
            &self.session,
            self.provider.as_ref(),
            self.session.is_canary,
        );
        let receiver = self.instruction_ui.management.submit(
            crate::instruction::InstructionRepositoryService::new(),
            context,
            self.instruction_ui.worker.target_resolver(),
            request,
        );
        self.instruction_ui.management_receiver = Some((id, receiver));
        true
    }

    async fn dispatch_remote_instruction_management(&mut self, remote: &mut RemoteConnection) {
        if !self.prepare_instruction_recovery() {
            return;
        }
        let Some(manager) = &self.instruction_ui.manager else {
            return;
        };
        if manager.borrow().editing.queued.is_none() {
            return;
        }
        let id = remote.reserve_context_request_id();
        let session = manager.borrow().session.clone();
        let request = manager.borrow_mut().editing.reserve(id, &session);
        let Some(request) = request else { return };
        if let Err(error) = remote.send_instruction_management(id, request).await {
            let mut manager = manager.borrow_mut();
            if manager
                .editing
                .pending
                .as_ref()
                .is_some_and(|(pending, _, _)| *pending == id)
            {
                manager.editing.pending = None;
                manager.editing.status = format!(
                    "Transport failed: {error}. The draft is retained; recover before retrying a Save."
                );
            }
        }
    }

    pub(super) fn run_pending_instruction_editor(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        events: &mut Option<crossterm::event::EventStream>,
    ) -> bool {
        let Some(manager) = &self.instruction_ui.manager else {
            return false;
        };
        let request = manager.borrow_mut().editing.editor.take();
        let Some(request) = request else { return false };
        let directory = crate::storage::durable_state_dir().join("instruction-editor");
        let result = crate::tui::instruction_manager::editing::external::run(
            terminal, events, &directory, &request,
        );
        let mut manager = manager.borrow_mut();
        manager.editing.wrapped.clear();
        manager.editing.recovery_dirty = true;
        match result {
            Ok((path, Ok(body))) => {
                manager.editing.status = format!(
                    "Editor returned. Local draft: {}. Source is unchanged until reviewed Save.",
                    path.display()
                );
                if let Some(index) = request.metadata_field {
                    manager.editing.set_external_metadata_value(index, body);
                    return true;
                }
                let change = if request.repair {
                    crate::protocol::InstructionDraftChange::RepairSource {
                        file: request.file,
                        source: body,
                    }
                } else {
                    crate::protocol::InstructionDraftChange::Body {
                        file: request.file,
                        body,
                    }
                };
                manager.editing.queued =
                    Some(crate::protocol::InstructionManagementRequest::Update {
                        draft: request.draft,
                        generation: request.generation,
                        change,
                    });
            }
            Ok((path, Err(error))) => {
                manager.editing.status = format!("{error:#}\nDraft retained: {}", path.display());
                manager.editing.document = manager.editing.status.clone();
                manager.editing.failed = true;
            }
            Err(error) => {
                manager.editing.status =
                    format!("Editor preparation failed: {error:#}. Original source is unchanged.");
                manager.editing.document = manager.editing.status.clone();
                manager.editing.failed = true;
            }
        }
        true
    }
    fn prepare_instruction_recovery(&mut self) -> bool {
        if let Some((expected_session, receiver)) = &mut self.instruction_ui.local_recovery_receiver
        {
            match receiver.try_recv() {
                Ok(result) => {
                    let expected_session = expected_session.clone();
                    self.instruction_ui.local_recovery_receiver = None;
                    self.instruction_ui.recovery_redraw = true;
                    if let Some(manager) = &self.instruction_ui.manager {
                        let mut manager = manager.borrow_mut();
                        manager.editing.local_loading = false;
                        if manager.session != expected_session {
                            return true;
                        }
                        match result {
                            Ok(LocalRecoveryOutput::Exported(path)) => {
                                manager.editing.document = format!(
                                    "EXPORTED REVISION\n\nComplete historical bytes were written to a new client-local directory:\n{path}\n\nNo source or Git history changed."
                                );
                                manager.editing.status = "Revision export completed.".into();
                                manager.editing.visible = true;
                                manager.editing.wrapped.clear();
                            }
                            Ok(LocalRecoveryOutput::Archived(key)) => {
                                manager.archived_local_intent(key)
                            }
                            Ok(LocalRecoveryOutput::Listed(rows)) => {
                                manager.open_local_recovery_menu(rows)
                            }
                            Ok(LocalRecoveryOutput::Loaded(snapshot)) => {
                                manager.restore_local_intent(*snapshot)
                            }
                            Err(error) => {
                                manager.editing.status = error;
                                manager.editing.failed = true;
                                manager.editing.archiving = false;
                            }
                        }
                    }
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.instruction_ui.local_recovery_receiver = None;
                    if let Some(manager) = &self.instruction_ui.manager {
                        let mut manager = manager.borrow_mut();
                        manager.editing.local_loading = false;
                        manager.editing.archiving = false;
                        manager.editing.status="Local operation stopped before returning a receipt. Source was not replayed. Inspect retained state and retry explicitly.".into();
                    }
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let Some(manager) = &self.instruction_ui.manager else {
            return true;
        };
        let mut manager = manager.borrow_mut();
        let session = manager.session.clone();
        if manager.editing.storage_blocked {
            return false;
        }
        if manager.editing.queued.is_some() && !manager.editing.request_preserved {
            manager.editing.recovery_dirty = true;
            manager.editing.request_preserved = true;
        }
        let store = self.instruction_ui.recovery.get_or_insert_with(|| {
            RecoveryStore::new(
                crate::storage::durable_state_dir(),
                self.remote_client_instance_id.clone(),
            )
        });
        if let Some(export) = manager.editing.export.take() {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let root = crate::storage::durable_state_dir();
            tokio::task::spawn_blocking(move || {
                let result = crate::tui::instruction_manager::editing::export::write_revision(&root, export).map(|path| LocalRecoveryOutput::Exported(path.display().to_string())).map_err(|error| format!("Revision export failed: {error:#}. Source remains unchanged; export the same revision again after repair."));
                let _ = sender.send(result);
            });
            self.instruction_ui.local_recovery_receiver = Some((session.clone(), receiver));
        }
        if let Some(key) = manager.editing.local_request.take() {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let store = store.clone();
            let task_session = session.clone();
            let archive = LocalSnapshot::capture(&manager, &self.remote_client_instance_id);
            tokio::task::spawn_blocking(move || {
                let result = match key {
                    LocalRecoveryRequest::Load(key) => store
                        .load(&task_session, &key)
                        .map(Box::new)
                        .map(LocalRecoveryOutput::Loaded),
                    LocalRecoveryRequest::List => {
                        store.list(&task_session).map(LocalRecoveryOutput::Listed)
                    }
                    LocalRecoveryRequest::Archive => archive
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("No local intent to preserve"))
                        .and_then(|snapshot| store.archive(snapshot))
                        .map(LocalRecoveryOutput::Archived),
                };
                let _ = sender
                    .send(result.map_err(|error| format!("Local recovery failed: {error:#}")));
            });
            self.instruction_ui.local_recovery_receiver = Some((session.clone(), receiver));
        }
        if manager.editing.recovery_dirty {
            let snapshot = LocalSnapshot::capture(&manager, &self.remote_client_instance_id);
            let needed = snapshot.is_some();
            if needed || self.instruction_ui.had_local_intent {
                self.instruction_ui.recovery_generation = store.schedule(&session, snapshot);
                self.instruction_ui.had_local_intent = needed;
            }
            manager.editing.recovery_dirty = false;
        }
        match store.ready(&session, self.instruction_ui.recovery_generation) {
            Ok(ready) => ready,
            Err(error) => {
                manager.editing.status = error.to_string();
                manager.editing.failed = true;
                manager.editing.storage_blocked = true;
                false
            }
        }
    }

    pub(super) fn take_instruction_recovery_redraw(&mut self) -> bool {
        std::mem::take(&mut self.instruction_ui.recovery_redraw)
    }

    pub(super) fn flush_instruction_recovery(&self) {
        let Some(manager) = &self.instruction_ui.manager else {
            return;
        };
        let manager = manager.borrow();
        let snapshot = LocalSnapshot::capture(&manager, &self.remote_client_instance_id);
        let result = match &self.instruction_ui.recovery {
            Some(store) => store.flush(&manager.session, snapshot),
            None if snapshot.is_some() => RecoveryStore::new(
                crate::storage::durable_state_dir(),
                self.remote_client_instance_id.clone(),
            )
            .flush(&manager.session, snapshot),
            None => return,
        };
        if let Err(error) = result {
            crate::logging::error(&format!(
                "Instruction client recovery flush failed: {error:#}"
            ));
        }
    }
}
