//! Primary launch intent. These types describe choices, not permission grants.
use crate::{Home, OperationId, Placement, RequestId, Revision};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrimaryPlacement {
    Existing { placement: Placement },
    Standalone { root: PathBuf },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrimaryCwd {
    Existing { path: PathBuf },
    CreateEmpty { path: PathBuf, home: Option<Home> },
}
impl PrimaryCwd {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Existing { path } | Self::CreateEmpty { path, .. } => path,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryModel {
    pub model: String,
    pub provider: String,
    pub api_method: String,
    pub effort: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryLaunchInput {
    pub placement: PrimaryPlacement,
    pub cwd: Option<PrimaryCwd>,
    pub agent: Option<String>,
    pub model: Option<PrimaryModel>,
    #[serde(default)]
    pub selfdev: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryLaunchState {
    Pending,
    Complete,
    Failed,
    RecoveryRequired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryLaunchRecord {
    pub request: RequestId,
    pub operation: OperationId,
    pub session: String,
    pub input: PrimaryLaunchInput,
    pub concrete_model: PrimaryModel,
    pub registration_request: RequestId,
    pub state: PrimaryLaunchState,
    pub reviewed_revision: Revision,
    pub published_revision: Option<Revision>,
    pub issue: Option<String>,
    #[serde(default)]
    pub backup_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryLaunchRequest {
    pub request: RequestId,
    pub expected_revision: Revision,
    pub input: PrimaryLaunchInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrimaryLaunchResponse {
    Launched {
        record: Box<PrimaryLaunchRecord>,
    },
    Rejected {
        request: RequestId,
        issue: crate::Issue,
    },
}

/// A trusted, explicit change. Revisions refer to Session authority and the
/// reviewed catalog, not a client's currently selected row or command cwd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationChangeRequest {
    pub request: RequestId,
    pub session: String,
    pub expected_session_revision: Revision,
    pub expected_catalog_revision: Revision,
    pub placement: Placement,
    pub cwd: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationChangeState {
    Pending,
    Complete,
    Cancelled,
    Failed,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationChangeRecord {
    pub operation: OperationId,
    pub input: LocationChangeRequest,
    pub state: LocationChangeState,
    pub effective_revision: Option<Revision>,
    pub notice_message: Option<String>,
    pub issue: Option<crate::Issue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrimaryLocationCommand {
    Change { request: LocationChangeRequest },
    Cancel { operation: OperationId },
    Inspect { operation: OperationId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrimaryLocationResponse {
    State { record: Box<LocationChangeRecord> },
    Rejected { issue: crate::Issue },
}
