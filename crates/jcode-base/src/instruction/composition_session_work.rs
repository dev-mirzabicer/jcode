//! The module-type registry over managed instruction resources. C05 writes
//! the content; sessions freeze the registry when they are activated.
use super::*;
use crate::instruction::{InstructionScopeSelector, ResourceValidationState};
use jcode_session_work_types::ModuleTypeDescriptor;

impl SystemPromptComposer {
    /// Every valid module type visible from `working_dir`, with project
    /// resources taking precedence over global ones of the same ID. Invalid
    /// resources are left out; their diagnostics stay in the instruction
    /// manager. Module types are descriptive, so an unknown or missing type
    /// never blocks a workflow.
    pub fn module_types(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<Vec<ModuleTypeDescriptor>, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        let mut ids = std::collections::BTreeSet::new();
        for summary in environment.runtime.resources() {
            if summary.resource.kind == InstructionKind::ModuleType
                && summary.state == ResourceValidationState::Valid
            {
                ids.insert(summary.resource.id.clone());
            }
        }
        let mut types = Vec::new();
        for id in ids {
            let selector = InstructionSelector {
                scope: InstructionScopeSelector::Unqualified,
                kind: InstructionKind::ModuleType,
                id,
            };
            let Ok(document) = environment.runtime.resolve(&selector) else {
                continue;
            };
            let metadata = document.metadata.module_type.clone().unwrap_or_default();
            types.push(ModuleTypeDescriptor {
                id: format!("{}:{}", document.scope, document.id),
                title: document.metadata.display_name.clone().unwrap_or_default(),
                description: document.metadata.description.clone().unwrap_or_default(),
                subtypes: metadata.subtypes,
                skill: metadata.skill,
            });
        }
        Ok(types)
    }

    /// The managed global path of a skill, whether or not it exists yet.
    pub fn global_skill_path(&self, skill: &str) -> Result<PathBuf, SystemPromptActivationError> {
        let global = self.repositories.global_repository()?;
        Ok(global
            .root
            .join(InstructionKind::Skill.directory())
            .join(skill)
            .join("SKILL.md"))
    }
}

const DECLARE_STOP_GUIDANCE_PATH: &str = "tools/declare-stop.md";
const DECLARE_STOP_CHILD_GUIDANCE_PATH: &str = "tools/declare-stop-child.md";

fn declare_stop_registration(
    role: jcode_session_work_types::SessionWorkRole,
) -> Result<ConsumerRegistration, InstructionError> {
    match role {
        jcode_session_work_types::SessionWorkRole::Primary => ConsumerRegistration::new(
            "declare-stop-guidance",
            "declare-stop",
            InstructionKind::ToolGuidance,
            DECLARE_STOP_GUIDANCE_PATH,
            "primary declare_stop tool",
            "Framework-stage guidance, frozen after successful request preflight. Validation and enforcement remain code-owned.",
        ),
        jcode_session_work_types::SessionWorkRole::Child => ConsumerRegistration::new(
            "declare-stop-child-guidance",
            "declare-stop-child",
            InstructionKind::ToolGuidance,
            DECLARE_STOP_CHILD_GUIDANCE_PATH,
            "isolated child declare_stop tool",
            "Framework-stage guidance, frozen after successful request preflight. Children declare completion only.",
        ),
    }
}

impl SystemPromptComposer {
    /// Rendered `declare_stop` description for a session's role.
    pub fn declare_stop_tool_guidance(
        &self,
        role: jcode_session_work_types::SessionWorkRole,
        working_dir: Option<&Path>,
    ) -> Result<String, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        Ok(environment
            .runtime
            .render_registered(&declare_stop_registration(role)?, &())?
            .text)
    }
}

pub(super) fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
    Ok(vec![
        declare_stop_registration(jcode_session_work_types::SessionWorkRole::Primary)?,
        declare_stop_registration(jcode_session_work_types::SessionWorkRole::Child)?,
    ])
}

/// The managed `session-work` skill. Only a person invokes it, so it stays
/// out of every session's available-skills catalog; activated sessions reach
/// it through their Session Context line.
const SESSION_WORK_SKILL_PATH: &str = "skills/session-work/SKILL.md";

pub(super) fn seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
    [
        (
            InstructionKind::ToolGuidance,
            DECLARE_STOP_GUIDANCE_PATH,
            include_str!("assets/declare-stop-guidance.md"),
        ),
        (
            InstructionKind::ToolGuidance,
            DECLARE_STOP_CHILD_GUIDANCE_PATH,
            include_str!("assets/declare-stop-child-guidance.md"),
        ),
        (
            InstructionKind::Skill,
            SESSION_WORK_SKILL_PATH,
            include_str!("assets/session-work-skill.md"),
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
    use crate::instruction::runtime::{parse_document, serialize_document};
    use crate::instruction::{InstructionKind, InstructionScope};
    use std::path::Path;

    fn parse(
        kind: InstructionKind,
        path: &str,
        source: &str,
    ) -> Result<super::InstructionDocument, String> {
        parse_document(InstructionScope::Global, kind, Path::new(path), source)
    }

    #[test]
    fn person_invoked_skills_round_trip_and_only_skills_take_the_flag() {
        let source = "---\nid: synthetic\nkind: skill\nname: synthetic\ndescription: Synthetic\ndisable-model-invocation: true\n---\nBody.\n";
        let document = parse(InstructionKind::Skill, "skills/synthetic/SKILL.md", source).unwrap();
        assert!(document.metadata.disable_model_invocation);
        let again = parse(
            InstructionKind::Skill,
            "skills/synthetic/SKILL.md",
            &serialize_document(&document).unwrap(),
        )
        .unwrap();
        assert_eq!(again.metadata, document.metadata);
        assert!(
            parse(
                InstructionKind::Module,
                "modules/m.md",
                "---\nid: m\ndisable-model-invocation: true\n---\nx\n",
            )
            .unwrap_err()
            .contains("disable-model-invocation")
        );
    }

    #[test]
    fn the_seed_ships_session_work_guidance_and_a_person_invoked_skill() {
        let documents = super::seed_documents().unwrap();
        let skill = documents
            .iter()
            .find(|document| document.path == Path::new("skills/session-work/SKILL.md"))
            .unwrap();
        assert_eq!(skill.kind, InstructionKind::Skill);
        assert!(skill.metadata.disable_model_invocation);
        for path in ["tools/declare-stop.md", "tools/declare-stop-child.md"] {
            let document = documents
                .iter()
                .find(|document| document.path == Path::new(path))
                .unwrap();
            assert_eq!(document.kind, InstructionKind::ToolGuidance);
        }
    }

    #[test]
    fn module_type_metadata_parses_and_round_trips() {
        let source = "---\nkind: module-type\nid: research\nname: Research\ndescription: Synthetic\nsubtypes:\n  - web\nskill: global:research\n---\nBody.\n";
        let document = parse(
            InstructionKind::ModuleType,
            "module-types/research.md",
            source,
        )
        .unwrap();
        let metadata = document.metadata.module_type.clone().unwrap();
        assert_eq!(metadata.subtypes, ["web"]);
        assert_eq!(metadata.skill.as_deref(), Some("global:research"));
        let serialized = serialize_document(&document).unwrap();
        let again = parse(
            InstructionKind::ModuleType,
            "module-types/research.md",
            &serialized,
        )
        .unwrap();
        assert_eq!(again.metadata, document.metadata);
        assert_eq!(again.body, document.body);
    }

    #[test]
    fn module_type_fields_are_rejected_elsewhere_and_validated_here() {
        for (kind, path, source) in [
            (
                InstructionKind::Module,
                "modules/m.md",
                "---\nid: m\nsubtypes:\n  - web\n---\nx\n",
            ),
            (
                InstructionKind::Module,
                "modules/m.md",
                "---\nid: m\nskill: global:x\n---\nx\n",
            ),
            (
                InstructionKind::ModuleType,
                "module-types/t.md",
                "---\nid: t\nname: T\ndescription: D\nsubtypes:\n  - Not Valid\n---\nx\n",
            ),
            (
                InstructionKind::ModuleType,
                "module-types/t.md",
                "---\nid: t\ndescription: D\n---\nx\n",
            ),
            (
                InstructionKind::ModuleType,
                "module-types/t.md",
                "---\nid: t\nname: T\n---\nx\n",
            ),
        ] {
            assert!(parse(kind, path, source).is_err(), "{source}");
        }
    }

    #[test]
    fn workflow_templates_belong_to_task_presets_and_must_be_valid() {
        let valid = "---\nid: task-preset.fixture\nworkflow: |\n  - [>] gather: Gather {research}\n  - [ ] report: Report\n---\nPreset text.\n";
        let document = parse(
            InstructionKind::Notification,
            "notifications/task-preset.fixture.md",
            valid,
        )
        .unwrap();
        assert_eq!(
            document.metadata.workflow_template.as_deref(),
            Some("- [>] gather: Gather {research}\n- [ ] report: Report\n")
        );
        let serialized = serialize_document(&document).unwrap();
        let again = parse(
            InstructionKind::Notification,
            "notifications/task-preset.fixture.md",
            &serialized,
        )
        .unwrap();
        assert_eq!(again.metadata, document.metadata);

        let invalid =
            "---\nid: task-preset.fixture\nworkflow: |\n  - [>] a: A\n  - [>] b: B\n---\nx\n";
        let error = parse(
            InstructionKind::Notification,
            "notifications/task-preset.fixture.md",
            invalid,
        )
        .unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        let not_a_preset =
            "---\nid: background-task-completed\nworkflow: |\n  - [ ] a: A\n---\nx\n";
        assert!(
            parse(
                InstructionKind::Notification,
                "notifications/background-task-completed.md",
                not_a_preset
            )
            .is_err()
        );
        let wrong_kind = "---\nid: m\nworkflow: |\n  - [ ] a: A\n---\nx\n";
        assert!(parse(InstructionKind::Module, "modules/m.md", wrong_kind).is_err());
    }
}
