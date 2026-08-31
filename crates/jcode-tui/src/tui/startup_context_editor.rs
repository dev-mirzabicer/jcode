use crate::protocol::{
    StartupContextDirectoryEntry, StartupContextDirectoryEntryKind, StartupContextDirectoryPage,
    StartupContextEditorSnapshot, StartupContextFailure, StartupContextFileDetail,
    StartupContextFileIssueKind, StartupContextFileIssueSnapshot, StartupContextFilePreview,
    StartupContextFileReceiptSnapshot, StartupContextLeaseOwnerSnapshot,
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
        generation: u64,
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
    DisabledApply,
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
    selection_generation: u64,
    preview_generation: u64,
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
            selection_generation: 0,
            preview_generation: 0,
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
        } else if name == "editor-external" {
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
        let active_searches = self
            .pending
            .iter()
            .filter_map(|(request_id, pending)| {
                matches!(pending, StartupContextPendingRequest::Search { .. })
                    .then_some(*request_id)
            })
            .collect::<Vec<_>>();
        self.queued.clear();
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
        if matches!(self.phase, EditorPhase::Closing) {
            self.phase = EditorPhase::Opening;
            self.queued.push_back(StartupContextEditorAction::Open);
        }
    }

    pub(crate) fn restart_after_reconnect(&mut self) {
        self.visible = true;
        self.phase = EditorPhase::Opening;
        self.editor = None;
        self.browser = BrowserState::default();
        self.preview = PreviewBuffer::default();
        self.input_mode = None;
        self.pending.clear();
        self.queued.clear();
        self.renew_due = None;
        self.queued.push_back(StartupContextEditorAction::Open);
        self.notice = Some("Reacquiring editor lease after reconnect…".to_string());
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
        }
        self.editor = Some(editor);
        self.phase = EditorPhase::Ready;
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
                self.bulk_directory = None;
                self.bulk_entries.clear();
                self.add_paths(paths);
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
            generation,
        }) = self.pending.remove(&id)
        else {
            return false;
        };
        if self.lease().map(|lease| lease.lease_id.as_str()) != Some(lease_id.as_str())
            || generation != self.selection_generation
            || preview.entry_count != self.draft.len()
        {
            return false;
        }
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
            self.notice = Some(format!(
                "Ignored {ignored_duplicates} duplicate selection(s) after server normalization"
            ));
        }
        self.queue_preview_for_focus();
        true
    }

    pub(crate) fn accept_failure(&mut self, id: u64, failure: StartupContextFailure) -> bool {
        let Some(pending) = self.pending.remove(&id) else {
            return false;
        };
        match pending {
            StartupContextPendingRequest::Open { .. } => self.phase = EditorPhase::Error(failure),
            StartupContextPendingRequest::Renew { .. } => self.phase = EditorPhase::Error(failure),
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
            StartupContextPendingRequest::Selection { generation, .. }
                if generation == self.selection_generation =>
            {
                self.notice = Some(failure.message);
            }
            StartupContextPendingRequest::CancelSearch { .. }
            | StartupContextPendingRequest::Preview { .. }
            | StartupContextPendingRequest::Detail { .. }
            | StartupContextPendingRequest::Directory { .. }
            | StartupContextPendingRequest::Search { .. }
            | StartupContextPendingRequest::Selection { .. } => return false,
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
            RowAction::DisabledApply => {
                self.notice = Some(
                    "Apply actions are intentionally disabled in the WP-07 editor foundation"
                        .to_string(),
                );
            }
            RowAction::CloseEditor => {
                self.close();
                return true;
            }
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
            });
    }

    fn add_paths(&mut self, paths: impl IntoIterator<Item = String>) {
        let mut exact_duplicates = 0usize;
        for path in paths {
            if self.draft.iter().any(|entry| entry.input.path == path) {
                exact_duplicates += 1;
                continue;
            }
            let id = self.alloc_local_id();
            self.draft.push(DraftEntry::pending(id, path));
        }
        self.draft_cursor = self.draft.len().saturating_sub(1);
        self.selection_view = SelectionView::Draft;
        self.active_pane = StartupContextEditorPane::Selection;
        if exact_duplicates > 0 {
            self.notice = Some(format!(
                "Ignored {exact_duplicates} duplicate path selection(s)"
            ));
        }
        self.queue_selection_preview();
        self.queue_preview_for_focus();
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
        self.queue_selection_preview();
    }

    fn remove_draft(&mut self, index: usize) {
        if index >= self.draft.len() {
            return;
        }
        self.draft.remove(index);
        self.draft_cursor = self.draft_cursor.min(self.draft.len().saturating_sub(1));
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

    pub(crate) fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        action_required: Option<&crate::protocol::StartupContextActionRequired>,
    ) {
        frame.render_widget(Clear, area);
        self.hit_regions.clear();
        let accent = Color::Rgb(120, 190, 255);
        let text = Style::default().fg(Color::Rgb(220, 220, 230));
        let dim = Style::default().fg(Color::Rgb(130, 135, 150));
        let warn = Style::default().fg(Color::Rgb(235, 190, 105));
        let error = Style::default().fg(Color::Rgb(240, 110, 110));
        let good = Style::default().fg(Color::Rgb(120, 220, 150));
        let styles = EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            good,
        };

        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(" Startup Context editor ");
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let vertical = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(inner);
        self.render_header(frame, vertical[0], status, styles);

        match &self.phase {
            EditorPhase::Opening => {
                self.render_opening(frame, vertical[1], status, action_required, styles)
            }
            EditorPhase::Busy { owner } => {
                let detail = owner
                    .as_ref()
                    .map(|owner| {
                        format!(
                            "Editor busy on {} · session {}",
                            owner.server_name, owner.session_id
                        )
                    })
                    .unwrap_or_else(|| "Another live editor owns this project".to_string());
                centered_message(frame, vertical[1], &detail, warn);
            }
            EditorPhase::Error(failure) => centered_message(
                frame,
                vertical[1],
                &format!("Editor unavailable: {}", failure.message),
                error,
            ),
            EditorPhase::Unsupported => centered_message(
                frame,
                vertical[1],
                "The connected server does not support the Startup Context editor.",
                warn,
            ),
            EditorPhase::Closing => {
                centered_message(frame, vertical[1], "Releasing editor lease…", dim)
            }
            EditorPhase::Ready => self.render_workspace(frame, vertical[1], styles),
        }

        self.render_footer(frame, vertical[2], status, dim, accent, warn);
        if let Some(mode) = &self.input_mode {
            self.render_input_modal(frame, area, mode, text, dim, accent);
        }
    }

    fn render_opening(
        &self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        action_required: Option<&crate::protocol::StartupContextActionRequired>,
        styles: EditorStyles,
    ) {
        let EditorStyles {
            text, dim, error, ..
        } = styles;
        let mut lines = vec![Line::from(Span::styled(
            "Acquiring the project editor lease…",
            dim,
        ))];
        if action_required.is_some() {
            lines.push(Line::from(Span::styled("Request not sent", error)));
        }
        if let Some(status) = status {
            for issue in status
                .issues
                .iter()
                .take(area.height.saturating_sub(2) as usize)
            {
                lines.push(Line::from(vec![
                    Span::styled("• ", error),
                    Span::styled(
                        issue
                            .logical_path
                            .as_deref()
                            .unwrap_or("<project>")
                            .to_string(),
                        text,
                    ),
                    Span::styled(format!(" · {}", issue_label(&issue.kind)), error),
                ]));
            }
        }
        if let Some(action) = action_required {
            lines.push(Line::from(Span::styled(action.detail.clone(), error)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn render_header(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        styles: EditorStyles,
    ) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            ..
        } = styles;
        let dirty = if self.is_dirty() {
            " · Unsaved draft"
        } else {
            ""
        };
        let counts = Line::from(vec![
            Span::styled(" Saved default ", dim),
            Span::styled(self.saved_default.len().to_string(), text),
            Span::styled(" · Session receipt ", dim),
            Span::styled(self.receipt.len().to_string(), text),
            Span::styled(" · Draft ", dim),
            Span::styled(self.draft.len().to_string(), text),
            Span::styled(dirty.to_string(), if dirty.is_empty() { dim } else { warn }),
        ]);
        let root = self
            .editor
            .as_ref()
            .map(|editor| editor.project.active_root.as_str())
            .or_else(|| {
                status.and_then(|status| {
                    status
                        .compact
                        .project
                        .as_ref()
                        .map(|project| project.active_root.as_str())
                })
            })
            .unwrap_or("authoritative project loading");
        let paragraph = Paragraph::new(vec![
            counts,
            Line::from(vec![
                Span::styled(" Project ", dim),
                Span::styled(root.to_string(), Style::default().fg(accent)),
            ]),
        ]);
        frame.render_widget(paragraph, area);
    }

    fn render_workspace(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            ..
        } = styles;
        if area.width >= 96 {
            let panes = Layout::horizontal([
                Constraint::Percentage(32),
                Constraint::Percentage(33),
                Constraint::Percentage(35),
            ])
            .split(area);
            self.render_browser(frame, panes[0], text, dim, accent, warn);
            self.render_selection(frame, panes[1], styles);
            self.render_preview(frame, panes[2], styles);
        } else {
            let tabs = Rect::new(area.x, area.y, area.width, 1.min(area.height));
            let body = Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height.saturating_sub(1),
            );
            let labels = [
                (StartupContextEditorPane::Browser, " Browser "),
                (StartupContextEditorPane::Selection, " Selection "),
                (StartupContextEditorPane::Preview, " Preview "),
            ];
            let mut x = tabs.x;
            for (pane, label) in labels {
                let width = label
                    .len()
                    .min(tabs.width.saturating_sub(x.saturating_sub(tabs.x)) as usize)
                    as u16;
                if width == 0 {
                    continue;
                }
                let rect = Rect::new(x, tabs.y, width, 1);
                let style = if pane == self.active_pane {
                    Style::default().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    dim
                };
                frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
                self.hit_regions.push(HitRegion {
                    rect,
                    action: RowAction::FocusPane(pane),
                });
                x = x.saturating_add(width);
            }
            match self.active_pane {
                StartupContextEditorPane::Browser => {
                    self.render_browser(frame, body, text, dim, accent, warn)
                }
                StartupContextEditorPane::Selection => self.render_selection(frame, body, styles),
                StartupContextEditorPane::Preview => self.render_preview(frame, body, styles),
            }
        }
    }

    fn render_browser(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        text: Style,
        dim: Style,
        accent: Color,
        warn: Style,
    ) {
        let focused = self.active_pane == StartupContextEditorPane::Browser;
        let title = if let Some(query) = &self.browser.search_query {
            format!(" Browser · search {query:?} ")
        } else if self.browser.directory.is_empty() {
            " Browser · Project ".to_string()
        } else {
            format!(
                " Browser · Project › {} ",
                self.browser.directory.replace('/', " › ")
            )
        };
        let block = pane_block(title, focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let toolbar = Rect::new(inner.x, inner.y, inner.width, 1);
        let search_label = "[/ search]";
        let external_label = "[a external]";
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(search_label, Style::default().fg(accent)),
                Span::styled(" ", dim),
                Span::styled(external_label, Style::default().fg(accent)),
            ])),
            toolbar,
        );
        self.hit_regions.push(HitRegion {
            rect: Rect::new(toolbar.x, toolbar.y, search_label.len() as u16, 1),
            action: RowAction::StartSearch,
        });
        self.hit_regions.push(HitRegion {
            rect: Rect::new(
                toolbar.x.saturating_add(search_label.len() as u16 + 1),
                toolbar.y,
                external_label.len() as u16,
                1,
            ),
            action: RowAction::StartExternal,
        });
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        let entries = self.browser.visible_entries();
        if entries.is_empty() {
            let message = if self.browser.loading {
                "Loading bounded project entries…"
            } else if self.browser.search_query.is_some() {
                "No matching project files"
            } else {
                "This directory has no entries"
            };
            frame.render_widget(Paragraph::new(Span::styled(message, dim)), list_area);
            return;
        }
        let start = window_start(
            self.browser.cursor,
            entries.len(),
            list_area.height as usize,
        );
        for (row, (index, entry)) in entries
            .iter()
            .enumerate()
            .skip(start)
            .take(list_area.height as usize)
            .enumerate()
        {
            let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
            let selected = index == self.browser.cursor;
            let marker = match entry.kind {
                StartupContextDirectoryEntryKind::Directory => "▸",
                StartupContextDirectoryEntryKind::File => "·",
                StartupContextDirectoryEntryKind::Symlink => "↗",
                StartupContextDirectoryEntryKind::Other => "×",
            };
            let action = if entry.navigable { "[open][+]" } else { "[+]" };
            let action_width = action.len().min(rect.width as usize) as u16;
            let name_width = rect.width.saturating_sub(action_width);
            let row_style = if selected {
                Style::default().fg(Color::Black).bg(accent)
            } else if !entry.path_valid_utf8 {
                warn
            } else {
                text
            };
            let name = truncate_middle(&format!("{marker} {}", entry.name), name_width as usize);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{name:<width$}", width = name_width as usize),
                        row_style,
                    ),
                    Span::styled(action, if selected { row_style } else { dim }),
                ])),
                rect,
            );
            self.hit_regions.push(HitRegion {
                rect: Rect::new(rect.x, rect.y, name_width, 1),
                action: RowAction::FocusBrowser(index),
            });
            if entry.navigable {
                let open_width = "[open]".len() as u16;
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(rect.x + name_width, rect.y, open_width, 1),
                    action: RowAction::OpenDirectory(index),
                });
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(
                        rect.x + name_width + open_width,
                        rect.y,
                        action_width.saturating_sub(open_width),
                        1,
                    ),
                    action: RowAction::SelectBrowser(index),
                });
            } else {
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(rect.x + name_width, rect.y, action_width, 1),
                    action: RowAction::SelectBrowser(index),
                });
            }
        }
        if self.browser.search_truncated {
            let y = list_area.bottom().saturating_sub(1);
            frame.render_widget(
                Paragraph::new(Span::styled("Search results bounded by server", warn)),
                Rect::new(list_area.x, y, list_area.width, 1),
            );
        }
    }

    fn render_selection(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            good,
        } = styles;
        let focused = self.active_pane == StartupContextEditorPane::Selection;
        let (title, count) = match self.selection_view {
            SelectionView::Draft => ("Ordered draft", self.draft.len()),
            SelectionView::Receipt => ("Persisted receipt", self.receipt.len()),
        };
        let block = pane_block(format!(" {title} · {count} "), focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let toggle = match self.selection_view {
            SelectionView::Draft => "[r inspect receipt]",
            SelectionView::Receipt => "[r back to draft]",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(toggle, Style::default().fg(accent))),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        self.hit_regions.push(HitRegion {
            rect: Rect::new(
                inner.x,
                inner.y,
                toggle.len().min(inner.width as usize) as u16,
                1,
            ),
            action: RowAction::ToggleReceipt,
        });
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        match self.selection_view {
            SelectionView::Draft => {
                if self.draft.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "No files selected. Browse or add an external path.",
                            dim,
                        )),
                        list_area,
                    );
                    return;
                }
                let start = window_start(
                    self.draft_cursor,
                    self.draft.len(),
                    list_area.height as usize,
                );
                for (row, (index, entry)) in self
                    .draft
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(list_area.height as usize)
                    .enumerate()
                {
                    let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
                    let selected = index == self.draft_cursor;
                    let controls = "[↑][↓][x]";
                    let controls_width = controls.len().min(rect.width as usize) as u16;
                    let state = if let Some(issue) = &entry.issue {
                        format!(" ! {}", compact_issue_label(&issue.kind))
                    } else if entry.bytes.is_some() {
                        format!(
                            " · {} · ~{}t",
                            format_bytes(entry.bytes.unwrap_or_default()),
                            entry.estimated_tokens.unwrap_or_default()
                        )
                    } else {
                        " · validating".to_string()
                    };
                    let state_width = state
                        .chars()
                        .count()
                        .min(24)
                        .min(rect.width.saturating_sub(controls_width) as usize)
                        as u16;
                    let label_width = rect
                        .width
                        .saturating_sub(controls_width)
                        .saturating_sub(state_width);
                    let class = match entry.classification {
                        Some(StartupContextPathClassification::External) => " ext",
                        Some(StartupContextPathClassification::Project) => "",
                        None => "",
                    };
                    let label = truncate_middle(
                        &format!("{:>3}. {}{class}", index + 1, entry.logical_path),
                        label_width as usize,
                    );
                    let state = truncate_middle(&state, state_width as usize);
                    let row_style = if selected {
                        Style::default().fg(Color::Black).bg(accent)
                    } else if entry.issue.is_some() {
                        error
                    } else if entry.classification
                        == Some(StartupContextPathClassification::External)
                    {
                        warn
                    } else if entry.bytes.is_some() {
                        good
                    } else {
                        text
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(
                                format!("{label:<width$}", width = label_width as usize),
                                row_style,
                            ),
                            Span::styled(
                                format!("{state:<width$}", width = state_width as usize),
                                row_style,
                            ),
                            Span::styled(controls, if selected { row_style } else { dim }),
                        ])),
                        rect,
                    );
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(rect.x, rect.y, label_width.saturating_add(state_width), 1),
                        action: RowAction::FocusDraft(index),
                    });
                    let button = controls_width / 3;
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(rect.x + label_width + state_width, rect.y, button, 1),
                        action: RowAction::MoveDraftUp(index),
                    });
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(
                            rect.x + label_width + state_width + button,
                            rect.y,
                            button,
                            1,
                        ),
                        action: RowAction::MoveDraftDown(index),
                    });
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(
                            rect.x + label_width + state_width + button.saturating_mul(2),
                            rect.y,
                            controls_width.saturating_sub(button.saturating_mul(2)),
                            1,
                        ),
                        action: RowAction::RemoveDraft(index),
                    });
                }
            }
            SelectionView::Receipt => {
                if self.receipt.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "This session has no captured receipt files.",
                            dim,
                        )),
                        list_area,
                    );
                    return;
                }
                let start = window_start(
                    self.receipt_cursor,
                    self.receipt.len(),
                    list_area.height as usize,
                );
                for (row, (index, receipt)) in self
                    .receipt
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(list_area.height as usize)
                    .enumerate()
                {
                    let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
                    let selected = index == self.receipt_cursor;
                    let observation = match receipt.latest_observation {
                        StartupContextObservedState::Current => "current",
                        StartupContextObservedState::Changed { .. } => "changed",
                        StartupContextObservedState::Missing => "missing",
                        StartupContextObservedState::Unreadable => "unreadable",
                        StartupContextObservedState::Unsupported => "unsupported",
                    };
                    let label = truncate_middle(
                        &format!(
                            "{:>3}. {} · {} · {observation}",
                            receipt.ordinal,
                            receipt.logical_path,
                            format_bytes(receipt.bytes)
                        ),
                        rect.width as usize,
                    );
                    let style = if selected {
                        Style::default().fg(Color::Black).bg(accent)
                    } else if observation == "current" {
                        text
                    } else {
                        warn
                    };
                    frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
                    self.hit_regions.push(HitRegion {
                        rect,
                        action: RowAction::FocusReceipt(index),
                    });
                }
            }
        }
    }

    fn render_preview(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            ..
        } = styles;
        let focused = self.active_pane == StartupContextEditorPane::Preview;
        let title = if self.preview.exact_receipt {
            " Captured receipt detail "
        } else {
            " Current file preview "
        };
        let block = pane_block(title.to_string(), focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let Some(path) = self.preview.path.clone() else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Focus a project, draft, or receipt file to inspect it.",
                    dim,
                )),
                inner,
            );
            return;
        };
        let classification = match self.preview.classification {
            Some(StartupContextPathClassification::Project) => "project",
            Some(StartupContextPathClassification::External) => "external",
            None => "validating",
        };
        let mut lines = vec![
            Line::from(vec![Span::styled("Path ", dim), Span::styled(path, text)]),
            Line::from(vec![
                Span::styled("Target ", dim),
                Span::styled(
                    self.preview
                        .resolved_path
                        .clone()
                        .unwrap_or_else(|| "loading".to_string()),
                    if classification == "external" {
                        warn
                    } else {
                        text
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("Class ", dim),
                Span::styled(
                    classification.to_string(),
                    if classification == "external" {
                        warn
                    } else {
                        text
                    },
                ),
                Span::styled(" · UTF-8 full · ", dim),
                Span::styled(
                    self.preview
                        .bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "loading".to_string()),
                    text,
                ),
                Span::styled(
                    format!(
                        " · ~{} tokens",
                        self.preview.estimated_tokens.unwrap_or_default()
                    ),
                    dim,
                ),
            ]),
        ];
        if let Some(hash) = &self.preview.sha256 {
            lines.push(Line::from(vec![
                Span::styled("SHA-256 ", dim),
                Span::styled(hash.clone(), text),
            ]));
        }
        lines.push(Line::from(""));
        if let Some(failure) = &self.preview.failure {
            lines.push(Line::from(Span::styled(failure.clone(), error)));
        } else if self.preview.loading && self.preview.content.is_empty() {
            lines.push(Line::from(Span::styled("Loading bounded content…", dim)));
        } else if self.preview.exact_receipt && self.preview.content.is_empty() {
            lines.push(Line::from(Span::styled(
                "Exact captured content is lazy. Press Enter to load the first bounded chunk.",
                dim,
            )));
        } else {
            for line in self.preview.content.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), text)));
            }
        }
        if self.preview.next_start_char.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "[Enter / click: load next exact chunk]",
                Style::default().fg(accent),
            )));
        } else if self.preview.exact_receipt && !self.preview.content.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "Complete captured content inspected · {} characters",
                    self.preview.total_chars
                ),
                dim,
            )));
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(
            paragraph.scroll((self.preview.scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
        if self.preview.next_start_char.is_some() {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                action: RowAction::LoadMorePreview,
            });
        }
    }

    fn render_footer(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        dim: Style,
        accent: Color,
        warn: Style,
    ) {
        let consequence = self.consequence_text(status);
        let buttons = "[ Use in this session ] [ Use in this session + save as project default ]";
        let foundation = "WP-07 foundation · Apply disabled until WP-08 · [ Close editor ]";
        let mut lines = vec![
            Line::from(Span::styled(
                foundation,
                Style::default().fg(Color::Rgb(105, 110, 125)),
            )),
            Line::from(Span::styled(
                buttons,
                Style::default().fg(Color::Rgb(105, 110, 125)),
            )),
            Line::from(Span::styled(consequence, dim)),
        ];
        if let Some(notice) = &self.notice {
            lines[2] = Line::from(Span::styled(notice.clone(), warn));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        if let Some(start) = foundation.find("[ Close editor ]") {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(
                    area.x.saturating_add(start as u16),
                    area.y,
                    "[ Close editor ]".len() as u16,
                    1,
                ),
                action: RowAction::CloseEditor,
            });
        }
        if area.height >= 2 {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(area.x, area.y + 1, area.width, 1),
                action: RowAction::DisabledApply,
            });
        }
        let _ = accent;
    }

    fn render_input_modal(
        &self,
        frame: &mut Frame,
        area: Rect,
        mode: &InputMode,
        text: Style,
        dim: Style,
        accent: Color,
    ) {
        let modal = centered_rect(
            72.min(area.width.saturating_sub(2)),
            7.min(area.height),
            area,
        );
        frame.render_widget(Clear, modal);
        let (title, value, help) = match mode {
            InputMode::Search { value } => (
                " Search project files ",
                value,
                "Enter search · Esc cancel · server search is bounded and cancellable",
            ),
            InputMode::ExternalPath { value } => (
                " Add exact external path ",
                value,
                "Enter add · Esc cancel · external target remains visibly unconfirmed until WP-08",
            ),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(title);
        let inner = block.inner(modal);
        frame.render_widget(block, modal);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(value.clone(), text)),
                Line::from(""),
                Line::from(Span::styled(help, dim)),
            ]),
            inner,
        );
    }

    pub(crate) fn debug_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "open": self.visible,
            "phase": format!("{:?}", self.phase),
            "session_id": self.session_id,
            "pane": format!("{:?}", self.active_pane),
            "directory": self.browser.directory,
            "search": self.browser.search_query,
            "browser_entries": self.browser.visible_entries().len(),
            "saved_default": self.saved_default.len(),
            "receipt": self.receipt.len(),
            "draft": self.draft.len(),
            "dirty": self.is_dirty(),
            "preview_chars": self.preview.content.chars().count(),
            "preview_exact_receipt": self.preview.exact_receipt,
            "pending_requests": self.pending.len(),
        })
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
mod tests {
    use super::*;

    fn ready_editor() -> StartupContextEditor {
        StartupContextEditor::debug_fixture("editor-populated", "session".to_string(), Vec::new())
    }

    fn receipt() -> StartupContextFileReceiptSnapshot {
        use crate::protocol::{StartupContextBatchKind, StartupContextDeliveryState};
        StartupContextFileReceiptSnapshot {
            batch_id: "batch".to_string(),
            batch_kind: StartupContextBatchKind::Initial,
            delivery_state: StartupContextDeliveryState::ProviderAccepted,
            spec_id: "spec".to_string(),
            message_id: "message".to_string(),
            ordinal: 1,
            logical_path: "docs/PLAN.md".to_string(),
            resolved_path: "/project/docs/PLAN.md".to_string(),
            classification: StartupContextPathClassification::Project,
            sha256: "0123456789abcdef".repeat(4),
            bytes: 12,
            estimated_tokens: 3,
            latest_observation: StartupContextObservedState::Current,
            notification_count: 0,
        }
    }

    #[test]
    fn draft_order_uses_stable_local_identity() {
        let mut editor = StartupContextEditor::new("session".to_string(), None);
        editor.phase = EditorPhase::Ready;
        editor.draft = vec![
            DraftEntry::pending(10, "a.md".to_string()),
            DraftEntry::pending(20, "b.md".to_string()),
        ];
        editor.draft_cursor = 0;
        editor.move_draft(1);
        assert_eq!(editor.draft[0].local_id, 20);
        assert_eq!(editor.draft[1].local_id, 10);
        assert_eq!(editor.draft_cursor, 1);
    }

    #[test]
    fn exact_duplicate_paths_are_ignored_without_destroying_draft() {
        let mut editor = StartupContextEditor::new("session".to_string(), None);
        editor.phase = EditorPhase::Ready;
        editor.draft = vec![DraftEntry::pending(10, "a.md".to_string())];
        editor.add_paths(["a.md".to_string(), "b.md".to_string()]);
        assert_eq!(editor.draft.len(), 2);
        assert_eq!(editor.draft[0].local_id, 10);
        assert!(editor.notice.as_deref().unwrap().contains("duplicate"));
    }

    #[test]
    fn narrow_back_unwinds_search_before_closing() {
        let mut editor = StartupContextEditor::new("session".to_string(), None);
        editor.phase = EditorPhase::Ready;
        editor.browser.search_query = Some("plan".to_string());
        assert!(!editor.handle_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(editor.visible);
        assert!(editor.browser.search_query.is_none());
        assert!(editor.handle_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!editor.visible);
    }

    #[test]
    fn directory_bulk_selection_uses_every_direct_non_directory_child_in_server_order() {
        let mut editor = ready_editor();
        editor.browser.cursor = 0;
        editor.select_current_browser_entry();
        let action = editor.take_action().expect("bulk directory action");
        let (lease_id, directory, generation) = match action {
            StartupContextEditorAction::ListDirectory {
                lease,
                directory,
                page_start: 0,
                generation,
                bulk: true,
            } => (lease.lease_id, directory, generation),
            other => panic!("unexpected action: {other:?}"),
        };
        editor.register_pending(
            10,
            StartupContextPendingRequest::Directory {
                lease_id,
                directory: directory.clone(),
                page_start: 0,
                generation,
                bulk: true,
            },
        );
        assert!(editor.accept_directory(
            10,
            StartupContextDirectoryPage {
                project_key_digest: "fixture-project".to_string(),
                plan_revision: 7,
                directory,
                total_entries: 3,
                page_start: 0,
                page_end: 3,
                next_page_start: None,
                entries: vec![
                    StartupContextDirectoryEntry {
                        name: "01-first.md".to_string(),
                        project_relative_path: "docs/01-first.md".to_string(),
                        resolved_path: "/fixture/project/docs/01-first.md".to_string(),
                        path_valid_utf8: true,
                        kind: StartupContextDirectoryEntryKind::File,
                        classification: StartupContextPathClassification::Project,
                        navigable: false,
                        bytes: Some(1),
                        selected_spec_id: None,
                    },
                    StartupContextDirectoryEntry {
                        name: "nested".to_string(),
                        project_relative_path: "docs/nested".to_string(),
                        resolved_path: "/fixture/project/docs/nested".to_string(),
                        path_valid_utf8: true,
                        kind: StartupContextDirectoryEntryKind::Directory,
                        classification: StartupContextPathClassification::Project,
                        navigable: true,
                        bytes: None,
                        selected_spec_id: None,
                    },
                    StartupContextDirectoryEntry {
                        name: "02-special".to_string(),
                        project_relative_path: "docs/02-special".to_string(),
                        resolved_path: "/fixture/project/docs/02-special".to_string(),
                        path_valid_utf8: true,
                        kind: StartupContextDirectoryEntryKind::Other,
                        classification: StartupContextPathClassification::Project,
                        navigable: false,
                        bytes: None,
                        selected_spec_id: None,
                    },
                ],
            }
        ));
        let paths = editor
            .draft
            .iter()
            .map(|entry| entry.input.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.ends_with(&["docs/01-first.md", "docs/02-special"]));
        assert!(!paths.contains(&"docs/nested"));
    }

    #[test]
    fn normalized_duplicate_preview_drops_only_duplicate_and_preserves_stable_identity() {
        let mut editor = ready_editor();
        editor.draft = vec![
            DraftEntry::pending(41, "docs/a.md".to_string()),
            DraftEntry::pending(42, "docs/link-to-a.md".to_string()),
        ];
        editor.selection_generation = 8;
        let lease_id = editor.lease().unwrap().lease_id.clone();
        editor.register_pending(
            20,
            StartupContextPendingRequest::Selection {
                lease_id,
                generation: 8,
            },
        );
        assert!(editor.accept_selection(
            20,
            StartupContextSelectionPreview {
                project_key_digest: "fixture-project".to_string(),
                plan_revision: 7,
                entry_count: 2,
                selected_count: 1,
                issue_count: 1,
                aggregate_bytes: 4,
                aggregate_estimated_tokens: 1,
                entries: vec![
                    StartupContextSelectionEntrySnapshot::Selected {
                        input_index: 0,
                        spec_id: "normalized-a".to_string(),
                        logical_path: "docs/a.md".to_string(),
                        resolved_path: "/fixture/project/docs/a.md".to_string(),
                        classification: StartupContextPathClassification::Project,
                        bytes: 4,
                        estimated_tokens: 1,
                        requires_external_approval: false,
                    },
                    StartupContextSelectionEntrySnapshot::Issue {
                        issue: StartupContextFileIssueSnapshot {
                            input_index: Some(1),
                            spec_id: None,
                            logical_path: Some("docs/link-to-a.md".to_string()),
                            kind: StartupContextFileIssueKind::DuplicateSelection {
                                first_input_index: 0,
                            },
                        },
                    },
                ],
                batch_issues: Vec::new(),
            }
        ));
        assert_eq!(editor.draft.len(), 1);
        assert_eq!(editor.draft[0].local_id, 41);
        assert_eq!(
            editor.draft[0].normalized_spec_id.as_deref(),
            Some("normalized-a")
        );
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap()
                .contains("Ignored 1 duplicate")
        );
    }

    #[test]
    fn stale_directory_preview_and_selection_responses_are_rejected() {
        let mut editor = ready_editor();
        let lease_id = editor.lease().unwrap().lease_id.clone();
        editor.browser.generation = 4;
        editor.register_pending(
            30,
            StartupContextPendingRequest::Directory {
                lease_id: lease_id.clone(),
                directory: String::new(),
                page_start: 0,
                generation: 3,
                bulk: false,
            },
        );
        assert!(!editor.accept_directory(
            30,
            StartupContextDirectoryPage {
                project_key_digest: "fixture-project".to_string(),
                plan_revision: 7,
                directory: String::new(),
                total_entries: 0,
                page_start: 0,
                page_end: 0,
                next_page_start: None,
                entries: Vec::new(),
            }
        ));
        editor.preview_generation = 9;
        editor.preview.begin_current("new.md".to_string(), 9);
        editor.register_pending(
            31,
            StartupContextPendingRequest::Preview {
                lease_id,
                path: "old.md".to_string(),
                start_char: 0,
                generation: 8,
            },
        );
        assert!(!editor.accept_preview(
            31,
            StartupContextFilePreview {
                project_key_digest: "fixture-project".to_string(),
                plan_revision: 7,
                logical_path: "old.md".to_string(),
                resolved_path: "/fixture/project/old.md".to_string(),
                classification: StartupContextPathClassification::Project,
                requires_external_approval: false,
                sha256: "0".repeat(64),
                bytes: 3,
                estimated_tokens: 1,
                total_chars: 3,
                start_char: 0,
                end_char: 3,
                next_start_char: None,
                truncated: false,
                content: "old".to_string(),
            }
        ));
        assert_eq!(editor.preview.path.as_deref(), Some("new.md"));
        assert!(editor.preview.content.is_empty());
    }

    #[test]
    fn exact_receipt_detail_reconstructs_unicode_chunks_without_eager_loading() {
        let mut editor = ready_editor();
        editor.receipt = vec![receipt()];
        editor.receipt_cursor = 0;
        editor.preview_generation = 5;
        editor.preview.begin_receipt(&editor.receipt[0], 5);
        editor.register_pending(
            40,
            StartupContextPendingRequest::Detail {
                batch_id: "batch".to_string(),
                spec_id: "spec".to_string(),
                start_char: 0,
                generation: 5,
            },
        );
        assert!(editor.accept_detail(
            40,
            StartupContextFileDetail {
                session_id: "session".to_string(),
                batch_id: "batch".to_string(),
                spec_id: "spec".to_string(),
                message_id: "message".to_string(),
                sha256: "0123456789abcdef".repeat(4),
                total_chars: 4,
                start_char: 0,
                end_char: 2,
                next_start_char: Some(2),
                content: "αβ".to_string(),
            }
        ));
        editor.register_pending(
            41,
            StartupContextPendingRequest::Detail {
                batch_id: "batch".to_string(),
                spec_id: "spec".to_string(),
                start_char: 2,
                generation: 5,
            },
        );
        assert!(editor.accept_detail(
            41,
            StartupContextFileDetail {
                session_id: "session".to_string(),
                batch_id: "batch".to_string(),
                spec_id: "spec".to_string(),
                message_id: "message".to_string(),
                sha256: "0123456789abcdef".repeat(4),
                total_chars: 4,
                start_char: 2,
                end_char: 4,
                next_start_char: None,
                content: "γδ".to_string(),
            }
        ));
        assert_eq!(editor.preview.content, "αβγδ");
        assert!(editor.preview.next_start_char.is_none());
    }

    #[test]
    fn external_path_failure_preserves_unsaved_draft_and_exposes_target() {
        let mut editor = ready_editor();
        let before = editor.draft.len();
        editor.input_mode = Some(InputMode::ExternalPath {
            value: "/external/NOTES.md".to_string(),
        });
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.draft.len(), before + 1);
        let generation = editor.selection_generation;
        let lease_id = editor.lease().unwrap().lease_id.clone();
        editor.register_pending(
            50,
            StartupContextPendingRequest::Selection {
                lease_id,
                generation,
            },
        );
        assert!(editor.accept_failure(
            50,
            StartupContextFailure {
                operation: crate::protocol::StartupContextOperation::PreviewSelection,
                kind: crate::protocol::StartupContextFailureKind::Io,
                message: "preview temporarily failed".to_string(),
                retryable: true,
                issues: Vec::new(),
            }
        ));
        assert_eq!(editor.draft.len(), before + 1);
        assert_eq!(
            editor.draft.last().unwrap().input.path,
            "/external/NOTES.md"
        );
    }

    #[test]
    fn external_entry_rejects_relative_paths_without_mutating_draft() {
        let mut editor = ready_editor();
        let before = editor.draft.len();
        editor.input_mode = Some(InputMode::ExternalPath {
            value: "relative/NOTES.md".to_string(),
        });
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.draft.len(), before);
        assert!(editor.notice.as_deref().unwrap().contains("absolute path"));
    }

    #[test]
    fn preview_keyboard_and_mouse_scrolling_share_the_same_transition() {
        let mut keyboard = ready_editor();
        keyboard.active_pane = StartupContextEditorPane::Preview;
        keyboard.preview.content = (0..30)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        keyboard.move_cursor(3);
        assert_eq!(keyboard.preview.scroll, 3);

        let mut mouse = ready_editor();
        mouse.active_pane = StartupContextEditorPane::Preview;
        mouse.preview.content = keyboard.preview.content.clone();
        mouse.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(mouse.preview.scroll, keyboard.preview.scroll);
    }

    #[test]
    fn closing_cancels_active_search_before_releasing_lease() {
        let mut editor = ready_editor();
        let lease_id = editor.lease().unwrap().lease_id.clone();
        editor.register_pending(
            77,
            StartupContextPendingRequest::Search {
                lease_id: lease_id.clone(),
                query: "plan".to_string(),
                generation: editor.browser.generation,
            },
        );
        editor.close();
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::CancelSearch {
                search_request_id: 77
            })
        ));
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::Close {
                lease_id: closed,
                ..
            }) if closed == lease_id
        ));
    }

    #[test]
    fn directory_page_continuation_preserves_order_and_cursor_generation() {
        let mut editor = ready_editor();
        editor.browser.entries.clear();
        editor.browser.generation = 11;
        let lease_id = editor.lease().unwrap().lease_id.clone();
        editor.register_pending(
            80,
            StartupContextPendingRequest::Directory {
                lease_id,
                directory: String::new(),
                page_start: 0,
                generation: 11,
                bulk: false,
            },
        );
        assert!(editor.accept_directory(
            80,
            StartupContextDirectoryPage {
                project_key_digest: "fixture-project".to_string(),
                plan_revision: 7,
                directory: String::new(),
                total_entries: 2,
                page_start: 0,
                page_end: 1,
                next_page_start: Some(1),
                entries: vec![StartupContextDirectoryEntry {
                    name: "a.md".to_string(),
                    project_relative_path: "a.md".to_string(),
                    resolved_path: "/fixture/project/a.md".to_string(),
                    path_valid_utf8: true,
                    kind: StartupContextDirectoryEntryKind::File,
                    classification: StartupContextPathClassification::Project,
                    navigable: false,
                    bytes: Some(1),
                    selected_spec_id: None,
                }],
            }
        ));
        assert_eq!(editor.browser.entries.len(), 1);
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::ListDirectory {
                page_start: 1,
                generation: 11,
                bulk: false,
                ..
            })
        ));
    }

    #[test]
    fn reconnect_reacquires_lease_and_preserves_unsaved_draft_identity() {
        let mut editor = ready_editor();
        editor.add_paths(["README.md".to_string()]);
        let draft_ids = editor
            .draft
            .iter()
            .map(|entry| entry.local_id)
            .collect::<Vec<_>>();
        assert!(editor.is_dirty());
        editor.restart_after_reconnect();
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::Open)
        ));
        editor.register_pending(
            90,
            StartupContextPendingRequest::Open {
                session_id: "session".to_string(),
            },
        );
        let snapshot = StartupContextEditor::debug_fixture(
            "editor-populated",
            "session".to_string(),
            Vec::new(),
        )
        .editor
        .expect("fixture editor snapshot");
        assert!(editor.accept_opened(90, snapshot));
        assert_eq!(
            editor
                .draft
                .iter()
                .map(|entry| entry.local_id)
                .collect::<Vec<_>>(),
            draft_ids
        );
        assert!(editor.is_dirty());
    }

    #[test]
    fn lease_renews_while_visible_and_explicit_close_queues_release() {
        let mut editor = ready_editor();
        editor.renew_due = Some(Instant::now() - Duration::from_secs(1));
        editor.tick(Instant::now());
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::Renew { .. })
        ));
        editor.close();
        assert!(!editor.visible);
        assert!(matches!(
            editor.take_action(),
            Some(StartupContextEditorAction::Close { .. })
        ));
    }

    #[test]
    fn browser_mouse_add_uses_the_same_transition_as_space() {
        let mut keyboard = ready_editor();
        keyboard.browser.cursor = 1;
        keyboard.select_current_browser_entry();
        let keyboard_paths = keyboard
            .draft
            .iter()
            .map(|entry| entry.input.path.clone())
            .collect::<Vec<_>>();

        let mut mouse = ready_editor();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| mouse.render(frame, frame.area(), None, None))
            .expect("render editor");
        let rect = mouse
            .hit_regions
            .iter()
            .find_map(|region| match region.action {
                RowAction::SelectBrowser(1) => Some(region.rect),
                _ => None,
            })
            .expect("browser add hit region");
        mouse.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        let mouse_paths = mouse
            .draft
            .iter()
            .map(|entry| entry.input.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(mouse_paths, keyboard_paths);
    }

    #[test]
    fn foundation_mouse_targets_match_keyboard_transitions() {
        fn render(editor: &mut StartupContextEditor, width: u16) {
            let backend = ratatui::backend::TestBackend::new(width, 30);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| editor.render(frame, frame.area(), None, None))
                .expect("render editor");
        }

        fn click(editor: &mut StartupContextEditor, action: RowAction) -> bool {
            let rect = editor
                .hit_regions
                .iter()
                .find(|region| region.action == action)
                .map(|region| region.rect)
                .unwrap_or_else(|| panic!("missing hit region for {action:?}"));
            editor.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
        }

        let mut keyboard = ready_editor();
        keyboard.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        let mut mouse = ready_editor();
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::StartSearch);
        assert!(matches!(
            keyboard.input_mode,
            Some(InputMode::Search { .. })
        ));
        assert!(matches!(mouse.input_mode, Some(InputMode::Search { .. })));

        let mut keyboard = ready_editor();
        keyboard.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        let mut mouse = ready_editor();
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::StartExternal);
        assert!(matches!(
            keyboard.input_mode,
            Some(InputMode::ExternalPath { .. })
        ));
        assert!(matches!(
            mouse.input_mode,
            Some(InputMode::ExternalPath { .. })
        ));

        let mut keyboard = ready_editor();
        keyboard.active_pane = StartupContextEditorPane::Selection;
        keyboard.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        let mut mouse = ready_editor();
        mouse.active_pane = StartupContextEditorPane::Selection;
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::ToggleReceipt);
        assert_eq!(keyboard.selection_view, mouse.selection_view);

        let mut keyboard = ready_editor();
        keyboard.active_pane = StartupContextEditorPane::Selection;
        keyboard.handle_key(KeyCode::Char('J'), KeyModifiers::NONE);
        let keyboard_ids = keyboard
            .draft
            .iter()
            .map(|entry| entry.local_id)
            .collect::<Vec<_>>();
        let mut mouse = ready_editor();
        mouse.active_pane = StartupContextEditorPane::Selection;
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::MoveDraftDown(0));
        assert_eq!(
            mouse
                .draft
                .iter()
                .map(|entry| entry.local_id)
                .collect::<Vec<_>>(),
            keyboard_ids
        );

        let mut keyboard = ready_editor();
        keyboard.active_pane = StartupContextEditorPane::Selection;
        keyboard.handle_key(KeyCode::Delete, KeyModifiers::NONE);
        let mut mouse = ready_editor();
        mouse.active_pane = StartupContextEditorPane::Selection;
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::RemoveDraft(0));
        assert_eq!(mouse.draft.len(), keyboard.draft.len());

        let mut keyboard = ready_editor();
        keyboard.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        let mut mouse = ready_editor();
        render(&mut mouse, 72);
        click(
            &mut mouse,
            RowAction::FocusPane(StartupContextEditorPane::Selection),
        );
        assert_eq!(keyboard.active_pane, mouse.active_pane);

        let mut keyboard = ready_editor();
        keyboard.browser.cursor = 0;
        keyboard.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let mut mouse = ready_editor();
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::OpenDirectory(0));
        assert!(matches!(
            keyboard.take_action(),
            Some(StartupContextEditorAction::ListDirectory { bulk: false, .. })
        ));
        assert!(matches!(
            mouse.take_action(),
            Some(StartupContextEditorAction::ListDirectory { bulk: false, .. })
        ));

        let mut mouse = ready_editor();
        render(&mut mouse, 120);
        click(&mut mouse, RowAction::DisabledApply);
        assert!(mouse.notice.as_deref().unwrap().contains("disabled"));

        let mut mouse = ready_editor();
        render(&mut mouse, 120);
        assert!(click(&mut mouse, RowAction::CloseEditor));
        assert!(!mouse.visible);
    }
}
