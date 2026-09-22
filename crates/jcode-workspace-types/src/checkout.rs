use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneSource {
    Remote { url: String },
    Local { path: PathBuf },
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
    #[serde(default)]
    pub trusted_local_submodule_urls: Vec<String>,
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
    Verifying,
    Publishing,
    Ready,
    PreparationFailed,
    Cancelled,
    RecoveryRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneRecord {
    pub operation: OperationId,
    pub request: RequestId,
    pub location: LocationId,
    pub review: CloneReview,
    pub state: CloneState,
    pub stage: Option<PathBuf>,
    pub output_run: Option<String>,
    pub issue: Option<Issue>,
    pub revision: Revision,
}
