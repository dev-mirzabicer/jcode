//! Filesystem effects of an explicitly reviewed launch, journaled independently
//! of Session publication. A failed launch never deletes new user files.
use super::*;
use crate::location::volume::PhysicalBinding;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyCwd {
    operation: OperationId,
    target: PathBuf,
    parent: PhysicalBinding,
    stage_name: String,
    staged: Option<PhysicalBinding>,
    published: Option<PhysicalBinding>,
}
impl WorkspaceService {
    /// Called under the request's launch lease. Replays only witnessed effects;
    /// an ambiguous directory is retained for explicit repair, never adopted by name.
    pub fn prepare_launch_filesystem(&self, request: RequestId) -> Result<Placement> {
        let record = self.inspect_primary_launch(request)?;
        if record.state == PrimaryLaunchState::RecoveryRequired {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Recover the retained Session checkpoint or review a fresh launch; restored intents do not replay filesystem effects",
            ));
        }
        let cwd = record
            .input
            .cwd
            .as_ref()
            .ok_or_else(|| issue(IssueCode::NeedsCwd, "Choose a command working directory"))?;
        #[cfg(not(target_os = "macos"))]
        if matches!(cwd, PrimaryCwd::CreateEmpty { .. }) {
            return Err(issue(
                IssueCode::UnsupportedCapability,
                "Exclusive empty-cwd publication is supported on macOS only",
            ));
        }
        if let PrimaryPlacement::Existing { placement } = record.input.placement {
            let _catalog = self.lease(false)?;
            super::primary_location::validate_primary_placement(&self.connection()?, placement)?;
        }
        match (&record.input.placement, cwd) {
            (PrimaryPlacement::Existing { placement }, PrimaryCwd::Existing { path }) => {
                self.prepare_primary_location(*placement, Some(path), record.operation)?;
                return Ok(*placement);
            }
            (PrimaryPlacement::Existing { placement }, PrimaryCwd::CreateEmpty { path, home }) => {
                let allowed = match (*placement, *home) {
                    (Placement::Project(project), Some(Home::Project(home))) => project == home,
                    (Placement::WorkArea(area), Some(Home::WorkArea(home))) => area == home,
                    (_, None) => {
                        let parent = path.parent().ok_or_else(|| {
                            issue(IssueCode::NeedsCwd, "New cwd needs an existing parent")
                        })?;
                        self.prepare_primary_location(*placement, Some(parent), record.operation)?;
                        true
                    }
                    _ => false,
                };
                if !allowed {
                    return Err(issue(
                        IssueCode::PermissionRequired,
                        "New directory association must explicitly match the selected placement",
                    ));
                }
            }
            (PrimaryPlacement::Standalone { root }, PrimaryCwd::CreateEmpty { path, home }) => {
                if home.is_some() || root != path {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "A new standalone cwd must name its own independent root and have no project home",
                    ));
                }
            }
            (PrimaryPlacement::Standalone { root }, PrimaryCwd::Existing { path }) => {
                let bound = self.resolver.bind_directory(root).map_err(io)?;
                let cwd = self.resolver.bind_directory(path).map_err(io)?;
                if !cwd.observed_path().starts_with(bound.observed_path()) {
                    return Err(issue(
                        IssueCode::PermissionRequired,
                        "Cwd is outside the selected standalone root",
                    ));
                }
            }
        }
        if let PrimaryCwd::CreateEmpty { path, .. } = cwd {
            self.create_launch_cwd(&record, path)?;
        }
        let registration = match (&record.input.placement, cwd) {
            (PrimaryPlacement::Standalone { root }, _) => {
                Some((root.clone(), Registration::Standalone))
            }
            (
                _,
                PrimaryCwd::CreateEmpty {
                    path,
                    home: Some(home),
                },
            ) => Some((path.clone(), Registration::Directory { home: *home })),
            _ => None,
        };
        let registered = if let Some((path, registration)) = registration {
            let receipt = match self.inspect_receipt(record.registration_request) {
                Ok(receipt) => receipt,
                Err(error) if error.code == IssueCode::InvalidIdentity => {
                    let review = self.review_organization_change(
                        self.status()?.revision,
                        OrganizationChange::RegisterLocation {
                            name: path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Primary working directory")
                                .into(),
                            path,
                            registration,
                        },
                    )?;
                    self.apply_organization_change(record.registration_request, review.id)?
                }
                Err(error) => return Err(error),
            };
            let Some(EntityId::Location(id)) = receipt.targets.first() else {
                return Err(corrupt("Launch registration produced no location"));
            };
            Some(*id)
        } else {
            None
        };
        let placement = match record.input.placement {
            PrimaryPlacement::Existing { placement } => placement,
            PrimaryPlacement::Standalone { .. } => Placement::Standalone(
                registered.ok_or_else(|| corrupt("Standalone launch has no location"))?,
            ),
        };
        self.prepare_primary_location(placement, Some(cwd.path()), record.operation)?;
        Ok(placement)
    }

    fn create_launch_cwd(&self, record: &PrimaryLaunchRecord, path: &Path) -> Result<()> {
        let journal = self.root.join("primary-directories");
        storage::private_dir(&journal)?;
        let receipt = journal.join(format!("{}.json", record.operation));
        let mut plan: EmptyCwd = if receipt.try_exists().map_err(io)? {
            storage::read_json(&receipt)?
        } else {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(issue(
                    IssueCode::Conflict,
                    "New cwd already exists; choose it explicitly as an existing directory",
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| issue(IssueCode::InvalidInput, "New cwd has no parent"))?;
            let bound = self.resolver.bind_directory(parent).map_err(io)?;
            let leaf = path
                .file_name()
                .ok_or_else(|| issue(IssueCode::InvalidInput, "New cwd has no directory name"))?;
            let target = bound.observed_path().join(leaf);
            let plan = EmptyCwd {
                operation: record.operation,
                target,
                parent: bound,
                stage_name: format!(".jcode-primary-{}", record.operation),
                staged: None,
                published: None,
            };
            storage::atomic_json(&receipt, &plan)?;
            plan
        };
        if plan.operation != record.operation || path.file_name() != plan.target.file_name() {
            return Err(corrupt("New-cwd journal identity mismatch"));
        }
        let parent = self.resolver.resolve_directory(&plan.parent).map_err(io)?;
        if parent.relocated
            || path
                .parent()
                .ok_or_else(|| io("Cwd has no parent"))?
                .canonicalize()
                .map_err(io)?
                != parent.path
        {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "New-cwd parent moved; review its location",
            ));
        }
        let _root = self.acquire_binding(&plan.parent)?;
        if let Some(published) = &plan.published {
            self.resolver.resolve_directory(published).map_err(io)?;
            return Ok(());
        }
        if let Some(stage) = &plan.staged {
            if let Ok(metadata) = std::fs::symlink_metadata(&plan.target)
                && (!metadata.is_dir() || metadata.file_type().is_symlink())
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Publication target is not the witnessed directory",
                ));
            }
            if let Ok(target) = self.resolver.bind_directory(&plan.target) {
                if target.observed_path() != plan.target
                    || target.volume() != stage.volume()
                    || target.root_witness() != stage.root_witness()
                {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Another directory occupies the publication target",
                    ));
                }
                plan.published = Some(target);
            } else {
                plan.published = Some(
                    self.resolver
                        .publish_empty_child(
                            &plan.parent,
                            stage,
                            plan.target
                                .file_name()
                                .ok_or_else(|| io("Cwd has no name"))?,
                        )
                        .map_err(io)?,
                );
                self.checkpoint("primary_directory_published")?;
            }
        } else {
            let stage = parent.path.join(&plan.stage_name);
            if std::fs::symlink_metadata(&stage).is_ok() {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    format!(
                        "Unwitnessed staging directory retained at {}; inspect it before choosing an existing cwd",
                        stage.display()
                    ),
                ));
            }
            plan.staged = Some(
                self.resolver
                    .create_empty_child(&plan.parent, std::ffi::OsStr::new(&plan.stage_name))
                    .map_err(io)?,
            );
            storage::atomic_json(&receipt, &plan)?;
            self.checkpoint("primary_directory_staged")?;
            // Re-enter the same journal path so publication and its recovery share
            // one identity check. Release the physical lease before reacquiring.
            drop(_root);
            return self.create_launch_cwd(record, path);
        }
        storage::atomic_json(&receipt, &plan)
    }
}
