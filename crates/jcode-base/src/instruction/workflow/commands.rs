//! Command composition only. Callers retain scheduling, interrupts and mode state.
use super::*;
use jcode_task_types::{CommandWorkflow, WorkflowLoopMode, WorkflowTodo};

pub fn render_command(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
    command: &CommandWorkflow,
) -> Result<String, SystemPromptActivationError> {
    if command.requires_swarm() && !crate::config::config().features.swarm {
        return Err(SystemPromptActivationError::Compatibility(
            crate::config::SWARM_WORKFLOW_UNAVAILABLE.into(),
        ));
    }
    let runtime = super::super::notification::occurrence_runtime(repositories, working_dir)?;
    Ok(render_in(&runtime, command)?)
}

fn render_in(
    runtime: &InstructionRuntime,
    command: &CommandWorkflow,
) -> Result<String, InstructionError> {
    use CommandWorkflow as C;
    let render = |resource: Workflow<'_>| resource.render_in(runtime);
    match command {
        C::Commit => render(Workflow::CommandCommit),
        C::CommitPush => render(Workflow::CommandCommitPush),
        C::ReleaseFast | C::ReleaseMacos | C::ReleaseRemote => {
            let (prepare, publish) = match command {
                C::ReleaseFast => (
                    Some(Workflow::CommandReleaseFastPrepare),
                    Workflow::CommandReleaseFastPublish,
                ),
                C::ReleaseMacos => (
                    Some(Workflow::CommandReleaseMacosPrepare),
                    Workflow::CommandReleaseMacosPublish,
                ),
                _ => (None, Workflow::CommandReleaseRemotePublish),
            };
            let preparation = match prepare {
                Some(prepare) => format!("{} ", render(prepare)?),
                None => String::new(),
            };
            let publication = render(publish)?;
            render(match command {
                C::ReleaseFast => Workflow::CommandReleaseFast {
                    preparation: &preparation,
                    publication: &publication,
                },
                C::ReleaseMacos => Workflow::CommandReleaseMacos {
                    preparation: &preparation,
                    publication: &publication,
                },
                _ => Workflow::CommandReleaseRemote {
                    preparation: &preparation,
                    publication: &publication,
                },
            })
        }
        C::Triage { focus } => {
            let mut result = render(Workflow::CommandTriage)?;
            if !focus.trim().is_empty() {
                result.push_str(&render(Workflow::CommandTriageFocus {
                    focus: focus.trim(),
                })?);
            }
            Ok(result)
        }
        C::Test { claim } => {
            let target = if claim.trim().is_empty() {
                render(Workflow::CommandTestDefault)?
            } else {
                claim.trim().into()
            };
            render(Workflow::CommandTest { target: &target })
        }
        C::Plan { goal } => {
            let target = match goal.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(goal) => goal.into(),
                None => render(Workflow::CommandPlanDefault)?,
            };
            render(Workflow::CommandPlan {
                goal_line: &format!("Goal: {target}\n\n"),
            })
        }
        C::Improve { plan_only, focus } | C::Refactor { plan_only, focus } => {
            let refactor = matches!(command, C::Refactor { .. });
            let focus_line = match focus {
                Some(focus) => render(if refactor {
                    Workflow::CommandRefactorFocus {
                        focus: focus.trim(),
                    }
                } else {
                    Workflow::CommandImproveFocus {
                        focus: focus.trim(),
                    }
                })?,
                None => String::new(),
            };
            render(match (refactor, plan_only) {
                (false, false) => Workflow::CommandImprove {
                    focus_line: &focus_line,
                },
                (false, true) => Workflow::CommandImprovePlan {
                    focus_line: &focus_line,
                },
                (true, false) => Workflow::CommandRefactor {
                    focus_line: &focus_line,
                },
                (true, true) => Workflow::CommandRefactorPlan {
                    focus_line: &focus_line,
                },
            })
        }
        C::ImproveStop => render(Workflow::CommandImproveStop),
        C::RefactorStop => render(Workflow::CommandRefactorStop),
        C::ImproveResume { mode, todos } => resume(runtime, false, *mode, todos),
        C::RefactorResume { mode, todos } => resume(runtime, true, *mode, todos),
    }
}

fn resume(
    runtime: &InstructionRuntime,
    refactor: bool,
    mode: WorkflowLoopMode,
    todos: &[WorkflowTodo],
) -> Result<String, InstructionError> {
    use WorkflowLoopMode as M;
    let mode = match (refactor, mode) {
        (false, M::ImproveRun) | (true, M::RefactorRun) => 0,
        (false, M::ImprovePlan) | (true, M::RefactorPlan) => 1,
        _ => 2,
    };
    let count = todos.len();
    let todo_rows = todos
        .iter()
        .map(|t| {
            format!(
                "  {} [{}] {}\n",
                if t.status == "in_progress" {
                    "🔄"
                } else {
                    "⬜"
                },
                t.priority,
                t.content
            )
        })
        .collect::<String>();
    let todo_rows = todo_rows.as_str();
    let plural = if count == 1 { "" } else { "s" };
    let resource = match (refactor, mode, todos.is_empty()) {
        (false, 0, true) => Workflow::CommandImproveResumeRunEmpty,
        (false, 1, true) => Workflow::CommandImproveResumePlanEmpty,
        (false, _, true) => Workflow::CommandImproveResumeOtherEmpty,
        (true, 0, true) => Workflow::CommandRefactorResumeRunEmpty,
        (true, 1, true) => Workflow::CommandRefactorResumePlanEmpty,
        (true, _, true) => Workflow::CommandRefactorResumeOtherEmpty,
        (false, 0, false) => Workflow::CommandImproveResumeRun {
            todo_rows,
            count,
            plural,
        },
        (false, 1, false) => Workflow::CommandImproveResumePlan {
            todo_rows,
            count,
            plural,
        },
        (false, _, false) => Workflow::CommandImproveResumeOther {
            todo_rows,
            count,
            plural,
        },
        (true, 0, false) => Workflow::CommandRefactorResumeRun {
            todo_rows,
            count,
            plural,
        },
        (true, 1, false) => Workflow::CommandRefactorResumePlan {
            todo_rows,
            count,
            plural,
        },
        (true, _, false) => Workflow::CommandRefactorResumeOther {
            todo_rows,
            count,
            plural,
        },
    };
    resource.render_in(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, id: &str, body: &str) {
        std::fs::write(
            root.join(format!("modules/{id}.md")),
            format!("---\nid: {id}\nkind: module\ntemplate: handlebars\n---\n{body}"),
        )
        .unwrap();
    }
    #[test]
    fn command_composition_is_current_scoped_and_skips_unused_optional_sources() {
        let temp = tempfile::tempdir().unwrap();
        let repositories = InstructionRepositoryService::from_paths(
            temp.path().join("home"),
            temp.path().join("state"),
        );
        SystemPromptComposer::from_repository_service(repositories.clone())
            .ensure_global_store()
            .unwrap();
        let global = repositories.global_repository().unwrap().root;
        write(&global, "workflow-plan", "PLAN {{goal_line}}");
        write(&global, "workflow-plan-default-goal", "{{missing}}");
        assert_eq!(
            render_command(
                &repositories,
                None,
                &CommandWorkflow::Plan {
                    goal: Some("<&{{data}}>".into())
                }
            )
            .unwrap(),
            "PLAN Goal: <&{{data}}>\n\n"
        );
        assert!(
            render_command(&repositories, None, &CommandWorkflow::Plan { goal: None }).is_err()
        );
        write(&global, "workflow-commit", "FIRST");
        let first = render_command(&repositories, None, &CommandWorkflow::Commit).unwrap();
        write(&global, "workflow-commit", "SECOND");
        assert_eq!(
            render_command(&repositories, None, &CommandWorkflow::Commit).unwrap(),
            "SECOND"
        );
        assert_eq!(first, "FIRST");
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let seed = InstructionStoreSeed {
            manifest: InstructionStoreManifest::current(),
            files: vec![InstructionSeedFile {
                relative_path: "modules/workflow-commit.md".into(),
                content: b"---\nid: workflow-commit\nkind: module\n---\nPROJECT".to_vec(),
            }],
        };
        let configured = repositories
            .configure_non_git_project(&project, "command-project", None, &seed, &[])
            .unwrap();
        assert_eq!(
            render_command(&repositories, Some(&project), &CommandWorkflow::Commit).unwrap(),
            "PROJECT"
        );
        write(&configured.repository.root, "workflow-commit", "");
        assert_eq!(
            render_command(&repositories, Some(&project), &CommandWorkflow::Commit).unwrap(),
            ""
        );
        write(
            &configured.repository.root,
            "workflow-commit",
            "{{missing}}",
        );
        assert!(render_command(&repositories, Some(&project), &CommandWorkflow::Commit).is_err());
    }
}
