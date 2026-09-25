//! Private workspace organization. No primary activation or native-write policy
//! is enabled merely by opening this service.
use crate::location::volume::{LocationResolver, PhysicalBinding};
pub use jcode_workspace_types::*;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod backup;
mod checkout;
mod closeout;
mod use_gate;
pub use use_gate::WorkspaceUseLease;
mod context_scope;
mod grants;
pub use context_scope::ContextScopePlan;
mod native_mutation;
pub use grants::WorkspaceClientAuthority;
pub use native_mutation::WorkspaceMutationPermit;
mod organization;
mod portable;
mod primary_controls;
mod primary_filesystem;
#[cfg(test)]
mod primary_filesystem_tests;
mod primary_launch;
#[cfg(test)]
mod primary_launch_tests;
mod primary_location;
pub use primary_controls::PrimaryControlLease;
pub use primary_launch::PrimaryLaunchLease;
pub use primary_location::PreparedPrimaryLocation;
#[cfg(test)]
mod permission_tests;
#[cfg(test)]
mod process_tests;
mod query;
mod restore;
mod scope;
mod startup_copy;
pub use startup_copy::StartupCopyIntent;
mod storage;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
pub use storage::{CatalogLease, RootLease};

pub type Result<T> = std::result::Result<T, Issue>;
pub(crate) fn issue(code: IssueCode, detail: impl Into<String>) -> Issue {
    Issue {
        code,
        detail: detail.into(),
    }
}
pub(crate) fn io(e: impl std::fmt::Display) -> Issue {
    issue(IssueCode::Io, e.to_string())
}
pub(crate) fn corrupt(e: impl std::fmt::Display) -> Issue {
    issue(IssueCode::CorruptState, e.to_string())
}
pub(crate) fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(io)
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(corrupt)
}
pub(crate) fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

/// Cheap handle. Constructing or inspecting it does not initialize a catalog.
#[cfg(test)]
type FaultCheckpoint = std::sync::Arc<dyn Fn(&str) -> Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct WorkspaceService {
    root: PathBuf,
    resolver: LocationResolver,
    #[cfg(test)]
    fault: Option<FaultCheckpoint>,
}
impl WorkspaceService {
    pub fn new(state_root: &Path) -> Self {
        Self {
            root: state_root.join("workspace"),
            resolver: LocationResolver::new(),
            #[cfg(test)]
            fault: None,
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn checkpoint(&self, _stage: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(fault) = &self.fault {
            fault(_stage)?;
        }
        Ok(())
    }
    pub fn status(&self) -> Result<CatalogStatus> {
        let _lease = self.lease(false)?;
        storage::status(&self.connection()?)
    }
    fn connection(&self) -> Result<Connection> {
        storage::connect(&self.root)
    }
    pub fn inspect(&self, target: EntityId) -> Result<Entity> {
        let _lease = self.lease(false)?;
        entity(&self.connection()?, target)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundLocation {
    pub binding: PhysicalBinding,
}

pub(crate) fn entity(connection: &Connection, id: EntityId) -> Result<Entity> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM entities WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(corrupt)?;
    let value: Entity = decode(
        &body.ok_or_else(|| issue(IssueCode::InvalidIdentity, format!("Unknown identity {id}")))?,
    )?;
    if value.id() != id {
        return Err(issue(IssueCode::InvalidIdentity, "Identity kind mismatch"));
    }
    Ok(value)
}

#[cfg(unix)]
pub use closeout::{CLOSEOUT_EXECUTION_SESSION, CLOSEOUT_EXECUTION_TOOL, CloseoutRuntime};
