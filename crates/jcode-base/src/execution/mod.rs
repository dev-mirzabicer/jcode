//! Shared execution persistence and source/output reading.
mod acceptance;
mod delivery;
pub use delivery::{BackgroundDelivery, DeliveryAttempt, DeliveryChannel, DeliveryState};
#[cfg(unix)]
pub mod command_handoff;
pub mod control_transport;
mod output;
#[cfg(unix)]
pub mod process;
pub mod reader;
mod runtime;
pub use output::present;
pub use runtime::RuntimeEndpoint;
mod capture;
mod storage;
mod store;
pub use capture::Capture;
pub use storage::{ArchiveConfig, StorageConfig};
pub use store::{ExecutionStore, Invocation, PreparedInvocation, RunRecord, RunState};
