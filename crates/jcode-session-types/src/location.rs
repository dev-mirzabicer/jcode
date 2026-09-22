//! Placement is Session authority. Physical resolution remains its existing owner.
use jcode_workspace_types::{OperationId, Placement, Revision};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The binding parameter keeps physical identity policy out of the stored-session
/// crate. Production uses the shared location resolver's exact PhysicalBinding,
/// rather than copying its witness schema or introducing another path resolver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSessionLocation<Binding> {
    pub placement: Placement,
    pub cwd: Binding,
    pub initial_cwd: PathBuf,
    pub revision: Revision,
    pub last_operation: Option<OperationId>,
}

/// A ready checkpoint is written only after complete primary preparation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPrimaryCreation {
    pub request: jcode_workspace_types::RequestId,
    pub operation: OperationId,
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredContextScope {
    pub operation: OperationId,
    pub ready: bool,
}

/// Last explanation committed with Session history, not permission authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredScopeNotice {
    pub location_revision: u64,
    pub installation: jcode_workspace_types::InstallationId,
    pub catalog_revision: u64,
    pub fingerprint: String,
    pub message: String,
}
