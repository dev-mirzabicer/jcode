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
        let mut changed = false;
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
}
