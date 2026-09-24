//! Read-only launch preparation over the existing catalog and physical resolver.
use super::scope::ordinary_member;
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

enum PreparationScope<'a> {
    Placement,
    Session(&'a str),
    Carry(&'a ContextScopePlan),
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
        self.prepare_location(placement, cwd, operation, PreparationScope::Placement)
    }
    pub fn prepare_session_location(
        &self,
        session: &str,
        placement: Placement,
        cwd: Option<&Path>,
        operation: OperationId,
    ) -> Result<PreparedPrimaryLocation> {
        self.prepare_location(
            placement,
            cwd,
            operation,
            PreparationScope::Session(session),
        )
    }
    pub fn prepare_context_location(
        &self,
        plan: &ContextScopePlan,
        operation: OperationId,
    ) -> Result<PreparedPrimaryLocation> {
        let location = plan.source_location();
        self.prepare_location(
            location.placement,
            Some(location.cwd.observed_path()),
            operation,
            PreparationScope::Carry(plan),
        )
    }
    fn prepare_location(
        &self,
        placement: Placement,
        cwd: Option<&Path>,
        operation: OperationId,
        scope: PreparationScope<'_>,
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
        validate_primary_placement(&connection, placement)?;
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
            if location.lifecycle.is_historical() {
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
        let session = match &scope {
            PreparationScope::Session(session) => *session,
            _ => "",
        };
        let mut grants = super::scope::applicable_grants(&connection, session, placement)?;
        if let PreparationScope::Carry(plan) = &scope {
            grants.extend_from_slice(plan.grants_for_copy(&connection)?);
        }
        let mut allowed = ordinary_member(&connection, placement, &root)?;
        for grant in &grants {
            allowed |= super::scope::target_contains(&connection, &grant.target, &root)?;
        }
        if !allowed {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Command cwd is outside the new placement's writable roots; choose an ordinary cwd or explicitly carry/approve its grant",
            ));
        }
        // Preparation protects against relocation/removal, not ordinary work
        // in the same checkout. Multiple sessions may share a busy Ready root.
        let root_lease = self.acquire_mutation_binding(&physical.binding)?;
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

pub(super) fn location_issue(error: crate::location::volume::LocationError) -> Issue {
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

pub(super) fn validate_primary_placement(
    connection: &Connection,
    placement: Placement,
) -> Result<()> {
    query::validate_placement(connection, placement)?;
    match entity(connection, query::placement_target(placement))? {
        Entity::Project(p) if p.state == OrganizationState::Retired => {
            Err(issue(IssueCode::InvalidIdentity, "Project is retired"))
        }
        Entity::WorkArea(a) if a.state == OrganizationState::Retired => {
            Err(issue(IssueCode::InvalidIdentity, "Work area is retired"))
        }
        Entity::Location(location)
            if location.retired || location.lifecycle != LocationLifecycle::Ready =>
        {
            Err(issue(
                IssueCode::RecoveryRequired,
                "Placement is not ready for primary work",
            ))
        }
        _ => Ok(()),
    }
}
