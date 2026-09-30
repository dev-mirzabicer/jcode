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
        let mut tools = self
            .registry
            .try_definitions(None)
            .map_err(|reason| ContextServiceError::Runtime(reason.to_string()))?;
        crate::tool::instruction_guidance::preview(&self.session, &mut tools)
            .map_err(|error| ContextServiceError::Runtime(error.to_string()))?;
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
        )
        .map_err(|error| {
            format!(
                "Replayed reasoning could not be checked against the request prefix: {error}. The provider request was not sent."
            )
        })?;
        self.pending_prefix_transitions.clear();
        let Some(outcome) = outcome else {
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
        Ok(Some(notice))
    }
}
