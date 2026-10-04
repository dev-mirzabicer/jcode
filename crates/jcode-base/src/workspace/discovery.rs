//! Agent-facing discovery over current catalog relationships. Reads only; the
//! caller supplies its authoritative Session, never model-selected identity.
use super::*;
use crate::session::Session;

/// Members shown inline before `list` continues.
pub const MEMBER_PREVIEW: u32 = 8;

impl WorkspaceService {
    /// Current placement, home chain, scope counts and immediate members.
    pub fn location_context(&self, session: &Session) -> Result<LocationContext> {
        let location = placed(session)?;
        let (placement_name, project, work_area, repository, root, revision, scope) = {
            let _lease = self.lease(false)?;
            let connection = self.connection()?;
            let transaction = connection.unchecked_transaction().map_err(io)?;
            let revision = storage::status(&transaction)?.revision;
            let named = |id: EntityId| -> Result<NamedEntity> {
                let name = match entity(&transaction, id)? {
                    Entity::Project(value) => value.name,
                    Entity::Repository(value) => value.name,
                    Entity::WorkArea(value) => value.name,
                    Entity::Location(value) => value.name,
                };
                Ok(NamedEntity { id, name })
            };
            let mut project = None;
            let mut work_area = None;
            let mut repository = None;
            let mut root = None;
            let target = query::placement_target(location.placement);
            let placement_name = named(target)?.name;
            match entity(&transaction, target)? {
                Entity::Project(value) => project = Some(named(EntityId::Project(value.id))?),
                Entity::WorkArea(value) => {
                    project = Some(named(EntityId::Project(value.project))?);
                    work_area = Some(named(EntityId::WorkArea(value.id))?);
                }
                Entity::Location(value) => {
                    match value.home {
                        Some(Home::Project(id)) => project = Some(named(EntityId::Project(id))?),
                        Some(Home::WorkArea(id)) => {
                            work_area = Some(named(EntityId::WorkArea(id))?);
                            project = Some(named(EntityId::Project(scope::home_project(
                                &transaction,
                                Home::WorkArea(id),
                            )?))?);
                        }
                        None => {}
                    }
                    if let LocationKind::Checkout { repository: id, .. } = &value.kind {
                        repository = Some(named(EntityId::Repository(*id))?);
                    }
                    root = Some(value);
                }
                Entity::Repository(_) => {
                    return Err(corrupt("Session placement names a repository"));
                }
            }
            let scope = self.scope_snapshot_mode(&transaction, &session.id, location, false)?;
            (
                placement_name,
                project,
                work_area,
                repository,
                root,
                revision,
                scope,
            )
        };
        let ordinary = scope.roots.iter().filter(|root| root.ordinary).count() as u64;
        let summary = ScopeSummary {
            ordinary_roots: ordinary,
            granted_roots: scope.roots.len() as u64 - ordinary,
            inactive_roots: scope
                .roots
                .iter()
                .filter(|root| {
                    root.location.retired || root.location.lifecycle != LocationLifecycle::Ready
                })
                .count() as u64,
            grants: scope.grants.len() as u64,
        };
        let members = match location.placement {
            Placement::Project(id) => Some(self.member_summary(
                Query {
                    project: Some(id),
                    ..Default::default()
                },
                true,
            )?),
            Placement::WorkArea(id) => Some(self.member_summary(
                Query {
                    home: Some(Home::WorkArea(id)),
                    ..Default::default()
                },
                false,
            )?),
            _ => None,
        };
        let pending_proposals = self
            .list_permissions(
                PermissionQuery::Proposals {
                    session: Some(session.id.clone()),
                    state: Some(AccessProposalState::Pending),
                },
                None,
                1,
            )?
            .total;
        Ok(LocationContext {
            session: session.id.clone(),
            placement: location.placement,
            placement_name,
            project,
            work_area,
            repository,
            location: root,
            cwd: location.cwd.observed_path().to_path_buf(),
            initial_cwd: location.initial_cwd.clone(),
            location_revision: location.revision,
            catalog_revision: revision,
            scope: summary,
            members,
            pending_proposals,
        })
    }

    fn member_summary(&self, base: Query, organization: bool) -> Result<MemberSummary> {
        let count = |kind: EntityKind| -> Result<u64> {
            Ok(self
                .list(
                    Query {
                        kind: Some(kind),
                        ..base.clone()
                    },
                    None,
                    1,
                )?
                .total)
        };
        let first = self.list(base.clone(), None, MEMBER_PREVIEW)?;
        Ok(MemberSummary {
            repositories: if organization {
                count(EntityKind::Repository)?
            } else {
                0
            },
            work_areas: if organization {
                count(EntityKind::WorkArea)?
            } else {
                0
            },
            locations: count(EntityKind::Location)?,
            more: first.next.is_some(),
            first: first.items,
        })
    }

    /// One page of writable roots. Only returned roots are physically checked,
    /// so a large membership never verifies every root to answer one page.
    pub fn session_scope_page(
        &self,
        session: &Session,
        after: Option<Cursor>,
        limit: u32,
    ) -> Result<ScopePage> {
        if !(1..=200).contains(&limit) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Page size must be 1 through 200. Continue for additional roots",
            ));
        }
        let location = placed(session)?;
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let mut scope = self.scope_snapshot_mode(&transaction, &session.id, location, false)?;
        let query_digest = digest(encode(&("scope", &session.id, location.revision))?.as_bytes());
        if let Some(cursor) = &after
            && (cursor.revision != scope.catalog_revision || cursor.query_digest != query_digest)
        {
            return Err(issue(
                IssueCode::Conflict,
                "Scope changed or continuation belongs to another Session; refresh from the first page",
            ));
        }
        scope.roots.sort_by_key(|root| root.location.id.to_string());
        let total = scope.roots.len() as u64;
        let start = after.as_ref().map(|cursor| cursor.after.clone());
        let mut roots: Vec<WritableRoot> = scope
            .roots
            .into_iter()
            .filter(|root| {
                start
                    .as_ref()
                    .is_none_or(|after| root.location.id.to_string() > *after)
            })
            .take(limit as usize + 1)
            .collect();
        let next = if roots.len() > limit as usize {
            roots.pop();
            roots.last().map(|root| Cursor {
                revision: scope.catalog_revision,
                after: root.location.id.to_string(),
                query_digest: query_digest.clone(),
            })
        } else {
            None
        };
        for root in &mut roots {
            root.issue = self
                .verify_writable_root(&transaction, &root.location)
                .err();
        }
        let grants = scope
            .grants
            .into_iter()
            .filter(|grant| roots.iter().any(|root| root.grants.contains(&grant.id)))
            .collect();
        Ok(ScopePage {
            session: session.id.clone(),
            placement: location.placement,
            session_revision: location.revision,
            catalog_revision: scope.catalog_revision,
            total,
            roots,
            grants,
            next,
        })
    }

    /// The deepest registered root containing `path`, and whether this Session
    /// may write there. Relative paths resolve against the Session cwd.
    pub fn locate_path(&self, session: &Session, path: &Path) -> Result<LocatedPath> {
        let location = placed(session)?;
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            location.cwd.observed_path().join(path)
        };
        let resolved =
            crate::location::native_files::resolve_target(&absolute).map_err(|error| {
                issue(
                    IssueCode::InvalidInput,
                    format!("Cannot resolve {}: {error}", absolute.display()),
                )
            })?;
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let owner = scope::locations(&transaction)?
            .into_iter()
            .filter(|root| resolved.starts_with(&root.observed_path))
            .max_by_key(|root| {
                (
                    root.observed_path.components().count(),
                    !root.lifecycle.is_historical() && !root.retired,
                )
            });
        let scope = self.scope_snapshot_mode(&transaction, &session.id, location, false)?;
        let allowed = owner
            .as_ref()
            .and_then(|owner| scope.roots.iter().find(|root| root.location.id == owner.id));
        let writable = match (&owner, allowed) {
            (Some(owner), Some(_)) => self.verify_writable_root(&transaction, owner).is_ok(),
            _ => false,
        };
        Ok(LocatedPath {
            path: resolved,
            writable,
            ordinary: allowed.is_some_and(|root| root.ordinary),
            grants: allowed.map(|root| root.grants.clone()).unwrap_or_default(),
            location: owner,
        })
    }
}

fn placed(session: &Session) -> Result<&crate::session::StoredSessionLocation> {
    if session.isolated_child.is_some() {
        return Err(issue(
            IssueCode::PermissionRequired,
            "Workspace discovery belongs to the original primary parent",
        ));
    }
    session.location.as_ref().ok_or_else(|| {
        issue(
            IssueCode::RecoveryRequired,
            "This Session has no workspace placement; a legacy Session needs reviewed adoption first",
        )
    })
}

/// Compact location facts for the initial Session Context block. Placed
/// sessions see these once, after stable Startup Context; later changes arrive
/// as location and write-access notices, and discovery reads current state.
pub fn location_context_text(context: &LocationContext) -> String {
    let mut lines = vec![format!(
        "Workspace placement: {} {:?} ({})",
        placement_kind(context.placement),
        context.placement_name,
        query::placement_target(context.placement)
    )];
    let placement_is = |id: EntityId| query::placement_target(context.placement) == id;
    if let Some(project) = context
        .project
        .as_ref()
        .filter(|value| !placement_is(value.id))
    {
        lines.push(format!("Project: {:?} ({})", project.name, project.id));
    }
    if let Some(area) = context
        .work_area
        .as_ref()
        .filter(|value| !placement_is(value.id))
    {
        lines.push(format!("Work area: {:?} ({})", area.name, area.id));
    }
    if let Some(repository) = &context.repository {
        lines.push(format!(
            "Repository: {:?} ({})",
            repository.name, repository.id
        ));
    }
    if let Some(root) = &context.location {
        lines.push(format!(
            "Placement root: {} ({})",
            root.observed_path.display(),
            lifecycle(root.lifecycle)
        ));
    }
    let scope = &context.scope;
    let mut writable = format!(
        "Writable roots: {} ordinary, {} granted",
        scope.ordinary_roots, scope.granted_roots
    );
    if scope.inactive_roots > 0 {
        writable.push_str(&format!(", {} not ready for writes", scope.inactive_roots));
    }
    writable.push_str(&format!(" (catalog revision {})", context.catalog_revision));
    lines.push(writable);
    if let Some(members) = &context.members {
        let mut counts = Vec::new();
        if matches!(context.placement, Placement::Project(_)) {
            counts.push(plural(members.repositories, "repository", "repositories"));
            counts.push(plural(members.work_areas, "work area", "work areas"));
        }
        counts.push(plural(members.locations, "location", "locations"));
        let mut header = format!("Members: {}", counts.join(", "));
        if members.more {
            header.push_str(&format!("; first {}:", members.first.len()));
        } else if !members.first.is_empty() {
            header.push(':');
        }
        lines.push(header);
        lines.extend(
            members
                .first
                .iter()
                .map(|entity| format!("- {}", entity_line(entity))),
        );
    }
    if context.pending_proposals > 0 {
        lines.push(format!(
            "Pending access proposals: {}",
            context.pending_proposals
        ));
    }
    lines.push(
        if context.members.as_ref().is_some_and(|members| members.more) {
            "Use the workspace tool to see paths, status, permission sources and the rest of the list."
        } else {
            "Use the workspace tool to see paths, status and permission sources."
        }
        .into(),
    );
    lines.join("\n")
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn placement_kind(placement: Placement) -> &'static str {
    match placement {
        Placement::Project(_) => "project",
        Placement::WorkArea(_) => "work area",
        Placement::Checkout(_) => "checkout",
        Placement::Directory(_) => "directory",
        Placement::Standalone(_) => "standalone location",
    }
}

fn lifecycle(value: LocationLifecycle) -> &'static str {
    match value {
        LocationLifecycle::Provisioning => "provisioning",
        LocationLifecycle::Ready => "ready",
        LocationLifecycle::PreparationFailed => "preparation failed",
        LocationLifecycle::Unavailable => "unavailable",
        LocationLifecycle::Closing => "closing",
        LocationLifecycle::Closed => "closed",
        LocationLifecycle::Unregistered => "unregistered",
    }
}

fn entity_line(entity: &Entity) -> String {
    match entity {
        Entity::Project(value) => format!("project {:?} ({})", value.name, value.id),
        Entity::Repository(value) => format!("repository {:?} ({})", value.name, value.id),
        Entity::WorkArea(value) => format!("work area {:?} ({})", value.name, value.id),
        Entity::Location(value) => format!(
            "{} {:?} ({}) {}, {}",
            match value.kind {
                LocationKind::Checkout { .. } => "checkout",
                LocationKind::Directory => "directory",
                LocationKind::Standalone { .. } => "standalone location",
            },
            value.name,
            value.id,
            value.observed_path.display(),
            lifecycle(value.lifecycle)
        ),
    }
}
