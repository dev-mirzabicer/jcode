use super::metadata::{parse, selector, selector_text};
use super::*;

pub(super) struct EditPlan {
    pub repository: InstructionRepositoryRef,
    pub files: BTreeMap<PathBuf, Option<String>>,
    pub observed: BTreeMap<PathBuf, Option<String>>,
    pub subject: String,
    pub warnings: Vec<String>,
}

pub(super) fn plan(
    service: &InstructionRepositoryService,
    target: &ResolvedManagementTarget,
    action: InstructionEditAction,
) -> Result<EditPlan> {
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
    let base = service.open_draft(&repository, &path).map_err(repo_error)?;
    if creation && base.content.is_some() {
        return Err(fail(
            "create resource",
            "Destination already exists. Edit it rather than overwriting it.",
        ));
    }
    let mut plan = EditPlan { repository: repository.clone(), files: BTreeMap::new(), observed: BTreeMap::from([(path.clone(), base.content.clone())]), subject: format!("instruction: edit {}", path.display()), warnings: vec!["Source changes affect later activations or occurrences. Current session system and active-skill instructions remain unchanged.".into()] };
    let current = base.content.clone().unwrap_or_default();
    let proposed = match action {
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
                plan.files.insert(path, Some(current));
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
            if dest.content.is_some() {
                return Err(fail("rename", "New resource ID/path already exists"));
            }
            let mut renamed = original;
            renamed.id = replacement;
            renamed.path = repository.root.join(&destination);
            plan.observed.insert(destination.clone(), None);
            plan.files.insert(
                destination,
                Some(
                    renamed
                        .to_markdown()
                        .map_err(|error| fail("rename", error))?,
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
        InstructionEditAction::CopySkill { .. } => {
            return Err(fail(
                "Copy skill",
                "Use the reviewed Copy action, not a text draft",
            ));
        }
    };
    plan.files.insert(path, Some(proposed));
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
    if document.kind == InstructionKind::Skill {
        return Err(fail(
            "change skill identity",
            "Skill packages require package-aware rename/delete, including references and binary files",
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
        let (found, rewritten) = rewritten(&runtime, document, &target, replacement)?;
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
        plan.observed
            .insert(relative.clone(), capture.content.clone());
        let content = if replacement.is_some() {
            rewritten
                .to_markdown()
                .map_err(|error| fail("repair references", error))?
        } else {
            capture.content.unwrap_or_default()
        };
        plan.files.insert(relative, Some(content));
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
        if !matches(&runtime, &reference, &target) {
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
                .content,
        );
        plan.files.insert(
            path,
            Some(toml::to_string_pretty(&manifest).map_err(|error| fail("repair default", error))?),
        );
        if replacement.is_none() {
            plan.warnings
                .push("The default-agent selection in this repository will be cleared.".into());
        }
    }
    Ok(())
}
