use super::*;
use rusqlite::{TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableCatalog {
    pub schema: u32,
    pub installation: InstallationId,
    pub revision: Revision,
    pub entities: Vec<Entity>,
    pub associations: Vec<(ProjectId, RepositoryId)>,
    pub bindings: Vec<(LocationId, BoundLocation)>,
    pub grants: Vec<GrantDefinition>,
    pub session_references: Vec<SessionIndex>,
    pub inherited_session_references: Vec<SessionReferenceSet>,
    pub closed: Vec<ClosedReference>,
    pub external_content: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionReferenceSet {
    projects: Vec<ProjectId>,
    installation: InstallationId,
    sessions: Vec<SessionIndex>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grant_definitions: Vec<GrantDefinition>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedReference {
    pub location: LocationId,
    pub operation: OperationId,
    pub preservation_paths: Vec<PathBuf>,
    pub report: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportEnvelope {
    name: String,
    project: ProjectId,
    sha256: String,
    catalog: PortableCatalog,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedImport {
    review: ImportReview,
    payload: PortableCatalog,
    input_digest: String,
}

pub(super) fn all_entities(connection: &Connection) -> Result<Vec<Entity>> {
    let mut stmt = connection
        .prepare("SELECT body FROM entities ORDER BY id")
        .map_err(io)?;
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map_err(io)?
        .map(|v| decode(&v.map_err(io)?))
        .collect()
}
pub(super) fn validate_graph(connection: &Connection) -> Result<()> {
    backup::integrity(connection)?;
    let entities = all_entities(connection)?;
    for value in &entities {
        match value {
            Entity::Project(_) => {}
            Entity::Repository(repo) => {
                organization::clean_remotes(&repo.remotes)?;
            }
            Entity::WorkArea(area) => {
                entity(connection, EntityId::Project(area.project))?;
            }
            Entity::Location(loc) => {
                match (&loc.kind, loc.home) {
                    (LocationKind::Standalone { .. }, None) => {}
                    (LocationKind::Checkout { repository, .. }, Some(home)) => {
                        entity(connection, EntityId::Repository(*repository))?;
                        let project = historical_home_project(connection, home)?;
                        let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM associations WHERE project=?1 AND repository=?2)",params![project.to_string(),repository.to_string()],|r|r.get(0)).map_err(io)?;
                        if !exists {
                            return Err(corrupt(
                                "Checkout repository is not associated with its home project",
                            ));
                        }
                    }
                    (LocationKind::Directory, Some(home)) => {
                        historical_home_project(connection, home)?;
                    }
                    _ => return Err(corrupt("Location kind and single-home ownership disagree")),
                }
                let body: String = connection
                    .query_row(
                        "SELECT body FROM bindings WHERE location=?1",
                        [loc.id.to_string()],
                        |r| r.get(0),
                    )
                    .map_err(corrupt)?;
                let bound: BoundLocation = decode(&body)?;
                let live_key: Option<String> = connection
                    .query_row(
                        "SELECT live_key FROM bindings WHERE location=?1",
                        [loc.id.to_string()],
                        |r| r.get(0),
                    )
                    .map_err(corrupt)?;
                let expected_key = if loc.lifecycle == LocationLifecycle::Closed {
                    None
                } else {
                    Some(storage::physical_key(&bound.binding)?)
                };
                if live_key != expected_key {
                    return Err(corrupt(
                        "Physical-root uniqueness index disagrees with binding",
                    ));
                }
                if bound.binding.observed_path() != loc.observed_path
                    || bound.binding.volume().as_str() != loc.volume_uuid
                    || bound.binding.generation() != loc.binding_generation
                {
                    return Err(corrupt(
                        "Location projection disagrees with its physical binding",
                    ));
                }
            }
        }
    }
    let mut stmt = connection
        .prepare("SELECT project,repository FROM associations")
        .map_err(io)?;
    for pair in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(io)?
    {
        let (p, r) = pair.map_err(io)?;
        entity(connection, EntityId::Project(p.parse().map_err(corrupt)?))?;
        entity(
            connection,
            EntityId::Repository(r.parse().map_err(corrupt)?),
        )?;
    }
    for grant in grants(connection)? {
        validate_grant(connection, &grant)?;
    }
    let mut references = connection
        .prepare("SELECT body FROM imported_references")
        .map_err(io)?;
    for body in references
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(io)?
    {
        let record: SessionReferenceSet = decode(&body.map_err(io)?)?;
        for project in record.projects {
            entity(connection, EntityId::Project(project))?;
        }
        if record
            .grant_definitions
            .iter()
            .any(|g| g.state != GrantState::Disabled)
        {
            return Err(corrupt("Imported external grant reference is not disabled"));
        }
    }
    let mut stmt = connection
        .prepare("SELECT body,target FROM session_index")
        .map_err(io)?;
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(io)?
    {
        let (body, target) = row.map_err(io)?;
        let index: SessionIndex = decode(&body)?;
        query::validate_placement(connection, index.placement)?;
        if query::placement_target(index.placement).to_string() != target {
            return Err(corrupt(
                "Session index target disagrees with its checkpoint projection",
            ));
        }
    }
    Ok(())
}
pub(super) fn historical_home_project(connection: &Connection, home: Home) -> Result<ProjectId> {
    let project = match home {
        Home::Project(id) => id,
        Home::WorkArea(id) => match entity(connection, EntityId::WorkArea(id))? {
            Entity::WorkArea(a) => a.project,
            _ => unreachable!(),
        },
    };
    entity(connection, EntityId::Project(project))?;
    Ok(project)
}
pub(super) fn grant_targets(grant: &GrantDefinition) -> Vec<EntityId> {
    let mut targets = vec![];
    match grant.audience {
        Audience::Project(id) => targets.push(EntityId::Project(id)),
        Audience::WorkArea(id) => targets.push(EntityId::WorkArea(id)),
        Audience::Checkout(id) => targets.push(EntityId::Location(id)),
        Audience::Session(_) => {}
    }
    targets.push(match grant.target {
        WriteTarget::Root(id) => EntityId::Location(id),
        WriteTarget::ProjectMembers(id) => EntityId::Project(id),
        WriteTarget::WorkAreaMembers(id) => EntityId::WorkArea(id),
    });
    targets
}
pub(super) fn validate_grant(connection: &Connection, grant: &GrantDefinition) -> Result<()> {
    for id in grant_targets(grant) {
        entity(connection, id)?;
    }
    if let Audience::Checkout(id) = grant.audience
        && !matches!(
            entity(connection, EntityId::Location(id))?,
            Entity::Location(Location {
                kind: LocationKind::Checkout { .. },
                ..
            })
        )
    {
        return Err(corrupt("Checkout grant audience is not a checkout"));
    }
    Ok(())
}
pub(super) fn grants(connection: &Connection) -> Result<Vec<GrantDefinition>> {
    let mut stmt = connection
        .prepare("SELECT body FROM grants ORDER BY id")
        .map_err(io)?;
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map_err(io)?
        .map(|v| decode(&v.map_err(io)?))
        .collect()
}
pub(super) fn save_grant(connection: &Connection, grant: &GrantDefinition) -> Result<()> {
    validate_grant(connection, grant)?;
    connection
        .execute(
            "INSERT INTO grants VALUES(?1,?2)",
            params![grant.id.to_string(), encode(grant)?],
        )
        .map_err(io)?;
    for target in grant_targets(grant) {
        connection
            .execute(
                "INSERT OR IGNORE INTO grant_references VALUES(?1,?2)",
                params![grant.id.to_string(), target.to_string()],
            )
            .map_err(io)?;
    }
    Ok(())
}

impl WorkspaceService {
    pub fn export_project(
        &self,
        request: RequestId,
        project: ProjectId,
        name: String,
    ) -> Result<PathBuf> {
        let _lease = self.lease(false)?;
        let lock = storage::private_file(&self.root.join("exports.lock"), false)?;
        lock.lock().map_err(io)?;
        let path = self.root.join("exports").join(format!("{request}.json"));
        if path.try_exists().map_err(io)? {
            let old: ExportEnvelope = storage::read_json(&path)?;
            if old.project != project || old.name != name {
                return Err(issue(
                    IssueCode::Conflict,
                    "Export request reused with different input",
                ));
            }
            verify_envelope(&old)?;
            return Ok(path);
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction().map_err(io)?;
        entity(&tx, EntityId::Project(project))?;
        let status = storage::status(&tx)?;
        let all = all_entities(&tx)?;
        let mut entities = vec![];
        for value in all {
            let included=match &value {
                Entity::Project(v)=>v.id==project,
                Entity::WorkArea(v)=>v.project==project,
                Entity::Location(v)=>v.home.map(|h|historical_home_project(&tx,h)).transpose()?==Some(project),
                Entity::Repository(v)=>tx.query_row("SELECT EXISTS(SELECT 1 FROM associations WHERE project=?1 AND repository=?2)",params![project.to_string(),v.id.to_string()],|r|r.get(0)).map_err(io)?,
            };
            if included {
                entities.push(value);
            }
        }
        let ids: BTreeSet<_> = entities.iter().map(|v| v.id().to_string()).collect();
        let mut associations = vec![];
        let mut bindings: Vec<(LocationId, BoundLocation)> = vec![];
        let mut closed: Vec<ClosedReference> = vec![];
        for value in &entities {
            if let Entity::Repository(repo) = value {
                associations.push((project, repo.id));
            }
            if let Entity::Location(loc) = value {
                let body: String = tx
                    .query_row(
                        "SELECT body FROM bindings WHERE location=?1",
                        [loc.id.to_string()],
                        |r| r.get(0),
                    )
                    .map_err(io)?;
                bindings.push((loc.id, decode(&body)?));
                let body: Option<String> = tx
                    .query_row(
                        "SELECT body FROM closed_history WHERE location=?1",
                        [loc.id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(io)?;
                if let Some(body) = body {
                    closed.push(decode(&body)?);
                }
            }
        }
        let mut external_content=vec!["Checkout/directory contents, Session transcripts, retained outputs, preserved file bodies and credentials are external and are NOT included".into()];
        for (id, bound) in &bindings {
            if let Err(error) = self.resolver.resolve_directory(&bound.binding) {
                external_content.push(format!("Location {id} is unavailable at export: {error}"));
            }
        }
        for record in &closed {
            for path in record.preservation_paths.iter().chain(record.report.iter()) {
                if let Err(error) = std::fs::metadata(path) {
                    external_content.push(format!(
                        "Preservation reference for {} is unavailable: {} ({error})",
                        record.location,
                        path.display()
                    ));
                }
            }
        }
        let mut external_grants = vec![];
        let grants = grants(&tx)?
            .into_iter()
            .filter(|g| {
                let refs = grant_targets(g);
                let included = refs.iter().all(|v| ids.contains(&v.to_string()));
                if !included && refs.iter().any(|v| ids.contains(&v.to_string())) {
                    external_grants.push(g.clone());
                    external_content.push(format!(
                        "Grant {} references identities outside this project and remains external",
                        g.id
                    ));
                }
                included
            })
            .collect();
        let mut stmt = tx
            .prepare("SELECT body,target FROM session_index ORDER BY session")
            .map_err(io)?;
        let mut session_references = vec![];
        for row in stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(io)?
        {
            let (body, target) = row.map_err(io)?;
            if ids.contains(&target) {
                session_references.push(decode(&body)?);
            }
        }
        let mut inherited_session_references = vec![];
        if !external_grants.is_empty() {
            inherited_session_references.push(SessionReferenceSet {
                projects: vec![project],
                installation: status.installation,
                sessions: vec![],
                grant_definitions: external_grants,
            });
        }
        let mut references = tx
            .prepare("SELECT body FROM imported_references ORDER BY id")
            .map_err(io)?;
        for body in references
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(io)?
        {
            let mut record: SessionReferenceSet = decode(&body.map_err(io)?)?;
            if record.projects.contains(&project) {
                record.projects = vec![project];
                inherited_session_references.push(record);
            }
        }
        let catalog = PortableCatalog {
            schema: 1,
            installation: status.installation,
            revision: status.revision,
            entities,
            associations,
            bindings,
            grants,
            session_references,
            inherited_session_references,
            closed,
            external_content,
        };
        let envelope = ExportEnvelope {
            name,
            project,
            sha256: digest(encode(&catalog)?.as_bytes()),
            catalog,
        };
        storage::atomic_json(&path, &envelope)?;
        Ok(path)
    }

    pub fn review_import(
        &self,
        path: PathBuf,
        expected: Revision,
        policy: ImportCollisionPolicy,
        remap: Vec<LocationRemap>,
    ) -> Result<ImportReview> {
        let envelope: ExportEnvelope = storage::read_json(&path)?;
        verify_envelope(&envelope)?;
        let input_digest = digest(encode(&(&envelope.sha256, policy, &remap))?.as_bytes());
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        organization::require_revision(&connection, expected)?;
        let mut payload = envelope.catalog;
        let mut collisions = vec![];
        let mut revision_differences = vec![];
        for value in &payload.entities {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM entities WHERE id=?1)",
                    [value.id().to_string()],
                    |r| r.get(0),
                )
                .map_err(io)?;
            if exists {
                collisions.push(value.id());
                revision_differences.push(ImportRevisionDifference {
                    identity: value.id(),
                    current: entity(&connection, value.id())?.revision(),
                    incoming: value.revision(),
                });
            }
        }
        let mut remapped = vec![];
        let mut unavailable = vec![];
        let mut seen = BTreeSet::new();
        for mapping in &remap {
            if !seen.insert(mapping.location)
                || !payload
                    .bindings
                    .iter()
                    .any(|(id, _)| *id == mapping.location)
            {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Duplicate or unknown location remapping",
                ));
            }
        }
        for (id, bound) in &mut payload.bindings {
            if let Some(mapping) = remap.iter().find(|v| v.location == *id) {
                bound.binding = self.resolver.bind_directory(&mapping.path).map_err(io)?;
                remapped.push(*id);
            }
            let available = self.resolver.resolve_directory(&bound.binding).is_ok();
            if !available {
                unavailable.push(*id);
            }
            let duplicate: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM bindings WHERE live_key=?1)",
                    [storage::physical_key(&bound.binding)?],
                    |r| r.get(0),
                )
                .map_err(io)?;
            if duplicate {
                return Err(issue(
                    IssueCode::Conflict,
                    format!(
                        "Location {id} already has a local physical identity. Remap it explicitly rather than duplicate ownership"
                    ),
                ));
            }
            for value in &mut payload.entities {
                if let Entity::Location(loc) = value
                    && loc.id == *id
                {
                    loc.observed_path = bound.binding.observed_path().into();
                    loc.volume_uuid = bound.binding.volume().as_str().into();
                    loc.binding_generation = bound.binding.generation();
                    if remapped.contains(id)
                        && let LocationKind::Checkout { repository, .. } = loc.kind
                    {
                        loc.kind = organization::checkout_kind(&loc.observed_path, repository)?;
                    }
                    if loc.lifecycle != LocationLifecycle::Closed {
                        loc.lifecycle = if available && loc.lifecycle == LocationLifecycle::Ready {
                            LocationLifecycle::Ready
                        } else {
                            LocationLifecycle::Unavailable
                        };
                    }
                }
            }
        }
        let disabled_grants = payload
            .grants
            .iter()
            .chain(
                payload
                    .inherited_session_references
                    .iter()
                    .flat_map(|r| r.grant_definitions.iter()),
            )
            .map(|g| g.id)
            .collect();
        for grant in &mut payload.grants {
            grant.state = GrantState::Disabled;
        }
        for record in &mut payload.inherited_session_references {
            for grant in &mut record.grant_definitions {
                grant.state = GrantState::Disabled;
            }
        }
        if policy == ImportCollisionPolicy::NewIdentities {
            remap_identities(&mut payload)?;
        }
        let local_revision = expected
            .checked_add(1)
            .ok_or_else(|| corrupt("Revision exhausted"))?;
        for value in &mut payload.entities {
            match value {
                Entity::Project(v) => v.revision = local_revision,
                Entity::Repository(v) => v.revision = local_revision,
                Entity::WorkArea(v) => v.revision = local_revision,
                Entity::Location(v) => v.revision = local_revision,
            }
        }
        for grant in &mut payload.grants {
            grant.revision = local_revision;
        }
        let issues = if !collisions.is_empty() && policy == ImportCollisionPolicy::Reject {
            vec![issue(
                IssueCode::Conflict,
                "Identity collisions require an explicit new-identity import or cancellation",
            )]
        } else {
            vec![]
        };
        let review = ImportReview {
            id: ReviewId::new(),
            revision: expected,
            source_installation: payload.installation,
            collisions,
            revision_differences,
            entities: payload.entities.clone(),
            remapped,
            unavailable,
            disabled_grants,
            external_content: payload.external_content.clone(),
            issues,
        };
        validate_portable(&payload)?;
        let prepared = PreparedImport {
            review: review.clone(),
            payload,
            input_digest,
        };
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        organization::require_revision(&tx, expected)?;
        tx.execute(
            "INSERT INTO reviews VALUES(?1,'import',?2)",
            params![review.id.to_string(), encode(&prepared)?],
        )
        .map_err(io)?;
        tx.commit().map_err(io)?;
        Ok(review)
    }
    pub fn apply_import(&self, request: RequestId, review: ReviewId) -> Result<Receipt> {
        let _lease = self.lease(true)?;
        let mut connection = self.connection()?;
        let prepared: PreparedImport = organization::read_review(&connection, review, "import")?;
        if let Some(old) = organization::replay(&connection, request, &prepared.input_digest)? {
            return Ok(old);
        }
        if !prepared.review.issues.is_empty() {
            return Err(prepared.review.issues[0].clone());
        }
        organization::require_revision(&connection, prepared.review.revision)?;
        for (id, bound) in &prepared.payload.bindings {
            if prepared.payload.entities.iter().any(|v|matches!(v,Entity::Location(loc) if loc.id==*id && loc.lifecycle==LocationLifecycle::Ready)) {
                self.resolver
                    .resolve_directory(&bound.binding)
                    .map_err(|e| issue(IssueCode::ReplacedRoot, e.to_string()))?;
            }
        }
        self.snapshot_locked(&connection, "Before portable import".into(), true)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        install_portable(&tx, &prepared.payload, false)?;
        self.checkpoint("import_before_commit")?;
        validate_graph(&tx)?;
        let receipt = organization::commit_receipt(
            &tx,
            request,
            &prepared.input_digest,
            prepared.payload.entities.iter().map(Entity::id).collect(),
        )?;
        tx.commit().map_err(io)?;
        self.checkpoint("import_after_commit")?;
        self.after_mutation(receipt)
    }
}

fn verify_envelope(envelope: &ExportEnvelope) -> Result<()> {
    if envelope.catalog.schema != 1
        || envelope.sha256 != digest(encode(&envelope.catalog)?.as_bytes())
    {
        return Err(corrupt("Portable catalog schema or checksum is invalid"));
    }
    validate_portable(&envelope.catalog)
}
fn validate_portable(payload: &PortableCatalog) -> Result<()> {
    let connection = Connection::open_in_memory().map_err(io)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(io)?;
    connection
        .execute_batch(include_str!("schema.sql"))
        .map_err(io)?;
    connection
        .execute(
            "INSERT INTO catalog VALUES(1,?1,?2)",
            params![
                payload.installation.to_string(),
                i64::try_from(payload.revision).map_err(corrupt)?
            ],
        )
        .map_err(io)?;
    install_portable(&connection, payload, true)?;
    validate_graph(&connection)
}
fn install_portable(
    connection: &Connection,
    payload: &PortableCatalog,
    validate_only: bool,
) -> Result<()> {
    for kind in [
        EntityKind::Project,
        EntityKind::Repository,
        EntityKind::WorkArea,
        EntityKind::Location,
    ] {
        for value in &payload.entities {
            if matches!(
                (kind, value),
                (EntityKind::Project, Entity::Project(_))
                    | (EntityKind::Repository, Entity::Repository(_))
                    | (EntityKind::WorkArea, Entity::WorkArea(_))
                    | (EntityKind::Location, Entity::Location(_))
            ) {
                connection
                    .execute(
                        "INSERT INTO entities(id,body) VALUES(?1,?2)",
                        params![value.id().to_string(), encode(value)?],
                    )
                    .map_err(|e| issue(IssueCode::Conflict, e.to_string()))?;
            }
        }
    }
    for (p, r) in &payload.associations {
        connection
            .execute(
                "INSERT INTO associations VALUES(?1,?2)",
                params![p.to_string(), r.to_string()],
            )
            .map_err(io)?;
    }
    for (id, bound) in &payload.bindings {
        let closed = matches!(
            entity(connection, EntityId::Location(*id))?,
            Entity::Location(Location {
                lifecycle: LocationLifecycle::Closed,
                ..
            })
        );
        connection
            .execute(
                "INSERT INTO bindings VALUES(?1,?2,?3)",
                params![
                    id.to_string(),
                    encode(bound)?,
                    if closed {
                        None
                    } else {
                        Some(storage::physical_key(&bound.binding)?)
                    }
                ],
            )
            .map_err(|e| issue(IssueCode::Conflict, e.to_string()))?;
    }
    for grant in &payload.grants {
        save_grant(connection, grant)?;
    }
    for reference in &payload.closed {
        connection
            .execute(
                "INSERT INTO closed_history VALUES(?1,?2)",
                params![reference.location.to_string(), encode(reference)?],
            )
            .map_err(io)?;
    }
    if !validate_only {
        // Foreign Session rows stay external references, never the live derived index.
        let own = SessionReferenceSet {
            projects: payload
                .entities
                .iter()
                .filter_map(|v| match v {
                    Entity::Project(p) => Some(p.id),
                    _ => None,
                })
                .collect(),
            installation: payload.installation,
            sessions: payload.session_references.clone(),
            grant_definitions: vec![],
        };
        for record in std::iter::once(&own).chain(payload.inherited_session_references.iter()) {
            for project in &record.projects {
                entity(connection, EntityId::Project(*project))?;
            }
            connection
                .execute(
                    "INSERT INTO imported_references VALUES(?1,?2)",
                    params![OperationId::new().to_string(), encode(record)?],
                )
                .map_err(io)?;
        }
    }
    Ok(())
}
fn remap_identities(payload: &mut PortableCatalog) -> Result<()> {
    let mut mapping = BTreeMap::new();
    for value in &payload.entities {
        let old = value.id();
        let new = match old {
            EntityId::Project(_) => EntityId::Project(ProjectId::new()),
            EntityId::Repository(_) => EntityId::Repository(RepositoryId::new()),
            EntityId::WorkArea(_) => EntityId::WorkArea(WorkAreaId::new()),
            EntityId::Location(_) => EntityId::Location(LocationId::new()),
        };
        mapping.insert(old.to_string(), new.to_string());
    }
    // UUID-backed typed identity strings are replaced only in the typed metadata
    // serialization. Paths, display names and descriptive text are not traversed.
    fn id<T: std::str::FromStr + std::fmt::Display>(
        value: T,
        map: &BTreeMap<String, String>,
    ) -> Result<T>
    where
        T::Err: std::fmt::Display,
    {
        map.get(&value.to_string())
            .ok_or_else(|| corrupt("Missing imported identity reference"))?
            .parse()
            .map_err(corrupt)
    }
    fn home(value: Home, map: &BTreeMap<String, String>) -> Result<Home> {
        Ok(match value {
            Home::Project(v) => Home::Project(id(v, map)?),
            Home::WorkArea(v) => Home::WorkArea(id(v, map)?),
        })
    }
    for value in &mut payload.entities {
        match value {
            Entity::Project(v) => v.id = id(v.id, &mapping)?,
            Entity::Repository(v) => v.id = id(v.id, &mapping)?,
            Entity::WorkArea(v) => {
                v.id = id(v.id, &mapping)?;
                v.project = id(v.project, &mapping)?;
            }
            Entity::Location(v) => {
                v.id = id(v.id, &mapping)?;
                v.home = v.home.map(|h| home(h, &mapping)).transpose()?;
                if let LocationKind::Checkout { repository, .. } = &mut v.kind {
                    *repository = id(*repository, &mapping)?;
                }
            }
        }
    }
    for (p, r) in &mut payload.associations {
        *p = id(*p, &mapping)?;
        *r = id(*r, &mapping)?;
    }
    for (l, _) in &mut payload.bindings {
        *l = id(*l, &mapping)?;
    }
    for grant in &mut payload.grants {
        grant.copied_from = Some(grant.id);
        grant.id = GrantId::new();
        grant.audience = match &grant.audience {
            Audience::Session(v) => Audience::Session(v.clone()),
            Audience::Project(v) => Audience::Project(id(*v, &mapping)?),
            Audience::WorkArea(v) => Audience::WorkArea(id(*v, &mapping)?),
            Audience::Checkout(v) => Audience::Checkout(id(*v, &mapping)?),
        };
        grant.target = match grant.target {
            WriteTarget::Root(v) => WriteTarget::Root(id(v, &mapping)?),
            WriteTarget::ProjectMembers(v) => WriteTarget::ProjectMembers(id(v, &mapping)?),
            WriteTarget::WorkAreaMembers(v) => WriteTarget::WorkAreaMembers(id(v, &mapping)?),
        };
    }
    for closed in &mut payload.closed {
        closed.location = id(closed.location, &mapping)?;
    }
    for record in &mut payload.inherited_session_references {
        for project in &mut record.projects {
            *project = id(*project, &mapping)?;
        }
    }
    // Session references deliberately retain their original installation-local placement.
    Ok(())
}
