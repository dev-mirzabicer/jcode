//! A closeout is one exact human-authorized transaction, not permission to
//! delete an arbitrary path. File bodies and large inventories are separate
//! private records; ordinary operation inspection returns metadata only.
use crate::{Issue, LocationId, OperationId, RequestId, Revision};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub preservation_digest: Option<String>,
    pub quarantine: Option<PathBuf>,
    pub removed_entries: u64,
    pub issues: Vec<Issue>,
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
