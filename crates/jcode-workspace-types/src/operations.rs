//! Read-only discovery of durable workspace operations. Each entry carries the
//! public record its owner already returns, so a client that lost a request ID
//! can find and inspect the operation again without private storage access.
use crate::{
    CloneRecord, CloseoutRecord, Cursor, EntityId, LocationChangeRecord, PrimaryLaunchRecord,
    Revision, StartupCopyRecord,
};
use serde::{Deserialize, Serialize};

/// Operations whose public records support human inspection and follow-up.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Clone,
    Closeout,
    StartupCopy,
    PrimaryLaunch,
    PrimaryLocation,
}

/// The ledger state recorded by the owning operation, not a fresh observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Complete,
    Failed,
    RecoveryRequired,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationQuery {
    /// Operations that reference this catalog identity.
    #[serde(default)]
    pub target: Option<EntityId>,
    /// Launch and location operations of this Session.
    #[serde(default)]
    pub session: Option<String>,
    /// Empty selects every supported kind.
    #[serde(default)]
    pub kinds: Vec<OperationKind>,
    /// Omit complete operations. Failed and recovery-required remain visible.
    #[serde(default)]
    pub unfinished_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", rename_all = "snake_case")]
pub enum WorkspaceOperation {
    Clone(Box<CloneRecord>),
    Closeout(Box<CloseoutRecord>),
    StartupCopy(Box<StartupCopyRecord>),
    PrimaryLaunch(Box<PrimaryLaunchRecord>),
    PrimaryLocation(Box<LocationChangeRecord>),
}

impl WorkspaceOperation {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Clone(_) => OperationKind::Clone,
            Self::Closeout(_) => OperationKind::Closeout,
            Self::StartupCopy(_) => OperationKind::StartupCopy,
            Self::PrimaryLaunch(_) => OperationKind::PrimaryLaunch,
            Self::PrimaryLocation(_) => OperationKind::PrimaryLocation,
        }
    }
    pub fn operation(&self) -> crate::OperationId {
        match self {
            Self::Clone(record) => record.operation,
            Self::Closeout(record) => record.operation,
            Self::StartupCopy(record) => record.operation,
            Self::PrimaryLaunch(record) => record.operation,
            Self::PrimaryLocation(record) => record.operation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationEntry {
    pub state: OperationState,
    pub targets: Vec<EntityId>,
    pub operation: WorkspaceOperation,
}

/// Newest first. A continuation is valid only for the same query and revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationPage {
    pub revision: Revision,
    pub total: u64,
    pub items: Vec<OperationEntry>,
    pub next: Option<Cursor>,
}

/// Current Startup Context plan revisions that a copy review must bind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartupCopyPlans {
    pub catalog_revision: Revision,
    pub source_revision: u64,
    pub target_revision: u64,
    pub source_entries: u64,
}
