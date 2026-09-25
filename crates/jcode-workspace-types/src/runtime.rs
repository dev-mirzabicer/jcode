//! Reviewed runtime control. Work observations are not terminal receipts.
use crate::{OperationId, RequestId, ReviewId, Revision};
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "runtime_lifecycle_v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStrategy {
    FinishCurrent,
    Interrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependentTasks {
    Stop,
    KeepSupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownOptions {
    pub strategy: StopStrategy,
    pub independent: IndependentTasks,
    /// Waiting for natural completion has no deadline. This bounds only the
    /// quiescence attempt after entering Stopping, never authorizes Force.
    pub quiescence_timeout_seconds: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeWorkKind {
    PrimaryTurn,
    Execution,
    BackgroundTask,
    Preparation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeWork {
    pub id: String,
    /// Originating runtime owner. A verified native worker may own capture now
    /// without changing the reviewed identity of this same command.
    pub owner: String,
    pub session: Option<String>,
    pub kind: RuntimeWorkKind,
    pub supported_survivor: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownReview {
    pub id: ReviewId,
    pub runtime: String,
    pub revision: Revision,
    pub options: ShutdownOptions,
    pub work: Vec<RuntimeWork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<ShutdownRevision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownRevision {
    pub operation: OperationId,
    pub revision: Revision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownPhase {
    WaitingForCurrent,
    Stopping,
    Blocked,
    Stopped,
    Cancelled,
    /// The coordinator disappeared before proving its outcome. This never
    /// implies interrupted commands can be executed again.
    Interrupted,
    Superseded,
    /// Explicit force authorized this owner to exit without claiming graceful
    /// quiescence. Remaining work and uncertain results stay inspectable.
    Forced,
}

impl ShutdownPhase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Cancelled | Self::Interrupted | Self::Superseded | Self::Forced
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownOperation {
    pub id: OperationId,
    pub request: RequestId,
    pub review: ShutdownReview,
    pub revision: Revision,
    pub phase: ShutdownPhase,
    pub force_requested: bool,
    #[serde(default)]
    pub cancellation_closed: bool,
    /// Fresh observations, distinct from the original immutable review.
    pub remaining: Vec<RuntimeWork>,
    pub preserved: Vec<RuntimeWork>,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    /// Present only on a response from the actual live coordinator.
    pub runtime: Option<String>,
    #[serde(default)]
    pub reload_in_progress: bool,
    pub desired_stopped: bool,
    pub revision: Revision,
    pub operation: Option<ShutdownOperation>,
    pub work: Vec<RuntimeWork>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeRequest {
    Status {},
    Review {
        options: ShutdownOptions,
    },
    ReviewChange {
        operation: OperationId,
        expected_revision: Revision,
        options: ShutdownOptions,
    },
    Begin {
        request: RequestId,
        review: ReviewId,
    },
    Inspect {
        operation: OperationId,
    },
    CancelWait {
        operation: OperationId,
        expected_revision: Revision,
    },
    Retry {
        operation: OperationId,
        expected_revision: Revision,
    },
    Force {
        operation: OperationId,
        expected_revision: Revision,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RuntimeResponse {
    Status(RuntimeStatus),
    Review(ShutdownReview),
    Operation(ShutdownOperation),
    Error(crate::Issue),
}
