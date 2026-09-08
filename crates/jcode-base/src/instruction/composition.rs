#[path = "composition_compatibility.rs"]
mod compatibility;

const PREFERRED_TOOLS_CUTOVER_SEED: u32 = 25;
use super::ConsumerScopePolicy;
use super::{
    AgentAvailability, AgentMetadata, ConsumerRegistration, InstructionConsumer,
    InstructionDocument, InstructionError, InstructionId, InstructionKind,
    InstructionLegacyImportSpec, InstructionLegacyImportTarget, InstructionMetadata,
    InstructionRepositoryError, InstructionRepositoryService, InstructionResourceRef,
    InstructionScope, InstructionSeedFile, InstructionSelector, InstructionStoreManifest,
    InstructionStoreSeed, LegacyInstructionSourceKind, TemplateMode,
};
use crate::prompt::{self, PromptCapabilities, SkillInfo};
use crate::session::{StoredAgentReference, StoredSystemPromptState};
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};

const KERNEL_ID: &str = "kernel";
const COMMON_ID: &str = "common";
const MERMAID_ID: &str = "mermaid";
const AVAILABLE_SKILLS_ID: &str = "available-skills";
const COMPATIBILITY_AGENT_ID: &str = "jcode";
const AGENT_TRANSITION_ID: &str = "agent-transition";
const AGENT_REPLACEMENT_ID: &str = "agent-replacement";

/// Mirza-approved Phase 3 profile-lifecycle kernel. The wrapper used for later
/// appended transitions remains code-owned by WP-04.
pub const AGENT_PROFILE_KERNEL: &str = "## Agent profiles\n\nThe initial agent profile appears in this system prompt. After the user explicitly changes agents, Jcode may append a Jcode-generated `<jcode_agent_profile>` user message containing a complete replacement profile. From that message onward, follow the latest such profile instead of earlier agent-profile instructions. It does not replace other system instructions or earlier conversation context.\n";
pub const AGENT_TRANSITION_PROSE: &str = "The user switched this session to the following complete agent profile. Follow it from this point onward instead of earlier agent-profile instructions.\n";
pub const AGENT_REPLACEMENT_PROSE: &str = "The user explicitly replaced this session's system prompt and active agent from {{previous_agent}} to {{new_agent}}.\n";
pub const AVAILABLE_SKILLS_PROSE: &str = "# Available Skills\n\nYou have access to the following skills that the user can invoke with `/skillname`:\n{{skills}}\n\nWhen a user asks about available skills or capabilities, mention these skills.";

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

    pub fn matches_stored(&self, agent: &StoredAgentReference) -> bool {
        match self {
            Self::Default => false,
            Self::Explicit(selector) if selector.id.as_str() != agent.id => false,
            Self::Explicit(selector) => match selector.scope {
                // Unqualified selection must resolve current project-first
                // specificity before it can be classified as a no-op.
                super::InstructionScopeSelector::Unqualified => false,
                super::InstructionScopeSelector::Global => agent.scope == InstructionScope::Global,
                super::InstructionScopeSelector::Project => {
                    agent.scope == InstructionScope::Project
                }
            },
        }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentProfileTransition {
    pub agent: StoredAgentReference,
    pub transition_sentence: String,
    pub complete_instructions: String,
    pub initialized_global_store: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemPromptReplacement {
    pub activation: SystemPromptActivation,
    pub audit_sentence: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCatalogEntry {
    pub agent: StoredAgentReference,
    pub description: String,
}

struct CompositionEnvironment {
    runtime: super::InstructionRuntime,
    global_manifest: InstructionStoreManifest,
    project_manifest: Option<InstructionStoreManifest>,
    project_root: Option<PathBuf>,
    initialized_global_store: bool,
}

struct RenderedAgentProfile {
    agent: StoredAgentReference,
    resource: InstructionResourceRef,
    text: String,
}

#[derive(Serialize)]
struct AgentReplacementValues<'a> {
    previous_agent: &'a str,
    new_agent: &'a str,
}

#[derive(Serialize)]
struct AvailableSkillsValues {
    skills: String,
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

    /// Initialize or validate the shipped global instruction store, including
    /// exact eligible legacy imports and versioned seed adoption.
    pub fn ensure_global_store(
        &self,
    ) -> Result<super::InstructionStoreInitialization, SystemPromptActivationError> {
        let seed = shipped_instruction_seed()?;
        let repository = self.repositories.global_repository()?;
        // This hint only plans cutover. initialize_global remains the authority
        // for validating damage and resuming an interrupted first installation.
        let prior = self.repositories.load_manifest(&repository).ok();
        let legacy = global_legacy_imports(&self.repositories, prior.as_ref())?;
        let initialized = self.repositories.initialize_global(&seed, &legacy)?;
        if !initialized.created {
            for spec in legacy.iter().filter(|spec| {
                matches!(
                    spec.source_kind,
                    LegacyInstructionSourceKind::PreferredTools
                        | LegacyInstructionSourceKind::SwarmPrompt
                )
            }) {
                self.repositories.import_legacy(
                    &initialized.repository,
                    spec,
                    &format!("wp07-{}", spec.import_id),
                )?;
            }
        }
        self.repositories
            .ensure_shipped_seed(&initialized.repository, &seed)?;
        Ok(initialized)
    }

    /// Ordinary readers validate their selected resources, not unrelated
    /// editable content. Bootstrap and seed upgrades still use full validation.
    pub(crate) fn prepare_global_store_for_read(
        &self,
    ) -> Result<(super::InstructionRepositoryRef, bool), SystemPromptActivationError> {
        let global = self.repositories.global_repository()?;
        let state = self.repositories.inspect(&global)?;
        match state.health {
            super::InstructionRepositoryHealth::Uninitialized => {}
            super::InstructionRepositoryHealth::Ready => {
                let manifest = self.repositories.load_manifest(&global)?;
                if manifest.seed_version == super::INSTRUCTION_STORE_SEED_VERSION {
                    return Ok((global, false));
                }
            }
            super::InstructionRepositoryHealth::Damaged(damage) => {
                return Err(SystemPromptActivationError::Compatibility(format!(
                    "cannot read managed sources from {}: {}",
                    global.root.display(),
                    damage.detail
                )));
            }
        }
        let initialized = self.ensure_global_store()?;
        Ok((initialized.repository, initialized.created))
    }

    /// Compatibility callers retain their original project-before-global slot
    /// order. Primary activation uses the approved global-before-project order.
    pub fn legacy_preferred_tools(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<(Option<String>, usize), SystemPromptActivationError> {
        let working_dir = working_dir.unwrap_or(Path::new("."));
        let runtime =
            super::notification::occurrence_runtime(&self.repositories, Some(working_dir))?;
        let project_repository = self.repositories.resolve_project_repository(working_dir)?;
        let environment = CompositionEnvironment {
            runtime,
            global_manifest: self
                .repositories
                .load_manifest(&self.repositories.global_repository()?)?,
            project_manifest: project_repository
                .as_ref()
                .map(|repo| self.repositories.load_manifest(repo))
                .transpose()?,
            project_root: Some(self.repositories.resolve_project_root(working_dir)?),
            initialized_global_store: false,
        };
        let mut parts = Vec::new();
        let mut chars = 0usize;
        for scope in [InstructionScope::Project, InstructionScope::Global] {
            if let Some((text, size)) = preferred_tools_section(&environment, scope)? {
                parts.push(text);
                chars = chars.saturating_add(size);
            }
        }
        Ok(((!parts.is_empty()).then(|| parts.join("\n\n")), chars))
    }

    pub fn activate(
        &self,
        request: SystemPromptActivationRequest<'_>,
    ) -> Result<SystemPromptActivation, SystemPromptActivationError> {
        let environment = self.prepare_environment(request.working_dir)?;
        compose_activation(&environment, request)
    }

    /// Compose current source for inspection only. Unlike activation, this never
    /// initializes, upgrades, imports, persists, or replaces session state.
    pub fn preview(
        &self,
        request: SystemPromptActivationRequest<'_>,
    ) -> Result<SystemPromptActivation, SystemPromptActivationError> {
        let environment = self.inspect_environment(request.working_dir)?;
        compose_activation(&environment, request)
    }

    pub fn preview_agent_component(
        &self,
        working_dir: Option<&Path>,
        selection: AgentSelection,
    ) -> Result<String, SystemPromptActivationError> {
        let environment = self.inspect_environment(working_dir)?;
        let profile = render_agent_profile_with_availability(&environment, selection, None)?;
        let mut parts = vec![profile.text];
        push_project_addenda(&environment.runtime, &profile.resource, &mut parts)?;
        Ok(parts.join("\n\n"))
    }

    fn inspect_environment(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<CompositionEnvironment, SystemPromptActivationError> {
        let global = self.repositories.global_repository()?;
        if !matches!(
            self.repositories.inspect(&global)?.health,
            super::InstructionRepositoryHealth::Ready
        ) {
            return Err(SystemPromptActivationError::Compatibility(
                "Global instruction store is unavailable. Inspection never initializes or repairs stores.".into(),
            ));
        }
        self.environment_from_sources(working_dir, &global, false)
    }

    pub fn render_agent_transition(
        &self,
        working_dir: Option<&Path>,
        selection: AgentSelection,
    ) -> Result<AgentProfileTransition, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        let profile = render_agent_profile(&environment, selection)?;
        let mut parts = vec![profile.text];
        push_project_addenda(&environment.runtime, &profile.resource, &mut parts)?;
        let transition_sentence =
            render_notification(&environment.runtime, AGENT_TRANSITION_ID, &())?;
        Ok(AgentProfileTransition {
            agent: profile.agent,
            transition_sentence,
            complete_instructions: parts.join("\n\n"),
            initialized_global_store: environment.initialized_global_store,
        })
    }

    pub fn replace_system_prompt(
        &self,
        request: SystemPromptActivationRequest<'_>,
        previous_agent: &StoredAgentReference,
    ) -> Result<SystemPromptReplacement, SystemPromptActivationError> {
        let environment = self.prepare_environment(request.working_dir)?;
        let activation = compose_activation(&environment, request)?;
        let audit_sentence = render_notification(
            &environment.runtime,
            AGENT_REPLACEMENT_ID,
            &AgentReplacementValues {
                previous_agent: &previous_agent.display_name,
                new_agent: &activation.state.active_agent.display_name,
            },
        )?;
        Ok(SystemPromptReplacement {
            activation,
            audit_sentence,
        })
    }

    pub fn list_primary_agents(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<Vec<AgentCatalogEntry>, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        let mut entries = Vec::new();
        for scope in [InstructionScope::Global, InstructionScope::Project] {
            for document in environment.runtime.documents(InstructionKind::Agent, scope) {
                let Some(metadata) = document.metadata.agent.as_ref() else {
                    continue;
                };
                if !matches!(
                    metadata.availability,
                    AgentAvailability::Primary | AgentAvailability::Both
                ) {
                    continue;
                }
                entries.push(AgentCatalogEntry {
                    agent: StoredAgentReference {
                        scope,
                        id: document.id.to_string(),
                        display_name: document
                            .metadata
                            .display_name
                            .clone()
                            .unwrap_or_else(|| document.id.to_string()),
                    },
                    description: document.metadata.description.clone().unwrap_or_default(),
                });
            }
        }

        // Legacy project guidance can contribute a selectable compatibility
        // profile without a managed document. It is optional for catalog
        // listing: damage to that profile must not hide unrelated agents.
        // Explicit/default activation still reports the original source error.
        if let Ok(compatibility) = render_agent_profile(
            &environment,
            AgentSelection::Explicit(InstructionSelector::unqualified(
                InstructionKind::Agent,
                COMPATIBILITY_AGENT_ID,
            )?),
        ) && !entries
            .iter()
            .any(|entry| entry.agent == compatibility.agent)
        {
            entries.push(AgentCatalogEntry {
                agent: compatibility.agent,
                description: "Jcode compatibility agent".to_string(),
            });
        }
        entries.sort_by(|left, right| {
            left.agent
                .display_name
                .to_ascii_lowercase()
                .cmp(&right.agent.display_name.to_ascii_lowercase())
                .then_with(|| left.agent.scope.cmp(&right.agent.scope))
                .then_with(|| left.agent.id.cmp(&right.agent.id))
        });
        Ok(entries)
    }

    fn prepare_environment(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<CompositionEnvironment, SystemPromptActivationError> {
        let (global_repository, initialized_global_store) = self.prepare_global_store_for_read()?;
        self.environment_from_sources(working_dir, &global_repository, initialized_global_store)
    }

    fn environment_from_sources(
        &self,
        working_dir: Option<&Path>,
        global_repository: &super::InstructionRepositoryRef,
        initialized_global_store: bool,
    ) -> Result<CompositionEnvironment, SystemPromptActivationError> {
        let project_root = working_dir
            .map(|working_dir| self.repositories.resolve_project_root(working_dir))
            .transpose()?;
        let project_repository = working_dir
            .map(|working_dir| self.repositories.resolve_project_repository(working_dir))
            .transpose()?
            .flatten();

        let mut sources = self
            .repositories
            .instruction_sources(project_repository.as_ref())?;
        sources = sources.with_global_agents_md(self.repositories.global_agents_path()?);
        if let Some(root) = project_root.as_ref() {
            sources = sources.with_project_agents_md(root.join("AGENTS.md"));
        }
        let runtime = super::InstructionRuntime::discover(sources);
        let global_manifest = self.repositories.load_manifest(global_repository)?;
        let project_manifest = project_repository
            .as_ref()
            .map(|repository| self.repositories.load_manifest(repository))
            .transpose()?;
        Ok(CompositionEnvironment {
            runtime,
            global_manifest,
            project_manifest,
            project_root,
            initialized_global_store,
        })
    }
}

fn compose_activation(
    environment: &CompositionEnvironment,
    request: SystemPromptActivationRequest<'_>,
) -> Result<SystemPromptActivation, SystemPromptActivationError> {
    let profile = render_agent_profile(environment, request.selection)?;
    let mut parts = Vec::new();
    parts.push(render_required_system(&environment.runtime, KERNEL_ID)?);
    parts.push(profile.text);
    if request.capabilities.mermaid {
        parts.push(render_required_system(&environment.runtime, MERMAID_ID)?);
    }
    if request.is_selfdev {
        parts.push(prompt::build_selfdev_prompt_static_for_working_dir(
            request.working_dir,
        ));
    }

    for scope in [InstructionScope::Global, InstructionScope::Project] {
        if let Some((text, _)) = common_section(environment, scope)? {
            parts.push(text);
        }
    }

    let external_agents = environment.runtime.render_external_agents();
    if !external_agents.is_empty() {
        parts.push(external_agents);
    }

    push_project_addenda(&environment.runtime, &profile.resource, &mut parts)?;
    for scope in [InstructionScope::Global, InstructionScope::Project] {
        if let Some((section, _)) = preferred_tools_section(environment, scope)? {
            parts.push(section);
        }
    }
    if !request.available_skills.is_empty() {
        parts.push(render_available_skills(
            &environment.runtime,
            request.available_skills,
        )?);
    }

    Ok(SystemPromptActivation {
        state: StoredSystemPromptState {
            text: parts.join("\n\n"),
            active_agent: profile.agent,
            first_provider_dispatch_at: None,
            active_transition_message_id: None,
        },
        initialized_global_store: environment.initialized_global_store,
    })
}

fn render_agent_profile(
    environment: &CompositionEnvironment,
    selection: AgentSelection,
) -> Result<RenderedAgentProfile, SystemPromptActivationError> {
    render_agent_profile_with_availability(environment, selection, Some(AgentAvailability::Primary))
}

fn render_agent_profile_with_availability(
    environment: &CompositionEnvironment,
    selection: AgentSelection,
    requested: Option<AgentAvailability>,
) -> Result<RenderedAgentProfile, SystemPromptActivationError> {
    let selection = resolve_selection(
        selection,
        environment.project_manifest.as_ref(),
        &environment.global_manifest,
    )?;
    let project_system_imported = environment
        .project_manifest
        .as_ref()
        .is_some_and(|manifest| {
            manifest
                .legacy_imports
                .values()
                .any(|receipt| receipt.source_kind == LegacyInstructionSourceKind::SystemPrompt)
        });
    let project_legacy_allowed = selection.id.as_str() == COMPATIBILITY_AGENT_ID
        && !matches!(selection.scope, super::InstructionScopeSelector::Global);
    let project_legacy_prompt = if project_system_imported || !project_legacy_allowed {
        None
    } else {
        environment
            .project_root
            .as_ref()
            .map(|root| read_nonblank(root.join(".jcode/system-prompt.md")))
            .transpose()?
            .flatten()
    };
    let use_project_legacy = if project_legacy_prompt.is_some() {
        let project_selector =
            InstructionSelector::project(InstructionKind::Agent, COMPATIBILITY_AGENT_ID)?;
        match environment.runtime.resolve(&project_selector) {
            Ok(_) => {
                return Err(SystemPromptActivationError::Compatibility(
                    "project legacy system-prompt.md and managed project:jcode both exist without an import receipt"
                        .to_string(),
                ));
            }
            Err(InstructionError::ResourceNotFound { .. }) => true,
            Err(error) => return Err(error.into()),
        }
    } else {
        false
    };
    let render_selection = if use_project_legacy {
        InstructionSelector::global(InstructionKind::Agent, COMPATIBILITY_AGENT_ID)?
    } else {
        selection
    };
    let mut rendered = environment.runtime.render_agent(
        &render_selection,
        requested.unwrap_or_else(|| {
            environment
                .runtime
                .resolve(&render_selection)
                .ok()
                .and_then(|document| {
                    document
                        .metadata
                        .agent
                        .as_ref()
                        .map(|agent| agent.availability)
                })
                .unwrap_or(AgentAvailability::Primary)
        }),
        &(),
    )?;
    let document = environment.runtime.resolve(&render_selection)?;
    let display_name = document
        .metadata
        .display_name
        .clone()
        .unwrap_or_else(|| rendered.root.id.to_string());
    if environment
        .global_manifest
        .legacy_imports
        .contains_key("global-system-prompt")
        && rendered.root.scope == InstructionScope::Global
        && rendered.root.id.as_str() == COMPATIBILITY_AGENT_ID
    {
        rendered.text = rendered.text.trim().to_string();
    }
    if use_project_legacy && let Some(project_prompt) = project_legacy_prompt {
        rendered.text = project_prompt.trim().to_string();
        rendered.root.scope = InstructionScope::Project;
    }
    Ok(RenderedAgentProfile {
        agent: StoredAgentReference {
            scope: rendered.root.scope,
            id: rendered.root.id.to_string(),
            display_name,
        },
        resource: rendered.root,
        text: rendered.text,
    })
}

fn render_notification<T: Serialize>(
    runtime: &super::InstructionRuntime,
    id: &str,
    values: &T,
) -> Result<String, InstructionError> {
    InstructionConsumer::<T>::new(profile_notification_registration(id)?)
        .render(runtime, values)
        .map(|rendered| rendered.text)
}

fn profile_notification_registration(id: &str) -> Result<ConsumerRegistration, InstructionError> {
    let path = match id {
        AGENT_TRANSITION_ID => "notifications/agent-transition.md",
        AGENT_REPLACEMENT_ID => "notifications/agent-replacement.md",
        _ => {
            return Err(InstructionError::InvalidId {
                value: id.to_string(),
                reason: "unregistered profile notification".to_string(),
            });
        }
    };
    ConsumerRegistration::new(
        format!("agent-profile-{id}"),
        id,
        InstructionKind::Notification,
        path,
        "session agent-profile lifecycle",
        "Managed profile transition or true-system replacement prose; session owns framing, structural identity, persistence, and cache behavior.",
    )
}

fn render_available_skills(
    runtime: &super::InstructionRuntime,
    skills: &[SkillInfo],
) -> Result<String, InstructionError> {
    let rows = skills
        .iter()
        .map(|skill| format!("\n- `/{} ` - {}", skill.name, skill.description))
        .collect::<String>();
    InstructionConsumer::<AvailableSkillsValues>::new(available_skills_registration()?)
        .render(runtime, &AvailableSkillsValues { skills: rows })
        .map(|rendered| rendered.text)
}

fn available_skills_registration() -> Result<ConsumerRegistration, InstructionError> {
    let mut registration = ConsumerRegistration::new(
        "available-skills-catalog",
        AVAILABLE_SKILLS_ID,
        InstructionKind::System,
        "system/available-skills.md",
        "primary activation and unprofiled compatibility composition",
        "Managed available-skills prose; activation supplies the sorted effective skill names and descriptions and freezes the complete result.",
    )?;
    registration.required = true;
    Ok(registration)
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

fn common_registration(scope: InstructionScope) -> Result<ConsumerRegistration, InstructionError> {
    let mut registration = ConsumerRegistration::new(
        format!("common-{scope}"),
        COMMON_ID,
        InstructionKind::System,
        "system/common.md",
        "paired common system sections",
        "Both scopes contribute independently. Primary composition is global-first; unprofiled compatibility callers retain project-first order.",
    )?;
    registration.required = false;
    registration.scope_policy = match scope {
        InstructionScope::Global => ConsumerScopePolicy::GlobalOnly,
        InstructionScope::Project => ConsumerScopePolicy::ProjectOnly,
    };
    Ok(registration)
}

fn common_section(
    environment: &CompositionEnvironment,
    scope: InstructionScope,
) -> Result<Option<(String, usize)>, SystemPromptActivationError> {
    let manifest = match scope {
        InstructionScope::Global => Some(&environment.global_manifest),
        InstructionScope::Project => environment.project_manifest.as_ref(),
    };
    let imported = manifest.is_some_and(|manifest| {
        manifest
            .legacy_imports
            .values()
            .any(|receipt| receipt.source_kind == LegacyInstructionSourceKind::PromptOverlay)
    });
    let legacy = if scope == InstructionScope::Project && !imported {
        environment
            .project_root
            .as_ref()
            .map(|root| read_present(root.join(".jcode/prompt-overlay.md")))
            .transpose()?
            .flatten()
    } else {
        None
    };
    if let Some(legacy) = legacy {
        match environment.runtime.resolve(&InstructionSelector::project(InstructionKind::System, COMMON_ID)?) {
            Ok(_) => return Err(SystemPromptActivationError::Compatibility(
                "project legacy prompt-overlay.md and managed project:common both exist without an import receipt".into(),
            )),
            Err(InstructionError::ResourceNotFound { .. }) => return Ok(Some((
                format!("# Project Prompt Overlay (.jcode/prompt-overlay.md)\n\n{}", legacy.trim()),
                legacy.len(),
            ))),
            Err(error) => return Err(error.into()),
        }
    }
    let mut registration = common_registration(scope)?;
    // An import receipt makes its destination mandatory. Missing is damage,
    // not an empty optional layer and never a reason to revive the old file.
    registration.required = imported;
    let rendered = match environment.runtime.render_registered(&registration, &()) {
        Ok(rendered) => rendered.text,
        Err(InstructionError::ResourceNotFound { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let size = rendered.len();
    if imported {
        let heading = match scope {
            InstructionScope::Global => "# Global Prompt Overlay (~/.jcode/prompt-overlay.md)",
            InstructionScope::Project => "# Project Prompt Overlay (.jcode/prompt-overlay.md)",
        };
        Ok(Some((format!("{heading}\n\n{}", rendered.trim()), size)))
    } else {
        Ok((!rendered.is_empty()).then_some((rendered, size)))
    }
}

fn push_project_addenda(
    runtime: &super::InstructionRuntime,
    active_agent: &InstructionResourceRef,
    parts: &mut Vec<String>,
) -> Result<(), InstructionError> {
    for addendum in runtime.applicable_project_addenda(active_agent)? {
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

pub fn preferred_tools_registration(
    scope: InstructionScope,
) -> Result<ConsumerRegistration, InstructionError> {
    let mut registration = ConsumerRegistration::new(
        format!("preferred-tools-{scope}"),
        "preferred-tools",
        InstructionKind::ToolGuidance,
        "tools/preferred-tools.md",
        "preferred tool system section",
        "Paired additive preferred-tool guidance. Each explicit scope contributes independently; project-only lookup must not fall back to global.",
    )?;
    registration.scope_policy = match scope {
        InstructionScope::Global => ConsumerScopePolicy::GlobalOnly,
        InstructionScope::Project => ConsumerScopePolicy::ProjectOnly,
    };
    registration.required = false;
    Ok(registration)
}

fn preferred_tools_section(
    environment: &CompositionEnvironment,
    scope: InstructionScope,
) -> Result<Option<(String, usize)>, SystemPromptActivationError> {
    let manifest = match scope {
        InstructionScope::Global => Some(&environment.global_manifest),
        InstructionScope::Project => environment.project_manifest.as_ref(),
    };
    let imported = manifest.is_some_and(|manifest| {
        manifest
            .legacy_imports
            .values()
            .any(|receipt| receipt.source_kind == LegacyInstructionSourceKind::PreferredTools)
    });
    let mut registration = preferred_tools_registration(scope)?;
    registration.required = imported;
    let (content, legacy) = match environment.runtime.render_registered(&registration, &()) {
        Ok(rendered) => (rendered.text, imported),
        Err(InstructionError::ResourceNotFound { .. })
            if scope == InstructionScope::Project && !imported =>
        {
            let Some(root) = environment.project_root.as_ref() else {
                return Ok(None);
            };
            let Some(content) = read_present(root.join(".jcode/preferred-tools.md"))? else {
                return Ok(None);
            };
            (content, true)
        }
        Err(InstructionError::ResourceNotFound { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let size = content.len();
    let label = match (scope, legacy) {
        (InstructionScope::Global, true) => "Global Preferred Tools (~/.jcode/preferred-tools.md)",
        (InstructionScope::Project, true) => "Project Preferred Tools (.jcode/preferred-tools.md)",
        (InstructionScope::Global, false) => "Global Preferred Tools",
        (InstructionScope::Project, false) => "Project Preferred Tools",
    };
    Ok(Some((
        format!(
            "# {label}\n\n{}",
            if legacy { content.trim() } else { &content }
        ),
        size,
    )))
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
    let mut documents = vec![
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
        InstructionDocument {
            id: InstructionId::parse(AVAILABLE_SKILLS_ID)?,
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Handlebars,
            metadata: InstructionMetadata::default(),
            body: AVAILABLE_SKILLS_PROSE.to_string(),
            path: PathBuf::from("system/available-skills.md"),
        },
        compatibility_agent_document()?,
        InstructionDocument {
            id: InstructionId::parse(AGENT_TRANSITION_ID)?,
            kind: InstructionKind::Notification,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
            body: AGENT_TRANSITION_PROSE.to_string(),
            path: PathBuf::from("notifications/agent-transition.md"),
        },
        InstructionDocument {
            id: InstructionId::parse(AGENT_REPLACEMENT_ID)?,
            kind: InstructionKind::Notification,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Handlebars,
            metadata: InstructionMetadata::default(),
            body: AGENT_REPLACEMENT_PROSE.to_string(),
            path: PathBuf::from("notifications/agent-replacement.md"),
        },
    ];
    documents.extend(super::notification::seed_documents()?);
    documents.extend(super::notification::module_seed_documents()?);
    documents.extend(super::workflow::seed_documents()?);
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
            .collect::<Result<Vec<_>, InstructionError>>()?
            .into_iter()
            .chain(std::iter::once(InstructionSeedFile {
                relative_path: PathBuf::from(crate::model_roster::ROSTER_PATH),
                content: crate::model_roster::SHIPPED_ROSTER.as_bytes().to_vec(),
            }))
            .collect(),
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

pub fn known_legacy_import(
    scope: InstructionScope,
    directory: &Path,
    kind: LegacyInstructionSourceKind,
) -> Result<InstructionLegacyImportSpec, InstructionError> {
    let (filename, suffix, target) = match kind {
        LegacyInstructionSourceKind::SystemPrompt => {
            let mut document = compatibility_agent_document()?;
            document.scope = scope;
            (
                "system-prompt.md",
                "system-prompt",
                legacy_target(&document),
            )
        }
        LegacyInstructionSourceKind::PromptOverlay => (
            "prompt-overlay.md",
            "prompt-overlay",
            InstructionLegacyImportTarget {
                relative_path: "system/common.md".into(),
                id: InstructionId::parse(COMMON_ID)?,
                kind: InstructionKind::System,
                scope,
                template_mode: TemplateMode::Plain,
                metadata: InstructionMetadata::default(),
            },
        ),
        LegacyInstructionSourceKind::PreferredTools => (
            "preferred-tools.md",
            "preferred-tools",
            InstructionLegacyImportTarget {
                relative_path: "tools/preferred-tools.md".into(),
                id: InstructionId::parse("preferred-tools")?,
                kind: InstructionKind::ToolGuidance,
                scope,
                template_mode: TemplateMode::Plain,
                metadata: InstructionMetadata::default(),
            },
        ),
        LegacyInstructionSourceKind::SwarmPrompt => (
            "swarm-prompt.md",
            "swarm-routing",
            InstructionLegacyImportTarget {
                relative_path: "tools/swarm-routing.md".into(),
                id: InstructionId::parse("swarm-routing")?,
                kind: InstructionKind::ToolGuidance,
                scope,
                template_mode: TemplateMode::Plain,
                metadata: InstructionMetadata::default(),
            },
        ),
        _ => {
            return Err(InstructionError::InvalidDocument {
                path: directory.into(),
                detail: "Not a known compatibility import".into(),
            });
        }
    };
    Ok(InstructionLegacyImportSpec {
        import_id: format!("{scope}-{suffix}"),
        source_kind: kind,
        source_path: directory.join(filename),
        target,
    })
}
fn global_legacy_imports(
    repositories: &InstructionRepositoryService,
    prior: Option<&InstructionStoreManifest>,
) -> Result<Vec<InstructionLegacyImportSpec>, SystemPromptActivationError> {
    let root = repositories.global_repository()?.root;
    let home = root.parent().ok_or_else(|| {
        SystemPromptActivationError::Compatibility(
            "global instruction repository has no Jcode home parent".into(),
        )
    })?;
    let mut imports = Vec::new();
    for kind in [
        LegacyInstructionSourceKind::SystemPrompt,
        LegacyInstructionSourceKind::PromptOverlay,
        LegacyInstructionSourceKind::PreferredTools,
        LegacyInstructionSourceKind::SwarmPrompt,
    ] {
        let spec = known_legacy_import(InstructionScope::Global, home, kind)?;
        let allowed = match kind {
            LegacyInstructionSourceKind::PreferredTools => prior.is_none_or(|manifest| {
                manifest.seed_version < PREFERRED_TOOLS_CUTOVER_SEED
                    && !manifest.legacy_imports.contains_key(&spec.import_id)
            }),
            LegacyInstructionSourceKind::SwarmPrompt => prior.is_none_or(|manifest| {
                manifest.seed_version < 26 && !manifest.legacy_imports.contains_key(&spec.import_id)
            }),
            _ => true,
        };
        if !allowed {
            continue;
        }
        let content = if matches!(
            kind,
            LegacyInstructionSourceKind::SystemPrompt | LegacyInstructionSourceKind::SwarmPrompt
        ) {
            read_nonblank(spec.source_path.clone())?
        } else {
            read_present(spec.source_path.clone())?
        };
        if content.is_some() {
            imports.push(spec);
        }
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

/// Current compatibility source eligibility. This belongs beside the composer,
/// not in UI precedence code. Global legacy input is imported at bootstrap;
/// project compatibility is active only when its managed target is absent.
pub(crate) fn legacy_source_eligibility(
    runtime: &super::InstructionRuntime,
    scope: InstructionScope,
    kind: LegacyInstructionSourceKind,
    imported: bool,
) -> Result<(bool, &'static str), SystemPromptActivationError> {
    if imported {
        return Ok((
            false,
            "Imported and inactive. The managed source is authoritative; the original is retained.",
        ));
    }
    if scope == InstructionScope::Global {
        return Ok((
            false,
            "Inactive compatibility file. An initialized global store uses managed sources, not a newly added legacy file.",
        ));
    }
    let selector = match kind {
        LegacyInstructionSourceKind::SystemPrompt => {
            InstructionSelector::project(InstructionKind::Agent, COMPATIBILITY_AGENT_ID)?
        }
        LegacyInstructionSourceKind::PromptOverlay => {
            InstructionSelector::project(InstructionKind::System, COMMON_ID)?
        }
        LegacyInstructionSourceKind::PreferredTools => InstructionSelector::project(
            InstructionKind::ToolGuidance,
            preferred_tools_registration(scope)?.id.to_string(),
        )?,
        LegacyInstructionSourceKind::SwarmPrompt => InstructionSelector::project(
            InstructionKind::ToolGuidance,
            super::workflow::Workflow::SwarmRouting
                .registration()?
                .id
                .to_string(),
        )?,
        LegacyInstructionSourceKind::InventoryApproved => {
            return Ok((false, "No active compatibility consumer registered."));
        }
    };
    match runtime.resolve(&selector) {
        Err(InstructionError::ResourceNotFound { .. }) => Ok((
            true,
            "Eligible project compatibility input for its owning consumer. Full system preview shows the selected profile's exact contribution.",
        )),
        Ok(_) if matches!(kind, LegacyInstructionSourceKind::SystemPrompt | LegacyInstructionSourceKind::PromptOverlay) => Err(SystemPromptActivationError::Compatibility(
            "Project legacy source and its managed target both exist without an import receipt. Affected system composition is blocked.".into(),
        )),
        Ok(_) => Ok((
            false,
            "Inactive: the managed project target takes precedence.",
        )),
        Err(error) => Err(error.into()),
    }
}

/// Code-owned composition consumers, shared with central inspection.
pub fn composition_registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
    let mut registrations = vec![
        available_skills_registration()?,
        profile_notification_registration(AGENT_TRANSITION_ID)?,
        profile_notification_registration(AGENT_REPLACEMENT_ID)?,
    ];
    for id in [KERNEL_ID, MERMAID_ID] {
        registrations.push(ConsumerRegistration::new(format!("primary-system-{id}"), id, InstructionKind::System, format!("system/{id}.md"), "primary system composer", "Required when the owning composition slot is selected; capability policy remains code-owned.")?);
    }
    registrations.push(ConsumerRegistration::new("compatibility-agent", COMPATIBILITY_AGENT_ID, InstructionKind::Agent, "agents/jcode.md", "primary compatibility selection and unprofiled compatibility body", "Named activation enforces availability. Unprofiled callers consume the compatibility body without adopting profile policy. Other agent identities remain independent.")?);
    for scope in [InstructionScope::Global, InstructionScope::Project] {
        registrations.push(common_registration(scope)?);
        registrations.push(preferred_tools_registration(scope)?);
    }
    Ok(registrations)
}

pub(crate) fn paired_composition_resource(kind: InstructionKind, id: &InstructionId) -> bool {
    (kind == InstructionKind::System && id.as_str() == COMMON_ID)
        || (kind == InstructionKind::ToolGuidance && id.as_str() == "preferred-tools")
}

pub(crate) fn composition_preview_values(
    id: &str,
) -> Result<Option<serde_json::Value>, serde_json::Error> {
    match id {
        AGENT_REPLACEMENT_ID => serde_json::to_value(AgentReplacementValues {
            previous_agent: "",
            new_agent: "",
        })
        .map(Some),
        AVAILABLE_SKILLS_ID => serde_json::to_value(AvailableSkillsValues {
            skills: String::new(),
        })
        .map(Some),
        _ => Ok(None),
    }
}

pub(crate) fn render_inspection_resource(
    runtime: &super::InstructionRuntime,
    selector: &InstructionSelector,
    skills: &[SkillInfo],
) -> Result<String, InstructionError> {
    if selector.kind == InstructionKind::System && selector.id.as_str() == AVAILABLE_SKILLS_ID {
        let values = AvailableSkillsValues {
            skills: skills
                .iter()
                .map(|skill| format!("\n- `/{} ` - {}", skill.name, skill.description))
                .collect(),
        };
        runtime
            .render(selector, &values)
            .map(|rendered| rendered.text)
    } else {
        runtime
            .render(selector, &serde_json::json!({}))
            .map(|rendered| rendered.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::AddendumMetadata;

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
    fn agent_catalog_isolates_unavailable_compatibility_profile() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer.ensure_global_store().unwrap();
        let root = fixture.jcode_home.join("instructions");
        let alternate = document(
            InstructionScope::Global,
            InstructionKind::Agent,
            "alternate",
            "agents/alternate.md",
            "SYNTHETIC ALTERNATE",
        );
        std::fs::write(
            root.join("agents/alternate.md"),
            alternate.to_markdown().unwrap(),
        )
        .unwrap();
        let compatibility = root.join("agents/jcode.md");
        for damaged in [Some("---\nid: jcode\nkind: agent\n---\nINVALID"), None] {
            if let Some(content) = damaged {
                std::fs::write(&compatibility, content).unwrap();
            } else {
                std::fs::remove_file(&compatibility).unwrap();
            }
            let selected = composer
                .activate(fixture.request(AgentSelection::parse(Some("global:alternate")).unwrap()))
                .expect("unrelated valid agent still activates");
            let catalog = composer
                .list_primary_agents(Some(&fixture.project))
                .expect("catalog must allow recovery through another valid agent");
            assert_eq!(catalog.len(), 1);
            assert_eq!(catalog[0].agent, selected.state.active_agent);
            assert!(
                composer
                    .activate(fixture.request(AgentSelection::Default))
                    .is_err()
            );
        }
        std::fs::write(root.join("instruction-store.toml"), "invalid manifest").unwrap();
        assert!(
            composer
                .list_primary_agents(Some(&fixture.project))
                .is_err()
        );
    }

    #[test]
    fn compatibility_composition_reads_managed_sources_without_adopting_profiles() {
        let fixture = Fixture::new();
        std::fs::write(fixture.jcode_home.join("system-prompt.md"), "OLD BASE").unwrap();
        std::fs::write(fixture.jcode_home.join("prompt-overlay.md"), "OLD COMMON").unwrap();
        let composer = fixture.composer();
        let skills = vec![SkillInfo {
            name: "synthetic".into(),
            description: "fixture".into(),
        }];
        let render = || {
            composer.compatibility_split(
                None,
                &skills,
                false,
                None,
                Some(&fixture.project),
                PromptCapabilities { mermaid: true },
            )
        };
        let original = render().unwrap().0;
        assert!(original.static_part.contains("OLD BASE"));
        let root = fixture.jcode_home.join("instructions");
        for (kind, id, path, text) in [
            (
                InstructionKind::Agent,
                "jcode",
                "agents/jcode.md",
                "NEW BASE",
            ),
            (
                InstructionKind::System,
                "common",
                "system/common.md",
                "NEW COMMON",
            ),
            (
                InstructionKind::System,
                "mermaid",
                "system/mermaid.md",
                "NEW CAPABILITY",
            ),
            (
                InstructionKind::System,
                "available-skills",
                "system/available-skills.md",
                "NEW CATALOG {{skills}}",
            ),
            (
                InstructionKind::System,
                "kernel",
                "system/kernel.md",
                "PRIMARY KERNEL ONLY",
            ),
        ] {
            let mut source = document(InstructionScope::Global, kind, id, path, text);
            if id == "jcode" {
                source.metadata.agent.as_mut().unwrap().availability = AgentAvailability::Isolated;
            }
            if id == "available-skills" {
                source.template_mode = TemplateMode::Handlebars;
            }
            std::fs::write(root.join(path), source.to_markdown().unwrap()).unwrap();
        }
        // Inactive originals cannot regain authority or cause parse failures.
        std::fs::write(fixture.jcode_home.join("system-prompt.md"), "INACTIVE BASE").unwrap();
        std::fs::write(fixture.jcode_home.join("prompt-overlay.md"), [0xff]).unwrap();
        let current = render().unwrap().0;
        for expected in [
            "NEW BASE",
            "NEW COMMON",
            "NEW CAPABILITY",
            "NEW CATALOG",
            "synthetic",
        ] {
            assert!(current.static_part.contains(expected), "{expected}");
        }
        for absent in [
            "OLD BASE",
            "OLD COMMON",
            "INACTIVE BASE",
            "PRIMARY KERNEL ONLY",
        ] {
            assert!(!current.static_part.contains(absent), "{absent}");
        }
        assert!(current.dynamic_part.is_empty());
        std::fs::write(root.join("system/available-skills.md"), "invalid").unwrap();
        assert!(render().is_err());
        assert!(
            composer
                .compatibility_split(
                    None,
                    &[],
                    false,
                    None,
                    Some(&fixture.project),
                    PromptCapabilities { mermaid: false }
                )
                .is_ok()
        );
    }

    #[test]
    fn compatibility_uses_canonical_project_sources_and_ignores_primary_defaults() {
        let fixture = Fixture::new();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(&fixture.project)
                .status()
                .unwrap()
                .success()
        );
        let nested = fixture.project.join("nested");
        for (directory, body) in [
            (&fixture.project, "ROOT SOURCE"),
            (&nested, "NESTED SOURCE"),
        ] {
            std::fs::create_dir_all(directory.join(".jcode")).unwrap();
            std::fs::write(directory.join(".jcode/system-prompt.md"), body).unwrap();
            std::fs::write(directory.join("AGENTS.md"), format!("ECOSYSTEM {body}")).unwrap();
        }
        let composer = fixture.composer();
        let initialized = composer.ensure_global_store().unwrap();
        let mut manifest = fixture
            .service()
            .load_manifest(&initialized.repository)
            .unwrap();
        manifest.default_agent = Some("absent-primary-default".into());
        std::fs::write(
            initialized.repository.root.join("instruction-store.toml"),
            toml::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let (text, _) = composer
            .compatibility_full(
                None,
                &[],
                false,
                None,
                Some(&nested),
                PromptCapabilities { mermaid: false },
            )
            .unwrap();
        assert!(text.starts_with("ROOT SOURCE"));
        assert!(text.contains("ECOSYSTEM ROOT SOURCE"));
        assert!(!text.contains("NESTED SOURCE"));
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_err()
        );
    }

    #[test]
    fn imported_common_is_required_in_primary_and_compatibility_composition() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.jcode_home.join("prompt-overlay.md"),
            "IMPORTED GLOBAL",
        )
        .unwrap();
        let composer = fixture.composer();
        composer.ensure_global_store().unwrap();
        let global = fixture.jcode_home.join("instructions/system/common.md");
        let saved = std::fs::read(&global).unwrap();
        std::fs::remove_file(&global).unwrap();
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_err()
        );
        assert!(
            composer
                .compatibility_full(
                    None,
                    &[],
                    false,
                    None,
                    Some(&fixture.project),
                    PromptCapabilities { mermaid: false }
                )
                .is_err()
        );
        std::fs::write(&global, saved).unwrap();

        let legacy = fixture.project.join(".jcode");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("prompt-overlay.md"), "IMPORTED PROJECT").unwrap();
        let import = known_legacy_import(
            InstructionScope::Project,
            &legacy,
            LegacyInstructionSourceKind::PromptOverlay,
        )
        .unwrap();
        let configured = fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "project-common-fixture",
                None,
                &InstructionStoreSeed::empty(),
                &[import],
            )
            .unwrap();
        let project_common = configured.repository.root.join("system/common.md");
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_ok()
        );
        std::fs::remove_file(&project_common).unwrap();
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_err()
        );
        assert!(
            composer
                .compatibility_split(
                    None,
                    &[],
                    false,
                    None,
                    Some(&fixture.project),
                    PromptCapabilities { mermaid: false }
                )
                .is_err()
        );
        let empty = document(
            InstructionScope::Project,
            InstructionKind::System,
            "common",
            "system/common.md",
            "",
        );
        std::fs::write(project_common, empty.to_markdown().unwrap()).unwrap();
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_ok()
        );
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
    fn ordinary_activation_isolates_invalid_roster_but_selected_sources_and_publication_still_fail()
    {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        let first = composer
            .activate(fixture.request(AgentSelection::Default))
            .unwrap();
        let path = fixture.jcode_home.join("instructions/model-roster.toml");
        std::fs::write(
            path,
            "[aliases.unused]\ndescription='synthetic'\nmodels=[]\n",
        )
        .unwrap();
        let second = composer
            .activate(fixture.request(AgentSelection::Default))
            .unwrap();
        assert_eq!(first.state.text, second.state.text);
        assert!(!second.initialized_global_store);
        assert!(
            composer.ensure_global_store().is_err(),
            "explicit initialization reuse retains full validation"
        );
        std::fs::remove_file(fixture.jcode_home.join("instructions/system/kernel.md")).unwrap();
        assert!(
            composer
                .activate(fixture.request(AgentSelection::Default))
                .is_err(),
            "selected required resource damage must still fail"
        );
    }

    #[test]
    fn available_skills_use_managed_prose_and_typed_activation_values() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize");
        let mut resource = document(
            InstructionScope::Global,
            InstructionKind::System,
            AVAILABLE_SKILLS_ID,
            "system/available-skills.md",
            "SYNTHETIC_SKILL_CATALOG{{skills}}",
        );
        resource.template_mode = TemplateMode::Handlebars;
        std::fs::write(
            fixture
                .jcode_home
                .join("instructions/system/available-skills.md"),
            resource.to_markdown().expect("serialize"),
        )
        .expect("managed available skills");
        let skills = vec![
            SkillInfo {
                name: "alpha".to_string(),
                description: "Alpha description".to_string(),
            },
            SkillInfo {
                name: "beta".to_string(),
                description: "Beta description".to_string(),
            },
        ];
        let activation = composer
            .activate(SystemPromptActivationRequest {
                working_dir: Some(&fixture.project),
                selection: AgentSelection::Default,
                is_selfdev: false,
                capabilities: PromptCapabilities { mermaid: false },
                available_skills: &skills,
            })
            .expect("activation with skills");
        assert!(activation.state.text.contains("SYNTHETIC_SKILL_CATALOG"));
        assert!(
            activation
                .state
                .text
                .contains("- `/alpha ` - Alpha description")
        );
        assert!(
            activation
                .state
                .text
                .contains("- `/beta ` - Beta description")
        );
    }

    #[test]
    fn transition_replacement_and_catalog_reuse_current_profile_resolution() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        let initial = composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initial activation");
        let global_root = initial
            .state
            .active_agent
            .scope
            .eq(&InstructionScope::Global)
            .then(|| fixture.jcode_home.join("instructions"))
            .expect("global compatibility activation");
        std::fs::write(
            global_root.join("system/kernel.md"),
            document(
                InstructionScope::Global,
                InstructionKind::System,
                KERNEL_ID,
                "system/kernel.md",
                "SYNTHETIC_KERNEL_ONLY_FOR_FULL_COMPOSITION",
            )
            .to_markdown()
            .expect("serialize kernel"),
        )
        .expect("write synthetic kernel");
        std::fs::create_dir_all(global_root.join("modules")).expect("create modules");
        std::fs::write(
            global_root.join("modules/transition-module.md"),
            document(
                InstructionScope::Global,
                InstructionKind::Module,
                "transition-module",
                "modules/transition-module.md",
                "SYNTHETIC_TRANSITION_MODULE",
            )
            .to_markdown()
            .expect("serialize module"),
        )
        .expect("write module");
        let mut agent = document(
            InstructionScope::Global,
            InstructionKind::Agent,
            "reviewer",
            "agents/reviewer.md",
            "SYNTHETIC_TRANSITION_AGENT",
        );
        agent.metadata.display_name = Some("Synthetic Reviewer".to_string());
        agent.metadata.includes = vec![
            InstructionSelector::unqualified(InstructionKind::Module, "transition-module")
                .expect("module selector"),
        ];
        std::fs::write(
            global_root.join("agents/reviewer.md"),
            agent.to_markdown().expect("serialize agent"),
        )
        .expect("write agent");

        let selection = AgentSelection::Explicit(
            InstructionSelector::global(InstructionKind::Agent, "reviewer")
                .expect("reviewer selector"),
        );
        let transition = composer
            .render_agent_transition(Some(&fixture.project), selection.clone())
            .expect("render transition");
        assert_eq!(transition.agent.id, "reviewer");
        assert_eq!(transition.agent.display_name, "Synthetic Reviewer");
        assert!(
            transition
                .complete_instructions
                .starts_with("SYNTHETIC_TRANSITION_MODULE")
        );
        assert!(
            transition
                .complete_instructions
                .contains("SYNTHETIC_TRANSITION_AGENT")
        );
        assert!(
            !transition
                .complete_instructions
                .contains("SYNTHETIC_KERNEL_ONLY_FOR_FULL_COMPOSITION")
        );

        let replacement = composer
            .replace_system_prompt(fixture.request(selection), &initial.state.active_agent)
            .expect("render replacement");
        assert_eq!(replacement.activation.state.active_agent.id, "reviewer");
        assert!(
            replacement
                .activation
                .state
                .text
                .contains("SYNTHETIC_TRANSITION_AGENT")
        );
        assert!(
            replacement
                .activation
                .state
                .text
                .contains("SYNTHETIC_KERNEL_ONLY_FOR_FULL_COMPOSITION")
        );
        assert!(replacement.audit_sentence.contains("Synthetic Reviewer"));

        let catalog = composer
            .list_primary_agents(Some(&fixture.project))
            .expect("list agents");
        assert!(catalog.iter().any(|entry| entry.agent.id == "reviewer"));
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

    #[test]
    fn applicable_invalid_and_ambiguous_addenda_fail_but_unrelated_damage_is_isolated() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize global");
        let mut manifest = InstructionStoreManifest::current();
        manifest.default_agent = Some("project:builder".to_string());
        let repository = fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "setup-addendum-validation",
                None,
                &seed(
                    manifest,
                    vec![
                        document(
                            InstructionScope::Project,
                            InstructionKind::Agent,
                            "builder",
                            "agents/builder.md",
                            "SYNTHETIC_BUILDER",
                        ),
                        document(
                            InstructionScope::Project,
                            InstructionKind::Agent,
                            "other",
                            "agents/other.md",
                            "SYNTHETIC_OTHER",
                        ),
                    ],
                ),
                &[],
            )
            .expect("project repository");
        std::fs::create_dir_all(repository.repository.root.join("addenda"))
            .expect("addenda directory");
        std::fs::write(
            repository.repository.root.join("addenda/unrelated.md"),
            "---\nid: unrelated\nkind: agent-addendum\ntarget: project:other\nunknown: rejected\n---\n\nUNRELATED",
        )
        .expect("write unrelated invalid addendum");
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("unrelated invalid addendum remains isolated");

        std::fs::write(
            repository.repository.root.join("addenda/applicable.md"),
            "---\nid: applicable\nkind: agent-addendum\ntarget: project:builder\nunknown: rejected\n---\n\nAPPLICABLE",
        )
        .expect("write applicable invalid addendum");
        let invalid = composer.activate(fixture.request(AgentSelection::Default));
        assert!(matches!(
            invalid,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::InvalidResource { .. }
            ))
        ));
        std::fs::remove_file(repository.repository.root.join("addenda/applicable.md"))
            .expect("remove invalid addendum");

        for name in ["ambiguous-a.md", "ambiguous-b.md"] {
            std::fs::write(
                repository.repository.root.join("addenda").join(name),
                "---\nid: ambiguous\nkind: agent-addendum\ntarget: project:builder\n---\n\nAMBIGUOUS",
            )
            .expect("write ambiguous addendum");
        }
        let ambiguous = composer.activate(fixture.request(AgentSelection::Default));
        assert!(matches!(
            ambiguous,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::AmbiguousResource { .. }
            ))
        ));
    }

    #[test]
    fn compatibility_fallback_requires_true_absence_and_respects_explicit_global_scope() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize global");
        let repository = fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "setup-specificity-validation",
                None,
                &seed(
                    InstructionStoreManifest::current(),
                    vec![document(
                        InstructionScope::Project,
                        InstructionKind::Agent,
                        COMPATIBILITY_AGENT_ID,
                        "agents/jcode.md",
                        "SYNTHETIC_PROJECT_MANAGED",
                    )],
                ),
                &[],
            )
            .expect("project repository");
        std::fs::create_dir_all(fixture.project.join(".jcode")).expect("project jcode dir");
        std::fs::write(
            fixture.project.join(".jcode/system-prompt.md"),
            "SYNTHETIC_PROJECT_LEGACY",
        )
        .expect("write legacy project prompt");
        std::fs::write(
            repository.repository.root.join("agents/jcode.md"),
            "---\nid: jcode\nkind: agent\nname: Broken\ndescription: broken\navailability: both\nunknown: rejected\n---\n\nBROKEN",
        )
        .expect("damage managed project agent");

        let unqualified = composer.activate(fixture.request(AgentSelection::Default));
        assert!(matches!(
            unqualified,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::InvalidResource { .. }
            ))
        ));
        let explicit_global = composer
            .activate(
                fixture.request(AgentSelection::Explicit(
                    InstructionSelector::global(InstructionKind::Agent, COMPATIBILITY_AGENT_ID)
                        .expect("global selector"),
                )),
            )
            .expect("explicit global ignores project legacy and project damage");
        assert_eq!(
            explicit_global.state.active_agent.scope,
            InstructionScope::Global
        );
        assert!(
            !explicit_global
                .state
                .text
                .contains("SYNTHETIC_PROJECT_LEGACY")
        );
    }

    #[test]
    fn invalid_managed_project_common_does_not_fall_back_to_legacy_overlay() {
        let fixture = Fixture::new();
        let composer = fixture.composer();
        composer
            .activate(fixture.request(AgentSelection::Default))
            .expect("initialize global");
        let repository = fixture
            .service()
            .configure_non_git_project(
                &fixture.project,
                "setup-common-specificity-validation",
                None,
                &seed(
                    InstructionStoreManifest::current(),
                    vec![document(
                        InstructionScope::Project,
                        InstructionKind::System,
                        COMMON_ID,
                        "system/common.md",
                        "SYNTHETIC_PROJECT_COMMON",
                    )],
                ),
                &[],
            )
            .expect("project repository");
        std::fs::create_dir_all(fixture.project.join(".jcode")).expect("project jcode dir");
        std::fs::write(
            fixture.project.join(".jcode/prompt-overlay.md"),
            "SYNTHETIC_LEGACY_OVERLAY",
        )
        .expect("write legacy overlay");
        std::fs::write(
            repository.repository.root.join("system/common.md"),
            "---\nid: common\nkind: system\nunknown: rejected\n---\n\nBROKEN",
        )
        .expect("damage managed common");

        let result = composer.activate(
            fixture.request(AgentSelection::Explicit(
                InstructionSelector::global(InstructionKind::Agent, COMPATIBILITY_AGENT_ID)
                    .expect("global selector"),
            )),
        );
        assert!(matches!(
            result,
            Err(SystemPromptActivationError::Instruction(
                InstructionError::InvalidResource { .. }
            ))
        ));
    }
}

#[cfg(test)]
#[path = "composition_preferred_tests.rs"]
mod preferred_tests;
