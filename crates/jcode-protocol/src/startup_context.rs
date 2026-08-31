use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const STARTUP_CONTEXT_PROTOCOL_VERSION: u32 = 1;
pub const STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
pub const STARTUP_CONTEXT_STATUS_DEFAULT_PAGE_SIZE: usize = 128;
pub const STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE: usize = 512;
pub const STARTUP_CONTEXT_DIRECTORY_DEFAULT_PAGE_SIZE: usize = 200;
pub const STARTUP_CONTEXT_DIRECTORY_MAX_PAGE_SIZE: usize = 1_000;
pub const STARTUP_CONTEXT_SEARCH_DEFAULT_MAX_RESULTS: usize = 200;
pub const STARTUP_CONTEXT_SEARCH_MAX_RESULTS: usize = 1_000;
pub const STARTUP_CONTEXT_FILE_PREVIEW_DEFAULT_MAX_CHARS: usize = 16 * 1024;
pub const STARTUP_CONTEXT_FILE_PREVIEW_MAX_CHARS: usize = 64 * 1024;
pub const STARTUP_CONTEXT_FILE_DETAIL_DEFAULT_MAX_CHARS: usize = 16 * 1024;
pub const STARTUP_CONTEXT_FILE_DETAIL_MAX_CHARS: usize = 64 * 1024;
pub const STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS: usize = 512;
pub const STARTUP_CONTEXT_PATH_MAX_CHARS: usize = 16 * 1024;
pub const STARTUP_CONTEXT_QUERY_MAX_CHARS: usize = 1_024;
pub const STARTUP_CONTEXT_SELECTION_MAX_ENTRIES: usize = 1_024;

/// Primary caller identity supplied during shared-server session creation.
///
/// Absence on the wire preserves compatibility and means the ordinary
/// interactive TUI path. Harness API creation opts in explicitly so the server
/// can reject blocked creation instead of registering a repairable TUI session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextPrimaryCaller {
    InteractiveTui,
    HarnessApiCreate,
    HarnessApiAttach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextProjectKind {
    Git,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextProjectSnapshot {
    pub key_digest: String,
    pub kind: StartupContextProjectKind,
    pub active_root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextStatusState {
    Unprepared,
    Empty,
    Prepared,
    Blocked,
    Dispatched,
    ProviderAccepted,
    MetadataRepair,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextPathClassification {
    Project,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextBatchKind {
    Initial,
    Late,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextDeliveryState {
    Captured,
    Dispatched,
    ProviderAccepted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StartupContextObservedState {
    Current,
    Changed { sha256: String, bytes: u64 },
    Missing,
    Unreadable,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextLeaseOwnerSnapshot {
    pub server_name: String,
    pub session_id: String,
    pub acquired_at: DateTime<Utc>,
    pub renewed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StartupContextLeaseAvailability {
    Available,
    Busy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<StartupContextLeaseOwnerSnapshot>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextLeaseSnapshot {
    pub lease_id: String,
    pub project_key_digest: String,
    pub owner_session_id: String,
    pub acquired_at: DateTime<Utc>,
    pub renewed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub plan_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextCompactStatus {
    pub protocol_version: u32,
    pub session_id: String,
    pub state: StartupContextStatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<StartupContextProjectSnapshot>,
    pub plan_revision: u64,
    pub plan_entry_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_plan_revision: Option<u64>,
    pub receipt_file_count: usize,
    pub captured_bytes: u64,
    pub estimated_tokens: u64,
    pub blocked_issue_count: usize,
    pub pending_update_count: usize,
    pub stale_file_count: usize,
    pub lease: StartupContextLeaseAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StartupContextFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextFileReceiptSnapshot {
    pub batch_id: String,
    pub batch_kind: StartupContextBatchKind,
    pub delivery_state: StartupContextDeliveryState,
    pub spec_id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub logical_path: String,
    pub resolved_path: String,
    pub classification: StartupContextPathClassification,
    pub sha256: String,
    pub bytes: u64,
    pub estimated_tokens: u64,
    pub latest_observation: StartupContextObservedState,
    pub notification_count: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextTargetType {
    Directory,
    SymlinkToDirectory,
    DeviceOrSpecial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextUnsupportedContent {
    Binary,
    Pdf,
    Image,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartupContextFileIssueKind {
    EmptyPath,
    InvalidPathEncoding,
    PathTraversal,
    Missing,
    BrokenSymlink,
    Unreadable {
        detail: String,
    },
    UnsupportedTarget {
        target_type: StartupContextTargetType,
    },
    UnsupportedContent {
        content: StartupContextUnsupportedContent,
    },
    NonUtf8,
    ExternalApprovalRequired {
        resolved_target: String,
    },
    ExternalTargetChanged {
        approved_target: String,
        resolved_target: String,
    },
    InvalidExternalApproval {
        detail: String,
    },
    DuplicateSelection {
        first_input_index: u32,
    },
    TooManyEntries {
        count: u32,
        limit: u32,
    },
    FileTooLarge {
        bytes: u64,
        limit: u64,
    },
    BatchTooLarge {
        bytes: u64,
        limit: u64,
    },
    ChangedDuringCapture,
    DirectoryOutsideProject,
    DirectoryReadFailed {
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextFileIssueSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
    pub kind: StartupContextFileIssueKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextStatusSnapshot {
    pub compact: StartupContextCompactStatus,
    pub total_files: usize,
    pub file_page_start: usize,
    pub file_page_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_file_page_start: Option<usize>,
    pub files: Vec<StartupContextFileReceiptSnapshot>,
    pub total_issues: usize,
    pub issue_page_start: usize,
    pub issue_page_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_issue_page_start: Option<usize>,
    pub issues: Vec<StartupContextFileIssueSnapshot>,
}

/// Why a user-initiated provider request stopped at the Startup Context gate.
///
/// This value is carried only inside the optional action metadata on a status
/// response. Existing clients can ignore that additive field while newer TUI
/// clients use it to present the correct recovery workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextActionKind {
    RequirementsUnresolved,
    DispatchPersistence,
}

/// Durable disposition of the unanswered user turn after Startup Context
/// prevented provider dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextPromptDisposition {
    /// The unanswered turn was removed from authoritative history. The client
    /// may restore its correlated composer snapshot for a manual resend.
    RolledBack,
    /// Durable rollback failed, so the authoritative turn remains in history
    /// and must not be resubmitted.
    Retained,
}

/// Prompt-safe recovery metadata attached to the status response that follows
/// a blocked user dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextActionRequired {
    pub kind: StartupContextActionKind,
    pub prompt_disposition: StartupContextPromptDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_input: Option<crate::ContextPendingInputMetadata>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextPlanEntrySnapshot {
    pub spec_id: String,
    pub logical_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_external_target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextEditorSnapshot {
    pub lease: StartupContextLeaseSnapshot,
    pub project: StartupContextProjectSnapshot,
    pub plan_revision: u64,
    pub plan_entries: Vec<StartupContextPlanEntrySnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextDirectoryEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextDirectoryEntry {
    pub name: String,
    pub project_relative_path: String,
    pub resolved_path: String,
    pub path_valid_utf8: bool,
    pub kind: StartupContextDirectoryEntryKind,
    pub classification: StartupContextPathClassification,
    pub navigable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_spec_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextDirectoryPage {
    pub project_key_digest: String,
    pub plan_revision: u64,
    pub directory: String,
    pub total_entries: usize,
    pub page_start: usize,
    pub page_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_start: Option<usize>,
    pub entries: Vec<StartupContextDirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextSearchResults {
    pub project_key_digest: String,
    pub plan_revision: u64,
    pub query: String,
    pub visited_entries: usize,
    pub omitted_results: usize,
    pub truncated: bool,
    pub results: Vec<StartupContextDirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextFilePreview {
    pub project_key_digest: String,
    pub plan_revision: u64,
    pub logical_path: String,
    pub resolved_path: String,
    pub classification: StartupContextPathClassification,
    pub requires_external_approval: bool,
    pub sha256: String,
    pub bytes: u64,
    pub estimated_tokens: u64,
    pub total_chars: usize,
    pub start_char: usize,
    pub end_char: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_char: Option<usize>,
    pub truncated: bool,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextFileDetail {
    pub session_id: String,
    pub batch_id: String,
    pub spec_id: String,
    pub message_id: String,
    pub sha256: String,
    pub total_chars: usize,
    pub start_char: usize,
    pub end_char: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_char: Option<usize>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextSelectionInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_spec_id: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_external_target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StartupContextSelectionEntrySnapshot {
    Selected {
        input_index: usize,
        spec_id: String,
        logical_path: String,
        resolved_path: String,
        classification: StartupContextPathClassification,
        bytes: u64,
        estimated_tokens: u64,
        requires_external_approval: bool,
    },
    Issue {
        issue: StartupContextFileIssueSnapshot,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextSelectionPreview {
    pub project_key_digest: String,
    pub plan_revision: u64,
    pub entry_count: usize,
    pub selected_count: usize,
    pub issue_count: usize,
    pub aggregate_bytes: u64,
    pub aggregate_estimated_tokens: u64,
    pub entries: Vec<StartupContextSelectionEntrySnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_issues: Vec<StartupContextFileIssueSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextApplyPhase {
    Queued,
    Applying,
    RecoveryRequired,
    Succeeded,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StartupContextApplyTargetState {
    NotRequested,
    Pending,
    Unchanged,
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
    Failed {
        message: String,
        retryable: bool,
    },
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextApplyStatus {
    pub operation_id: String,
    pub session_id: String,
    pub phase: StartupContextApplyPhase,
    pub session_target: StartupContextApplyTargetState,
    pub project_default_target: StartupContextApplyTargetState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub file_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<StartupContextFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextOperation {
    Status,
    OpenEditor,
    RenewLease,
    CloseEditor,
    ListDirectory,
    SearchFiles,
    CancelSearch,
    PreviewFile,
    FileDetail,
    PreviewSelection,
    ApplySelection,
    CancelApply,
    ApplyStatus,
    HistoryProjection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupContextFailureKind {
    Unsupported,
    InvalidRequest,
    ProjectIdentity,
    PlanStorage,
    LeaseBusy,
    LeaseNotFound,
    LeaseExpired,
    LeaseOwnerMismatch,
    StalePlanRevision,
    InvalidPath,
    Io,
    ReceiptNotFound,
    MessageMismatch,
    DigestMismatch,
    EventTooLarge,
    SearchCanceled,
    ApplyNotFound,
    OperationConflict,
    Recovery,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextFailure {
    pub operation: StartupContextOperation,
    pub kind: StartupContextFailureKind,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<StartupContextFileIssueSnapshot>,
}
