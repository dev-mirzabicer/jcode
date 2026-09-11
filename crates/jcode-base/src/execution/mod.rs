//! Shared execution persistence and source/output reading.
mod output;
pub mod reader;
pub use output::present;
mod capture;
mod storage;
mod store;
pub use capture::Capture;
pub use storage::{ArchiveConfig, StorageConfig};
pub use store::{ExecutionStore, Invocation, PreparedInvocation, RunRecord, RunState};
