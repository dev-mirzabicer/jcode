use super::*;
use crate::workspace::{IssueCode, WorkspaceService};
use anyhow::{Context, ensure};

impl Agent {
    pub(crate) async fn run_primary_input_capture(
        &mut self,
        input: jcode_session_types::PrimaryInputEnvelope,
    ) -> Result<String> {
        ensure!(
            input.session == self.session_id(),
            "Primary input target changed"
        );
        let store = crate::primary_input::PrimaryInputStore::current();
        store.accept(input.clone())?;
        let receipt = store.inspect(self.session_id(), input.id)?;
        if receipt.state == jcode_session_types::PrimaryInputState::Committed {
            return Ok(String::new());
        }
        ensure!(
            receipt.state == jcode_session_types::PrimaryInputState::Accepted,
            "Primary input needs explicit recovery: {:?}",
            receipt.issue
        );
        self.apply_primary_location_changes().await?;
        self.session.require_published_primary()?;
        match self.begin_arrival_turn(&input, None) {
            Ok(true) => {}
            // Held by the arrival rule: it stays durable for the next wake.
            Ok(false) => return Ok(String::new()),
            Err(error) => {
                self.pending_arrival_lead = None;
                store.fail(self.session_id(), input.id, format!("{error:#}"))?;
                return Err(error);
            }
        }
        self.pending_primary_input = Some(input.clone());
        let result = self
            .run_capture_context(
                &input.content,
                input.display_role,
                input.unattended_context,
                input.origin,
            )
            .await;
        self.pending_primary_input = None;
        self.pending_arrival_lead = None;
        if let Err(error) = &result {
            store.fail(self.session_id(), input.id, format!("{error:#}"))?;
        }
        result
    }
    pub(crate) async fn run_primary_input(
        &mut self,
        input: jcode_session_types::PrimaryInputEnvelope,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        let observe = input.observe_startup_context.unwrap_or(
            input.display_role.is_none()
                && input.delivery != jcode_session_types::PrimaryInputDelivery::ContextOnly,
        );
        self.run_primary_input_correlated(input, Some(0), observe, event_tx)
            .await
    }

    pub(crate) async fn run_primary_input_correlated(
        &mut self,
        input: jcode_session_types::PrimaryInputEnvelope,
        request_id: Option<u64>,
        observe_startup: bool,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        ensure!(
            input.session == self.session_id(),
            "Primary input target changed"
        );
        self.apply_primary_location_changes().await?;
        if input.delivery == jcode_session_types::PrimaryInputDelivery::ContextOnly {
            self.session.require_primary_publication()?;
        } else {
            self.session.require_published_primary()?;
        }
        let store = crate::primary_input::PrimaryInputStore::current();
        if store.inspect(self.session_id(), input.id)?.state
            != jcode_session_types::PrimaryInputState::Accepted
        {
            return Ok(());
        }
        if observe_startup && let Err(error) = self.observe_startup_context_before_user_turn() {
            crate::logging::warn(&format!(
                "Primary input continues without a new Startup Context observation: {error}"
            ));
        }
        if let Some(skill) = &input.activate_skill {
            ensure!(
                input.delivery == jcode_session_types::PrimaryInputDelivery::NextTurn,
                "Skill activation requires a new primary turn"
            );
            let activation = self.activate_skill(skill)?;
            let _ = event_tx.send(ServerEvent::SkillActivated {
                id: request_id.unwrap_or(0),
                skill_id: activation.skill_id,
                description: activation.description,
                source: activation.source.kind.to_string(),
            });
        }
        if input.delivery != jcode_session_types::PrimaryInputDelivery::ContextOnly {
            match self.begin_arrival_turn(&input, Some(&event_tx)) {
                Ok(true) => {}
                // Held by the arrival rule: it stays durable for the next wake.
                Ok(false) => return Ok(()),
                Err(error) => {
                    self.pending_arrival_lead = None;
                    store.fail(self.session_id(), input.id, format!("{error:#}"))?;
                    return Err(error);
                }
            }
        }
        self.pending_primary_input = Some(input.clone());
        let result = if input.delivery == jcode_session_types::PrimaryInputDelivery::ContextOnly {
            self.append_user_context_blocks_with_origin(
                Self::user_context_blocks(&input.content, input.images.clone()),
                input.display_role,
                input.origin.clone(),
                input.system_reminder.as_deref(),
            )
        } else {
            self.run_once_streaming_mpsc_with_request_context(
                &input.content,
                input.images.clone(),
                input.system_reminder.clone(),
                event_tx,
                super::turn_execution::StreamingTurnContext {
                    request_id,
                    display_role: input.display_role,
                    unattended_context: input.unattended_context.clone(),
                    origin: input.origin.clone(),
                },
            )
            .await
        };
        self.pending_primary_input = None;
        self.pending_arrival_lead = None;
        if let Err(error) = &result {
            store.fail(self.session_id(), input.id, format!("{error:#}"))?;
        }
        result
    }

    pub(super) fn inject_primary_inputs(
        &mut self,
    ) -> Result<Vec<super::interrupts::InjectedSoftInterrupt>> {
        if let Some(runtime) = crate::runtime_lifecycle::admission::current_runtime()? {
            return runtime
                .input_boundary(|| self.inject_primary_inputs_admitted())
                .unwrap_or_else(|| Ok(Vec::new()));
        }
        self.inject_primary_inputs_admitted()
    }

    fn inject_primary_inputs_admitted(
        &mut self,
    ) -> Result<Vec<super::interrupts::InjectedSoftInterrupt>> {
        if self.session.isolated_child.is_some() {
            return Ok(Vec::new());
        }
        if !self.finish_emergency_retry_audit(
            jcode_session_types::StoredContextEmergencyRetryOutcome::Succeeded,
        ) {
            return Ok(Vec::new());
        }
        let store = crate::primary_input::PrimaryInputStore::current();
        let deliveries = self.route_pending_inputs(
            store.pending(self.session_id())?,
            crate::session_work::arrivals::Activity::Working,
        )?;
        // One boundary injects the leading inputs that share an unattended
        // scope and a system reminder: safe-boundary input, and held input
        // the arrival rule released whatever its delivery. The shared reminder
        // is one occurrence: it is delivered once, committed with the group's
        // last input, after all of the group's content.
        let mut group: Vec<crate::session_work::delivery::Delivery> = Vec::new();
        for delivery in deliveries {
            let input = &delivery.input;
            if !delivery.released
                && input.delivery != jcode_session_types::PrimaryInputDelivery::SafeBoundary
            {
                continue;
            }
            if group.first().is_some_and(|first| {
                first.input.unattended_context != input.unattended_context
                    || first.input.system_reminder != input.system_reminder
            }) {
                break;
            }
            group.push(delivery);
        }
        let mut injected = Vec::new();
        let group_len = group.len();
        for (index, delivery) in group.into_iter().enumerate() {
            let input = delivery.input;
            let reminder = (index + 1 == group_len)
                .then_some(input.system_reminder.as_deref())
                .flatten();
            self.pending_arrival_lead = self.render_arrival_lead(delivery.lead.as_ref())?;
            self.pending_primary_input = Some(input.clone());
            self.append_user_context_blocks_with_origin(
                Self::user_context_blocks(&input.content, input.images.clone()),
                input.display_role,
                input.origin.clone(),
                reminder,
            )?;
            let source = match input.display_role {
                None => SoftInterruptSource::User,
                Some(StoredDisplayRole::System) => SoftInterruptSource::System,
                Some(StoredDisplayRole::BackgroundTask) => SoftInterruptSource::BackgroundTask,
            };
            let committed_len = self.session.messages.len();
            if let Some(context) = self.active_turn_context.as_mut() {
                // Earlier tool effects and committed safe-boundary input stay
                // authoritative if the following request exceeds its budget.
                context.transcript_len_before_pending = committed_len;
                context.unattended_context = input.unattended_context;
                context.emergency_attempted = false;
                context.emergency_transaction_id = None;
                context.provider_output_started = false;
                context.pending_input = None;
            }
            injected.push(super::interrupts::InjectedSoftInterrupt {
                receipt: Some(store.inspect(self.session_id(), input.id)?),
                content: input.content,
                source,
            });
        }
        Ok(injected)
    }

    /// The pending inputs this session may receive now, in delivery order.
    /// Sessions with session work route them through the arrival rule;
    /// others receive them all, as before.
    pub(crate) fn route_pending_inputs(
        &self,
        pending: Vec<jcode_session_types::PrimaryInputEnvelope>,
        activity: crate::session_work::arrivals::Activity,
    ) -> Result<Vec<crate::session_work::delivery::Delivery>> {
        if self.session.session_work.is_none() {
            return Ok(pending
                .into_iter()
                .map(|input| crate::session_work::delivery::Delivery {
                    input,
                    lead: None,
                    released: false,
                    position: None,
                })
                .collect());
        }
        crate::session_work::delivery::route_pending(self.session_id(), pending, activity)
            .context("Session work is unavailable for this session")
    }

    fn render_arrival_lead(
        &self,
        lead: Option<&crate::session_work::arrivals::Lead>,
    ) -> Result<Option<String>> {
        lead.map(|lead| {
            crate::session_work::delivery::render_lead(
                lead,
                self.session
                    .working_dir
                    .as_deref()
                    .map(std::path::Path::new),
            )
        })
        .transpose()
    }

    /// Before a turn starts with `waking`: decide its arrival if no one has,
    /// and keep it if the rule holds it (`false`). Otherwise deliver the held
    /// input the rule now releases, oldest first, each with its note, before
    /// the waking input, which carries its own note. Under a kept stop
    /// nothing is released.
    fn begin_arrival_turn(
        &mut self,
        waking: &jcode_session_types::PrimaryInputEnvelope,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<bool> {
        if self.session.session_work.is_none() {
            return Ok(true);
        }
        let recorded = match crate::session_work::delivery::recorded(self.session_id(), waking)
            .context("Session work is unavailable for this session")?
        {
            Some(record) => record,
            None => crate::session_work::delivery::arrive(
                self.session_id(),
                waking,
                crate::session_work::arrivals::Activity::Idle,
            )
            .context("Session work is unavailable for this session")?,
        };
        if !crate::session_work::arrivals::deliverable(&recorded) {
            return Ok(false);
        }
        // The turn belongs to the waking input, as its start event says; the
        // released held input committed ahead of it must not take over the
        // turn's identity.
        if let Some(presentation) = &self.primary_presentation {
            presentation.bind_input(waking.id);
        }
        let store = crate::primary_input::PrimaryInputStore::current();
        let pending: Vec<_> = store
            .pending(self.session_id())?
            .into_iter()
            .filter(|input| input.id != waking.id)
            .collect();
        // Held input that arrived before the waking input comes first; any
        // that arrived after it follows at the turn's next safe point.
        let waking_position = crate::session_work::SessionWorkStore::new()
            .arrivals(self.session_id())
            .context("Session work is unavailable for this session")?
            .iter()
            .position(|record| record.input == waking.id.to_string());
        let released: Vec<_> = self
            .route_pending_inputs(pending, crate::session_work::arrivals::Activity::Working)?
            .into_iter()
            .filter(|delivery| {
                delivery.released
                    && waking_position
                        .is_none_or(|waking| delivery.position.is_some_and(|at| at < waking))
            })
            .collect();
        for delivery in released {
            let input = delivery.input;
            self.pending_arrival_lead = self.render_arrival_lead(delivery.lead.as_ref())?;
            self.pending_primary_input = Some(input.clone());
            self.append_user_context_blocks_with_origin(
                Self::user_context_blocks(&input.content, input.images.clone()),
                input.display_role,
                input.origin.clone(),
                input.system_reminder.as_deref(),
            )?;
            if let Some(event_tx) = event_tx {
                let _ = event_tx.send(ServerEvent::PrimaryInputFinished {
                    receipt: store.inspect(self.session_id(), input.id)?,
                    delivery: Some(input.delivery),
                });
            }
        }
        // The input that starts the turn carries its own note, such as the
        // end of the stop it was awaited by.
        let lead = crate::session_work::arrivals::lead(&recorded);
        self.pending_arrival_lead = self.render_arrival_lead(lead.as_ref())?;
        Ok(true)
    }

    /// Only called with the Agent's exclusive owner guard, at idle or after a
    /// complete tool-result batch. Location controls never enter the interrupt
    /// queue and cannot themselves request a provider continuation.
    pub(crate) async fn apply_primary_location_changes(&mut self) -> Result<()> {
        if self.session.isolated_child.is_some()
            || (self.session.location.is_none() && !crate::primary::launch_enabled())
        {
            return Ok(());
        }
        let _runtime = crate::runtime_lifecycle::admission::preparation(
            "location-checkpoint",
            Some(self.session_id().into()),
        )?;
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        // An unplaced Session can only gain a location through a catalog
        // operation. Without a catalog none can be pending, and the scope
        // gate below explains what the Session needs.
        if self.session.location.is_none() && !workspace.catalog_present()? {
            return Ok(());
        }
        let _control = workspace.primary_control_lease(self.session_id())?;
        for record in workspace.pending_location_changes(self.session_id())? {
            let current = self.session.location.as_ref();
            if current.and_then(|location| location.last_operation) == Some(record.operation) {
                workspace.reconcile_location_change(record.operation)?;
                continue;
            }
            if record.state == crate::workspace::LocationChangeState::RecoveryRequired {
                // Catalog restore invalidates old operation authorization. A
                // historical or uncheckpointed intent cannot move this Session.
                continue;
            }
            let preparation = match current {
                Some(current) => workspace.prepare_location_change(&record, current),
                None => workspace.prepare_legacy_adoption(&record, &self.session),
            };
            let prepared = match preparation {
                Ok(prepared) => prepared,
                Err(problem)
                    if matches!(
                        problem.code,
                        IssueCode::Conflict
                            | IssueCode::ReplacedRoot
                            | IssueCode::OfflineVolume
                            | IssueCode::InvalidIdentity
                            | IssueCode::InvalidInput
                            | IssueCode::PermissionRequired
                    ) =>
                {
                    workspace.fail_location_change(record.operation, problem)?;
                    continue;
                }
                Err(problem) => return Err(problem.into()),
            };
            let old_placement = current
                .map(|location| format!("{:?}", location.placement))
                .unwrap_or_else(|| "Unplaced".into());
            let new_placement = format!("{:?}", prepared.location.placement);
            let old_cwd = self
                .session
                .working_dir
                .clone()
                .unwrap_or_else(|| "Unknown (not recorded in legacy state)".into());
            let new_cwd = prepared.location.cwd.observed_path().to_string_lossy();
            let mut observed_session = self.session.clone();
            observed_session.location = Some(prepared.location.clone());
            observed_session.scope_notice = None;
            let observed = workspace
                .observe_session_scope(&observed_session)?
                .context("Prepared location has no scope observation")?;
            let scope = WorkspaceService::scope_summary(&observed);
            let notice = crate::instruction::notification::Notification::SessionLocationChanged {
                old_placement: &old_placement,
                new_placement: &new_placement,
                old_cwd: &old_cwd,
                new_cwd: &new_cwd,
                scope_summary: &scope,
            }
            .render_with(
                &self.instruction_repositories,
                Some(prepared.location.cwd.observed_path()),
            )?;
            ensure!(
                !notice.trim().is_empty(),
                "Location notice is empty; repair its managed source before applying"
            );
            let mut candidate = if current.is_some() {
                self.session
                    .stage_location_change(prepared.location.clone(), notice)?
            } else {
                self.session
                    .stage_legacy_location_adoption(prepared.location.clone(), notice)?
            };
            candidate.scope_notice = Some(jcode_session_types::StoredScopeNotice {
                installation: observed.installation,
                catalog_revision: observed.catalog_revision,
                location_revision: observed.location_revision,
                fingerprint: observed.fingerprint.clone(),
                message: format!("location_{}", record.operation),
            });
            let projected = candidate.projected_messages_for_provider()?;
            let split = self.build_system_prompt_split()?;
            let tools = match self.recorded_request_tools() {
                Some(tools) => tools,
                None => self.tool_definitions_for_session(&candidate).await?,
            };
            let breakdown =
                crate::context::request_token_breakdown(&projected, 0, 0, &split, &tools);
            let preflight = crate::context::evaluate_context_preflight(
                candidate.context_view.revision,
                self.provider.context_request_budget(),
                breakdown,
            );
            ensure!(
                preflight.pressure != crate::protocol::ContextPressureLevel::Blocked,
                "Location notice exceeds the provider request budget; change remains pending"
            );
            self.session.commit_location_candidate(candidate)?;
            self.rewind_undo_snapshot = None;
            // Do not allow pending-input rollback to erase this committed
            // control. Retained input remains available through normal repair.
            if let Some(context) = self.active_turn_context.as_mut() {
                context.transcript_len_before_pending = self.session.messages.len();
                context.pending_input = None;
            }
            workspace.reconcile_location_change(record.operation)?;
        }
        self.apply_scope_notice(&workspace).await?;
        Ok(())
    }
    async fn apply_scope_notice(&mut self, workspace: &WorkspaceService) -> Result<()> {
        let Some((observed, mut candidate)) =
            workspace.prepare_scope_notice(&self.session, &self.instruction_repositories)?
        else {
            return Ok(());
        };
        let projected = candidate.projected_messages_for_provider()?;
        let split = self.build_system_prompt_split()?;
        let tools = match self.recorded_request_tools() {
            Some(tools) => tools,
            None => self.tool_definitions_for_session(&candidate).await?,
        };
        let breakdown = crate::context::request_token_breakdown(&projected, 0, 0, &split, &tools);
        let preflight = crate::context::evaluate_context_preflight(
            candidate.context_view.revision,
            self.provider.context_request_budget(),
            breakdown,
        );
        ensure!(
            preflight.pressure != crate::protocol::ContextPressureLevel::Blocked,
            "Scope notice exceeds request budget; explanation remains pending, current write policy is already enforced"
        );
        // A change during rendering is not an excuse to deliver stale authority
        // as current. Retry from durable facts at the next safe boundary.
        ensure!(
            workspace.observe_session_scope(&self.session)?.as_ref() == Some(&observed),
            "Scope changed during notice preparation; retry its pending explanation"
        );
        self.session.commit_scope_notice(candidate)?;
        self.rewind_undo_snapshot = None;
        if let Some(context) = self.active_turn_context.as_mut() {
            context.transcript_len_before_pending = self.session.messages.len();
            context.pending_input = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoInference;
    #[async_trait::async_trait]
    impl Provider for NoInference {
        async fn complete(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
            _: &str,
            _: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            anyhow::bail!("Input commitment must not call a provider")
        }
        fn name(&self) -> &str {
            "input-commit-fixture"
        }
        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(NoInference)
        }
    }

    #[test]
    fn deferred_input_does_not_block_safe_boundary_or_urgent_delivery() -> Result<()> {
        let _environment = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let provider: Arc<dyn Provider> = Arc::new(NoInference);
            let registry = Registry::new(provider.clone()).await;
            let mut agent = Agent::new(provider, registry);
            let store = crate::primary_input::PrimaryInputStore::current();
            let deferred = jcode_session_types::PrimaryInputEnvelope::new(
                agent.session_id().into(),
                "future turn".into(),
                jcode_session_types::PrimaryInputDelivery::NextTurn,
            );
            store.accept(deferred.clone())?;
            let current = jcode_session_types::PrimaryInputEnvelope::new(
                agent.session_id().into(),
                "current boundary".into(),
                jcode_session_types::PrimaryInputDelivery::SafeBoundary,
            );
            store.accept(current)?;
            let mut urgent = jcode_session_types::PrimaryInputEnvelope::new(
                agent.session_id().into(),
                "urgent current boundary".into(),
                jcode_session_types::PrimaryInputDelivery::SafeBoundary,
            );
            urgent.urgent = true;
            store.accept(urgent)?;
            assert!(agent.has_urgent_interrupt());
            let injected = agent.inject_primary_inputs()?;
            assert_eq!(
                injected
                    .iter()
                    .map(|item| item.content.as_str())
                    .collect::<Vec<_>>(),
                vec!["current boundary", "urgent current boundary"]
            );
            assert_eq!(store.pending(agent.session_id())?, vec![deferred]);
            assert_eq!(agent.session.primary_inputs.len(), 2);
            assert!(!agent.has_urgent_interrupt());
            Ok(())
        })
    }
}
