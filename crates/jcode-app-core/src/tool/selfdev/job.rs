//! Self-development queue participants use the common execution/capture owner.
//! The existing worktree queue, source validation and publication remain unchanged.
use super::*;
use anyhow::Context;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::sync::Arc;

pub(super) enum BuildJob {
    Build {
        repo: PathBuf,
        command: SelfDevBuildCommand,
        reason: String,
    },
    Test {
        repo: PathBuf,
        command: SelfDevBuildCommand,
        reason: String,
    },
    Watch {
        original: String,
    },
}

pub(super) struct BuildOutput {
    pub capture: Arc<dyn OutputCapture>,
    pub stop: jcode_agent_runtime::InterruptSignal,
    pub process_exit: Option<jcode_tool_types::ProcessExit>,
}
impl BuildOutput {
    pub fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self {
            capture: ctx
                .invocation
                .capture
                .clone()
                .context("Build output capture is unavailable")?,
            stop: ctx
                .graceful_shutdown_signal
                .clone()
                .context("Build stop owner is unavailable")?,
            process_exit: None,
        })
    }
    pub fn check_stop(&self) -> Result<()> {
        anyhow::ensure!(
            !self.stop.is_set(),
            "Self-development work stopped; completed effects were not rolled back"
        );
        Ok(())
    }
    pub async fn line(&self, line: impl AsRef<str>) -> Result<()> {
        self.check_stop()?;
        let capture = self.capture.clone();
        let bytes = format!("{}\n", line.as_ref());
        tokio::task::spawn_blocking(move || capture.write(OutputStream::Text, bytes.as_bytes()))
            .await?
    }
}

impl SelfDevTool {
    pub(super) async fn spawn_build_job(
        request_id: String,
        job: BuildJob,
        ctx: &ToolContext,
        notify: bool,
        wake: bool,
    ) -> Result<background::BackgroundTaskInfo> {
        let name = match &job {
            BuildJob::Build { .. } => "selfdev-build",
            BuildJob::Test { .. } => "selfdev-test",
            BuildJob::Watch { .. } => "selfdev-build-watch",
        };
        let mut context = if ctx.invocation.identity.is_some() {
            ctx.for_subcall(request_id.clone())
        } else {
            let mut context = ctx.clone();
            context.tool_call_id = request_id.clone();
            context.invocation = Default::default();
            context
        };
        context.stdin_request_tx = None;
        context.invocation.policy = jcode_tool_core::ExecutionPolicy {
            background: true,
            manual_ready: true,
            cooperative_stop: true,
            notify,
            wake,
            ..Default::default()
        };
        let target = crate::config::config().output.target("selfdev", None);
        context.invocation.output_target = Some(target);
        let mut request =
            BuildRequest::load(&request_id)?.context("Missing self-development request")?;
        anyhow::ensure!(
            request.session_id == ctx.session_id,
            "Self-development request belongs to another session"
        );
        let job_input = match &job {
            BuildJob::Build {
                repo,
                command,
                reason,
            }
            | BuildJob::Test {
                repo,
                command,
                reason,
            } => json!({"repo":repo,"program":command.program,"args":command.args,"reason":reason}),
            BuildJob::Watch { original } => json!({"original":original}),
        };
        let invocation = crate::execution::invocation(
            &context,
            name,
            json!({"request_id":request_id,"command":request.command,"attached_to":request.attached_to_request_id,"job":job_input}),
        );
        let id = invocation.id();
        request.background_task_id = Some(id.clone());
        request.save()?;
        let failure_request = request_id.clone();
        let result = crate::execution::execute(
            invocation,
            context,
            target,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let mut output = BuildOutput::from_context(&ctx)?;
                    // Delivery is registered independently of the caller's receipt wait.
                    // A cancelled watcher cannot signal the build it merely observes.
                    crate::execution::background_handoff(&ctx).await?;
                    let identity = ctx
                        .invocation
                        .identity
                        .as_ref()
                        .context("Missing build invocation identity")?;
                    let info = background::global()
                        .managed_task_info(&identity.id)
                        .await?
                        .context("Missing managed build delivery")?;
                    let mut request = BuildRequest::load(&request_id)?
                        .context("Missing self-development request")?;
                    request.background_task_id = Some(info.task_id);
                    request.output_file = Some(info.output_file.display().to_string());
                    request.status_file = Some(info.status_file.display().to_string());
                    request.save()?;
                    ctx.invocation
                        .ready
                        .as_ref()
                        .context("Missing build readiness")?
                        .mark();
                    let result = match job {
                        BuildJob::Build {
                            repo,
                            command,
                            reason,
                        } => {
                            Self::run_build_request(
                                request_id.clone(),
                                repo,
                                command,
                                reason,
                                &mut output,
                            )
                            .await
                        }
                        BuildJob::Test {
                            repo,
                            command,
                            reason,
                        } => {
                            Self::run_test_request(
                                request_id.clone(),
                                repo,
                                command,
                                reason,
                                &mut output,
                            )
                            .await
                        }
                        BuildJob::Watch { original } => {
                            Self::follow_existing_build(request_id.clone(), original, &mut output)
                                .await
                        }
                    };
                    if let Err(error) = &result
                        && let Some(mut request) = BuildRequest::load(&request_id)?
                    {
                        request.completed_at = Some(Utc::now().to_rfc3339());
                        request.state = if matches!(
                            output.stop.stop_cause(),
                            Some(
                                jcode_tool_types::StopCause::HumanCancellation
                                    | jcode_tool_types::StopCause::ParentForegroundCancellation
                            )
                        ) {
                            BuildRequestState::Cancelled
                        } else {
                            BuildRequestState::Failed
                        };
                        request.error = Some(format!("{error:#}"));
                        request.save()?;
                    }
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let message = format!("{error:#}");
                            let capture = output.capture.clone();
                            let footer = format!("\n[Self-development error]\n{message}\n");
                            tokio::task::spawn_blocking(move || {
                                capture.write(OutputStream::Text, footer.as_bytes())
                            })
                            .await??;
                            TaskResult::failed(
                                output
                                    .process_exit
                                    .as_ref()
                                    .and_then(|exit| exit.shell_code()),
                                message,
                            )
                        }
                    };
                    let superseded =
                        matches!(result.status, Some(BackgroundTaskStatus::Superseded));
                    let mut result_output = ToolOutput::new("").with_error(
                        !superseded
                            && (result.error.is_some()
                                || matches!(result.status, Some(BackgroundTaskStatus::Failed))),
                    );
                    result_output.source =
                        jcode_tool_types::OutputSource::Retained(output.capture.reference()?);
                    result_output.process_exit = output.process_exit;
                    result_output.superseded = superseded;
                    result_output.metadata =
                        Some(json!({"selfdev_request_id":request_id,"error":result.error}));
                    Ok(result_output)
                })
            }),
        )
        .await;
        if let Err(error) = result {
            if let Some(mut request) = BuildRequest::load(&failure_request)?
                && matches!(
                    request.state,
                    BuildRequestState::Queued
                        | BuildRequestState::Building
                        | BuildRequestState::Attached
                )
            {
                let store = crate::execution::ExecutionStore::open(&crate::storage::jcode_dir()?)?;
                request.state = if store
                    .inspect(&id)?
                    .is_some_and(|record| record.state == crate::execution::RunState::Cancelled)
                {
                    BuildRequestState::Cancelled
                } else {
                    BuildRequestState::Failed
                };
                request.completed_at = Some(Utc::now().to_rfc3339());
                request.error = Some(format!("{error:#}"));
                request.save()?;
            }
            return Err(error);
        }
        background::global()
            .managed_task_info(&id)
            .await?
            .context("Self-development acceptance has no managed task receipt")
    }
}
