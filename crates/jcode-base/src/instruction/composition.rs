use super::{
    AddendumMetadata, AgentAvailability, AgentMetadata, InstructionDocument, InstructionError,
    InstructionId, InstructionKind, InstructionLegacyImportSpec, InstructionLegacyImportTarget,
    InstructionMetadata, InstructionRepositoryError, InstructionRepositoryService,
    InstructionResourceRef, InstructionScope, InstructionSeedFile, InstructionSelector,
    InstructionStoreManifest, InstructionStoreSeed, LegacyInstructionSourceKind, TemplateMode,
};
use crate::prompt::{self, PromptCapabilities, SkillInfo};
use crate::session::{StoredAgentReference, StoredSystemPromptState};
use std::fmt;
use std::path::{Path, PathBuf};

const KERNEL_ID: &str = "kernel";
const COMMON_ID: &str = "common";
const MERMAID_ID: &str = "mermaid";
const COMPATIBILITY_AGENT_ID: &str = "jcode";

/// Mirza-approved Phase 3 profile-lifecycle kernel. The wrapper used for later
/// appended transitions remains code-owned by WP-04.
pub const AGENT_PROFILE_KERNEL: &str = "## Agent profiles\n\nThe initial agent profile appears in this system prompt. After the user explicitly changes agents, Jcode may append a Jcode-generated `<jcode_agent_profile>` user message containing a complete replacement profile. From that message onward, follow the latest such profile instead of earlier agent-profile instructions. It does not replace other system instructions or earlier conversation context.\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentSelection {
    Default,
    Explicit(InstructionSelector),
}

impl AgentSelection {
    pub fn parse(value: Option<&str>) -> Result<Self, InstructionError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => {
                InstructionSelector::parse(InstructionKind::Agent, value).map(Self::Explicit)
            }
            None => Ok(Self::Default),
        }
    }

    pub fn from_stored(agent: &StoredAgentReference) -> Result<Self, InstructionError> {
        let selector = match agent.scope {
            InstructionScope::Global => {
                InstructionSelector::global(InstructionKind::Agent, agent.id.clone())?
            }
            InstructionScope::Project => {
                InstructionSelector::project(InstructionKind::Agent, agent.id.clone())?
            }
        };
        Ok(Self::Explicit(selector))
    }
}

#[derive(Clone, Debug)]
pub struct SystemPromptActivationRequest<'a> {
    pub working_dir: Option<&'a Path>,
    pub selection: AgentSelection,
    pub is_selfdev: bool,
    pub capabilities: PromptCapabilities,
    pub available_skills: &'a [SkillInfo],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemPromptActivation {
    pub state: StoredSystemPromptState,
    pub initialized_global_store: bool,
}

#[derive(Debug)]
pub enum SystemPromptActivationError {
    Repository(InstructionRepositoryError),
    Instruction(InstructionError),
    InvalidDefault {
        scope: InstructionScope,
        value: String,
    },
    Compatibility(String),
}

impl fmt::Display for SystemPromptActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => write!(formatter, "{error}"),
            Self::Instruction(error) => write!(formatter, "{error}"),
            Self::InvalidDefault { scope, value } => {
                write!(formatter, "invalid {scope} default agent '{value}'")
            }
            Self::Compatibility(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for SystemPromptActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Instruction(error) => Some(error),
            Self::InvalidDefault { .. } | Self::Compatibility(_) => None,
        }
    }
}

impl From<InstructionRepositoryError> for SystemPromptActivationError {
    fn from(error: InstructionRepositoryError) -> Self {
        Self::Repository(error)
    }
}

impl From<InstructionError> for SystemPromptActivationError {
    fn from(error: InstructionError) -> Self {
        Self::Instruction(error)
    }
}

#[derive(Clone, Debug)]
pub struct SystemPromptComposer {
    repositories: InstructionRepositoryService,
}

impl Default for SystemPromptComposer {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemPromptComposer {
    pub fn new() -> Self {
        Self {
            repositories: InstructionRepositoryService::new(),
        }
    }

    pub fn from_repository_service(repositories: InstructionRepositoryService) -> Self {
        Self { repositories }
    }

    pub fn activate(
        &self,
        request: SystemPromptActivationRequest<'_>,
    ) -> Result<SystemPromptActivation, SystemPromptActivationError> {
        let seed = shipped_instruction_seed()?;
        let legacy = global_legacy_imports(&self.repositories)?;
        let initialized = self.repositories.initialize_global(&seed, &legacy)?;
        let project_root = request
            .working_dir
            .map(|working_dir| self.repositories.resolve_project_root(working_dir))
            .transpose()?;
        let project_repository = request
            .working_dir
            .map(|working_dir| self.repositories.resolve_project_repository(working_dir))
            .transpose()?
            .flatten();

        let mut sources = self
            .repositories
            .instruction_sources(project_repository.as_ref())?;
        let global_repository = self.repositories.global_repository()?;
        let jcode_home = global_repository.root.parent().ok_or_else(|| {
            SystemPromptActivationError::Compatibility(
                "global instruction repository has no Jcode home parent".to_string(),
            )
        })?;
        sources = sources.with_global_agents_md(self.repositories.global_agents_path()?);
        if let Some(root) = project_root.as_ref() {
            sources = sources.with_project_agents_md(root.join("AGENTS.md"));
        }
        let runtime = super::InstructionRuntime::discover(sources);

        let global_manifest = self.repositories.load_manifest(&initialized.repository)?;
        let project_manifest = project_repository
            .as_ref()
            .map(|repository| self.repositories.load_manifest(repository))
            .transpose()?;
        let selection = resolve_selection(
            request.selection,
            project_manifest.as_ref(),
            &global_manifest,
        )?;

        let mut rendered_agent =
            runtime.render_agent(&selection, AgentAvailability::Primary, &())?;
        let mut agent_reference = rendered_agent.root.clone();
        let agent_document = runtime.resolve(&selection)?;
        let display_name = agent_document
            .metadata
            .display_name
            .clone()
            .unwrap_or_else(|| agent_reference.id.to_string());
        if global_manifest
            .legacy_imports
            .contains_key("global-system-prompt")
            && agent_reference.scope == InstructionScope::Global
            && agent_reference.id.as_str() == COMPATIBILITY_AGENT_ID
        {
            rendered_agent.text = rendered_agent.text.trim().to_string();
        }
        let project_system_imported = project_manifest.as_ref().is_some_and(|manifest| {
            manifest
                .legacy_imports
                .values()
                .any(|receipt| receipt.source_kind == LegacyInstructionSourceKind::SystemPrompt)
        });
        if agent_reference.id.as_str() == COMPATIBILITY_AGENT_ID
            && !project_system_imported
            && let Some(root) = project_root.as_ref()
            && let Some(project_prompt) = read_nonblank(root.join(".jcode/system-prompt.md"))?
        {
            if agent_reference.scope == InstructionScope::Project {
                return Err(SystemPromptActivationError::Compatibility(
                    "project legacy system-prompt.md and managed project:jcode both exist without an import receipt"
                        .to_string(),
                ));
            }
            rendered_agent.text = project_prompt.trim().to_string();
            agent_reference.scope = InstructionScope::Project;
        }

        let mut parts = Vec::new();
        parts.push(render_required_system(&runtime, KERNEL_ID)?);
        parts.push(rendered_agent.text);
        if request.capabilities.mermaid {
            parts.push(render_required_system(&runtime, MERMAID_ID)?);
        }
        if request.is_selfdev {
            parts.push(prompt::build_selfdev_prompt_static_for_working_dir(
                request.working_dir,
            ));
        }

        push_optional_system(
            &runtime,
            InstructionScope::Global,
            COMMON_ID,
            global_manifest
                .legacy_imports
                .contains_key("global-prompt-overlay"),
            &mut parts,
        )?;
        let project_overlay_imported = project_manifest.as_ref().is_some_and(|manifest| {
            manifest
                .legacy_imports
                .values()
                .any(|receipt| receipt.source_kind == LegacyInstructionSourceKind::PromptOverlay)
        });
        let project_legacy_overlay = project_root
            .as_ref()
            .map(|root| read_present(root.join(".jcode/prompt-overlay.md")))
            .transpose()?
            .flatten();
        if project_repository.is_some()
            && (project_overlay_imported || project_legacy_overlay.is_none())
        {
            push_optional_system(
                &runtime,
                InstructionScope::Project,
                COMMON_ID,
                project_overlay_imported,
                &mut parts,
            )?;
        } else if let Some(content) = project_legacy_overlay {
            let managed_common = InstructionSelector::project(InstructionKind::System, COMMON_ID)?;
            if runtime.resolve(&managed_common).is_ok() {
                return Err(SystemPromptActivationError::Compatibility(
                    "project legacy prompt-overlay.md and managed project:common both exist without an import receipt"
                        .to_string(),
                ));
            }
            parts.push(format!(
                "# Project Prompt Overlay (.jcode/prompt-overlay.md)\n\n{}",
                content.trim()
            ));
        }

        let external_agents = runtime.render_external_agents();
        if !external_agents.is_empty() {
            parts.push(external_agents);
        }

        push_project_addenda(&runtime, &agent_reference, &mut parts)?;
        push_legacy_preferred_tools(jcode_home, project_root.as_deref(), &mut parts)?;
        if let Some(skills) = prompt::build_available_skills_prompt(request.available_skills) {
            parts.push(skills);
        }

        Ok(SystemPromptActivation {
            state: StoredSystemPromptState {
                text: parts.join("\n\n"),
                active_agent: StoredAgentReference {
                    scope: agent_reference.scope,
                    id: agent_reference.id.to_string(),
                    display_name,
                },
                first_provider_dispatch_at: None,
                active_transition_message_id: None,
            },
            initialized_global_store: initialized.created,
        })
    }
}

fn resolve_selection(
    requested: AgentSelection,
    project: Option<&InstructionStoreManifest>,
    global: &InstructionStoreManifest,
) -> Result<InstructionSelector, SystemPromptActivationError> {
    if let AgentSelection::Explicit(selector) = requested {
        return Ok(selector);
    }
    if let Some(value) = project.and_then(|manifest| manifest.default_agent.as_deref()) {
        return InstructionSelector::parse(InstructionKind::Agent, value).map_err(|_| {
            SystemPromptActivationError::InvalidDefault {
                scope: InstructionScope::Project,
                value: value.to_string(),
            }
        });
    }
    if let Some(value) = global.default_agent.as_deref() {
        return InstructionSelector::parse(InstructionKind::Agent, value).map_err(|_| {
            SystemPromptActivationError::InvalidDefault {
                scope: InstructionScope::Global,
                value: value.to_string(),
            }
        });
    }
    Ok(InstructionSelector::unqualified(
        InstructionKind::Agent,
        COMPATIBILITY_AGENT_ID,
    )?)
}

fn render_required_system(
    runtime: &super::InstructionRuntime,
    id: &str,
) -> Result<String, InstructionError> {
    runtime
        .render(
            &InstructionSelector::unqualified(InstructionKind::System, id)?,
            &(),
        )
        .map(|rendered| rendered.text)
}

fn push_optional_system(
    runtime: &super::InstructionRuntime,
    scope: InstructionScope,
    id: &str,
    legacy_overlay: bool,
    parts: &mut Vec<String>,
) -> Result<(), InstructionError> {
    let selector = match scope {
        InstructionScope::Global => InstructionSelector::global(InstructionKind::System, id)?,
        InstructionScope::Project => InstructionSelector::project(InstructionKind::System, id)?,
    };
    match runtime.render(&selector, &()) {
        Ok(rendered) if legacy_overlay => {
            let heading = match scope {
                InstructionScope::Global => "# Global Prompt Overlay (~/.jcode/prompt-overlay.md)",
                InstructionScope::Project => "# Project Prompt Overlay (.jcode/prompt-overlay.md)",
            };
            parts.push(format!("{heading}\n\n{}", rendered.text.trim()));
        }
        Ok(rendered) if !rendered.text.is_empty() => parts.push(rendered.text),
        Ok(_) | Err(InstructionError::ResourceNotFound { .. }) => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn push_project_addenda(
    runtime: &super::InstructionRuntime,
    active_agent: &InstructionResourceRef,
    parts: &mut Vec<String>,
) -> Result<(), InstructionError> {
    for addendum in runtime.documents(InstructionKind::AgentAddendum, InstructionScope::Project) {
        let Some(AddendumMetadata { target }) = addendum.metadata.addendum.as_ref() else {
            continue;
        };
        let target = runtime.resolve(target)?;
        let target_ref = InstructionResourceRef {
            scope: target.scope,
            kind: target.kind,
            id: target.id.clone(),
        };
        if &target_ref != active_agent {
            continue;
        }
        let rendered = runtime.render(
            &InstructionSelector::project(InstructionKind::AgentAddendum, addendum.id.to_string())?,
            &(),
        )?;
        if !rendered.text.is_empty() {
            parts.push(rendered.text);
        }
    }
    Ok(())
}

fn push_legacy_preferred_tools(
    jcode_home: &Path,
    project_root: Option<&Path>,
    parts: &mut Vec<String>,
) -> Result<(), SystemPromptActivationError> {
    if let Some(content) = read_present(jcode_home.join("preferred-tools.md"))? {
        parts.push(format!(
            "# Global Preferred Tools (~/.jcode/preferred-tools.md)\n\n{}",
            content.trim()
        ));
    }
    if let Some(project_root) = project_root
        && let Some(content) = read_present(project_root.join(".jcode/preferred-tools.md"))?
    {
        parts.push(format!(
            "# Project Preferred Tools (.jcode/preferred-tools.md)\n\n{}",
            content.trim()
        ));
    }
    Ok(())
}

fn read_present(path: PathBuf) -> Result<Option<String>, SystemPromptActivationError> {
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SystemPromptActivationError::Compatibility(format!(
            "read compatibility instruction {}: {error}",
            path.display()
        ))),
    }
}

fn read_nonblank(path: PathBuf) -> Result<Option<String>, SystemPromptActivationError> {
    Ok(read_present(path)?.filter(|content| !content.trim().is_empty()))
}

pub fn shipped_instruction_seed() -> Result<InstructionStoreSeed, InstructionError> {
    let documents = [
        InstructionDocument {
            id: InstructionId::parse(KERNEL_ID)?,
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
            body: AGENT_PROFILE_KERNEL.to_string(),
            path: PathBuf::from("system/kernel.md"),
        },
        InstructionDocument {
            id: InstructionId::parse(COMMON_ID)?,
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
            body: String::new(),
            path: PathBuf::from("system/common.md"),
        },
        InstructionDocument {
            id: InstructionId::parse(MERMAID_ID)?,
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
            body: prompt::MERMAID_PROMPT.to_string(),
            path: PathBuf::from("system/mermaid.md"),
        },
        compatibility_agent_document()?,
    ];
    Ok(InstructionStoreSeed {
        manifest: InstructionStoreManifest::current(),
        files: documents
            .into_iter()
            .map(|document| {
                Ok(InstructionSeedFile {
                    relative_path: document.path.clone(),
                    content: document.to_markdown()?.into_bytes(),
                })
            })
            .collect::<Result<Vec<_>, InstructionError>>()?,
    })
}

fn compatibility_agent_document() -> Result<InstructionDocument, InstructionError> {
    Ok(InstructionDocument {
        id: InstructionId::parse(COMPATIBILITY_AGENT_ID)?,
        kind: InstructionKind::Agent,
        scope: InstructionScope::Global,
        template_mode: TemplateMode::Plain,
        metadata: InstructionMetadata {
            display_name: Some("Jcode".to_string()),
            description: Some("Jcode compatibility agent".to_string()),
            agent: Some(AgentMetadata {
                availability: AgentAvailability::Both,
            }),
            ..InstructionMetadata::default()
        },
        body: prompt::DEFAULT_SYSTEM_PROMPT.to_string(),
        path: PathBuf::from("agents/jcode.md"),
    })
}

fn global_legacy_imports(
    repositories: &InstructionRepositoryService,
) -> Result<Vec<InstructionLegacyImportSpec>, SystemPromptActivationError> {
    let root = repositories.global_repository()?.root;
    let Some(jcode_home) = root.parent() else {
        return Err(SystemPromptActivationError::Compatibility(
            "global instruction repository has no Jcode home parent".to_string(),
        ));
    };
    let mut imports = Vec::new();
    let system_prompt = jcode_home.join("system-prompt.md");
    if read_nonblank(system_prompt.clone())?.is_some() {
        let document = compatibility_agent_document()?;
        imports.push(InstructionLegacyImportSpec {
            import_id: "global-system-prompt".to_string(),
            source_kind: LegacyInstructionSourceKind::SystemPrompt,
            source_path: system_prompt,
            target: legacy_target(&document),
        });
    }
    let overlay = jcode_home.join("prompt-overlay.md");
    if read_present(overlay.clone())?.is_some() {
        let document = InstructionDocument {
            id: InstructionId::parse(COMMON_ID)?,
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
            body: String::new(),
            path: PathBuf::from("system/common.md"),
        };
        imports.push(InstructionLegacyImportSpec {
            import_id: "global-prompt-overlay".to_string(),
            source_kind: LegacyInstructionSourceKind::PromptOverlay,
            source_path: overlay,
            target: legacy_target(&document),
        });
    }
    Ok(imports)
}

fn legacy_target(document: &InstructionDocument) -> InstructionLegacyImportTarget {
    InstructionLegacyImportTarget {
        relative_path: document.path.clone(),
        id: document.id.clone(),
        kind: document.kind,
        scope: document.scope,
        template_mode: document.template_mode,
        metadata: document.metadata.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        jcode_home: PathBuf,
        durable: PathBuf,
        project: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let home = temp.path().join("home");
            let jcode_home = home.join(".jcode");
            let durable = jcode_home.join("state");
            let project = temp.path().join("project");
            std::fs::create_dir_all(&jcode_home).expect("jcode home");
            std::fs::create_dir_all(&durable).expect("durable");
            std::fs::create_dir_all(&project).expect("project");
            Self {
                _temp: temp,
                home,
                jcode_home,
                durable,
                project,
            }
        }

        fn service(&self) -> InstructionRepositoryService {
            InstructionRepositoryService::from_paths(&self.jcode_home, &self.durable)
        }

        fn composer(&self) -> SystemPromptComposer {
            SystemPromptComposer::from_repository_service(self.service())
        }

        fn request<'a>(&'a self, selection: AgentSelection) -> SystemPromptActivationRequest<'a> {
            SystemPromptActivationRequest {
                working_dir: Some(&self.project),
                selection,
                is_selfdev: false,
                capabilities: PromptCapabilities { mermaid: false },
                available_skills: &[],
            }
        }
    }

    fn document(
        scope: InstructionScope,
        kind: InstructionKind,
        id: &str,
        path: &str,
        body: &str,
    ) -> InstructionDocument {
        let metadata = if kind == InstructionKind::Agent {
            InstructionMetadata {
                display_name: Some(id.to_string()),
                description: Some("synthetic mechanism fixture".to_string()),
                agent: Some(AgentMetadata {
                    availability: AgentAvailability::Both,
                }),
                ..InstructionMetadata::default()
            }
        } else {
            InstructionMetadata::default()
        };
        InstructionDocument {
            id: InstructionId::parse(id).expect("id"),
            kind,
            scope,
            template_mode: TemplateMode::Plain,
            metadata,
            body: body.to_string(),
            path: PathBuf::from(path),
        }
    }

    fn seed(
        manifest: InstructionStoreManifest,
        documents: Vec<InstructionDocument>,
    ) -> InstructionStoreSeed {
        InstructionStoreSeed {
            manifest,
            files: documents
                .into_iter()
                .map(|document| InstructionSeedFile {
                    relative_path: document.path.clone(),
                    content: document.to_markdown().expect("serialize").into_bytes(),
                })
                .collect(),
        }
    }

    #[test]
    fn activation_initializes_once_and_renders_current_working_files() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        let first = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("first activation");
        assert!(first.initialized_global_store);
        assert_eq!(first.state.active_agent.scope, InstructionScope::Global);
        assert_eq!(first.state.active_agent.id, "jcode");

        let agent_path = fixture.jcode_home.join("instructions/agents/jcode.md");
        let synthetic = document(
            InstructionScope::Global,
            InstructionKind::Agent,
            "jcode",
            "agents/jcode.md",
            "SYNTHETIC_AGENT_V2",
        );
        std::fs::write(agent_path, synthetic.to_markdown().expect("serialize"))
            .expect("write working agent");
        let second = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("second activation");
        assert!(!second.initialized_global_store);
        assert!(second.state.text.contains("SYNTHETIC_AGENT_V2"));
        assert!(!first.state.text.contains("SYNTHETIC_AGENT_V2"));
    }

    #[test]
    fn legacy_sources_preserve_scope_order_and_project_compatibility_redefinition() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.jcode_home.join("system-prompt.md"),
            "GLOBAL_LEGACY_AGENT\n",
        )
        .expect("global legacy agent");
        std::fs::write(
            fixture.jcode_home.join("prompt-overlay.md"),
            "GLOBAL_COMMON",
        )
        .expect("global overlay");
        std::fs::write(
            fixture.jcode_home.join("preferred-tools.md"),
            "GLOBAL_TOOLS",
        )
        .expect("global tools");
        std::fs::write(fixture.home.join("AGENTS.md"), "GLOBAL_AGENTS").expect("global agents");
        std::fs::create_dir_all(fixture.project.join(".jcode")).expect("project jcode");
        std::fs::write(
            fixture.project.join(".jcode/system-prompt.md"),
            "PROJECT_LEGACY_AGENT\n",
        )
        .expect("project legacy agent");
        std::fs::write(
            fixture.project.join(".jcode/prompt-overlay.md"),
            "PROJECT_COMMON",
        )
        .expect("project overlay");
        std::fs::write(
            fixture.project.join(".jcode/preferred-tools.md"),
            "PROJECT_TOOLS",
        )
        .expect("project tools");
        std::fs::write(fixture.project.join("AGENTS.md"), "PROJECT_AGENTS")
            .expect("project agents");

        let activation = fixture
            .composer()
            .activate(fixture.request(AgentSelection::Default))
            .expect("activation");
        assert_eq!(
            activation.state.active_agent.scope,
            InstructionScope::Project
        );
        assert!(activation.state.text.contains("PROJECT_LEGACY_AGENT"));
        assert!(!activation.state.text.contains("GLOBAL_LEGACY_AGENT"));
        for (left, right) in [
            ("GLOBAL_COMMON", "PROJECT_COMMON"),
            ("GLOBAL_AGENTS", "PROJECT_AGENTS"),
            ("GLOBAL_TOOLS", "PROJECT_TOOLS"),
        ] {
            assert!(
                activation.state.text.find(left) < activation.state.text.find(right),
                "{left} must precede {right}"
            );
        }
    }

    #[test]
    fn project_default_agent_and_addendum_compose_without_replacing_global_common() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize global");
        let global_common = document(
            InstructionScope::Global,
            InstructionKind::System,
            COMMON_ID,
            "system/common.md",
            "GLOBAL_COMMON",
        );
        std::fs::write(
            fixture.jcode_home.join("instructions/system/common.md"),
            global_common.to_markdown().expect("serialize"),
        )
        .expect("global common");

        let mut manifest = InstructionStoreManifest::current();
        manifest.default_agent = Some("project:builder".to_string());
        let mut addendum = document(
            InstructionScope::Project,
            InstructionKind::AgentAddendum,
            "builder-project",
            "addenda/builder-project.md",
            "PROJECT_ADDENDUM",
        );
        addendum.metadata.addendum = Some(AddendumMetadata {
            target: InstructionSelector::project(InstructionKind::Agent, "builder")
                .expect("target"),
        });
        fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "setup-project-instructions",
                None,
                &seed(
                    manifest,
                    vec![
                        document(
                            InstructionScope::Project,
                            InstructionKind::Agent,
                            "builder",
                            "agents/builder.md",
                            "PROJECT_AGENT",
                        ),
                        document(
                            InstructionScope::Project,
                            InstructionKind::System,
                            COMMON_ID,
                            "system/common.md",
                            "PROJECT_COMMON",
                        ),
                        addendum,
                    ],
                ),
                &[],
            )
            .expect("project repository");

        let activation = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("project activation");
        assert_eq!(
            activation.state.active_agent.scope,
            InstructionScope::Project
        );
        assert_eq!(activation.state.active_agent.id, "builder");
        let global = activation.state.text.find("GLOBAL_COMMON").expect("global");
        let project = activation
            .state
            .text
            .find("PROJECT_COMMON")
            .expect("project");
        let addendum = activation
            .state
            .text
            .find("PROJECT_ADDENDUM")
            .expect("addendum");
        assert!(global < project);
        assert!(project < addendum);
    }

    #[test]
    fn managed_project_jcode_redefines_the_compatibility_fallback() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize global");
        fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "setup-project-jcode-redefinition",
                None,
                &seed(
                    InstructionStoreManifest::current(),
                    vec![document(
                        InstructionScope::Project,
                        InstructionKind::Agent,
                        COMPATIBILITY_AGENT_ID,
                        "agents/jcode.md",
                        "SYNTHETIC_PROJECT_JCODE",
                    )],
                ),
                &[],
            )
            .expect("project repository");

        let activation = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("project compatibility redefinition");
        assert_eq!(
            activation.state.active_agent.scope,
            InstructionScope::Project
        );
        assert_eq!(activation.state.active_agent.id, COMPATIBILITY_AGENT_ID);
        assert!(activation.state.text.contains("SYNTHETIC_PROJECT_JCODE"));
    }

    #[test]
    fn explicit_invalid_and_unavailable_agents_fail_without_fallback() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        let missing = composer.activate(fixture.request(AgentSelection::Explicit(
            InstructionSelector::global(InstructionKind::Agent, "missing").expect("selector"),
        )));
        assert!(matches!(
            missing,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::ResourceNotFound { .. }
            ))
        ));

        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize");
        let mut isolated = document(
            InstructionScope::Global,
            InstructionKind::Agent,
            "isolated",
            "agents/isolated.md",
            "ISOLATED",
        );
        isolated.metadata.agent = Some(AgentMetadata {
            availability: AgentAvailability::Isolated,
        });
        std::fs::write(
            fixture.jcode_home.join("instructions/agents/isolated.md"),
            isolated.to_markdown().expect("serialize"),
        )
        .expect("isolated agent");
        let unavailable = composer.activate(fixture.request(AgentSelection::Explicit(
            InstructionSelector::global(InstructionKind::Agent, "isolated").expect("selector"),
        )));
        assert!(matches!(
            unavailable,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::AgentUnavailable { .. }
            ))
        ));
    }

    #[test]
    fn global_default_selects_exact_agent_and_missing_default_does_not_fallback() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize");
        let global_root = fixture.jcode_home.join("instructions");
        let preferred = document(
            InstructionScope::Global,
            InstructionKind::Agent,
            "preferred",
            "agents/preferred.md",
            "SYNTHETIC_GLOBAL_DEFAULT",
        );
        std::fs::write(
            global_root.join("agents/preferred.md"),
            preferred.to_markdown().expect("serialize"),
        )
        .expect("write preferred");
        std::fs::write(
            global_root.join("instruction-store.toml"),
            "schema_version = 1\ndefault_agent = \"global:preferred\"\n",
        )
        .expect("write default");
        let selected = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("select global default");
        assert_eq!(selected.state.active_agent.id, "preferred");
        assert!(selected.state.text.contains("SYNTHETIC_GLOBAL_DEFAULT"));

        std::fs::write(
            global_root.join("instruction-store.toml"),
            "schema_version = 1\ndefault_agent = \"global:missing\"\n",
        )
        .expect("write missing default");
        let missing = composer.activate(fixture.request(AgentSelection::Default));
        assert!(matches!(
            missing,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::ResourceNotFound { .. }
            ))
        ));
    }
}
