//! Shared execution persistence and source/output reading.
mod acceptance;
mod delivery;
mod progress;
pub use delivery::{BackgroundDelivery, DeliveryAttempt, DeliveryChannel, DeliveryState};
pub use progress::ExecutionProgress;
#[cfg(unix)]
pub mod command_handoff;
pub mod control_transport;
mod managed_read;
mod output;
#[cfg(unix)]
pub mod owned_child;
#[cfg(unix)]
pub mod process;
pub mod reader;
mod recovery;
mod runtime;
pub use output::present;
pub use runtime::RuntimeEndpoint;
mod capture;
mod storage;
mod store;
pub use capture::{Capture, output_digest};
pub use storage::{ArchiveConfig, StorageConfig};
pub use store::{ExecutionStore, Invocation, PreparedInvocation, RunRecord, RunState};

mod provider_result;
pub use provider_result::{received_sdk_result, sdk_failure_body, tool_result_blocks};
