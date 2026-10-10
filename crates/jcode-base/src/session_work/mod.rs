//! Session work: a session's workflow and the later stop, interaction,
//! proposal, closeout and timing records, in one private store.
//!
//! The store is the authority. Agents edit a host-owned `workflow.md` with
//! their native file tools; the host validates each text before it commits a
//! revision and writes the file. See `docs/SESSION_WORK.md`.
use std::path::PathBuf;

pub mod activation;
pub mod module_types;
pub mod store;
pub mod surface;
pub mod workflow;

pub use activation::*;
pub use module_types::*;
pub use store::{CommitReceipt, InitialRevision, SessionWorkStore, WorkflowHead};
pub use surface::*;
pub use workflow::{
    WorkflowErrors, WorkflowIssue, derived_parent_status, parse_workflow, render_workflow,
};

/// Whether new sessions are created with session work.
pub fn session_work_enabled() -> bool {
    crate::config::config().features.session_work
}

/// Why a session-work operation failed. Every variant leaves stored state
/// unchanged.
#[derive(Debug)]
pub enum SessionWorkError {
    /// The store file is absent although state was expected.
    StoreMissing(PathBuf),
    /// The store or one of its records cannot be read.
    Corrupt(String),
    /// The store was written by an unknown schema.
    UnknownSchema(String),
    Io(String),
    /// The session has no session-work activation.
    NotActivated(String),
    /// A request identity was reused with different content.
    Conflict(String),
    /// The workflow text breaks the grammar or its rules.
    InvalidWorkflow(WorkflowErrors),
    /// The operation is not allowed.
    Invalid(String),
}

impl SessionWorkError {
    pub(crate) fn corrupt(error: impl std::fmt::Display) -> Self {
        Self::Corrupt(error.to_string())
    }
    pub(crate) fn io(error: impl std::fmt::Display) -> Self {
        Self::Io(error.to_string())
    }
}

impl std::fmt::Display for SessionWorkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoreMissing(path) => write!(
                f,
                "The session-work store is missing ({}). Restore it before this session continues; its workflow was not treated as empty.",
                path.display()
            ),
            Self::Corrupt(detail) => write!(
                f,
                "The session-work store cannot be read ({detail}). Repair or restore it before this session continues."
            ),
            Self::UnknownSchema(detail) => write!(
                f,
                "The session-work store has an unknown schema ({detail}). Use a matching Jcode version or restore the store."
            ),
            Self::Io(detail) => write!(f, "Session-work storage failed: {detail}"),
            Self::NotActivated(session) => {
                write!(f, "Session {session} does not use session work.")
            }
            Self::Conflict(detail) => write!(f, "Session-work request conflict: {detail}"),
            Self::InvalidWorkflow(errors) => errors.fmt(f),
            Self::Invalid(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for SessionWorkError {}
