//! Snapshot references, never conversation bodies or a second Session authority.
use super::*;
use crate::session::Session;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct References {
    pub location: Location,
    pub organization: Vec<Entity>,
    pub grants: Vec<GrantDefinition>,
    pub sessions: Vec<SessionReference>,
    pub links: Vec<Link>,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SessionReference {
    pub session: String,
    pub placement: Option<Placement>,
    pub cwd: Option<String>,
    pub initial_cwd: Option<PathBuf>,
    pub original_parent: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Link {
    pub owner: String,
    pub kind: String,
    pub path: PathBuf,
}

impl WorkspaceService {
    pub(super) fn closeout_references(
        &self,
        stored: &StoredCloseout,
        session_root: &Path,
    ) -> Result<References> {
        for control in [session_root, self.root.as_path()] {
            if contained(control, stored.binding.observed_path())? {
                return Err(issue(
                    IssueCode::Referenced,
                    "Checkout contains durable harness state; relocate that state explicitly before closeout",
                ));
            }
        }
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let Entity::Location(location) = entity(
            &transaction,
            EntityId::Location(stored.record.spec.location),
        )?
        else {
            return Err(corrupt("Closeout lost location metadata"));
        };
        let mut result = References {
            location: location.clone(),
            organization: Vec::new(),
            grants: Vec::new(),
            sessions: Vec::new(),
            links: Vec::new(),
            issues: Vec::new(),
        };
        let mut project = None;
        if let Some(home) = location.home {
            let id = scope::home_project(&transaction, home)?;
            project = Some(id);
            result
                .organization
                .push(entity(&transaction, EntityId::Project(id))?);
            if let Home::WorkArea(id) = home {
                result
                    .organization
                    .push(entity(&transaction, EntityId::WorkArea(id))?);
            }
        }
        if let LocationKind::Checkout { repository, .. } = location.kind {
            result
                .organization
                .push(entity(&transaction, EntityId::Repository(repository))?);
        }
        {
            let mut query = transaction
                .prepare("SELECT body FROM grants ORDER BY id")
                .map_err(io)?;
            for body in query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(io)?
            {
                let grant: GrantDefinition = decode(&body.map_err(io)?)?;
                if scope::target_contains(&transaction, &grant.target, &location)?
                    || grant.audience == Audience::Checkout(location.id)
                {
                    result.grants.push(grant);
                }
            }
        }
        for other in scope::locations(&transaction)? {
            if other.id != location.id
                && other.lifecycle != LocationLifecycle::Closed
                && other.observed_path.starts_with(&location.observed_path)
            {
                result.issues.push(issue(IssueCode::Referenced, format!("Registered location {} is inside this checkout; resolve its independent lifecycle before removal", other.id)));
                result.links.push(Link {
                    owner: other.id.to_string(),
                    kind: "registered_location".into(),
                    path: other.observed_path,
                });
            }
        }
        drop(transaction);
        let sessions = session_root.join("sessions");
        if sessions.try_exists().map_err(io)? {
            for entry in std::fs::read_dir(sessions).map_err(io)? {
                let path = entry.map_err(io)?.path();
                if path.extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                let id = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| corrupt("Session filename cannot be inspected"))?;
                let session = match Session::load_startup_stub_in(session_root, id) {
                    Ok(session) => session,
                    Err(error) => {
                        result.issues.push(issue(
                            IssueCode::IncompleteCapture,
                            format!("Session {id} references cannot be inspected: {error}"),
                        ));
                        continue;
                    }
                };
                let placement = session.location.as_ref().map(|v| v.placement);
                let mut related = match placement {
                    Some(
                        Placement::Checkout(id)
                        | Placement::Directory(id)
                        | Placement::Standalone(id),
                    ) => id == location.id,
                    Some(Placement::Project(id)) => Some(id) == project,
                    Some(Placement::WorkArea(id)) => location.home == Some(Home::WorkArea(id)),
                    None => false,
                };
                if let Some(path) = &session.working_dir {
                    related |= contained(Path::new(path), &location.observed_path)?;
                }
                if let Some(location_state) = &session.location {
                    related |= contained(&location_state.initial_cwd, &location.observed_path)?;
                }
                for page in
                    crate::side_panel::references_for_session_in(session_root, id).map_err(io)?
                {
                    let path = PathBuf::from(page.file_path);
                    if contained(&path, &location.observed_path)? {
                        related = true;
                        result.links.push(Link {
                            owner: format!("{id}/{}", page.id),
                            kind: "side_panel".into(),
                            path,
                        });
                    }
                }
                if let Some(child) = &session.isolated_child
                    && contained(&child.identity.artifact_dir, &location.observed_path)?
                {
                    related = true;
                    result.links.push(Link {
                        owner: id.into(),
                        kind: "child_artifacts".into(),
                        path: child.identity.artifact_dir.clone(),
                    });
                }
                if related {
                    result.sessions.push(SessionReference {
                        session: session.id,
                        placement,
                        cwd: session.working_dir,
                        initial_cwd: session.location.map(|v| v.initial_cwd),
                        original_parent: session.isolated_child.map(|v| v.identity.original_parent),
                    });
                }
            }
        }
        let instructions = crate::instruction::InstructionRepositoryService::from_paths(
            session_root,
            self.root
                .parent()
                .ok_or_else(|| corrupt("Catalog has no parent"))?,
        );
        match instructions.preservation_references(&location.observed_path) {
            Ok(paths) => {
                for path in paths {
                    if contained(&path, &location.observed_path)? {
                        result.links.push(Link {
                            owner: location.id.to_string(),
                            kind: "instruction_location".into(),
                            path,
                        });
                    }
                }
            }
            Err(error) => result.issues.push(issue(
                IssueCode::IncompleteCapture,
                format!("Instruction binding cannot be inspected: {error}"),
            )),
        }
        result.sessions.sort_by(|a, b| a.session.cmp(&b.session));
        result
            .links
            .sort_by(|a, b| (&a.kind, &a.owner, &a.path).cmp(&(&b.kind, &b.owner, &b.path)));
        Ok(result)
    }
}

fn contained(path: &Path, root: &Path) -> Result<bool> {
    if !path.is_absolute() {
        return Err(issue(
            IssueCode::IncompleteCapture,
            "A stored reference has no absolute location; repair it explicitly before closeout",
        ));
    }
    Ok(crate::location::native_files::resolve_target(path)
        .map_err(io)?
        .starts_with(root)
        || crate::location::native_files::resolve_removal_entry(path)
            .map_err(io)?
            .starts_with(root))
}

pub(super) fn entry(references: &References) -> Result<CloseoutEntry> {
    Ok(CloseoutEntry {
        id: String::new(),
        path: PathBuf::new(),
        kind: CloseoutEntryKind::Reference,
        bytes: 0,
        sha256: Some(digest(encode(references)?.as_bytes())),
        link_target: None,
        links: 0,
        mode: 0,
        facts: vec![format!(
            "{} Session references; {} artifact/store links; {} grant records",
            references.sessions.len(),
            references.links.len(),
            references.grants.len()
        )],
        blockers: references.issues.clone(),
    })
}
