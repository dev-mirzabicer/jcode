//! Shared execution persistence and source/output reading.
mod acceptance;
mod delegation;
pub use delegation::{ChildAdmission, ChildTurnClaim};
mod activity;
mod retention;
pub use retention::{RetentionIssue, RetentionReport};
mod snapshots;
pub use activity::{IDLE_SECONDS, SessionActivityGuard};
pub use snapshots::{SnapshotPruneOutcome, SnapshotRead};
mod delivery;
mod progress;
pub use delivery::{BackgroundDelivery, DeliveryAttempt, DeliveryChannel, DeliveryState};
pub use progress::ExecutionProgress;
#[cfg(unix)]
pub mod command_handoff;
pub mod control_transport;
pub mod inspection;
mod managed_read;
mod native_process;
mod output;
#[cfg(unix)]
pub mod owned_child;
mod part_read;
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

mod provider_ingress;
mod provider_result;
pub use provider_ingress::ProviderReceipt;
pub use provider_result::{
    received_sdk_result, sdk_failure_body, tool_result_blocks, undecodable_sdk_record,
};
