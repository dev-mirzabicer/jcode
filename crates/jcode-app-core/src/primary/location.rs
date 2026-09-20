use super::*;
use crate::workspace::*;
use anyhow::Result;

impl PrimaryHost {
    pub(crate) async fn request_location_restoring(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
        provider: &Arc<dyn crate::provider::Provider>,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        repositories: &crate::instruction::InstructionRepositoryService,
    ) -> PrimaryLocationResponse {
        if launch_enabled()
            && let PrimaryLocationCommand::Change { request } = &command
            && let Err(error) = self
                .restore_for_location_repair(&request.session, provider, pool, repositories)
                .await
        {
            return PrimaryLocationResponse::Rejected {
                issue: Issue {
                    code: IssueCode::RecoveryRequired,
                    detail: format!("{error:#}"),
                },
            };
        }
        self.request_location(command).await
    }
    pub async fn request_location(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
    ) -> PrimaryLocationResponse {
        if !launch_enabled() && matches!(&command, PrimaryLocationCommand::Change { .. }) {
            return PrimaryLocationResponse::Rejected { issue: Issue { code: IssueCode::UnsupportedCapability, detail: "Managed location controls are staged until workspace management is available".into() } };
        }
        let result = self.location_command(command).await;
        match result {
            Ok(record) => PrimaryLocationResponse::State {
                record: Box::new(record),
            },
            Err(error) => PrimaryLocationResponse::Rejected {
                issue: error
                    .downcast_ref::<Issue>()
                    .cloned()
                    .unwrap_or_else(|| Issue {
                        code: IssueCode::RecoveryRequired,
                        detail: format!("{error:#}"),
                    }),
            },
        }
    }

    async fn location_command(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
    ) -> Result<LocationChangeRecord> {
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        match command {
            PrimaryLocationCommand::Inspect { operation } => {
                Ok(workspace.inspect_location_change(operation)?)
            }
            PrimaryLocationCommand::Cancel { operation } => {
                let record = workspace.inspect_location_change(operation)?;
                let _owner = self.claim(&record.input.session)?;
                Ok(workspace.cancel_location_change(operation)?)
            }
            PrimaryLocationCommand::Change { request } => {
                ensure!(self.accepts_input(), "Primary runtime is stopping");
                let session = request.session.clone();
                let agent = self
                    .read()
                    .await
                    .get(&session)
                    .cloned()
                    .context("Attach the primary before requesting its location change")?;
                let _owner = self.claim(&session)?;
                let record = workspace.request_location_change(request)?;
                if record.state != LocationChangeState::Pending {
                    return Ok(record);
                }
                if let Ok(mut guard) = agent.try_lock() {
                    guard.apply_primary_location_changes().await?;
                } else {
                    // A failed provider turn may never reach B/D. The runtime
                    // retains this idle reconciliation independently of clients.
                    let weak = Arc::downgrade(self);
                    self.retain_delivery(async move {
                        let Some(host) = weak.upgrade() else {
                            return;
                        };
                        if host.wait_idle(&session).await.is_err() {
                            return;
                        }
                        let mut guard = agent.lock().await;
                        if let Err(error) = guard.apply_primary_location_changes().await {
                            crate::logging::warn(&format!(
                                "Primary location remains pending for {session}: {error:#}"
                            ));
                        }
                    });
                }
                Ok(workspace.inspect_location_change(record.operation)?)
            }
        }
    }
}
