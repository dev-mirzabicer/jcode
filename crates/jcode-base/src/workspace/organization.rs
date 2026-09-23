use super::*;
use crate::location::volume::{LocationIssue, PathBinding, VolumeIdentity};
use crate::location::{ProjectKey, resolve_project};
use rusqlite::{TransactionBehavior, params};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PreparedChange {
    pub review: Review,
    pub entities: Vec<Entity>,
    pub binding: Option<PhysicalBinding>,
    pub default: Option<PathBinding>,
    #[serde(default)]
    pub previous_binding: Option<PhysicalBinding>,
}

impl WorkspaceService {
    pub fn review_organization_change(
        &self,
        expected: Revision,
        change: OrganizationChange,
    ) -> Result<Review> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        // External observations precede a short metadata transaction.
        let binding = match &change {
            OrganizationChange::RegisterLocation { path, .. }
            | OrganizationChange::RebindLocation { new_path: path, .. } => {
                Some(self.resolver.bind_directory(path).map_err(io)?)
            }
            OrganizationChange::AdoptStandalone { location, .. } => {
                let body: String = connection
                    .query_row(
                        "SELECT body FROM bindings WHERE location=?1",
                        [location.to_string()],
                        |r| r.get(0),
                    )
                    .map_err(corrupt)?;
                let bound: BoundLocation = decode(&body)?;
                let resolved = self
                    .resolver
                    .resolve_directory(&bound.binding)
                    .map_err(|e| issue(IssueCode::ReplacedRoot, e.to_string()))?;
                if resolved.relocated {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        "Location moved physically. Review its rebind before organizational adoption",
                    ));
                }
                Some(bound.binding)
            }
            _ => None,
        };
        let previous_binding = if let OrganizationChange::RebindLocation {
            location,
            expected_old_path,
            expected_generation,
            ..
        } = &change
        {
            let old: BoundLocation = decode(
                &connection
                    .query_row(
                        "SELECT body FROM bindings WHERE location=?1",
                        [location.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(corrupt)?,
            )?;
            let previous = old.binding;
            if previous.observed_path() != expected_old_path
                || previous.generation() != *expected_generation
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Checkout binding path or generation changed before rebind review",
                ));
            }
            let new = binding
                .as_ref()
                .ok_or_else(|| corrupt("Rebind needs a new physical target"))?;
            if previous.volume() == new.volume()
                && previous.root_witness() == new.root_witness()
                && previous.observed_path() == new.observed_path()
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Location is already bound to this physical root",
                ));
            }
            require_old_rebindable(&self.resolver, &previous, new)?;
            Some(previous)
        } else {
            None
        };
        let binding = match (binding, &previous_binding) {
            (Some(binding), Some(previous)) => Some(
                binding.with_generation(
                    previous
                        .generation()
                        .checked_add(1)
                        .ok_or_else(|| corrupt("Physical binding generation exhausted"))?,
                ),
            ),
            (other, _) => other,
        };
        let default = if let OrganizationChange::SetVolumeDefault { path, volume_uuid } = &change {
            let bound = self.resolver.bind_path(path).map_err(io)?;
            if bound.volume() != &VolumeIdentity::parse(volume_uuid).map_err(io)? {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Default path is on another volume",
                ));
            }
            Some(bound)
        } else {
            None
        };
        require_revision(&connection, expected)?;
        let prepared = prepare(
            &connection,
            expected,
            change,
            binding,
            default,
            previous_binding,
        )?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_revision(&transaction, expected)?;
        transaction
            .execute(
                "INSERT INTO reviews VALUES(?1,'organization',?2)",
                params![prepared.review.id.to_string(), encode(&prepared)?],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(prepared.review)
    }

    pub fn apply_organization_change(
        &self,
        request: RequestId,
        review: ReviewId,
    ) -> Result<Receipt> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let prepared: PreparedChange = read_review(&connection, review, "organization")?;
        let input = digest(encode(&prepared.review.change)?.as_bytes());
        if let Some(receipt) = replay(&connection, request, &input)? {
            return Ok(receipt);
        }
        let _root_lease = prepared
            .binding
            .as_ref()
            .map(|binding| self.acquire_binding(binding))
            .transpose()?;
        let _old_root_lease = match (&prepared.previous_binding, &prepared.binding) {
            (Some(previous), Some(current)) => {
                require_old_rebindable(&self.resolver, previous, current)?;
                if storage::physical_key(previous)? == storage::physical_key(current)? {
                    None
                } else if self.resolver.resolve_directory(previous).is_ok() {
                    Some(self.acquire_binding(previous)?)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(binding) = &prepared.binding {
            self.resolver
                .resolve_directory(binding)
                .map_err(|e| issue(IssueCode::ReplacedRoot, e.to_string()))?;
            for value in &prepared.entities {
                if let Entity::Location(location) = value {
                    match &location.kind {
                        LocationKind::Checkout { repository, .. } => {
                            if !matching_checkout_kind(
                                binding.observed_path(),
                                *repository,
                                &location.kind,
                            )? {
                                return Err(issue(
                                    IssueCode::ReplacedRoot,
                                    "Git layout changed after review",
                                ));
                            }
                        }
                        LocationKind::Directory => {
                            if resolve_project(binding.observed_path())
                                .map_err(io)?
                                .key()
                                .is_git()
                            {
                                return Err(issue(
                                    IssueCode::ReplacedRoot,
                                    "Directory became a Git location after review",
                                ));
                            }
                        }
                        LocationKind::Standalone { git } => {
                            if resolve_project(binding.observed_path())
                                .map_err(io)?
                                .key()
                                .is_git()
                                != *git
                            {
                                return Err(issue(
                                    IssueCode::ReplacedRoot,
                                    "Standalone physical kind changed after review",
                                ));
                            }
                        }
                    }
                }
            }
        }
        if let Some(default) = &prepared.default {
            self.resolver.resolve_path(default).map_err(io)?;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(receipt) = replay(&transaction, request, &input)? {
            return Ok(receipt);
        }
        require_revision(&transaction, prepared.review.revision)?;
        apply(&transaction, &prepared)?;
        let receipt = commit_receipt(
            &transaction,
            request,
            &input,
            prepared.review.targets.clone(),
        )?;
        if let (
            OrganizationChange::RebindLocation { location, .. },
            Some(previous),
            Some(current),
        ) = (
            &prepared.review.change,
            &prepared.previous_binding,
            &prepared.binding,
        ) {
            let history = RebindRecord {
                operation: receipt.operation,
                location: *location,
                old_path: previous.observed_path().to_path_buf(),
                new_path: current.observed_path().to_path_buf(),
                old_volume_uuid: previous.volume().as_str().into(),
                new_volume_uuid: current.volume().as_str().into(),
                old_generation: previous.generation(),
                new_generation: current.generation(),
            };
            transaction
                .execute(
                    "INSERT INTO operations VALUES(?1,'location_rebind','complete',?2)",
                    params![receipt.operation.to_string(), encode(&history)?],
                )
                .map_err(io)?;
            transaction
                .execute(
                    "INSERT INTO operation_targets VALUES(?1,?2)",
                    params![receipt.operation.to_string(), location.to_string()],
                )
                .map_err(io)?;
        }
        transaction.commit().map_err(io)?;
        self.after_mutation(receipt)
    }

    pub fn inspect_receipt(&self, request: RequestId) -> Result<Receipt> {
        let _lease = self.lease(false)?;
        let body: Option<String> = self
            .connection()?
            .query_row(
                "SELECT body FROM receipts WHERE request=?1",
                [request.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(io)?;
        decode(&body.ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown request"))?)
    }

    pub fn inspect_rebind(&self, operation: OperationId) -> Result<RebindRecord> {
        let _lease = self.lease(false)?;
        let body: Option<String> = self.connection()?.query_row(
            "SELECT body FROM operations WHERE id=?1 AND kind='location_rebind' AND state='complete'",
            [operation.to_string()], |row| row.get(0),
        ).optional().map_err(io)?;
        let result: RebindRecord = decode(
            &body.ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown rebind operation"))?,
        )?;
        if result.operation != operation {
            return Err(corrupt("Rebind history identity mismatch"));
        }
        Ok(result)
    }
}

pub(super) fn require_revision(connection: &Connection, expected: Revision) -> Result<()> {
    let current = storage::status(connection)?.revision;
    if current != expected {
        return Err(issue(
            IssueCode::Conflict,
            format!("Catalog changed: expected {expected}, current {current}"),
        ));
    }
    Ok(())
}
pub(super) fn read_review<T: DeserializeOwned>(
    connection: &Connection,
    id: ReviewId,
    kind: &str,
) -> Result<T> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM reviews WHERE id=?1 AND kind=?2",
            params![id.to_string(), kind],
            |r| r.get(0),
        )
        .optional()
        .map_err(io)?;
    decode(&body.ok_or_else(|| {
        issue(
            IssueCode::InvalidIdentity,
            "Review is absent or belongs to another operation",
        )
    })?)
}
pub(super) fn replay(
    connection: &Connection,
    request: RequestId,
    input: &str,
) -> Result<Option<Receipt>> {
    let previous: Option<(String, String)> = connection
        .query_row(
            "SELECT input_digest,body FROM receipts WHERE request=?1",
            [request.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    match previous {
        Some((known, body)) if known == input => decode(&body).map(Some),
        Some(_) => Err(issue(
            IssueCode::Conflict,
            "Request ID was already used for different input",
        )),
        None => Ok(None),
    }
}
pub(super) fn commit_receipt(
    connection: &Connection,
    request: RequestId,
    input: &str,
    targets: Vec<EntityId>,
) -> Result<Receipt> {
    connection
        .execute(
            "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
            [],
        )
        .map_err(io)?;
    let receipt = Receipt {
        operation: OperationId::new(),
        request,
        revision: storage::status(connection)?.revision,
        targets,
        issues: vec![],
    };
    connection
        .execute(
            "INSERT INTO receipts VALUES(?1,?2,?3)",
            params![request.to_string(), input, encode(&receipt)?],
        )
        .map_err(io)?;
    Ok(receipt)
}
pub(super) fn save_entity(connection: &Connection, value: &Entity) -> Result<()> {
    connection.execute("INSERT INTO entities(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![value.id().to_string(),encode(value)?]).map_err(io)?;
    Ok(())
}
fn name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(issue(
            IssueCode::InvalidInput,
            "Name must be nonempty text without control characters",
        ));
    }
    Ok(value.into())
}
pub(super) fn clean_remotes(values: &[String]) -> Result<Vec<String>> {
    values.iter().map(|value| {
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_control) { return Err(issue(IssueCode::InvalidInput,"Invalid remote reference")); }
        if value.contains("://") {
            let url = url::Url::parse(value).map_err(|_| issue(IssueCode::InvalidInput,"Invalid remote URL"))?;
            if url.password().is_some() || (!url.username().is_empty() && url.scheme() != "ssh") || url.query().is_some() || url.fragment().is_some() {
                return Err(issue(IssueCode::InvalidInput,"Credential-bearing remote references cannot enter the catalog. Use Git-managed authentication"));
            }
        } else if value.contains('@') && !value.starts_with("git@") {
            return Err(issue(IssueCode::InvalidInput,"Use a credential-free repository reference"));
        }
        Ok(value.into())
    }).collect()
}
fn usable(connection: &Connection, id: EntityId) -> Result<Entity> {
    let value = entity(connection, id)?;
    let retired = match &value {
        Entity::Project(v) => v.state == OrganizationState::Retired,
        Entity::Repository(v) => v.state == OrganizationState::Retired,
        Entity::WorkArea(v) => v.state == OrganizationState::Retired,
        Entity::Location(v) => v.retired || v.lifecycle == LocationLifecycle::Closed,
    };
    if retired {
        return Err(issue(
            IssueCode::InvalidIdentity,
            "Retired identity cannot acquire new relationships",
        ));
    }
    Ok(value)
}
pub(super) fn home_project(connection: &Connection, home: Home) -> Result<ProjectId> {
    let project = match home {
        Home::Project(id) => id,
        Home::WorkArea(id) => match usable(connection, EntityId::WorkArea(id))? {
            Entity::WorkArea(area) => area.project,
            _ => unreachable!(),
        },
    };
    usable(connection, EntityId::Project(project))?;
    Ok(project)
}
pub(super) fn association(
    connection: &Connection,
    home: Home,
    repository: RepositoryId,
    allow: bool,
) -> Result<()> {
    let project = home_project(connection, home)?;
    usable(connection, EntityId::Repository(repository))?;
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM associations WHERE project=?1 AND repository=?2)",
            params![project.to_string(), repository.to_string()],
            |r| r.get(0),
        )
        .map_err(io)?;
    if !exists && !allow {
        return Err(issue(
            IssueCode::InvalidInput,
            "Repository must be explicitly associated with the destination project",
        ));
    }
    Ok(())
}
fn location(connection: &Connection, id: LocationId) -> Result<Location> {
    match usable(connection, EntityId::Location(id))? {
        Entity::Location(value) => Ok(value),
        _ => unreachable!(),
    }
}
pub(super) fn checkout_kind(path: &Path, repository: RepositoryId) -> Result<LocationKind> {
    let facts = resolve_project(path).map_err(io)?;
    match facts.key() {
        ProjectKey::Git {
            canonical_common_dir,
        } if facts.active_root() == path => Ok(LocationKind::Checkout {
            repository,
            origin: if facts.is_linked_worktree().map_err(io)? {
                CheckoutOrigin::LinkedWorktree
            } else {
                CheckoutOrigin::AdoptedGit
            },
            common_directory: canonical_common_dir.clone(),
        }),
        _ => Err(issue(
            IssueCode::InvalidInput,
            "Checkout registration requires the actual non-bare Git root",
        )),
    }
}

fn require_old_rebindable(
    resolver: &crate::location::volume::LocationResolver,
    previous: &PhysicalBinding,
    next: &PhysicalBinding,
) -> Result<()> {
    match resolver.resolve_directory(previous) {
        Ok(observed)
            if previous.volume() != next.volume()
                || previous.root_witness() != next.root_witness() =>
        {
            Err(issue(
                IssueCode::Conflict,
                format!(
                    "Original root remains available at {}; rebind does not replace or move its files",
                    observed.path.display()
                ),
            ))
        }
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind,
                LocationIssue::OfflineVolume
                    | LocationIssue::WrongVolume
                    | LocationIssue::ReplacedRoot
            ) =>
        {
            Ok(())
        }
        Err(error)
            if error.kind == LocationIssue::Io
                && std::fs::symlink_metadata(previous.observed_path())
                    .is_err_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(())
        }
        Err(error) => Err(issue(
            IssueCode::RecoveryRequired,
            format!("Cannot establish whether the old root is still active: {error}"),
        )),
    }
}

fn matching_checkout_kind(
    path: &Path,
    repository: RepositoryId,
    expected: &LocationKind,
) -> Result<bool> {
    let current = checkout_kind(path, repository)?;
    let (
        LocationKind::Checkout {
            origin: actual,
            common_directory: common,
            ..
        },
        LocationKind::Checkout {
            origin: old,
            common_directory: expected_common,
            ..
        },
    ) = (&current, expected)
    else {
        return Ok(false);
    };
    let same_layout = match old {
        CheckoutOrigin::LinkedWorktree => *actual == CheckoutOrigin::LinkedWorktree,
        CheckoutOrigin::AdoptedGit | CheckoutOrigin::ManagedClone => {
            *actual == CheckoutOrigin::AdoptedGit
        }
    };
    Ok(same_layout && common == expected_common)
}

fn prepare(
    connection: &Connection,
    expected: Revision,
    mut change: OrganizationChange,
    binding: Option<PhysicalBinding>,
    default: Option<PathBinding>,
    previous_binding: Option<PhysicalBinding>,
) -> Result<PreparedChange> {
    let revision = expected
        .checked_add(1)
        .ok_or_else(|| corrupt("Revision exhausted"))?;
    let mut entities = vec![];
    let mut targets = vec![];
    match &mut change {
        OrganizationChange::CreateProject { name: n } => {
            *n = name(n)?;
            entities.push(Entity::Project(Project {
                id: ProjectId::new(),
                name: n.clone(),
                state: OrganizationState::Active,
                revision,
            }));
        }
        OrganizationChange::CreateRepository { name: n, remotes } => {
            *n = name(n)?;
            *remotes = clean_remotes(remotes)?;
            entities.push(Entity::Repository(Repository {
                id: RepositoryId::new(),
                name: n.clone(),
                remotes: remotes.clone(),
                state: OrganizationState::Active,
                revision,
            }));
        }
        OrganizationChange::CreateWorkArea { project, name: n } => {
            usable(connection, EntityId::Project(*project))?;
            *n = name(n)?;
            entities.push(Entity::WorkArea(WorkArea {
                id: WorkAreaId::new(),
                project: *project,
                name: n.clone(),
                state: OrganizationState::Active,
                revision,
            }));
        }
        OrganizationChange::AssociateRepository {
            project,
            repository,
        }
        | OrganizationChange::RemoveRepositoryAssociation {
            project,
            repository,
        } => {
            usable(connection, EntityId::Project(*project))?;
            usable(connection, EntityId::Repository(*repository))?;
            targets.extend([
                EntityId::Project(*project),
                EntityId::Repository(*repository),
            ]);
        }
        OrganizationChange::RegisterLocation {
            name: n,
            path,
            registration,
        } => {
            *n = name(n)?;
            let bound = binding
                .as_ref()
                .ok_or_else(|| corrupt("Missing reviewed physical binding"))?;
            *path = bound.observed_path().into();
            let key = storage::physical_key(bound)?;
            let existing: Option<String> = connection
                .query_row(
                    "SELECT location FROM bindings WHERE live_key=?1",
                    [key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(io)?;
            if let Some(existing) = existing {
                // Aliases identify the existing object without silently changing its home or kind.
                targets.push(EntityId::Location(existing.parse().map_err(corrupt)?));
            } else {
                let (home, kind) = match registration {
                    Registration::Checkout { home, repository } => {
                        association(connection, *home, *repository, false)?;
                        (Some(*home), checkout_kind(path, *repository)?)
                    }
                    Registration::Directory { home } => {
                        home_project(connection, *home)?;
                        if resolve_project(path).map_err(io)?.key().is_git() {
                            return Err(issue(
                                IssueCode::InvalidInput,
                                "Use checkout registration for a Git root",
                            ));
                        }
                        (Some(*home), LocationKind::Directory)
                    }
                    Registration::Standalone => (
                        None,
                        LocationKind::Standalone {
                            git: resolve_project(path).map_err(io)?.key().is_git(),
                        },
                    ),
                };
                entities.push(Entity::Location(Location {
                    id: LocationId::new(),
                    name: n.clone(),
                    home,
                    kind,
                    observed_path: path.clone(),
                    volume_uuid: bound.volume().as_str().into(),
                    binding_generation: bound.generation(),
                    lifecycle: LocationLifecycle::Ready,
                    retired: false,
                    revision,
                }));
            }
        }
        OrganizationChange::RebindLocation {
            location: id,
            expected_old_path,
            expected_generation,
            new_path,
        } => {
            let mut value = location(connection, *id)?;
            if matches!(
                value.lifecycle,
                LocationLifecycle::Closing | LocationLifecycle::Closed
            ) || value.retired
            {
                return Err(issue(
                    IssueCode::InvalidIdentity,
                    "Closing, closed or retired roots cannot be rebound",
                ));
            }
            let old = previous_binding
                .as_ref()
                .ok_or_else(|| corrupt("Missing prior rebind witness"))?;
            if value.observed_path != *expected_old_path
                || value.binding_generation != *expected_generation
                || value.volume_uuid != old.volume().as_str()
                || old.observed_path() != expected_old_path
                || old.generation() != *expected_generation
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Reviewed old root no longer matches catalog location",
                ));
            }
            let bound = binding
                .as_ref()
                .ok_or_else(|| corrupt("Missing new rebind witness"))?;
            let key = storage::physical_key(bound)?;
            let other: Option<String> = connection
                .query_row(
                    "SELECT location FROM bindings WHERE live_key=?1",
                    [key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(io)?;
            if other.is_some_and(|existing| existing != id.to_string()) {
                return Err(issue(
                    IssueCode::Conflict,
                    "New root is already registered under another location identity",
                ));
            }
            value.kind = match value.kind {
                LocationKind::Checkout {
                    repository, origin, ..
                } => {
                    let actual = checkout_kind(bound.observed_path(), repository)?;
                    let LocationKind::Checkout {
                        origin: found,
                        common_directory,
                        ..
                    } = actual
                    else {
                        unreachable!()
                    };
                    if (origin == CheckoutOrigin::LinkedWorktree)
                        != (found == CheckoutOrigin::LinkedWorktree)
                    {
                        return Err(issue(
                            IssueCode::InvalidInput,
                            "Rebind cannot silently convert an independent checkout to a linked worktree or vice versa",
                        ));
                    }
                    LocationKind::Checkout {
                        repository,
                        origin,
                        common_directory,
                    }
                }
                LocationKind::Directory => {
                    if resolve_project(bound.observed_path())
                        .map_err(io)?
                        .key()
                        .is_git()
                    {
                        return Err(issue(
                            IssueCode::InvalidInput,
                            "A directory reference cannot silently become a checkout",
                        ));
                    }
                    LocationKind::Directory
                }
                LocationKind::Standalone { git } => {
                    if resolve_project(bound.observed_path())
                        .map_err(io)?
                        .key()
                        .is_git()
                        != git
                    {
                        return Err(issue(
                            IssueCode::InvalidInput,
                            "Standalone rebind cannot change its physical Git kind",
                        ));
                    }
                    LocationKind::Standalone { git }
                }
            };
            *new_path = bound.observed_path().to_path_buf();
            value.observed_path = new_path.clone();
            value.volume_uuid = bound.volume().as_str().into();
            value.binding_generation = bound.generation();
            value.lifecycle = LocationLifecycle::Ready;
            value.revision = revision;
            entities.push(Entity::Location(value));
        }
        OrganizationChange::MoveLocation {
            location: id,
            home,
            associate_repository,
        } => {
            let mut value = location(connection, *id)?;
            if value.home.is_none() {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Standalone ownership requires explicit adoption",
                ));
            }
            home_project(connection, *home)?;
            if let LocationKind::Checkout { repository, .. } = value.kind {
                association(connection, *home, repository, *associate_repository)?;
            }
            value.home = Some(*home);
            value.revision = revision;
            entities.push(Entity::Location(value));
        }
        OrganizationChange::AdoptStandalone {
            location: id,
            home,
            repository,
            associate_repository,
        } => {
            let mut value = location(connection, *id)?;
            if value.home.is_some() {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Location is already project-owned",
                ));
            }
            home_project(connection, *home)?;
            value.kind = match repository {
                Some(repo) => {
                    association(connection, *home, *repo, *associate_repository)?;
                    checkout_kind(&value.observed_path, *repo)?
                }
                None => {
                    if resolve_project(&value.observed_path)
                        .map_err(io)?
                        .key()
                        .is_git()
                    {
                        return Err(issue(
                            IssueCode::InvalidInput,
                            "Git adoption requires a logical repository",
                        ));
                    }
                    LocationKind::Directory
                }
            };
            value.home = Some(*home);
            value.revision = revision;
            entities.push(Entity::Location(value));
        }
        OrganizationChange::Rename { target, name: n } => {
            *n = name(n)?;
            let mut value = usable(connection, *target)?;
            match &mut value {
                Entity::Project(v) => {
                    v.name = n.clone();
                    v.revision = revision
                }
                Entity::Repository(v) => {
                    v.name = n.clone();
                    v.revision = revision
                }
                Entity::WorkArea(v) => {
                    v.name = n.clone();
                    v.revision = revision
                }
                Entity::Location(v) => {
                    v.name = n.clone();
                    v.revision = revision
                }
            }
            entities.push(value);
        }
        OrganizationChange::Archive { target, archived } => {
            let state = if *archived {
                OrganizationState::Archived
            } else {
                OrganizationState::Active
            };
            let mut value = usable(connection, *target)?;
            match &mut value {
                Entity::Project(v) => {
                    v.state = state;
                    v.revision = revision
                }
                Entity::WorkArea(v) => {
                    v.state = state;
                    v.revision = revision
                }
                _ => {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Archive applies to projects and work areas",
                    ));
                }
            }
            entities.push(value);
        }
        OrganizationChange::Retire { target } => {
            let mut value = entity(connection, *target)?;
            match &mut value {
                Entity::Project(v) => {
                    v.state = OrganizationState::Retired;
                    v.revision = revision
                }
                Entity::Repository(v) => {
                    v.state = OrganizationState::Retired;
                    v.revision = revision
                }
                Entity::WorkArea(v) => {
                    v.state = OrganizationState::Retired;
                    v.revision = revision
                }
                Entity::Location(v) => {
                    if matches!(v.kind, LocationKind::Checkout { .. })
                        && v.lifecycle != LocationLifecycle::Closed
                    {
                        return Err(issue(
                            IssueCode::InvalidInput,
                            "Use checkout closeout rather than retiring live files",
                        ));
                    }
                    v.retired = true;
                    v.revision = revision
                }
            }
            entities.push(value);
        }
        OrganizationChange::DiscardUnused { target } => {
            entity(connection, *target)?;
            targets.push(*target);
        }
        OrganizationChange::SetVolumeDefault { volume_uuid, .. } => {
            *volume_uuid = VolumeIdentity::parse(volume_uuid.clone())
                .map_err(io)?
                .as_str()
                .into();
        }
    }
    targets.extend(entities.iter().map(Entity::id));
    Ok(PreparedChange {
        review: Review {
            id: ReviewId::new(),
            revision: expected,
            change,
            targets,
            issues: vec![],
        },
        entities,
        binding,
        default,
        previous_binding,
    })
}

fn apply(connection: &Connection, prepared: &PreparedChange) -> Result<()> {
    match &prepared.review.change {
        OrganizationChange::AssociateRepository {
            project,
            repository,
        } => {
            connection
                .execute(
                    "INSERT OR IGNORE INTO associations VALUES(?1,?2)",
                    params![project.to_string(), repository.to_string()],
                )
                .map_err(io)?;
        }
        OrganizationChange::RemoveRepositoryAssociation {
            project,
            repository,
        } => {
            let used:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM entities e LEFT JOIN entities a ON a.id=e.home_area WHERE e.repository=?1 AND (e.home_project=?2 OR a.area_project=?2))",params![repository.to_string(),project.to_string()],|r|r.get(0)).map_err(io)?;
            if used {
                return Err(issue(
                    IssueCode::Referenced,
                    "Repository association has checkout history",
                ));
            }
            connection
                .execute(
                    "DELETE FROM associations WHERE project=?1 AND repository=?2",
                    params![project.to_string(), repository.to_string()],
                )
                .map_err(io)?;
        }
        OrganizationChange::DiscardUnused { target } => {
            // The initial creation receipt is not use. Every other durable reference is.
            let used:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM receipts, json_each(receipts.body,'$.targets') t WHERE json_extract(t.value,'$.id')=?1 GROUP BY json_extract(t.value,'$.id') HAVING count(*)>1)",[target.to_string()],|r|r.get(0)).map_err(io)?;
            if used || matches!(target, EntityId::Location(_)) {
                return Err(issue(
                    IssueCode::Referenced,
                    "Identity has been used and must remain historical",
                ));
            }
            connection
                .execute("DELETE FROM entities WHERE id=?1", [target.to_string()])
                .map_err(|e| issue(IssueCode::Referenced, e.to_string()))?;
        }
        OrganizationChange::SetVolumeDefault { volume_uuid, .. } => {
            connection.execute("INSERT INTO volume_defaults VALUES(?1,?2) ON CONFLICT(volume) DO UPDATE SET body=excluded.body",params![volume_uuid,encode(prepared.default.as_ref().ok_or_else(||corrupt("Missing default binding"))?)?]).map_err(io)?;
        }
        _ => {}
    }
    for value in &prepared.entities {
        if let Entity::Location(loc) = value
            && let (Some(home), LocationKind::Checkout { repository, .. }) = (loc.home, &loc.kind)
        {
            let project = home_project(connection, home)?;
            connection
                .execute(
                    "INSERT OR IGNORE INTO associations VALUES(?1,?2)",
                    params![project.to_string(), repository.to_string()],
                )
                .map_err(io)?;
        }
        save_entity(connection, value)?;
        if let (Entity::Location(loc), Some(binding)) = (value, &prepared.binding)
            && matches!(
                prepared.review.change,
                OrganizationChange::RegisterLocation { .. }
            )
        {
            connection
                .execute(
                    "INSERT INTO bindings VALUES(?1,?2,?3)",
                    params![
                        loc.id.to_string(),
                        encode(&BoundLocation {
                            binding: binding.clone()
                        })?,
                        storage::physical_key(binding)?
                    ],
                )
                .map_err(|e| issue(IssueCode::Conflict, e.to_string()))?;
        }
        if let (Entity::Location(loc), Some(binding), Some(previous)) =
            (value, &prepared.binding, &prepared.previous_binding)
            && matches!(
                prepared.review.change,
                OrganizationChange::RebindLocation { .. }
            )
        {
            let changed = connection
                .execute(
                    "UPDATE bindings SET body=?2,live_key=?3 WHERE location=?1 AND body=?4",
                    params![
                        loc.id.to_string(),
                        encode(&BoundLocation {
                            binding: binding.clone()
                        })?,
                        storage::physical_key(binding)?,
                        encode(&BoundLocation {
                            binding: previous.clone()
                        })?
                    ],
                )
                .map_err(|e| {
                    issue(
                        IssueCode::Conflict,
                        format!("Rebind target is already owned or the old binding changed: {e}"),
                    )
                })?;
            if changed != 1 {
                return Err(issue(
                    IssueCode::Conflict,
                    "Old physical binding changed after review",
                ));
            }
        }
    }
    Ok(())
}
