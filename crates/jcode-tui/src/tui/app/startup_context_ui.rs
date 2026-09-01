use super::{App, ProcessingStatus};
use crate::protocol::{
    Request, STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE, StartupContextActionRequired,
    StartupContextCompactStatus, StartupContextPromptDisposition, StartupContextStatusSnapshot,
    StartupContextStatusState,
};
use crate::tui::StartupContextAvailability;
use crate::tui::backend::RemoteConnection;
use crate::tui::startup_context_editor::{
    STARTUP_CONTEXT_EDITOR_PREVIEW_PAGE_CHARS, StartupContextEditor, StartupContextEditorAction,
    StartupContextPendingRequest,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};
use jcode_tui_messages::DisplayMessage;
use std::cell::RefCell;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StartupContextStatusPageRequest {
    file_page_start: usize,
    issue_page_start: usize,
    page_size: usize,
}

#[derive(Debug)]
pub(super) struct StartupContextUiState {
    availability: StartupContextAvailability,
    session_id: Option<String>,
    compact: Option<StartupContextCompactStatus>,
    detail: Option<StartupContextStatusSnapshot>,
    action_required: Option<StartupContextActionRequired>,
    overlay_scroll: Option<usize>,
    pending_page: Option<StartupContextStatusPageRequest>,
    outstanding_request: Option<(u64, String)>,
    action_request_id: Option<u64>,
    editor: Option<RefCell<StartupContextEditor>>,
}

impl Default for StartupContextUiState {
    fn default() -> Self {
        Self {
            availability: StartupContextAvailability::Loading,
            session_id: None,
            compact: None,
            detail: None,
            action_required: None,
            overlay_scroll: None,
            pending_page: None,
            outstanding_request: None,
            action_request_id: None,
            editor: None,
        }
    }
}

impl StartupContextUiState {
    fn begin_session(&mut self, session_id: &str) {
        self.availability = StartupContextAvailability::Loading;
        self.session_id = Some(session_id.to_string());
        self.compact = None;
        self.detail = None;
        self.action_required = None;
        self.overlay_scroll = None;
        self.pending_page = None;
        self.outstanding_request = None;
        self.action_request_id = None;
        self.editor = None;
    }

    fn accept_history(&mut self, session_id: &str, status: Option<StartupContextCompactStatus>) {
        let same_session = self.session_id.as_deref() == Some(session_id);
        if !same_session {
            self.begin_session(session_id);
        } else if let Some(editor) = self.editor.as_ref() {
            if editor.borrow().is_visible() || editor.borrow().has_tracked_apply() {
                editor.borrow_mut().restart_after_reconnect();
            } else {
                self.editor = None;
            }
        }
        self.availability = if status.is_some() {
            StartupContextAvailability::Available
        } else {
            StartupContextAvailability::Unsupported
        };
        self.compact = status;
        self.detail = None;
        self.action_required = None;
        self.outstanding_request = None;
        self.action_request_id = None;
        if self.overlay_scroll.is_some()
            && self.availability == StartupContextAvailability::Available
        {
            self.queue_full_status();
        }
    }

    fn queue_full_status(&mut self) {
        if self.availability != StartupContextAvailability::Available {
            return;
        }
        self.pending_page = Some(StartupContextStatusPageRequest {
            file_page_start: 0,
            issue_page_start: 0,
            page_size: STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE,
        });
    }

    fn queue_compact_status(&mut self) {
        if self.availability != StartupContextAvailability::Available {
            return;
        }
        self.pending_page = Some(StartupContextStatusPageRequest {
            file_page_start: 0,
            issue_page_start: 0,
            page_size: 1,
        });
    }

    fn queue_continuation(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let file_page_start = detail.next_file_page_start.unwrap_or(detail.total_files);
        let issue_page_start = detail.next_issue_page_start.unwrap_or(detail.total_issues);
        if file_page_start >= detail.total_files && issue_page_start >= detail.total_issues {
            return;
        }
        self.pending_page = Some(StartupContextStatusPageRequest {
            file_page_start,
            issue_page_start,
            page_size: STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE,
        });
    }

    fn accept_snapshot(
        &mut self,
        id: u64,
        snapshot: StartupContextStatusSnapshot,
        action_required: Option<StartupContextActionRequired>,
    ) -> bool {
        let session_id = snapshot.compact.session_id.clone();
        if self.session_id.as_deref() != Some(session_id.as_str()) {
            return false;
        }
        let is_action = action_required.is_some();
        if !is_action && self.outstanding_request.as_ref() != Some(&(id, session_id.clone())) {
            return false;
        }

        self.outstanding_request = None;
        self.availability = StartupContextAvailability::Available;
        self.compact = Some(snapshot.compact.clone());
        if snapshot.file_page_start == 0 && snapshot.issue_page_start == 0 {
            self.detail = Some(snapshot);
        } else if let Some(detail) = self.detail.as_mut() {
            if snapshot.file_page_start == detail.files.len() {
                detail.files.extend(snapshot.files);
                detail.file_page_end = snapshot.file_page_end;
                detail.next_file_page_start = snapshot.next_file_page_start;
            }
            if snapshot.issue_page_start == detail.issues.len() {
                detail.issues.extend(snapshot.issues);
                detail.issue_page_end = snapshot.issue_page_end;
                detail.next_issue_page_start = snapshot.next_issue_page_start;
            }
            detail.compact = snapshot.compact;
            detail.total_files = snapshot.total_files;
            detail.total_issues = snapshot.total_issues;
        } else {
            return false;
        }
        if let Some(action) = action_required {
            self.action_required = Some(action);
            self.action_request_id = Some(id);
            self.overlay_scroll = Some(0);
            if self.editor.is_none() {
                self.editor = Some(RefCell::new(StartupContextEditor::new(
                    session_id.clone(),
                    self.detail.as_ref(),
                )));
            }
        }
        if self.overlay_scroll.is_some() {
            self.queue_continuation();
        }
        if let (Some(editor), Some(detail)) = (self.editor.as_ref(), self.detail.as_ref()) {
            editor.borrow_mut().refresh_receipt(detail);
        }
        true
    }
}

impl App {
    pub(in crate::tui::app) fn observe_local_startup_context_before_user_turn(&mut self) {
        if self.is_remote {
            return;
        }
        match self
            .session
            .observe_startup_context_before_user_turn(&crate::startup_context::StartupContext::new())
        {
            Ok(outcome) => {
                if outcome.provider_history_changed() {
                    self.reseed_context_runtime_from_provider_messages();
                }
            }
            Err(error) => {
                crate::logging::warn(&format!(
                    "STARTUP_CONTEXT_STALE_OBSERVATION_FAILED session={} error={}",
                    self.session.id, error
                ));
                self.set_status_notice(format!(
                    "Startup Context warning: observation was not saved; the turn will continue without a stale marker · {error}"
                ));
            }
        }
    }

    pub(in crate::tui::app) fn startup_context_debug_fixture_names() -> &'static [&'static str] {
        &[
            "loading",
            "none",
            "ready",
            "blocked",
            "blocked-action",
            "dispatched",
            "accepted",
            "queued",
            "stale",
            "busy",
            "unsupported",
            "metadata-repair",
            "storage-error",
            "editor-loading",
            "editor-empty",
            "editor-populated",
            "editor-stale",
            "editor-invalid",
            "editor-external",
            "editor-busy",
            "editor-apply-review",
            "editor-apply-review-late",
            "editor-apply-external",
            "editor-apply-queued",
            "editor-apply-applying",
            "editor-apply-recovery",
            "editor-apply-success",
            "editor-apply-partial",
            "editor-apply-failed",
            "editor-apply-canceled",
        ]
    }

    pub(in crate::tui::app) fn apply_startup_context_debug_fixture(
        &mut self,
        name: &str,
    ) -> Result<(), String> {
        use crate::protocol::{
            StartupContextActionKind, StartupContextBatchKind, StartupContextDeliveryState,
            StartupContextFailure, StartupContextFailureKind, StartupContextFileIssueKind,
            StartupContextFileIssueSnapshot, StartupContextFileReceiptSnapshot,
            StartupContextLeaseAvailability, StartupContextLeaseOwnerSnapshot,
            StartupContextObservedState, StartupContextOperation, StartupContextPathClassification,
            StartupContextProjectKind, StartupContextProjectSnapshot,
        };

        if !Self::startup_context_debug_fixture_names().contains(&name) {
            return Err(format!(
                "unknown Startup Context fixture {name:?}; available fixtures: {}",
                Self::startup_context_debug_fixture_names().join(", ")
            ));
        }

        self.is_remote = true;
        let session_id = self
            .remote_session_id
            .clone()
            .unwrap_or_else(|| "fixture-startup-session".to_string());
        self.remote_session_id = Some(session_id.clone());
        self.startup_context_ui.begin_session(&session_id);
        if name == "loading" {
            return Ok(());
        }
        if name == "unsupported" {
            self.startup_context_ui.availability = StartupContextAvailability::Unsupported;
            return Ok(());
        }

        let now = chrono::Utc::now();
        let mut compact = StartupContextCompactStatus {
            protocol_version: crate::protocol::STARTUP_CONTEXT_PROTOCOL_VERSION,
            session_id: session_id.clone(),
            state: StartupContextStatusState::Prepared,
            project: Some(StartupContextProjectSnapshot {
                key_digest: "fixture-project".to_string(),
                kind: StartupContextProjectKind::Git,
                active_root: "/fixture/project".to_string(),
            }),
            plan_revision: 7,
            plan_entry_count: 4,
            receipt_plan_revision: Some(7),
            receipt_file_count: 4,
            captured_bytes: 91_200,
            estimated_tokens: 22_400,
            blocked_issue_count: 0,
            pending_update_count: 0,
            stale_file_count: 0,
            lease: StartupContextLeaseAvailability::Available,
            error: None,
        };
        let mut files = vec![StartupContextFileReceiptSnapshot {
            batch_id: "fixture-batch".to_string(),
            batch_kind: StartupContextBatchKind::Initial,
            delivery_state: StartupContextDeliveryState::Captured,
            spec_id: "fixture-spec".to_string(),
            message_id: "fixture-message".to_string(),
            ordinal: 1,
            logical_path: "docs/PLAN.md".to_string(),
            resolved_path: "/fixture/project/docs/PLAN.md".to_string(),
            classification: StartupContextPathClassification::Project,
            sha256: "0123456789abcdef".repeat(4),
            bytes: 91_200,
            estimated_tokens: 22_400,
            latest_observation: StartupContextObservedState::Current,
            notification_count: 0,
        }];
        let mut issues = Vec::new();

        match name {
            "none" => {
                compact.state = StartupContextStatusState::Empty;
                compact.plan_entry_count = 0;
                compact.receipt_file_count = 0;
                compact.captured_bytes = 0;
                compact.estimated_tokens = 0;
                files.clear();
            }
            "ready" => {}
            "blocked" | "blocked-action" => {
                compact.state = StartupContextStatusState::Blocked;
                compact.blocked_issue_count = 1;
                compact.receipt_file_count = 0;
                compact.captured_bytes = 0;
                compact.estimated_tokens = 0;
                files.clear();
                issues.push(StartupContextFileIssueSnapshot {
                    input_index: Some(0),
                    spec_id: Some("fixture-spec".to_string()),
                    logical_path: Some("docs/MISSING.md".to_string()),
                    kind: StartupContextFileIssueKind::Missing,
                });
            }
            "dispatched" => {
                compact.state = StartupContextStatusState::Dispatched;
                files[0].delivery_state = StartupContextDeliveryState::Dispatched;
            }
            "accepted" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "queued" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                compact.pending_update_count = 2;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "stale" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                compact.stale_file_count = 1;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
                files[0].latest_observation = StartupContextObservedState::Changed {
                    sha256: "fedcba9876543210".repeat(4),
                    bytes: 91_240,
                };
                files[0].notification_count = 2;
            }
            "busy" => {
                compact.lease = StartupContextLeaseAvailability::Busy {
                    owner: Some(StartupContextLeaseOwnerSnapshot {
                        server_name: "fixture-server".to_string(),
                        session_id: "fixture-owner".to_string(),
                        acquired_at: now,
                        renewed_at: now,
                        expires_at: now + chrono::Duration::minutes(2),
                    }),
                };
            }
            "metadata-repair" => {
                compact.state = StartupContextStatusState::MetadataRepair;
            }
            "storage-error" => {
                compact.state = StartupContextStatusState::Error;
                compact.project = None;
                compact.plan_entry_count = 0;
                compact.receipt_file_count = 0;
                compact.captured_bytes = 0;
                compact.estimated_tokens = 0;
                files.clear();
                compact.error = Some(StartupContextFailure {
                    operation: StartupContextOperation::Status,
                    kind: StartupContextFailureKind::PlanStorage,
                    message: "fixture plan storage is unreadable".to_string(),
                    retryable: true,
                    issues: Vec::new(),
                });
            }
            "editor-loading" => {
                compact.state = StartupContextStatusState::Unprepared;
                compact.receipt_file_count = 0;
                compact.captured_bytes = 0;
                compact.estimated_tokens = 0;
                files.clear();
            }
            "editor-empty" => {
                compact.state = StartupContextStatusState::Empty;
                compact.plan_entry_count = 0;
                compact.receipt_file_count = 0;
                compact.captured_bytes = 0;
                compact.estimated_tokens = 0;
                files.clear();
            }
            "editor-busy" => {
                compact.lease = StartupContextLeaseAvailability::Busy {
                    owner: Some(StartupContextLeaseOwnerSnapshot {
                        server_name: "fixture-server".to_string(),
                        session_id: "fixture-owner".to_string(),
                        acquired_at: now,
                        renewed_at: now,
                        expires_at: now + chrono::Duration::minutes(2),
                    }),
                };
            }
            "editor-populated"
            | "editor-stale"
            | "editor-invalid"
            | "editor-external"
            | "editor-apply-review"
            | "editor-apply-external" => {}
            "editor-apply-review-late" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "editor-apply-queued" | "editor-apply-applying" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                compact.pending_update_count = 1;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "editor-apply-recovery" | "editor-apply-partial" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                compact.plan_revision = 8;
                compact.pending_update_count = 1;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "editor-apply-success" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                compact.plan_revision = 8;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "editor-apply-failed" | "editor-apply-canceled" => {
                compact.state = StartupContextStatusState::ProviderAccepted;
                files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            }
            "loading" | "unsupported" => unreachable!(),
            _ => return Err(format!("unhandled Startup Context fixture {name:?}")),
        }

        if name == "editor-stale" {
            compact.state = StartupContextStatusState::ProviderAccepted;
            compact.stale_file_count = 1;
            files[0].delivery_state = StartupContextDeliveryState::ProviderAccepted;
            files[0].latest_observation = StartupContextObservedState::Changed {
                sha256: "fedcba9876543210".repeat(4),
                bytes: 91_240,
            };
            files[0].notification_count = 2;
        }

        self.startup_context_ui.availability = StartupContextAvailability::Available;
        self.startup_context_ui.compact = Some(compact.clone());
        self.startup_context_ui.detail = Some(StartupContextStatusSnapshot {
            compact,
            total_files: files.len(),
            file_page_start: 0,
            file_page_end: files.len(),
            next_file_page_start: None,
            files,
            total_issues: issues.len(),
            issue_page_start: 0,
            issue_page_end: issues.len(),
            next_issue_page_start: None,
            issues,
        });
        if name.starts_with("editor-") {
            let receipt = self
                .startup_context_ui
                .detail
                .as_ref()
                .map(|detail| detail.files.clone())
                .unwrap_or_default();
            self.startup_context_ui.editor = Some(RefCell::new(
                StartupContextEditor::debug_fixture(name, session_id, receipt),
            ));
            self.startup_context_ui.overlay_scroll = Some(0);
        }
        if name == "blocked-action" {
            self.startup_context_ui.action_required = Some(StartupContextActionRequired {
                kind: StartupContextActionKind::RequirementsUnresolved,
                prompt_disposition: StartupContextPromptDisposition::RolledBack,
                pending_input: None,
                detail:
                    "Synthetic request was not sent because one required startup file is missing."
                        .to_string(),
            });
            self.startup_context_ui.overlay_scroll = Some(0);
        }
        Ok(())
    }

    pub(in crate::tui::app) fn begin_remote_startup_context_session(&mut self, session_id: &str) {
        self.startup_context_ui.begin_session(session_id);
    }

    pub(in crate::tui::app) fn accept_remote_startup_context_history(
        &mut self,
        session_id: &str,
        status: Option<StartupContextCompactStatus>,
    ) {
        self.startup_context_ui.accept_history(session_id, status);
    }

    pub(in crate::tui::app) fn open_startup_context_details(&mut self) {
        if !self.is_remote {
            self.set_status_notice("Startup Context details require the shared-server TUI");
            return;
        }
        let Some(session_id) = self.active_client_session_id().map(str::to_string) else {
            self.set_status_notice("Startup Context editor requires an active session");
            return;
        };
        self.startup_context_ui.overlay_scroll = Some(0);
        self.startup_context_ui.queue_full_status();
        match self.startup_context_ui.editor.as_ref() {
            Some(editor) if editor.borrow().session_id() == session_id => {
                editor.borrow_mut().reopen();
            }
            _ => {
                let editor = if self.startup_context_ui.availability
                    == StartupContextAvailability::Unsupported
                {
                    StartupContextEditor::unsupported(session_id)
                } else {
                    StartupContextEditor::new(session_id, self.startup_context_ui.detail.as_ref())
                };
                self.startup_context_ui.editor = Some(RefCell::new(editor));
            }
        }
        self.force_full_redraw = true;
    }

    pub(in crate::tui::app) fn close_startup_context_details(&mut self) {
        if let Some(editor) = self.startup_context_ui.editor.as_ref() {
            editor.borrow_mut().close();
        }
        self.startup_context_ui.overlay_scroll = None;
        self.startup_context_ui.action_required = None;
        self.force_full_redraw = true;
    }

    pub(in crate::tui::app) fn handle_startup_context_details_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) {
        if let Some(editor) = self.startup_context_ui.editor.as_ref() {
            let close = editor.borrow_mut().handle_key(code, modifiers);
            if close {
                self.startup_context_ui.overlay_scroll = None;
                self.startup_context_ui.action_required = None;
            }
            self.force_full_redraw = true;
            return;
        }
        let scroll = self.startup_context_ui.overlay_scroll.unwrap_or(0);
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_startup_context_details(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_startup_context_details(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_startup_context_details(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_startup_context_details(20),
            KeyCode::PageUp => self.scroll_startup_context_details(-20),
            KeyCode::Home | KeyCode::Char('g') => self.set_startup_context_details_scroll(0),
            KeyCode::End | KeyCode::Char('G') => {
                self.set_startup_context_details_scroll(usize::MAX)
            }
            KeyCode::Char('r') => self.refresh_startup_context_details(),
            KeyCode::Enter | KeyCode::Char('e') => self.set_status_notice(
                "Startup Context file-selection editor is not active in this build; repair the listed file or /clear after repair",
            ),
            _ => self.set_startup_context_details_scroll(scroll),
        }
    }

    pub(in crate::tui::app) fn handle_startup_context_editor_mouse(
        &mut self,
        mouse: MouseEvent,
    ) -> bool {
        let Some(editor) = self.startup_context_ui.editor.as_ref() else {
            return false;
        };
        let close = editor.borrow_mut().handle_mouse(mouse);
        if close {
            self.startup_context_ui.overlay_scroll = None;
        }
        self.force_full_redraw = true;
        true
    }

    pub(in crate::tui::app) fn refresh_startup_context_details(&mut self) {
        self.startup_context_ui.queue_full_status();
        self.set_status_notice("Refreshing Startup Context status...");
    }

    pub(in crate::tui::app) fn scroll_startup_context_details(&mut self, delta: i16) {
        let Some(scroll) = self.startup_context_ui.overlay_scroll.as_mut() else {
            return;
        };
        *scroll = if delta < 0 {
            scroll.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            scroll.saturating_add(delta as usize)
        };
    }

    pub(in crate::tui::app) fn set_startup_context_details_scroll(&mut self, scroll: usize) {
        if self.startup_context_ui.overlay_scroll.is_some() {
            self.startup_context_ui.overlay_scroll = Some(scroll);
        }
    }

    pub(in crate::tui::app) fn mark_startup_context_dispatched(&mut self) {
        let active_session_id = self
            .active_client_session_id()
            .unwrap_or_default()
            .to_string();
        let Some(status) = self.startup_context_ui.compact.as_mut() else {
            return;
        };
        if status.session_id == active_session_id
            && status.state == StartupContextStatusState::Prepared
        {
            status.state = StartupContextStatusState::Dispatched;
        }
    }

    pub(in crate::tui::app) fn mark_startup_context_provider_accepted(&mut self) {
        let active_session_id = self
            .active_client_session_id()
            .unwrap_or_default()
            .to_string();
        let Some(status) = self.startup_context_ui.compact.as_mut() else {
            return;
        };
        if status.session_id == active_session_id
            && matches!(
                status.state,
                StartupContextStatusState::Prepared | StartupContextStatusState::Dispatched
            )
        {
            status.state = StartupContextStatusState::ProviderAccepted;
            if self.startup_context_ui.overlay_scroll.is_some() {
                self.startup_context_ui.queue_full_status();
            }
        }
    }

    pub(in crate::tui::app) fn queue_startup_context_status_refresh(&mut self) {
        if self.startup_context_ui.overlay_scroll.is_some() {
            self.startup_context_ui.queue_full_status();
        } else {
            self.startup_context_ui.queue_compact_status();
        }
    }

    pub(in crate::tui::app) async fn dispatch_remote_startup_context_request(
        &mut self,
        remote: &mut RemoteConnection,
    ) {
        if self.startup_context_ui.outstanding_request.is_none()
            && let Some(page) = self.startup_context_ui.pending_page.take()
            && let Some(session_id) = self.startup_context_ui.session_id.clone()
        {
            let id = remote.reserve_startup_context_request_id();
            self.startup_context_ui.outstanding_request = Some((id, session_id));
            let request = Request::GetStartupContextStatus {
                id,
                file_page_start: page.file_page_start,
                file_page_size: Some(page.page_size),
                issue_page_start: page.issue_page_start,
                issue_page_size: Some(page.page_size),
            };
            if let Err(error) = remote.send_reserved_startup_context_request(request).await {
                self.startup_context_ui.outstanding_request = None;
                self.startup_context_ui.pending_page = Some(page);
                self.set_status_notice(format!("Startup Context status request failed: {error}"));
            }
        }

        if let Some(editor) = self.startup_context_ui.editor.as_ref() {
            editor.borrow_mut().tick(Instant::now());
        }
        loop {
            let (action, session_id) = {
                let Some(editor) = self.startup_context_ui.editor.as_ref() else {
                    break;
                };
                let mut editor = editor.borrow_mut();
                (editor.take_action(), editor.session_id().to_string())
            };
            let Some(action) = action else {
                break;
            };
            let id = remote.reserve_startup_context_request_id();
            let (request, pending) = match action.clone() {
                StartupContextEditorAction::Open => (
                    Request::OpenStartupContextEditor { id },
                    StartupContextPendingRequest::Open { session_id },
                ),
                StartupContextEditorAction::Renew { lease } => (
                    Request::RenewStartupContextEditorLease {
                        id,
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                    },
                    StartupContextPendingRequest::Renew {
                        lease_id: lease.lease_id,
                    },
                ),
                StartupContextEditorAction::Close {
                    lease_id,
                    project_key_digest,
                } => (
                    Request::CloseStartupContextEditor {
                        id,
                        lease_id: lease_id.clone(),
                        project_key_digest,
                    },
                    StartupContextPendingRequest::Close { lease_id },
                ),
                StartupContextEditorAction::ListDirectory {
                    lease,
                    directory,
                    page_start,
                    generation,
                    bulk,
                } => (
                    Request::ListStartupContextDirectory {
                        id,
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                        directory: directory.clone(),
                        page_start,
                        page_size: Some(crate::protocol::STARTUP_CONTEXT_DIRECTORY_MAX_PAGE_SIZE),
                    },
                    StartupContextPendingRequest::Directory {
                        lease_id: lease.lease_id,
                        directory,
                        page_start,
                        generation,
                        bulk,
                    },
                ),
                StartupContextEditorAction::Search {
                    lease,
                    query,
                    generation,
                } => (
                    Request::SearchStartupContextFiles {
                        id,
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                        query: query.clone(),
                        max_results: Some(crate::protocol::STARTUP_CONTEXT_SEARCH_MAX_RESULTS),
                    },
                    StartupContextPendingRequest::Search {
                        lease_id: lease.lease_id,
                        query,
                        generation,
                    },
                ),
                StartupContextEditorAction::CancelSearch { search_request_id } => (
                    Request::CancelStartupContextSearch {
                        id,
                        search_request_id,
                    },
                    StartupContextPendingRequest::CancelSearch { search_request_id },
                ),
                StartupContextEditorAction::PreviewFile {
                    lease,
                    path,
                    start_char,
                    generation,
                } => (
                    Request::PreviewStartupContextFile {
                        id,
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                        path: path.clone(),
                        start_char,
                        max_chars: Some(STARTUP_CONTEXT_EDITOR_PREVIEW_PAGE_CHARS),
                    },
                    StartupContextPendingRequest::Preview {
                        lease_id: lease.lease_id,
                        path,
                        start_char,
                        generation,
                    },
                ),
                StartupContextEditorAction::FileDetail {
                    receipt,
                    start_char,
                    generation,
                } => (
                    Request::GetStartupContextFileDetail {
                        id,
                        batch_id: receipt.batch_id.clone(),
                        spec_id: receipt.spec_id.clone(),
                        message_id: receipt.message_id,
                        expected_sha256: receipt.sha256,
                        start_char,
                        max_chars: Some(STARTUP_CONTEXT_EDITOR_PREVIEW_PAGE_CHARS),
                    },
                    StartupContextPendingRequest::Detail {
                        batch_id: receipt.batch_id,
                        spec_id: receipt.spec_id,
                        start_char,
                        generation,
                    },
                ),
                StartupContextEditorAction::PreviewSelection {
                    lease,
                    selection,
                    generation,
                    draft_generation,
                    purpose,
                } => (
                    Request::PreviewStartupContextSelection {
                        id,
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest.clone(),
                        expected_plan_revision: lease.plan_revision,
                        selection,
                    },
                    StartupContextPendingRequest::Selection {
                        lease_id: lease.lease_id,
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                        generation,
                        draft_generation,
                        purpose,
                    },
                ),
                StartupContextEditorAction::ApplySelection {
                    lease,
                    operation_id,
                    selection,
                    save_project_default,
                    draft_generation,
                } => (
                    Request::ApplyStartupContextSelection {
                        id,
                        operation_id: operation_id.clone(),
                        lease_id: lease.lease_id.clone(),
                        project_key_digest: lease.project_key_digest.clone(),
                        expected_plan_revision: lease.plan_revision,
                        selection,
                        save_project_default,
                    },
                    StartupContextPendingRequest::Apply {
                        operation_id,
                        lease_id: lease.lease_id,
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                        draft_generation,
                    },
                ),
                StartupContextEditorAction::CancelApply {
                    lease,
                    operation_id,
                } => (
                    Request::CancelStartupContextApply {
                        id,
                        operation_id: operation_id.clone(),
                        lease_id: lease.lease_id,
                        project_key_digest: lease.project_key_digest,
                        expected_plan_revision: lease.plan_revision,
                    },
                    StartupContextPendingRequest::CancelApply { operation_id },
                ),
                StartupContextEditorAction::GetApplyStatus { operation_id } => (
                    Request::GetStartupContextApplyStatus {
                        id,
                        operation_id: operation_id.clone(),
                    },
                    StartupContextPendingRequest::ApplyStatus { operation_id },
                ),
            };
            let Some(editor) = self.startup_context_ui.editor.as_ref() else {
                break;
            };
            editor.borrow_mut().register_pending(id, pending);
            if let Err(error) = remote.send_reserved_startup_context_request(request).await {
                editor.borrow_mut().reject_transport(
                    id,
                    format!("Startup Context editor request failed before confirmation: {error}"),
                );
                editor.borrow_mut().requeue_front(action);
                self.set_status_notice(format!("Startup Context editor transport failed: {error}"));
                break;
            }
        }
    }

    pub(in crate::tui::app) fn accept_remote_startup_context_status(
        &mut self,
        id: u64,
        snapshot: StartupContextStatusSnapshot,
        action_required: Option<StartupContextActionRequired>,
    ) -> bool {
        if action_required.as_ref().is_some_and(|action| {
            !self.accept_startup_context_action_required(id, &snapshot, action)
        }) {
            return false;
        }
        self.startup_context_ui
            .accept_snapshot(id, snapshot, action_required)
    }

    pub(in crate::tui::app) fn accept_remote_startup_context_failure(
        &mut self,
        id: u64,
        failure: crate::protocol::StartupContextFailure,
    ) -> bool {
        use crate::protocol::{StartupContextFailureKind, StartupContextOperation};
        if self
            .startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|editor| editor.borrow_mut().accept_failure(id, failure.clone()))
        {
            self.set_status_notice(format!("Startup Context: {}", failure.message));
            return true;
        }
        if failure.operation != StartupContextOperation::Status {
            self.set_status_notice(format!("Startup Context: {}", failure.message));
            return true;
        }
        let Some((expected_id, expected_session)) =
            self.startup_context_ui.outstanding_request.as_ref()
        else {
            return false;
        };
        if *expected_id != id || self.active_client_session_id() != Some(expected_session.as_str())
        {
            return false;
        }
        self.startup_context_ui.outstanding_request = None;
        if failure.kind == StartupContextFailureKind::Unsupported {
            self.startup_context_ui.availability = StartupContextAvailability::Unsupported;
            self.startup_context_ui.compact = None;
        } else if let Some(status) = self.startup_context_ui.compact.as_mut() {
            status.state = StartupContextStatusState::Error;
            status.error = Some(failure.clone());
        }
        self.set_status_notice(format!("Startup Context: {}", failure.message));
        true
    }

    pub(in crate::tui::app) fn accept_startup_context_editor_opened(
        &mut self,
        id: u64,
        editor: crate::protocol::StartupContextEditorSnapshot,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_opened(id, editor))
    }

    pub(in crate::tui::app) fn accept_startup_context_editor_busy(
        &mut self,
        id: u64,
        owner: Option<crate::protocol::StartupContextLeaseOwnerSnapshot>,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_busy(id, owner))
    }

    pub(in crate::tui::app) fn accept_startup_context_editor_renewed(
        &mut self,
        id: u64,
        lease: crate::protocol::StartupContextLeaseSnapshot,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_renewed(id, lease))
    }

    pub(in crate::tui::app) fn accept_startup_context_editor_closed(
        &mut self,
        id: u64,
        lease_id: &str,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_closed(id, lease_id))
    }

    pub(in crate::tui::app) fn accept_startup_context_directory_page(
        &mut self,
        id: u64,
        page: crate::protocol::StartupContextDirectoryPage,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_directory(id, page))
    }

    pub(in crate::tui::app) fn accept_startup_context_search_results(
        &mut self,
        id: u64,
        results: crate::protocol::StartupContextSearchResults,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_search(id, results))
    }

    pub(in crate::tui::app) fn accept_startup_context_search_canceled(
        &mut self,
        id: u64,
        search_request_id: u64,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| {
                state
                    .borrow_mut()
                    .accept_search_canceled(id, search_request_id)
            })
    }

    pub(in crate::tui::app) fn accept_startup_context_file_preview(
        &mut self,
        id: u64,
        preview: crate::protocol::StartupContextFilePreview,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_preview(id, preview))
    }

    pub(in crate::tui::app) fn accept_startup_context_file_detail(
        &mut self,
        id: u64,
        detail: crate::protocol::StartupContextFileDetail,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_detail(id, detail))
    }

    pub(in crate::tui::app) fn accept_startup_context_selection_preview(
        &mut self,
        id: u64,
        preview: crate::protocol::StartupContextSelectionPreview,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_selection(id, preview))
    }

    pub(in crate::tui::app) fn accept_startup_context_apply_status(
        &mut self,
        id: u64,
        status: crate::protocol::StartupContextApplyStatus,
    ) -> bool {
        self.startup_context_ui
            .editor
            .as_ref()
            .is_some_and(|state| state.borrow_mut().accept_apply_status(id, status))
    }

    fn accept_startup_context_action_required(
        &mut self,
        request_id: u64,
        snapshot: &StartupContextStatusSnapshot,
        action: &StartupContextActionRequired,
    ) -> bool {
        if self.active_client_session_id() != Some(snapshot.compact.session_id.as_str())
            || self.current_message_id != Some(request_id)
        {
            return false;
        }
        let pending = self.pending_composer_input.as_ref();
        let exact_pending_match = pending.is_some_and(|pending| {
            pending.request_id == Some(request_id)
                && action.pending_input.as_ref().is_some_and(|metadata| {
                    metadata.matches(request_id, &pending.expanded, pending.image_count)
                })
        });
        let pending_raw_input = pending
            .map(|pending| pending.raw_input.clone())
            .unwrap_or_default();
        let pending_expanded = pending
            .map(|pending| pending.expanded.clone())
            .unwrap_or_default();
        let pending_image_count = pending.map_or(0, |pending| pending.image_count);
        let images = self
            .rate_limit_pending_message
            .as_ref()
            .filter(|message| {
                message.content == pending_expanded && message.images.len() == pending_image_count
            })
            .map(|message| message.images.clone())
            .unwrap_or_default();

        if action.prompt_disposition == StartupContextPromptDisposition::RolledBack
            && exact_pending_match
            && let Some(index) = self
                .display_messages
                .iter()
                .rposition(|message| message.role == "user" && message.content == pending_raw_input)
        {
            self.remove_display_message(index);
        }

        self.rate_limit_pending_message = None;
        self.clear_pending_fallback_offer();
        self.current_message_id = None;
        self.is_processing = false;
        self.pending_turn = false;
        self.stream_message_ended = false;
        self.processing_started = None;
        self.replay_processing_started_ms = None;
        self.replay_elapsed_override = None;
        self.remote_resume_activity = None;
        self.batch_progress = None;
        self.status = ProcessingStatus::Idle;

        match action.prompt_disposition {
            StartupContextPromptDisposition::RolledBack if exact_pending_match => {
                self.restore_blocked_composer_input(images);
            }
            StartupContextPromptDisposition::RolledBack => {
                self.pending_composer_input = None;
                self.last_submitted_input = None;
                self.set_status_notice(
                    "Startup Context blocked the request; exact prompt restoration unavailable",
                );
            }
            StartupContextPromptDisposition::Retained => {
                self.pending_composer_input = None;
                self.last_submitted_input = None;
                self.set_status_notice(
                    "Startup Context blocked the request; authoritative turn retained",
                );
            }
        }
        self.push_display_message(DisplayMessage::system(
            "Startup Context blocked provider dispatch. Review the preserved resolution details in /startup."
                .to_string(),
        ));
        true
    }

    pub(in crate::tui::app) fn consume_startup_context_terminal_error(&mut self, id: u64) -> bool {
        if self.startup_context_ui.action_request_id != Some(id) {
            return false;
        }
        self.startup_context_ui.action_request_id = None;
        true
    }

    pub(in crate::tui::app) fn startup_context_availability(&self) -> StartupContextAvailability {
        self.startup_context_ui.availability
    }

    pub(in crate::tui::app) fn startup_context_compact_status(
        &self,
    ) -> Option<&StartupContextCompactStatus> {
        self.startup_context_ui.compact.as_ref()
    }

    pub(in crate::tui::app) fn startup_context_detail(
        &self,
    ) -> Option<&StartupContextStatusSnapshot> {
        self.startup_context_ui.detail.as_ref()
    }

    pub(in crate::tui::app) fn startup_context_action_required(
        &self,
    ) -> Option<&StartupContextActionRequired> {
        self.startup_context_ui.action_required.as_ref()
    }

    pub(in crate::tui::app) fn startup_context_overlay_scroll(&self) -> Option<usize> {
        self.startup_context_ui.overlay_scroll
    }

    pub(in crate::tui::app) fn startup_context_editor(
        &self,
    ) -> Option<&RefCell<StartupContextEditor>> {
        self.startup_context_ui
            .editor
            .as_ref()
            .filter(|editor| editor.borrow().is_visible())
    }

    pub(in crate::tui::app) fn startup_context_debug_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "availability": format!("{:?}", self.startup_context_ui.availability),
            "session_id": self.startup_context_ui.session_id,
            "state": self.startup_context_ui.compact.as_ref().map(|status| format!("{:?}", status.state)),
            "files": self.startup_context_ui.compact.as_ref().map(|status| status.receipt_file_count),
            "issues": self.startup_context_ui.compact.as_ref().map(|status| status.blocked_issue_count),
            "overlay_open": self.startup_context_ui.overlay_scroll.is_some(),
            "detail_files": self.startup_context_ui.detail.as_ref().map(|detail| detail.files.len()),
            "detail_issues": self.startup_context_ui.detail.as_ref().map(|detail| detail.issues.len()),
            "action_required": self.startup_context_ui.action_required.as_ref().map(|action| action.kind),
            "editor": self.startup_context_ui.editor.as_ref().map(|editor| editor.borrow().debug_summary()),
        })
    }

    #[cfg(test)]
    pub(in crate::tui::app) fn expect_startup_context_status_response_for_test(
        &mut self,
        id: u64,
        session_id: &str,
    ) {
        self.startup_context_ui.outstanding_request = Some((id, session_id.to_string()));
    }
}
