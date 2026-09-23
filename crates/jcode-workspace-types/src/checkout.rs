use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneSource {
    Remote { url: String },
    Local { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceVolume {
    pub uuid: String,
    pub mount: PathBuf,
    pub label: String,
    pub internal: bool,
    pub writable: bool,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneBase {
    Branch { name: String },
    Tag { name: String },
    Commit { oid: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneBranch {
    KeepName,
    Create { name: String },
    Detached,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneDestination {
    Default {
        volume_uuid: String,
        project_component: String,
        checkout_component: String,
    },
    Custom {
        volume_uuid: String,
        path: PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneRemote {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneSpec {
    pub home: Home,
    pub repository: RepositoryId,
    pub name: String,
    pub source: CloneSource,
    pub base: CloneBase,
    pub branch: CloneBranch,
    pub remotes: Vec<CloneRemote>,
    pub destination: CloneDestination,
    pub submodules: bool,
    pub lfs: bool,
    #[serde(default, alias = "trusted_local_submodule_urls")]
    pub trusted_submodule_urls: Vec<String>,
    #[serde(default)]
    pub trusted_lfs_urls: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneReview {
    pub id: ReviewId,
    pub revision: Revision,
    pub spec: CloneSpec,
    pub source_commit: String,
    pub destination: PathBuf,
    pub volume_uuid: String,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneState {
    Pending,
    Acquiring,
    Materializing,
    AwaitingTrust,
    Verifying,
    Publishing,
    Ready,
    PreparationFailed,
    Cancelled,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneTrustKind {
    Submodule,
    Lfs,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneTrustSource {
    pub kind: CloneTrustKind,
    /// Relative location of the declaring repository inside the owned stage.
    pub repository: PathBuf,
    /// Submodule path relative to that repository, or `.lfsconfig`.
    pub path: PathBuf,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneTrustReview {
    pub id: ReviewId,
    pub clone: RequestId,
    pub catalog_revision: Revision,
    pub clone_revision: Revision,
    pub stage: PathBuf,
    pub source_commit: String,
    pub sources: Vec<CloneTrustSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneTrustApproval {
    pub request: RequestId,
    pub review: ReviewId,
    pub issued_by: String,
    pub sources: Vec<CloneTrustSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneRecord {
    pub operation: OperationId,
    pub request: RequestId,
    pub location: LocationId,
    pub review: CloneReview,
    pub state: CloneState,
    pub cancel_requested: bool,
    pub stage: Option<PathBuf>,
    pub output_runs: Vec<String>,
    #[serde(default)]
    pub discovered_sources: Vec<CloneTrustSource>,
    #[serde(default)]
    pub pending_trust: Vec<CloneTrustSource>,
    #[serde(default)]
    pub trust_approvals: Vec<CloneTrustApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_issue: Option<Issue>,
    pub issue: Option<Issue>,
    pub revision: Revision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebindRecord {
    pub operation: OperationId,
    pub location: LocationId,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub old_volume_uuid: String,
    pub new_volume_uuid: String,
    pub old_generation: u64,
    pub new_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupCopyApproval {
    pub source_spec_id: String,
    pub approved_resolved_target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupCopyEntry {
    pub source_spec_id: String,
    pub selected_path: PathBuf,
    pub resolved_target: PathBuf,
    pub external: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupCopyReview {
    pub id: ReviewId,
    pub catalog_revision: Revision,
    pub source: PathBuf,
    pub target: LocationId,
    pub target_path: PathBuf,
    pub target_binding_generation: u64,
    pub source_plan_revision: u64,
    pub target_plan_revision: u64,
    pub proposed_plan_revision: u64,
    pub entries: Vec<StartupCopyEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupCopyState {
    Pending,
    Complete,
    RecoveryRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupCopyRecord {
    pub request: RequestId,
    pub operation: OperationId,
    pub review: StartupCopyReview,
    pub state: StartupCopyState,
    pub issue: Option<Issue>,
}
