use super::*;

impl App {
    pub(super) fn ensure_provider_messages_hydrated(&mut self) {
        if !self.is_remote || !self.messages.is_empty() || self.session.messages.is_empty() {
            return;
        }

        let provider_messages = self.session.raw_messages_for_provider_uncached();
        self.replace_provider_messages(provider_messages);
    }

    pub(super) fn materialized_provider_messages(&self) -> Vec<Message> {
        if self.is_remote || !self.messages.is_empty() {
            self.messages.clone()
        } else {
            self.session.raw_messages_for_provider_uncached()
        }
    }

    /// Materialize the exact provider view for a normal request.
    ///
    /// Raw provider-shaped history remains available to editor, migration, export,
    /// transfer, and diagnostics paths. A live local provider request must instead
    /// fail closed if the persisted context projection is invalid.
    pub(super) fn projected_messages_for_provider_send(&mut self) -> Result<Vec<Message>, String> {
        self.ensure_provider_messages_hydrated();
        if self.is_remote {
            return Ok(self.messages.clone());
        }
        self.session
            .projected_messages_for_provider()
            .map_err(|error| {
                format!(
                    "The provider request was not sent because the active context view could not be projected: {error}. Open /context history to inspect or revert the invalid transaction."
                )
            })
    }

    pub(super) fn local_transcript_message_count(&self) -> usize {
        if self.is_remote {
            self.messages.len()
        } else {
            self.session.messages.len()
        }
    }

    pub(super) fn add_provider_message(&mut self, message: Message) {
        if self.is_remote {
            self.ensure_provider_messages_hydrated();
            self.messages.push(message.clone());
        }

        if !self.is_remote {
            let context_budget = self.registry.context_budget();
            if let Ok(mut tracker) = context_budget.try_write() {
                tracker.record_message(&message);
            } else {
                crate::logging::warn("Context budget lock unavailable during TUI message append");
            }
        }
    }

    pub(super) fn replace_provider_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.last_injected_memory_signature = None;
        self.reset_tool_output_tracking();
        if self.is_remote {
            self.reseed_context_runtime_from_provider_messages();
        } else {
            self.reseed_context_budget_from_messages(
                &self.messages,
                "TUI provider history replaced",
            );
        }
        self.note_runtime_memory_event_force("provider_messages_replaced", "provider_view_reset");
    }

    pub(super) fn clear_provider_messages(&mut self) {
        self.messages.clear();
        self.last_injected_memory_signature = None;
        self.reset_tool_output_tracking();
        if self.is_remote {
            self.reseed_context_runtime_from_provider_messages();
        } else {
            self.reseed_context_budget_from_messages(&[], "TUI provider history cleared");
        }
        self.note_runtime_memory_event_force("provider_messages_cleared", "provider_view_cleared");
    }

    pub(super) fn reset_tool_output_tracking(&mut self) {
        self.tool_call_ids.clear();
        self.tool_result_ids.clear();
        self.tool_output_scan_index = 0;
    }

    pub(super) fn reseed_context_runtime_from_provider_messages(&mut self) {
        if self.is_remote {
            let context_budget = self.registry.context_budget();
            if let Ok(mut tracker) = context_budget.try_write() {
                tracker.set_budget(self.context_limit as usize);
                tracker.reset();
            } else {
                crate::logging::warn(
                    "Context budget lock unavailable during remote TUI history replacement",
                );
            }
            return;
        }

        match self.session.projected_messages_for_provider() {
            Ok(provider_messages) => self.reseed_context_budget_from_messages(
                &provider_messages,
                "TUI provider-view reseed",
            ),
            Err(error) => {
                self.reseed_context_budget_from_messages(&[], "invalid TUI context projection");
                crate::logging::error(&format!(
                    "Cannot seed local TUI provider context for session {}: {}",
                    self.session.id, error
                ));
            }
        }
    }

    pub(super) fn reseed_context_budget_from_messages(&self, messages: &[Message], reason: &str) {
        let context_budget = self.registry.context_budget();
        if let Ok(mut tracker) = context_budget.try_write() {
            tracker.set_budget(self.context_limit as usize);
            tracker.seed_messages(messages);
        } else {
            crate::logging::warn(&format!(
                "Context budget lock unavailable during {reason}; accounting was not reseeded"
            ));
        }
    }

    pub fn set_status_notice(&mut self, text: impl Into<String>) {
        self.status_notice = Some((text.into(), Instant::now()));
    }

    /// Stash a persistent startup notice card and show it immediately.
    ///
    /// The card is also re-applied once the remote History bootstrap clears the
    /// transcript for a brand-new session, so launch-hotkey / welcome tips stay
    /// visible on the idle screen instead of flashing for a moment and vanishing.
    pub fn set_pending_startup_notice(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
    ) {
        let title = title.into();
        let message = message.into();
        self.push_display_message(
            DisplayMessage::system(message.clone()).with_title(title.clone()),
        );
        self.pending_startup_notice = Some((title, message));
    }

    /// Re-apply the stashed startup notice card if it is no longer present in the
    /// transcript (e.g. after the History bootstrap reset the display history).
    /// Scoped to the idle screen: once a real conversation has started the notice
    /// is consumed so it never reappears (and never leaks into a switched-to
    /// session).
    pub(crate) fn reapply_pending_startup_notice_if_cleared(&mut self) {
        let Some((title, message)) = self.pending_startup_notice.clone() else {
            return;
        };
        let conversation_started = self
            .display_messages
            .iter()
            .any(|m| matches!(m.role.as_str(), "user" | "assistant" | "tool" | "reasoning"));
        if conversation_started {
            self.pending_startup_notice = None;
            return;
        }
        let already_present = self
            .display_messages
            .iter()
            .any(|m| m.role == "system" && m.content == message);
        if !already_present {
            self.push_display_message(DisplayMessage::system(message).with_title(title));
        }
    }

    pub(crate) fn set_remote_startup_phase(&mut self, phase: super::RemoteStartupPhase) {
        let changed = self.remote_startup_phase.as_ref() != Some(&phase);
        self.remote_startup_phase = Some(phase);
        if changed || self.remote_startup_phase_started.is_none() {
            self.remote_startup_phase_started = Some(Instant::now());
        }
    }

    pub(crate) fn clear_remote_startup_phase(&mut self) {
        self.remote_startup_phase = None;
        self.remote_startup_phase_started = None;
    }

    /// Begin (or restart) the per-connection history-recovery budget.
    ///
    /// Called when a remote connection starts waiting for the bootstrap
    /// `History` payload. Each fresh connection gets a clean budget so a stall on
    /// one connection does not exhaust the retries available to the next.
    pub(crate) fn begin_remote_history_wait(&mut self) {
        self.remote_history_wait_started = Some(Instant::now());
        self.remote_history_recovery_attempts = 0;
        self.remote_history_recovery_last_attempt = None;
    }

    /// Clear the history-recovery watchdog once history has loaded (or the
    /// connection is no longer waiting on it).
    pub(crate) fn clear_remote_history_wait(&mut self) {
        self.remote_history_wait_started = None;
        self.remote_history_recovery_attempts = 0;
        self.remote_history_recovery_last_attempt = None;
    }

    pub(super) fn set_memory_feature_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if !enabled {
            crate::memory::clear_pending_memory(&self.session.id);
            crate::memory::clear_activity();
            crate::memory_agent::reset();
            self.last_injected_memory_signature = None;
        }
    }

    pub(super) fn set_autoreview_feature_enabled(&mut self, enabled: bool) {
        self.autoreview_enabled = enabled;
        self.session.autoreview_enabled = Some(enabled);
    }

    pub(super) fn set_autojudge_feature_enabled(&mut self, enabled: bool) {
        self.autojudge_enabled = enabled;
        self.session.autojudge_enabled = Some(enabled);
    }

    pub(super) fn trigger_save_memory_extraction(&self) {
        let provider_messages = self.materialized_provider_messages();
        if self.is_remote || !self.memory_enabled || provider_messages.len() < 4 {
            return;
        }

        let transcript = crate::memory_agent::build_transcript_for_extraction(&provider_messages);
        crate::memory_agent::trigger_final_extraction_with_dir(
            transcript,
            self.session.id.clone(),
            self.session.working_dir.clone(),
        );
    }

    pub(super) fn memory_prompt_signature(prompt: &str) -> String {
        prompt
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<String>>()
            .join("\n")
    }

    pub(super) fn should_inject_memory_context(&mut self, prompt: &str) -> bool {
        let signature = Self::memory_prompt_signature(prompt);
        let now = Instant::now();
        if let Some((last_signature, last_injected_at)) =
            self.last_injected_memory_signature.as_ref()
            && *last_signature == signature
            && now.duration_since(*last_injected_at).as_secs() < MEMORY_INJECTION_SUPPRESSION_SECS
        {
            return false;
        }
        self.last_injected_memory_signature = Some((signature, now));
        true
    }

    pub(in crate::tui::app) fn clear_active_experimental_feature_notice(&mut self) {
        self.active_experimental_feature_notice = None;
    }

    pub(in crate::tui::app) fn note_experimental_feature_use(
        &mut self,
        key: &'static str,
    ) -> Option<&'static str> {
        const NOTICE: &str = "experimental feature";
        if self
            .experimental_feature_warnings_seen
            .insert(key.to_string())
        {
            self.active_experimental_feature_notice = Some(NOTICE.to_string());
            Some(NOTICE)
        } else {
            None
        }
    }

    pub(in crate::tui::app) fn experimental_feature_key_for_tool(
        tool: &crate::message::ToolCall,
    ) -> Option<&'static str> {
        if tool.name != "swarm" {
            return None;
        }

        let action = tool.input.get("action").and_then(|value| value.as_str());
        let spawns_agents = matches!(action, Some("spawn") | Some("fill_slots"))
            || matches!(action, Some("assign_task") | Some("assign_next"))
                && (tool
                    .input
                    .get("spawn_if_needed")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                    || tool
                        .input
                        .get("prefer_spawn")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false));

        spawns_agents.then_some("swarm_spawn")
    }

    pub(super) fn set_swarm_feature_enabled(&mut self, enabled: bool) {
        self.swarm_enabled = enabled;
        if !enabled {
            self.remote_swarm_members.clear();
        }
    }

    pub(super) fn extract_thought_line(text: &str) -> Option<String> {
        let trimmed = text.trim();
        if trimmed.starts_with("Thought for ") && trimmed.ends_with('s') {
            Some(trimmed.to_string())
        } else {
            None
        }
    }

    /// Handle quit request (Ctrl+C/Ctrl+D). Returns true if should actually quit.
    pub(super) fn handle_quit_request(&mut self) -> bool {
        const QUIT_TIMEOUT: Duration = Duration::from_secs(2);

        if let Some(pending_time) = self.quit_pending
            && pending_time.elapsed() < QUIT_TIMEOUT
        {
            self.session.provider_session_id = self.provider_session_id.clone();
            crate::telemetry::end_session_with_reason(
                self.provider.name(),
                &self.provider.model(),
                crate::telemetry::SessionEndReason::NormalExit,
            );
            self.session.mark_closed();
            let _ = self.session.save();
            self.should_quit = true;
            return true;
        }

        // First press or timeout expired - show warning
        self.quit_pending = Some(Instant::now());
        self.set_status_notice("Press Ctrl+C again to quit");
        false
    }

    fn collect_missing_tool_outputs_since_last_scan(&mut self) -> Vec<(usize, Vec<String>)> {
        let message_len = self.local_transcript_message_count();
        if self.tool_output_scan_index > message_len {
            self.reset_tool_output_tracking();
        }

        let scan_start = self.tool_output_scan_index;
        let mut new_result_ids = Vec::new();
        let mut assistant_tool_uses: Vec<(usize, Vec<String>)> = Vec::new();

        if self.is_remote {
            for (index, msg) in self.messages.iter().enumerate().skip(scan_start) {
                match msg.role {
                    Role::User => {
                        for block in &msg.content {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                                new_result_ids.push(tool_use_id.clone());
                            }
                        }
                    }
                    Role::Assistant => {
                        let tool_uses = msg
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if !tool_uses.is_empty() {
                            assistant_tool_uses.push((index, tool_uses));
                        }
                    }
                }
            }
        } else {
            for (index, msg) in self.session.messages.iter().enumerate().skip(scan_start) {
                match msg.role {
                    Role::User => {
                        for block in &msg.content {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                                new_result_ids.push(tool_use_id.clone());
                            }
                        }
                    }
                    Role::Assistant => {
                        let tool_uses = msg
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if !tool_uses.is_empty() {
                            assistant_tool_uses.push((index, tool_uses));
                        }
                    }
                }
            }
        }

        self.tool_result_ids.extend(new_result_ids);

        let mut missing_repairs = Vec::new();
        for (index, tool_uses) in assistant_tool_uses {
            let mut missing_for_message = Vec::new();
            for id in tool_uses {
                self.tool_call_ids.insert(id.clone());
                if self.tool_result_ids.contains(&id) {
                    continue;
                }
                // Still-executing tools will deliver their own result; a
                // placeholder here becomes a duplicate tool_result that
                // Anthropic rejects. See `jcode_app_core::tool::inflight`.
                if crate::tool::inflight::is_tool_in_flight(&id) {
                    crate::logging::info(&format!(
                        "Skipping missing tool-output repair for {id}: tool is still executing"
                    ));
                    continue;
                }
                missing_for_message.push(id);
            }
            if !missing_for_message.is_empty() {
                missing_repairs.push((index, missing_for_message));
            }
        }

        self.tool_output_scan_index = message_len;
        missing_repairs
    }

    pub(super) fn missing_tool_result_ids(&mut self) -> Vec<String> {
        self.collect_missing_tool_outputs_since_last_scan();
        self.tool_call_ids
            .difference(&self.tool_result_ids)
            .cloned()
            .collect::<Vec<_>>()
    }

    pub(super) fn summarize_tool_results_missing(&mut self) -> Option<String> {
        let missing = self.missing_tool_result_ids();
        if missing.is_empty() {
            return None;
        }
        let sample = missing
            .iter()
            .take(3)
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let count = missing.len();
        let suffix = if count > 3 { "..." } else { "" };
        Some(format!(
            "Missing tool outputs for {} call(s): {}{}",
            count, sample, suffix
        ))
    }

    pub(super) fn repair_missing_tool_outputs(&mut self) -> usize {
        let session_before = self.session.clone();
        let provider_messages_before = self.messages.clone();
        let tool_call_ids_before = self.tool_call_ids.clone();
        let tool_result_ids_before = self.tool_result_ids.clone();
        let scan_index_before = self.tool_output_scan_index;
        let missing_repairs = self.collect_missing_tool_outputs_since_last_scan();
        let mut repaired = 0usize;
        let mut inserted = 0usize;
        for (index, missing_for_message) in missing_repairs {
            for (offset, id) in missing_for_message.iter().enumerate() {
                let tool_block = ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: TOOL_OUTPUT_MISSING_TEXT.to_string(),
                    is_error: Some(true),
                };
                let inserted_message = Message {
                    role: Role::User,
                    content: vec![tool_block.clone()],
                    timestamp: None,
                    tool_duration_ms: None,
                };
                let stored_message = crate::session::StoredMessage {
                    id: id::new_id("message"),
                    role: Role::User,
                    content: vec![tool_block],
                    display_role: None,
                    timestamp: Some(chrono::Utc::now()),
                    tool_duration_ms: None,
                    token_usage: None,
                };
                if self.is_remote || !self.messages.is_empty() {
                    self.messages
                        .insert(index + 1 + inserted + offset, inserted_message);
                }
                self.session
                    .insert_message(index + 1 + inserted + offset, stored_message);
                self.tool_result_ids.insert(id.clone());
                repaired += 1;
            }
            inserted += missing_for_message.len();
        }

        self.tool_output_scan_index = self.local_transcript_message_count();

        if repaired > 0 {
            let reconciliation = match jcode_context_core::reconcile_context_after_transcript_edit(
                &self.session.messages,
                &self.session.context_view,
                chrono::Utc::now(),
                "historical tool-output repair inserted exact provider structure",
            ) {
                Ok(reconciliation) => reconciliation,
                Err(_) => {
                    self.session = session_before;
                    self.messages = provider_messages_before;
                    self.tool_call_ids = tool_call_ids_before;
                    self.tool_result_ids = tool_result_ids_before;
                    self.tool_output_scan_index = scan_index_before;
                    crate::logging::error(
                        "Missing tool-output repair failed safely during context reconciliation",
                    );
                    return 0;
                }
            };
            self.session.context_view = reconciliation.state;
            self.session.provider_session_id = None;
            if self.session.save().is_err() {
                self.session = session_before;
                self.messages = provider_messages_before;
                self.tool_call_ids = tool_call_ids_before;
                self.tool_result_ids = tool_result_ids_before;
                self.tool_output_scan_index = scan_index_before;
                crate::logging::error(
                    "Missing tool-output repair failed safely during session persistence",
                );
                return 0;
            }
            if let Err(error) = self.after_local_provider_context_changed(
                "historical tool repair",
                &format!("inserted {repaired} missing tool output(s) into prior provider history"),
            ) {
                crate::logging::error(&format!(
                    "Persisted tool-output repair could not rebuild provider context: {error}"
                ));
            }
        }

        repaired
    }

    /// Rebuild current session into a new one without tool calls
    pub(super) fn recover_session_without_tools(&mut self) {
        let old_session = self.session.clone();
        let old_messages = old_session.messages.clone();

        let new_session_id = format!("session_recovery_{}", id::new_id("rec"));
        let mut new_session =
            Session::create_with_id(new_session_id, Some(old_session.id.clone()), None);
        new_session.title = old_session.title.clone();
        new_session.custom_title = old_session.custom_title.clone();
        new_session.provider_session_id = old_session.provider_session_id.clone();
        new_session.model = old_session.model.clone();
        new_session.is_canary = old_session.is_canary;
        new_session.testing_build = old_session.testing_build.clone();
        new_session.is_debug = old_session.is_debug;
        new_session.saved = old_session.saved;
        new_session.save_label = old_session.save_label.clone();
        new_session.working_dir = old_session.working_dir.clone();

        self.session = new_session;
        self.clear_provider_messages();
        self.clear_display_messages();
        // Ctrl+R is reachable mid-stream (turn.rs key handling); drop the
        // in-flight streaming render state (including the ephemeral mermaid
        // preview slot) so it cannot leak into the recovered session's
        // transcript. ACTIVE_DIAGRAMS deliberately survives: recovery keeps
        // every text block, so registered diagrams still back retained
        // messages, and body-cache prefix reuse (ui_prepare.rs) would skip
        // re-registering them if we cleared the registry here.
        self.clear_streaming_render_state();
        self.queued_messages.clear();
        self.pasted_contents.clear();
        self.pending_images.clear();
        self.active_skill = None;
        self.provider_session_id = None;
        self.set_side_panel_snapshot(
            crate::side_panel::snapshot_for_session(&self.session.id).unwrap_or_default(),
        );

        for msg in old_messages {
            let role = msg.role.clone();
            let kept_blocks: Vec<ContentBlock> = msg
                .content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::Text { .. }))
                .collect();
            if kept_blocks.is_empty() {
                continue;
            }
            self.add_provider_message(Message {
                role: role.clone(),
                content: kept_blocks.clone(),
                timestamp: None,
                tool_duration_ms: None,
            });
            self.push_display_message(DisplayMessage {
                role: match role {
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                },
                content: kept_blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            });
            let _ = self.session.add_message(role, kept_blocks);
        }
        let _ = self.session.save();

        self.push_display_message(DisplayMessage::system(format!(
            "Recovery complete. New session: {}. Tool calls stripped; context preserved.",
            self.session.id
        )));
        self.set_status_notice("Recovered session");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;

    #[test]
    fn experimental_feature_key_marks_swarm_spawn_actions() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "spawn", "prompt": "try it"}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(
            App::experimental_feature_key_for_tool(&tool),
            Some("swarm_spawn")
        );
    }

    #[test]
    fn experimental_feature_key_marks_spawn_if_needed_assignment() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "assign_task", "spawn_if_needed": true}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(
            App::experimental_feature_key_for_tool(&tool),
            Some("swarm_spawn")
        );
    }

    #[test]
    fn experimental_feature_key_ignores_non_spawning_swarm_actions() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "status"}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(App::experimental_feature_key_for_tool(&tool), None);
    }
}
