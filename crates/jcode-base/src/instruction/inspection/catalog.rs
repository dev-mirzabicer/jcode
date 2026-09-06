use super::*;
use crate::skill::{SkillCatalogState, SkillRegistry};

impl InstructionInspector {
    pub(super) fn collect(
        repositories: InstructionRepositoryService,
        context: InspectionContext,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let global = repositories
            .global_repository()
            .map_err(|error| fail("discover global store", error))?;
        let project_result = context
            .working_dir
            .as_deref()
            .map(|path| repositories.resolve_project_repository(path))
            .transpose();
        let project_error = project_result.as_ref().err().map(ToString::to_string);
        let project = project_result.ok().flatten().flatten();
        let root_result = context
            .working_dir
            .as_deref()
            .map(|path| repositories.resolve_project_root(path))
            .transpose();
        let root_error = root_result.as_ref().err().map(ToString::to_string);
        let project_root = root_result.ok().flatten();
        let mut sources = repositories
            .instruction_sources(project.as_ref())
            .map_err(|error| fail("discover sources", error))?;
        sources.global_agents_md = repositories.global_agents_path().ok();
        sources.project_agents_md = project_root.as_ref().map(|root| root.join("AGENTS.md"));
        let runtime = InstructionRuntime::discover(sources.clone());
        let mut consumers = notification::registrations()
            .map_err(|error| fail("list notification consumers", error))?;
        consumers.extend(
            workflow::registrations().map_err(|error| fail("list workflow consumers", error))?,
        );
        consumers.extend(
            composition_registrations()
                .map_err(|error| fail("list composition consumers", error))?,
        );
        let mut inspector = Self {
            repositories,
            context,
            snapshot: uuid::Uuid::new_v4().to_string(),
            stores: BTreeMap::new(),
            resources: BTreeMap::new(),
            sources,
            runtime,
            skills: Vec::new(),
            consumers,
            document: None,
        };
        inspector.add_store(global.clone(), false);
        if let Some(project) = project {
            inspector.add_store(project, false);
        }
        inspector.stores.insert("external".into(), Repository {
            row: InstructionRepositoryRow { key: "external".into(), kind: "legacy / external".into(), root: "Dedicated ecosystem and compatibility sources".into(), branch: None, detached: false, health: "Read-only sources".into(), dirty: false, conflicts: 0, active_lease: false },
            reference: None, state: None, detail: "External files are not managed commits. External skills offer Copy in WP-10. AGENTS.md remains a dedicated ecosystem input. No parent project is committed by Jcode.".into(),
        });
        for error in project_error.into_iter().chain(root_error) {
            let path = inspector
                .context
                .working_dir
                .clone()
                .unwrap_or_default()
                .join(".jcode/instructions.toml");
            inspector.add_external(
                path,
                "project",
                "configuration",
                InstructionOrigin::External,
                false,
                Some(error),
            );
        }
        let summaries = inspector.runtime.resources();
        for summary in &summaries {
            canceled(cancel)?;
            let resource = &summary.resource;
            let selector = selector(resource);
            let mut graph_error = inspector
                .runtime
                .validate_graph(&selector)
                .err()
                .map(|error| error.to_string());
            for consumer in inspector
                .consumers
                .iter()
                .filter(|consumer| consumer.kind == resource.kind && consumer.id == resource.id)
            {
                let mut scoped = consumer.clone();
                scoped.scope_policy = if resource.scope == InstructionScope::Global {
                    ConsumerScopePolicy::GlobalOnly
                } else {
                    ConsumerScopePolicy::ProjectOnly
                };
                if let Err(error) = inspector.runtime.validate_registered_graph(&scoped) {
                    graph_error = Some(error.to_string());
                }
            }
            let warning = match &summary.state {
                ResourceValidationState::Valid => graph_error,
                ResourceValidationState::Invalid(error) => Some(error.clone()),
                ResourceValidationState::Ambiguous => {
                    Some("Ambiguous identity: multiple files define this scoped resource".into())
                }
            };
            let doc = inspector.runtime.resolve(&selector).ok();
            let name = doc
                .and_then(|doc| doc.metadata.display_name.clone())
                .unwrap_or_else(|| resource.id.to_string());
            let paired = paired_composition_resource(resource.kind, &resource.id);
            let effective = paired
                || resource.scope == InstructionScope::Project
                || !summaries.iter().any(|other| {
                    other.resource.scope == InstructionScope::Project
                        && other.resource.kind == resource.kind
                        && other.resource.id == resource.id
                });
            for path in &summary.paths {
                let key = format!("{}:{}", resource, path.display());
                let repository = inspector.repository_for(path);
                let mut annotation = if paired {
                    "Additive paired source: global and project both contribute independently.\n"
                        .into()
                } else {
                    String::new()
                };
                if !paired
                    && resource.scope == InstructionScope::Project
                    && summaries.iter().any(|other| {
                        other.resource.scope == InstructionScope::Global
                            && other.resource.kind == resource.kind
                            && other.resource.id == resource.id
                    })
                {
                    annotation.push_str("Project redefinition: unqualified references select this source instead of global. Explicit global references remain available.\n");
                    if matches!(
                        resource.kind,
                        InstructionKind::System
                            | InstructionKind::Agent
                            | InstructionKind::Notification
                    ) {
                        annotation.push_str("HIGH IMPACT: redefines system, profile, or control prose. User-authorized redefinition is allowed.\n");
                    }
                }
                if resource.kind == InstructionKind::AgentAddendum {
                    annotation.push_str("Explicit agent addendum. Target is a validation-only relationship, not an implicit render include.\n");
                }
                inspector.resources.insert(
                    key.clone(),
                    Resource {
                        row: InstructionRow {
                            key,
                            id: resource.id.to_string(),
                            name: name.clone(),
                            kind: resource.kind.to_string(),
                            scope: resource.scope.to_string(),
                            repository,
                            origin: InstructionOrigin::Managed,
                            effective,
                            valid: warning.is_none(),
                            warning: warning.clone(),
                        },
                        path: path.clone(),
                        managed: Some(resource.clone()),
                        alias: None,
                        annotation,
                    },
                );
            }
        }
        // Preserve unidentified failures too, not just resolvable identities.
        for diagnostic in inspector.runtime.diagnostics().to_vec() {
            if !inspector
                .resources
                .values()
                .any(|resource| resource.path == diagnostic.path)
            {
                inspector.add_external(
                    diagnostic.path,
                    &diagnostic.scope.to_string(),
                    "invalid-resource",
                    InstructionOrigin::Managed,
                    false,
                    Some(diagnostic.detail),
                );
            }
        }
        for (scope, path) in [
            ("global", inspector.sources.global_agents_md.clone()),
            ("project", inspector.sources.project_agents_md.clone()),
        ] {
            if let Some(path) = path
                && std::fs::symlink_metadata(&path).is_ok()
            {
                inspector.add_external(
                    path,
                    scope,
                    "AGENTS.md",
                    InstructionOrigin::External,
                    true,
                    None,
                );
            }
        }
        inspector.collect_skills(cancel)?;
        inspector.collect_legacy(project_root.as_deref(), cancel)?;
        inspector.collect_roster(&global);
        // Repository metadata is inspectable, including damaged/absent manifests.
        let manifests = inspector
            .stores
            .values()
            .filter_map(|store| store.reference.as_ref())
            .filter(|store| !store.id.starts_with("external:"))
            .cloned()
            .collect::<Vec<_>>();
        for repository in manifests {
            let scope = if repository.kind == InstructionRepositoryKind::Global {
                "global"
            } else {
                "project"
            };
            if scope == "project"
                && std::fs::symlink_metadata(repository.root.join(crate::model_roster::ROSTER_PATH))
                    .is_ok()
            {
                inspector.add_external(
                    repository.root.join(crate::model_roster::ROSTER_PATH),
                    scope,
                    "model-roster",
                    InstructionOrigin::Managed,
                    false,
                    Some(
                        "Model roster is global-only. Project aliases are not active policy."
                            .into(),
                    ),
                );
            }
            inspector.add_external(
                repository.root.join("instruction-store.toml"),
                scope,
                "store-settings",
                InstructionOrigin::Managed,
                true,
                None,
            );
        }
        // Required singletons remain visible after deletion, without restoring them.
        for registration in inspector.consumers.clone() {
            if registration.required
                && !inspector.resources.values().any(|resource| {
                    resource.managed.as_ref().is_some_and(|resource| {
                        resource.scope == InstructionScope::Global
                            && resource.kind == registration.kind
                            && resource.id == registration.id
                    })
                })
            {
                inspector.add_external(
                    global.root.join(&registration.default_relative_path),
                    "global",
                    &registration.kind.to_string(),
                    InstructionOrigin::Managed,
                    false,
                    Some(format!(
                        "Required resource missing for {} ({})",
                        registration.key, registration.delivery_owner
                    )),
                );
                if let Some(resource) = inspector.resources.values_mut().find(|resource| {
                    resource.path == global.root.join(&registration.default_relative_path)
                }) {
                    resource.row.id = registration.id.to_string();
                    resource.managed = Some(InstructionResourceRef {
                        scope: InstructionScope::Global,
                        kind: registration.kind,
                        id: registration.id.clone(),
                    });
                    resource.annotation = registration.inventory_note.clone();
                }
            }
        }
        canceled(cancel)?;
        Ok(inspector)
    }

    fn add_store(&mut self, reference: InstructionRepositoryRef, external: bool) {
        let result = self.repositories.inspect(&reference);
        self.insert_store(reference, result, external);
    }

    fn insert_store(
        &mut self,
        reference: InstructionRepositoryRef,
        result: std::result::Result<InstructionRepositoryState, InstructionRepositoryError>,
        external: bool,
    ) {
        let state = result.as_ref().ok();
        let key = reference.id.clone();
        let health = state
            .map(|state| match &state.health {
                InstructionRepositoryHealth::Ready => "Ready".into(),
                InstructionRepositoryHealth::Uninitialized => {
                    "Uninitialized (inspection does not initialize)".into()
                }
                InstructionRepositoryHealth::Damaged(damage) => {
                    format!("Damaged: {}", damage.detail)
                }
            })
            .unwrap_or_else(|| {
                format!(
                    "Inspection error: {}",
                    result.as_ref().expect_err("failed state")
                )
            });
        let detail = match &result {
            Ok(state) => {
                serde_json::to_string_pretty(state).unwrap_or_else(|error| error.to_string())
            }
            Err(error) => error.to_string(),
        };
        self.stores.insert(
            key.clone(),
            Repository {
                row: InstructionRepositoryRow {
                    key,
                    kind: if external {
                        "external source Git repository".into()
                    } else {
                        reference.kind.to_string()
                    },
                    root: reference.root.display().to_string(),
                    branch: state.and_then(|state| state.branch.clone()),
                    detached: state.is_some_and(|state| state.detached),
                    health,
                    dirty: state.is_some_and(|state| !state.changes.is_empty()),
                    conflicts: state.map_or(0, |state| state.conflicts.len()),
                    active_lease: state.is_some_and(|state| state.active_mutation.is_some()),
                },
                reference: Some(reference),
                state: result.ok(),
                detail,
            },
        );
    }

    fn repository_for(&mut self, path: &Path) -> String {
        if let Some(store) = self
            .stores
            .values()
            .filter(|store| {
                store
                    .reference
                    .as_ref()
                    .is_some_and(|reference| path.starts_with(&reference.root))
            })
            .max_by_key(|store| {
                store
                    .reference
                    .as_ref()
                    .map(|reference| reference.root.components().count())
            })
        {
            return store.row.key.clone();
        }
        match self.repositories.inspect_external_source(path) {
            Ok(Some((reference, state))) => {
                let key = reference.id.clone();
                self.insert_store(reference, Ok(state), true);
                key
            }
            _ => "external".into(),
        }
    }

    fn add_external(
        &mut self,
        path: PathBuf,
        scope: &str,
        kind: &str,
        origin: InstructionOrigin,
        effective: bool,
        warning: Option<String>,
    ) {
        let key = format!("{scope}:{kind}:{}", path.display());
        let repository = self.repository_for(&path);
        let read_error = std::fs::symlink_metadata(&path)
            .and_then(|metadata| {
                if origin == InstructionOrigin::Managed && metadata.file_type().is_symlink() {
                    return Err(std::io::Error::other("Managed source is a symlink"));
                }
                if !std::fs::metadata(&path)?.is_file() {
                    return Err(std::io::Error::other("Source is not a regular file"));
                }
                std::fs::read_to_string(&path)
            })
            .err()
            .map(|error| error.to_string());
        let warning = warning.or(read_error);
        self.resources.insert(
            key.clone(),
            Resource {
                row: InstructionRow {
                    key,
                    id: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into(),
                    name: path.display().to_string(),
                    kind: kind.into(),
                    scope: scope.into(),
                    repository,
                    origin,
                    effective,
                    valid: warning.is_none(),
                    warning,
                },
                path,
                managed: None,
                alias: None,
                annotation: String::new(),
            },
        );
    }

    fn collect_skills(&mut self, cancel: &AtomicBool) -> Result<()> {
        let base = match SkillRegistry::inspect_global_sources() {
            Ok(base) => base,
            Err(error) => {
                let path = self
                    .repositories
                    .global_repository()
                    .map_err(|error| fail("discover skills", error))?
                    .root
                    .join("../skills");
                self.add_external(
                    path,
                    "global",
                    "skill",
                    InstructionOrigin::External,
                    false,
                    Some(error.to_string()),
                );
                SkillRegistry::default()
            }
        };
        let registry = SkillRegistry::effective_for_working_dir_with_repositories(
            &base,
            self.context.working_dir.as_deref(),
            &self.repositories,
        );
        self.skills = registry
            .list()
            .iter()
            .map(|skill| SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();
        for entry in registry.catalog_entries() {
            canceled(cancel)?;
            let path = entry.source.package_root.join("SKILL.md");
            if let Some(resource) = self
                .resources
                .values_mut()
                .find(|resource| resource.path == path)
            {
                resource.row.effective = entry.effective;
                resource.annotation.push_str(&format!(
                    "Skill invocation name: {}\nSource: {}\n",
                    entry.name, entry.source.kind
                ));
                continue;
            }
            let warning = match entry.state {
                SkillCatalogState::Valid => None,
                SkillCatalogState::Invalid(error) => Some(error),
            };
            self.add_external(
                path.clone(),
                &entry.source.kind.scope().to_string(),
                "skill",
                if entry.source.kind.is_managed() {
                    InstructionOrigin::Managed
                } else {
                    InstructionOrigin::External
                },
                entry.effective,
                warning,
            );
            if let Some(resource) = self
                .resources
                .values_mut()
                .find(|resource| resource.path == path)
            {
                resource.row.name = entry.name;
                resource.annotation = format!(
                    "Source: {}\nExternal skill: read-only. Copy to managed global/project store is available in WP-10, not executed by this inspector.\n",
                    entry.source.kind
                );
            }
        }
        let external_diagnostics =
            SkillRegistry::inspection_diagnostics(self.context.working_dir.as_deref());
        for diagnostic in registry.diagnostics().iter().chain(&external_diagnostics) {
            let path = diagnostic
                .source
                .as_ref()
                .map(|source| source.package_root.join("SKILL.md"))
                .unwrap_or_else(|| {
                    self.context
                        .working_dir
                        .clone()
                        .unwrap_or_default()
                        .join("skills")
                });
            if !self
                .resources
                .values()
                .any(|resource| resource.path == path && !resource.row.valid)
            {
                self.add_external(
                    path,
                    diagnostic.source.as_ref().map_or("global", |source| {
                        if source.kind.scope() == InstructionScope::Global {
                            "global"
                        } else {
                            "project"
                        }
                    }),
                    "skill",
                    InstructionOrigin::External,
                    false,
                    Some(diagnostic.detail.clone()),
                );
            }
        }
        Ok(())
    }

    fn collect_legacy(&mut self, project_root: Option<&Path>, cancel: &AtomicBool) -> Result<()> {
        let global = self
            .repositories
            .global_repository()
            .map_err(|error| fail("legacy sources", error))?;
        let mut roots = vec![(
            InstructionScope::Global,
            global.root.parent().unwrap_or(&global.root).to_path_buf(),
        )];
        if let Some(root) = project_root {
            roots.push((InstructionScope::Project, root.join(".jcode")));
        }
        for (scope, root) in roots {
            for (name, kind) in [
                (
                    "system-prompt.md",
                    LegacyInstructionSourceKind::SystemPrompt,
                ),
                (
                    "prompt-overlay.md",
                    LegacyInstructionSourceKind::PromptOverlay,
                ),
                (
                    "preferred-tools.md",
                    LegacyInstructionSourceKind::PreferredTools,
                ),
                ("swarm-prompt.md", LegacyInstructionSourceKind::SwarmPrompt),
            ] {
                canceled(cancel)?;
                let path = root.join(name);
                if std::fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                let manifest = self
                    .stores
                    .values()
                    .filter_map(|store| store.reference.as_ref())
                    .find(|repository| {
                        (repository.kind == InstructionRepositoryKind::Global)
                            == (scope == InstructionScope::Global)
                            && !repository.id.starts_with("external:")
                    })
                    .and_then(|repository| self.repositories.load_manifest(repository).ok());
                let imported = manifest.as_ref().is_some_and(|manifest| {
                    manifest
                        .legacy_imports
                        .values()
                        .any(|receipt| receipt.source_kind == kind)
                });
                let regular = std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file());
                let blank = regular
                    && std::fs::read_to_string(&path).is_ok_and(|text| text.trim().is_empty());
                let eligibility = legacy_source_eligibility(&self.runtime, scope, kind, imported);
                let (eligible, activity) = eligibility.as_ref().copied().unwrap_or((false, "Managed target is invalid. It does not silently expose this compatibility file."));
                self.add_external(
                    path.clone(),
                    &scope.to_string(),
                    "legacy-prompt",
                    InstructionOrigin::Legacy,
                    regular && eligible && !blank,
                    eligibility.err().map(|error| error.to_string()),
                );
                if let Some(resource) = self
                    .resources
                    .values_mut()
                    .find(|resource| resource.path == path)
                {
                    resource.annotation = if blank { "Blank legacy input is inactive; managed empty bodies have distinct semantics." } else { activity }.into();
                }
            }
        }
        Ok(())
    }

    fn collect_roster(&mut self, global: &InstructionRepositoryRef) {
        let path = global.root.join(crate::model_roster::ROSTER_PATH);
        let parsed = self
            .repositories
            .read_file(
                global,
                crate::model_roster::ROSTER_PATH,
                InstructionReadPolicy::WorkingTreeOnly,
            )
            .map_err(|error| error.to_string())
            .and_then(|file| {
                crate::model_roster::ModelRoster::parse(&file.content)
                    .map_err(|error| error.to_string())
            });
        self.add_external(
            path.clone(),
            "global",
            "model-roster",
            InstructionOrigin::Managed,
            true,
            parsed.as_ref().err().cloned().or_else(|| {
                parsed
                    .as_ref()
                    .ok()
                    .filter(|roster| !roster.validate().is_empty())
                    .map(|roster| {
                        roster
                            .validate()
                            .iter()
                            .map(|error| error.detail.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            }),
        );
        let Ok(roster) = parsed else {
            return;
        };
        let entries = roster
            .list()
            .into_iter()
            .map(|entry| (entry.alias, None))
            .chain(roster.validate().iter().filter_map(|error| {
                error
                    .alias
                    .clone()
                    .map(|alias| (alias, Some(error.detail.clone())))
            }))
            .collect::<Vec<_>>();
        for (alias, warning) in entries {
            let key = format!("model-roster:{alias}");
            self.resources.insert(key.clone(), Resource {
                row: InstructionRow { key, id: alias.clone(), name: alias.clone(), kind: "model-roster".into(), scope: "global".into(), repository: global.id.clone(), origin: InstructionOrigin::Managed, effective: true, valid: warning.is_none(), warning },
                path: path.clone(), managed: None, alias: Some(alias), annotation: "Global-only launch policy. Preview uses independent provider construction, without inference or modifying the primary provider.".into(),
            });
        }
    }
}

pub(super) fn selector(resource: &InstructionResourceRef) -> InstructionSelector {
    InstructionSelector {
        scope: match resource.scope {
            InstructionScope::Global => InstructionScopeSelector::Global,
            InstructionScope::Project => InstructionScopeSelector::Project,
        },
        kind: resource.kind,
        id: resource.id.clone(),
    }
}
