//! Private workspace organization. No primary activation or native-write policy
//! is enabled merely by opening this service.
use crate::location::volume::{LocationResolver, PhysicalBinding};
pub use jcode_workspace_types::*;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod backup;
mod organization;
mod portable;
mod query;
mod restore;
mod storage;
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
#[derive(Clone)]
pub struct WorkspaceService {
    root: PathBuf,
    resolver: LocationResolver,
    #[cfg(test)]
    fault: Option<std::sync::Arc<dyn Fn(&str) -> Result<()> + Send + Sync>>,
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
