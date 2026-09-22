use super::*;
use crate::workspace::*;
use anyhow::Result;

impl PrimaryHost {
    /// Idle reconciliation never restores an evicted Agent, admits a turn, or
    /// creates a provider call. Busy owners flush at their existing safe points.
    pub(crate) async fn reconcile_idle_scope_notices(&self) {
        if !self.accepts_input() {
            return;
        }
        let agents = self.read().await.values().cloned().collect::<Vec<_>>();
        for agent in agents {
            let Ok(mut guard) = agent.try_lock() else {
                continue;
            };
            if guard.startup_context_session().location.is_none() {
                continue;
            }
            if let Err(error) = guard.apply_primary_location_changes().await {
                crate::logging::debug(&format!(
                    "Scope explanation remains pending for {}: {error:#}",
                    guard.session_id()
                ));
            }
        }
    }

    pub(crate) async fn request_location_restoring(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
        provider: &Arc<dyn crate::provider::Provider>,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        repositories: &crate::instruction::InstructionRepositoryService,
    ) -> PrimaryLocationResponse {
        let target = match &command {
            PrimaryLocationCommand::Change { request } => Some(request.session.as_str()),
            PrimaryLocationCommand::AdoptLegacy { request } => Some(request.session.as_str()),
            _ => None,
        };
        if launch_enabled()
            && let Some(session) = target
            && let Err(error) = self
                .restore_for_location_repair(session, provider, pool, repositories)
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
        if !launch_enabled()
            && matches!(
                &command,
                PrimaryLocationCommand::Change { .. } | PrimaryLocationCommand::AdoptLegacy { .. }
            )
        {
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
            command @ (PrimaryLocationCommand::Change { .. }
            | PrimaryLocationCommand::AdoptLegacy { .. }) => {
                ensure!(self.accepts_input(), "Primary runtime is stopping");
                let session = match &command {
                    PrimaryLocationCommand::Change { request } => request.session.clone(),
                    PrimaryLocationCommand::AdoptLegacy { request } => request.session.clone(),
                    _ => unreachable!(),
                };
                let agent = self
                    .read()
                    .await
                    .get(&session)
                    .cloned()
                    .context("Attach the primary before requesting its location change")?;
                let _owner = self.claim(&session)?;
                let record = match command {
                    PrimaryLocationCommand::Change { request } => {
                        workspace.request_location_change(request)?
                    }
                    PrimaryLocationCommand::AdoptLegacy { request } => {
                        workspace.request_legacy_adoption(request)?
                    }
                    _ => unreachable!(),
                };
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
