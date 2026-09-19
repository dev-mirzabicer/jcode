//! Workspace contracts contain no storage, filesystem execution or UI policy.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);
        impl $name {
            pub fn new() -> Self { Self(uuid::Uuid::new_v4()) }
        }
        impl Default for $name { fn default() -> Self { Self::new() } }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> { uuid::Uuid::parse_str(s).map(Self) }
        }
    )+};
}
identity!(
    ProjectId,
    RepositoryId,
    WorkAreaId,
    LocationId,
    GrantId,
    ProposalId,
    OperationId,
    ReviewId,
    RequestId,
    InstallationId,
    SnapshotId
);

pub type Revision = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Home {
    Project(ProjectId),
    WorkArea(WorkAreaId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EntityId {
    Project(ProjectId),
    Repository(RepositoryId),
    WorkArea(WorkAreaId),
    Location(LocationId),
}
impl std::fmt::Display for EntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(id) => id.fmt(f),
            Self::Repository(id) => id.fmt(f),
            Self::WorkArea(id) => id.fmt(f),
            Self::Location(id) => id.fmt(f),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Placement {
    Project(ProjectId),
    WorkArea(WorkAreaId),
    Checkout(LocationId),
    Directory(LocationId),
    Standalone(LocationId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationState {
    Active,
    Archived,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub state: OrganizationState,
    pub revision: Revision,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub id: RepositoryId,
    pub name: String,
    pub remotes: Vec<String>,
    pub state: OrganizationState,
    pub revision: Revision,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkArea {
    pub id: WorkAreaId,
    pub project: ProjectId,
    pub name: String,
    pub state: OrganizationState,
    pub revision: Revision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutOrigin {
    ManagedClone,
    AdoptedGit,
    LinkedWorktree,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocationKind {
    Checkout {
        repository: RepositoryId,
        origin: CheckoutOrigin,
        common_directory: PathBuf,
    },
    Directory,
    Standalone {
        git: bool,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationLifecycle {
    Provisioning,
    Ready,
    PreparationFailed,
    Unavailable,
    Closing,
    Closed,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub id: LocationId,
    pub name: String,
    pub home: Option<Home>,
    pub kind: LocationKind,
    pub observed_path: PathBuf,
    pub volume_uuid: String,
    pub binding_generation: u64,
    pub lifecycle: LocationLifecycle,
    pub retired: bool,
    pub revision: Revision,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Entity {
    Project(Project),
    Repository(Repository),
    WorkArea(WorkArea),
    Location(Location),
}
impl Entity {
    pub fn id(&self) -> EntityId {
        match self {
            Self::Project(p) => EntityId::Project(p.id),
            Self::Repository(r) => EntityId::Repository(r.id),
            Self::WorkArea(a) => EntityId::WorkArea(a.id),
            Self::Location(l) => EntityId::Location(l.id),
        }
    }
    pub fn revision(&self) -> Revision {
        match self {
            Self::Project(p) => p.revision,
            Self::Repository(r) => r.revision,
            Self::WorkArea(a) => a.revision,
            Self::Location(l) => l.revision,
        }
    }
}

/// Registration records existing physical data. It never provisions or moves files.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Registration {
    Checkout {
        home: Home,
        repository: RepositoryId,
    },
    Directory {
        home: Home,
    },
    Standalone,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrganizationChange {
    CreateProject {
        name: String,
    },
    CreateRepository {
        name: String,
        remotes: Vec<String>,
    },
    AssociateRepository {
        project: ProjectId,
        repository: RepositoryId,
    },
    RemoveRepositoryAssociation {
        project: ProjectId,
        repository: RepositoryId,
    },
    CreateWorkArea {
        project: ProjectId,
        name: String,
    },
    RegisterLocation {
        name: String,
        path: PathBuf,
        registration: Registration,
    },
    MoveLocation {
        location: LocationId,
        home: Home,
        associate_repository: bool,
    },
    AdoptStandalone {
        location: LocationId,
        home: Home,
        repository: Option<RepositoryId>,
        associate_repository: bool,
    },
    Rename {
        target: EntityId,
        name: String,
    },
    Archive {
        target: EntityId,
        archived: bool,
    },
    Retire {
        target: EntityId,
    },
    DiscardUnused {
        target: EntityId,
    },
    SetVolumeDefault {
        volume_uuid: String,
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Project,
    Repository,
    WorkArea,
    Location,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Current,
    All,
    Archived,
    Retired,
    Closed,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub kind: Option<EntityKind>,
    pub project: Option<ProjectId>,
    pub home: Option<Home>,
    pub repository: Option<RepositoryId>,
    pub visibility: Visibility,
    pub active_sessions_only: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub revision: Revision,
    pub after: String,
    pub query_digest: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub revision: Revision,
    pub total: u64,
    pub items: Vec<Entity>,
    pub next: Option<Cursor>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogStatus {
    pub installation: InstallationId,
    pub schema: u32,
    pub revision: Revision,
    pub managed_rollout: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub revision: Revision,
    pub change: OrganizationChange,
    pub targets: Vec<EntityId>,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub operation: OperationId,
    pub request: RequestId,
    pub revision: Revision,
    pub targets: Vec<EntityId>,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    InvalidIdentity,
    Conflict,
    Busy,
    OfflineVolume,
    ReplacedRoot,
    CorruptState,
    RecoveryRequired,
    UnsupportedCapability,
    InvalidInput,
    Referenced,
    BackupFailed,
    Io,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub code: IssueCode,
    pub detail: String,
}
impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.detail)
    }
}
impl std::error::Error for Issue {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Audience {
    Session(String),
    Project(ProjectId),
    WorkArea(WorkAreaId),
    Checkout(LocationId),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum WriteTarget {
    Root(LocationId),
    ProjectMembers(ProjectId),
    WorkAreaMembers(WorkAreaId),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Disabled,
    Active,
    Revoked,
}
/// Definitions only in the catalog foundation. Enforcement is a separate capability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantDefinition {
    pub id: GrantId,
    pub audience: Audience,
    pub target: WriteTarget,
    pub state: GrantState,
    pub revision: Revision,
    pub copied_from: Option<GrantId>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessProposal {
    pub id: ProposalId,
    pub session: String,
    pub target: WriteTarget,
    pub revision: Revision,
}

/// A projection of a committed Session, never a replacement Session authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIndex {
    pub session: String,
    pub placement: Placement,
    pub session_revision: Revision,
    pub operation: OperationId,
    pub active: bool,
    pub reconciled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportCollisionPolicy {
    Reject,
    NewIdentities,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationRemap {
    pub location: LocationId,
    pub path: PathBuf,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportReview {
    pub id: ReviewId,
    pub revision: Revision,
    pub source_installation: InstallationId,
    pub collisions: Vec<EntityId>,
    pub remapped: Vec<LocationId>,
    pub unavailable: Vec<LocationId>,
    pub disabled_grants: Vec<GrantId>,
    pub external_content: Vec<String>,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub revision: Revision,
    pub automatic: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreReview {
    pub id: ReviewId,
    pub snapshot: Snapshot,
    pub current_revision: Option<Revision>,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceRequest {
    Status,
    Initialize {
        request: RequestId,
    },
    List {
        query: Query,
        after: Option<Cursor>,
        limit: u32,
    },
    Inspect {
        target: EntityId,
    },
    Review {
        expected_revision: Revision,
        change: OrganizationChange,
    },
    Apply {
        request: RequestId,
        review: ReviewId,
    },
    InspectReceipt {
        request: RequestId,
    },
    Sessions {
        target: Option<EntityId>,
        after: Option<String>,
        limit: u32,
    },
    Backup {
        request: RequestId,
        name: String,
    },
    Snapshots,
    Export {
        request: RequestId,
        project: ProjectId,
        name: String,
    },
    ReviewImport {
        path: PathBuf,
        expected_revision: Revision,
        collisions: ImportCollisionPolicy,
        remap: Vec<LocationRemap>,
    },
    ApplyImport {
        request: RequestId,
        review: ReviewId,
    },
    ReviewRestore {
        snapshot: SnapshotId,
    },
    ApplyRestore {
        request: RequestId,
        review: ReviewId,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum WorkspaceResponse {
    Status(CatalogStatus),
    Page(Page),
    Entity(Entity),
    Review(Review),
    Receipt(Receipt),
    Sessions(Vec<SessionIndex>),
    Snapshot(Snapshot),
    Snapshots(Vec<Snapshot>),
    Export(PathBuf),
    ImportReview(ImportReview),
    RestoreReview(RestoreReview),
    Error(Issue),
}
