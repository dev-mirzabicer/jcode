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

    /// The tool set the next local request carries: locked at the first
    /// request and changed only at a recorded tool-set transition, as the
    /// agent's requests are (`crate::tool::tool_set`).
    pub(super) async fn local_tool_definitions(&mut self) -> anyhow::Result<Vec<ToolDefinition>> {
        let mut tool_set = std::mem::take(&mut self.tool_set);
        let resolved = tool_set
            .resolve(
                &self.registry,
                &crate::tool::ToolSetFilters::none(),
                || async {
                    let mut tools = self.registry.definitions(None).await;
                    crate::tool::instruction_guidance::preview(&self.session, &mut tools)?;
                    Ok(tools)
                },
            )
            .await;
        self.tool_set = tool_set;
        let resolved = resolved?;
        for transition in resolved.transitions {
            self.record_local_prefix_transition(
                transition.label(),
                "the local session's tool set changed",
            );
        }
        Ok(resolved.tools)
    }

    /// The `mcp` tool may have changed registry membership: the next local
    /// request rebuilds the tool set.
    pub(super) fn release_local_tool_set_after_mcp_management(&mut self) {
        if self.tool_set.release_after_mcp_management() {
            self.record_local_prefix_transition(
                crate::tool::ToolSetTransition::McpManagement.label(),
                "the mcp tool changed MCP servers; the next request rebuilds the tool set",
            );
        }
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
        let tools = match self.tool_set.locked() {
            Some(locked) => {
                let mut tools = locked.to_vec();
                tools.retain(|tool| crate::tool::tool_is_globally_available(&tool.name));
                tools
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
