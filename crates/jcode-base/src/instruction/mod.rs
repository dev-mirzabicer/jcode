//! Typed, file-backed instruction discovery and rendering.
//!
//! This module owns instruction resource identity, scope specificity, document
//! parsing, dependency validation, and complete rendering. It deliberately does
//! not own Git, session activation, message roles, provider framing, or TUI
//! editing. Those callers receive finished text and retain their own behavior.

mod composition;
mod repository;
mod runtime;
mod template;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::str::FromStr;

pub use composition::*;
pub use repository::*;
pub use runtime::InstructionRuntime;

/// Stable user-readable identity for one managed instruction resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstructionId(String);

impl InstructionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, InstructionError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.chars().next().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            })
            && value.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '-' | '_' | '.')
            });
        if !valid {
            return Err(InstructionError::InvalidId {
                value,
                reason: "IDs must use lowercase ASCII letters, digits, '-', '_', or '.', and must start with a letter or digit".to_string(),
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstructionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for InstructionId {
    type Err = InstructionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionScope {
    Global,
    Project,
}

impl fmt::Display for InstructionScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "global",
            Self::Project => "project",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstructionKind {
    System,
    Agent,
    AgentAddendum,
    Module,
    Notification,
    ToolGuidance,
    Skill,
}

impl InstructionKind {
    pub fn directory(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Agent => "agents",
            Self::AgentAddendum => "addenda",
            Self::Module => "modules",
            Self::Notification => "notifications",
            Self::ToolGuidance => "tools",
            Self::Skill => "skills",
        }
    }
}

impl fmt::Display for InstructionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::System => "system",
            Self::Agent => "agent",
            Self::AgentAddendum => "agent-addendum",
            Self::Module => "module",
            Self::Notification => "notification",
            Self::ToolGuidance => "tool-guidance",
            Self::Skill => "skill",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateMode {
    #[default]
    Plain,
    Handlebars,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentAvailability {
    Primary,
    Isolated,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionScopeSelector {
    Unqualified,
    Global,
    Project,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionSelector {
    pub scope: InstructionScopeSelector,
    pub kind: InstructionKind,
    pub id: InstructionId,
}

impl InstructionSelector {
    pub fn parse(kind: InstructionKind, value: &str) -> Result<Self, InstructionError> {
        let (scope, id) = match value.split_once(':') {
            Some(("global", id)) => (InstructionScopeSelector::Global, id),
            Some(("project", id)) => (InstructionScopeSelector::Project, id),
            Some((prefix, _)) => {
                return Err(InstructionError::InvalidSelector {
                    value: value.to_string(),
                    reason: format!(
                        "unknown scope prefix '{prefix}'; expected global: or project:"
                    ),
                });
            }
            None => (InstructionScopeSelector::Unqualified, value),
        };
        Ok(Self {
            scope,
            kind,
            id: InstructionId::parse(id)?,
        })
    }

    pub fn unqualified(
        kind: InstructionKind,
        id: impl Into<String>,
    ) -> Result<Self, InstructionError> {
        Ok(Self {
            scope: InstructionScopeSelector::Unqualified,
            kind,
            id: InstructionId::parse(id)?,
        })
    }

    pub fn global(kind: InstructionKind, id: impl Into<String>) -> Result<Self, InstructionError> {
        Ok(Self {
            scope: InstructionScopeSelector::Global,
            kind,
            id: InstructionId::parse(id)?,
        })
    }

    pub fn project(kind: InstructionKind, id: impl Into<String>) -> Result<Self, InstructionError> {
        Ok(Self {
            scope: InstructionScopeSelector::Project,
            kind,
            id: InstructionId::parse(id)?,
        })
    }
}

impl fmt::Display for InstructionSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scope {
            InstructionScopeSelector::Unqualified => write!(formatter, "{}:{}", self.kind, self.id),
            InstructionScopeSelector::Global => {
                write!(formatter, "global:{}:{}", self.kind, self.id)
            }
            InstructionScopeSelector::Project => {
                write!(formatter, "project:{}:{}", self.kind, self.id)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstructionResourceRef {
    pub scope: InstructionScope,
    pub kind: InstructionKind,
    pub id: InstructionId,
}

impl fmt::Display for InstructionResourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.scope, self.kind, self.id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentMetadata {
    pub availability: AgentAvailability,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddendumMetadata {
    pub target: InstructionSelector,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstructionMetadata {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub agent: Option<AgentMetadata>,
    pub addendum: Option<AddendumMetadata>,
    pub includes: Vec<InstructionSelector>,
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionDocument {
    pub id: InstructionId,
    pub kind: InstructionKind,
    pub scope: InstructionScope,
    pub template_mode: TemplateMode,
    pub metadata: InstructionMetadata,
    pub body: String,
    pub path: PathBuf,
}

impl InstructionDocument {
    /// Serialize a semantic document back to a human-readable managed Markdown
    /// resource. The frontmatter layout is deterministic; body bytes are kept.
    pub fn to_markdown(&self) -> Result<String, InstructionError> {
        runtime::serialize_document(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAgentsInstruction {
    pub scope: InstructionScope,
    pub path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct InstructionSources {
    pub global_root: PathBuf,
    pub project_root: Option<PathBuf>,
    pub global_agents_md: Option<PathBuf>,
    pub project_agents_md: Option<PathBuf>,
}

impl InstructionSources {
    pub fn new(global_root: impl Into<PathBuf>) -> Self {
        Self {
            global_root: global_root.into(),
            project_root: None,
            global_agents_md: None,
            project_agents_md: None,
        }
    }

    pub fn with_project_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.project_root = Some(root.into());
        self
    }

    pub fn with_global_agents_md(mut self, path: impl Into<PathBuf>) -> Self {
        self.global_agents_md = Some(path.into());
        self
    }

    pub fn with_project_agents_md(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_agents_md = Some(path.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionDiagnostic {
    pub scope: InstructionScope,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceValidationState {
    Valid,
    Invalid(String),
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionResourceSummary {
    pub resource: InstructionResourceRef,
    pub paths: Vec<PathBuf>,
    pub state: ResourceValidationState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstructionGraph {
    /// Dependencies whose rendered text contributes to the consumer.
    pub render_dependencies:
        std::collections::BTreeMap<InstructionResourceRef, Vec<InstructionResourceRef>>,
    /// References that must resolve and validate but whose text is not rendered.
    pub validation_dependencies:
        std::collections::BTreeMap<InstructionResourceRef, Vec<InstructionResourceRef>>,
    pub reverse_consumers:
        std::collections::BTreeMap<InstructionResourceRef, Vec<InstructionResourceRef>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedInstruction {
    pub root: InstructionResourceRef,
    pub text: String,
    pub graph: InstructionGraph,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerScopePolicy {
    GlobalOnly,
    ProjectThenGlobal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerRegistration {
    pub key: String,
    pub id: InstructionId,
    pub kind: InstructionKind,
    pub default_relative_path: PathBuf,
    pub scope_policy: ConsumerScopePolicy,
    pub required: bool,
    pub empty_is_meaningful: bool,
    pub delivery_owner: String,
    pub inventory_note: String,
}

impl ConsumerRegistration {
    pub fn new(
        key: impl Into<String>,
        id: impl Into<String>,
        kind: InstructionKind,
        default_relative_path: impl Into<PathBuf>,
        delivery_owner: impl Into<String>,
        inventory_note: impl Into<String>,
    ) -> Result<Self, InstructionError> {
        Ok(Self {
            key: key.into(),
            id: InstructionId::parse(id)?,
            kind,
            default_relative_path: default_relative_path.into(),
            scope_policy: ConsumerScopePolicy::ProjectThenGlobal,
            required: true,
            empty_is_meaningful: true,
            delivery_owner: delivery_owner.into(),
            inventory_note: inventory_note.into(),
        })
    }
}

/// A code-owned consumer whose value type is fixed at registration.
#[derive(Clone, Debug)]
pub struct InstructionConsumer<T> {
    registration: ConsumerRegistration,
    marker: PhantomData<fn(&T)>,
}

impl<T> InstructionConsumer<T> {
    pub fn new(registration: ConsumerRegistration) -> Self {
        Self {
            registration,
            marker: PhantomData,
        }
    }

    pub fn registration(&self) -> &ConsumerRegistration {
        &self.registration
    }
}

impl<T: Serialize> InstructionConsumer<T> {
    pub fn render(
        &self,
        runtime: &InstructionRuntime,
        values: &T,
    ) -> Result<RenderedInstruction, InstructionError> {
        runtime.render_registered(&self.registration, values)
    }
}

#[derive(Debug)]
pub enum InstructionError {
    InvalidId {
        value: String,
        reason: String,
    },
    InvalidSelector {
        value: String,
        reason: String,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        detail: String,
    },
    InvalidDocument {
        path: PathBuf,
        detail: String,
    },
    InvalidResource {
        resource: InstructionResourceRef,
        path: PathBuf,
        detail: String,
    },
    AmbiguousResource {
        selector: InstructionSelector,
        scope: InstructionScope,
        paths: Vec<PathBuf>,
    },
    ResourceNotFound {
        selector: InstructionSelector,
    },
    RegisteredResourceMissing {
        key: String,
        selector: InstructionSelector,
        expected_path: PathBuf,
    },
    EmptyResourceNotAllowed {
        key: String,
        resource: InstructionResourceRef,
    },
    RestrictedTemplate {
        resource: InstructionResourceRef,
        detail: String,
    },
    Render {
        resource: InstructionResourceRef,
        detail: String,
    },
    DependencyCycle {
        chain: Vec<InstructionResourceRef>,
    },
    Serialization {
        detail: String,
    },
    AgentUnavailable {
        resource: InstructionResourceRef,
        requested: AgentAvailability,
        actual: AgentAvailability,
    },
}

impl fmt::Display for InstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId { value, reason } => {
                write!(formatter, "invalid instruction ID '{value}': {reason}")
            }
            Self::InvalidSelector { value, reason } => {
                write!(
                    formatter,
                    "invalid instruction selector '{value}': {reason}"
                )
            }
            Self::Io {
                operation,
                path,
                detail,
            } => {
                write!(
                    formatter,
                    "could not {operation} instruction path {}: {detail}",
                    path.display()
                )
            }
            Self::InvalidDocument { path, detail } => {
                write!(
                    formatter,
                    "invalid instruction document {}: {detail}",
                    path.display()
                )
            }
            Self::InvalidResource {
                resource,
                path,
                detail,
            } => write!(
                formatter,
                "instruction resource {resource} is invalid at {}: {detail}",
                path.display()
            ),
            Self::AmbiguousResource {
                selector,
                scope,
                paths,
            } => write!(
                formatter,
                "instruction selector {selector} is ambiguous in {scope} scope across: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ResourceNotFound { selector } => {
                write!(
                    formatter,
                    "instruction resource not found for selector {selector}"
                )
            }
            Self::RegisteredResourceMissing {
                key,
                selector,
                expected_path,
            } => write!(
                formatter,
                "registered instruction consumer '{key}' requires {selector}, but its singleton file is missing (expected {})",
                expected_path.display()
            ),
            Self::EmptyResourceNotAllowed { key, resource } => write!(
                formatter,
                "registered instruction consumer '{key}' does not allow empty prose for {resource}"
            ),
            Self::RestrictedTemplate { resource, detail } => {
                write!(
                    formatter,
                    "restricted template validation failed for {resource}: {detail}"
                )
            }
            Self::Render { resource, detail } => {
                write!(
                    formatter,
                    "could not render instruction {resource}: {detail}"
                )
            }
            Self::DependencyCycle { chain } => write!(
                formatter,
                "instruction dependency cycle: {}",
                chain
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::Serialization { detail } => write!(
                formatter,
                "could not serialize instruction values: {detail}"
            ),
            Self::AgentUnavailable {
                resource,
                requested,
                actual,
            } => write!(
                formatter,
                "agent {resource} is unavailable for {requested:?} activation (declared {actual:?})"
            ),
        }
    }
}

impl std::error::Error for InstructionError {}

#[cfg(test)]
mod tests;
