//! Workflow-owned framing around managed prose. Rendering never executes a task.
use crate::instruction::{
    InstructionRepositoryService, SystemPromptActivationError, workflow::Workflow,
};
use jcode_task_types::WorkflowPromptRequest;
use std::path::Path;

pub fn render_prompt(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
    request: &WorkflowPromptRequest,
) -> Result<String, SystemPromptActivationError> {
    match request {
        WorkflowPromptRequest::StructuredInitial { content, schema } => {
            let prose =
                Workflow::StructuredOutput { schema }.render_with(repositories, working_dir)?;
            Ok(format!("{content}\n\n{prose}```json\n{schema}\n```"))
        }
        WorkflowPromptRequest::StructuredCorrection {
            schema,
            error_lines,
            previous_response,
        } => {
            let prose = Workflow::StructuredCorrection {
                schema,
                error_lines,
                previous_response,
            }
            .render_with(repositories, working_dir)?;
            Ok(format!(
                "{prose}```json\n{schema}\n```\nValidation errors:\n{error_lines}Previous response:\n```\n{previous_response}\n```"
            ))
        }
    }
}
