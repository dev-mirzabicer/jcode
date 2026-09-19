//! Startup Context error adapter. Physical discovery has no plan/storage policy.
use super::{ActiveProject, StartupContextError};
use std::path::Path;

pub(super) fn resolve_project(launch_dir: &Path) -> Result<ActiveProject, StartupContextError> {
    crate::location::resolve_project(launch_dir).map_err(|error| {
        StartupContextError::ProjectIdentity {
            path: error.path,
            detail: error.detail,
        }
    })
}
