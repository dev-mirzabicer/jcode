//! Module types: the registry over managed `module-type` resources, frozen
//! into each session when it is activated.
use super::{SessionWorkError, SessionWorkStore};
use crate::instruction::{InstructionRepositoryService, SystemPromptComposer};
use jcode_session_work_types::ModuleTypeDescriptor;
use std::path::Path;

/// The current registry, as a new session would freeze it.
pub fn current_module_types(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
) -> anyhow::Result<Vec<ModuleTypeDescriptor>> {
    Ok(
        SystemPromptComposer::from_repository_service(repositories.clone())
            .module_types(working_dir)?,
    )
}

/// The registry frozen into `session` at activation. Later instruction edits
/// do not change it.
pub fn frozen_module_types(
    store: &SessionWorkStore,
    session: &str,
) -> Result<Vec<ModuleTypeDescriptor>, SessionWorkError> {
    store
        .activation(session)?
        .map(|activation| activation.module_types)
        .ok_or_else(|| SessionWorkError::NotActivated(session.to_string()))
}

/// The label a workflow line uses for a module type: its unqualified name.
pub fn module_type_label(descriptor: &ModuleTypeDescriptor) -> &str {
    descriptor
        .id
        .split_once(':')
        .map_or(descriptor.id.as_str(), |(_, name)| name)
}

/// `module-types.md`: the session's frozen module types, for the agent to
/// read. Generated from the activation record; never edited.
pub fn render_module_types(types: &[ModuleTypeDescriptor]) -> String {
    let mut text = String::from("# Module types\n\n");
    if types.is_empty() {
        text.push_str("None.\n");
        return text;
    }
    for descriptor in types {
        let title = descriptor.title.trim().trim_end_matches('.');
        let description = descriptor.description.trim();
        text.push_str(&format!("- `{}`: {title}", module_type_label(descriptor)));
        if !description.is_empty() {
            text.push_str(&format!(". {description}"));
        }
        text.push('\n');
        if !descriptor.subtypes.is_empty() {
            let subtypes: Vec<String> = descriptor
                .subtypes
                .iter()
                .map(|subtype| format!("`{subtype}`"))
                .collect();
            text.push_str(&format!("  Subtypes: {}\n", subtypes.join(", ")));
        }
        if let Some(skill) = &descriptor.skill {
            text.push_str(&format!("  Skill: `{skill}`\n"));
        }
    }
    text
}
