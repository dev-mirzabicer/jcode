//! Read-only instruction inspection values shared by the domain and transport.
//! Identities belong to an ephemeral inspection, never to session prompt state.
use serde::{Deserialize, Serialize};

pub const ROW_PAGE_SIZE: usize = 64;
pub const TEXT_PAGE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionFilter {
    pub search: String,
    pub kind: Option<String>,
    pub scope: Option<String>,
    pub repository: Option<String>,
    pub origin: Option<InstructionOrigin>,
    pub effective: Option<bool>,
    pub valid: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionOrigin {
    Managed,
    Legacy,
    External,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRow {
    /// Opaque, snapshot-local identity. Never interpreted as a filesystem path.
    pub key: String,
    pub id: String,
    pub name: String,
    pub kind: String,
    pub scope: String,
    pub repository: String,
    pub origin: InstructionOrigin,
    pub effective: bool,
    pub valid: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRepositoryRow {
    pub key: String,
    pub kind: String,
    pub root: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub health: String,
    pub dirty: bool,
    pub conflicts: usize,
    pub active_lease: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionInspectionSnapshot {
    pub snapshot: String,
    pub session_id: String,
    pub active_agent: Option<String>,
    pub repositories: Vec<InstructionRepositoryRow>,
    pub resources: InstructionRowsPage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRowsPage {
    pub offset: usize,
    pub total: usize,
    pub next: Option<usize>,
    pub rows: Vec<InstructionRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", content = "key", rename_all = "snake_case")]
pub enum InstructionInspectionTarget {
    Resource(String),
    Repository(String),
    Session,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionInspectionView {
    #[default]
    Source,
    Metadata,
    Rendered,
    System,
    Dependencies,
    History,
    WorkingDiff,
    ScopeComparison,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRevisionSelection {
    pub from: String,
    /// None requests content at `from`; Some requests the two-revision diff.
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum InstructionInspectionRequest {
    Open {
        filter: InstructionFilter,
    },
    Resources {
        snapshot: String,
        filter: InstructionFilter,
        offset: usize,
    },
    Detail {
        snapshot: String,
        target: InstructionInspectionTarget,
        view: InstructionInspectionView,
        revision: Option<InstructionRevisionSelection>,
    },
    Text {
        snapshot: String,
        document: String,
        offset: usize,
    },
    History {
        snapshot: String,
        target: InstructionInspectionTarget,
        offset: usize,
    },
    /// Stops pending inspection work and discards exact detail, not any source.
    Cancel,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionTextPage {
    pub document: String,
    pub title: String,
    pub offset: usize,
    pub total_bytes: usize,
    pub next: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionCommitRow {
    pub commit: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionHistoryPage {
    pub offset: usize,
    pub next: Option<usize>,
    pub commits: Vec<InstructionCommitRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum InstructionInspectionResult {
    Opened(InstructionInspectionSnapshot),
    Resources(InstructionRowsPage),
    Text(InstructionTextPage),
    History(InstructionHistoryPage),
    Canceled,
    Closed,
    Failed(InstructionInspectionFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionInspectionFailure {
    pub operation: String,
    pub detail: String,
    pub refresh_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionInspectionReply {
    pub session_id: String,
    pub snapshot: Option<String>,
    pub result: InstructionInspectionResult,
}
