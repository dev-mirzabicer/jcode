use super::App;
use crate::instruction::inspection::{InspectionContext, InspectionWorker};
use crate::protocol::{InstructionInspectionReply, InstructionInspectionRequest};
use crate::tui::{backend::RemoteConnection, instruction_manager::InstructionManager};
use crossterm::event::{KeyCode, KeyModifiers};
use std::cell::RefCell;

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
        let mut manager = InstructionManager::new(session, kind == Some("model-roster"));
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
        if let Some(manager) = &self.instruction_ui.manager
            && manager.borrow().visible
        {
            manager.borrow_mut().refresh(session);
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
        let mut changed = self.dispatch_local_instruction_management();
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
        let accepted = manager.editing.accept(id, reply);
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
        match result {
            Ok((path, Ok(body))) => {
                manager.editing.status = format!(
                    "Editor returned. Local draft: {}. Source is unchanged until reviewed Save.",
                    path.display()
                );
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
}
