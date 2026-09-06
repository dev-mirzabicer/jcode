//! Worker-contract prose adapters. Swarm core retains framing and control policy.
use crate::instruction::workflow::Workflow;
use std::path::Path;

#[derive(Debug)]
pub(super) struct WorkerContractError(crate::instruction::SystemPromptActivationError);
impl From<crate::instruction::SystemPromptActivationError> for WorkerContractError {
    fn from(error: crate::instruction::SystemPromptActivationError) -> Self {
        Self(error)
    }
}
impl std::fmt::Display for WorkerContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for WorkerContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub(super) fn append_swarm_completion_report_instructions(
    message: &str,
    working_dir: Option<&Path>,
) -> Result<String, WorkerContractError> {
    Ok(
        jcode_swarm_core::append_swarm_completion_report_instructions(message, || {
            Workflow::SwarmWorkerReport.render(working_dir)
        })?,
    )
}

pub(super) fn append_deep_node_instructions(
    message: &str,
    node_id: &str,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    Ok(jcode_swarm_core::append_deep_node_instructions(
        message,
        |bounded| {
            let resource = if bounded {
                Workflow::SwarmDeepNodeBounded {
                    node_id,
                    member_cap: jcode_swarm_core::MAX_SWARM_MEMBERS,
                }
            } else {
                Workflow::SwarmDeepNode {
                    node_id,
                    member_cap: jcode_swarm_core::MAX_SWARM_MEMBERS,
                }
            };
            resource.render(working_dir)
        },
    )?)
}

pub(super) fn append_deep_gate_instructions(
    message: &str,
    gate_id: &str,
    audited_ids: &[String],
    low_confidence: &[String],
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    jcode_swarm_core::append_deep_gate_instructions(message, || {
        let mut parts = vec![Workflow::SwarmDeepGate { gate_id }.render(working_dir)?];
        if !audited_ids.is_empty() {
            parts.push(
                Workflow::SwarmDeepGateScope {
                    ids: &audited_ids.join(", "),
                }
                .render(working_dir)?,
            );
        }
        if !low_confidence.is_empty() {
            parts.push(
                Workflow::SwarmDeepGatePriority {
                    ids: &low_confidence.join(", "),
                }
                .render(working_dir)?,
            );
        }
        parts.push(Workflow::SwarmDeepGateFinish.render(working_dir)?);
        Ok(parts.join("\n"))
    })
}

pub(super) fn combine_assignment_text(
    content: &str,
    message: Option<&str>,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    match message {
        Some(extra) => Ok(format!(
            "{content}\n\n{}\n{extra}",
            Workflow::SwarmAssignmentAddendum.render(working_dir)?
        )),
        None => Ok(content.into()),
    }
}

pub(super) fn build_control_assignment_text(
    action: crate::plan::TaskControlAction,
    content: &str,
    message: Option<&str>,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    use crate::plan::TaskControlAction;
    let mut parts = Vec::new();
    match action {
        TaskControlAction::Resume => parts.push(Workflow::SwarmTaskResume.render(working_dir)?),
        TaskControlAction::Retry => parts.push(Workflow::SwarmTaskRetryFull.render(working_dir)?),
        _ => {}
    }
    parts.push(content.to_string());
    if let Some(extra) = message {
        parts.push(format!(
            "{}\n{extra}",
            Workflow::SwarmAssignmentAddendum.render(working_dir)?
        ));
    }
    Ok(parts.join("\n\n"))
}

pub(super) fn composite_synthesis_content(
    item_id: &str,
    content: &str,
    composite: bool,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    if !composite {
        return Ok(content.into());
    }
    Ok(format!(
        "{} Original brief: {content}",
        Workflow::SwarmComposite { item_id }.render(working_dir)?
    ))
}

pub(super) fn assigned_notification(
    content: &str,
    message: Option<&str>,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    let prose = Workflow::SwarmTaskAssigned.render(working_dir)?;
    Ok(match message {
        Some(extra) => format!("{prose} {content} — {extra}"),
        None => format!("{prose} {content}"),
    })
}

pub(super) fn wake_message(
    task_id: &str,
    assignment: &str,
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    Ok(format!(
        "{}\n\n{assignment}",
        Workflow::SwarmTaskWake { task_id }.render(working_dir)?
    ))
}

pub(super) fn planner_message(request: &str, working_dir: Option<&Path>) -> anyhow::Result<String> {
    let directory = working_dir
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let hint = working_dir
        .map(|_| format!("Working directory: {directory}\n"))
        .unwrap_or_default();
    Ok(format!(
        "{hint}{}\n\nRequest:\n{request}",
        Workflow::SwarmPlanner {
            request,
            working_dir: &directory
        }
        .render(working_dir)?
    ))
}

pub(super) fn integration_message(
    request: &str,
    outputs: &[(String, String)],
    working_dir: Option<&Path>,
) -> anyhow::Result<String> {
    let output_text = outputs
        .iter()
        .map(|(desc, output)| format!("\n--- {desc} ---\n{output}\n"))
        .collect::<String>();
    let intro = Workflow::SwarmIntegrator {
        request,
        outputs: &output_text,
    }
    .render(working_dir)?;
    let finish = Workflow::SwarmIntegratorFinish.render(working_dir)?;
    Ok(format!(
        "{intro}\n\nOriginal request:\n{request}\n\nSubagent outputs:\n{output_text}\n{finish}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct RejectCalls(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl crate::provider::Provider for RejectCalls {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume: Option<&str>,
        ) -> anyhow::Result<crate::provider::EventStream> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("unexpected provider call")
        }
        fn name(&self) -> &str {
            "workflow-fixture"
        }
        fn fork(&self) -> std::sync::Arc<dyn crate::provider::Provider> {
            std::sync::Arc::new(self.clone())
        }
    }
    #[test]
    fn invalid_swarm_planner_never_starts_a_model_or_worker() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        std::fs::write(
            home.root()
                .join("instructions/modules/swarm-task-planner.md"),
            "---\nid: swarm-task-planner\nkind: module\ntemplate: handlebars\n---\n{{missing}}",
        )
        .unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent = crate::agent::Agent::new_with_disabled_startup_context(
            std::sync::Arc::new(RejectCalls(calls.clone())),
            crate::tool::Registry::empty(),
            None,
        );
        let before = agent.message_count();
        let agent = std::sync::Arc::new(tokio::sync::Mutex::new(agent));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(
            runtime
                .block_on(crate::server::swarm::run_swarm_message(
                    agent.clone(),
                    "REQUEST"
                ))
                .is_err()
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(runtime.block_on(agent.lock()).message_count(), before);
    }

    #[test]
    fn worker_contract_rendering_is_lazy_and_keeps_structural_identity() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        let write = |id: &str, body: &str| {
            std::fs::write(
                home.root()
                    .join(format!("instructions/notifications/{id}.md")),
                format!("---\nid: {id}\nkind: notification\ntemplate: handlebars\n---\n{body}"),
            )
            .unwrap()
        };
        write("swarm-worker-report-contract", "FIRST");
        let first = append_swarm_completion_report_instructions("TASK", None).unwrap();
        write("swarm-worker-report-contract", "SECOND");
        assert!(
            append_swarm_completion_report_instructions("TASK", None)
                .unwrap()
                .contains("SECOND")
        );
        write("swarm-worker-report-contract", "{{missing}}");
        assert_eq!(
            append_swarm_completion_report_instructions(&first, None).unwrap(),
            first
        );
        assert!(append_swarm_completion_report_instructions("TASK", None).is_err());
        write("swarm-deep-node-contract", "{{missing}}");
        write(
            "swarm-deep-node-bounded",
            "BOUNDED {{node_id}} {{member_cap}}",
        );
        let bounded =
            append_deep_node_instructions("Do not expand this node.", "NODE<&>", None).unwrap();
        assert!(bounded.contains(&format!(
            "BOUNDED NODE<&> {}",
            jcode_swarm_core::MAX_SWARM_MEMBERS
        )));
        assert!(append_deep_node_instructions("TASK", "NODE", None).is_err());
        write("swarm-deep-gate-contract", "GATE {{gate_id}}");
        write("swarm-deep-gate-finish", "END");
        write("swarm-deep-gate-scope", "{{missing}}");
        write("swarm-deep-gate-priority", "{{missing}}");
        assert!(
            append_deep_gate_instructions("TASK", "GATE", &[], &[], None)
                .unwrap()
                .contains("GATE GATE\nEND")
        );
        assert!(append_deep_gate_instructions("TASK", "GATE", &["A".into()], &[], None).is_err());
        assert_eq!(
            append_deep_gate_instructions(&bounded, "GATE", &["A".into()], &[], None).unwrap(),
            bounded
        );
    }
}
