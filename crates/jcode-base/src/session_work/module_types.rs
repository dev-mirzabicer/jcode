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
