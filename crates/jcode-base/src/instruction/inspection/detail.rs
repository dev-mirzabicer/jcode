use super::catalog::selector;
use super::*;

impl InstructionInspector {
    pub(super) fn detail(
        &self,
        target: &InstructionInspectionTarget,
        view: InstructionInspectionView,
        revision: Option<&InstructionRevisionSelection>,
        cancel: &AtomicBool,
    ) -> Result<(String, String)> {
        if let Some(revision) = revision {
            return self.revision(target, revision);
        }
        match target {
            InstructionInspectionTarget::Session => {
                let text = if view == InstructionInspectionView::System {
                    self.context
                        .stored_system
                        .as_deref()
                        .ok_or_else(|| {
                            fail(
                                "inspect stored system",
                                "This session has no stored system prompt",
                            )
                        })?
                        .to_string()
                } else {
                    self.session_metadata()
                };
                Ok((
                    "Current session (captured at open; refresh to recapture)".into(),
                    text,
                ))
            }
            InstructionInspectionTarget::Repository(key) => {
                let store = self.store(key)?;
                let text = if view == InstructionInspectionView::WorkingDiff {
                    let repository = store.reference.as_ref().ok_or_else(|| {
                        fail(
                            "working diff",
                            "External sources have no shared Git repository",
                        )
                    })?;
                    self.repositories
                        .working_diff(repository, None)
                        .map_err(|error| fail("working diff", error))?
                } else {
                    format!("{}\n{}\n\n{}", store.row.kind, store.row.root, store.detail)
                };
                Ok((format!("Repository: {}", store.row.kind), text))
            }
            InstructionInspectionTarget::Resource(key) => {
                let resource = self.resource(key)?;
                let title = format!(
                    "{}:{} · {} · {view:?}",
                    resource.row.scope,
                    resource.row.id,
                    resource.path.display()
                );
                let runtime = InstructionRuntime::discover(self.sources.clone());
                canceled(cancel)?;
                let text = match view {
                    InstructionInspectionView::Source => self.read_source(resource)?,
                    InstructionInspectionView::Metadata => self.metadata(resource, &runtime),
                    InstructionInspectionView::Rendered => self.render(resource, &runtime)?,
                    InstructionInspectionView::System => {
                        let managed = resource
                            .managed
                            .as_ref()
                            .filter(|value| value.kind == InstructionKind::Agent)
                            .ok_or_else(|| {
                                fail(
                                    "system preview",
                                    "Select an agent for full system composition",
                                )
                            })?;
                        SystemPromptComposer::from_repository_service(self.repositories.clone())
                            .preview(SystemPromptActivationRequest {
                                working_dir: self.context.working_dir.as_deref(),
                                selection: AgentSelection::Explicit(selector(managed)),
                                is_selfdev: self.context.is_selfdev,
                                capabilities: self.context.capabilities,
                                available_skills: &self.skills,
                            })
                            .map_err(|error| fail("full system preview (no activation)", error))?
                            .state
                            .text
                    }
                    InstructionInspectionView::Dependencies => {
                        self.dependencies(resource, &runtime, cancel)?
                    }
                    InstructionInspectionView::WorkingDiff => {
                        let (repository, path, _) = self.git_target(target)?;
                        let mut patch = self
                            .repositories
                            .working_diff(repository, path.as_deref())
                            .map_err(|error| fail("working diff", error))?;
                        if self
                            .store(&resource.row.repository)?
                            .state
                            .as_ref()
                            .is_some_and(|state| {
                                state.changes.iter().any(|change| {
                                    Some(&change.path) == path.as_ref()
                                        && change.worktree == Some(GitDelta::Untracked)
                                })
                            })
                        {
                            patch.push_str("\nUntracked file (not represented by HEAD diff):\n");
                            patch.push_str(&self.read_source(resource)?);
                        }
                        patch
                    }
                    InstructionInspectionView::ScopeComparison => {
                        self.scope_comparison(resource)?
                    }
                    InstructionInspectionView::History => {
                        return Err(fail("history", "Use the history page operation"));
                    }
                };
                Ok((title, text))
            }
        }
    }

    fn read_source(&self, resource: &Resource) -> Result<String> {
        if resource.row.origin == InstructionOrigin::Managed {
            let store = self.store(&resource.row.repository)?;
            let repository = store
                .reference
                .as_ref()
                .ok_or_else(|| fail("read source", "Managed repository unavailable"))?;
            let path = resource
                .path
                .strip_prefix(&repository.root)
                .map_err(|error| fail("read source", error))?;
            return self
                .repositories
                .read_file(repository, path, InstructionReadPolicy::WorkingTreeOnly)
                .map(|file| file.content)
                .map_err(|error| fail("read complete source", error));
        }
        let metadata = std::fs::metadata(&resource.path).map_err(|error| {
            fail(
                "read external source",
                format!("{}: {error}", resource.path.display()),
            )
        })?;
        if !metadata.is_file() {
            return Err(fail("read external source", "Source is not a regular file"));
        }
        std::fs::read_to_string(&resource.path).map_err(|error| {
            fail(
                "read complete UTF-8 source",
                format!("{}: {error}", resource.path.display()),
            )
        })
    }

    fn session_metadata(&self) -> String {
        let mut text = format!(
            "Session: {}\nActive agent: {}\nProject: {}\nSelf-development composition: {}\n\nDefault precedence: explicit selection, project default, global default, effective unqualified jcode.\nResume uses exact stored instructions, not this preview. No provider request or activation occurs here.\n",
            self.context.session_id,
            self.context.active_agent.as_deref().unwrap_or("none"),
            self.context
                .working_dir
                .as_deref()
                .map_or_else(|| "none".into(), |path| path.display().to_string()),
            self.context.is_selfdev
        );
        for store in self
            .stores
            .values()
            .filter_map(|store| store.reference.as_ref())
            .filter(|store| !store.id.starts_with("external:"))
        {
            match self.repositories.load_manifest(store) {
                Ok(manifest) => text.push_str(&format!(
                    "\n{} default: {:?}\nSchema: {}  Seed: {}\n",
                    store.kind,
                    manifest.default_agent,
                    manifest.schema_version,
                    manifest.seed_version
                )),
                Err(error) => text.push_str(&format!("\n{} settings error: {error}\n", store.kind)),
            }
        }
        text
    }

    fn metadata(&self, resource: &Resource, runtime: &InstructionRuntime) -> String {
        let row = &resource.row;
        let mut text = format!(
            "ID: {}\nName: {}\nKind: {}\nScope: {}\nOrigin: {:?}\nRepository: {}\nPath: {}\nEffective at catalog capture: {}\nValidation at catalog capture: {}\n{}\n",
            row.id,
            row.name,
            row.kind,
            row.scope,
            row.origin,
            row.repository,
            resource.path.display(),
            row.effective,
            row.warning.as_deref().unwrap_or("valid"),
            resource.annotation
        );
        if let Some(managed) = &resource.managed {
            match runtime.resolve(&selector(managed)) {
                Ok(document) => {
                    text.push_str(&format!("Template: {:?}\nDescription: {}\nAvailability: {:?}\nAddendum target: {}\nIncludes: {}\nAllowed tools: {:?}\n", document.template_mode, document.metadata.description.as_deref().unwrap_or("none"), document.metadata.agent.as_ref().map(|agent| agent.availability), document.metadata.addendum.as_ref().map_or_else(|| "none".into(), |addendum| addendum.target.to_string()), document.metadata.includes.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "), document.metadata.allowed_tools));
                }
                Err(error) => text.push_str(&format!("Current validation error: {error}\n")),
            }
            for consumer in self
                .consumers
                .iter()
                .filter(|consumer| consumer.kind == managed.kind && consumer.id == managed.id)
            {
                text.push_str(&format!("\nConsumer: {}\nDelivery owner: {}\nScope policy: {:?}\nRequired: {}\nEmpty meaningful: {}\n{}\n", consumer.key, consumer.delivery_owner, consumer.scope_policy, consumer.required, consumer.empty_is_meaningful, consumer.inventory_note));
            }
        }
        if let Some(alias) = &resource.alias {
            match self.read_source(resource).and_then(|source| {
                crate::model_roster::ModelRoster::parse(&source)
                    .map_err(|error| fail("parse roster", error))
            }) {
                Ok(roster) => match roster.inspect(alias) {
                    Ok(entry) => {
                        text.push_str(&serde_json::to_string_pretty(entry).unwrap_or_default())
                    }
                    Err(error) => text.push_str(&error.to_string()),
                },
                Err(error) => text.push_str(&error.detail),
            }
        }
        match self.read_source(resource) {
            Ok(source) => {
                text.push_str("\nComplete frontmatter (exact source):\n");
                text.push_str(
                    frontmatter(&source)
                        .unwrap_or("No YAML frontmatter. Source view retains the complete file."),
                );
            }
            Err(error) => text.push_str(&format!("\nSource error: {}", error.detail)),
        }
        text
    }

    fn render(&self, resource: &Resource, runtime: &InstructionRuntime) -> Result<String> {
        if let Some(alias) = &resource.alias {
            let roster = crate::model_roster::ModelRoster::parse(&self.read_source(resource)?)
                .map_err(|error| fail("parse roster", error))?;
            let catalog = self.context.roster_catalog.as_deref().ok_or_else(|| fail("preview roster", "Current route catalog unavailable; no inference or authentication refresh was attempted"))?;
            let availability = roster
                .availability(alias, catalog)
                .map_err(|error| fail("preview availability", error))?;
            let resolution = roster.preview(
                &crate::model_roster::ModelRosterRequest::alias(alias),
                catalog,
            );
            return Ok(format!(
                "Availability (catalog/constructor evidence, not quota or inference):\n{}\n\nResolution:\n{}",
                serde_json::to_string_pretty(&availability)
                    .map_err(|error| fail("format availability", error))?,
                match resolution {
                    Ok(preview) => serde_json::to_string_pretty(&preview)
                        .map_err(|error| fail("format resolution", error))?,
                    Err(error) => error.to_string(),
                }
            ));
        }
        if let Some(managed) = &resource.managed {
            if managed.kind == InstructionKind::Agent {
                return SystemPromptComposer::from_repository_service(self.repositories.clone())
                    .preview_agent_component(
                        self.context.working_dir.as_deref(),
                        AgentSelection::Explicit(selector(managed)),
                    )
                    .map_err(|error| fail("effective agent component", error));
            }
            return runtime
                .render(&selector(managed), &serde_json::json!({}))
                .map(|rendered| rendered.text)
                .map_err(|error| {
                    fail(
                        "render preview (occurrence-specific values are not available)",
                        error,
                    )
                });
        }
        let source = self.read_source(resource)?;
        if resource.row.kind == "skill" {
            return crate::skill::SkillRegistry::parse_skill_source(&resource.path, &source)
                .map(|skill| skill.content)
                .map_err(|error| fail("preview external skill", error));
        }
        Ok(source)
    }

    fn dependencies(
        &self,
        resource: &Resource,
        runtime: &InstructionRuntime,
        cancel: &AtomicBool,
    ) -> Result<String> {
        let managed = resource.managed.as_ref().ok_or_else(|| {
            fail(
                "dependencies",
                "This external or policy resource has no managed instruction graph",
            )
        })?;
        let mut text = String::from(
            "Rendered dependencies and validation-only relationships are distinct.\n\n",
        );
        match runtime.validate_graph(&selector(managed)) {
            Ok(graph) => {
                for (owner, dependencies) in &graph.render_dependencies {
                    for dependency in dependencies {
                        text.push_str(&format!("RENDER  {owner} -> {dependency}\n"));
                    }
                }
                for (owner, dependencies) in &graph.validation_dependencies {
                    for dependency in dependencies {
                        text.push_str(&format!("VALIDATE ONLY  {owner} -> {dependency}\n"));
                    }
                }
            }
            Err(error) => text.push_str(&format!("Validation error: {error}\n")),
        }
        text.push_str("\nReverse resource consumers:\n");
        for other in runtime.resources() {
            canceled(cancel)?;
            if let Ok(graph) = runtime.validate_graph(&selector(&other.resource))
                && let Some(consumers) = graph.reverse_consumers.get(managed)
            {
                for consumer in consumers {
                    text.push_str(&format!("{consumer}\n"));
                }
            }
        }
        text.push_str("\nRegistered code consumers:\n");
        for consumer in &self.consumers {
            let applies = match consumer.scope_policy {
                ConsumerScopePolicy::GlobalOnly => managed.scope == InstructionScope::Global,
                ConsumerScopePolicy::ProjectOnly => managed.scope == InstructionScope::Project,
                ConsumerScopePolicy::ProjectThenGlobal => true,
            };
            if applies && consumer.id == managed.id && consumer.kind == managed.kind {
                text.push_str(&format!(
                    "{}: {} ({:?})\n{}\n",
                    consumer.key,
                    consumer.delivery_owner,
                    consumer.scope_policy,
                    consumer.inventory_note
                ));
            }
        }
        Ok(text)
    }

    fn scope_comparison(&self, resource: &Resource) -> Result<String> {
        let managed = resource.managed.as_ref().ok_or_else(|| {
            fail(
                "scope comparison",
                "Select a managed instruction with global/project specificity",
            )
        })?;
        let mut text = String::new();
        for scope in [InstructionScope::Global, InstructionScope::Project] {
            text.push_str(&format!(
                "\n===== {scope}:{}:{} =====\n",
                managed.kind, managed.id
            ));
            let candidates = self
                .resources
                .values()
                .filter(|other| {
                    other.managed.as_ref().is_some_and(|reference| {
                        reference.scope == scope
                            && reference.kind == managed.kind
                            && reference.id == managed.id
                    })
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                text.push_str("Absent\n");
            }
            for other in candidates {
                text.push_str(&format!("Path: {}\n", other.path.display()));
                match self.read_source(other) {
                    Ok(source) => text.push_str(&source),
                    Err(error) => text.push_str(&error.detail),
                }
            }
        }
        Ok(text)
    }

    fn git_target(
        &self,
        target: &InstructionInspectionTarget,
    ) -> Result<(&InstructionRepositoryRef, Option<PathBuf>, &str)> {
        let (key, path) = match target {
            InstructionInspectionTarget::Resource(key) => {
                let resource = self.resource(key)?;
                (&resource.row.repository, Some(&resource.path))
            }
            InstructionInspectionTarget::Repository(key) => (key, None),
            InstructionInspectionTarget::Session => {
                return Err(fail(
                    "history",
                    "Session prompt is stored state, not an instruction Git revision",
                ));
            }
        };
        let store = self.store(key)?;
        let repository = store
            .reference
            .as_ref()
            .ok_or_else(|| fail("history", "Source has no Git repository"))?;
        let path = path
            .map(|path| path.strip_prefix(&repository.root).map(Path::to_path_buf))
            .transpose()
            .map_err(|error| fail("history path", error))?;
        let head = store
            .state
            .as_ref()
            .and_then(|state| state.head.as_deref())
            .ok_or_else(|| fail("history", "Repository has no readable HEAD"))?;
        Ok((repository, path, head))
    }

    pub(super) fn history(
        &self,
        target: &InstructionInspectionTarget,
        offset: usize,
    ) -> Result<InstructionHistoryPage> {
        let (repository, path, head) = self.git_target(target)?;
        let mut entries = self
            .repositories
            .history_page(repository, path.as_deref(), head, offset, ROW_PAGE_SIZE + 1)
            .map_err(|error| fail("history page", error))?;
        let next = (entries.len() > ROW_PAGE_SIZE).then_some(offset.saturating_add(ROW_PAGE_SIZE));
        entries.truncate(ROW_PAGE_SIZE);
        Ok(InstructionHistoryPage {
            offset,
            next,
            commits: entries
                .into_iter()
                .map(|entry| InstructionCommitRow {
                    commit: entry.commit,
                    author: format!("{} <{}>", entry.author_name, entry.author_email),
                    date: entry.authored_at,
                    subject: entry.subject,
                    paths: entry
                        .changed_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                })
                .collect(),
        })
    }

    pub(super) fn revision(
        &self,
        target: &InstructionInspectionTarget,
        revision: &InstructionRevisionSelection,
    ) -> Result<(String, String)> {
        let (repository, path, _) = self.git_target(target)?;
        let text = if let Some(to) = &revision.to {
            self.repositories
                .compare_revisions(repository, &revision.from, to, path.as_deref())
                .map_err(|error| fail("compare revisions", error))?
                .patch
        } else if let Some(path) = path {
            self.repositories
                .content_at_revision(repository, &revision.from, path)
                .map_err(|error| fail("read revision", error))?
                .content
        } else {
            let entries = self
                .repositories
                .history_page(repository, None, &revision.from, 0, 1)
                .map_err(|error| fail("revision metadata", error))?;
            let entry = entries
                .first()
                .ok_or_else(|| fail("revision metadata", "Commit not found"))?;
            let mut text = format!(
                "{}\n{} <{}>\n{}\n{}\n\nFiles:\n{}",
                entry.commit,
                entry.author_name,
                entry.author_email,
                entry.authored_at,
                entry.subject,
                entry
                    .changed_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            if let Some(parent) = entry.parents.first() {
                text.push_str(
                    &self
                        .repositories
                        .compare_revisions(repository, parent, &entry.commit, None)
                        .map_err(|error| fail("revision diff", error))?
                        .patch,
                );
            }
            text
        };
        Ok((
            format!(
                "Git revision {}{}",
                revision.from,
                revision
                    .to
                    .as_ref()
                    .map_or_else(String::new, |to| format!(" -> {to}"))
            ),
            text,
        ))
    }
}

fn frontmatter(source: &str) -> Option<&str> {
    let mut offset = 0;
    let mut lines = source.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    offset += first.len();
    for line in lines {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(&source[..offset]);
        }
    }
    Some(source)
}
