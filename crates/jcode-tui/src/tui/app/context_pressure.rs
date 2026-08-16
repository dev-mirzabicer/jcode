use super::*;
use crate::protocol::{
    ContextActionRequiredReason, ContextPayloadPressure, ContextPendingInputMetadata,
    ContextPreflightReport, ContextPressureLevel,
};

impl App {
    pub(in crate::tui::app) fn context_pressure_debug_fixture_names() -> &'static [&'static str] {
        &["normal", "notice", "urgent", "blocked", "payload"]
    }

    pub(in crate::tui::app) fn apply_context_pressure_debug_fixture(
        &mut self,
        name: &str,
    ) -> Result<(), String> {
        if !Self::context_pressure_debug_fixture_names().contains(&name) {
            return Err(format!(
                "unknown context pressure fixture {name:?}; available fixtures: {}",
                Self::context_pressure_debug_fixture_names().join(", ")
            ));
        }

        self.clear_context_action_after_context_change();
        self.input = "Synthetic preserved composer draft".to_string();
        self.cursor_pos = self.input.len();
        self.pasted_contents = vec!["synthetic paste backing".to_string()];
        self.pending_images = if name == "payload" {
            vec![("image/png".to_string(), "synthetic-image-data".to_string())]
        } else {
            Vec::new()
        };
        let projected_input_tokens = match name {
            "normal" => 75_000,
            "notice" => 80_000,
            "urgent" => 90_000,
            "blocked" | "payload" => 96_000,
            _ => unreachable!(),
        };
        let report = crate::context::evaluate_context_preflight(
            self.session.context_view.revision,
            jcode_provider_core::ContextRequestBudget::unknown(100_000),
            crate::protocol::ContextRequestTokenBreakdown {
                system_tokens: 0,
                tool_definition_tokens: 0,
                historical_message_tokens: projected_input_tokens,
                pending_input_tokens: 0,
                memory_tokens: 0,
            },
        );
        self.set_local_context_pressure(report.clone());
        let fixture_session_id = self
            .remote_session_id
            .clone()
            .or_else(|| self.context_protocol.accepted_session_id.clone())
            .unwrap_or_else(|| self.session.id.clone());
        self.context_pressure_session_id = Some(fixture_session_id.clone());

        if matches!(name, "blocked" | "payload") {
            let request_id = 9_900;
            let reason = if name == "payload" {
                ContextActionRequiredReason::PayloadTooLarge
            } else {
                ContextActionRequiredReason::PreflightLimit
            };
            let payload = (name == "payload").then_some(ContextPayloadPressure {
                image_count: 1,
                estimated_base64_bytes: "synthetic-image-data".len(),
            });
            self.context_action_request_id = Some(request_id);
            self.context_protocol.action_required =
                Some(super::context_protocol::ContextActionRequiredState {
                    request_id,
                    session_id: fixture_session_id,
                    context_revision: self.session.context_view.revision,
                    reason,
                    required_reduction_tokens: report.required_reduction_tokens,
                    pending_input: None,
                    payload,
                    details: vec!["synthetic visual-acceptance fixture".to_string()],
                    automatic_retry: false,
                });
        }
        Ok(())
    }

    pub(super) fn accept_context_pressure_update(
        &mut self,
        request_id: u64,
        session_id: &str,
        report: ContextPreflightReport,
    ) -> bool {
        if !self.context_event_matches_active_session(session_id) {
            return false;
        }
        if self.is_remote && self.current_message_id != Some(request_id) {
            return false;
        }
        if let Some(revision) = self.context_protocol.accepted_context_revision
            && revision != report.context_revision
        {
            return false;
        }
        if report.pressure != ContextPressureLevel::Blocked {
            self.context_protocol.action_required = None;
            self.context_action_request_id = None;
        }
        self.context_pressure = Some(report);
        self.context_pressure_session_id = Some(session_id.to_string());
        true
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "exact session, revision, request, prompt, pressure, and payload correlation is required"
    )]
    pub(super) fn accept_context_action_required_event(
        &mut self,
        request_id: u64,
        session_id: &str,
        context_revision: u64,
        pending_input: Option<&ContextPendingInputMetadata>,
        preflight: Option<&ContextPreflightReport>,
        payload: Option<&ContextPayloadPressure>,
        automatic_retry: bool,
    ) -> bool {
        if automatic_retry || !self.context_event_matches_active_session(session_id) {
            return false;
        }
        if self
            .context_protocol
            .accepted_session_id
            .as_deref()
            .is_some_and(|accepted| accepted != session_id)
        {
            return false;
        }
        if let Some(revision) = self.context_protocol.accepted_context_revision
            && revision != context_revision
        {
            return false;
        }
        if pending_input.is_some_and(|metadata| metadata.request_id != request_id) {
            return false;
        }

        let pending = self.pending_composer_input.as_ref();
        let mut exact_pending_match = false;
        if let Some(pending) = pending {
            if pending.request_id.or(self.current_message_id) != Some(request_id) {
                return false;
            }
            match pending_input {
                Some(metadata)
                    if metadata.matches(request_id, &pending.expanded, pending.image_count) =>
                {
                    exact_pending_match = true;
                }
                Some(metadata) if metadata.content_sha256.is_empty() => {}
                None => {}
                _ => return false,
            }
        } else if self.current_message_id.is_some_and(|id| id != request_id) {
            return false;
        } else if let (Some(metadata), Some(transport)) =
            (pending_input, self.rate_limit_pending_message.as_ref())
            && !metadata.matches(request_id, &transport.content, transport.images.len())
        {
            return false;
        }
        let had_pending_composer = pending.is_some();
        let has_restorable_composer = had_pending_composer && exact_pending_match;
        let pending_expanded = pending
            .map(|pending| pending.expanded.clone())
            .unwrap_or_default();
        let pending_image_count = pending.map_or(0, |pending| pending.image_count);
        let pending_raw_input = pending
            .map(|pending| pending.raw_input.clone())
            .unwrap_or_default();
        let pending_output_started = pending.is_some_and(|pending| pending.output_started);

        if let Some(report) = preflight {
            if report.context_revision != context_revision {
                return false;
            }
            self.context_pressure = Some(report.clone());
        } else if payload.is_none() {
            self.force_blocked_context_pressure(context_revision);
        }
        self.context_pressure_session_id = Some(session_id.to_string());
        self.context_action_request_id = Some(request_id);

        let images = self
            .rate_limit_pending_message
            .as_ref()
            .filter(|message| {
                message.content == pending_expanded && message.images.len() == pending_image_count
            })
            .map(|message| message.images.clone())
            .unwrap_or_default();

        if has_restorable_composer
            && !pending_output_started
            && pending_input.is_some()
            && let Some(index) = self
                .display_messages
                .iter()
                .rposition(|message| message.role == "user" && message.content == pending_raw_input)
        {
            self.remove_display_message(index);
        }

        self.rate_limit_pending_message = None;
        self.clear_pending_fallback_offer();
        self.current_message_id = None;
        let finalized_partial_output = if pending_output_started || pending_input.is_none() {
            self.finalize_remote_partial_output_for_context_action()
        } else {
            false
        };
        let pending_output_started = pending_output_started || finalized_partial_output;
        self.is_processing = false;
        self.pending_turn = false;
        self.stream_message_ended = false;
        self.processing_started = None;
        self.replay_processing_started_ms = None;
        self.replay_elapsed_override = None;
        self.remote_resume_activity = None;
        self.batch_progress = None;
        self.status = ProcessingStatus::Idle;
        if pending_output_started || pending_input.is_none() {
            self.pending_composer_input = None;
            self.last_submitted_input = None;
            self.set_status_notice(if pending_output_started {
                "Context action required · partial output preserved"
            } else {
                "Context action required · authoritative turn retained"
            });
        } else if has_restorable_composer {
            self.restore_blocked_composer_input(images);
        } else {
            self.pending_composer_input = None;
            self.last_submitted_input = None;
            self.set_status_notice(if had_pending_composer {
                "Context action required · exact prompt restoration unavailable"
            } else {
                "Context action required · request blocked"
            });
        }
        true
    }

    fn finalize_remote_partial_output_for_context_action(&mut self) -> bool {
        let ops = self.stream_buffer.flush();
        self.apply_stream_ops(ops);
        let had_partial_output = !self.streaming.streaming_text.is_empty()
            || self.reasoning_streaming
            || !self.streaming_tool_calls.is_empty();
        if self.reasoning_streaming {
            self.close_reasoning_region(None);
        }
        self.pause_streaming_tps(false);
        if !self.streaming.streaming_text.is_empty() {
            let duration = self.display_turn_duration_secs();
            let content = self.take_streaming_text();
            let content = self.collapse_reasoning_for_commit(content);
            if !content.trim().is_empty() {
                self.push_display_message(DisplayMessage {
                    role: "assistant".to_string(),
                    content,
                    tool_calls: Vec::new(),
                    duration_secs: duration,
                    title: None,
                    tool_data: None,
                });
            }
            self.push_turn_footer(duration);
        } else if self.has_streaming_footer_stats() {
            let duration = self.display_turn_duration_secs();
            self.push_turn_footer(duration);
        }
        crate::tui::mermaid::clear_streaming_preview_diagram();
        self.streaming_tool_calls.clear();
        self.thought_line_inserted = false;
        self.thinking_prefix_emitted = false;
        self.thinking_buffer.clear();
        had_partial_output
    }

    pub(super) fn mark_pending_provider_output_started(&mut self) {
        if let Some(pending) = self.pending_composer_input.as_mut() {
            pending.output_started = true;
        }
    }

    pub(super) fn finish_pending_composer_turn(&mut self) {
        self.pending_composer_input = None;
        self.blocked_composer_restore_pending = false;
        self.partial_output_checkpointed = false;
        self.partial_output_persistence_error = None;
        self.context_action_request_id = None;
        self.last_submitted_input = None;
    }

    pub(super) fn recalculate_local_context_pressure(&mut self) {
        if self.is_remote {
            self.clear_context_action_after_context_change();
            return;
        }
        let Some(previous) = self.context_pressure.as_ref() else {
            return;
        };
        let report = crate::context::evaluate_context_preflight(
            self.session.context_view.revision,
            self.provider.context_request_budget(),
            previous.breakdown.clone(),
        );
        self.set_local_context_pressure(report);
    }

    pub(super) fn clear_context_action_after_context_change(&mut self) {
        self.context_protocol.action_required = None;
        self.context_action_request_id = None;
        self.context_pressure = None;
        self.context_pressure_session_id = None;
    }

    pub(super) fn clear_context_turn_state_for_session_change(&mut self) {
        self.clear_context_action_after_context_change();
        self.pending_composer_input = None;
        self.blocked_composer_restore_pending = false;
        self.partial_output_checkpointed = false;
        self.partial_output_persistence_error = None;
        self.last_submitted_input = None;
        self.rate_limit_pending_message = None;
    }

    pub(super) fn maybe_restore_blocked_composer_input(&mut self) -> bool {
        if self.pending_composer_input.is_none()
            || !self.input.is_empty()
            || !self.pending_images.is_empty()
            || !self.pasted_contents.is_empty()
            || self.is_processing
        {
            return false;
        }
        self.restore_blocked_composer_input(Vec::new())
    }

    pub(super) fn rollback_pending_local_turn_before_output(&mut self) -> bool {
        let Some(pending) = self.pending_composer_input.as_ref() else {
            return false;
        };
        if pending.output_started {
            return false;
        }
        let (Some(session_len), Some(display_len), Some(provider_len)) = (
            pending.local_session_len_before,
            pending.local_display_len_before,
            pending.local_provider_len_before,
        ) else {
            return false;
        };

        let mut images = Vec::new();
        for message in self.session.messages.iter().skip(session_len) {
            for block in &message.content {
                if let ContentBlock::Image { media_type, data } = block {
                    images.push((media_type.clone(), data.clone()));
                }
            }
        }
        let removed_session = self.session.messages[session_len..].to_vec();
        let removed_provider = self.messages[provider_len..].to_vec();
        let removed_display = self.display_messages[display_len..].to_vec();
        let original_tool_output_scan_index = self.tool_output_scan_index;
        self.session.truncate_messages(session_len);
        self.messages.truncate(provider_len);
        while self.display_messages.len() > display_len {
            let last = self.display_messages.len() - 1;
            self.remove_display_message(last);
        }
        self.tool_output_scan_index = self.tool_output_scan_index.min(self.session.messages.len());
        self.reseed_context_runtime_from_provider_messages();
        if let Err(error) = self.session.save() {
            let mut restored_session = self.session.messages.clone();
            restored_session.extend(removed_session);
            self.session.replace_messages(restored_session);
            self.messages.extend(removed_provider);
            for message in removed_display {
                self.push_display_message(message);
            }
            self.tool_output_scan_index = original_tool_output_scan_index;
            self.reseed_context_runtime_from_provider_messages();
            crate::logging::warn(&format!(
                "Failed to persist prompt-safe local turn rollback for session {}: {}",
                self.session.id, error
            ));
            self.set_status_notice(
                "Could not persist prompt restoration; pending turn remains in history",
            );
            return false;
        }
        self.session_save_pending = false;
        self.current_turn_system_reminder = None;
        self.restore_blocked_composer_input(images);
        true
    }

    pub(super) fn pending_context_input_metadata(&self) -> Option<ContextPendingInputMetadata> {
        let pending = self.pending_composer_input.as_ref()?;
        let request_id = pending.request_id.or(self.current_message_id)?;
        Some(ContextPendingInputMetadata::new(
            request_id,
            &pending.expanded,
            pending.image_count,
        ))
    }

    pub(super) fn set_local_context_pressure(&mut self, report: ContextPreflightReport) {
        if report.pressure != ContextPressureLevel::Blocked {
            self.context_protocol.action_required = None;
            self.context_action_request_id = None;
        }
        self.context_pressure_session_id = Some(self.session.id.clone());
        self.context_pressure = Some(report);
    }

    pub(super) fn set_local_action_required(
        &mut self,
        request_id: u64,
        pending_input: Option<ContextPendingInputMetadata>,
        reason: ContextActionRequiredReason,
        report: Option<ContextPreflightReport>,
        payload: Option<ContextPayloadPressure>,
        details: Vec<String>,
    ) {
        let context_revision = self.session.context_view.revision;
        let required_reduction_tokens = report
            .as_ref()
            .map(|report| report.required_reduction_tokens)
            .unwrap_or_default();
        if let Some(report) = report.as_ref() {
            self.set_local_context_pressure(report.clone());
        } else if payload.is_none() {
            self.force_blocked_context_pressure(context_revision);
        }
        self.context_action_request_id = Some(request_id);
        self.context_protocol.accept_action_required(
            request_id,
            self.session.id.clone(),
            context_revision,
            reason,
            required_reduction_tokens,
            pending_input,
            report,
            payload,
            details,
            false,
        );
    }

    pub(super) fn handle_local_provider_size_error(&mut self, error: &str) -> bool {
        let reason = if is_request_payload_too_large_error(error) {
            ContextActionRequiredReason::PayloadTooLarge
        } else if is_context_limit_error(error) {
            ContextActionRequiredReason::ProviderContextLimit
        } else {
            return false;
        };

        let output_started = self
            .pending_composer_input
            .as_ref()
            .is_some_and(|pending| pending.output_started);
        let pending_metadata = self.pending_context_input_metadata();
        let Some(request_id) = pending_metadata
            .as_ref()
            .map(|metadata| metadata.request_id)
        else {
            return false;
        };
        let partial_output_persistence_error = self.partial_output_persistence_error.take();
        let partial_output_checkpointed = self.partial_output_checkpointed;
        let payload = (reason == ContextActionRequiredReason::PayloadTooLarge).then(|| {
            if let Some(payload) = self
                .pending_composer_input
                .as_ref()
                .and_then(|pending| pending.request_payload_pressure.clone())
            {
                return payload;
            }
            let mut image_count = 0usize;
            let mut estimated_base64_bytes = 0usize;
            if let Some(pending) = self.pending_composer_input.as_ref()
                && let Some(start) = pending.local_session_len_before
            {
                for block in self
                    .session
                    .messages
                    .iter()
                    .skip(start)
                    .flat_map(|message| message.content.iter())
                {
                    if let ContentBlock::Image { data, .. } = block {
                        image_count = image_count.saturating_add(1);
                        estimated_base64_bytes = estimated_base64_bytes.saturating_add(data.len());
                    }
                }
            }
            ContextPayloadPressure {
                image_count,
                estimated_base64_bytes,
            }
        });
        let report = (reason == ContextActionRequiredReason::ProviderContextLimit)
            .then(|| {
                let mut report = self.context_pressure.clone()?;
                report.pressure = ContextPressureLevel::Blocked;
                report.required_reduction_tokens = report.required_reduction_tokens.max(1);
                report.remaining_safe_input_tokens = 0;
                Some(report)
            })
            .flatten();
        let mut details = match reason {
            ContextActionRequiredReason::PayloadTooLarge => vec![
                "Provider rejected the request payload; images and prompt were preserved"
                    .to_string(),
            ],
            ContextActionRequiredReason::ProviderContextLimit => vec![
                "Provider rejected the request context; no compaction or retry was performed"
                    .to_string(),
            ],
            ContextActionRequiredReason::PreflightLimit => Vec::new(),
        };
        if let Some(error) = partial_output_persistence_error.as_ref() {
            details.push(crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE.to_string());
            details.push(format!("Persistence error: {error}"));
        }
        if output_started && !partial_output_checkpointed {
            details = vec![
                crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE.to_string(),
                "The authoritative pending turn was retained; edit context before continuing."
                    .to_string(),
            ];
        }
        if output_started {
            self.set_local_action_required(request_id, None, reason, report, payload, details);
            let durability = if !partial_output_checkpointed {
                " Provider output began, but no structurally complete partial response could be saved."
            } else if partial_output_persistence_error.is_some() {
                " Partial output remains visible in memory but could not be saved durably; keep this session open."
            } else {
                " Partial provider output was preserved."
            };
            self.push_display_message(DisplayMessage::error(format!(
                "Error: {error}{durability} No automatic context mutation or retry was performed."
            )));
            self.finish_pending_composer_turn();
        } else {
            let restored = self.rollback_pending_local_turn_before_output();
            if !restored {
                self.pending_composer_input = None;
                self.last_submitted_input = None;
            }
            self.set_local_action_required(
                request_id,
                restored.then_some(pending_metadata).flatten(),
                reason,
                report,
                payload,
                details,
            );
            let explanation = if !restored {
                "Request blocked, but durable prompt restoration failed. The pending turn remains in authoritative history; do not resubmit it."
            } else {
                match reason {
                    ContextActionRequiredReason::PayloadTooLarge => {
                        "Request payload was too large. Prompt and images were restored; edit context and submit manually."
                    }
                    ContextActionRequiredReason::ProviderContextLimit => {
                        "Provider context limit reached. Prompt and attachments were restored; edit context and submit manually."
                    }
                    ContextActionRequiredReason::PreflightLimit => unreachable!(),
                }
            };
            self.push_display_message(DisplayMessage::error(explanation));
        }
        super::commands::stop_auto_poke_for_non_retryable_error(self, error);
        self.stop_overnight_auto_poke_for_non_retryable_error(error);
        self.set_status_notice("Context action required · open Context Editor");
        true
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "partial-output checkpoint mirrors local stream accumulators"
    )]
    pub(super) fn checkpoint_partial_local_provider_output(
        &mut self,
        text: &str,
        reasoning: &str,
        reasoning_signature: &str,
        openai_reasoning_items: &[ContentBlock],
        tool_calls: &[ToolCall],
        sdk_tool_results: &std::collections::HashMap<String, (String, bool)>,
        generated_image_contexts: &[Vec<ContentBlock>],
        store_reasoning_content: bool,
    ) -> anyhow::Result<()> {
        if !self
            .pending_composer_input
            .as_ref()
            .is_some_and(|pending| pending.output_started)
        {
            return Ok(());
        }

        let mut assistant_blocks = Vec::new();
        if !text.is_empty() {
            assistant_blocks.push(ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            });
        }
        crate::message::push_reasoning_blocks(
            &mut assistant_blocks,
            self.provider.name(),
            reasoning,
            Some(reasoning_signature),
            store_reasoning_content,
        );
        if store_reasoning_content {
            assistant_blocks.extend(openai_reasoning_items.iter().cloned());
        }

        let mut result_blocks = Vec::new();
        for tool_call in tool_calls {
            let Some((content, is_error)) = sdk_tool_results.get(&tool_call.id) else {
                continue;
            };
            assistant_blocks.push(ContentBlock::ToolUse {
                id: tool_call.id.clone(),
                name: tool_call.name.clone(),
                input: tool_call.input.clone(),
                thought_signature: tool_call.thought_signature.clone(),
            });
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tool_call.id.clone(),
                content: content.clone(),
                is_error: Some(*is_error),
            });
        }

        let checkpointed = !assistant_blocks.is_empty()
            || !result_blocks.is_empty()
            || generated_image_contexts
                .iter()
                .any(|blocks| !blocks.is_empty());
        self.partial_output_checkpointed |= checkpointed;
        if !checkpointed {
            return Ok(());
        }

        if !assistant_blocks.is_empty() {
            crate::telemetry::record_assistant_response();
            self.add_provider_message(Message {
                role: Role::Assistant,
                content: assistant_blocks.clone(),
                timestamp: Some(chrono::Utc::now()),
                tool_duration_ms: None,
            });
            self.session.add_message(Role::Assistant, assistant_blocks);
        }
        if !result_blocks.is_empty() {
            self.add_provider_message(Message {
                role: Role::User,
                content: result_blocks.clone(),
                timestamp: Some(chrono::Utc::now()),
                tool_duration_ms: None,
            });
            self.session.add_message(Role::User, result_blocks);
        }
        for blocks in generated_image_contexts {
            self.add_provider_message(Message {
                role: Role::User,
                content: blocks.clone(),
                timestamp: Some(chrono::Utc::now()),
                tool_duration_ms: None,
            });
            self.session.add_message(Role::User, blocks.clone());
        }
        self.commit_pending_streaming_assistant_message();
        if let Err(error) = self.session.save() {
            crate::logging::warn(&format!(
                "Failed to persist partial local provider output for session {}: {}",
                self.session.id, error
            ));
            self.partial_output_persistence_error = Some(error.to_string());
        }
        Ok(())
    }

    fn context_event_matches_active_session(&self, session_id: &str) -> bool {
        let active = self
            .remote_session_id
            .as_deref()
            .unwrap_or(self.session.id.as_str());
        active == session_id
    }

    fn force_blocked_context_pressure(&mut self, context_revision: u64) {
        if let Some(report) = self.context_pressure.as_mut() {
            report.context_revision = context_revision;
            report.pressure = ContextPressureLevel::Blocked;
            report.required_reduction_tokens = report.required_reduction_tokens.max(1);
            report.remaining_safe_input_tokens = 0;
        }
    }

    fn restore_blocked_composer_input(&mut self, images: Vec<(String, String)>) -> bool {
        if let Some(pending) = self.pending_composer_input.as_mut()
            && !images.is_empty()
        {
            pending.restoration_images = Some(images);
        }
        if !self.input.is_empty()
            || !self.pending_images.is_empty()
            || !self.pasted_contents.is_empty()
        {
            self.blocked_composer_restore_pending = true;
            self.set_status_notice("Blocked prompt retained; current composer left unchanged");
            return false;
        }

        let Some(mut pending) = self.pending_composer_input.take() else {
            return false;
        };
        self.input = pending.raw_input;
        self.cursor_pos = self.input.len();
        self.pasted_contents = pending.pasted_contents;
        self.pending_images = pending.restoration_images.take().unwrap_or_default();
        self.blocked_composer_restore_pending = false;
        self.last_submitted_input = None;
        self.reset_tab_completion();
        self.sync_model_picker_preview_from_input();
        self.set_status_notice("Request blocked; prompt and attachments restored");
        true
    }
}
