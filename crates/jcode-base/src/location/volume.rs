//! Read-only physical bindings. These facts are not write grants or creation permits.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

mod native;
mod paths;
pub use paths::{
    CheckoutDestination, DirectoryWitness, PathBinding, PhysicalBinding, ResolvedPath,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VolumeIdentity(String);
impl VolumeIdentity {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let uuid = uuid::Uuid::parse_str(&value).map_err(|_| {
            LocationError::new(
                LocationIssue::InvalidPath,
                Path::new(""),
                "invalid volume UUID",
            )
        })?;
        Ok(Self(uuid.hyphenated().to_string().to_uppercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for VolumeIdentity {
    type Error = LocationError;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}
impl From<VolumeIdentity> for String {
    fn from(value: VolumeIdentity) -> Self {
        value.0
    }
}

/// Fresh observations. Callers must resolve again before filesystem effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub identity: VolumeIdentity,
    pub mount: PathBuf,
    pub label: String,
    pub internal: bool,
    pub writable: bool,
    pub available_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationIssue {
    InvalidPath,
    OfflineVolume,
    WrongVolume,
    AmbiguousVolume,
    ReadOnlyVolume,
    ReplacedRoot,
    AlreadyExists,
    Unsupported,
    Io,
}
#[derive(Debug)]
pub struct LocationError {
    pub kind: LocationIssue,
    pub path: PathBuf,
    pub detail: String,
}
impl LocationError {
    fn new(kind: LocationIssue, path: &Path, detail: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            detail: detail.into(),
        }
    }
    fn io(path: &Path, error: impl std::fmt::Display) -> Self {
        Self::new(LocationIssue::Io, path, error.to_string())
    }
}
impl std::fmt::Display for LocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} at {}: {}",
            self.kind,
            self.path.display(),
            self.detail
        )
    }
}
impl std::error::Error for LocationError {}

type Result<T> = std::result::Result<T, LocationError>;
trait VolumeEnvironment: Send + Sync {
    fn mounted(&self) -> Result<Vec<VolumeInfo>>;
    fn containing(&self, existing: &Path) -> Result<VolumeInfo>;
}

#[derive(Clone)]
pub struct LocationResolver {
    environment: Arc<dyn VolumeEnvironment>,
}
impl Default for LocationResolver {
    fn default() -> Self {
        Self::new()
    }
}
impl LocationResolver {
    pub fn new() -> Self {
        Self {
            environment: Arc::new(native::NativeVolumes),
        }
    }

    /// Native workspace volume discovery is currently supported on macOS.
    pub fn mounted_volumes(&self) -> Result<Vec<VolumeInfo>> {
        self.environment.mounted()
    }

    pub fn containing_volume(&self, existing: &Path) -> Result<VolumeInfo> {
        let path = existing
            .canonicalize()
            .map_err(|e| LocationError::io(existing, e))?;
        self.environment.containing(&path)
    }

    pub fn volume(&self, identity: &VolumeIdentity) -> Result<VolumeInfo> {
        let mut found = self
            .mounted_volumes()?
            .into_iter()
            .filter(|v| &v.identity == identity);
        let first = found.next().ok_or_else(|| {
            LocationError::new(
                LocationIssue::OfflineVolume,
                Path::new(""),
                format!("volume {} is not mounted", identity.as_str()),
            )
        })?;
        if found.next().is_some() {
            return Err(LocationError::new(
                LocationIssue::AmbiguousVolume,
                &first.mount,
                "multiple mounts report this volume UUID; select a uniquely identifiable volume",
            ));
        }
        Ok(first)
    }
}

fn validate_relative(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
        || path.to_str().is_none()
    {
        return Err(LocationError::new(
            LocationIssue::InvalidPath,
            path,
            "expected a UTF-8 relative path without traversal",
        ));
    }
    Ok(())
}
fn validate_absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path.components().any(|p| matches!(p, Component::ParentDir))
    {
        return Err(LocationError::new(
            LocationIssue::InvalidPath,
            path,
            "expected an absolute UTF-8 path without traversal",
        ));
    }
    Ok(())
}

/// Archive compatibility adapter. The archive still owns its pinned handles,
/// relative-child creation and stored configuration. No mount is ever created.
pub(crate) fn verify_archive_mount(mount: &Path, uuid: &str) -> anyhow::Result<()> {
    native::verify_archive_mount(mount, uuid)
}
pub(crate) use native::available_bytes;

#[cfg(test)]
mod tests;
