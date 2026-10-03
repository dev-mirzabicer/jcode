//! Agent-facing workspace discovery views. These describe current catalog
//! facts for one Session. They carry no authority: access comes only from
//! trusted-client grants, and closeout removal only from its human-issued
//! authorization.
use crate::*;

/// A catalog identity with its current display name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamedEntity {
    pub id: EntityId,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScopeSummary {
    pub ordinary_roots: u64,
    pub granted_roots: u64,
    /// Included roots that cannot accept writes now (closing, closed,
    /// unavailable, retired). Physical availability is checked by `scope`.
    pub inactive_roots: u64,
    pub grants: u64,
}

/// Members directly below a project or work-area placement. `first` is a
/// bounded prefix; totals are complete and `list` pages the rest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub repositories: u64,
    pub work_areas: u64,
    pub locations: u64,
    pub first: Vec<Entity>,
    pub more: bool,
}

/// The current location facts of one placed Session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocationContext {
    pub session: String,
    pub placement: Placement,
    pub placement_name: String,
    /// Home chain, where the placement has one.
    pub project: Option<NamedEntity>,
    pub work_area: Option<NamedEntity>,
    pub repository: Option<NamedEntity>,
    /// The placement root for checkout, directory and standalone placements.
    pub location: Option<Location>,
    pub cwd: std::path::PathBuf,
    pub initial_cwd: std::path::PathBuf,
    pub location_revision: Revision,
    pub catalog_revision: Revision,
    pub scope: ScopeSummary,
    pub members: Option<MemberSummary>,
    pub pending_proposals: u64,
}

/// One page of a Session's writable roots, ordered by location identity.
/// Physical availability is verified for the returned roots only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScopePage {
    pub session: String,
    pub placement: Placement,
    pub session_revision: Revision,
    pub catalog_revision: Revision,
    pub total: u64,
    pub roots: Vec<WritableRoot>,
    /// Grants that are sources of the returned roots.
    pub grants: Vec<GrantDefinition>,
    pub next: Option<Cursor>,
}

/// Which registered root, if any, owns a path, and whether this Session may
/// write there now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocatedPath {
    pub path: std::path::PathBuf,
    pub location: Option<Location>,
    pub writable: bool,
    pub ordinary: bool,
    pub grants: Vec<GrantId>,
}
