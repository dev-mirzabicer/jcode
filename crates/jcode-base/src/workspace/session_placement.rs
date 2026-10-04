//! Placement review for an unplaced primary Session.
//!
//! Once managed launch is rolled out, a primary needs a placement before it
//! runs. Ordinary new sessions and older sessions both start unplaced. This
//! owner proposes placements that keep the Session's recorded cwd, so the
//! frozen instructions and Startup Context chosen for that cwd stay correct,
//! and resolves the chosen one for the existing legacy adoption control.
use super::*;
use crate::session::Session;

impl WorkspaceService {
    /// Reviewable placements for `session`. Changes nothing.
    ///
    /// A registered live root containing the recorded cwd is offered first,
    /// with its home work area and project. Otherwise one new standalone root
    /// is offered: the Git worktree root, or the directory itself. A broad
    /// root (home, filesystem or volume root) is offered without a default.
    pub fn propose_session_placement(&self, session: &Session) -> Result<PlacementProposal> {
        let working_dir = unplaced_working_dir(session)?;
        if !self.catalog_present()? {
            return Err(Issue::not_initialized());
        }
        let cwd = self
            .resolver
            .bind_directory(&working_dir)
            .map_err(primary_location::location_issue)?
            .observed_path()
            .to_path_buf();
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let catalog_revision = storage::status(&transaction)?.revision;
        let containing = scope::locations(&transaction)?
            .into_iter()
            .filter(|location| !location.lifecycle.is_historical() && !location.retired)
            .filter(|location| cwd.starts_with(&location.observed_path))
            .max_by_key(|location| location.observed_path.components().count());
        let mut candidates = Vec::new();
        let default = match containing {
            Some(root) => {
                if root.lifecycle != LocationLifecycle::Ready {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        format!(
                            "{} contains this directory but is {:?}; repair it in /workspace before placing the session",
                            root.observed_path.display(),
                            root.lifecycle
                        ),
                    ));
                }
                let broad = is_broad_root(&root.observed_path);
                let project = match root.home {
                    Some(home) => Some(project_of(&transaction, home)?),
                    None => None,
                };
                candidates.push(PlacementCandidate {
                    placement: PrimaryPlacement::Existing {
                        placement: location_placement(&root),
                    },
                    root: root.observed_path.clone(),
                    name: root.name.clone(),
                    project: project.as_ref().map(|project| project.name.clone()),
                    broad,
                });
                if let Some(Home::WorkArea(area)) = root.home {
                    let Entity::WorkArea(area) = entity(&transaction, EntityId::WorkArea(area))?
                    else {
                        return Err(corrupt("Work area identity has wrong kind"));
                    };
                    candidates.push(PlacementCandidate {
                        placement: PrimaryPlacement::Existing {
                            placement: Placement::WorkArea(area.id),
                        },
                        root: root.observed_path.clone(),
                        name: area.name,
                        project: project.as_ref().map(|project| project.name.clone()),
                        broad,
                    });
                }
                if let Some(project) = project {
                    candidates.push(PlacementCandidate {
                        placement: PrimaryPlacement::Existing {
                            placement: Placement::Project(project.id),
                        },
                        root: root.observed_path.clone(),
                        name: project.name.clone(),
                        project: Some(project.name),
                        broad,
                    });
                }
                (!broad).then_some(0)
            }
            None => {
                let root = crate::location::resolve_project(&cwd)
                    .map_err(|error| issue(IssueCode::InvalidInput, error.to_string()))?
                    .active_root()
                    .to_path_buf();
                let broad = is_broad_root(&root);
                candidates.push(PlacementCandidate {
                    placement: PrimaryPlacement::Standalone { root: root.clone() },
                    name: root_name(&root),
                    root,
                    project: None,
                    broad,
                });
                (!broad).then_some(0)
            }
        };
        Ok(PlacementProposal {
            session: session.id.clone(),
            working_dir,
            catalog_revision,
            candidates,
            default,
        })
    }

    /// Resolve a reviewed choice to the placement the legacy adoption control
    /// applies, registering a new standalone root first. Returns the catalog
    /// revision that adoption must expect. Retrying the same request reuses
    /// its registration.
    pub fn resolve_session_placement(
        &self,
        request: &SessionPlacementRequest,
        session: &Session,
    ) -> Result<(Placement, Revision)> {
        let working_dir = unplaced_working_dir(session)?;
        if working_dir != request.working_dir {
            return Err(issue(
                IssueCode::Conflict,
                "The session's working directory changed since this placement review; review it again",
            ));
        }
        match &request.placement {
            PrimaryPlacement::Existing { placement } => {
                Ok((*placement, request.expected_catalog_revision))
            }
            PrimaryPlacement::Standalone { root } => {
                let registration = derived_request(request.request, "standalone-root");
                let receipt = match self.inspect_receipt(registration) {
                    Ok(receipt) => receipt,
                    Err(error) if error.code == IssueCode::InvalidIdentity => {
                        if self.status()?.revision != request.expected_catalog_revision {
                            return Err(issue(
                                IssueCode::Conflict,
                                "Workspace changed since this placement review; review it again",
                            ));
                        }
                        let canonical = self
                            .resolver
                            .bind_directory(root)
                            .map_err(primary_location::location_issue)?
                            .observed_path()
                            .to_path_buf();
                        let cwd = self
                            .resolver
                            .bind_directory(&working_dir)
                            .map_err(primary_location::location_issue)?;
                        if !cwd.observed_path().starts_with(&canonical) {
                            return Err(issue(
                                IssueCode::PermissionRequired,
                                "The session's working directory is outside the selected standalone root",
                            ));
                        }
                        let review = self.review_organization_change(
                            request.expected_catalog_revision,
                            OrganizationChange::RegisterLocation {
                                name: root_name(&canonical),
                                path: canonical,
                                registration: Registration::Standalone,
                            },
                        )?;
                        self.apply_organization_change(registration, review.id)?
                    }
                    Err(error) => return Err(error),
                };
                let Some(EntityId::Location(location)) = receipt.targets.first() else {
                    return Err(corrupt("Standalone registration produced no location"));
                };
                Ok((Placement::Standalone(*location), receipt.revision))
            }
        }
    }
}

fn unplaced_working_dir(session: &Session) -> Result<PathBuf> {
    if session.isolated_child.is_some() {
        return Err(issue(
            IssueCode::PermissionRequired,
            "Child placement remains part of its fixed execution identity",
        ));
    }
    if session.location.is_some() {
        return Err(issue(
            IssueCode::Conflict,
            "This session is already placed; use a location change instead",
        ));
    }
    session
        .working_dir
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            issue(
                IssueCode::NeedsCwd,
                "This session has no recorded working directory; adopt it in /workspace → Sessions with an explicit cwd",
            )
        })
}

fn location_placement(location: &Location) -> Placement {
    match location.kind {
        LocationKind::Checkout { .. } => Placement::Checkout(location.id),
        LocationKind::Directory => Placement::Directory(location.id),
        LocationKind::Standalone { .. } => Placement::Standalone(location.id),
    }
}

fn project_of(connection: &Connection, home: Home) -> Result<Project> {
    match entity(
        connection,
        EntityId::Project(scope::home_project(connection, home)?),
    )? {
        Entity::Project(project) => Ok(project),
        _ => Err(corrupt("Project identity has wrong kind")),
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Standalone location")
        .to_string()
}

/// Home, a filesystem root or a mounted volume root. Placing a session there
/// grants ordinary write access to everything below, so it is never proposed
/// by default.
fn is_broad_root(root: &Path) -> bool {
    let home = dirs::home_dir().and_then(|home| home.canonicalize().ok());
    root.parent().is_none()
        || home.as_deref() == Some(root)
        || root.parent() == Some(Path::new("/Volumes"))
}

/// A stable identity for one effect of a client request, so a retry converges
/// on the original receipt.
fn derived_request(request: RequestId, label: &str) -> RequestId {
    let seed = format!("jcode-session-placement\0{request}\0{label}");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
        .to_string()
        .parse()
        .expect("UUID text parses as a request ID")
}
