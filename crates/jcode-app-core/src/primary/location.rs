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
            PrimaryLocationCommand::Place { request } => Some(request.session.as_str()),
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
        if let PrimaryLocationCommand::InspectSession { session } = command {
            return inspect_session_location(session).await;
        }
        if let PrimaryLocationCommand::ProposePlacement { session } = command {
            return propose_session_placement(session).await;
        }
        if !launch_enabled()
            && matches!(
                &command,
                PrimaryLocationCommand::Change { .. }
                    | PrimaryLocationCommand::AdoptLegacy { .. }
                    | PrimaryLocationCommand::Place { .. }
            )
        {
            return PrimaryLocationResponse::Rejected { issue: Issue { code: IssueCode::UnsupportedCapability, detail: "Managed location controls are staged until workspace management is available".into() } };
        }
        let result = match command {
            PrimaryLocationCommand::Place { request } => self.place_session(request).await,
            command => self.location_command(command).await,
        };
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

    /// Resolve a reviewed placement, registering a new standalone root if
    /// chosen, then adopt the Session in place through the legacy adoption
    /// control. Retrying the same request converges on its first effects.
    async fn place_session(
        self: &Arc<Self>,
        request: SessionPlacementRequest,
    ) -> Result<LocationChangeRecord> {
        // A retried request returns its recorded adoption instead of being
        // reviewed again against a Session it already placed.
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        let operation: OperationId = request.request.to_string().parse()?;
        if let Ok(record) = workspace.inspect_location_change(operation) {
            ensure!(
                record.input.session == request.session,
                "Placement request identity already belongs to another session"
            );
            return Ok(record);
        }
        let permit = crate::runtime_lifecycle::admission::preparation(
            "primary-placement",
            Some(request.session.clone()),
        )?;
        let resolved = crate::runtime_lifecycle::admission::scope(permit, async {
            let request = request.clone();
            tokio::task::spawn_blocking(move || -> Result<_> {
                let stored = crate::session::Session::load_startup_stub(&request.session)?;
                Ok(WorkspaceService::new(&crate::storage::durable_state_dir())
                    .resolve_session_placement(&request, &stored)?)
            })
            .await?
        })
        .await?;
        let (placement, expected_catalog_revision) = resolved;
        self.location_command(PrimaryLocationCommand::AdoptLegacy {
            request: LegacyLocationAdoptionRequest {
                request: request.request,
                session: request.session,
                expected_working_dir: Some(request.working_dir.clone()),
                expected_catalog_revision,
                placement,
                cwd: request.working_dir,
            },
        })
        .await
    }

    async fn location_command(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
    ) -> Result<LocationChangeRecord> {
        let permit = match &command {
            PrimaryLocationCommand::Change { request } => {
                crate::runtime_lifecycle::admission::preparation(
                    "primary-location",
                    Some(request.session.clone()),
                )?
            }
            PrimaryLocationCommand::AdoptLegacy { request } => {
                crate::runtime_lifecycle::admission::preparation(
                    "primary-adoption",
                    Some(request.session.clone()),
                )?
            }
            _ => None,
        };
        crate::runtime_lifecycle::admission::scope(permit, self.location_command_admitted(command))
            .await
    }

    async fn location_command_admitted(
        self: &Arc<Self>,
        command: PrimaryLocationCommand,
    ) -> Result<LocationChangeRecord> {
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        match command {
            PrimaryLocationCommand::Inspect { operation } => {
                Ok(workspace.inspect_location_change(operation)?)
            }
            PrimaryLocationCommand::InspectSession { .. }
            | PrimaryLocationCommand::ProposePlacement { .. } => {
                anyhow::bail!("Session inspection returns a view, not a location change")
            }
            PrimaryLocationCommand::Place { .. } => {
                anyhow::bail!("Session placement resolves before its adoption control")
            }
            PrimaryLocationCommand::Cancel { operation } => {
                let record = workspace.inspect_location_change(operation)?;
                let _owner = self.claim(&record.input.session)?;
                crate::runtime_lifecycle::admission::control(|| {
                    workspace.cancel_location_change(operation)
                })?
                .map_err(Into::into)
            }
            command @ (PrimaryLocationCommand::Change { .. }
            | PrimaryLocationCommand::AdoptLegacy { .. }) => {
                ensure!(self.accepts_prepared_work(), "Primary runtime is stopping");
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
                    self.retain_delivery(crate::runtime_lifecycle::admission::scope(
                        crate::runtime_lifecycle::admission::current_scope(),
                        async move {
                            let Some(host) = weak.upgrade() else {
                                return;
                            };
                            if host.wait_idle(&session).await.is_err() {
                                return;
                            }
                            if !host.accepts_prepared_work() {
                                return;
                            }
                            let mut guard = agent.lock().await;
                            if let Err(error) = guard.apply_primary_location_changes().await {
                                crate::logging::warn(&format!(
                                    "Primary location remains pending for {session}: {error:#}"
                                ));
                            }
                        },
                    ));
                }
                Ok(workspace.inspect_location_change(record.operation)?)
            }
        }
    }
}

/// Read-only. Loads only the Session's startup metadata, never a live Agent.
async fn propose_session_placement(session: String) -> PrimaryLocationResponse {
    let result = tokio::task::spawn_blocking(move || {
        let stored =
            crate::session::Session::load_startup_stub(&session).map_err(|error| Issue {
                code: IssueCode::InvalidIdentity,
                detail: format!("Read authoritative Session {session}: {error:#}"),
            })?;
        WorkspaceService::new(&crate::storage::durable_state_dir())
            .propose_session_placement(&stored)
    })
    .await;
    match result {
        Ok(Ok(proposal)) => PrimaryLocationResponse::Proposal {
            proposal: Box::new(proposal),
        },
        Ok(Err(issue)) => PrimaryLocationResponse::Rejected { issue },
        Err(error) => PrimaryLocationResponse::Rejected {
            issue: Issue {
                code: IssueCode::RecoveryRequired,
                detail: format!("Placement review worker stopped: {error}"),
            },
        },
    }
}

async fn inspect_session_location(session: String) -> PrimaryLocationResponse {
    let result = tokio::task::spawn_blocking(move || {
        let stored =
            crate::session::Session::load_startup_stub(&session).map_err(|error| Issue {
                code: IssueCode::InvalidIdentity,
                detail: format!("Read authoritative Session {session}: {error:#}"),
            })?;
        Ok::<_, Issue>(
            WorkspaceService::new(&crate::storage::durable_state_dir())
                .session_location_view(&stored),
        )
    })
    .await;
    match result {
        Ok(Ok(view)) => PrimaryLocationResponse::Session {
            view: Box::new(view),
        },
        Ok(Err(issue)) => PrimaryLocationResponse::Rejected { issue },
        Err(error) => PrimaryLocationResponse::Rejected {
            issue: Issue {
                code: IssueCode::RecoveryRequired,
                detail: format!("Session inspection worker stopped: {error}"),
            },
        },
    }
}
