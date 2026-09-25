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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_failure: Option<HistoricalFailure>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct HistoricalFailure {
    #[serde(with = "jcode_workspace_types::filesystem_path::optional")]
    pub path: Option<PathBuf>,
    pub expected_digest: Option<String>,
    pub observed_digest: Option<String>,
    pub witness: Option<inventory::Witness>,
    pub issue: Issue,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SessionReference {
    pub session: String,
    pub placement: Option<Placement>,
    pub cwd: Option<String>,
    #[serde(default, with = "jcode_workspace_types::filesystem_path::optional")]
    pub initial_cwd: Option<PathBuf>,
    pub original_parent: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Link {
    pub owner: String,
    pub kind: String,
    #[serde(with = "jcode_workspace_types::filesystem_path")]
    pub path: PathBuf,
}

impl WorkspaceService {
    pub(super) fn closeout_references(
        &self,
        stored: &StoredCloseout,
        session_root: &Path,
    ) -> Result<References> {
        let mut roots = vec![stored.binding.observed_path()];
        if let Some(removal) = &stored.removal {
            roots.extend(removal.control_paths());
        }
        for control in [session_root, self.root.as_path()] {
            if contained_in(control, &roots)? {
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
            historical_failure: None,
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
                && !other.lifecycle.is_historical()
                && roots
                    .iter()
                    .any(|root| other.observed_path.starts_with(root))
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
                    related |= contained_in(Path::new(path), &roots)?;
                }
                if let Some(location_state) = &session.location {
                    related |= contained_in(&location_state.initial_cwd, &roots)?;
                }
                for page in
                    crate::side_panel::references_for_session_in(session_root, id).map_err(io)?
                {
                    let path = PathBuf::from(page.file_path);
                    if contained_in(&path, &roots)? {
                        related = true;
                        result.links.push(Link {
                            owner: format!("{id}/{}", page.id),
                            kind: "side_panel".into(),
                            path,
                        });
                    }
                }
                if let Some(child) = &session.isolated_child
                    && contained_in(&child.identity.artifact_dir, &roots)?
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
        let original_absent = matches!(std::fs::symlink_metadata(&location.observed_path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        if stored.removal.is_some() && original_absent {
            let reviewed = stored
                .removal
                .as_ref()
                .and_then(|removal| removal.reference_snapshot());
            match historical_references(reviewed.or(stored.references.as_ref())) {
                Ok(historical) => {
                    if reviewed.is_some() {
                        result.issues.extend(historical.issues);
                    } else {
                        result.issues.push(issue(IssueCode::IncompleteCapture,
                            "This removed source predates review-bound reference capture; retain files or restore the reviewed source before resuming removal"));
                    }
                    result.links.extend(
                        historical
                            .links
                            .into_iter()
                            .filter(|link| link.kind == "instruction_location")
                            .map(|mut link| {
                                link.kind = "recorded_instruction_location".into();
                                link
                            }),
                    );
                }
                Err(failure) => {
                    result.issues.push(failure.issue.clone());
                    result.historical_failure = Some(*failure);
                }
            }
        } else {
            match instructions.preservation_references(&location.observed_path) {
                Ok(paths) => {
                    for path in paths {
                        if contained_in(&path, &roots)? {
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
        }
        result.sessions.sort_by(|a, b| a.session.cmp(&b.session));
        result
            .links
            .sort_by(|a, b| (&a.kind, &a.owner, &a.path).cmp(&(&b.kind, &b.owner, &b.path)));
        Ok(result)
    }
}

/// Only unavailable historical evidence is downgraded into an observation.
/// Current catalog, Session, side-panel, physical and live-owner errors above
/// still propagate. Hash and witness bind a retain-files decision to the exact
/// observed failure, not just an error message that different bytes could share.
fn historical_references(
    reference: Option<&(PathBuf, String)>,
) -> std::result::Result<References, Box<HistoricalFailure>> {
    let mut failure = HistoricalFailure {
        path: reference.map(|(path, _)| path.clone()),
        expected_digest: reference.map(|(_, hash)| hash.clone()),
        observed_digest: None,
        witness: None,
        issue: issue(
            IssueCode::IncompleteCapture,
            "Removed source has no retained reference inventory",
        ),
    };
    let result = (|| -> Result<References> {
        let (path, expected) = reference.ok_or_else(|| failure.issue.clone())?;
        let metadata = std::fs::symlink_metadata(path).map_err(io)?;
        failure.witness = Some(inventory::Witness::of(&metadata)?);
        if !metadata.is_file() {
            return Err(corrupt(
                "Historical reference evidence is not a regular file",
            ));
        }
        let observed = backup::file_digest(path)?;
        failure.observed_digest = Some(observed.clone());
        if observed != *expected {
            return Err(corrupt("Retained reference inventory changed"));
        }
        let references = storage::read_json(path)?;
        if Some(inventory::Witness::of(
            &std::fs::symlink_metadata(path).map_err(io)?,
        )?) != failure.witness
        {
            return Err(issue(
                IssueCode::Conflict,
                "Historical reference evidence changed during observation",
            ));
        }
        Ok(references)
    })();
    result.map_err(|issue| {
        failure.issue = issue;
        Box::new(failure)
    })
}

fn contained_in(path: &Path, roots: &[&Path]) -> Result<bool> {
    if !path.is_absolute() {
        return Err(issue(
            IssueCode::IncompleteCapture,
            "A stored reference has no absolute location; repair it explicitly before closeout",
        ));
    }
    let target = crate::location::native_files::resolve_target(path).map_err(io)?;
    let entry = crate::location::native_files::resolve_removal_entry(path).map_err(io)?;
    Ok(roots
        .iter()
        .any(|root| target.starts_with(root) || entry.starts_with(root)))
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
