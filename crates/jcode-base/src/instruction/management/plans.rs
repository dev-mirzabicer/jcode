use super::metadata::{parse, selector, selector_text};
use super::*;

pub(super) struct EditPlan {
    pub repository: InstructionRepositoryRef,
    pub head: String,
    pub files: BTreeMap<PathBuf, Option<Vec<u8>>>,
    pub executables: BTreeMap<PathBuf, bool>,
    pub observed: BTreeMap<PathBuf, InstructionFileState>,
    pub subject: String,
    pub warnings: Vec<String>,
}

pub(super) fn plan(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
    action: InstructionEditAction,
) -> Result<EditPlan> {
    if let InstructionEditAction::CopySkill {
        scope,
        destination_id,
    } = action
    {
        return copy_skill(service, target, scope, destination_id.as_deref());
    }
    if let InstructionEditAction::RestorePath { revision, path } = &action {
        return historical_path(service, target, revision, path);
    }
    if matches!(action, InstructionEditAction::ImportLegacy) {
        return import_legacy(service, target);
    }
    let selected = || -> Result<(InstructionRepositoryRef, PathBuf)> {
        let row = target
            .row
            .as_ref()
            .ok_or_else(|| fail("edit", "Select a managed resource"))?;
        if row.origin != InstructionOrigin::Managed {
            return Err(fail(
                "edit",
                "External sources are not managed files. Use the dedicated ecosystem or Copy action.",
            ));
        }
        let repository = target
            .repository
            .clone()
            .ok_or_else(|| fail("edit", "Instruction repository unavailable"))?;
        if repository.id.starts_with("external:") {
            return Err(fail(
                "edit",
                "The enclosing project is not an instruction repository",
            ));
        }
        let path = target
            .path
            .as_ref()
            .and_then(|path| path.strip_prefix(&repository.root).ok())
            .ok_or_else(|| fail("edit", "Resource no longer belongs to its repository"))?
            .to_path_buf();
        Ok((repository, path))
    };
    if target
        .resource
        .as_ref()
        .is_some_and(|resource| resource.kind == InstructionKind::Skill)
        && matches!(
            action,
            InstructionEditAction::Rename { .. }
                | InstructionEditAction::Delete
                | InstructionEditAction::RedefineInProject
        )
    {
        let (repository, path) = selected()?;
        return skill_package(service, target, repository, path, action);
    }
    let (repository, path, creation) = match &action {
        InstructionEditAction::Create { scope, fields } => {
            let repository = resolve_repository(service, &target.context, *scope)?;
            InstructionId::parse(&fields.id).map_err(|error| fail("create resource", error))?;
            let path = resource_path(metadata::kind(fields.kind), &fields.id);
            (repository, path, true)
        }
        InstructionEditAction::RedefineInProject => {
            let (source, path) = selected()?;
            if source.kind != InstructionRepositoryKind::Global {
                return Err(fail("redefine", "Select a global resource"));
            }
            (
                resolve_repository(service, &target.context, InstructionEditScope::Project)?,
                path,
                true,
            )
        }
        InstructionEditAction::Addendum { id } => {
            if target
                .resource
                .as_ref()
                .is_none_or(|resource| resource.kind != InstructionKind::Agent)
            {
                return Err(fail("addendum", "Select an agent to receive the addendum"));
            }
            InstructionId::parse(id).map_err(|error| fail("addendum ID", error))?;
            (
                resolve_repository(service, &target.context, InstructionEditScope::Project)?,
                resource_path(InstructionKind::AgentAddendum, id),
                true,
            )
        }
        InstructionEditAction::Settings { scope } => (
            resolve_repository(service, &target.context, *scope)?,
            PathBuf::from("instruction-store.toml"),
            false,
        ),
        _ => {
            let (repository, path) = selected()?;
            (repository, path, false)
        }
    };
    let action = if let InstructionEditAction::Restore { revision } = action {
        let revision = if revision == "HEAD" {
            service
                .inspect(&repository)
                .map_err(repo_error)?
                .head
                .ok_or_else(|| fail("restore HEAD", "Repository has no baseline"))?
        } else {
            revision
        };
        InstructionEditAction::Restore { revision }
    } else {
        action
    };
    if let InstructionEditAction::Restore { revision } = &action
        && path.starts_with("skills")
        && path.file_name().is_some_and(|name| name == "SKILL.md")
    {
        return restore_package(service, repository, &path, revision);
    }
    let base = service.open_draft(&repository, &path).map_err(repo_error)?;
    if creation && base.source_bytes().is_some() {
        return Err(fail(
            "create resource",
            "Destination already exists. Edit it rather than overwriting it.",
        ));
    }
    let mut plan = EditPlan { repository: repository.clone(), head: base.base_head.clone(), files: BTreeMap::new(), executables: BTreeMap::from([(path.clone(), base.base.executable)]), observed: BTreeMap::from([(path.clone(), base.base.clone())]), subject: format!("instruction: edit {}", path.display()), warnings: vec!["Source changes affect later activations or occurrences. Current session system and active-skill instructions remain unchanged.".into()] };
    if base.binary_content.is_some() && !matches!(action, InstructionEditAction::Restore { .. }) {
        return Err(fail(
            "edit invalid source",
            "Source is not UTF-8. Its bytes are preserved; restore a valid Git revision instead of treating it as empty.",
        ));
    }
    let current = base.content.clone().unwrap_or_default();
    let proposed = match action {
        InstructionEditAction::ImportLegacy => {
            return Err(fail(
                "import",
                "Import must resolve its compatibility source",
            ));
        }
        InstructionEditAction::Edit
        | InstructionEditAction::CommitExternal
        | InstructionEditAction::Settings { .. } => current,
        InstructionEditAction::Create { fields, .. } => {
            plan.subject = format!("instruction: create {}", fields.id);
            metadata::document(&repository, &path, &fields, String::new())?
                .to_markdown()
                .map_err(|error| fail("create resource", error))?
        }
        InstructionEditAction::RedefineInProject => {
            let (source_repo, source_path) = selected()?;
            let source = service
                .read_file(
                    &source_repo,
                    &source_path,
                    InstructionReadPolicy::WorkingTreeOnly,
                )
                .map_err(repo_error)?
                .content;
            let document = parse(&repository, &path, &source)?;
            plan.warnings.push(format!("Project redefinition of {}. Unqualified project lookups will use this definition, including high-impact system/profile/control guidance.", document.id));
            plan.subject = format!("instruction: redefine {} in project", document.id);
            source
        }
        InstructionEditAction::Addendum { id } => {
            let resource = target
                .resource
                .as_ref()
                .ok_or_else(|| fail("addendum", "Agent selection expired"))?;
            let document = InstructionDocument {
                id: InstructionId::parse(id).map_err(|error| fail("addendum", error))?,
                kind: InstructionKind::AgentAddendum,
                scope: InstructionScope::Project,
                template_mode: TemplateMode::Plain,
                metadata: InstructionMetadata {
                    addendum: Some(AddendumMetadata {
                        target: selector(resource),
                    }),
                    ..Default::default()
                },
                body: String::new(),
                path: repository.root.join(&path),
            };
            plan.subject = format!("instruction: add guidance for {}", resource.id);
            document
                .to_markdown()
                .map_err(|error| fail("addendum", error))?
        }
        InstructionEditAction::Clear => {
            let mut document = parse(&repository, &path, &current)?;
            if document.body.is_empty() {
                plan.files.insert(path, Some(current.into_bytes()));
                return Ok(plan);
            }
            document.body.clear();
            plan.subject = format!("instruction: clear {}", document.id);
            plan.warnings.push("Clear retains resource identity and intentionally contributes an empty body. It does not reveal a global definition.".into());
            document
                .to_markdown()
                .map_err(|error| fail("clear body", error))?
        }
        InstructionEditAction::Restore { revision } => {
            let content = service
                .content_at_revision(&repository, &revision, &path)
                .map_err(repo_error)?;
            plan.subject = format!("instruction: restore {} from {}", path.display(), revision);
            plan.warnings.push("Restore creates a new commit. It does not rewrite history or discard unrelated changes.".into());
            content.content
        }
        InstructionEditAction::Rename { id } => {
            let replacement = InstructionId::parse(id).map_err(|error| fail("rename", error))?;
            let original = parse(&repository, &path, &current)?;
            if replacement == original.id {
                plan.files.insert(path, Some(current.into_bytes()));
                return Ok(plan);
            }
            protect_registered(&original)?;
            repair_references(
                service,
                &target.context,
                &original,
                Some(&replacement),
                &mut plan,
            )?;
            let destination = resource_path(original.kind, replacement.as_str());
            let dest = service
                .open_draft(&repository, &destination)
                .map_err(repo_error)?;
            if dest.source_bytes().is_some() {
                return Err(fail("rename", "New resource ID/path already exists"));
            }
            let mut renamed = original;
            renamed.id = replacement;
            renamed.path = repository.root.join(&destination);
            plan.executables
                .insert(destination.clone(), base.base.executable);
            plan.observed.insert(destination.clone(), dest.base);
            plan.files.insert(
                destination,
                Some(
                    renamed
                        .to_markdown()
                        .map_err(|error| fail("rename", error))?
                        .into_bytes(),
                ),
            );
            plan.files.insert(path, None);
            plan.subject = format!("instruction: rename resource to {}", renamed.id);
            return Ok(plan);
        }
        InstructionEditAction::Delete => {
            let original = parse(&repository, &path, &current)?;
            protect_registered(&original)?;
            repair_references(service, &target.context, &original, None, &mut plan)?;
            plan.files.insert(path, None);
            plan.subject = format!("instruction: delete {}", original.id);
            plan.warnings.push("Deletion removes the resource. Deleting a project redefinition reveals the global definition. Referencing files are included in this draft for explicit repair before Save.".into());
            return Ok(plan);
        }
        InstructionEditAction::RestorePath { .. } => {
            return Err(fail(
                "restore",
                "Historical path must be resolved by its repository",
            ));
        }
        InstructionEditAction::CopySkill { .. } => {
            return Err(fail(
                "Copy skill",
                "Use the reviewed Copy action, not a text draft",
            ));
        }
    };
    plan.files.insert(path, Some(proposed.into_bytes()));
    Ok(plan)
}

pub(super) fn resource_path(kind: InstructionKind, id: &str) -> PathBuf {
    if kind == InstructionKind::Skill {
        PathBuf::from("skills").join(id).join("SKILL.md")
    } else {
        PathBuf::from(kind.directory()).join(format!("{id}.md"))
    }
}
fn protect_registered(document: &InstructionDocument) -> Result<()> {
    if document.scope == InstructionScope::Global
        && registrations()?
            .iter()
            .any(|entry| entry.required && entry.kind == document.kind && entry.id == document.id)
    {
        return Err(fail(
            "change resource identity",
            "This code-registered resource has a fixed identity. Edit its display metadata/body, or create a separate user resource.",
        ));
    }
    Ok(())
}
fn matches(
    runtime: &InstructionRuntime,
    reference: &InstructionSelector,
    target: &InstructionResourceRef,
) -> bool {
    runtime.resolve(reference).is_ok_and(|document| {
        document.scope == target.scope && document.kind == target.kind && document.id == target.id
    })
}
fn rewritten(
    runtime: &InstructionRuntime,
    document: &InstructionDocument,
    target: &InstructionResourceRef,
    replacement: Option<&InstructionId>,
) -> Result<(bool, InstructionDocument)> {
    let mut result = document.clone();
    let mut found = false;
    for reference in result.metadata.includes.iter_mut().chain(
        result
            .metadata
            .addendum
            .iter_mut()
            .map(|value| &mut value.target),
    ) {
        if matches(runtime, reference, target) {
            found = true;
            if let Some(replacement) = replacement {
                reference.id = replacement.clone();
            }
        }
    }
    if result.template_mode == TemplateMode::Handlebars {
        let identity = InstructionResourceRef {
            scope: document.scope,
            kind: document.kind,
            id: document.id.clone(),
        };
        let segments = super::super::template::parse_restricted_template(&identity, &document.body)
            .map_err(|error| fail("inspect template references", error))?;
        for segment in segments.into_iter().rev() {
            if let super::super::template::TemplateSegment::Partial(mut reference, range) = segment
                && matches(runtime, &reference, target)
            {
                found = true;
                if let Some(replacement) = replacement {
                    reference.id = replacement.clone();
                    result.body.replace_range(range, &selector_text(&reference));
                }
            }
        }
    }
    Ok((found, result))
}
fn repair_references(
    service: &InstructionRepositoryService,
    context: &InspectionContext,
    original: &InstructionDocument,
    replacement: Option<&InstructionId>,
    plan: &mut EditPlan,
) -> Result<()> {
    let project = context
        .working_dir
        .as_deref()
        .map(|path| service.resolve_project_repository(path))
        .transpose()
        .map_err(repo_error)?
        .flatten();
    let sources = service
        .instruction_sources(project.as_ref())
        .map_err(repo_error)?;
    let global_runtime = InstructionRuntime::discover(InstructionSources::new(
        service.global_repository().map_err(repo_error)?.root,
    ));
    let runtime = InstructionRuntime::discover(sources);
    let target = InstructionResourceRef {
        scope: original.scope,
        kind: original.kind,
        id: original.id.clone(),
    };
    for summary in runtime.resources() {
        if summary.resource == target {
            continue;
        }
        let document = runtime
            .resolve(&selector(&summary.resource))
            .map_err(|error| {
                fail(
                    "reference analysis",
                    format!(
                        "Cannot prove reference repair while a catalog resource is invalid: {error}"
                    ),
                )
            })?;
        let owner_runtime = if document.scope == InstructionScope::Global {
            &global_runtime
        } else {
            &runtime
        };
        let (found, rewritten) = rewritten(owner_runtime, document, &target, replacement)?;
        if found
            && replacement.is_some()
            && document.scope == InstructionScope::Global
            && project.is_some()
        {
            let (_, effective_rewrite) = self::rewritten(&runtime, document, &target, replacement)?;
            if effective_rewrite != rewritten {
                return Err(fail(
                    "project shadow migration",
                    "Renaming this global resource would also change an unqualified reference currently supplied by a project shadow. Migrate the project shadow/references explicitly before removing the old global identity. No automatic cross-repository rewrite was performed.",
                ));
            }
        }
        if !found {
            continue;
        }
        let relative = document.path.strip_prefix(&plan.repository.root).map_err(|_| fail("cross-repository reference", format!("{} references {} from another repository. Create the new resource first, repair those references in their repository, then remove the old resource. Unopened projects are not scanned.", document.path.display(), original.id)))?.to_path_buf();
        let capture = service
            .open_draft(&plan.repository, &relative)
            .map_err(repo_error)?;
        if parse(
            &plan.repository,
            &relative,
            capture.content.as_deref().unwrap_or_default(),
        )? != *document
        {
            return Err(fail(
                "reference analysis",
                "Source changed while planning reference repairs; refresh",
            ));
        }
        plan.observed.insert(relative.clone(), capture.base.clone());
        let content = if replacement.is_some() {
            rewritten
                .to_markdown()
                .map_err(|error| fail("repair references", error))?
        } else {
            capture.content.unwrap_or_default()
        };
        plan.files.insert(relative, Some(content.into_bytes()));
    }
    for repository in std::iter::once(plan.repository.clone())
        .chain(project.filter(|project| project.root != plan.repository.root))
    {
        let Ok(mut manifest) = service.load_manifest(&repository) else {
            continue;
        };
        let Some(default) = &manifest.default_agent else {
            continue;
        };
        let reference = InstructionSelector::parse(InstructionKind::Agent, default)
            .map_err(|error| fail("default reference", error))?;
        let owner_runtime = if repository.kind == InstructionRepositoryKind::Global {
            &global_runtime
        } else {
            &runtime
        };
        if !matches(owner_runtime, &reference, &target) {
            continue;
        }
        if repository.root != plan.repository.root {
            return Err(fail(
                "cross-repository default",
                "The active project's default agent references this resource. Migrate that default explicitly before removing the old ID.",
            ));
        }
        manifest.default_agent = replacement.map(|id| {
            let mut reference = reference.clone();
            reference.id = id.clone();
            selector_text(&reference)
        });
        let path = PathBuf::from("instruction-store.toml");
        plan.observed.insert(
            path.clone(),
            service
                .open_draft(&repository, &path)
                .map_err(repo_error)?
                .base,
        );
        plan.files.insert(
            path,
            Some(
                toml::to_string_pretty(&manifest)
                    .map_err(|error| fail("repair default", error))?
                    .into_bytes(),
            ),
        );
        if replacement.is_none() {
            plan.warnings
                .push("The default-agent selection in this repository will be cleared.".into());
        }
    }
    Ok(())
}

fn copy_skill(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
    scope: InstructionEditScope,
    destination_id: Option<&str>,
) -> Result<EditPlan> {
    let row = target
        .row
        .as_ref()
        .filter(|row| row.kind == "skill" && row.origin == InstructionOrigin::External)
        .ok_or_else(|| fail("Copy skill", "Select an external skill source"))?;
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| fail("Copy skill", "Source path is unavailable"))?;
    let base = crate::skill::SkillRegistry::inspect_global_sources()
        .map_err(|error| fail("Copy discovery", error))?;
    let registry = crate::skill::SkillRegistry::effective_for_working_dir_with_repositories(
        &base,
        target.context.working_dir.as_deref(),
        service,
    );
    let source = registry
        .catalog_entries()
        .into_iter()
        .find(|entry| {
            entry.source.package_root.join("SKILL.md") == *path && !entry.source.kind.is_managed()
        })
        .ok_or_else(|| fail("Copy skill", "Selected source changed; refresh discovery"))?;
    let operation = uuid::Uuid::new_v4().to_string();
    let prepared = crate::skill::prepare_external_skill_copy(
        service,
        &registry,
        crate::skill::ManagedSkillCopyRequest {
            skill_name: &source.name,
            source: Some(&source.source),
            working_dir: target.context.working_dir.as_deref(),
            destination: if scope == InstructionEditScope::Global {
                crate::skill::ManagedSkillDestination::Global
            } else {
                crate::skill::ManagedSkillDestination::Project
            },
            destination_id,
            operation_id: &operation,
        },
    )
    .map_err(repo_error)?;
    let observed = prepared
        .request
        .expected_files
        .into_iter()
        .map(|state| (state.relative_path.clone(), state))
        .collect();
    let executables = prepared
        .request
        .mutations
        .iter()
        .filter_map(|mutation| match mutation {
            InstructionFileMutation::WriteFile {
                relative_path,
                executable,
                ..
            } => Some((relative_path.clone(), *executable)),
            _ => None,
        })
        .collect();
    let files = prepared
        .request
        .mutations
        .into_iter()
        .map(|mutation| match mutation {
            InstructionFileMutation::Write {
                relative_path,
                content,
            }
            | InstructionFileMutation::WriteFile {
                relative_path,
                content,
                ..
            } => Ok((relative_path, Some(content))),
            _ => Err(fail(
                "Copy skill",
                "Prepared Copy contained a non-write mutation",
            )),
        })
        .collect::<Result<_>>()?;
    Ok(EditPlan {
        repository: prepared.repository,
        head: prepared.request.expected_head,
        files,
        executables,
        observed,
        subject: prepared.request.message,
        warnings: vec![format!(
            "Copy {} from {}. The complete captured package and attribution are retained. Source files are not modified and no scripts are executed. Effective in the source project after Save: {}. Existing active skill text remains frozen.",
            prepared.invocation_name, row.scope, prepared.effective_for_source_project
        )],
    })
}

fn skill_package(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
    source_repo: InstructionRepositoryRef,
    path: PathBuf,
    action: InstructionEditAction,
) -> Result<EditPlan> {
    let root = path
        .parent()
        .ok_or_else(|| fail("skill package", "Entry point has no package directory"))?
        .to_path_buf();
    let source = crate::skill::capture_managed_skill_package(service, &source_repo, &root)
        .map_err(repo_error)?;
    let entry = source
        .get(&path)
        .ok_or_else(|| fail("skill package", "SKILL.md is absent"))?;
    let mut document = parse(
        &source_repo,
        &path,
        std::str::from_utf8(entry).map_err(|error| fail("skill entry", error))?,
    )?;
    let (repository, destination, delete_source, rename) = match &action {
        InstructionEditAction::Delete => (source_repo.clone(), root.clone(), true, None),
        InstructionEditAction::Rename { id } => {
            let id = InstructionId::parse(id).map_err(|error| fail("skill ID", error))?;
            let destination = PathBuf::from("skills").join(id.as_str());
            (
                source_repo.clone(),
                destination,
                id != document.id,
                Some(id),
            )
        }
        InstructionEditAction::RedefineInProject => {
            if source_repo.kind != InstructionRepositoryKind::Global {
                return Err(fail("skill redefinition", "Select a global package"));
            }
            (
                resolve_repository(service, &target.context, InstructionEditScope::Project)?,
                root.clone(),
                false,
                None,
            )
        }
        _ => return Err(fail("skill package", "Invalid package action")),
    };
    let deleting = matches!(action, InstructionEditAction::Delete);
    if !deleting
        && (repository.root != source_repo.root || destination != root)
        && !crate::skill::capture_managed_skill_package(service, &repository, &destination)
            .map_err(repo_error)?
            .is_empty()
    {
        return Err(fail(
            "skill destination",
            "Destination package contains existing files. Nothing was overwritten.",
        ));
    }
    let head = service
        .inspect(&repository)
        .map_err(repo_error)?
        .head
        .ok_or_else(|| fail("skill package", "Destination has no baseline commit"))?;
    let mut plan = EditPlan { repository: repository.clone(), head, files: BTreeMap::new(), executables: BTreeMap::new(), observed: BTreeMap::new(), subject: if deleting { format!("skill: delete {}", document.id) } else if let Some(id) = &rename { format!("skill: rename package {} to {id}", document.id) } else { format!("skill: redefine {} in project", document.id) }, warnings: vec!["The complete package, including references and binary files, is one reviewed transaction. Existing active skill snapshots remain unchanged. Resource-ID rename preserves the invocation name; change its metadata explicitly if you also want a new slash name.".into()] };
    for (source_path, mut content) in source {
        let observed_source = service
            .open_draft(&source_repo, &source_path)
            .map_err(repo_error)?;
        if observed_source.source_bytes() != Some(content.as_slice()) {
            return Err(fail(
                "skill source",
                "Package changed during planning. Refresh before proceeding.",
            ));
        }
        if delete_source || deleting {
            plan.observed
                .insert(source_path.clone(), observed_source.base.clone());
            plan.files.insert(source_path.clone(), None);
        }
        if !deleting {
            let destination_path = destination.join(
                source_path
                    .strip_prefix(&root)
                    .map_err(|error| fail("package path", error))?,
            );
            if source_path == path
                && let Some(id) = &rename
                && *id != document.id
            {
                document.id = id.clone();
                document.path = repository.root.join(&destination_path);
                content = document
                    .to_markdown()
                    .map_err(|error| fail("rename skill metadata", error))?
                    .into_bytes();
            }
            let base = service
                .open_draft(&repository, &destination_path)
                .map_err(repo_error)?;
            if base.base_head != plan.head {
                return Err(fail(
                    "skill destination",
                    "Repository changed during planning",
                ));
            }
            plan.executables
                .insert(destination_path.clone(), observed_source.base.executable);
            plan.observed.insert(destination_path.clone(), base.base);
            plan.files.insert(destination_path, Some(content));
        }
    }
    Ok(plan)
}

fn historical_path(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
    revision: &str,
    path: &str,
) -> Result<EditPlan> {
    let repository = target
        .repository
        .as_ref()
        .filter(|repository| !repository.id.starts_with("external:"))
        .ok_or_else(|| {
            fail(
                "restore history",
                "Select an instruction repository revision",
            )
        })?
        .clone();
    let entries = service
        .history_page(&repository, None, revision, 0, 1)
        .map_err(repo_error)?;
    let entry = entries
        .first()
        .filter(|entry| entry.commit == revision)
        .ok_or_else(|| fail("restore history", "Revision was not found"))?;
    let path = PathBuf::from(path);
    if !entry.changed_paths.contains(&path) {
        return Err(fail(
            "restore history",
            "Choose a changed path from this exact revision",
        ));
    }
    let package = path
        .strip_prefix("skills")
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| {
            PathBuf::from("skills")
                .join(component.as_os_str())
                .join("SKILL.md")
        });
    if let Some(package) = package {
        return restore_package(service, repository, &package, revision);
    }
    let content = service
        .content_at_revision(&repository, revision, &path)
        .map_err(repo_error)?;
    if path != Path::new("instruction-store.toml")
        && path != Path::new(crate::model_roster::ROSTER_PATH)
    {
        parse(&repository, &path, &content.content)?;
    }
    let base = service.open_draft(&repository, &path).map_err(repo_error)?;
    let mode = service
        .file_modes_at_revision(&repository, revision, &path)
        .map_err(repo_error)?
        .get(&path)
        .copied()
        .unwrap_or(false);
    Ok(EditPlan { repository, head: base.base_head, files: BTreeMap::from([(path.clone(),Some(content.content.into_bytes()))]), executables: BTreeMap::from([(path.clone(),mode)]), observed: BTreeMap::from([(path,base.base)]), subject: format!("instruction: restore historical resource from {revision}"), warnings: vec!["Restore publishes the selected historical content as a new reviewed commit. Parent history and unrelated paths remain untouched.".into()] })
}
fn restore_package(
    service: &InstructionRepositoryService,
    repository: InstructionRepositoryRef,
    entry: &Path,
    revision: &str,
) -> Result<EditPlan> {
    let root = entry
        .parent()
        .ok_or_else(|| fail("restore package", "Invalid package path"))?;
    let files = service
        .files_at_revision_under(&repository, revision, root)
        .map_err(repo_error)?;
    if !files.contains_key(entry) {
        return Err(fail(
            "restore package",
            "Choose a revision containing this package's SKILL.md",
        ));
    }
    let modes = service
        .file_modes_at_revision(&repository, revision, root)
        .map_err(repo_error)?;
    let current = crate::skill::capture_managed_skill_package(service, &repository, root)
        .map_err(repo_error)?;
    let head = service
        .inspect(&repository)
        .map_err(repo_error)?
        .head
        .ok_or_else(|| fail("restore package", "No current baseline"))?;
    let mut plan = EditPlan { repository: repository.clone(), head, files: files.into_iter().map(|(path,bytes)| (path,Some(bytes))).collect(), executables: modes, observed: BTreeMap::new(), subject: format!("skill: restore {} from {revision}", root.display()), warnings: vec!["This restores the complete historical skill package, including reference bytes and executable modes. Newer package files absent in that revision will be removed in the same reviewed commit. Existing active skill snapshots remain unchanged.".into()] };
    for path in current.keys() {
        plan.files.entry(path.clone()).or_insert(None);
    }
    for path in plan.files.keys() {
        let base = service.open_draft(&repository, path).map_err(repo_error)?;
        if base.base_head != plan.head {
            return Err(fail(
                "restore package",
                "Repository changed during preparation",
            ));
        }
        plan.observed.insert(path.clone(), base.base);
    }
    Ok(plan)
}

fn import_legacy(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
) -> Result<EditPlan> {
    let row = target
        .row
        .as_ref()
        .filter(|row| row.origin == InstructionOrigin::Legacy)
        .ok_or_else(|| {
            fail(
                "legacy import",
                "Select a legacy compatibility source, not AGENTS.md or an external skill",
            )
        })?;
    let selected_path = target
        .path
        .as_deref()
        .ok_or_else(|| fail("legacy import", "Source identity is missing"))?;
    let source = service
        .discover_known_legacy_sources(target.context.working_dir.as_deref())
        .map_err(repo_error)?
        .into_iter()
        .find(|source| source.path == selected_path)
        .ok_or_else(|| {
            fail(
                "legacy import",
                "Selected source is not a known compatibility input",
            )
        })?;
    let repository = resolve_repository(
        service,
        &target.context,
        if source.scope == InstructionScope::Global {
            InstructionEditScope::Global
        } else {
            InstructionEditScope::Project
        },
    )?;
    let directory = source
        .path
        .parent()
        .ok_or_else(|| fail("legacy import", "Source directory is missing"))?;
    let spec = known_legacy_import(source.scope, directory, source.source_kind)
        .map_err(|error| fail("import target", error))?;
    let import = service
        .plan_legacy_import(&spec)
        .map_err(repo_error)?
        .ok_or_else(|| fail("legacy import", "Source is absent"))?;
    let manifest_path = PathBuf::from("instruction-store.toml");
    let captured = service
        .open_draft(&repository, &manifest_path)
        .map_err(repo_error)?;
    let mut manifest: InstructionStoreManifest =
        toml::from_str(captured.content.as_deref().unwrap_or_default())
            .map_err(|error| fail("import manifest", error))?;
    if manifest.legacy_imports.contains_key(&spec.import_id) {
        return Err(fail(
            "legacy import",
            "This source already has a durable import receipt. Edit or restore its managed resource; the preserved legacy file is inactive.",
        ));
    }
    let destination = service
        .open_draft(&repository, &spec.target.relative_path)
        .map_err(repo_error)?;
    if destination.base_head != captured.base_head {
        return Err(fail(
            "legacy import",
            "Repository changed during preparation",
        ));
    }
    manifest
        .legacy_imports
        .insert(spec.import_id, import.receipt());
    Ok(EditPlan {
        repository,
        head: captured.base_head,
        files: BTreeMap::from([
            (
                manifest_path.clone(),
                Some(
                    toml::to_string_pretty(&manifest)
                        .map_err(|error| fail("import manifest", error))?
                        .into_bytes(),
                ),
            ),
            (
                spec.target.relative_path.clone(),
                Some(import.managed_content.into_bytes()),
            ),
        ]),
        executables: BTreeMap::new(),
        observed: BTreeMap::from([
            (manifest_path, captured.base),
            (spec.target.relative_path, destination.base),
        ]),
        subject: format!("instruction: import {} compatibility source", row.scope),
        warnings: vec![format!(
            "Import preserves the complete captured source from {} and retains the original file. The managed definition and cutover receipt are one reviewed commit. If a managed destination exists, its replacement is visible in the diff. Existing sessions keep their frozen instructions.",
            selected_path.display()
        )],
    })
}
