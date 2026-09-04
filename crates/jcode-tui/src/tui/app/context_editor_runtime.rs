use super::App;
use crate::context::ContextDraftRuntimeInput;
use crate::protocol::{
    ContextDraftStatus, ContextRequestKind, ContextServiceError, ContextTransactionDetail, Request,
    ServerEvent,
};
use crate::tui::backend::RemoteConnection;
use crate::tui::context_editor::{ContextEditor, ContextEditorAction, ContextEditorOpenMode};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};
use jcode_tui_messages::DisplayMessage;
use std::cell::RefCell;
use std::sync::Arc;

pub(super) struct PreparedRemoteContextRequest {
    pub(super) id: u64,
    request: Request,
    request_kind: ContextRequestKind,
    draft_id: Option<String>,
    transaction_id: Option<String>,
}

impl App {
    pub(super) fn handle_context_editor_command(&mut self, command: &str) -> bool {
        if command == "/startup" {
            self.open_startup_context_details();
            return true;
        }
        if command.starts_with("/startup ") {
            self.push_display_message(DisplayMessage::error(
                "Usage: /startup. This opens the Startup Context browser, ordered draft, preview, and receipt editor."
                    .to_string(),
            ));
            return true;
        }
        let mode = match command {
            "/compact" | "/context edit" => Some(ContextEditorOpenMode::Edit),
            "/context history" => Some(ContextEditorOpenMode::History),
            "/context restore" => Some(ContextEditorOpenMode::Restore),
            "/context undo" => Some(ContextEditorOpenMode::UndoLatest),
            _ => None,
        };
        if let Some(mode) = mode {
            self.open_context_editor(mode);
            return true;
        }
        if command == "/compact mode"
            || command == "/compact mode status"
            || command.starts_with("/compact mode ")
        {
            self.push_display_message(DisplayMessage::system(
                "Compaction modes are obsolete. Use /compact or /context edit to review and apply one explicit context transaction."
                    .to_string(),
            ));
            return true;
        }
        if command.starts_with("/compact ") {
            self.push_display_message(DisplayMessage::error(
                "Usage: /compact. This opens the Context Editor and never starts an automatic rewrite."
                    .to_string(),
            ));
            return true;
        }
        if command.starts_with("/context ") {
            self.push_display_message(DisplayMessage::error(
                "Usage: /context, /context edit, /context history, /context restore, or /context undo."
                    .to_string(),
            ));
            return true;
        }
        false
    }

    pub(super) fn open_context_editor(&mut self, mode: ContextEditorOpenMode) {
        let epoch = self.context_protocol.begin_editor_epoch();
        let editor = ContextEditor::new_for_protocol_epoch(mode, epoch);
        let initial_action = editor.initial_action();
        self.context_editor_overlay = Some(RefCell::new(editor));
        self.context_editor_actions.clear();
        self.context_editor_actions.push_back(initial_action);
        self.force_full_redraw = true;
    }

    pub(super) fn handle_context_editor_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        let Some(editor_cell) = self.context_editor_overlay.as_ref() else {
            return false;
        };
        let (close, action, epoch) = {
            let mut editor = editor_cell.borrow_mut();
            let (close, action) = editor.handle_key(code, modifiers);
            (close, action, editor.protocol_epoch())
        };
        if close {
            self.context_editor_actions.clear();
            if let Some(epoch) = epoch {
                self.context_protocol.end_editor_epoch(epoch);
            }
            self.context_editor_overlay = None;
            self.force_full_redraw = true;
        }
        if let Some(action) = action {
            self.context_editor_actions.push_back(action);
        }
        true
    }

    pub(super) fn handle_context_editor_mouse(&mut self, mouse: MouseEvent) {
        let action = self
            .context_editor_overlay
            .as_ref()
            .and_then(|editor| editor.borrow_mut().handle_mouse(mouse));
        if let Some(action) = action {
            self.context_editor_actions.push_back(action);
        }
    }

    pub(super) fn sync_context_editor_from_protocol(&mut self) {
        let follow_up = self.context_editor_overlay.as_ref().and_then(|editor| {
            let mut editor = editor.borrow_mut();
            editor.sync_protocol(&self.context_protocol);
            editor.take_follow_up_action()
        });
        if let Some(action) = follow_up {
            self.context_editor_actions.push_back(action);
        }
    }

    pub(super) fn drain_local_context_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.local_context_event_rx.try_recv() {
            match self.reduce_context_server_event(event) {
                Ok(accepted) => changed |= accepted,
                Err(_) => debug_assert!(false, "local context channel carried a non-context event"),
            }
        }
        changed
    }

    pub(super) fn next_local_context_request_id(&mut self) -> u64 {
        let id = self.next_local_context_request_id;
        self.next_local_context_request_id =
            self.next_local_context_request_id.wrapping_add(1).max(1);
        id
    }

    pub(super) fn reduce_context_server_event(
        &mut self,
        event: ServerEvent,
    ) -> Result<bool, Box<ServerEvent>> {
        let accepted = match event {
            ServerEvent::ContextEditorSnapshot { id, snapshot } => {
                self.context_protocol.accept_snapshot(id, snapshot)
            }
            ServerEvent::ContextMessageDetail { id, detail } => {
                self.context_protocol.accept_detail(id, detail)
            }
            ServerEvent::ContextRangeClosurePreview { id, preview } => {
                self.context_protocol.accept_range_preview(id, preview)
            }
            ServerEvent::ContextCuratorPlanPreview { id, preview } => {
                self.context_protocol.accept_curator_plan(id, preview)
            }
            ServerEvent::ContextCuratorDefaultSaved {
                id,
                selection,
                resolved_route,
                unavailable_reason,
            } => self.context_protocol.accept_curator_default_saved(
                id,
                selection,
                resolved_route,
                unavailable_reason,
            ),
            ServerEvent::ContextDraftProgress {
                id,
                draft_id,
                progress,
            } => self
                .context_protocol
                .accept_draft_progress(id, draft_id, progress),
            ServerEvent::ContextDraftReady { id, draft } => {
                self.context_protocol.accept_draft_ready(id, draft)
            }
            ServerEvent::ContextDraftApplying { id, identity } => {
                self.context_protocol.accept_draft_applying(id, identity)
            }
            ServerEvent::ContextDraftFailed {
                id,
                identity,
                error,
            } => self
                .context_protocol
                .accept_draft_failed(id, identity, error, false),
            ServerEvent::ContextDraftStale {
                id,
                identity,
                error,
            } => self
                .context_protocol
                .accept_draft_failed(id, identity, error, true),
            ServerEvent::ContextDraftCanceled { id, identity } => {
                self.context_protocol.accept_draft_canceled(id, identity)
            }
            ServerEvent::ContextDraftExpired { id, identity } => {
                self.context_protocol.accept_draft_expired(id, identity)
            }
            ServerEvent::ContextDraftApplied {
                id,
                identity,
                transaction_id,
                revision,
            } => self
                .context_protocol
                .accept_draft_applied(id, identity, transaction_id, revision),
            ServerEvent::ContextDraftSelectionPreview { id, preview } => {
                self.context_protocol.accept_selection_preview(id, preview)
            }
            ServerEvent::ContextTransactionHistory {
                id,
                context_revision,
                total_transactions,
                offset,
                next_offset,
                transactions,
            } => self.context_protocol.accept_transaction_history(
                id,
                context_revision,
                total_transactions,
                offset,
                next_offset,
                transactions,
            ),
            ServerEvent::ContextTransactionDetail { id, detail } => {
                self.context_protocol.accept_transaction_detail(id, *detail)
            }
            ServerEvent::ContextTransactionApplied {
                id,
                draft_id,
                result,
            } => {
                let accepted = self.context_protocol.accept_transaction_result(
                    id,
                    ContextRequestKind::ApplyDraft,
                    draft_id,
                    result,
                );
                if accepted {
                    self.bump_context_revision();
                    self.clear_context_action_after_context_change();
                }
                accepted
            }
            ServerEvent::ContextTransactionReverted {
                id,
                transaction_id,
                result,
            } => {
                let accepted = self.context_protocol.accept_transaction_result(
                    id,
                    ContextRequestKind::RevertTransaction,
                    transaction_id,
                    result,
                );
                if accepted {
                    self.bump_context_revision();
                    self.clear_context_action_after_context_change();
                }
                accepted
            }
            ServerEvent::ContextTransactionReapplied {
                id,
                transaction_id,
                result,
            } => {
                let accepted = self.context_protocol.accept_transaction_result(
                    id,
                    ContextRequestKind::ReapplyTransaction,
                    transaction_id,
                    result,
                );
                if accepted {
                    self.bump_context_revision();
                    self.clear_context_action_after_context_change();
                }
                accepted
            }
            ServerEvent::ContextRequestRejected {
                id,
                request,
                draft_id,
                transaction_id,
                error,
            } => {
                let message = error.to_string();
                let accepted = self.context_protocol.accept_rejection(
                    id,
                    request,
                    draft_id,
                    transaction_id,
                    error,
                );
                if accepted {
                    self.set_status_notice(format!("Context request rejected: {message}"));
                }
                accepted
            }
            ServerEvent::ContextActionRequired {
                id,
                session_id,
                context_revision,
                reason,
                required_reduction_tokens,
                pending_input,
                preflight,
                payload,
                details,
                automatic_retry,
            } => {
                let partial_output_not_durable = details
                    .iter()
                    .any(|detail| detail == crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE);
                let partial_output_not_replayable = details
                    .iter()
                    .any(|detail| detail == crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE);
                let accepted = self.accept_context_action_required_event(
                    id,
                    &session_id,
                    context_revision,
                    pending_input.as_ref(),
                    preflight.as_ref(),
                    payload.as_ref(),
                    automatic_retry,
                );
                let accepted = accepted
                    && self.context_protocol.accept_action_required(
                        id,
                        session_id,
                        context_revision,
                        reason,
                        required_reduction_tokens,
                        pending_input,
                        preflight,
                        payload,
                        details,
                        automatic_retry,
                    );
                if accepted && partial_output_not_durable {
                    self.set_status_notice(
                        "Partial output is visible but could not be saved · keep session open",
                    );
                } else if accepted && partial_output_not_replayable {
                    self.set_status_notice(
                        "Provider output began, but no complete partial response could be retained",
                    );
                }
                accepted
            }
            ServerEvent::ContextPressureUpdated {
                id,
                session_id,
                report,
            } => self.accept_context_pressure_update(id, &session_id, report),
            ServerEvent::ContextEmergencyPolicyChanged {
                id,
                session_id,
                policy,
            } => self.context_protocol.accept_policy(id, session_id, policy),
            event => return Err(Box::new(event)),
        };
        if accepted {
            self.sync_context_editor_from_protocol();
        }
        Ok(accepted)
    }

    pub(super) fn context_editor_debug_summary(&self) -> serde_json::Value {
        self.context_editor_overlay
            .as_ref()
            .map(|editor| editor.borrow().debug_summary())
            .unwrap_or_else(|| serde_json::json!({ "open": false }))
    }

    pub(super) async fn dispatch_remote_context_editor_actions(
        &mut self,
        remote: &mut RemoteConnection,
    ) {
        while let Some(action) = self.context_editor_actions.pop_front() {
            let action = match action {
                ContextEditorAction::CopySafeMetadata(text) => {
                    let copied = super::helpers::copy_to_clipboard(&text);
                    self.set_status_notice(if copied {
                        "Context transaction metadata copied"
                    } else {
                        "Could not copy context transaction metadata"
                    });
                    continue;
                }
                action => action,
            };

            let prepared = self.prepare_remote_context_editor_action(remote, action);
            if !self
                .send_prepared_remote_context_request(remote, prepared)
                .await
            {
                break;
            }
        }
    }

    pub(super) async fn send_prepared_remote_context_request(
        &mut self,
        remote: &RemoteConnection,
        prepared: PreparedRemoteContextRequest,
    ) -> bool {
        let PreparedRemoteContextRequest {
            id,
            request,
            request_kind,
            draft_id,
            transaction_id,
        } = prepared;
        let Err(error) = remote.send_reserved_context_request(request).await else {
            return true;
        };
        let message =
            format!("Context editor transport request {id} failed before confirmation: {error}");
        let accepted = self.context_protocol.accept_rejection(
            id,
            request_kind,
            draft_id,
            transaction_id,
            ContextServiceError::Runtime(message.clone()),
        );
        debug_assert!(
            accepted,
            "remote context transport failure must clear its exact pending request"
        );
        if accepted {
            self.sync_context_editor_from_protocol();
        }
        self.report_context_editor_error(message);
        false
    }

    pub(super) fn prepare_remote_context_editor_action(
        &mut self,
        remote: &mut RemoteConnection,
        action: ContextEditorAction,
    ) -> PreparedRemoteContextRequest {
        let id = remote.reserve_context_request_id();
        let (request, request_kind, draft_id, transaction_id) = match action {
            ContextEditorAction::LoadSnapshot {
                page_start,
                page_size,
            } => {
                self.context_protocol.begin_snapshot_request(id);
                (
                    Request::GetContextEditorSnapshot {
                        id,
                        page_start,
                        page_size: Some(page_size),
                    },
                    ContextRequestKind::Snapshot,
                    None,
                    None,
                )
            }
            ContextEditorAction::LoadDetail {
                context_revision,
                transcript_digest,
                message_id,
                block_ordinal,
                start_char,
                max_chars,
            } => {
                self.context_protocol.begin_detail_request(
                    id,
                    self.context_session_id(),
                    context_revision,
                    transcript_digest,
                    message_id.clone(),
                    block_ordinal,
                );
                (
                    Request::GetContextMessageDetail {
                        id,
                        expected_context_revision: context_revision,
                        expected_transcript_digest: transcript_digest,
                        message_id,
                        block_ordinal,
                        start_char,
                        max_chars: Some(max_chars),
                    },
                    ContextRequestKind::MessageDetail,
                    None,
                    None,
                )
            }
            ContextEditorAction::PreviewRanges {
                context_revision,
                transcript_digest,
                ranges,
            } => {
                self.context_protocol.begin_range_preview_request(
                    id,
                    self.context_session_id(),
                    context_revision,
                    transcript_digest,
                    ranges.clone(),
                );
                (
                    Request::PreviewContextRanges {
                        id,
                        expected_context_revision: context_revision,
                        expected_transcript_digest: transcript_digest,
                        ranges,
                    },
                    ContextRequestKind::RangeClosurePreview,
                    None,
                    None,
                )
            }
            ContextEditorAction::PreviewCuratorPlan {
                context_revision,
                transcript_digest,
                request,
            } => {
                self.context_protocol.begin_curator_plan_request(
                    id,
                    self.context_session_id(),
                    context_revision,
                    transcript_digest,
                );
                (
                    Request::PreviewContextCuratorPlan {
                        id,
                        expected_context_revision: context_revision,
                        expected_transcript_digest: transcript_digest,
                        request,
                    },
                    ContextRequestKind::CuratorPlanPreview,
                    None,
                    None,
                )
            }
            ContextEditorAction::SaveCuratorDefault(selection) => {
                self.context_protocol.begin_curator_default_request(id);
                (
                    Request::SaveContextCuratorDefault { id, selection },
                    ContextRequestKind::SaveCuratorDefault,
                    None,
                    None,
                )
            }
            ContextEditorAction::PrepareDraft(request) => {
                self.context_protocol.begin_prepare_draft(id);
                (
                    Request::PrepareContextDraft { id, request },
                    ContextRequestKind::PrepareDraft,
                    None,
                    None,
                )
            }
            ContextEditorAction::CancelDraft { draft_id } => {
                self.context_protocol
                    .begin_cancel_draft(id, draft_id.clone());
                (
                    Request::CancelContextDraft {
                        id,
                        draft_id: draft_id.clone(),
                    },
                    ContextRequestKind::CancelDraft,
                    Some(draft_id),
                    None,
                )
            }
            ContextEditorAction::MonitorDraft { draft_id } => {
                self.context_protocol
                    .begin_draft_monitor(id, draft_id.clone());
                (
                    Request::GetContextDraftStatus {
                        id,
                        draft_id: draft_id.clone(),
                    },
                    ContextRequestKind::DraftStatus,
                    Some(draft_id),
                    None,
                )
            }
            ContextEditorAction::PreviewDraftSelection {
                draft_id,
                selected_distillation_ids,
            } => {
                self.context_protocol.begin_selection_preview_request(
                    id,
                    draft_id.clone(),
                    selected_distillation_ids.clone(),
                );
                (
                    Request::PreviewContextDraftSelection {
                        id,
                        draft_id: draft_id.clone(),
                        selected_distillation_ids,
                    },
                    ContextRequestKind::DraftSelectionPreview,
                    Some(draft_id),
                    None,
                )
            }
            ContextEditorAction::ApplyDraft {
                draft_id,
                selected_distillation_ids,
            } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::ApplyDraft,
                    draft_id.clone(),
                );
                (
                    Request::ApplyContextDraft {
                        id,
                        draft_id: draft_id.clone(),
                        selected_distillation_ids: Some(selected_distillation_ids),
                    },
                    ContextRequestKind::ApplyDraft,
                    Some(draft_id),
                    None,
                )
            }
            ContextEditorAction::LoadHistory { offset, limit } => {
                self.context_protocol
                    .begin_history_request(id, self.context_session_id());
                (
                    Request::ListContextTransactions {
                        id,
                        offset,
                        limit: Some(limit),
                    },
                    ContextRequestKind::TransactionHistory,
                    None,
                    None,
                )
            }
            ContextEditorAction::LoadTransactionDetail {
                context_revision,
                transaction_id,
            } => {
                self.context_protocol.begin_transaction_detail_request(
                    id,
                    self.context_session_id(),
                    context_revision,
                    transaction_id.clone(),
                );
                (
                    Request::GetContextTransactionDetail {
                        id,
                        expected_context_revision: context_revision,
                        transaction_id: transaction_id.clone(),
                    },
                    ContextRequestKind::TransactionDetail,
                    None,
                    Some(transaction_id),
                )
            }
            ContextEditorAction::RevertTransaction { transaction_id } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::RevertTransaction,
                    transaction_id.clone(),
                );
                (
                    Request::RevertContextTransaction {
                        id,
                        transaction_id: transaction_id.clone(),
                    },
                    ContextRequestKind::RevertTransaction,
                    None,
                    Some(transaction_id),
                )
            }
            ContextEditorAction::ReapplyTransaction { transaction_id } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::ReapplyTransaction,
                    transaction_id.clone(),
                );
                (
                    Request::ReapplyContextTransaction {
                        id,
                        transaction_id: transaction_id.clone(),
                    },
                    ContextRequestKind::ReapplyTransaction,
                    None,
                    Some(transaction_id),
                )
            }
            ContextEditorAction::SetEmergencyPolicy(policy) => {
                self.context_protocol.begin_policy_request(id);
                (
                    Request::SetContextEmergencyPolicy { id, policy },
                    ContextRequestKind::SetEmergencyPolicy,
                    None,
                    None,
                )
            }
            ContextEditorAction::CopySafeMetadata(_) => {
                unreachable!("copy actions are handled before reserving a remote request ID")
            }
        };
        PreparedRemoteContextRequest {
            id,
            request,
            request_kind,
            draft_id,
            transaction_id,
        }
    }

    pub(super) fn dispatch_local_context_editor_actions(&mut self) -> bool {
        let mut changed = false;
        while let Some(action) = self.context_editor_actions.pop_front() {
            changed = true;
            self.dispatch_one_local_context_editor_action(action);
        }
        changed
    }

    fn dispatch_one_local_context_editor_action(&mut self, action: ContextEditorAction) {
        let id = self.next_local_context_request_id();
        match action {
            ContextEditorAction::LoadSnapshot {
                page_start,
                page_size,
            } => {
                self.context_protocol.begin_snapshot_request(id);
                let event = self
                    .context_transactions
                    .context_editor_snapshot_page_for_session(
                        &self.session.id,
                        &self.session.messages,
                        &self.session.context_view,
                        self.is_processing,
                        self.provider.as_ref(),
                        &self.local_context_route_identity(),
                        self.local_context_request_token_estimate(),
                        self.session.active_transition_message_id(),
                        page_start,
                        page_size,
                    )
                    .map(|snapshot| ServerEvent::ContextEditorSnapshot { id, snapshot });
                self.send_local_context_result(id, ContextRequestKind::Snapshot, None, None, event);
            }
            ContextEditorAction::LoadDetail {
                context_revision,
                transcript_digest,
                message_id,
                block_ordinal,
                start_char,
                max_chars,
            } => {
                self.context_protocol.begin_detail_request(
                    id,
                    self.session.id.clone(),
                    context_revision,
                    transcript_digest,
                    message_id.clone(),
                    block_ordinal,
                );
                let event = self
                    .context_transactions
                    .context_message_detail_for_session(
                        &self.session.id,
                        &self.session.messages,
                        &self.session.context_view,
                        context_revision,
                        transcript_digest,
                        &message_id,
                        block_ordinal,
                        start_char,
                        max_chars,
                    )
                    .map(|detail| ServerEvent::ContextMessageDetail { id, detail });
                self.send_local_context_result(
                    id,
                    ContextRequestKind::MessageDetail,
                    None,
                    None,
                    event,
                );
            }
            ContextEditorAction::PreviewRanges {
                context_revision,
                transcript_digest,
                ranges,
            } => {
                self.context_protocol.begin_range_preview_request(
                    id,
                    self.session.id.clone(),
                    context_revision,
                    transcript_digest,
                    ranges.clone(),
                );
                let event = self
                    .context_transactions
                    .preview_context_ranges_with_active_profile(
                        &self.session.id,
                        &self.session.messages,
                        &self.session.context_view,
                        context_revision,
                        transcript_digest,
                        self.session.active_transition_message_id(),
                        &ranges,
                    )
                    .map(|preview| ServerEvent::ContextRangeClosurePreview { id, preview });
                self.send_local_context_result(
                    id,
                    ContextRequestKind::RangeClosurePreview,
                    None,
                    None,
                    event,
                );
            }
            ContextEditorAction::PreviewCuratorPlan {
                context_revision,
                transcript_digest,
                request,
            } => {
                self.context_protocol.begin_curator_plan_request(
                    id,
                    self.session.id.clone(),
                    context_revision,
                    transcript_digest,
                );
                let event = self
                    .context_transactions
                    .preview_context_curator_plan_for_session_with_active_profile(
                        &self.session.id,
                        &self.session.messages,
                        &self.session.context_view,
                        self.is_processing,
                        self.provider.as_ref(),
                        &self.local_context_route_identity(),
                        &self.provider.model_routes(),
                        context_revision,
                        transcript_digest,
                        request,
                        self.session.active_transition_message_id(),
                        &crate::config::config().context.curator,
                    )
                    .map(|preview| ServerEvent::ContextCuratorPlanPreview { id, preview });
                self.send_local_context_result(
                    id,
                    ContextRequestKind::CuratorPlanPreview,
                    None,
                    None,
                    event,
                );
            }
            ContextEditorAction::SaveCuratorDefault(selection) => {
                self.context_protocol.begin_curator_default_request(id);
                let config = crate::config::ContextCuratorConfig {
                    provider: selection.provider.clone(),
                    route: selection.route.clone(),
                    model: selection.model.clone(),
                    effort: selection.effort.clone(),
                };
                let event = if self.is_processing {
                    Err(ContextServiceError::SessionBusy)
                } else {
                    crate::context::resolve_context_curator_route(
                        self.provider.fork(),
                        &self.provider.model_routes(),
                        &self.local_context_route_identity(),
                        &config,
                    )
                    .map_err(|error| ContextServiceError::Curator(error.to_string()))
                    .and_then(|route| {
                        crate::config::Config::set_context_curator(&config)
                            .map_err(|error| ContextServiceError::Persistence(error.to_string()))?;
                        Ok(ServerEvent::ContextCuratorDefaultSaved {
                            id,
                            selection,
                            resolved_route: Some(route.preview()),
                            unavailable_reason: None,
                        })
                    })
                };
                self.send_local_context_result(
                    id,
                    ContextRequestKind::SaveCuratorDefault,
                    None,
                    None,
                    event,
                );
            }
            ContextEditorAction::PrepareDraft(request) => {
                self.context_protocol.begin_prepare_draft(id);
                let input = ContextDraftRuntimeInput {
                    session_id: self.session.id.clone(),
                    messages: self.session.messages.clone(),
                    context_view: self.session.context_view.clone(),
                    provider: Arc::clone(&self.provider),
                    route: self.local_context_route_identity(),
                    model_routes: self.provider.model_routes(),
                    estimated_total_request_tokens_before: self
                        .local_context_request_token_estimate(),
                    active_agent_profile_message_id: self
                        .session
                        .active_transition_message_id()
                        .map(str::to_string),
                };
                match self.context_transactions.prepare_draft_for_session(
                    input,
                    request,
                    self.is_processing,
                ) {
                    Ok(draft_id) => self.spawn_local_context_draft_monitor(
                        id,
                        ContextRequestKind::PrepareDraft,
                        draft_id,
                    ),
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::PrepareDraft,
                        None,
                        None,
                        error,
                    ),
                }
            }
            ContextEditorAction::CancelDraft { draft_id } => {
                self.context_protocol
                    .begin_cancel_draft(id, draft_id.clone());
                let result = self.context_transactions.cancel_draft(&draft_id);
                match result {
                    Ok(()) => self.spawn_local_context_draft_monitor(
                        id,
                        ContextRequestKind::CancelDraft,
                        draft_id,
                    ),
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::CancelDraft,
                        Some(draft_id),
                        None,
                        error,
                    ),
                }
            }
            ContextEditorAction::MonitorDraft { draft_id } => {
                self.context_protocol
                    .begin_draft_monitor(id, draft_id.clone());
                self.spawn_local_context_draft_monitor(
                    id,
                    ContextRequestKind::DraftStatus,
                    draft_id,
                );
            }
            ContextEditorAction::PreviewDraftSelection {
                draft_id,
                selected_distillation_ids,
            } => {
                self.context_protocol.begin_selection_preview_request(
                    id,
                    draft_id.clone(),
                    selected_distillation_ids.clone(),
                );
                let event = self
                    .context_transactions
                    .preview_draft_selection_for_session(
                        &self.session.id,
                        &self.session.messages,
                        &self.session.context_view,
                        self.provider.as_ref(),
                        &self.local_context_route_identity(),
                        self.local_context_request_token_estimate(),
                        &draft_id,
                        selected_distillation_ids,
                    )
                    .map(|preview| ServerEvent::ContextDraftSelectionPreview { id, preview });
                self.send_local_context_result(
                    id,
                    ContextRequestKind::DraftSelectionPreview,
                    Some(draft_id),
                    None,
                    event,
                );
            }
            ContextEditorAction::ApplyDraft {
                draft_id,
                selected_distillation_ids,
            } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::ApplyDraft,
                    draft_id.clone(),
                );
                let service = Arc::clone(&self.context_transactions);
                let route = self.local_context_route_identity();
                let estimate = self.local_context_request_token_estimate();
                match service.apply_draft_to_session(
                    &mut self.session,
                    self.provider.as_ref(),
                    &route,
                    estimate,
                    &draft_id,
                    Some(selected_distillation_ids),
                    self.is_processing,
                ) {
                    Ok(mut transition) => {
                        if let Err(error) = self.after_local_provider_context_changed(
                            "context transaction",
                            &transition.invalidation_detail,
                        ) {
                            transition.result.warnings.push(error);
                        }
                        let _ = self.local_context_event_tx.send(
                            ServerEvent::ContextTransactionApplied {
                                id,
                                draft_id,
                                result: transition.result,
                            },
                        );
                    }
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::ApplyDraft,
                        Some(draft_id),
                        None,
                        error,
                    ),
                }
            }
            ContextEditorAction::LoadHistory { offset, limit } => {
                self.context_protocol
                    .begin_history_request(id, self.session.id.clone());
                let transactions =
                    crate::context::list_context_transactions(&self.session.context_view);
                if offset > transactions.len() {
                    self.send_local_context_rejection(
                        id,
                        ContextRequestKind::TransactionHistory,
                        None,
                        None,
                        ContextServiceError::InvalidSelection(format!(
                            "context history offset {offset} exceeds {} transactions",
                            transactions.len()
                        )),
                    );
                } else {
                    let end = offset.saturating_add(limit).min(transactions.len());
                    let _ =
                        self.local_context_event_tx
                            .send(ServerEvent::ContextTransactionHistory {
                                id,
                                context_revision: self.session.context_view.revision,
                                total_transactions: transactions.len(),
                                offset,
                                next_offset: (end < transactions.len()).then_some(end),
                                transactions: transactions[offset..end].to_vec(),
                            });
                }
            }
            ContextEditorAction::LoadTransactionDetail {
                context_revision,
                transaction_id,
            } => {
                self.context_protocol.begin_transaction_detail_request(
                    id,
                    self.session.id.clone(),
                    context_revision,
                    transaction_id.clone(),
                );
                let event = if self.session.context_view.revision != context_revision {
                    Err(ContextServiceError::Stale(format!(
                        "context revision changed from {context_revision} to {}",
                        self.session.context_view.revision
                    )))
                } else {
                    self.session
                        .context_view
                        .transactions
                        .iter()
                        .find(|transaction| transaction.id == transaction_id)
                        .cloned()
                        .ok_or_else(|| {
                            ContextServiceError::TransactionNotFound(transaction_id.clone())
                        })
                        .map(|transaction| ServerEvent::ContextTransactionDetail {
                            id,
                            detail: Box::new(ContextTransactionDetail {
                                session_id: self.session.id.clone(),
                                context_revision,
                                transaction,
                            }),
                        })
                };
                self.send_local_context_result(
                    id,
                    ContextRequestKind::TransactionDetail,
                    None,
                    Some(transaction_id),
                    event,
                );
            }
            ContextEditorAction::RevertTransaction { transaction_id } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::RevertTransaction,
                    transaction_id.clone(),
                );
                let route = self.local_context_route_identity();
                let estimate = self.local_context_request_token_estimate();
                let service = Arc::clone(&self.context_transactions);
                match service.revert_transaction_in_session(
                    &mut self.session,
                    self.provider.as_ref(),
                    &route,
                    estimate,
                    &transaction_id,
                    self.is_processing,
                ) {
                    Ok(mut transition) => {
                        if let Err(error) = self.after_local_provider_context_changed(
                            "context transaction",
                            &transition.invalidation_detail,
                        ) {
                            transition.result.warnings.push(error);
                        }
                        let _ = self.local_context_event_tx.send(
                            ServerEvent::ContextTransactionReverted {
                                id,
                                transaction_id,
                                result: transition.result,
                            },
                        );
                    }
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::RevertTransaction,
                        None,
                        Some(transaction_id),
                        error,
                    ),
                }
            }
            ContextEditorAction::ReapplyTransaction { transaction_id } => {
                self.context_protocol.begin_transaction_request(
                    id,
                    ContextRequestKind::ReapplyTransaction,
                    transaction_id.clone(),
                );
                let route = self.local_context_route_identity();
                let estimate = self.local_context_request_token_estimate();
                let service = Arc::clone(&self.context_transactions);
                match service.reapply_transaction_in_session(
                    &mut self.session,
                    self.provider.as_ref(),
                    &route,
                    estimate,
                    &transaction_id,
                    self.is_processing,
                ) {
                    Ok(mut transition) => {
                        if let Err(error) = self.after_local_provider_context_changed(
                            "context transaction",
                            &transition.invalidation_detail,
                        ) {
                            transition.result.warnings.push(error);
                        }
                        let _ = self.local_context_event_tx.send(
                            ServerEvent::ContextTransactionReapplied {
                                id,
                                transaction_id,
                                result: transition.result,
                            },
                        );
                    }
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::ReapplyTransaction,
                        None,
                        Some(transaction_id),
                        error,
                    ),
                }
            }
            ContextEditorAction::SetEmergencyPolicy(policy) => {
                self.context_protocol.begin_policy_request(id);
                match self.context_transactions.set_emergency_policy_for_session(
                    &mut self.session,
                    policy,
                    self.is_processing,
                ) {
                    Ok((session_id, policy)) => {
                        let _ = self.local_context_event_tx.send(
                            ServerEvent::ContextEmergencyPolicyChanged {
                                id,
                                session_id,
                                policy,
                            },
                        );
                    }
                    Err(error) => self.send_local_context_rejection(
                        id,
                        ContextRequestKind::SetEmergencyPolicy,
                        None,
                        None,
                        error,
                    ),
                }
            }
            ContextEditorAction::CopySafeMetadata(text) => {
                let copied = super::helpers::copy_to_clipboard(&text);
                self.set_status_notice(if copied {
                    "Context transaction metadata copied"
                } else {
                    "Could not copy context transaction metadata"
                });
            }
        }
    }

    fn context_session_id(&self) -> String {
        self.context_protocol
            .accepted_session_id
            .clone()
            .or_else(|| self.remote_session_id.clone())
            .unwrap_or_else(|| self.session.id.clone())
    }

    fn local_context_route_identity(&self) -> String {
        self.session
            .route_api_method
            .clone()
            .or_else(|| self.session.provider_key.clone())
            .unwrap_or_else(|| self.provider.name().to_string())
    }

    fn local_context_request_token_estimate(&self) -> Option<usize> {
        self.registry
            .context_budget()
            .try_read()
            .ok()
            .map(|tracker| tracker.effective_token_count())
    }

    fn report_context_editor_error(&mut self, message: String) {
        if let Some(editor) = self.context_editor_overlay.as_ref() {
            editor.borrow_mut().report_error(message.clone(), false);
        }
        self.set_status_notice(message);
    }

    fn send_local_context_result(
        &self,
        id: u64,
        request: ContextRequestKind,
        draft_id: Option<String>,
        transaction_id: Option<String>,
        result: Result<ServerEvent, ContextServiceError>,
    ) {
        match result {
            Ok(event) => {
                let _ = self.local_context_event_tx.send(event);
            }
            Err(error) => {
                self.send_local_context_rejection(id, request, draft_id, transaction_id, error)
            }
        }
    }

    fn send_local_context_rejection(
        &self,
        id: u64,
        request: ContextRequestKind,
        draft_id: Option<String>,
        transaction_id: Option<String>,
        error: ContextServiceError,
    ) {
        let _ = self
            .local_context_event_tx
            .send(ServerEvent::ContextRequestRejected {
                id,
                request,
                draft_id,
                transaction_id,
                error,
            });
    }

    fn spawn_local_context_draft_monitor(
        &self,
        id: u64,
        request: ContextRequestKind,
        draft_id: String,
    ) {
        let service = Arc::clone(&self.context_transactions);
        let event_tx = self.local_context_event_tx.clone();
        let expected_session_id = self.session.id.clone();
        tokio::spawn(async move {
            monitor_local_context_draft(
                service,
                event_tx,
                id,
                request,
                draft_id,
                expected_session_id,
            )
            .await;
        });
    }

    pub(super) fn after_local_provider_context_changed(
        &mut self,
        source: &'static str,
        detail: &str,
    ) -> Result<(), String> {
        #[cfg(test)]
        {
            self.context_reset_counters.hook_calls += 1;
            self.context_reset_counters.invalidation_records += 1;
        }
        crate::cache_invalidation::record(source, detail.to_string());
        #[cfg(test)]
        {
            self.context_reset_counters.cache_generation_advances += 1;
        }
        self.kv_cache.cache_generation = self.kv_cache.cache_generation.wrapping_add(1);
        self.kv_cache.kv_cache_baseline = None;
        self.kv_cache.cold_cache_warned_baseline_completed_at = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        #[cfg(test)]
        {
            self.context_reset_counters.continuation_invalidations += 1;
        }
        self.provider.invalidate_context_continuation(detail);
        self.streaming.streaming_context_stale = true;
        self.streaming.streaming_usage_call_reset_pending = true;

        #[cfg(test)]
        {
            self.context_reset_counters.projected_rebuild_attempts += 1;
        }
        let projected = self.session.projected_messages_for_provider().map_err(|error| {
            #[cfg(test)]
            {
                self.context_reset_counters.budget_reseeds += 1;
            }
            self.reseed_context_budget_from_messages(&[], "invalid changed provider context");
            format!(
                "The provider-context change was persisted, but rebuilding projected messages failed: {error}"
            )
        })?;
        #[cfg(test)]
        {
            self.context_reset_counters.budget_reseeds += 1;
        }
        self.replace_provider_messages(projected.clone());
        self.reseed_context_budget_from_messages(&projected, detail);
        Ok(())
    }
}

async fn monitor_local_context_draft(
    service: Arc<crate::context::ContextTransactionService>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    id: u64,
    request: ContextRequestKind,
    draft_id: String,
    expected_session_id: String,
) {
    let mut status = match service.draft_status(&draft_id) {
        Ok(status) if status.identity().session_id == expected_session_id => status,
        Ok(_) => {
            send_monitor_rejection(
                &event_tx,
                id,
                request,
                draft_id,
                ContextServiceError::DraftNotFound("retained draft".to_string()),
            );
            return;
        }
        Err(error) => {
            send_monitor_rejection(&event_tx, id, request, draft_id, error);
            return;
        }
    };
    loop {
        let terminal = status.is_terminal();
        let _ = event_tx.send(local_draft_status_event(id, status.clone()));
        if terminal {
            return;
        }
        status = match service.wait_for_draft_update(&draft_id, &status).await {
            Ok(status) if status.identity().session_id == expected_session_id => status,
            Ok(_) => {
                send_monitor_rejection(
                    &event_tx,
                    id,
                    request,
                    draft_id,
                    ContextServiceError::DraftNotFound("retained draft".to_string()),
                );
                return;
            }
            Err(error) => {
                send_monitor_rejection(&event_tx, id, request, draft_id, error);
                return;
            }
        };
    }
}

fn local_draft_status_event(id: u64, status: ContextDraftStatus) -> ServerEvent {
    match status {
        ContextDraftStatus::Preparing { identity, progress } => ServerEvent::ContextDraftProgress {
            id,
            draft_id: identity.draft_id,
            progress,
        },
        ContextDraftStatus::Ready { draft } => ServerEvent::ContextDraftReady { id, draft },
        ContextDraftStatus::Applying { identity } => {
            ServerEvent::ContextDraftApplying { id, identity }
        }
        ContextDraftStatus::Applied {
            identity,
            transaction_id,
            revision,
        } => ServerEvent::ContextDraftApplied {
            id,
            identity,
            transaction_id,
            revision,
        },
        ContextDraftStatus::Failed { identity, error } => {
            if matches!(error, ContextServiceError::Stale(_)) {
                ServerEvent::ContextDraftStale {
                    id,
                    identity,
                    error,
                }
            } else {
                ServerEvent::ContextDraftFailed {
                    id,
                    identity,
                    error,
                }
            }
        }
        ContextDraftStatus::Canceled { identity } => {
            ServerEvent::ContextDraftCanceled { id, identity }
        }
        ContextDraftStatus::Expired { identity } => {
            ServerEvent::ContextDraftExpired { id, identity }
        }
    }
}

fn send_monitor_rejection(
    event_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    id: u64,
    request: ContextRequestKind,
    draft_id: String,
    error: ContextServiceError,
) {
    let _ = event_tx.send(ServerEvent::ContextRequestRejected {
        id,
        request,
        draft_id: Some(draft_id),
        transaction_id: None,
        error,
    });
}
