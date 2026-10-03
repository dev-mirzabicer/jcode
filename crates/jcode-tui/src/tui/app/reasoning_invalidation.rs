//! Replayed reasoning bound to its request prefix, for sessions the TUI runs
//! locally (INT-01 WP-04). The rule and its only writer live in
//! `jcode_app_core::context`; this module supplies the local session's
//! request prefix and applies the request-time check before a local send.

use super::App;
use crate::context::ContextRequestPrefix;
use crate::message::ToolDefinition;
use crate::protocol::ContextServiceError;

impl App {
    /// Record an intentional change to what the next local request carries
    /// before its messages: documented in the cache-invalidation journal and
    /// kept as a cause for replayed reasoning it may invalidate.
    pub(super) fn record_local_prefix_transition(
        &mut self,
        source: &'static str,
        detail: impl Into<String>,
    ) {
        crate::cache_invalidation::record(source, detail);
        if !self
            .pending_prefix_transitions
            .iter()
            .any(|label| label == source)
        {
            self.pending_prefix_transitions.push(source.to_string());
        }
    }

    /// The tool set the next local request carries, with the agent's rule
    /// (`crate::tool::tool_set`): the session's frozen set compared with the
    /// registry, each change announced once and persisted before the request.
    pub(super) async fn local_tool_definitions(&mut self) -> anyhow::Result<Vec<ToolDefinition>> {
        let mut live = self.registry.definitions(None).await;
        crate::tool::retain_workspace_tool_for_session(&mut live, &self.session);
        // A tool the runtime would reject by name is not advertised
        // (INT-01/WP-06 R26); the person is told when that set changes.
        let withheld = crate::tool::withhold_rejected_tool_names(&mut live, self.provider.as_ref());
        let names: Vec<String> = withheld.iter().map(|(name, _)| name.clone()).collect();
        if names != self.withheld_tool_names {
            self.withheld_tool_names = names;
            if let Some(notice) = crate::tool::withheld_tools_notice(&withheld) {
                crate::logging::warn(&notice);
                self.push_display_message(crate::tui::DisplayMessage::system(notice));
            }
        }
        crate::tool::instruction_guidance::preview(&self.session, &mut live)?;
        let in_view = if !self.provider.renders_tool_changes() {
            None
        } else if crate::tool::tool_set_notice_count(&self.session.messages) == 0 {
            Some(Vec::new())
        } else {
            let projected = self
                .session
                .projected_messages_for_provider()
                .map_err(|error| {
                    anyhow::anyhow!("The provider context could not be projected: {error:?}")
                })?;
            Some(crate::tool::tool_changes_in_view(
                &self.session.messages,
                &projected,
            ))
        };
        let plan = crate::tool::plan_tool_set(
            self.session.tool_set.as_ref(),
            live,
            in_view.as_deref(),
            self.registry.mcp_connecting(),
        );
        crate::tool::set_session_unavailable_tools(&self.session.id, plan.unavailable);
        if plan.announced.is_empty() && self.session.tool_set.as_ref() == Some(&plan.record) {
            return Ok(plan.tools);
        }
        if !plan.announced.is_empty() {
            let notice = crate::tool::tool_set_notice(
                crate::tool::tool_set_notice_count(&self.session.messages) + 1,
                &plan.announced,
            );
            let changes = plan
                .announced
                .iter()
                .map(|change| change.change.clone())
                .collect();
            if let Some(id) = self.session.append_tool_set_delivery(&notice, changes)
                && let Some(message) = self.session.messages.iter().rev().find(|m| m.id == id)
            {
                let message = message.to_message();
                self.add_provider_message(message);
            }
        }
        self.session.set_tool_set(plan.record);
        self.session.save()?;
        if plan.array_changed {
            self.provider
                .invalidate_context_continuation(crate::tool::TOOL_SET_TRANSITION);
            self.provider_session_id = None;
            self.record_local_prefix_transition(
                crate::tool::TOOL_SET_TRANSITION,
                "the provider's tool array changed with the announced tool-set changes",
            );
        }
        Ok(plan.tools)
    }

    /// The prefix the local session's next request carries, built as
    /// `prepare_local_provider_invocation` builds it; `None` when replayed
    /// reasoning need not be checked (the route does not bind reasoning, or
    /// the transcript holds none and no managed set is in force).
    pub(super) fn local_request_prefix_if_needed(
        &mut self,
    ) -> Result<Option<ContextRequestPrefix>, ContextServiceError> {
        if !crate::context::reasoning_reconciliation_needed(
            self.provider.as_ref(),
            &self.session.messages,
            &self.session.context_view,
        ) {
            return Ok(None);
        }
        let system = self
            .build_system_prompt_split()
            .map_err(|error| ContextServiceError::Runtime(error.to_string()))?
            .static_part;
        let tools = match self.session.tool_set.as_ref() {
            Some(record) => {
                crate::tool::recorded_tools(record, self.provider.renders_tool_changes())
            }
            None => {
                let mut tools = self
                    .registry
                    .try_definitions(None)
                    .map_err(|reason| ContextServiceError::Runtime(reason.to_string()))?;
                crate::tool::instruction_guidance::preview(&self.session, &mut tools)
                    .map_err(|error| ContextServiceError::Runtime(error.to_string()))?;
                tools
            }
        };
        Ok(Some(ContextRequestPrefix {
            system,
            tools,
            recorded_transitions: self.pending_prefix_transitions.clone(),
        }))
    }

    /// Before a local provider request, suppress replayed reasoning that no
    /// longer matches this request's prefix and restore reasoning that
    /// matches again. The change is persisted before the request is sent.
    /// Returns a notice when anything changed.
    pub(super) fn reconcile_local_replayed_reasoning(
        &mut self,
        system: &str,
        tools: &[ToolDefinition],
    ) -> Result<Option<String>, String> {
        if let Some((from, to)) = self
            .session
            .note_provider_client_identity(self.provider.client_identity_text())
        {
            self.record_local_prefix_transition(
                crate::context::CLIENT_IDENTITY_TRANSITION,
                format!("the runtime's client identity text changed from `{from}` to `{to}`"),
            );
        }
        let prefix = ContextRequestPrefix {
            system: system.to_string(),
            tools: tools.to_vec(),
            recorded_transitions: self.pending_prefix_transitions.clone(),
        };
        let outcome = crate::context::reconcile_before_request(
            self.provider.as_ref(),
            &self.session.messages,
            &self.session.context_view,
            &prefix,
            &self.provider_reported_reasoning,
        )
        .map_err(|error| {
            format!(
                "Replayed reasoning could not be checked against the request prefix: {error}. The provider request was not sent."
            )
        })?;
        self.pending_prefix_transitions.clear();
        let Some(outcome) = outcome else {
            self.provider_reported_reasoning.clear();
            return Ok(None);
        };
        let previous = std::mem::replace(&mut self.session.context_view, outcome.state);
        if let Err(error) = self.session.save() {
            self.session.context_view = previous;
            return Err(format!(
                "Invalid replayed reasoning could not be persisted as suppressed; the provider request was not sent: {error}"
            ));
        }
        let notice = outcome
            .summary
            .as_ref()
            .and_then(crate::context::describe_reasoning_invalidation)
            .unwrap_or_else(|| "Replayed reasoning suppression updated".to_string());
        crate::cache_invalidation::record("reasoning invalidation", notice.clone());
        self.provider
            .invalidate_context_continuation("replayed reasoning invalidation changed");
        self.provider_reported_reasoning.clear();
        Ok(Some(notice))
    }

    /// Whether `error` is the runtime handing a local request back to be
    /// planned again, as the agent loops handle it
    /// (`Agent::accept_provider_replan`): a model fallback is adopted and
    /// recorded, rejected reasoning is held for suppression, and the turn
    /// loop reconciles and sends a new request.
    pub(super) fn accept_local_provider_replan(&mut self, error: &anyhow::Error) -> bool {
        use jcode_provider_core::ProviderRequestReplan;
        const MAX_CONSECUTIVE_REPLANS: u32 = 4;

        let Some(replan) = ProviderRequestReplan::of(error) else {
            return false;
        };
        if self.provider_replans >= MAX_CONSECUTIVE_REPLANS {
            return false;
        }
        self.provider_replans += 1;
        match replan {
            ProviderRequestReplan::ModelFallback { from, to, cause } => {
                self.session.model = Some(self.provider.model());
                self.provider_session_id = None;
                self.record_local_prefix_transition(
                    "provider model fallback",
                    format!("model '{from}' is {cause}; requests continue on '{to}'"),
                );
                self.push_display_message(super::DisplayMessage::system(format!(
                    "Model '{from}' is {cause}; continuing on '{to}'."
                )));
            }
            ProviderRequestReplan::ReplayedReasoningInvalid { .. } => {
                if !self
                    .pending_prefix_transitions
                    .iter()
                    .any(|label| label == "request plan change")
                {
                    self.pending_prefix_transitions
                        .push("request plan change".to_string());
                }
            }
            ProviderRequestReplan::ReasoningRejected { block_ids, reason } => {
                self.provider_reported_reasoning
                    .push(crate::context::ProviderReportedReasoning {
                        block_ids: block_ids.clone(),
                        reason: reason.clone(),
                    });
            }
        }
        true
    }
}
