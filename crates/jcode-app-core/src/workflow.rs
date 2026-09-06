//! Workflow-owned framing around managed prose. Rendering never executes a task.
use crate::instruction::{
    InstructionRepositoryService, SystemPromptActivationError, workflow::Workflow,
};
pub use jcode_task_types::{ReviewWorkflowKind, WorkflowPromptRequest};
use std::path::Path;

pub fn render_prompt(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
    request: &WorkflowPromptRequest,
) -> Result<String, SystemPromptActivationError> {
    match request {
        WorkflowPromptRequest::ReviewStartup {
            mode,
            parent_session_id,
        } => {
            use jcode_task_types::ReviewWorkflowKind;
            let resource = match mode {
                ReviewWorkflowKind::Review => Workflow::ReviewStartup { parent_session_id },
                ReviewWorkflowKind::Autoreview => Workflow::AutoreviewStartup { parent_session_id },
                ReviewWorkflowKind::Judge => Workflow::JudgeStartup { parent_session_id },
                ReviewWorkflowKind::Autojudge => Workflow::AutojudgeStartup { parent_session_id },
            };
            resource.render_with(repositories, working_dir)
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, id: &str, body: &str, handlebars: bool) {
        std::fs::write(
            root.join(format!("modules/{id}.md")),
            format!(
                "---\nid: {id}\nkind: module\ntemplate: {}\n---\n{body}",
                if handlebars { "handlebars" } else { "plain" }
            ),
        )
        .unwrap();
    }
    #[test]
    fn reviewer_rendering_uses_typed_values_and_project_modules_without_prose_inference() {
        let temp = tempfile::tempdir().unwrap();
        let repositories = InstructionRepositoryService::from_paths(
            temp.path().join("home"),
            temp.path().join("state"),
        );
        crate::instruction::SystemPromptComposer::from_repository_service(repositories.clone())
            .ensure_global_store()
            .unwrap();
        let global = repositories.global_repository().unwrap().root;
        write(
            &global,
            "review-startup",
            "R {{parent_session_id}} [{{> review-read-only-guardrails}}]",
            true,
        );
        write(&global, "review-read-only-guardrails", "GLOBAL", false);
        let request = WorkflowPromptRequest::ReviewStartup {
            mode: ReviewWorkflowKind::Review,
            parent_session_id: "P<&{{literal}}>".into(),
        };
        assert_eq!(
            render_prompt(&repositories, None, &request).unwrap(),
            "R P<&{{literal}}> [GLOBAL]"
        );
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let seed = crate::instruction::InstructionStoreSeed {
            manifest: crate::instruction::InstructionStoreManifest::current(),
            files: vec![crate::instruction::InstructionSeedFile {
                relative_path: "modules/review-read-only-guardrails.md".into(),
                content: b"---\nid: review-read-only-guardrails\nkind: module\n---\nPROJECT"
                    .to_vec(),
            }],
        };
        let configured = repositories
            .configure_non_git_project(&project, "review-fixture", None, &seed, &[])
            .unwrap();
        assert_eq!(
            render_prompt(&repositories, Some(&project), &request).unwrap(),
            "R P<&{{literal}}> [PROJECT]"
        );
        write(
            &configured.repository.root,
            "review-read-only-guardrails",
            "",
            false,
        );
        assert_eq!(
            render_prompt(&repositories, Some(&project), &request).unwrap(),
            "R P<&{{literal}}> []"
        );
        write(
            &configured.repository.root,
            "review-read-only-guardrails",
            "{{missing}}",
            true,
        );
        assert!(render_prompt(&repositories, Some(&project), &request).is_err());
        assert_eq!(
            render_prompt(&repositories, None, &request).unwrap(),
            "R P<&{{literal}}> [GLOBAL]"
        );
    }
}
