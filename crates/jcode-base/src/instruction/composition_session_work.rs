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
