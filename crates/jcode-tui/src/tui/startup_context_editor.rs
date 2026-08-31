use crate::protocol::{
    StartupContextApplyStatus, StartupContextDirectoryEntry, StartupContextDirectoryEntryKind,
    StartupContextDirectoryPage, StartupContextEditorSnapshot, StartupContextFailure,
    StartupContextFileDetail, StartupContextFileIssueKind, StartupContextFileIssueSnapshot,
    StartupContextFilePreview, StartupContextFileReceiptSnapshot, StartupContextLeaseOwnerSnapshot,
    StartupContextLeaseSnapshot, StartupContextObservedState, StartupContextPathClassification,
    StartupContextSearchResults, StartupContextSelectionEntrySnapshot,
    StartupContextSelectionInput, StartupContextSelectionPreview, StartupContextStatusSnapshot,
    StartupContextStatusState,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

mod apply;
mod render;

use apply::{
    ApplyIntent, ApplyOverlay, ApplySelectionPurpose, ApplyTracking, EditorAuthorityRefresh,
};

const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const STARTUP_CONTEXT_EDITOR_PREVIEW_PAGE_CHARS: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupContextEditorPane {
    Browser,
    Selection,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionView {
    Draft,
    Receipt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditorPhase {
    Opening,
    Ready,
    Busy {
        owner: Option<StartupContextLeaseOwnerSnapshot>,
    },
    Error(StartupContextFailure),
    Unsupported,
    Closing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InputMode {
    Search { value: String },
    ExternalPath { value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupContextEditorAction {
    Open,
    Renew {
        lease: StartupContextLeaseSnapshot,
    },
    Close {
        lease_id: String,
        project_key_digest: String,
    },
    ListDirectory {
        lease: StartupContextLeaseSnapshot,
        directory: String,
        page_start: usize,
        generation: u64,
        bulk: bool,
    },
    Search {
        lease: StartupContextLeaseSnapshot,
        query: String,
        generation: u64,
    },
    CancelSearch {
        search_request_id: u64,
    },
    PreviewFile {
        lease: StartupContextLeaseSnapshot,
        path: String,
        start_char: usize,
        generation: u64,
    },
    FileDetail {
        receipt: StartupContextFileReceiptSnapshot,
        start_char: usize,
        generation: u64,
    },
    PreviewSelection {
        lease: StartupContextLeaseSnapshot,
        selection: Vec<StartupContextSelectionInput>,
        generation: u64,
        draft_generation: u64,
        purpose: ApplySelectionPurpose,
    },
    ApplySelection {
        lease: StartupContextLeaseSnapshot,
        operation_id: String,
        selection: Vec<StartupContextSelectionInput>,
        save_project_default: bool,
        draft_generation: u64,
    },
    CancelApply {
        lease: StartupContextLeaseSnapshot,
        operation_id: String,
    },
    GetApplyStatus {
        operation_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupContextPendingRequest {
    Open {
        session_id: String,
    },
    Renew {
        lease_id: String,
    },
    Close {
        lease_id: String,
    },
    Directory {
        lease_id: String,
        directory: String,
        page_start: usize,
        generation: u64,
        bulk: bool,
    },
    Search {
        lease_id: String,
        query: String,
        generation: u64,
    },
    CancelSearch {
        search_request_id: u64,
    },
    Preview {
        lease_id: String,
        path: String,
        start_char: usize,
        generation: u64,
    },
    Detail {
        batch_id: String,
        spec_id: String,
        start_char: usize,
        generation: u64,
    },
    Selection {
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        generation: u64,
        draft_generation: u64,
        purpose: ApplySelectionPurpose,
    },
    Apply {
        operation_id: String,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        draft_generation: u64,
    },
    CancelApply {
        operation_id: String,
    },
    ApplyStatus {
        operation_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DraftEntry {
    local_id: u64,
    input: StartupContextSelectionInput,
    normalized_spec_id: Option<String>,
    logical_path: String,
    resolved_path: Option<String>,
    classification: Option<StartupContextPathClassification>,
    bytes: Option<u64>,
    estimated_tokens: Option<u64>,
    requires_external_approval: bool,
    issue: Option<StartupContextFileIssueSnapshot>,
}

impl DraftEntry {
    fn from_plan(local_id: u64, entry: &crate::protocol::StartupContextPlanEntrySnapshot) -> Self {
        Self {
            local_id,
            input: StartupContextSelectionInput {
                existing_spec_id: Some(entry.spec_id.clone()),
                path: entry.logical_path.clone(),
                approved_external_target: entry.approved_external_target.clone(),
            },
            normalized_spec_id: Some(entry.spec_id.clone()),
            logical_path: entry.logical_path.clone(),
            resolved_path: entry.approved_external_target.clone(),
            classification: entry
                .approved_external_target
                .as_ref()
                .map(|_| StartupContextPathClassification::External),
            bytes: None,
            estimated_tokens: None,
            requires_external_approval: false,
            issue: None,
        }
    }

    fn pending(local_id: u64, path: String) -> Self {
        Self {
            local_id,
            input: StartupContextSelectionInput {
                existing_spec_id: None,
                path: path.clone(),
                approved_external_target: None,
            },
            normalized_spec_id: None,
            logical_path: path,
            resolved_path: None,
            classification: None,
            bytes: None,
            estimated_tokens: None,
            requires_external_approval: false,
            issue: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BrowserState {
    directory: String,
    entries: Vec<StartupContextDirectoryEntry>,
    cursor: usize,
    next_page_start: Option<usize>,
    total_entries: usize,
    search_query: Option<String>,
    search_results: Vec<StartupContextDirectoryEntry>,
    search_truncated: bool,
    generation: u64,
    loading: bool,
}

impl BrowserState {
    fn visible_entries(&self) -> &[StartupContextDirectoryEntry] {
        if self.search_query.is_some() {
            &self.search_results
        } else {
            &self.entries
        }
    }

    fn current(&self) -> Option<&StartupContextDirectoryEntry> {
        self.visible_entries().get(self.cursor)
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self
            .cursor
            .min(self.visible_entries().len().saturating_sub(1));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PreviewBuffer {
    path: Option<String>,
    resolved_path: Option<String>,
    classification: Option<StartupContextPathClassification>,
    sha256: Option<String>,
    bytes: Option<u64>,
    estimated_tokens: Option<u64>,
    total_chars: usize,
    content: String,
    next_start_char: Option<usize>,
    exact_receipt: bool,
    loading: bool,
    failure: Option<String>,
    generation: u64,
    scroll: usize,
}

impl PreviewBuffer {
    fn begin_current(&mut self, path: String, generation: u64) {
        *self = Self {
            path: Some(path),
            loading: true,
            generation,
            ..Self::default()
        };
    }

    fn begin_receipt(&mut self, receipt: &StartupContextFileReceiptSnapshot, generation: u64) {
        *self = Self {
            path: Some(receipt.logical_path.clone()),
            resolved_path: Some(receipt.resolved_path.clone()),
            classification: Some(receipt.classification),
            sha256: Some(receipt.sha256.clone()),
            bytes: Some(receipt.bytes),
            estimated_tokens: Some(receipt.estimated_tokens),
            exact_receipt: true,
            loading: true,
            generation,
            ..Self::default()
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    FocusBrowser(usize),
    OpenDirectory(usize),
    SelectBrowser(usize),
    FocusDraft(usize),
    MoveDraftUp(usize),
    MoveDraftDown(usize),
    RemoveDraft(usize),
    FocusReceipt(usize),
    LoadMorePreview,
    FocusPane(StartupContextEditorPane),
    StartSearch,
    StartExternal,
    ToggleReceipt,
    ApplySession,
    ApplyAndSave,
    ApplyConfirm,
    ApplyCancelLayer,
    ApplyCancelQueued,
    ApplyRetry,
    ApplyRefreshStatus,
    ApplyShowStatus,
    CloseEditor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HitRegion {
    rect: Rect,
    action: RowAction,
}

#[derive(Clone, Copy)]
struct EditorStyles {
    text: Style,
    dim: Style,
    accent: Color,
    warn: Style,
    error: Style,
    good: Style,
}

#[derive(Debug)]
pub(crate) struct StartupContextEditor {
    session_id: String,
    visible: bool,
    phase: EditorPhase,
    editor: Option<StartupContextEditorSnapshot>,
    saved_default: Vec<crate::protocol::StartupContextPlanEntrySnapshot>,
    receipt: Vec<StartupContextFileReceiptSnapshot>,
    draft: Vec<DraftEntry>,
    next_local_id: u64,
    active_pane: StartupContextEditorPane,
    selection_view: SelectionView,
    browser: BrowserState,
    bulk_directory: Option<String>,
    bulk_entries: Vec<StartupContextDirectoryEntry>,
    bulk_generation: u64,
    draft_cursor: usize,
    receipt_cursor: usize,
    preview: PreviewBuffer,
    input_mode: Option<InputMode>,
    notice: Option<String>,
    batch_issues: Vec<StartupContextFileIssueSnapshot>,
    draft_generation: u64,
    selection_generation: u64,
    preview_generation: u64,
    apply_overlay: Option<ApplyOverlay>,
    apply_tracking: Option<ApplyTracking>,
    authority_refresh: EditorAuthorityRefresh,
    renew_due: Option<Instant>,
    pending: HashMap<u64, StartupContextPendingRequest>,
    queued: VecDeque<StartupContextEditorAction>,
    hit_regions: Vec<HitRegion>,
}

impl StartupContextEditor {
    pub(crate) fn new(session_id: String, status: Option<&StartupContextStatusSnapshot>) -> Self {
        let receipt = status.map(|value| value.files.clone()).unwrap_or_default();
        let mut value = Self {
            session_id,
            visible: true,
            phase: EditorPhase::Opening,
            editor: None,
            saved_default: Vec::new(),
            receipt,
            draft: Vec::new(),
            next_local_id: 1,
            active_pane: StartupContextEditorPane::Browser,
            selection_view: SelectionView::Draft,
            browser: BrowserState::default(),
            bulk_directory: None,
            bulk_entries: Vec::new(),
            bulk_generation: 0,
            draft_cursor: 0,
            receipt_cursor: 0,
            preview: PreviewBuffer::default(),
            input_mode: None,
            notice: None,
            batch_issues: Vec::new(),
            draft_generation: 0,
            selection_generation: 0,
            preview_generation: 0,
            apply_overlay: None,
            apply_tracking: None,
            authority_refresh: EditorAuthorityRefresh::None,
            renew_due: None,
            pending: HashMap::new(),
            queued: VecDeque::new(),
            hit_regions: Vec::new(),
        };
        value.queued.push_back(StartupContextEditorAction::Open);
        value
    }

    pub(crate) fn unsupported(session_id: String) -> Self {
        let mut value = Self::new(session_id, None);
        value.phase = EditorPhase::Unsupported;
        value.queued.clear();
        value
    }

    pub(crate) fn debug_fixture(
        name: &str,
        session_id: String,
        receipt: Vec<StartupContextFileReceiptSnapshot>,
    ) -> Self {
        use crate::protocol::{
            StartupContextPlanEntrySnapshot, StartupContextProjectKind,
            StartupContextProjectSnapshot,
        };
        let now = chrono::Utc::now();
        let lease = StartupContextLeaseSnapshot {
            lease_id: "fixture-startup-lease".to_string(),
            project_key_digest: "fixture-project".to_string(),
            owner_session_id: session_id.clone(),
            acquired_at: now,
            renewed_at: now,
            expires_at: now + chrono::Duration::minutes(2),
            plan_revision: 7,
        };
        let mut value = Self::new(session_id, None);
        value.queued.clear();
        value.receipt = receipt;
        if name == "editor-loading" {
            return value;
        }
        if name == "editor-busy" {
            value.phase = EditorPhase::Busy {
                owner: Some(StartupContextLeaseOwnerSnapshot {
                    server_name: "fixture-server".to_string(),
                    session_id: "fixture-owner".to_string(),
                    acquired_at: now,
                    renewed_at: now,
                    expires_at: now + chrono::Duration::minutes(2),
                }),
            };
            return value;
        }

        let plan_entries = if name == "editor-empty" {
            Vec::new()
        } else {
            vec![
                StartupContextPlanEntrySnapshot {
                    spec_id: "fixture-plan-spec".to_string(),
                    logical_path: "docs/PLAN.md".to_string(),
                    approved_external_target: None,
                },
                StartupContextPlanEntrySnapshot {
                    spec_id: "fixture-progress-spec".to_string(),
                    logical_path: "docs/PROGRESS.md".to_string(),
                    approved_external_target: None,
                },
            ]
        };
        value.editor = Some(StartupContextEditorSnapshot {
            lease,
            project: StartupContextProjectSnapshot {
                key_digest: "fixture-project".to_string(),
                kind: StartupContextProjectKind::Git,
                active_root: "/fixture/project".to_string(),
            },
            plan_revision: 7,
            plan_entries: plan_entries.clone(),
        });
        value.saved_default = plan_entries.clone();
        value.draft = plan_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut draft = DraftEntry::from_plan((index + 1) as u64, entry);
                draft.resolved_path = Some(format!("/fixture/project/{}", entry.logical_path));
                draft.classification = Some(StartupContextPathClassification::Project);
                draft.bytes = Some(4_096 + index as u64 * 2_048);
                draft.estimated_tokens = Some(1_000 + index as u64 * 500);
                draft
            })
            .collect();
        value.next_local_id = value.draft.len() as u64 + 1;
        value.browser.entries = vec![
            StartupContextDirectoryEntry {
                name: "docs".to_string(),
                project_relative_path: "docs".to_string(),
                resolved_path: "/fixture/project/docs".to_string(),
                path_valid_utf8: true,
                kind: StartupContextDirectoryEntryKind::Directory,
                classification: StartupContextPathClassification::Project,
                navigable: true,
                bytes: None,
                selected_spec_id: None,
            },
            StartupContextDirectoryEntry {
                name: "Cargo.toml".to_string(),
                project_relative_path: "Cargo.toml".to_string(),
                resolved_path: "/fixture/project/Cargo.toml".to_string(),
                path_valid_utf8: true,
                kind: StartupContextDirectoryEntryKind::File,
                classification: StartupContextPathClassification::Project,
                navigable: false,
                bytes: Some(2_048),
                selected_spec_id: None,
            },
            StartupContextDirectoryEntry {
                name: "README.md".to_string(),
                project_relative_path: "README.md".to_string(),
                resolved_path: "/fixture/project/README.md".to_string(),
                path_valid_utf8: true,
                kind: StartupContextDirectoryEntryKind::File,
                classification: StartupContextPathClassification::Project,
                navigable: false,
                bytes: Some(8_192),
                selected_spec_id: None,
            },
        ];
        value.browser.total_entries = value.browser.entries.len();
        value.phase = EditorPhase::Ready;
        value.preview = PreviewBuffer {
            path: Some("Cargo.toml".to_string()),
            resolved_path: Some("/fixture/project/Cargo.toml".to_string()),
            classification: Some(StartupContextPathClassification::Project),
            sha256: Some("0123456789abcdef".repeat(4)),
            bytes: Some(2_048),
            estimated_tokens: Some(510),
            total_chars: 2_048,
            content: "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n".to_string(),
            next_start_char: None,
            exact_receipt: false,
            loading: false,
            failure: None,
            generation: 1,
            scroll: 0,
        };
        if name == "editor-invalid" {
            let id = value.alloc_local_id();
            let mut invalid = DraftEntry::pending(id, "docs/MISSING.md".to_string());
            invalid.issue = Some(StartupContextFileIssueSnapshot {
                input_index: Some(value.draft.len() as u32),
                spec_id: None,
                logical_path: Some("docs/MISSING.md".to_string()),
                kind: StartupContextFileIssueKind::Missing,
            });
            value.draft.push(invalid);
            value.draft_cursor = value.draft.len() - 1;
            value.active_pane = StartupContextEditorPane::Selection;
            value.queue_preview_for_focus();
        } else if matches!(name, "editor-external" | "editor-apply-external") {
            let id = value.alloc_local_id();
            let mut external = DraftEntry::pending(id, "/Users/mirza/private/NOTES.md".to_string());
            external.resolved_path = Some("/Users/mirza/private/NOTES.md".to_string());
            external.classification = Some(StartupContextPathClassification::External);
            external.requires_external_approval = true;
            external.issue = Some(StartupContextFileIssueSnapshot {
                input_index: Some(value.draft.len() as u32),
                spec_id: None,
                logical_path: Some(external.logical_path.clone()),
                kind: StartupContextFileIssueKind::ExternalApprovalRequired {
                    resolved_target: "/Users/mirza/private/NOTES.md".to_string(),
                },
            });
            value.draft.push(external);
            value.draft_cursor = value.draft.len() - 1;
            value.active_pane = StartupContextEditorPane::Selection;
            value.queue_preview_for_focus();
        }
        value.install_debug_apply_fixture(name);
        value
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn take_action(&mut self) -> Option<StartupContextEditorAction> {
        self.queued.pop_front()
    }

    pub(crate) fn requeue_front(&mut self, action: StartupContextEditorAction) {
        self.queued.push_front(action);
    }

    pub(crate) fn register_pending(&mut self, id: u64, pending: StartupContextPendingRequest) {
        self.pending.insert(id, pending);
    }

    pub(crate) fn reject_transport(&mut self, id: u64, message: String) -> bool {
        if self.pending.remove(&id).is_none() {
            return false;
        }
        self.notice = Some(message);
        true
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        if !self.visible || !matches!(self.phase, EditorPhase::Ready) {
            return;
        }
        if self.renew_due.is_some_and(|due| now >= due)
            && !self
                .pending
                .values()
                .any(|pending| matches!(pending, StartupContextPendingRequest::Renew { .. }))
            && let Some(lease) = self.lease().cloned()
        {
            self.renew_due = Some(now + LEASE_RENEW_INTERVAL);
            self.queued
                .push_back(StartupContextEditorAction::Renew { lease });
        }
    }

    pub(crate) fn refresh_receipt(&mut self, status: &StartupContextStatusSnapshot) {
        self.receipt = status.files.clone();
        self.receipt_cursor = self
            .receipt_cursor
            .min(self.receipt.len().saturating_sub(1));
    }

    fn lease(&self) -> Option<&StartupContextLeaseSnapshot> {
        self.editor.as_ref().map(|editor| &editor.lease)
    }

    pub(crate) fn close(&mut self) {
        self.visible = false;
        self.input_mode = None;
        self.apply_overlay = None;
        self.authority_refresh = EditorAuthorityRefresh::None;
        let active_searches = self
            .pending
            .iter()
            .filter_map(|(request_id, pending)| {
                matches!(pending, StartupContextPendingRequest::Search { .. })
                    .then_some(*request_id)
            })
            .collect::<Vec<_>>();
        let durable_apply_actions = self
            .queued
            .drain(..)
            .filter(|action| {
                matches!(
                    action,
                    StartupContextEditorAction::ApplySelection { .. }
                        | StartupContextEditorAction::CancelApply { .. }
                        | StartupContextEditorAction::GetApplyStatus { .. }
                )
            })
            .collect::<VecDeque<_>>();
        self.queued = durable_apply_actions;
        for search_request_id in active_searches {
            self.queued
                .push_back(StartupContextEditorAction::CancelSearch { search_request_id });
        }
        if let Some(lease) = self.lease().cloned() {
            self.phase = EditorPhase::Closing;
            self.queued.push_back(StartupContextEditorAction::Close {
                lease_id: lease.lease_id,
                project_key_digest: lease.project_key_digest,
            });
        }
    }

    pub(crate) fn reopen(&mut self) {
        self.visible = true;
        if self.editor.is_none()
            && matches!(
                self.phase,
                EditorPhase::Opening | EditorPhase::Busy { .. } | EditorPhase::Error(_)
            )
        {
            self.phase = EditorPhase::Opening;
            if !self
                .pending
                .values()
                .any(|pending| matches!(pending, StartupContextPendingRequest::Open { .. }))
                && !self
                    .queued
                    .iter()
                    .any(|action| matches!(action, StartupContextEditorAction::Open))
            {
                self.queued.push_back(StartupContextEditorAction::Open);
            }
        }
    }

    pub(crate) fn restart_after_reconnect(&mut self) {
        let was_visible = self.visible;
        self.phase = EditorPhase::Opening;
        self.editor = None;
        self.browser = BrowserState::default();
        self.preview = PreviewBuffer::default();
        self.input_mode = None;
        self.pending.clear();
        self.queued.clear();
        self.renew_due = None;
        self.authority_refresh = EditorAuthorityRefresh::None;
        if was_visible {
            self.queued.push_back(StartupContextEditorAction::Open);
            self.notice = Some("Reacquiring editor lease after reconnect…".to_string());
        }
        self.reconnect_apply_status();
    }

    pub(crate) fn accept_opened(&mut self, id: u64, editor: StartupContextEditorSnapshot) -> bool {
        let Some(StartupContextPendingRequest::Open { session_id }) = self.pending.remove(&id)
        else {
            return false;
        };
        if session_id != self.session_id {
            return false;
        }
        let preserve_unsaved_draft = self.is_dirty();
        self.saved_default = editor.plan_entries.clone();
        if !preserve_unsaved_draft {
            self.draft = editor
                .plan_entries
                .iter()
                .map(|entry| {
                    let id = self.alloc_local_id();
                    DraftEntry::from_plan(id, entry)
                })
                .collect();
            self.draft_generation = self.draft_generation.saturating_add(1);
        }
        self.editor = Some(editor);
        self.phase = EditorPhase::Ready;
        self.authority_refresh = EditorAuthorityRefresh::None;
        self.renew_due = Some(Instant::now() + LEASE_RENEW_INTERVAL);
        self.browser = BrowserState::default();
        self.queue_directory(String::new(), 0, true);
        self.queue_selection_preview();
        true
    }

    pub(crate) fn accept_busy(
        &mut self,
        id: u64,
        owner: Option<StartupContextLeaseOwnerSnapshot>,
    ) -> bool {
        let Some(StartupContextPendingRequest::Open { session_id }) = self.pending.remove(&id)
        else {
            return false;
        };
        if session_id != self.session_id {
            return false;
        }
        self.phase = EditorPhase::Busy { owner };
        true
    }

    pub(crate) fn accept_renewed(&mut self, id: u64, lease: StartupContextLeaseSnapshot) -> bool {
        let Some(StartupContextPendingRequest::Renew { lease_id }) = self.pending.remove(&id)
        else {
            return false;
        };
        if lease_id != lease.lease_id
            || self.lease().map(|value| &value.lease_id) != Some(&lease_id)
        {
            return false;
        }
        if let Some(editor) = self.editor.as_mut() {
            editor.plan_revision = lease.plan_revision;
            editor.lease = lease;
        }
        self.renew_due = Some(Instant::now() + LEASE_RENEW_INTERVAL);
        true
    }

    pub(crate) fn accept_closed(&mut self, id: u64, lease_id: &str) -> bool {
        let Some(StartupContextPendingRequest::Close { lease_id: expected }) =
            self.pending.remove(&id)
        else {
            return false;
        };
        if expected != lease_id {
            return false;
        }
        self.phase = EditorPhase::Opening;
        self.editor = None;
        if self.visible {
            self.authority_refresh = EditorAuthorityRefresh::ReopenQueued;
            self.queued.push_back(StartupContextEditorAction::Open);
        } else {
            self.authority_refresh = EditorAuthorityRefresh::None;
        }
        true
    }

    pub(crate) fn accept_directory(&mut self, id: u64, page: StartupContextDirectoryPage) -> bool {
        let Some(StartupContextPendingRequest::Directory {
            lease_id,
            directory,
            page_start,
            generation,
            bulk,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        if self.lease().map(|lease| lease.lease_id.as_str()) != Some(lease_id.as_str())
            || page.directory != directory
            || page.page_start != page_start
        {
            return false;
        }
        if bulk {
            if generation != self.bulk_generation
                || self.bulk_directory.as_deref() != Some(directory.as_str())
            {
                return false;
            }
            if page_start == 0 {
                self.bulk_entries = page.entries;
            } else if page_start == self.bulk_entries.len() {
                self.bulk_entries.extend(page.entries);
            } else {
                return false;
            }
            if let Some(next) = page.next_page_start {
                let Some(lease) = self.lease().cloned() else {
                    return false;
                };
                self.queued
                    .push_back(StartupContextEditorAction::ListDirectory {
                        lease,
                        directory,
                        page_start: next,
                        generation: self.bulk_generation,
                        bulk: true,
                    });
            } else {
                let paths = self
                    .bulk_entries
                    .iter()
                    .filter(|entry| entry.kind != StartupContextDirectoryEntryKind::Directory)
                    .map(|entry| entry.project_relative_path.clone())
                    .collect::<Vec<_>>();
                let candidate_count = paths.len();
                let prior_draft_count = self.draft.len();
                let completed_directory = directory.clone();
                self.bulk_directory = None;
                self.bulk_entries.clear();
                self.add_paths(paths);
                let added_count = self.draft.len().saturating_sub(prior_draft_count);
                let duplicate_count = candidate_count.saturating_sub(added_count);
                self.notice = Some(if duplicate_count == 0 {
                    format!("Added {added_count} direct file(s) from {completed_directory}")
                } else {
                    format!(
                        "Added {added_count} direct file(s) from {completed_directory}; ignored {duplicate_count} duplicate(s)"
                    )
                });
            }
            return true;
        }
        if generation != self.browser.generation || directory != self.browser.directory {
            return false;
        }
        if page_start == 0 {
            self.browser.entries = page.entries;
        } else if page_start == self.browser.entries.len() {
            self.browser.entries.extend(page.entries);
        } else {
            return false;
        }
        self.browser.total_entries = page.total_entries;
        self.browser.next_page_start = page.next_page_start;
        self.browser.loading = false;
        self.browser.clamp_cursor();
        if let Some(next) = self.browser.next_page_start {
            self.queue_directory(directory, next, false);
        }
        self.queue_preview_for_focus();
        true
    }

    pub(crate) fn accept_search(&mut self, id: u64, results: StartupContextSearchResults) -> bool {
        let Some(StartupContextPendingRequest::Search {
            lease_id,
            query,
            generation,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        if self.lease().map(|lease| lease.lease_id.as_str()) != Some(lease_id.as_str())
            || generation != self.browser.generation
            || self.browser.search_query.as_deref() != Some(query.as_str())
            || results.query != query
        {
            return false;
        }
        self.browser.search_results = results.results;
        self.browser.search_truncated = results.truncated || results.omitted_results > 0;
        self.browser.cursor = 0;
        self.browser.loading = false;
        self.queue_preview_for_focus();
        true
    }

    pub(crate) fn accept_search_canceled(&mut self, id: u64, search_request_id: u64) -> bool {
        matches!(
            self.pending.remove(&id),
            Some(StartupContextPendingRequest::CancelSearch {
                search_request_id: expected
            }) if expected == search_request_id
        )
    }

    pub(crate) fn accept_preview(&mut self, id: u64, preview: StartupContextFilePreview) -> bool {
        let Some(StartupContextPendingRequest::Preview {
            lease_id,
            path,
            start_char,
            generation,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        if self.lease().map(|lease| lease.lease_id.as_str()) != Some(lease_id.as_str())
            || generation != self.preview.generation
            || path != preview.logical_path
            || start_char != preview.start_char
        {
            return false;
        }
        if start_char == 0 {
            self.preview.content = preview.content;
        } else if start_char == self.preview.content.chars().count() {
            self.preview.content.push_str(&preview.content);
        } else {
            return false;
        }
        self.preview.path = Some(preview.logical_path);
        self.preview.resolved_path = Some(preview.resolved_path);
        self.preview.classification = Some(preview.classification);
        self.preview.sha256 = Some(preview.sha256);
        self.preview.bytes = Some(preview.bytes);
        self.preview.estimated_tokens = Some(preview.estimated_tokens);
        self.preview.total_chars = preview.total_chars;
        self.preview.next_start_char = preview.next_start_char;
        self.preview.exact_receipt = false;
        self.preview.loading = false;
        self.preview.failure = None;
        true
    }

    pub(crate) fn accept_detail(&mut self, id: u64, detail: StartupContextFileDetail) -> bool {
        let Some(StartupContextPendingRequest::Detail {
            batch_id,
            spec_id,
            start_char,
            generation,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        if generation != self.preview.generation
            || batch_id != detail.batch_id
            || spec_id != detail.spec_id
            || start_char != detail.start_char
        {
            return false;
        }
        if start_char == 0 {
            self.preview.content = detail.content;
        } else if start_char == self.preview.content.chars().count() {
            self.preview.content.push_str(&detail.content);
        } else {
            return false;
        }
        self.preview.total_chars = detail.total_chars;
        self.preview.next_start_char = detail.next_start_char;
        self.preview.exact_receipt = true;
        self.preview.loading = false;
        self.preview.failure = None;
        true
    }

    pub(crate) fn accept_selection(
        &mut self,
        id: u64,
        preview: StartupContextSelectionPreview,
    ) -> bool {
        let Some(StartupContextPendingRequest::Selection {
            lease_id,
            project_key_digest,
            expected_plan_revision,
            generation,
            draft_generation,
            purpose,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        let Some(lease) = self.lease() else {
            return false;
        };
        if generation != self.selection_generation || draft_generation != self.draft_generation {
            return false;
        }
        if lease.lease_id != lease_id
            || lease.project_key_digest != project_key_digest
            || lease.plan_revision != expected_plan_revision
            || preview.project_key_digest != project_key_digest
            || preview.plan_revision != expected_plan_revision
        {
            if let ApplySelectionPurpose::Apply(intent) = purpose {
                let failure = StartupContextFailure {
                    operation: crate::protocol::StartupContextOperation::PreviewSelection,
                    kind: crate::protocol::StartupContextFailureKind::StalePlanRevision,
                    message: "Authoritative apply preview no longer matches the editor lease or project revision"
                        .to_string(),
                    retryable: true,
                    issues: Vec::new(),
                };
                self.apply_overlay = Some(ApplyOverlay::PreviewFailed { intent, failure });
                self.request_authority_refresh();
            }
            return false;
        }
        if preview.entry_count != self.draft.len() {
            if let ApplySelectionPurpose::Apply(intent) = purpose {
                self.apply_overlay = Some(ApplyOverlay::ValidationIssues { intent });
            }
            return false;
        }
        let selected_count = preview.selected_count;
        let issue_count = preview.issue_count;
        let aggregate_bytes = preview.aggregate_bytes;
        let aggregate_estimated_tokens = preview.aggregate_estimated_tokens;
        let mut selected_by_index = HashMap::new();
        let mut issues_by_index = HashMap::new();
        for entry in preview.entries {
            match entry {
                StartupContextSelectionEntrySnapshot::Selected {
                    input_index,
                    spec_id,
                    logical_path,
                    resolved_path,
                    classification,
                    bytes,
                    estimated_tokens,
                    requires_external_approval,
                } => {
                    selected_by_index.insert(
                        input_index,
                        (
                            spec_id,
                            logical_path,
                            resolved_path,
                            classification,
                            bytes,
                            estimated_tokens,
                            requires_external_approval,
                        ),
                    );
                }
                StartupContextSelectionEntrySnapshot::Issue { issue } => {
                    if let Some(index) = issue.input_index {
                        issues_by_index.insert(index as usize, issue);
                    }
                }
            }
        }

        let mut ignored_duplicates = 0usize;
        let old = std::mem::take(&mut self.draft);
        self.draft = old
            .into_iter()
            .enumerate()
            .filter_map(|(index, mut entry)| {
                if let Some(selected) = selected_by_index.remove(&index) {
                    entry.normalized_spec_id = Some(selected.0.clone());
                    entry.input.existing_spec_id = Some(selected.0);
                    entry.logical_path = selected.1;
                    entry.input.path = entry.logical_path.clone();
                    entry.resolved_path = Some(selected.2);
                    entry.classification = Some(selected.3);
                    entry.bytes = Some(selected.4);
                    entry.estimated_tokens = Some(selected.5);
                    entry.requires_external_approval = selected.6;
                    entry.issue = None;
                    Some(entry)
                } else if let Some(issue) = issues_by_index.remove(&index) {
                    if matches!(
                        issue.kind,
                        StartupContextFileIssueKind::DuplicateSelection { .. }
                    ) {
                        ignored_duplicates += 1;
                        None
                    } else {
                        entry.issue = Some(issue);
                        Some(entry)
                    }
                } else {
                    entry.issue = Some(StartupContextFileIssueSnapshot {
                        input_index: Some(index as u32),
                        spec_id: entry.normalized_spec_id.clone(),
                        logical_path: Some(entry.logical_path.clone()),
                        kind: StartupContextFileIssueKind::Unreadable {
                            detail: "selection preview omitted this entry".to_string(),
                        },
                    });
                    Some(entry)
                }
            })
            .collect();
        self.batch_issues = preview.batch_issues;
        self.draft_cursor = self.draft_cursor.min(self.draft.len().saturating_sub(1));
        if ignored_duplicates > 0 {
            self.draft_generation = self.draft_generation.saturating_add(1);
            self.notice = Some(format!(
                "Ignored {ignored_duplicates} duplicate selection(s) after server normalization"
            ));
        }
        self.queue_preview_for_focus();
        if let ApplySelectionPurpose::Apply(intent) = purpose {
            self.finish_apply_preview(
                intent,
                selected_count,
                issue_count,
                aggregate_bytes,
                aggregate_estimated_tokens,
                ignored_duplicates,
            );
        }
        true
    }

    pub(crate) fn accept_failure(&mut self, id: u64, failure: StartupContextFailure) -> bool {
        let Some(pending) = self.pending.remove(&id) else {
            return false;
        };
        if self.accept_apply_failure(pending.clone(), failure.clone()) {
            return true;
        }
        match pending {
            StartupContextPendingRequest::Open { .. } => self.phase = EditorPhase::Error(failure),
            StartupContextPendingRequest::Renew { .. }
                if matches!(
                    failure.kind,
                    crate::protocol::StartupContextFailureKind::StalePlanRevision
                        | crate::protocol::StartupContextFailureKind::LeaseExpired
                        | crate::protocol::StartupContextFailureKind::LeaseNotFound
                ) =>
            {
                self.notice = Some(failure.message);
                self.request_authority_refresh();
            }
            StartupContextPendingRequest::Renew { .. } => self.phase = EditorPhase::Error(failure),
            StartupContextPendingRequest::Close { .. }
                if matches!(self.authority_refresh, EditorAuthorityRefresh::CloseQueued)
                    && matches!(
                        failure.kind,
                        crate::protocol::StartupContextFailureKind::LeaseExpired
                            | crate::protocol::StartupContextFailureKind::LeaseNotFound
                    ) =>
            {
                self.editor = None;
                self.phase = EditorPhase::Opening;
                self.authority_refresh = EditorAuthorityRefresh::ReopenQueued;
                if self.visible {
                    self.queued.push_back(StartupContextEditorAction::Open);
                }
                self.notice = Some(
                    "Previous editor lease was already gone; reacquiring authoritative state"
                        .to_string(),
                );
            }
            StartupContextPendingRequest::Close { .. } => {
                self.notice = Some(format!("Could not close editor lease: {}", failure.message));
            }
            StartupContextPendingRequest::Preview { generation, .. }
            | StartupContextPendingRequest::Detail { generation, .. }
                if generation == self.preview.generation =>
            {
                self.preview.loading = false;
                self.preview.failure = Some(failure.message);
            }
            StartupContextPendingRequest::Directory { generation, .. }
            | StartupContextPendingRequest::Search { generation, .. }
                if generation == self.browser.generation =>
            {
                self.browser.loading = false;
                self.notice = Some(failure.message);
            }
            StartupContextPendingRequest::Selection {
                generation,
                draft_generation,
                purpose: ApplySelectionPurpose::DraftValidation,
                ..
            } if generation == self.selection_generation
                && draft_generation == self.draft_generation =>
            {
                self.notice = Some(failure.message);
            }
            StartupContextPendingRequest::CancelSearch { .. }
            | StartupContextPendingRequest::Preview { .. }
            | StartupContextPendingRequest::Detail { .. }
            | StartupContextPendingRequest::Directory { .. }
            | StartupContextPendingRequest::Search { .. }
            | StartupContextPendingRequest::Selection { .. }
            | StartupContextPendingRequest::Apply { .. }
            | StartupContextPendingRequest::CancelApply { .. }
            | StartupContextPendingRequest::ApplyStatus { .. } => return false,
        }
        true
    }

    pub(crate) fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if let Some(mode) = self.input_mode.as_mut() {
            match code {
                KeyCode::Esc => self.input_mode = None,
                KeyCode::Enter => {
                    let mode = self.input_mode.take().expect("input mode exists");
                    match mode {
                        InputMode::Search { value } => self.start_search(value),
                        InputMode::ExternalPath { value } => {
                            let value = value.trim().to_string();
                            if std::path::Path::new(&value).is_absolute() {
                                self.add_paths([value]);
                            } else if !value.is_empty() {
                                self.notice = Some(
                                    "External Startup Context entry must be one absolute path"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
                KeyCode::Backspace => match mode {
                    InputMode::Search { value } | InputMode::ExternalPath { value } => {
                        value.pop();
                    }
                },
                KeyCode::Char(character)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    match mode {
                        InputMode::Search { value } | InputMode::ExternalPath { value } => {
                            value.push(character)
                        }
                    }
                }
                _ => {}
            }
            return false;
        }

        if let Some(close) = self.handle_apply_key(code) {
            return close;
        }

        if !matches!(self.phase, EditorPhase::Ready) {
            return match code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.close();
                    true
                }
                _ => false,
            };
        }

        match code {
            KeyCode::Char('q') => {
                self.close();
                true
            }
            KeyCode::Esc => {
                if self.browser.search_query.is_some() {
                    self.clear_search();
                    false
                } else if self.selection_view == SelectionView::Receipt {
                    self.selection_view = SelectionView::Draft;
                    self.active_pane = StartupContextEditorPane::Selection;
                    self.queue_preview_for_focus();
                    false
                } else if self.active_pane == StartupContextEditorPane::Preview {
                    self.active_pane = StartupContextEditorPane::Selection;
                    self.queue_preview_for_focus();
                    false
                } else if self.active_pane == StartupContextEditorPane::Browser
                    && !self.browser.directory.is_empty()
                {
                    self.queue_directory(parent_directory(&self.browser.directory), 0, true);
                    false
                } else {
                    self.close();
                    true
                }
            }
            KeyCode::Tab => {
                self.cycle_pane(1);
                false
            }
            KeyCode::BackTab => {
                self.cycle_pane(-1);
                false
            }
            KeyCode::Char('/') => {
                self.input_mode = Some(InputMode::Search {
                    value: self.browser.search_query.clone().unwrap_or_default(),
                });
                false
            }
            KeyCode::Char('a') => {
                self.input_mode = Some(InputMode::ExternalPath {
                    value: String::new(),
                });
                false
            }
            KeyCode::Char('u') => {
                self.begin_apply(ApplyIntent::SessionOnly);
                false
            }
            KeyCode::Char('p') => {
                self.begin_apply(ApplyIntent::SessionAndProjectDefault);
                false
            }
            KeyCode::Char('s') if self.has_tracked_apply() => {
                self.show_apply_status();
                false
            }
            KeyCode::Char('r') => {
                self.selection_view = match self.selection_view {
                    SelectionView::Draft => SelectionView::Receipt,
                    SelectionView::Receipt => SelectionView::Draft,
                };
                self.active_pane = StartupContextEditorPane::Selection;
                self.queue_preview_for_focus();
                false
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cycle_pane(-1);
                false
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.cycle_pane(1);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                false
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                false
            }
            KeyCode::Char('J') if self.active_pane == StartupContextEditorPane::Selection => {
                self.move_draft(1);
                false
            }
            KeyCode::Char('K') if self.active_pane == StartupContextEditorPane::Selection => {
                self.move_draft(-1);
                false
            }
            KeyCode::Delete | KeyCode::Backspace
                if self.active_pane == StartupContextEditorPane::Selection
                    && self.selection_view == SelectionView::Draft =>
            {
                self.remove_draft(self.draft_cursor);
                false
            }
            KeyCode::Backspace
                if self.active_pane == StartupContextEditorPane::Browser
                    && !self.browser.directory.is_empty() =>
            {
                self.queue_directory(parent_directory(&self.browser.directory), 0, true);
                false
            }
            KeyCode::Char(' ') if self.active_pane == StartupContextEditorPane::Browser => {
                self.select_current_browser_entry();
                false
            }
            KeyCode::Enter => {
                self.activate_current();
                false
            }
            KeyCode::Home => {
                self.set_cursor(0);
                false
            }
            KeyCode::End => {
                self.set_cursor(usize::MAX);
                false
            }
            _ => false,
        }
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        if (self.input_mode.is_some() || self.apply_overlay.is_some())
            && !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_cursor(-3);
                return false;
            }
            MouseEventKind::ScrollDown => {
                self.move_cursor(3);
                return false;
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return false,
        }
        let action = self
            .hit_regions
            .iter()
            .rev()
            .find(|region| point_in_rect(mouse.column, mouse.row, region.rect))
            .map(|region| region.action);
        let Some(action) = action else {
            return false;
        };
        if !self.row_action_allowed_while_layered(action) {
            return false;
        }
        if let Some(close) = self.handle_apply_row_action(action) {
            return close;
        }
        match action {
            RowAction::FocusBrowser(index) => {
                self.active_pane = StartupContextEditorPane::Browser;
                self.browser.cursor = index;
                self.queue_preview_for_focus();
            }
            RowAction::OpenDirectory(index) => {
                self.browser.cursor = index;
                self.open_current_directory();
            }
            RowAction::SelectBrowser(index) => {
                self.browser.cursor = index;
                self.select_current_browser_entry();
            }
            RowAction::FocusDraft(index) => {
                self.active_pane = StartupContextEditorPane::Selection;
                self.selection_view = SelectionView::Draft;
                self.draft_cursor = index;
                self.queue_preview_for_focus();
            }
            RowAction::MoveDraftUp(index) => {
                self.draft_cursor = index;
                self.move_draft(-1);
            }
            RowAction::MoveDraftDown(index) => {
                self.draft_cursor = index;
                self.move_draft(1);
            }
            RowAction::RemoveDraft(index) => self.remove_draft(index),
            RowAction::FocusReceipt(index) => {
                self.active_pane = StartupContextEditorPane::Selection;
                self.selection_view = SelectionView::Receipt;
                self.receipt_cursor = index;
                self.queue_preview_for_focus();
            }
            RowAction::LoadMorePreview => self.load_more_preview(),
            RowAction::FocusPane(pane) => self.active_pane = pane,
            RowAction::StartSearch => {
                self.input_mode = Some(InputMode::Search {
                    value: self.browser.search_query.clone().unwrap_or_default(),
                });
            }
            RowAction::StartExternal => {
                self.input_mode = Some(InputMode::ExternalPath {
                    value: String::new(),
                });
            }
            RowAction::ToggleReceipt => {
                self.selection_view = match self.selection_view {
                    SelectionView::Draft => SelectionView::Receipt,
                    SelectionView::Receipt => SelectionView::Draft,
                };
                self.active_pane = StartupContextEditorPane::Selection;
                self.queue_preview_for_focus();
            }
            RowAction::CloseEditor => {
                self.close();
                return true;
            }
            RowAction::ApplySession
            | RowAction::ApplyAndSave
            | RowAction::ApplyConfirm
            | RowAction::ApplyCancelLayer
            | RowAction::ApplyCancelQueued
            | RowAction::ApplyRetry
            | RowAction::ApplyRefreshStatus
            | RowAction::ApplyShowStatus => unreachable!("apply row action handled above"),
        }
        false
    }

    fn alloc_local_id(&mut self) -> u64 {
        let id = self.next_local_id;
        self.next_local_id = self.next_local_id.saturating_add(1);
        id
    }

    fn queue_directory(&mut self, directory: String, page_start: usize, reset: bool) {
        let Some(lease) = self.lease().cloned() else {
            return;
        };
        if reset {
            self.browser.generation = self.browser.generation.saturating_add(1);
            self.browser.directory = directory.clone();
            self.browser.entries.clear();
            self.browser.search_query = None;
            self.browser.search_results.clear();
            self.browser.cursor = 0;
            self.browser.next_page_start = None;
        }
        self.browser.loading = true;
        self.queued
            .push_back(StartupContextEditorAction::ListDirectory {
                lease,
                directory,
                page_start,
                generation: self.browser.generation,
                bulk: false,
            });
    }

    fn start_search(&mut self, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.clear_search();
            return;
        }
        let Some(lease) = self.lease().cloned() else {
            return;
        };
        if let Some((request_id, _)) = self
            .pending
            .iter()
            .find(|(_, pending)| matches!(pending, StartupContextPendingRequest::Search { .. }))
        {
            self.queued
                .push_back(StartupContextEditorAction::CancelSearch {
                    search_request_id: *request_id,
                });
        }
        self.browser.generation = self.browser.generation.saturating_add(1);
        self.browser.search_query = Some(query.clone());
        self.browser.search_results.clear();
        self.browser.cursor = 0;
        self.browser.loading = true;
        self.queued.push_back(StartupContextEditorAction::Search {
            lease,
            query,
            generation: self.browser.generation,
        });
    }

    fn clear_search(&mut self) {
        self.browser.generation = self.browser.generation.saturating_add(1);
        self.browser.search_query = None;
        self.browser.search_results.clear();
        self.browser.cursor = 0;
        self.browser.loading = false;
        self.queue_preview_for_focus();
    }

    fn queue_selection_preview(&mut self) {
        self.queue_selection_preview_for(ApplySelectionPurpose::DraftValidation);
    }

    fn queue_selection_preview_for(&mut self, purpose: ApplySelectionPurpose) {
        let Some(lease) = self.lease().cloned() else {
            return;
        };
        self.selection_generation = self.selection_generation.saturating_add(1);
        let selection = self.draft.iter().map(|entry| entry.input.clone()).collect();
        self.queued
            .push_back(StartupContextEditorAction::PreviewSelection {
                lease,
                selection,
                generation: self.selection_generation,
                draft_generation: self.draft_generation,
                purpose,
            });
    }

    fn add_paths(&mut self, paths: impl IntoIterator<Item = String>) {
        let mut exact_duplicates = 0usize;
        let mut added = 0usize;
        for path in paths {
            if self.draft.iter().any(|entry| entry.input.path == path) {
                exact_duplicates += 1;
                continue;
            }
            let id = self.alloc_local_id();
            self.draft.push(DraftEntry::pending(id, path));
            added += 1;
        }
        self.draft_cursor = self.draft.len().saturating_sub(1);
        self.selection_view = SelectionView::Draft;
        self.active_pane = StartupContextEditorPane::Selection;
        if exact_duplicates > 0 {
            self.notice = Some(format!(
                "Ignored {exact_duplicates} duplicate path selection(s)"
            ));
        }
        if added > 0 {
            self.note_draft_mutation();
            self.queue_selection_preview();
            self.queue_preview_for_focus();
        }
    }

    fn select_current_browser_entry(&mut self) {
        let Some(entry) = self.browser.current().cloned() else {
            return;
        };
        match entry.kind {
            StartupContextDirectoryEntryKind::Directory if entry.navigable => {
                let directory = entry.project_relative_path;
                self.bulk_generation = self.bulk_generation.saturating_add(1);
                let Some(lease) = self.lease().cloned() else {
                    return;
                };
                self.notice = Some(format!("Loading direct files from {directory}"));
                self.bulk_directory = Some(directory.clone());
                self.bulk_entries.clear();
                self.queued
                    .push_back(StartupContextEditorAction::ListDirectory {
                        lease,
                        directory,
                        page_start: 0,
                        generation: self.bulk_generation,
                        bulk: true,
                    });
            }
            StartupContextDirectoryEntryKind::File | StartupContextDirectoryEntryKind::Symlink => {
                self.add_paths([entry.project_relative_path]);
            }
            _ => {
                self.notice = Some("This entry cannot be selected as a startup file".to_string());
            }
        }
    }

    fn open_current_directory(&mut self) {
        let Some(entry) = self.browser.current() else {
            return;
        };
        if entry.navigable {
            self.queue_directory(entry.project_relative_path.clone(), 0, true);
        }
    }

    fn activate_current(&mut self) {
        match self.active_pane {
            StartupContextEditorPane::Browser => {
                if self.browser.current().is_some_and(|entry| entry.navigable) {
                    self.open_current_directory();
                } else {
                    self.select_current_browser_entry();
                }
            }
            StartupContextEditorPane::Selection => {
                if self.selection_view == SelectionView::Receipt {
                    self.load_receipt_detail(0);
                    self.active_pane = StartupContextEditorPane::Preview;
                } else {
                    self.active_pane = StartupContextEditorPane::Preview;
                    self.queue_preview_for_focus();
                }
            }
            StartupContextEditorPane::Preview => self.load_more_preview(),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.active_pane {
            StartupContextEditorPane::Browser => {
                self.browser.cursor = offset_index(
                    self.browser.cursor,
                    self.browser.visible_entries().len(),
                    delta,
                );
            }
            StartupContextEditorPane::Selection => match self.selection_view {
                SelectionView::Draft => {
                    self.draft_cursor = offset_index(self.draft_cursor, self.draft.len(), delta)
                }
                SelectionView::Receipt => {
                    self.receipt_cursor =
                        offset_index(self.receipt_cursor, self.receipt.len(), delta)
                }
            },
            StartupContextEditorPane::Preview => {
                self.preview.scroll = offset_index(
                    self.preview.scroll,
                    self.preview.content.lines().count().saturating_add(8),
                    delta,
                )
            }
        }
        self.queue_preview_for_focus();
    }

    fn set_cursor(&mut self, index: usize) {
        match self.active_pane {
            StartupContextEditorPane::Browser => {
                self.browser.cursor =
                    index.min(self.browser.visible_entries().len().saturating_sub(1))
            }
            StartupContextEditorPane::Selection => match self.selection_view {
                SelectionView::Draft => {
                    self.draft_cursor = index.min(self.draft.len().saturating_sub(1))
                }
                SelectionView::Receipt => {
                    self.receipt_cursor = index.min(self.receipt.len().saturating_sub(1))
                }
            },
            StartupContextEditorPane::Preview => {
                self.preview.scroll = index.min(
                    self.preview
                        .content
                        .lines()
                        .count()
                        .saturating_add(8)
                        .saturating_sub(1),
                )
            }
        }
        self.queue_preview_for_focus();
    }

    fn move_draft(&mut self, delta: isize) {
        if self.selection_view != SelectionView::Draft || self.draft.is_empty() {
            return;
        }
        let target = offset_index(self.draft_cursor, self.draft.len(), delta);
        if target == self.draft_cursor {
            return;
        }
        self.draft.swap(self.draft_cursor, target);
        self.draft_cursor = target;
        self.note_draft_mutation();
        self.queue_selection_preview();
    }

    fn remove_draft(&mut self, index: usize) {
        if index >= self.draft.len() {
            return;
        }
        self.draft.remove(index);
        self.draft_cursor = self.draft_cursor.min(self.draft.len().saturating_sub(1));
        self.note_draft_mutation();
        self.queue_selection_preview();
        self.queue_preview_for_focus();
    }

    fn cycle_pane(&mut self, delta: isize) {
        let index = match self.active_pane {
            StartupContextEditorPane::Browser => 0,
            StartupContextEditorPane::Selection => 1,
            StartupContextEditorPane::Preview => 2,
        };
        self.active_pane = match (index + delta).rem_euclid(3) {
            0 => StartupContextEditorPane::Browser,
            1 => StartupContextEditorPane::Selection,
            _ => StartupContextEditorPane::Preview,
        };
        self.queue_preview_for_focus();
    }

    fn note_draft_mutation(&mut self) {
        self.draft_generation = self.draft_generation.saturating_add(1);
        if !matches!(self.apply_overlay, Some(ApplyOverlay::Status)) {
            self.apply_overlay = None;
        }
    }

    fn queue_preview_for_focus(&mut self) {
        let path = match self.active_pane {
            StartupContextEditorPane::Browser => self
                .browser
                .current()
                .filter(|entry| {
                    matches!(
                        entry.kind,
                        StartupContextDirectoryEntryKind::File
                            | StartupContextDirectoryEntryKind::Symlink
                    )
                })
                .map(|entry| entry.project_relative_path.clone()),
            StartupContextEditorPane::Selection if self.selection_view == SelectionView::Draft => {
                if let Some(entry) = self.draft.get(self.draft_cursor)
                    && let Some(issue) = entry.issue.as_ref()
                {
                    self.preview_generation = self.preview_generation.saturating_add(1);
                    let resolved_path = match &issue.kind {
                        StartupContextFileIssueKind::ExternalApprovalRequired {
                            resolved_target,
                        } => Some(resolved_target.clone()),
                        StartupContextFileIssueKind::ExternalTargetChanged {
                            resolved_target,
                            ..
                        } => Some(resolved_target.clone()),
                        _ => entry.resolved_path.clone(),
                    };
                    self.preview = PreviewBuffer {
                        path: Some(entry.logical_path.clone()),
                        resolved_path,
                        classification: entry.classification,
                        failure: Some(issue_label(&issue.kind)),
                        generation: self.preview_generation,
                        ..PreviewBuffer::default()
                    };
                    return;
                }
                self.draft
                    .get(self.draft_cursor)
                    .map(|entry| entry.input.path.clone())
            }
            StartupContextEditorPane::Selection | StartupContextEditorPane::Preview => None,
        };
        let Some(path) = path else {
            if self.selection_view == SelectionView::Receipt
                && self.active_pane == StartupContextEditorPane::Selection
                && let Some(receipt) = self.receipt.get(self.receipt_cursor).cloned()
            {
                self.preview_generation = self.preview_generation.saturating_add(1);
                self.preview
                    .begin_receipt(&receipt, self.preview_generation);
                self.preview.loading = false;
                self.preview.content.clear();
                self.preview.next_start_char = Some(0);
            }
            return;
        };
        let Some(lease) = self.lease().cloned() else {
            return;
        };
        self.preview_generation = self.preview_generation.saturating_add(1);
        self.preview
            .begin_current(path.clone(), self.preview_generation);
        self.queued
            .push_back(StartupContextEditorAction::PreviewFile {
                lease,
                path,
                start_char: 0,
                generation: self.preview_generation,
            });
    }

    fn load_receipt_detail(&mut self, start_char: usize) {
        let Some(receipt) = self.receipt.get(self.receipt_cursor).cloned() else {
            return;
        };
        if start_char == 0 {
            self.preview_generation = self.preview_generation.saturating_add(1);
            self.preview
                .begin_receipt(&receipt, self.preview_generation);
        } else {
            self.preview.loading = true;
        }
        self.queued
            .push_back(StartupContextEditorAction::FileDetail {
                receipt,
                start_char,
                generation: self.preview.generation,
            });
    }

    fn load_more_preview(&mut self) {
        let Some(next) = self.preview.next_start_char else {
            return;
        };
        if self.preview.exact_receipt {
            self.load_receipt_detail(next);
        } else if let (Some(lease), Some(path)) = (self.lease().cloned(), self.preview.path.clone())
        {
            self.preview.loading = true;
            self.queued
                .push_back(StartupContextEditorAction::PreviewFile {
                    lease,
                    path,
                    start_char: next,
                    generation: self.preview.generation,
                });
        }
    }

    fn is_dirty(&self) -> bool {
        if self.saved_default.len() != self.draft.len() {
            return true;
        }
        self.saved_default
            .iter()
            .zip(&self.draft)
            .any(|(saved, draft)| {
                saved.spec_id != draft.normalized_spec_id.as_deref().unwrap_or_default()
                    || saved.logical_path != draft.logical_path
                    || saved.approved_external_target != draft.input.approved_external_target
            })
    }

    fn consequence_text(&self, status: Option<&StartupContextStatusSnapshot>) -> &'static str {
        match status.map(|status| status.compact.state) {
            Some(StartupContextStatusState::Unprepared)
            | Some(StartupContextStatusState::Empty)
            | Some(StartupContextStatusState::Prepared)
            | Some(StartupContextStatusState::Blocked)
            | Some(StartupContextStatusState::Error)
            | None => {
                "Before first dispatch: apply will rebuild this session's unsent Startup Context."
            }
            Some(StartupContextStatusState::Dispatched)
            | Some(StartupContextStatusState::ProviderAccepted)
            | Some(StartupContextStatusState::MetadataRepair) => {
                "After first dispatch: additions can append later; removals and order affect future/default state only."
            }
        }
    }
}

fn pane_block(title: String, focused: bool, accent: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            accent
        } else {
            Color::Rgb(80, 85, 100)
        }))
        .title(title)
}

fn centered_message(frame: &mut Frame, area: Rect, message: &str, style: Style) {
    let y = area.y.saturating_add(area.height / 2);
    frame.render_widget(
        Paragraph::new(Span::styled(message.to_string(), style)).alignment(Alignment::Center),
        Rect::new(area.x, y, area.width, 1.min(area.height)),
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

fn offset_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs()).min(len - 1)
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}

fn parent_directory(directory: &str) -> String {
    directory
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

fn window_start(cursor: usize, len: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        0
    } else {
        cursor.saturating_sub(height / 2).min(len - height)
    }
}

fn truncate_middle(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let left = (width - 1) / 2;
    let right = width - 1 - left;
    let prefix: String = value.chars().take(left).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn issue_label(kind: &StartupContextFileIssueKind) -> String {
    match kind {
        StartupContextFileIssueKind::EmptyPath => "empty path".to_string(),
        StartupContextFileIssueKind::InvalidPathEncoding => "invalid path encoding".to_string(),
        StartupContextFileIssueKind::PathTraversal => "path traversal".to_string(),
        StartupContextFileIssueKind::Missing => "missing".to_string(),
        StartupContextFileIssueKind::BrokenSymlink => "broken symlink".to_string(),
        StartupContextFileIssueKind::Unreadable { detail } => format!("unreadable: {detail}"),
        StartupContextFileIssueKind::UnsupportedTarget { target_type } => {
            format!("unsupported target: {target_type:?}")
        }
        StartupContextFileIssueKind::UnsupportedContent { content } => {
            format!("unsupported content: {content:?}")
        }
        StartupContextFileIssueKind::NonUtf8 => "not UTF-8".to_string(),
        StartupContextFileIssueKind::ExternalApprovalRequired { resolved_target } => {
            format!("external target needs confirmation: {resolved_target}")
        }
        StartupContextFileIssueKind::ExternalTargetChanged {
            approved_target,
            resolved_target,
        } => format!("external target changed: {approved_target} → {resolved_target}"),
        StartupContextFileIssueKind::InvalidExternalApproval { detail } => {
            format!("invalid external approval: {detail}")
        }
        StartupContextFileIssueKind::DuplicateSelection { first_input_index } => {
            format!("duplicate of entry {}", first_input_index + 1)
        }
        StartupContextFileIssueKind::TooManyEntries { count, limit } => {
            format!("{count} entries exceeds {limit}")
        }
        StartupContextFileIssueKind::FileTooLarge { bytes, limit } => {
            format!("{bytes} bytes exceeds {limit}")
        }
        StartupContextFileIssueKind::BatchTooLarge { bytes, limit } => {
            format!("batch {bytes} bytes exceeds {limit}")
        }
        StartupContextFileIssueKind::ChangedDuringCapture => "changed during capture".to_string(),
        StartupContextFileIssueKind::DirectoryOutsideProject => {
            "directory outside project".to_string()
        }
        StartupContextFileIssueKind::DirectoryReadFailed { detail } => {
            format!("directory read failed: {detail}")
        }
    }
}

fn compact_issue_label(kind: &StartupContextFileIssueKind) -> &'static str {
    match kind {
        StartupContextFileIssueKind::EmptyPath => "empty path",
        StartupContextFileIssueKind::InvalidPathEncoding => "invalid path",
        StartupContextFileIssueKind::PathTraversal => "path traversal",
        StartupContextFileIssueKind::Missing => "missing",
        StartupContextFileIssueKind::BrokenSymlink => "broken symlink",
        StartupContextFileIssueKind::Unreadable { .. } => "unreadable",
        StartupContextFileIssueKind::UnsupportedTarget { .. } => "unsupported target",
        StartupContextFileIssueKind::UnsupportedContent { .. } => "unsupported content",
        StartupContextFileIssueKind::NonUtf8 => "not UTF-8",
        StartupContextFileIssueKind::ExternalApprovalRequired { .. } => "confirm external",
        StartupContextFileIssueKind::ExternalTargetChanged { .. } => "external changed",
        StartupContextFileIssueKind::InvalidExternalApproval { .. } => "invalid approval",
        StartupContextFileIssueKind::DuplicateSelection { .. } => "duplicate",
        StartupContextFileIssueKind::TooManyEntries { .. } => "too many entries",
        StartupContextFileIssueKind::FileTooLarge { .. } => "file too large",
        StartupContextFileIssueKind::BatchTooLarge { .. } => "batch too large",
        StartupContextFileIssueKind::ChangedDuringCapture => "changed during read",
        StartupContextFileIssueKind::DirectoryOutsideProject => "outside project",
        StartupContextFileIssueKind::DirectoryReadFailed { .. } => "directory failed",
    }
}

#[cfg(test)]
mod tests;
