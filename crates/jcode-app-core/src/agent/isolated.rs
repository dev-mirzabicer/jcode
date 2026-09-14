//! Explicit child caller policy over the ordinary streaming Agent loop.
use super::*;
use anyhow::{Context, ensure};
use jcode_tool_types::delegation::Permission;

#[cfg(test)]
#[path = "isolated_tests.rs"]
mod tests;

impl Agent {
    /// The caller supplies an independently prepared/restored roster provider.
    /// Unlike primary attachment, this cannot best-effort switch the model or
    /// silently keep a different route after failed restoration.
    pub(crate) fn from_isolated_session(
        provider: Arc<dyn Provider>,
        registry: Registry,
        session: Session,
        repositories: crate::instruction::InstructionRepositoryService,
    ) -> Result<Self> {
        session.validate_active_agent_profile()?;
        let child = session
            .isolated_child
            .as_ref()
            .context("Missing isolated child origin")?;
        ensure!(
            !provider.handles_tools_internally(),
            "Selected route cannot enforce child native-tool policy through the host"
        );
        child
            .identity
            .resolution
            .validate_provider(provider.as_ref())
            .map_err(|error| {
                anyhow::anyhow!(
                    "Child provider differs from its concrete execution receipt: {error:?}"
                )
            })?;
        let selection = crate::config::config().tools.selection();
        let mut disabled = selection.disabled_tools;
        disabled.extend(
            crate::tool::child_policy::ADMIN_TOOLS
                .iter()
                .map(|name| name.to_string()),
        );
        let continuation = session.provider_session_id.clone();
        let mut agent = Self::build_base(
            provider,
            registry,
            session,
            selection.allowed_tools,
            disabled,
        );
        agent.instruction_repositories = repositories;
        agent.provider_session_id = continuation;
        agent.memory_enabled = false;
        if let Err(error) = agent.registry.bind_child_policy(&agent.session) {
            crate::tool::clear_session_tool_policy(&agent.session.id);
            return Err(error);
        }
        agent.reseed_context_runtime_from_session();
        Ok(agent)
    }

    /// Settings, structural notices and the exact parent prompt commit together.
    /// Queue submission never calls this; its owner invokes it only at FIFO start.
    pub(crate) async fn prepare_isolated_turn(
        &mut self,
        run_id: &str,
        prompt: &str,
        permission: Option<Permission>,
        preset: Option<crate::instruction::TaskPresetActivation>,
    ) -> Result<()> {
        self.session.validate_active_agent_profile()?;
        let child = self
            .session
            .isolated_child
            .as_ref()
            .context("Not an isolated child")?;
        ensure!(
            child.last_started_run.as_deref() != Some(run_id),
            "Child turn already entered history; uncertain work is not replayed"
        );
        ensure!(!prompt.trim().is_empty(), "Child prompt must be nonempty");
        let previous = self.session.clone();
        let prepared: Result<()> = async {
            self.session.stage_child_settings(permission, preset)?;
            self.session.add_user_message_with_origin(Self::user_context_blocks(prompt, Vec::new()), None, None)?;
            self.session.isolated_child.as_mut().context("Missing child state")?.last_started_run = Some(run_id.to_string());
            self.session.validate_active_agent_profile()?;
            let messages = self.projected_provider_messages_for_request()?;
            let operations = crate::context::projection_validation_operations(&self.session.context_view);
            if !operations.is_empty() {
                crate::context::provider_validation::require_supported_projected_messages(self.provider.as_ref(), &messages, &operations)?;
            }
            let tools = self.tool_definitions_for_debug().await?;
            let split = self.build_system_prompt_split(None)?;
            let report = self.evaluate_provider_request_preflight(&messages, 0, &split, &tools, None);
            ensure!(report.pressure != crate::protocol::ContextPressureLevel::Blocked,
                "Child context preparation exceeds safe budget by {} tokens; no automatic compaction or provider request occurred", report.required_reduction_tokens);
            self.session.save()?;
            Ok(())
        }.await;
        if let Err(error) = prepared {
            self.session = previous;
            self.reseed_context_runtime_from_session();
            return Err(error);
        }
        self.rewind_undo_snapshot = None;
        self.registry.bind_child_policy(&self.session)?;
        self.reseed_context_runtime_from_session();
        Ok(())
    }

    /// No primary server followup, title, review, scheduling or unattended policy
    /// is adopted. Ordinary provider/tool retry and streaming cancellation stay
    /// with the existing Agent loop. A clarification reply ends this turn.
    pub(crate) async fn run_prepared_isolated_turn(
        &mut self,
        run_id: &str,
        stop: InterruptSignal,
        events: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<String> {
        self.session.validate_active_agent_profile()?;
        ensure!(
            self.session
                .isolated_child
                .as_ref()
                .and_then(|child| child.last_started_run.as_deref())
                == Some(run_id),
            "Child run does not match its persisted turn"
        );
        ensure!(!stop.is_set(), "Child stopped before provider dispatch");
        let start = self
            .session
            .messages
            .len()
            .checked_sub(1)
            .context("Child turn has no prompt")?;
        let prompt = self.session.messages[start]
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text, .. } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The ordinary streaming loop resets its interactive signal on exit.
        // It must never reset the durable execution owner's Stop authority.
        let child_stop = InterruptSignal::new();
        self.graceful_shutdown = child_stop.clone();
        self.begin_pending_turn(None, &prompt, 0, 0, start, PendingTurnOptions::default());
        let result = {
            let turn = self.run_turn_streaming_mpsc(events);
            tokio::pin!(turn);
            tokio::select! {
                result = &mut turn => result,
                _ = stop.notified() => {
                    child_stop.fire_with_cause(stop.stop_cause().unwrap_or(jcode_tool_types::StopCause::ParentForegroundCancellation));
                    turn.await
                }
            }
        };
        if child_stop.epoch() != 0 && !stop.is_set() {
            stop.fire_with_cause(jcode_tool_types::StopCause::HumanCancellation);
        }
        self.finish_pending_turn();
        self.session.save()?;
        result?;
        ensure!(
            !stop.is_set(),
            "Child run interrupted: {}",
            stop.stop_cause()
                .map(|cause| cause.description())
                .unwrap_or("stopped")
        );
        Ok(self.latest_assistant_text_after(start).unwrap_or_default())
    }
}
