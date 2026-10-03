//! Framework-stage workspace tool guidance (SP-58-C01). The approved
//! provisional operating text is a managed resource; C05 owns final prose.
use super::*;

const WORKSPACE_GUIDANCE_PATH: &str = "tools/workspace.md";

fn guidance_registration() -> Result<ConsumerRegistration, InstructionError> {
    ConsumerRegistration::new(
        "workspace-guidance",
        "workspace",
        InstructionKind::ToolGuidance,
        WORKSPACE_GUIDANCE_PATH,
        "placed primary workspace tool",
        "Framework-stage guidance, frozen after successful request preflight. Exposure and authority remain code-owned.",
    )
}

impl SystemPromptComposer {
    /// Rendered tool guidance for a placed primary Session's workspace tool.
    pub fn workspace_tool_guidance(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<String, SystemPromptActivationError> {
        let environment = self.prepare_environment(working_dir)?;
        Ok(environment
            .runtime
            .render_registered(&guidance_registration()?, &())?
            .text)
    }
}

pub(super) fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
    Ok(vec![guidance_registration()?])
}

pub(super) fn seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
    let text = include_str!("assets/workspace-guidance.md");
    Ok(vec![
        crate::instruction::runtime::parse_document(
            InstructionScope::Global,
            InstructionKind::ToolGuidance,
            Path::new(WORKSPACE_GUIDANCE_PATH),
            text,
        )
        .map_err(|detail| InstructionError::InvalidDocument {
            path: WORKSPACE_GUIDANCE_PATH.into(),
            detail,
        })?,
    ])
}
