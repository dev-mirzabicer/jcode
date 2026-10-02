//! Reviewed runtime control. Work observations are not terminal receipts.
use crate::{OperationId, RequestId, ReviewId, Revision};
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "runtime_lifecycle_v1";
/// Planned restart, selected crash recovery and power inspection. Clients send
/// supervision requests or a non-default destination only after negotiating it.
pub const SUPERVISION_CAPABILITY: &str = "runtime_supervision_v1";

/// Where the runtime goes after verified quiescence. Only `Stopped` records an
/// intentional Stop; a restart keeps the runtime desired-available.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDestination {
    #[default]
    Stopped,
    Restart,
}

impl RuntimeDestination {
    pub fn is_stopped(&self) -> bool {
        *self == Self::Stopped
    }
}

/// Who initiated a shutdown. External signals are not reviewed user intent and
/// never record an intentional Stop.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownOrigin {
    #[default]
    Reviewed,
    ExternalSignal,
}

impl ShutdownOrigin {
    pub fn is_reviewed(&self) -> bool {
        *self == Self::Reviewed
    }
}

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
    #[serde(default, skip_serializing_if = "RuntimeDestination::is_stopped")]
    pub destination: RuntimeDestination,
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
    #[serde(default, skip_serializing_if = "ShutdownOrigin::is_reviewed")]
    pub origin: ShutdownOrigin,
}

impl ShutdownOperation {
    /// Only reviewed intent to stop leaves the runtime desired-stopped. A
    /// restart and an external signal keep it available for the next start.
    pub fn records_intentional_stop(&self) -> bool {
        self.origin.is_reviewed() && self.review.options.destination.is_stopped()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub namespace: String,
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
    /// Requires `runtime_supervision_v1`.
    Supervision {},
    /// Resolve one unexpected-exit recovery item exactly once. Requires
    /// `runtime_supervision_v1`. The same request ID replays its outcome.
    Recover {
        item: RecoveryId,
        expected_revision: Revision,
        request: RequestId,
        decision: RecoveryDecision,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RuntimeResponse {
    Status(RuntimeStatus),
    Review(ShutdownReview),
    Operation(ShutdownOperation),
    Error(crate::Issue),
    Supervision(SupervisionStatus),
    Recovery(RecoveryItem),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecoveryId(uuid::Uuid);
impl RecoveryId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}
impl Default for RecoveryId {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for RecoveryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::str::FromStr for RecoveryId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(s).map(Self)
    }
}

/// Why a primary turn ended without a terminal record from its runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCause {
    /// The runtime process ended without a verified shutdown outcome.
    UnexpectedExit,
    /// An explicit Force ended the runtime with uncertain outcomes.
    ForcedExit,
    /// An external termination signal interrupted the turn.
    ExternalSignal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDecision {
    Continue,
    LeaveStopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryResolution {
    /// A trusted client chose continuation; this input carries it.
    Continued {
        input: RequestId,
    },
    LeftStopped {},
    /// A later human message to the session resolved the interruption.
    SupersededByInput {
        input: RequestId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryResolved {
    pub resolution: RecoveryResolution,
    /// Present for trusted-client decisions; replay with it returns this item.
    pub request: Option<RequestId>,
    pub resolved_at: String,
}

/// Execution owned by the interrupted session that had not reached a terminal
/// receipt when inspected. A live owner is still running; it is not replayed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryExecution {
    pub id: String,
    pub tool: String,
    pub state: String,
    pub live_owner: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryItem {
    pub id: RecoveryId,
    pub session: String,
    /// Durable turn identity from the runtime that admitted it.
    pub turn: String,
    /// Runtime incarnation that admitted the interrupted turn.
    pub runtime: String,
    pub cause: RecoveryCause,
    pub detected_at: String,
    pub revision: Revision,
    pub resolved: Option<RecoveryResolved>,
    /// Fresh inspection, filled by the live coordinator; not durable authority.
    #[serde(default)]
    pub executions: Vec<RecoveryExecution>,
}

/// Actual inhibitor state. `active` reports a held platform assertion, not the
/// desired state; `available` is false on unsupported platforms or opt-out.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerStatus {
    pub enabled: bool,
    pub available: bool,
    pub active: bool,
    pub active_work: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisionStatus {
    pub namespace: String,
    pub runtime: String,
    /// The process was launched by the namespaced login service.
    pub supervised: bool,
    pub power: PowerStatus,
    /// Unresolved items first, then recently resolved items.
    pub recoveries: Vec<RecoveryItem>,
}

impl RuntimeRequest {
    /// Requests that only a `runtime_supervision_v1` runtime understands.
    /// Clients negotiate before sending; older runtimes reject them.
    pub fn requires_supervision(&self) -> bool {
        match self {
            Self::Supervision {} | Self::Recover { .. } => true,
            Self::Review { options } | Self::ReviewChange { options, .. } => {
                !options.destination.is_stopped()
            }
            _ => false,
        }
    }

    /// Correlation only. This never grants authority or infers completion from
    /// a transport acknowledgement. Begin replay may return later durable state.
    pub fn matches_response(&self, response: &RuntimeResponse) -> bool {
        match (self, response) {
            (_, RuntimeResponse::Error(_)) => true,
            (Self::Status {}, RuntimeResponse::Status(_)) => true,
            (Self::Review { options }, RuntimeResponse::Review(review)) => {
                &review.options == options && review.replaces.is_none()
            }
            (
                Self::ReviewChange {
                    operation,
                    expected_revision,
                    options,
                },
                RuntimeResponse::Review(review),
            ) => {
                &review.options == options
                    && review.replaces.as_ref().is_some_and(|prior| {
                        prior.operation == *operation && prior.revision == *expected_revision
                    })
            }
            (Self::Begin { request, review }, RuntimeResponse::Operation(op)) => {
                op.request == *request && op.review.id == *review
            }
            (Self::Inspect { operation }, RuntimeResponse::Operation(op)) => op.id == *operation,
            (Self::Supervision {}, RuntimeResponse::Supervision(_)) => true,
            (
                Self::Recover {
                    item: id, request, ..
                },
                RuntimeResponse::Recovery(item),
            ) => {
                item.id == *id
                    && item
                        .resolved
                        .as_ref()
                        .is_some_and(|resolved| resolved.request == Some(*request))
            }
            (
                Self::CancelWait {
                    operation,
                    expected_revision,
                },
                RuntimeResponse::Operation(op),
            ) => {
                op.id == *operation
                    && expected_revision.checked_add(1) == Some(op.revision)
                    && op.phase == ShutdownPhase::Cancelled
            }
            (
                Self::Retry {
                    operation,
                    expected_revision,
                },
                RuntimeResponse::Operation(op),
            ) => {
                op.id == *operation
                    && expected_revision.checked_add(1) == Some(op.revision)
                    && op.phase == ShutdownPhase::Stopping
                    && op.cancellation_closed
            }
            (
                Self::Force {
                    operation,
                    expected_revision,
                },
                RuntimeResponse::Operation(op),
            ) => {
                op.id == *operation
                    && expected_revision.checked_add(1) == Some(op.revision)
                    && op.phase == ShutdownPhase::Stopping
                    && op.cancellation_closed
                    && op.force_requested
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        request: RuntimeRequest,
        response: serde_json::Value,
        accepted: bool,
    }

    #[test]
    fn shared_runtime_correlation_matrix() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("runtime_correlation.json")).unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let accepted = serde_json::from_value::<RuntimeResponse>(case.response)
                .is_ok_and(|response| case.request.matches_response(&response));
            assert_eq!(accepted, case.accepted, "{}", case.name);
        }
    }
}
