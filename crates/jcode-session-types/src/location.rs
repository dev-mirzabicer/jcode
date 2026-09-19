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
