//! Isolated caller composition over the same sources, renderer and specificity.
use super::*;
use crate::instruction::{InstructionScopeSelector, ResourceValidationState};
use serde::{Deserialize, Serialize};

const PRESET_PREFIX: &str = "task-preset.";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationInstructionEntry {
    /// Qualified usable selector, not the resource's human display label.
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationInstructionCatalog {
    pub profiles: Vec<DelegationInstructionEntry>,
    pub task_presets: Vec<DelegationInstructionEntry>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskPresetActivation {
    pub resource: InstructionResourceRef,
    pub text: String,
}

fn preset_selector(value: &str) -> Result<InstructionSelector, InstructionError> {
    let selector = InstructionSelector::parse(InstructionKind::Notification, value)?;
    let id = if selector.id.as_str().starts_with(PRESET_PREFIX) {
        selector.id.clone()
    } else {
        InstructionId::parse(format!("{PRESET_PREFIX}{}", selector.id))?
    };
    Ok(InstructionSelector { id, ..selector })
}

fn same_qualified(selector: &InstructionSelector, current: &InstructionResourceRef) -> bool {
    selector.kind == current.kind
        && selector.id == current.id
        && match selector.scope {
            InstructionScopeSelector::Global => current.scope == InstructionScope::Global,
            InstructionScopeSelector::Project => current.scope == InstructionScope::Project,
            InstructionScopeSelector::Unqualified => false,
        }
}

impl SystemPromptComposer {
    pub fn delegation_tool_guidance(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<String, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        let registration = ConsumerRegistration::new(
            "delegation-guidance",
            "subagent",
            InstructionKind::ToolGuidance,
            "tools/subagent.md",
            "parent delegation tool",
            "Framework-stage source, frozen after successful request preflight",
        )?;
        Ok(environment
            .runtime
            .render_registered(&registration, &())?
            .text)
    }
    /// Explicit caller adoption. Never inherit primary defaults or selfdev state.
    /// The task preset is deliberately absent from the returned true system.
    pub fn activate_isolated(
        &self,
        request: SystemPromptActivationRequest<'_>,
    ) -> Result<SystemPromptActivation, SystemPromptActivationError> {
        if matches!(request.selection, AgentSelection::Default) || request.is_selfdev {
            return Err(SystemPromptActivationError::Compatibility(
                "Isolated activation requires an explicit profile and excludes self-development administration".into(),
            ));
        }
        let environment = self.prepare_environment(request.working_dir)?;
        let common = environment
            .runtime
            .render_registered(&child_registration()?, &())?;
        let mut activation =
            compose_activation_for(&environment, request, AgentAvailability::Isolated)?;
        activation.state.text.push_str("\n\n");
        activation.state.text.push_str(&common.text);
        Ok(activation)
    }

    /// Omitted/same selection preserves the stored notice without a body read.
    /// Unqualified selection resolves current specificity before comparison.
    pub fn activate_task_preset(
        &self,
        working_dir: Option<&Path>,
        selection: Option<&str>,
        current: Option<&InstructionResourceRef>,
    ) -> Result<Option<TaskPresetActivation>, SystemPromptActivationError> {
        if selection.is_none() && current.is_some() {
            return Ok(None);
        }
        let selector = preset_selector(selection.unwrap_or("general"))?;
        if current.is_some_and(|current| same_qualified(&selector, current)) {
            return Ok(None);
        }
        let environment = self.prepare_environment(working_dir)?;
        let document = environment.runtime.resolve(&selector)?;
        let resource = InstructionResourceRef {
            scope: document.scope,
            kind: document.kind,
            id: document.id.clone(),
        };
        if current == Some(&resource) {
            return Ok(None);
        }
        let rendered = environment.runtime.render(&selector, &())?;
        Ok(Some(TaskPresetActivation {
            resource,
            text: rendered.text,
        }))
    }

    /// Compact derived catalog. Invalid selected entries retain scoped diagnostics.
    pub fn delegation_catalog(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<DelegationInstructionCatalog, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        let mut catalog = DelegationInstructionCatalog::default();
        for diagnostic in environment.runtime.diagnostics() {
            catalog.diagnostics.push(format!(
                "{}: {}",
                diagnostic.path.display(),
                diagnostic.detail
            ));
        }
        for summary in environment.runtime.resources() {
            let resource = &summary.resource;
            let preset = resource.kind == InstructionKind::Notification
                && resource.id.as_str().starts_with(PRESET_PREFIX);
            if resource.kind != InstructionKind::Agent && !preset {
                continue;
            }
            if summary.state != ResourceValidationState::Valid {
                catalog.diagnostics.push(format!(
                    "{}:{}: {:?}",
                    resource.scope, resource.id, summary.state
                ));
                continue;
            }
            let selector = InstructionSelector::parse(
                resource.kind,
                &format!("{}:{}", resource.scope, resource.id),
            )?;
            let document = environment.runtime.resolve(&selector)?;
            if !preset
                && !document.metadata.agent.as_ref().is_some_and(|metadata| {
                    matches!(
                        metadata.availability,
                        AgentAvailability::Isolated | AgentAvailability::Both
                    )
                })
            {
                continue;
            }
            let checked = if preset {
                environment
                    .runtime
                    .render(&selector, &())
                    .map(|_| ())
                    .map_err(SystemPromptActivationError::from)
            } else {
                render_agent_profile_with_availability(
                    &environment,
                    AgentSelection::Explicit(selector),
                    Some(AgentAvailability::Isolated),
                )
                .and_then(|profile| {
                    push_project_addenda(&environment.runtime, &profile.resource, &mut Vec::new())
                        .map_err(SystemPromptActivationError::from)
                })
            };
            if let Err(error) = checked {
                catalog
                    .diagnostics
                    .push(format!("{}:{}: {error}", resource.scope, resource.id));
                continue;
            }
            let id = if preset {
                resource
                    .id
                    .as_str()
                    .strip_prefix(PRESET_PREFIX)
                    .unwrap_or_default()
            } else {
                resource.id.as_str()
            };
            let entry = DelegationInstructionEntry {
                name: format!("{}:{id}", resource.scope),
                description: document.metadata.description.clone().unwrap_or_default(),
            };
            if preset {
                catalog.task_presets.push(entry);
            } else {
                catalog.profiles.push(entry);
            }
        }
        Ok(catalog)
    }
}

fn child_registration() -> Result<ConsumerRegistration, InstructionError> {
    ConsumerRegistration::new(
        "isolated-system",
        "subagent",
        InstructionKind::System,
        "system/subagent.md",
        "isolated child composer",
        "Common child system addition. Permissions and execution policy remain code-owned.",
    )
}

pub(super) fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
    Ok(vec![
        child_registration()?,
        ConsumerRegistration::new(
            "delegation-guidance",
            "subagent",
            InstructionKind::ToolGuidance,
            "tools/subagent.md",
            "parent delegation tool",
            "Framework-stage guidance. Final role and skill prose remains independently editable.",
        )?,
    ])
}

pub(super) fn seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
    [
        (
            InstructionKind::System,
            "system/subagent.md",
            include_str!("assets/subagent-system.md"),
        ),
        (
            InstructionKind::Notification,
            "notifications/task-preset.general.md",
            include_str!("assets/subagent-general.md"),
        ),
        (
            InstructionKind::ToolGuidance,
            "tools/subagent.md",
            include_str!("assets/subagent-guidance.md"),
        ),
    ]
    .into_iter()
    .map(|(kind, path, text)| {
        crate::instruction::runtime::parse_document(
            InstructionScope::Global,
            kind,
            Path::new(path),
            text,
        )
        .map_err(|detail| InstructionError::InvalidDocument {
            path: path.into(),
            detail,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        project: PathBuf,
        composer: SystemPromptComposer,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let project = temp.path().join("project");
            std::fs::create_dir_all(&project).unwrap();
            let service = InstructionRepositoryService::from_paths(&home, home.join("state"));
            let composer = SystemPromptComposer::from_repository_service(service);
            composer.ensure_global_store().unwrap();
            let fixture = Self {
                _temp: temp,
                home,
                project,
                composer,
            };
            fixture.source("system/subagent.md", "subagent", "system", "", "CHILD-ONLY");
            fixture.source(
                "agents/fixture.md",
                "fixture",
                "agent",
                "name: Fixture\ndescription: synthetic\navailability: both\n",
                "PROFILE",
            );
            fixture.source(
                "notifications/task-preset.general.md",
                "task-preset.general",
                "notification",
                "name: general\ndescription: synthetic\n",
                "INITIAL-PRESET",
            );
            fixture
        }
        fn source(&self, path: &str, id: &str, kind: &str, metadata: &str, body: &str) {
            let path = self.home.join("instructions").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("---\nid: {id}\nkind: {kind}\n{metadata}---\n{body}"),
            )
            .unwrap();
        }
        fn request(&self) -> SystemPromptActivationRequest<'_> {
            SystemPromptActivationRequest {
                working_dir: Some(&self.project),
                selection: AgentSelection::parse(Some("global:fixture")).unwrap(),
                is_selfdev: false,
                capabilities: PromptCapabilities { mermaid: false },
                available_skills: &[],
            }
        }
    }

    #[test]
    fn isolated_composition_preserves_normal_prefix_and_excludes_preset() {
        let f = Fixture::new();
        let primary = f.composer.activate(f.request()).unwrap();
        let child = f.composer.activate_isolated(f.request()).unwrap();
        assert_eq!(
            child.state.text,
            format!("{}\n\nCHILD-ONLY", primary.state.text)
        );
        let preset = f
            .composer
            .activate_task_preset(Some(&f.project), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(preset.text, "INITIAL-PRESET");
        assert!(!child.state.text.contains(&preset.text));
        assert_eq!(child.state.active_agent, primary.state.active_agent);
        assert!(child.state.first_provider_dispatch_at.is_none());
    }

    #[test]
    fn isolated_selection_is_explicit_and_enforces_availability() {
        let f = Fixture::new();
        let mut request = f.request();
        request.selection = AgentSelection::Default;
        assert!(f.composer.activate_isolated(request).is_err());
        let mut request = f.request();
        request.is_selfdev = true;
        assert!(f.composer.activate_isolated(request).is_err());
        f.source(
            "agents/fixture.md",
            "fixture",
            "agent",
            "name: Fixture\ndescription: synthetic\navailability: primary\n",
            "PRIMARY",
        );
        assert!(f.composer.activate_isolated(f.request()).is_err());
        f.source(
            "agents/fixture.md",
            "fixture",
            "agent",
            "name: Fixture\ndescription: synthetic\navailability: isolated\n",
            "ISOLATED",
        );
        assert!(f.composer.activate(f.request()).is_err());
        assert!(f.composer.activate_isolated(f.request()).is_ok());
    }

    #[test]
    fn preset_noop_is_source_free_and_changes_use_current_complete_source() {
        let f = Fixture::new();
        let first = f
            .composer
            .activate_task_preset(Some(&f.project), None, None)
            .unwrap()
            .unwrap();
        f.source(
            "notifications/task-preset.special.md",
            "task-preset.special",
            "notification",
            "template: handlebars\n",
            "{{missing}}",
        );
        assert!(
            f.composer
                .activate_task_preset(Some(&f.project), Some("special"), Some(&first.resource))
                .is_err()
        );
        f.source(
            "notifications/task-preset.special.md",
            "task-preset.special",
            "notification",
            "",
            "NEW-SPECIAL",
        );
        let changed = f
            .composer
            .activate_task_preset(Some(&f.project), Some("special"), Some(&first.resource))
            .unwrap()
            .unwrap();
        assert_eq!(changed.text, "NEW-SPECIAL");
        f.source(
            "notifications/task-preset.special.md",
            "task-preset.special",
            "notification",
            "template: handlebars\n",
            "{{missing}}",
        );
        assert!(
            f.composer
                .activate_task_preset(Some(&f.project), Some("special"), Some(&changed.resource))
                .unwrap()
                .is_none()
        );
        std::fs::rename(f.home.join("instructions"), f.home.join("unavailable")).unwrap();
        for selector in [
            None,
            Some("global:special"),
            Some("global:task-preset.special"),
        ] {
            assert!(
                f.composer
                    .activate_task_preset(Some(&f.project), selector, Some(&changed.resource))
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            f.composer
                .activate_task_preset(Some(&f.project), Some("special"), Some(&changed.resource))
                .is_err()
        );
    }

    #[test]
    fn catalog_uses_selectors_and_scoped_diagnostics_not_prompt_bodies() {
        let f = Fixture::new();
        f.source(
            "notifications/task-preset.broken.md",
            "task-preset.broken",
            "notification",
            "template: handlebars\n",
            "{{missing}}",
        );
        f.source(
            "notifications/ordinary.md",
            "ordinary",
            "notification",
            "",
            "NOT-A-PRESET",
        );
        let catalog = f.composer.delegation_catalog(Some(&f.project)).unwrap();
        assert!(
            catalog
                .profiles
                .iter()
                .any(|entry| entry.name == "global:fixture")
        );
        assert!(
            catalog
                .task_presets
                .iter()
                .any(|entry| entry.name == "global:general")
        );
        assert!(
            !catalog
                .task_presets
                .iter()
                .any(|entry| entry.name.contains("broken") || entry.name.contains("ordinary"))
        );
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|detail| detail.contains("task-preset.broken"))
        );
        for entry in catalog.task_presets {
            assert!(
                f.composer
                    .activate_task_preset(Some(&f.project), Some(&entry.name), None)
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            !serde_json::to_string(&catalog.profiles)
                .unwrap()
                .contains("PROFILE")
        );
    }
}
