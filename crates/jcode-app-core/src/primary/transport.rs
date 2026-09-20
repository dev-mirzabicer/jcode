//! Trusted creation transport. Preparation is owned by the runtime even when
//! the requesting socket disappears. This is not an agent tool or scheduler.
use super::*;
use crate::workspace::*;
use anyhow::Result;

pub fn launch_enabled() -> bool {
    crate::config::config().features.managed_primary_launch
}
impl PrimaryHost {
    pub async fn request_launch(
        self: &Arc<Self>,
        request: PrimaryLaunchRequest,
        provider: Arc<dyn crate::provider::Provider>,
        pool: Arc<crate::mcp::SharedMcpPool>,
        repositories: crate::instruction::InstructionRepositoryService,
    ) -> PrimaryLaunchResponse {
        let id = request.request;
        if !launch_enabled() {
            return PrimaryLaunchResponse::Rejected {
                request: id,
                issue: Issue {
                    code: IssueCode::UnsupportedCapability,
                    detail:
                        "Managed primary launch is staged until workspace controls are available"
                            .into(),
                },
            };
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let host = Arc::downgrade(self);
        {
            let mut tasks = self.tasks.lock().expect("primary tasks");
            if !self.accepting.load(Ordering::Acquire) {
                return PrimaryLaunchResponse::Rejected {
                    request: id,
                    issue: Issue {
                        code: IssueCode::Busy,
                        detail: "Primary runtime is stopping".into(),
                    },
                };
            }
            tasks.spawn(async move {
                let Some(host) = host.upgrade() else {
                    return;
                };
                let launcher = PrimaryLauncher {
                    workspace: WorkspaceService::new(&crate::storage::durable_state_dir()),
                    repositories,
                    provider,
                    registry: PrimaryRegistryMode::Shared(pool),
                };
                let response = match launcher
                    .launch_hosted(
                        &host,
                        request.request,
                        request.expected_revision,
                        request.input,
                        crate::agent::StartupContextCaller::HarnessApi,
                    )
                    .await
                {
                    Ok(record) => PrimaryLaunchResponse::Launched {
                        record: Box::new(record),
                    },
                    Err(error) => PrimaryLaunchResponse::Rejected {
                        request: id,
                        issue: error
                            .downcast_ref::<Issue>()
                            .cloned()
                            .unwrap_or_else(|| Issue {
                                code: IssueCode::RecoveryRequired,
                                detail: format!("{error:#}"),
                            }),
                    },
                };
                let _ = sender.send(response);
            });
        }
        receiver.await.unwrap_or_else(|_|PrimaryLaunchResponse::Rejected{request:id,issue:Issue{code:IssueCode::RecoveryRequired,detail:"Launch worker ended before delivery; inspect the original request before retrying".into()}})
    }
}

/// Explicit process launch intent installed by the CLI. Callers retain this
/// immutable request in their own retry state, rather than rereading on reconnect.
pub fn configured_launch() -> Result<Option<PrimaryLaunchRequest>> {
    let Some(path) = std::env::var_os("JCODE_PRIMARY_LAUNCH_FILE") else {
        return Ok(None);
    };
    crate::env::remove_var("JCODE_PRIMARY_LAUNCH_FILE");
    let bytes = std::fs::read(&path).context("Read explicit primary launch request")?;
    Ok(Some(
        serde_json::from_slice(&bytes).context("Invalid primary launch request")?,
    ))
}

pub async fn launch_local_request(
    provider: Arc<dyn crate::provider::Provider>,
    request: PrimaryLaunchRequest,
    caller: crate::agent::StartupContextCaller,
) -> Result<Agent> {
    ensure!(
        launch_enabled(),
        "Managed primary launch is staged until workspace controls are available"
    );
    let launcher = PrimaryLauncher {
        workspace: WorkspaceService::new(&crate::storage::durable_state_dir()),
        repositories: crate::instruction::InstructionRepositoryService::new(),
        provider,
        registry: PrimaryRegistryMode::Process,
    };
    let (agent, _) = launcher
        .launch_local(
            request.request,
            request.expected_revision,
            request.input,
            caller,
        )
        .await?;
    Ok(agent)
}
