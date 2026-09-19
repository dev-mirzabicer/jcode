//! Read-only launch preparation over the existing catalog and physical resolver.
use super::*;
use crate::session::StoredSessionLocation;

/// Kept by the launch owner until Session publication/reconciliation. This is not
/// an extra write grant or a replacement for invocation-time scope enforcement.
pub struct PreparedPrimaryLocation {
    pub location: StoredSessionLocation,
    pub catalog_revision: Revision,
    pub root: LocationId,
    _root_lease: RootLease,
}

impl WorkspaceService {
    /// Resolve an explicitly chosen cwd. Organization never supplies a guessed
    /// command directory and this operation creates no directory or clone.
    pub fn prepare_primary_location(
        &self,
        placement: Placement,
        cwd: Option<&Path>,
        operation: OperationId,
    ) -> Result<PreparedPrimaryLocation> {
        let cwd = cwd.ok_or_else(|| {
            issue(
                IssueCode::NeedsCwd,
                "Choose a concrete command working directory before launching",
            )
        })?;
        if !cwd.is_absolute() {
            return Err(issue(
                IssueCode::NeedsCwd,
                "Command working directory must be absolute",
            ));
        }
        let _catalog = self.lease(false)?;
        let connection = self.connection()?;
        let connection = connection.unchecked_transaction().map_err(io)?;
        let revision = storage::status(&connection)?.revision;
        query::validate_placement(&connection, placement)?;
        match entity(&connection, query::placement_target(placement))? {
            Entity::Project(p) if p.state == OrganizationState::Retired => {
                return Err(issue(IssueCode::InvalidIdentity, "Project is retired"));
            }
            Entity::WorkArea(a) if a.state == OrganizationState::Retired => {
                return Err(issue(IssueCode::InvalidIdentity, "Work area is retired"));
            }
            _ => {}
        }
        let binding = self.resolver.bind_directory(cwd).map_err(location_issue)?;
        let mut statement=connection.prepare("SELECT e.body,b.body FROM entities e JOIN bindings b ON b.location=e.id WHERE e.kind='location'").map_err(io)?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(io)?;
        let mut containing = Vec::new();
        for row in rows {
            let (body, physical) = row.map_err(io)?;
            let Entity::Location(location) = decode(&body)? else {
                return Err(corrupt("Location query returned another entity kind"));
            };
            let bound: BoundLocation = decode(&physical)?;
            // Historical closed roots cannot confer live authority. A new root
            // later occupying that spelling must acquire its own identity.
            if location.lifecycle == LocationLifecycle::Closed {
                continue;
            }
            if binding.observed_path().starts_with(&location.observed_path) {
                let resolved = self
                    .resolver
                    .resolve_directory(&bound.binding)
                    .map_err(location_issue)?;
                if resolved.relocated {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        "Working root moved; review its binding before launch",
                    ));
                }
                containing.push((location.observed_path.components().count(), location, bound));
            }
        }
        containing.sort_by_key(|(depth, _, _)| *depth);
        let (_,root,physical)=containing.pop().ok_or_else(||issue(IssueCode::PermissionRequired,"Working directory has no registered root in the selected placement; choose or register its location explicitly"))?;
        if root.retired || root.lifecycle != LocationLifecycle::Ready {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Working root is not ready for new primary work",
            ));
        }
        if !ordinary_member(&connection, placement, &root)? {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Working directory lies outside this placement's ordinary roots; resolve scope before launch",
            ));
        }
        let root_lease = self.acquire_root(root.id)?;
        root_lease.validate_binding(&physical.binding)?;
        self.resolver
            .resolve_directory(&binding)
            .map_err(location_issue)?;
        if self.status()?.revision != revision {
            return Err(issue(
                IssueCode::Conflict,
                "Catalog changed during primary location preparation",
            ));
        }
        Ok(PreparedPrimaryLocation {
            location: StoredSessionLocation {
                placement,
                initial_cwd: binding.observed_path().into(),
                cwd: binding,
                revision: 1,
                last_operation: Some(operation),
            },
            catalog_revision: revision,
            root: root.id,
            _root_lease: root_lease,
        })
    }
}

fn ordinary_member(
    connection: &Connection,
    placement: Placement,
    location: &Location,
) -> Result<bool> {
    Ok(match placement {
        Placement::Checkout(id) | Placement::Directory(id) | Placement::Standalone(id) => {
            id == location.id
        }
        Placement::WorkArea(id) => location.home == Some(Home::WorkArea(id)),
        Placement::Project(id) => match location.home {
            Some(Home::Project(home)) => home == id,
            Some(Home::WorkArea(area)) => {
                matches!(entity(connection,EntityId::WorkArea(area))?,Entity::WorkArea(area) if area.project==id)
            }
            None => false,
        },
    })
}
fn location_issue(error: crate::location::volume::LocationError) -> Issue {
    use crate::location::volume::LocationIssue;
    let code = match error.kind {
        LocationIssue::OfflineVolume
        | LocationIssue::WrongVolume
        | LocationIssue::AmbiguousVolume => IssueCode::OfflineVolume,
        LocationIssue::ReplacedRoot => IssueCode::ReplacedRoot,
        LocationIssue::Unsupported => IssueCode::UnsupportedCapability,
        LocationIssue::InvalidPath | LocationIssue::AlreadyExists => IssueCode::InvalidInput,
        _ => IssueCode::Io,
    };
    issue(code, error.to_string())
}
