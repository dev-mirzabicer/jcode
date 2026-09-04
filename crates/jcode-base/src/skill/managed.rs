use super::{
    Skill, SkillCatalogDiagnostic, SkillRegistry, SkillSource, SkillSourceKind,
    build_skill_search_text,
};
use crate::instruction::{
    InstructionCommitDisposition, InstructionCommitRequest, InstructionDocument,
    InstructionFileMutation, InstructionId, InstructionKind, InstructionMetadata,
    InstructionRepositoryError, InstructionRepositoryErrorKind, InstructionRepositoryHealth,
    InstructionRepositoryRef, InstructionRepositoryService, InstructionResourceRef,
    InstructionScope, InstructionSelector, ResourceValidationState, SystemPromptComposer,
    TemplateMode,
};
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub(super) struct ManagedSkillLayers {
    pub(super) global: ManagedSkillLayer,
    pub(super) project: ManagedSkillLayer,
    pub(super) diagnostics: Vec<SkillCatalogDiagnostic>,
}

#[derive(Default)]
pub(super) struct ManagedSkillLayer {
    pub(super) valid: HashMap<String, (Skill, SkillSource)>,
    pub(super) blocked: HashMap<String, SkillCatalogDiagnostic>,
}

impl ManagedSkillLayers {
    pub(super) fn load_with_repositories(
        working_dir: Option<&Path>,
        repositories: &InstructionRepositoryService,
    ) -> Self {
        let mut layers = Self {
            global: ManagedSkillLayer::default(),
            project: ManagedSkillLayer::default(),
            diagnostics: Vec::new(),
        };
        let global = match repositories.global_repository() {
            Ok(repository) => repository,
            Err(error) => {
                layers.diagnostics.push(source_error(error));
                return layers;
            }
        };
        let global_ready = repository_ready(repositories, &global, &mut layers.diagnostics);
        let project = match working_dir {
            Some(working_dir) => match repositories.resolve_project_repository(working_dir) {
                Ok(repository) => repository,
                Err(error) => {
                    layers.diagnostics.push(source_error(error));
                    None
                }
            },
            None => None,
        };
        let project_ready = project.as_ref().is_some_and(|repository| {
            repository_ready(repositories, repository, &mut layers.diagnostics)
        });
        if !global_ready && !project_ready {
            return layers;
        }
        let sources = match repositories.instruction_sources(project.as_ref()) {
            Ok(sources) => sources,
            Err(error) => {
                layers.diagnostics.push(source_error(error));
                return layers;
            }
        };
        let runtime = crate::instruction::InstructionRuntime::discover(sources);
        if global_ready {
            layers.global = load_scope(
                &runtime,
                InstructionScope::Global,
                SkillSourceKind::ManagedGlobal,
            );
        }
        if project_ready {
            layers.project = load_scope(
                &runtime,
                InstructionScope::Project,
                SkillSourceKind::ManagedProject,
            );
        }
        layers
    }
}

fn repository_ready(
    repositories: &InstructionRepositoryService,
    repository: &InstructionRepositoryRef,
    diagnostics: &mut Vec<SkillCatalogDiagnostic>,
) -> bool {
    match repositories.inspect(repository) {
        Ok(state) => match state.health {
            InstructionRepositoryHealth::Ready => true,
            InstructionRepositoryHealth::Uninitialized => false,
            InstructionRepositoryHealth::Damaged(damage) => {
                diagnostics.push(SkillCatalogDiagnostic {
                    name: None,
                    source: Some(SkillSource {
                        kind: if repository.kind
                            == crate::instruction::InstructionRepositoryKind::Global
                        {
                            SkillSourceKind::ManagedGlobal
                        } else {
                            SkillSourceKind::ManagedProject
                        },
                        package_root: repository.root.join("skills"),
                        managed_resource: None,
                    }),
                    detail: damage.detail,
                });
                false
            }
        },
        Err(error) => {
            diagnostics.push(source_error(error));
            false
        }
    }
}

fn load_scope(
    runtime: &crate::instruction::InstructionRuntime,
    scope: InstructionScope,
    source_kind: SkillSourceKind,
) -> ManagedSkillLayer {
    let mut layer = ManagedSkillLayer::default();
    let mut resources = runtime
        .resources()
        .into_iter()
        .filter(|summary| {
            summary.resource.kind == InstructionKind::Skill && summary.resource.scope == scope
        })
        .collect::<Vec<_>>();
    resources.sort_by(|left, right| left.resource.id.cmp(&right.resource.id));
    for summary in resources {
        let path = summary.paths.first().cloned().unwrap_or_default();
        let source = SkillSource {
            kind: source_kind,
            package_root: path.parent().unwrap_or(&path).to_path_buf(),
            managed_resource: Some(summary.resource.clone()),
        };
        match summary.state {
            ResourceValidationState::Valid => {
                let selector = selector_for(&summary.resource);
                let loaded = runtime.resolve(&selector).and_then(|document| {
                    runtime.render(&selector, &()).map(|rendered| {
                        let name = document
                            .metadata
                            .display_name
                            .clone()
                            .unwrap_or_else(|| document.id.to_string());
                        let description = document.metadata.description.clone().unwrap_or_default();
                        Skill {
                            name: name.clone(),
                            description: description.clone(),
                            allowed_tools: document.metadata.allowed_tools.clone(),
                            content: rendered.text.clone(),
                            path: document.path.clone(),
                            search_text: build_skill_search_text(
                                &name,
                                &description,
                                &rendered.text,
                            ),
                        }
                    })
                });
                match loaded {
                    Ok(skill) => insert_layer_skill(&mut layer, skill, source),
                    Err(error) => insert_layer_error(
                        &mut layer,
                        skill_name_hint(&path).unwrap_or_else(|| summary.resource.id.to_string()),
                        source,
                        error.to_string(),
                    ),
                }
            }
            ResourceValidationState::Invalid(detail) => insert_layer_error(
                &mut layer,
                skill_name_hint(&path).unwrap_or_else(|| summary.resource.id.to_string()),
                source,
                detail,
            ),
            ResourceValidationState::Ambiguous => {
                let mut names = summary
                    .paths
                    .iter()
                    .filter_map(|path| skill_name_hint(path))
                    .collect::<BTreeSet<_>>();
                if names.is_empty() {
                    names.insert(summary.resource.id.to_string());
                }
                for name in names {
                    insert_layer_error(
                        &mut layer,
                        name,
                        source.clone(),
                        format!(
                            "managed skill ID '{}' is ambiguous across {} files",
                            summary.resource.id,
                            summary.paths.len()
                        ),
                    );
                }
            }
        }
    }
    layer
}

fn insert_layer_skill(layer: &mut ManagedSkillLayer, skill: Skill, source: SkillSource) {
    let name = skill.name.clone();
    if layer.valid.contains_key(&name) || layer.blocked.contains_key(&name) {
        layer.valid.remove(&name);
        insert_layer_error(
            layer,
            name.clone(),
            source,
            format!("multiple managed skills in one scope use invocation name '{name}'"),
        );
    } else {
        layer.valid.insert(name, (skill, source));
    }
}

fn insert_layer_error(
    layer: &mut ManagedSkillLayer,
    name: String,
    source: SkillSource,
    detail: String,
) {
    layer.valid.remove(&name);
    layer.blocked.insert(
        name.clone(),
        SkillCatalogDiagnostic {
            name: Some(name),
            source: Some(source),
            detail,
        },
    );
}

fn selector_for(resource: &InstructionResourceRef) -> InstructionSelector {
    match resource.scope {
        InstructionScope::Global => {
            InstructionSelector::global(resource.kind, resource.id.to_string())
        }
        InstructionScope::Project => {
            InstructionSelector::project(resource.kind, resource.id.to_string())
        }
    }
    .expect("discovered resource IDs are valid selectors")
}

fn source_error(error: InstructionRepositoryError) -> SkillCatalogDiagnostic {
    SkillCatalogDiagnostic {
        name: None,
        source: None,
        detail: error.to_string(),
    }
}

fn skill_name_hint(path: &Path) -> Option<String> {
    let source = std::fs::read_to_string(path).ok()?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let rest = source
        .strip_prefix("---\r\n")
        .or_else(|| source.strip_prefix("---\n"))?;
    let end = rest.find("\n---\r\n").or_else(|| rest.find("\n---\n"))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&rest[..end]).ok()?;
    value
        .as_mapping()?
        .get(serde_yaml::Value::String("name".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedSkillDestination {
    Global,
    Project,
}

#[derive(Clone, Debug)]
pub struct ManagedSkillCopyRequest<'a> {
    pub skill_name: &'a str,
    pub working_dir: Option<&'a Path>,
    pub destination: ManagedSkillDestination,
    pub destination_id: Option<&'a str>,
    pub operation_id: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedSkillCopyDisposition {
    Created,
    AlreadyCommitted,
    NoChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSkillCopyOutcome {
    pub disposition: ManagedSkillCopyDisposition,
    pub repository: InstructionRepositoryRef,
    pub skill_id: String,
    pub invocation_name: String,
    pub commit: String,
    pub changed_paths: Vec<PathBuf>,
    pub effective_for_source_project: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct SkillSourceAttribution<'a> {
    schema_version: u32,
    source_kind: String,
    source_skill: &'a str,
    source_package_sha256: String,
}

pub fn copy_external_skill(
    repositories: &InstructionRepositoryService,
    registry: &SkillRegistry,
    request: ManagedSkillCopyRequest<'_>,
) -> Result<ManagedSkillCopyOutcome, InstructionRepositoryError> {
    let skill = registry
        .resolve(request.skill_name)
        .map_err(|error| copy_error(error.to_string()))?
        .ok_or_else(|| copy_error(format!("skill '{}' was not found", request.skill_name)))?;
    let source = registry
        .source(request.skill_name)
        .cloned()
        .ok_or_else(|| copy_error("effective skill has no source identity"))?;
    if source.kind.is_managed() {
        return Err(copy_error(format!(
            "skill '{}' is already managed in {} scope",
            request.skill_name,
            source.kind.scope()
        )));
    }

    let repository = destination_repository(repositories, &request)?;
    let skill_id = match request.destination_id {
        Some(id) => {
            InstructionId::parse(id.to_string()).map_err(|error| copy_error(error.to_string()))?
        }
        None => copied_skill_id(skill)?,
    };
    let package_root = PathBuf::from("skills").join(skill_id.as_str());
    let desired = copied_package(skill, &source, &package_root, &skill_id)?;
    let existing = existing_package_files(&repository.root, &package_root)?;
    let head = repositories
        .inspect(&repository)?
        .head
        .ok_or_else(|| copy_error("destination instruction repository has no current HEAD"))?;
    let committed = repositories.files_at_revision_under(&repository, &head, &package_root)?;
    let working_conflicts = existing
        .iter()
        .any(|(path, content)| desired.get(path) != Some(content));
    let committed_has_extra = committed.keys().any(|path| !desired.contains_key(path));
    if working_conflicts || committed_has_extra {
        return Err(copy_collision(&repository, &package_root));
    }
    if existing == desired && committed == desired {
        return Ok(ManagedSkillCopyOutcome {
            disposition: ManagedSkillCopyDisposition::NoChange,
            repository,
            skill_id: skill_id.to_string(),
            invocation_name: skill.name.clone(),
            commit: head,
            changed_paths: Vec::new(),
            effective_for_source_project: destination_is_effective(
                request.destination,
                source.kind,
            ),
        });
    }

    let mut expected_files = Vec::new();
    let mut mutations = Vec::new();
    for (relative_path, content) in &desired {
        let draft = repositories.open_draft(&repository, relative_path)?;
        expected_files.push(draft.base);
        mutations.push(InstructionFileMutation::Write {
            relative_path: relative_path.clone(),
            content: content.clone(),
        });
    }
    let outcome = repositories.commit(
        &repository,
        &InstructionCommitRequest {
            operation_id: request.operation_id.to_string(),
            message: format!("skill: copy {} from {}", skill.name, source.kind),
            expected_head: head,
            expected_files,
            mutations,
        },
    )?;
    Ok(ManagedSkillCopyOutcome {
        disposition: match outcome.disposition {
            InstructionCommitDisposition::Created => ManagedSkillCopyDisposition::Created,
            InstructionCommitDisposition::AlreadyCommitted => {
                ManagedSkillCopyDisposition::AlreadyCommitted
            }
            InstructionCommitDisposition::NoChange => ManagedSkillCopyDisposition::NoChange,
        },
        repository,
        skill_id: skill_id.to_string(),
        invocation_name: skill.name.clone(),
        commit: outcome.commit,
        changed_paths: outcome.changed_paths,
        effective_for_source_project: destination_is_effective(request.destination, source.kind),
    })
}

fn destination_repository(
    repositories: &InstructionRepositoryService,
    request: &ManagedSkillCopyRequest<'_>,
) -> Result<InstructionRepositoryRef, InstructionRepositoryError> {
    match request.destination {
        ManagedSkillDestination::Global => {
            SystemPromptComposer::from_repository_service(repositories.clone())
                .ensure_global_store()
                .map_err(|error| copy_error(error.to_string()))?;
            repositories.global_repository()
        }
        ManagedSkillDestination::Project => {
            let working_dir = request
                .working_dir
                .ok_or_else(|| copy_error("project destination requires a working directory"))?;
            repositories
                .resolve_project_repository(working_dir)?
                .ok_or_else(|| {
                    copy_error(
                        "project has no configured managed instruction repository; set up the project store before Copy skill",
                    )
                })
        }
    }
}

fn copied_skill_id(skill: &Skill) -> Result<InstructionId, InstructionRepositoryError> {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in skill.name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            pending_dash = false;
        } else if !slug.is_empty() {
            pending_dash = true;
        }
    }
    InstructionId::parse(slug).map_err(|error| copy_error(error.to_string()))
}

fn copied_package(
    skill: &Skill,
    source: &SkillSource,
    package_root: &Path,
    skill_id: &InstructionId,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, InstructionRepositoryError> {
    let mut files = BTreeMap::new();
    collect_package(
        &source.package_root,
        &source.package_root,
        package_root,
        &mut files,
    )?;
    let original_skill = files
        .remove(&package_root.join("SKILL.md"))
        .ok_or_else(|| copy_error("external skill package is missing SKILL.md"))?;
    let preserved_source = package_root.join(".jcode-source/original-SKILL.md");
    if files.contains_key(&preserved_source) {
        return Err(copy_error(format!(
            "external skill package already uses reserved Copy metadata path {}",
            preserved_source.display()
        )));
    }
    files.insert(preserved_source, original_skill);
    let document = InstructionDocument {
        id: skill_id.clone(),
        kind: InstructionKind::Skill,
        scope: InstructionScope::Global,
        template_mode: TemplateMode::Plain,
        metadata: InstructionMetadata {
            display_name: Some(skill.name.clone()),
            description: Some(skill.description.clone()),
            allowed_tools: skill.allowed_tools.clone(),
            ..InstructionMetadata::default()
        },
        body: skill.content.clone(),
        path: package_root.join("SKILL.md"),
    };
    files.insert(
        package_root.join("SKILL.md"),
        document
            .to_markdown()
            .map_err(|error| copy_error(error.to_string()))?
            .into_bytes(),
    );
    let attribution = SkillSourceAttribution {
        schema_version: 1,
        source_kind: source.kind.to_string(),
        source_skill: &skill.name,
        source_package_sha256: hash_package(&files),
    };
    files.insert(
        package_root.join(".jcode-source.toml"),
        toml::to_string_pretty(&attribution)
            .map_err(|error| copy_error(format!("serialize skill attribution: {error}")))?
            .into_bytes(),
    );
    Ok(files)
}

fn collect_package(
    root: &Path,
    directory: &Path,
    destination_root: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), InstructionRepositoryError> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        copy_error(format!(
            "read skill package {}: {error}",
            directory.display()
        ))
    })? {
        let entry = entry.map_err(|error| copy_error(format!("read skill entry: {error}")))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            copy_error(format!("inspect skill path {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(copy_error(format!(
                "skill package contains a symlink at {}; Copy requires regular files and directories",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_package(root, &path, destination_root, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| copy_error(format!("resolve skill relative path: {error}")))?;
            files.insert(
                destination_root.join(relative),
                std::fs::read(&path).map_err(|error| {
                    copy_error(format!("read skill file {}: {error}", path.display()))
                })?,
            );
        } else {
            return Err(copy_error(format!(
                "skill package contains unsupported file type at {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn existing_package_files(
    repository_root: &Path,
    package_root: &Path,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, InstructionRepositoryError> {
    let absolute = repository_root.join(package_root);
    if !absolute.exists() {
        return Ok(BTreeMap::new());
    }
    let mut files = BTreeMap::new();
    collect_package(&absolute, &absolute, package_root, &mut files)?;
    Ok(files)
}

fn hash_package(files: &BTreeMap<PathBuf, Vec<u8>>) -> String {
    let mut hasher = Sha256::new();
    for (path, content) in files {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content);
    }
    format!("{:x}", hasher.finalize())
}

fn destination_is_effective(destination: ManagedSkillDestination, source: SkillSourceKind) -> bool {
    destination == ManagedSkillDestination::Project || source.scope() == InstructionScope::Global
}

fn copy_error(detail: impl Into<String>) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::RepositoryUnavailable,
        "copy external skill",
        detail,
    )
}

fn copy_collision(
    repository: &InstructionRepositoryRef,
    package_root: &Path,
) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Conflict,
        "copy external skill",
        format!(
            "managed destination {} contains content outside this external package",
            package_root.display()
        ),
    )
    .repository(repository)
    .path(package_root)
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct AttributionProbe {
    schema_version: u32,
    source_kind: String,
    source_skill: String,
    source_package_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::{InstructionSeedFile, InstructionStoreManifest, InstructionStoreSeed};

    struct Fixture {
        _root: tempfile::TempDir,
        home: PathBuf,
        project: PathBuf,
        service: InstructionRepositoryService,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("skill fixture");
            let home = root.path().join("jcode-home");
            let state = root.path().join("state");
            let project = root.path().join("project");
            std::fs::create_dir_all(&home).expect("home");
            std::fs::create_dir_all(&state).expect("state");
            std::fs::create_dir_all(&project).expect("project");
            let service = InstructionRepositoryService::from_paths(&home, &state);
            SystemPromptComposer::from_repository_service(service.clone())
                .ensure_global_store()
                .expect("initialize global store");
            Self {
                _root: root,
                home,
                project,
                service,
            }
        }

        fn global_repository(&self) -> InstructionRepositoryRef {
            self.service.global_repository().expect("global repository")
        }

        fn external_global(&self, name: &str, description: &str, body: &str) -> PathBuf {
            let root = self.home.join("skills").join(name);
            std::fs::create_dir_all(&root).expect("external global skill dir");
            std::fs::write(
                root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
            )
            .expect("external skill");
            root
        }

        fn external_registry(&self) -> SkillRegistry {
            let mut registry = SkillRegistry::default();
            registry
                .load_from_dir(
                    &self.home.join("skills"),
                    SkillSourceKind::ExternalJcodeGlobal,
                )
                .expect("external registry");
            registry
        }

        fn effective(&self, base: &SkillRegistry) -> SkillRegistry {
            SkillRegistry::effective_for_working_dir_with_repositories(
                base,
                Some(&self.project),
                &self.service,
            )
        }

        fn write_global_document(&self, document: InstructionDocument, operation_id: &str) {
            let repository = self.global_repository();
            let path = document.path.clone();
            let draft = self.service.open_draft(&repository, &path).expect("draft");
            self.service
                .commit(
                    &repository,
                    &InstructionCommitRequest {
                        operation_id: operation_id.to_string(),
                        message: format!("test: write {}", document.id),
                        expected_head: draft.base_head,
                        expected_files: vec![draft.base],
                        mutations: vec![InstructionFileMutation::Write {
                            relative_path: path,
                            content: document.to_markdown().expect("markdown").into_bytes(),
                        }],
                    },
                )
                .expect("commit document");
        }
    }

    fn skill_document(
        scope: InstructionScope,
        id: &str,
        name: &str,
        body: &str,
        template_mode: TemplateMode,
    ) -> InstructionDocument {
        InstructionDocument {
            id: InstructionId::parse(id).expect("id"),
            kind: InstructionKind::Skill,
            scope,
            template_mode,
            metadata: InstructionMetadata {
                display_name: Some(name.to_string()),
                description: Some(format!("{name} description")),
                ..InstructionMetadata::default()
            },
            body: body.to_string(),
            path: PathBuf::from(format!("skills/{id}/SKILL.md")),
        }
    }

    fn project_seed(documents: Vec<InstructionDocument>) -> InstructionStoreSeed {
        InstructionStoreSeed {
            manifest: InstructionStoreManifest::current(),
            files: documents
                .into_iter()
                .map(|document| InstructionSeedFile {
                    relative_path: document.path.clone(),
                    content: document.to_markdown().expect("markdown").into_bytes(),
                })
                .collect(),
        }
    }

    #[test]
    fn scope_and_source_precedence_are_deterministic() {
        let fixture = Fixture::new();
        fixture.external_global("shared-skill", "external global", "EXTERNAL_GLOBAL");
        let base = fixture.external_registry();
        fixture.write_global_document(
            skill_document(
                InstructionScope::Global,
                "shared-skill",
                "shared-skill",
                "MANAGED_GLOBAL",
                TemplateMode::Plain,
            ),
            "managed-global-skill",
        );

        let effective = fixture.effective(&base);
        let activation = effective
            .activate("shared-skill")
            .expect("resolve")
            .expect("activation");
        assert!(activation.rendered_text.contains("MANAGED_GLOBAL"));
        assert_eq!(activation.source.kind, SkillSourceKind::ManagedGlobal);

        let project_external = fixture.project.join(".jcode/skills/shared-skill");
        std::fs::create_dir_all(&project_external).expect("project external dir");
        std::fs::write(
            project_external.join("SKILL.md"),
            "---\nname: shared-skill\ndescription: project external\n---\nPROJECT_EXTERNAL\n",
        )
        .expect("project external skill");
        let effective = fixture.effective(&base);
        let activation = effective
            .activate("shared-skill")
            .expect("resolve")
            .expect("activation");
        assert!(activation.rendered_text.contains("PROJECT_EXTERNAL"));
        assert_eq!(
            activation.source.kind,
            SkillSourceKind::ExternalJcodeProject
        );

        fixture
            .service
            .configure_non_git_project(
                &fixture.project,
                "setup-project-skills",
                None,
                &project_seed(vec![skill_document(
                    InstructionScope::Project,
                    "shared-skill",
                    "shared-skill",
                    "MANAGED_PROJECT",
                    TemplateMode::Plain,
                )]),
                &[],
            )
            .expect("project repository");
        let effective = fixture.effective(&base);
        let activation = effective
            .activate("shared-skill")
            .expect("resolve")
            .expect("activation");
        assert!(activation.rendered_text.contains("MANAGED_PROJECT"));
        assert_eq!(activation.source.kind, SkillSourceKind::ManagedProject);
    }

    #[test]
    fn invalid_managed_specific_skill_does_not_fall_back_to_external() {
        let fixture = Fixture::new();
        fixture.external_global("blocked-skill", "external", "EXTERNAL_FALLBACK");
        let base = fixture.external_registry();
        fixture.write_global_document(
            skill_document(
                InstructionScope::Global,
                "blocked-skill",
                "blocked-skill",
                "VALID_MANAGED",
                TemplateMode::Plain,
            ),
            "create-blocked-skill",
        );
        fixture.write_global_document(
            skill_document(
                InstructionScope::Global,
                "healthy-skill",
                "healthy-skill",
                "HEALTHY_MANAGED",
                TemplateMode::Plain,
            ),
            "create-healthy-skill",
        );
        std::fs::write(
            fixture
                .global_repository()
                .root
                .join("skills/blocked-skill/SKILL.md"),
            "---\nid: blocked-skill\nname: blocked-skill\nkind: skill\nunknown-field: true\n---\nINVALID\n",
        )
        .expect("damage managed skill");

        let effective = fixture.effective(&base);
        let error = effective
            .activate("blocked-skill")
            .expect_err("invalid managed skill must block fallback");
        assert!(error.to_string().contains("unknown-field"), "{error}");
        assert!(effective.get("blocked-skill").is_none());
        assert!(
            effective
                .activate("healthy-skill")
                .expect("unrelated valid skill remains usable")
                .expect("healthy skill")
                .rendered_text
                .contains("HEALTHY_MANAGED")
        );
    }

    #[test]
    fn managed_handlebars_renders_current_modules_and_plain_braces_remain_literal() {
        let fixture = Fixture::new();
        fixture.write_global_document(
            InstructionDocument {
                id: InstructionId::parse("skill-module").expect("id"),
                kind: InstructionKind::Module,
                scope: InstructionScope::Global,
                template_mode: TemplateMode::Plain,
                metadata: InstructionMetadata::default(),
                body: "MODULE_TEXT".to_string(),
                path: PathBuf::from("modules/skill-module.md"),
            },
            "create-skill-module",
        );
        let mut templated = skill_document(
            InstructionScope::Global,
            "templated-skill",
            "templated-skill",
            "{{> skill-module}}\nBODY",
            TemplateMode::Handlebars,
        );
        templated.metadata.includes = Vec::new();
        fixture.write_global_document(templated, "create-templated-skill");
        fixture.write_global_document(
            skill_document(
                InstructionScope::Global,
                "plain-skill",
                "plain-skill",
                "{{literal}}",
                TemplateMode::Plain,
            ),
            "create-plain-skill",
        );

        let effective = fixture.effective(&SkillRegistry::default());
        let templated = effective
            .activate("templated-skill")
            .expect("render")
            .expect("templated skill");
        assert!(templated.rendered_text.contains("MODULE_TEXT\nBODY"));
        let plain = effective
            .activate("plain-skill")
            .expect("render")
            .expect("plain skill");
        assert!(plain.rendered_text.ends_with("{{literal}}"));
    }

    #[test]
    fn copy_external_skill_commits_complete_package_is_idempotent_and_removal_reveals_source() {
        let fixture = Fixture::new();
        let source_root =
            fixture.external_global("Copy Skill", "Copy description", "Copy body {{literal}}");
        std::fs::create_dir_all(source_root.join("references/nested")).expect("references");
        std::fs::write(source_root.join("references/guide.md"), "GUIDE").expect("guide");
        std::fs::write(
            source_root.join("references/nested/data.bin"),
            [0_u8, 1, 2, 3],
        )
        .expect("binary reference");
        let base = fixture.external_registry();
        let initial = fixture.effective(&base);
        assert_eq!(
            initial.source("Copy Skill").expect("source").kind,
            SkillSourceKind::ExternalJcodeGlobal
        );

        let copied = copy_external_skill(
            &fixture.service,
            &initial,
            ManagedSkillCopyRequest {
                skill_name: "Copy Skill",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Global,
                destination_id: None,
                operation_id: "copy-external-skill",
            },
        )
        .expect("copy skill");
        assert_eq!(copied.disposition, ManagedSkillCopyDisposition::Created);
        assert_eq!(copied.skill_id, "copy-skill");
        let destination = copied.repository.root.join("skills/copy-skill");
        assert_eq!(
            std::fs::read_to_string(destination.join("references/guide.md")).expect("guide"),
            "GUIDE"
        );
        assert_eq!(
            std::fs::read(destination.join("references/nested/data.bin")).expect("binary"),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            fixture
                .service
                .files_at_revision_under(
                    &copied.repository,
                    &copied.commit,
                    Path::new("skills/copy-skill"),
                )
                .expect("committed binary package")
                .get(Path::new("skills/copy-skill/references/nested/data.bin"))
                .map(Vec::as_slice),
            Some(&[0, 1, 2, 3][..])
        );
        assert_eq!(
            std::fs::read(destination.join(".jcode-source/original-SKILL.md"))
                .expect("original skill source"),
            std::fs::read(source_root.join("SKILL.md")).expect("external original")
        );
        let attribution: AttributionProbe = toml::from_str(
            &std::fs::read_to_string(destination.join(".jcode-source.toml")).expect("attribution"),
        )
        .expect("parse attribution");
        assert_eq!(attribution.schema_version, 1);
        assert_eq!(attribution.source_skill, "Copy Skill");
        assert_eq!(attribution.source_kind, "external Jcode global");
        assert_eq!(attribution.source_package_sha256.len(), 64);

        let effective = fixture.effective(&base);
        let activation = effective
            .activate("Copy Skill")
            .expect("resolve managed copy")
            .expect("managed copy");
        assert_eq!(activation.source.kind, SkillSourceKind::ManagedGlobal);
        assert_eq!(
            activation.rendered_text,
            "# Skill: Copy Skill\n\nCopy description\n\nCopy body {{literal}}"
        );

        let repeated = copy_external_skill(
            &fixture.service,
            &initial,
            ManagedSkillCopyRequest {
                skill_name: "Copy Skill",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Global,
                destination_id: None,
                operation_id: "copy-external-skill-again",
            },
        )
        .expect("repeat copy");
        assert_eq!(repeated.disposition, ManagedSkillCopyDisposition::NoChange);

        std::fs::write(destination.join("references/guide.md"), "DIVERGENT")
            .expect("divergent managed edit");
        let collision = copy_external_skill(
            &fixture.service,
            &initial,
            ManagedSkillCopyRequest {
                skill_name: "Copy Skill",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Global,
                destination_id: None,
                operation_id: "copy-external-skill-collision",
            },
        )
        .expect_err("divergent destination must not be overwritten");
        assert_eq!(collision.kind, InstructionRepositoryErrorKind::Conflict);

        let paths = copied.changed_paths;
        let head = fixture
            .service
            .inspect(&copied.repository)
            .expect("inspect")
            .head
            .expect("head");
        let expected_files = paths
            .iter()
            .map(|path| {
                fixture
                    .service
                    .open_draft(&copied.repository, path)
                    .expect("draft")
                    .base
            })
            .collect();
        let mutations = paths
            .iter()
            .cloned()
            .map(|relative_path| InstructionFileMutation::Delete { relative_path })
            .collect();
        fixture
            .service
            .commit(
                &copied.repository,
                &InstructionCommitRequest {
                    operation_id: "remove-managed-copy".to_string(),
                    message: "skill: remove managed copy".to_string(),
                    expected_head: head,
                    expected_files,
                    mutations,
                },
            )
            .expect("remove managed copy");
        let exposed = fixture.effective(&base);
        assert_eq!(
            exposed.source("Copy Skill").expect("external exposed").kind,
            SkillSourceKind::ExternalJcodeGlobal
        );
    }

    #[test]
    fn copy_external_skill_supports_project_destination() {
        let fixture = Fixture::new();
        let project_external = fixture.project.join(".jcode/skills/project-copy");
        std::fs::create_dir_all(&project_external).expect("project external");
        std::fs::write(
            project_external.join("SKILL.md"),
            "---\nname: project-copy\ndescription: Project copy\n---\nPROJECT_COPY_BODY\n",
        )
        .expect("project external skill");
        fixture
            .service
            .configure_non_git_project(
                &fixture.project,
                "setup-project-copy-store",
                None,
                &project_seed(Vec::new()),
                &[],
            )
            .expect("project repository");
        let initial = fixture.effective(&SkillRegistry::default());
        assert_eq!(
            initial
                .source("project-copy")
                .expect("external source")
                .kind,
            SkillSourceKind::ExternalJcodeProject
        );
        let copied = copy_external_skill(
            &fixture.service,
            &initial,
            ManagedSkillCopyRequest {
                skill_name: "project-copy",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Project,
                destination_id: None,
                operation_id: "copy-project-skill",
            },
        )
        .expect("copy project skill");
        assert!(copied.effective_for_source_project);
        assert_eq!(copied.disposition, ManagedSkillCopyDisposition::Created);
        let effective = fixture.effective(&SkillRegistry::default());
        let activation = effective
            .activate("project-copy")
            .expect("resolve")
            .expect("managed project copy");
        assert_eq!(activation.source.kind, SkillSourceKind::ManagedProject);
        assert!(activation.rendered_text.contains("PROJECT_COPY_BODY"));
    }

    #[test]
    fn copy_external_skill_retry_completes_partial_matching_worktree() {
        let fixture = Fixture::new();
        let source_root = fixture.external_global("partial-copy", "Partial copy", "PARTIAL_BODY");
        std::fs::write(source_root.join("reference.txt"), "REFERENCE").expect("reference");
        let registry = fixture.effective(&fixture.external_registry());
        let skill = registry.get("partial-copy").expect("skill");
        let source = registry.source("partial-copy").expect("source");
        let id = InstructionId::parse("partial-copy").expect("id");
        let package_root = PathBuf::from("skills/partial-copy");
        let desired = copied_package(skill, source, &package_root, &id).expect("desired package");
        let repository = fixture.global_repository();
        let (partial_path, partial_content) = desired.iter().next().expect("package file");
        let absolute = repository.root.join(partial_path);
        std::fs::create_dir_all(absolute.parent().expect("parent")).expect("partial parent");
        std::fs::write(&absolute, partial_content).expect("partial matching file");

        let outcome = copy_external_skill(
            &fixture.service,
            &registry,
            ManagedSkillCopyRequest {
                skill_name: "partial-copy",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Global,
                destination_id: None,
                operation_id: "complete-partial-copy",
            },
        )
        .expect("complete partial Copy");
        assert_eq!(outcome.disposition, ManagedSkillCopyDisposition::Created);
        assert_eq!(
            fixture
                .service
                .files_at_revision_under(&repository, &outcome.commit, &package_root)
                .expect("committed package"),
            desired
        );
        assert!(
            fixture
                .service
                .inspect(&repository)
                .expect("inspect")
                .changes
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_external_skill_rejects_package_symlinks() {
        let fixture = Fixture::new();
        let source_root = fixture.external_global("symlink-skill", "Symlink", "BODY");
        std::fs::write(source_root.join("target.txt"), "TARGET").expect("target");
        std::os::unix::fs::symlink("target.txt", source_root.join("link.txt")).expect("symlink");
        let registry = fixture.effective(&fixture.external_registry());
        let error = copy_external_skill(
            &fixture.service,
            &registry,
            ManagedSkillCopyRequest {
                skill_name: "symlink-skill",
                working_dir: Some(&fixture.project),
                destination: ManagedSkillDestination::Global,
                destination_id: None,
                operation_id: "copy-symlink-skill",
            },
        )
        .expect_err("symlink must be rejected");
        assert!(error.detail.contains("symlink"), "{error}");
    }
}
