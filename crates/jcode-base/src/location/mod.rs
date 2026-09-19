//! Shared physical location facts, separate from logical workspace ownership.
//! Resolving a location never loads plans, initializes instruction stores, or
//! changes a Session. Legacy keys keep their original path-based digest.
mod project;
mod resolve;
pub mod volume;
pub use project::{ProjectFacts, ProjectKey};
pub use resolve::resolve_project;

#[derive(Debug)]
pub struct ProjectResolutionError {
    pub path: std::path::PathBuf,
    pub detail: String,
}
impl std::fmt::Display for ProjectResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not resolve project identity for {}: {}",
            self.path.display(),
            self.detail
        )
    }
}
impl std::error::Error for ProjectResolutionError {}
