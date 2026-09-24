//! A closeout is one exact human-authorized transaction, not permission to
//! delete an arbitrary path. File bodies and large inventories are separate
//! private records; ordinary operation inspection returns metadata only.
use crate::{Issue, LocationId, OperationId, RequestId, ReviewId, Revision};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloseoutAction {
    Refresh,
    Preserve,
    Disposition { decision: CloseoutDecision },
    ReviewRemoval,
    ApproveRemoval { review: ReviewId },
    Finish,
    ReviewRecovery { choice: CloseoutRecoveryAction },
    ApplyRecovery { review: ReviewId },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutActionSpec {
    pub operation: OperationId,
    pub expected_revision: Revision,
    pub action: CloseoutAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutActionRecord {
    pub request: RequestId,
    pub spec: CloseoutActionSpec,
    pub initiated_by: String,
    pub run_id: String,
    /// Domain receipt, not proof that the execution output has been sealed.
    pub result: Option<CloseoutActionResult>,
    pub issue: Option<Issue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CloseoutActionResult {
    Record(Box<CloseoutRecord>),
    Review(Box<CloseoutReview>),
    Recovery(Box<CloseoutRecoveryReview>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloseoutRequest {
    Begin {
        request: RequestId,
        expected_revision: Revision,
        spec: CloseoutSpec,
    },
    Inspect {
        operation: OperationId,
    },
    Inventory {
        operation: OperationId,
        digest: String,
        after: u64,
        limit: u32,
    },
    Revoke {
        request: RequestId,
        operation: OperationId,
        expected_revision: Revision,
    },
    Execute {
        request: RequestId,
        spec: CloseoutActionSpec,
    },
    InspectAction {
        request: RequestId,
    },
    Execution {
        request: RequestId,
        control: jcode_tool_types::execution::ExecutionRequest,
    },
    Review {
        operation: OperationId,
    },
    Recovery {
        operation: OperationId,
    },
    RemovalProgress {
        operation: OperationId,
        expected_revision: Revision,
        after: u64,
        limit: u32,
    },
    History {
        location: LocationId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CloseoutResponse {
    Record(Box<CloseoutRecord>),
    Action(Box<CloseoutActionRecord>),
    Inventory(CloseoutInventoryPage),
    Review(Option<Box<CloseoutReview>>),
    Recovery(Option<Box<CloseoutRecoveryReview>>),
    RemovalProgress(CloseoutRemovalPage),
    History(Box<CloseoutHistory>),
    Execution(jcode_tool_types::execution::ExecutionResponse),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutSpec {
    pub location: LocationId,
    pub expected_generation: u64,
    /// Absent selects the private local closeout store. A supplied directory
    /// must already exist and is bound to its observed volume and witness.
    pub preservation_directory: Option<PathBuf>,
    #[serde(default)]
    pub conditional_no_loss: bool,
    #[serde(default)]
    pub full_archive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutStage {
    Preparing,
    NeedsDecision,
    Preserving,
    ReadyForApproval,
    Authorized,
    Removing,
    Closed,
    Revoked,
    RecoveryRequired,
    Retained,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutRecoveryAction {
    RestartPreparation,
    ResumeRemoval,
    UnregisterRetainFiles,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRecoveryPath {
    pub path: PathBuf,
    pub present: bool,
    pub matches_recorded_root: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRecoveryReview {
    pub id: ReviewId,
    pub operation: OperationId,
    pub revision: Revision,
    pub action: CloseoutRecoveryAction,
    pub paths: Vec<CloseoutRecoveryPath>,
    pub adopt_empty_holding: Option<PathBuf>,
    pub work: CloseoutWorkReport,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRecord {
    pub operation: OperationId,
    pub request: RequestId,
    pub spec: CloseoutSpec,
    pub revision: Revision,
    pub stage: CloseoutStage,
    pub initiated_by: String,
    pub inventory_digest: Option<String>,
    pub inventory_entries: u64,
    pub preservation_directory: PathBuf,
    /// Observed filesystem property, not a claim of encryption or a sandbox.
    #[serde(default)]
    pub preservation_volume_ownership: Option<bool>,
    #[serde(default)]
    pub authorization: Option<CloseoutAuthorization>,
    pub preservation_digest: Option<String>,
    pub quarantine: Option<PathBuf>,
    pub removed_entries: u64,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutReview {
    pub id: ReviewId,
    pub operation: OperationId,
    pub revision: Revision,
    pub inventory_digest: Option<String>,
    pub preservation_digest: Option<String>,
    pub references_digest: String,
    pub work: CloseoutWorkReport,
    pub issues: Vec<Issue>,
    pub preservation_volume_ownership: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutReviewTarget {
    pub operation: OperationId,
    pub review: ReviewId,
}

impl CloseoutReview {
    pub fn target(&self) -> CloseoutReviewTarget {
        CloseoutReviewTarget {
            operation: self.operation,
            review: self.id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutAuthorization {
    pub review: ReviewId,
    pub seal: String,
    pub source: CloseoutAuthorizationSource,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CloseoutAuthorizationSource {
    Human { client: String },
    Conditional { session: String, assessment: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutEntryKind {
    File,
    Directory,
    Symlink,
    Mount,
    Special,
    Git,
    Reference,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutEntry {
    /// Bound to inventory contents, never a display row number.
    pub id: String,
    pub path: PathBuf,
    pub kind: CloseoutEntryKind,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub link_target: Option<PathBuf>,
    pub links: u64,
    pub mode: u32,
    pub facts: Vec<String>,
    pub blockers: Vec<Issue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloseoutDisposition {
    Retain {
        reason: String,
    },
    Preserve,
    /// Verification compares the source with this exact readable destination.
    Preserved {
        path: PathBuf,
    },
    Redundant {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutDecision {
    pub entry: String,
    pub disposition: CloseoutDisposition,
    pub recorded_by: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutInventoryPage {
    pub operation: OperationId,
    pub digest: String,
    pub total: u64,
    pub entries: Vec<CloseoutEntry>,
    pub next: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutEntryProgress {
    NotProcessed,
    Unconfirmed,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRemovalEntry {
    pub id: String,
    pub path: PathBuf,
    pub progress: CloseoutEntryProgress,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRemovalPage {
    pub operation: OperationId,
    pub revision: Revision,
    pub total: u64,
    pub completed: u64,
    pub pending: Option<CloseoutRemovalEntry>,
    pub entries: Vec<CloseoutRemovalEntry>,
    pub next: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloseoutHistory {
    pub location: crate::Location,
    pub operation: OperationId,
    pub record: Option<CloseoutRecord>,
    pub preservation_paths: Vec<PathBuf>,
    pub report: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutWorkKind {
    Session,
    Execution,
    PendingInput,
    PendingControl,
    ExternalProcess,
    PhysicalLease,
    Executor,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutWorkFinding {
    pub kind: CloseoutWorkKind,
    pub identity: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseoutWorkReport {
    pub operation: OperationId,
    pub observed_at: String,
    pub findings: Vec<CloseoutWorkFinding>,
}
