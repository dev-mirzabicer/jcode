//! Current catalog relationships, not cwd or explanatory messages, determine scope.
use super::*;
use crate::session::{Session, StoredSessionLocation};

impl WorkspaceService {
    /// The caller supplies its authoritative Session, never a client-selected placement.
    /// Inspection includes unavailable roots. Mutation admission must reject their issues.
    pub fn session_write_scope(&self, session: &Session) -> Result<SessionWriteScope> {
        let location = session.location.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Review this legacy Session's placement and cwd before tool-enabled work",
            )
        })?;
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        self.scope_snapshot(&transaction, &session.id, location)
    }

    pub(super) fn scope_snapshot(
        &self,
        connection: &Connection,
        session: &str,
        location: &StoredSessionLocation,
    ) -> Result<SessionWriteScope> {
        self.scope_snapshot_mode(connection, session, location, true)
    }
    pub(super) fn scope_snapshot_mode(
        &self,
        connection: &Connection,
        session: &str,
        location: &StoredSessionLocation,
        verify_physical: bool,
    ) -> Result<SessionWriteScope> {
        query::validate_placement(connection, location.placement)?;
        let status = storage::status(connection)?;
        let grants = applicable_grants(connection, session, location.placement)?;
        let mut roots = Vec::new();
        for root in locations(connection)? {
            let ordinary = ordinary_member(connection, location.placement, &root)?;
            let mut sources = Vec::new();
            for grant in &grants {
                if target_contains(connection, &grant.target, &root)? {
                    sources.push(grant.id);
                }
            }
            if ordinary || !sources.is_empty() {
                let problem = if verify_physical {
                    self.verify_writable_root(connection, &root).err()
                } else {
                    None
                };
                roots.push(WritableRoot {
                    location: root,
                    ordinary,
                    grants: sources,
                    issue: problem,
                });
            }
        }
        Ok(SessionWriteScope {
            session: session.into(),
            placement: location.placement,
            session_revision: location.revision,
            catalog_revision: status.revision,
            roots,
            grants,
        })
    }

    pub(super) fn verify_writable_root(
        &self,
        connection: &Connection,
        root: &Location,
    ) -> Result<PhysicalBinding> {
        if root.retired || root.lifecycle != LocationLifecycle::Ready {
            return Err(issue(
                IssueCode::PermissionRequired,
                format!(
                    "Location {} is {:?}{} and cannot accept writes",
                    root.id,
                    root.lifecycle,
                    if root.retired { " (retired)" } else { "" }
                ),
            ));
        }
        let body: String = connection
            .query_row(
                "SELECT body FROM bindings WHERE location=?1",
                [root.id.to_string()],
                |r| r.get(0),
            )
            .map_err(corrupt)?;
        let bound: BoundLocation = decode(&body)?;
        let resolved = self
            .resolver
            .resolve_directory(&bound.binding)
            .map_err(primary_location::location_issue)?;
        if resolved.relocated || bound.binding.observed_path() != root.observed_path {
            return Err(issue(
                IssueCode::RecoveryRequired,
                format!("Location {} moved; explicitly review its binding", root.id),
            ));
        }
        Ok(bound.binding)
    }
}

pub(super) fn locations(connection: &Connection) -> Result<Vec<Location>> {
    let mut statement = connection
        .prepare("SELECT body FROM entities WHERE kind='location' ORDER BY id")
        .map_err(io)?;
    statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(io)?
        .map(|row| match decode(&row.map_err(io)?)? {
            Entity::Location(location) => Ok(location),
            _ => Err(corrupt("Location query returned another entity kind")),
        })
        .collect()
}

pub(super) fn home_project(connection: &Connection, home: Home) -> Result<ProjectId> {
    match home {
        Home::Project(id) => Ok(id),
        Home::WorkArea(id) => match entity(connection, EntityId::WorkArea(id))? {
            Entity::WorkArea(area) => Ok(area.project),
            _ => Err(corrupt("Work area identity has wrong kind")),
        },
    }
}

pub(super) fn ordinary_member(
    connection: &Connection,
    placement: Placement,
    root: &Location,
) -> Result<bool> {
    Ok(match placement {
        Placement::Checkout(id) | Placement::Directory(id) | Placement::Standalone(id) => {
            id == root.id
        }
        Placement::WorkArea(id) => root.home == Some(Home::WorkArea(id)),
        Placement::Project(id) => {
            root.home
                .map(|home| home_project(connection, home))
                .transpose()?
                == Some(id)
        }
    })
}

pub(super) fn target_contains(
    connection: &Connection,
    target: &WriteTarget,
    root: &Location,
) -> Result<bool> {
    ordinary_member(
        connection,
        match *target {
            WriteTarget::Root(id) => Placement::Standalone(id), // Identity comparison, not kind validation.
            WriteTarget::ProjectMembers(id) => Placement::Project(id),
            WriteTarget::WorkAreaMembers(id) => Placement::WorkArea(id),
        },
        root,
    )
}

pub(super) fn audience_applies(
    connection: &Connection,
    audience: &Audience,
    session: &str,
    placement: Placement,
) -> Result<bool> {
    let home = match entity(connection, query::placement_target(placement))? {
        Entity::Project(project) => Some(Home::Project(project.id)),
        Entity::WorkArea(area) => Some(Home::WorkArea(area.id)),
        Entity::Location(root) => root.home,
        _ => return Err(corrupt("Session placement is not an execution location")),
    };
    Ok(match audience {
        Audience::Session(id) => id == session,
        Audience::Project(id) => {
            home.map(|home| home_project(connection, home))
                .transpose()?
                == Some(*id)
        }
        Audience::WorkArea(id) => home == Some(Home::WorkArea(*id)),
        Audience::Checkout(id) => placement == Placement::Checkout(*id),
    })
}

pub(super) fn applicable_grants(
    connection: &Connection,
    session: &str,
    placement: Placement,
) -> Result<Vec<GrantDefinition>> {
    let installation = storage::status(connection)?.installation;
    portable::grants(connection)?
        .into_iter()
        .filter_map(|grant| {
            if grant.state != GrantState::Active
                || !grant
                    .authorization
                    .as_ref()
                    .is_some_and(|a| a.installation == installation)
            {
                return None;
            }
            match audience_applies(connection, &grant.audience, session, placement) {
                Ok(true) => Some(Ok(grant)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Result<Vec<_>>>()
}
