use super::*;
use crate::protocol::{ContextActionRequiredReason, ContextPayloadPressure, ContextPressureLevel};

impl Agent {
    pub(super) fn prepare_startup_context_provider_dispatch(&mut self) -> Result<()> {
        match self
            .session
            .retry_startup_context_metadata_repair_with(self.startup_context_persistence.as_ref())
        {
            crate::session::StartupContextRepairOutcome::NotNeeded
            | crate::session::StartupContextRepairOutcome::Persisted => {}
            crate::session::StartupContextRepairOutcome::StillRequired { error } => {
                logging::warn(&format!(
                    "Startup Context receipt metadata repair remains pending for session {}: {}",
                    self.session.id, error
                ));
            }
        }

        let outcome = self
            .session
            .mark_startup_context_dispatched_with(self.startup_context_persistence.as_ref())
            .map_err(|error| anyhow::anyhow!(error))?;
        if let crate::session::StartupContextDispatchOutcome::Persisted { batches_marked } = outcome
        {
            let accounting = self.session.startup_context_accounting();
            logging::event_info(
                "STARTUP_CONTEXT_DISPATCHED",
                vec![
                    ("session_id", self.session.id.clone()),
                    ("batches", batches_marked.to_string()),
                    ("files", accounting.file_count.to_string()),
                    ("bytes", accounting.captured_bytes.to_string()),
                    ("estimated_tokens", accounting.estimated_tokens.to_string()),
                ],
            );
        }
        Ok(())
    }

    pub(super) fn record_startup_context_provider_acceptance(&mut self) {
        match self
            .session
            .mark_startup_context_provider_accepted_with(self.startup_context_persistence.as_ref())
        {
            crate::session::StartupContextAcceptanceOutcome::Persisted { batches_marked } => {
                logging::event_info(
                    "STARTUP_CONTEXT_PROVIDER_ACCEPTED",
                    vec![
                        ("session_id", self.session.id.clone()),
                        ("batches", batches_marked.to_string()),
                    ],
                );
            }
            crate::session::StartupContextAcceptanceOutcome::MetadataRepairRequired { error } => {
                logging::warn(&format!(
                    "Provider accepted Startup Context for session {}, but receipt metadata persistence requires repair: {}",
                    self.session.id, error
                ));
            }
            crate::session::StartupContextAcceptanceOutcome::NotApplicable
            | crate::session::StartupContextAcceptanceOutcome::Unchanged => {}
        }
    }

    pub(super) fn begin_pending_turn(
        &mut self,
        request_id: Option<u64>,
        user_message: &str,
        image_count: usize,
        pending_input_tokens: usize,
        transcript_len_before_pending: usize,
        options: PendingTurnOptions,
    ) {
        self.active_turn_context = Some(ActiveTurnContext {
            request_id,
            pending_input: request_id
                .map(|id| ContextPendingInputMetadata::new(id, user_message, image_count)),
            pending_input_tokens,
            transcript_len_before_pending,
            reserved_alerts: options.reserved_alerts,
            provider_output_started: false,
            partial_output_checkpointed: false,
            partial_output_persistence_error: None,
            last_preflight: None,
            cache_tracker_before_pending: self.cache_tracker.clone(),
            locked_tools_before_pending: self.locked_tools.clone(),
            mcp_late_register_resolved_before_pending: self.mcp_late_register_resolved,
            tool_output_scan_index_before_pending: self.tool_output_scan_index,
            unattended_context: options.unattended_context,
            emergency_attempted: false,
            emergency_transaction_id: None,
        });
    }

    pub(super) fn finish_pending_turn(&mut self) {
        self.active_turn_context = None;
    }

    pub(super) fn abort_pending_turn_setup(&mut self) {
        let Some(mut context) = self.active_turn_context.take() else {
            return;
        };
        self.session
            .truncate_messages(context.transcript_len_before_pending);
        self.restore_pending_turn_runtime_state(&context);
        let provider_messages = self.session.messages_for_provider();
        self.reseed_context_budget_from_messages(&provider_messages, "aborted pending-turn setup");
        if !context.reserved_alerts.is_empty() {
            context.reserved_alerts.append(&mut self.pending_alerts);
            self.pending_alerts = context.reserved_alerts;
        }
    }

    pub(super) fn mark_provider_output_started(&mut self) {
        if let Some(context) = self.active_turn_context.as_mut() {
            context.provider_output_started = true;
        }
    }

    pub(super) fn commit_reserved_memory_for_request(
        &mut self,
        memory: crate::memory::PendingMemory,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) {
        let count = memory.count.max(1);
        let age_ms = memory.computed_at.elapsed().as_millis() as u64;
        crate::memory::commit_reserved_memory(&self.session.id, &memory);
        crate::memory::record_injected_prompt(&memory.prompt, count, age_ms);
        self.record_memory_injection_in_session(&memory);
        let _ = self.prepare_memory_injection_message(&memory);
        if let Some(event_tx) = event_tx {
            let _ = event_tx.send(ServerEvent::MemoryInjected {
                count,
                prompt: memory.prompt.clone(),
                display_prompt: memory.display_prompt.clone(),
                prompt_chars: memory.prompt.chars().count(),
                computed_age_ms: age_ms,
            });
        }
        logging::info(&format!(
            "Memory injected as message after provider acceptance ({} chars)",
            memory.prompt.chars().count()
        ));
    }

    pub(super) fn provider_output_started(&self) -> bool {
        self.active_turn_context
            .as_ref()
            .is_some_and(|context| context.provider_output_started)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "partial-output checkpoint mirrors the provider stream accumulators exactly"
    )]
    pub(super) fn checkpoint_partial_provider_output(
        &mut self,
        text: &str,
        reasoning: &str,
        reasoning_signature: &str,
        openai_reasoning_items: &[ContentBlock],
        tool_calls: &[ToolCall],
        sdk_tool_results: &std::collections::HashMap<String, (String, bool)>,
        generated_image_contexts: &[Vec<ContentBlock>],
        store_reasoning_content: bool,
        token_usage: Option<crate::session::StoredTokenUsage>,
    ) -> Result<()> {
        if !self.provider_output_started() {
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
        if let Some(context) = self.active_turn_context.as_mut() {
            context.partial_output_checkpointed |= checkpointed;
        }
        if !checkpointed {
            return Ok(());
        }

        if !assistant_blocks.is_empty() {
            crate::telemetry::record_assistant_response();
            self.add_message_ext(Role::Assistant, assistant_blocks, None, token_usage);
        }
        if !result_blocks.is_empty() {
            self.add_message(Role::User, result_blocks);
        }
        for blocks in generated_image_contexts {
            self.add_message(Role::User, blocks.clone());
        }
        if let Err(error) = self.session.save() {
            crate::logging::warn(&format!(
                "Failed to persist partial provider output for session {}: {}",
                self.session.id, error
            ));
            if let Some(context) = self.active_turn_context.as_mut() {
                context.partial_output_persistence_error = Some(error.to_string());
            }
        }
        Ok(())
    }

    pub(super) fn evaluate_provider_request_preflight(
        &mut self,
        messages: &[Message],
        memory_tokens: usize,
        split_prompt: &crate::prompt::SplitSystemPrompt,
        tools: &[ToolDefinition],
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> ContextPreflightReport {
        let pending_input_tokens = self
            .active_turn_context
            .as_ref()
            .filter(|context| !context.provider_output_started)
            .map(|context| context.pending_input_tokens)
            .unwrap_or_default();
        let breakdown = crate::context::request_token_breakdown(
            messages,
            pending_input_tokens,
            memory_tokens,
            split_prompt,
            tools,
        );
        let report = crate::context::evaluate_context_preflight(
            self.session.context_view.revision,
            self.provider.context_request_budget(),
            breakdown,
        );
        if let Some(context) = self.active_turn_context.as_mut() {
            context.last_preflight = Some(report.clone());
        }
        if let (Some(event_tx), Some(request_id)) = (
            event_tx,
            self.active_turn_context
                .as_ref()
                .and_then(|context| context.request_id),
        ) {
            let _ = event_tx.send(ServerEvent::ContextPressureUpdated {
                id: request_id,
                session_id: self.session.id.clone(),
                report: report.clone(),
            });
        }
        report
    }

    pub(super) fn block_for_preflight(
        &mut self,
        report: ContextPreflightReport,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<anyhow::Error> {
        debug_assert_eq!(report.pressure, ContextPressureLevel::Blocked);
        let pending_input = match self.rollback_pending_turn_before_output() {
            Ok(pending_input) => pending_input,
            Err(error) => {
                self.emit_context_action_required(
                    ContextActionRequiredReason::PreflightLimit,
                    Some(report),
                    None,
                    None,
                    vec![
                        "The provider request was not sent, but durable pending-turn rollback failed."
                            .to_string(),
                        "The pending turn remains in authoritative history and must not be resubmitted."
                            .to_string(),
                    ],
                    event_tx,
                );
                return Ok(anyhow::anyhow!(
                    "Request not sent, but durable prompt restoration failed; the pending turn remains in authoritative history and must not be resubmitted: {error}"
                ));
            }
        };
        self.emit_context_action_required(
            ContextActionRequiredReason::PreflightLimit,
            Some(report.clone()),
            None,
            pending_input,
            vec![
                "The complete projected request exceeds this provider's safe input budget."
                    .to_string(),
                "The provider request was not sent and no context transformation was applied."
                    .to_string(),
            ],
            event_tx,
        );
        Ok(anyhow::anyhow!(
            "Request not sent: projected input exceeds the safe provider budget by {} token(s); edit context and submit manually",
            report.required_reduction_tokens
        ))
    }

    pub(super) fn has_unattended_context(&self) -> bool {
        self.active_turn_context
            .as_ref()
            .and_then(|context| context.unattended_context.as_ref())
            .is_some_and(jcode_session_types::StoredUnattendedContextAuthorization::is_authorized)
    }

    pub(super) fn block_unattended_preflight(
        &mut self,
        report: ContextPreflightReport,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> anyhow::Error {
        self.block_unattended_preflight_with_detail(
            report,
            event_tx,
            "safe authorized reduction was unavailable or the single emergency retry still did not fit"
                .to_string(),
        )
    }

    pub(super) fn block_unattended_preflight_with_detail(
        &mut self,
        report: ContextPreflightReport,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
        detail: String,
    ) -> anyhow::Error {
        self.finish_emergency_retry_audit(
            jcode_session_types::StoredContextEmergencyRetryOutcome::Blocked {
                required_reduction_tokens: report.required_reduction_tokens,
            },
        );
        self.emit_context_action_required(
            ContextActionRequiredReason::PreflightLimit,
            Some(report.clone()),
            None,
            None,
            vec![
                "The explicitly unattended request remains protected in authoritative history."
                    .to_string(),
                crate::util::truncate_str(&detail, 512).to_string(),
                "No second emergency transaction or automatic raw-context mutation was attempted."
                    .to_string(),
            ],
            event_tx,
        );
        anyhow::anyhow!(
            "Unattended request blocked after authorized context recovery: {} token(s) still required; {}",
            report.required_reduction_tokens,
            crate::util::truncate_str(&detail, 512)
        )
    }

    pub(super) fn handle_provider_size_rejection(
        &mut self,
        error: &str,
        request_payload: ContextPayloadPressure,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) -> Result<Option<anyhow::Error>> {
        let reason = if jcode_provider_core::is_request_payload_too_large_error(error) {
            ContextActionRequiredReason::PayloadTooLarge
        } else if Self::is_context_limit_error(error) {
            ContextActionRequiredReason::ProviderContextLimit
        } else {
            return Ok(None);
        };

        if reason == ContextActionRequiredReason::ProviderContextLimit
            && !self.provider_output_started()
        {
            self.finish_emergency_retry_audit(
                jcode_session_types::StoredContextEmergencyRetryOutcome::ProviderRejected,
            );
        }

        let output_started = self.provider_output_started();
        let partial_output_persistence_error = self
            .active_turn_context
            .as_ref()
            .and_then(|context| context.partial_output_persistence_error.clone());
        let partial_output_checkpointed = self
            .active_turn_context
            .as_ref()
            .is_some_and(|context| context.partial_output_checkpointed);
        let mut preflight = self
            .active_turn_context
            .as_ref()
            .and_then(|context| context.last_preflight.clone());
        if reason == ContextActionRequiredReason::ProviderContextLimit
            && let Some(report) = preflight.as_mut()
        {
            report.pressure = ContextPressureLevel::Blocked;
            report.required_reduction_tokens = report.required_reduction_tokens.max(1);
            report.remaining_safe_input_tokens = 0;
        }
        let (pending_input, rollback_failed) = if output_started {
            (None, None)
        } else {
            match self.rollback_pending_turn_before_output() {
                Ok(pending_input) => (pending_input, None),
                Err(error) => (None, Some(error)),
            }
        };
        let payload =
            (reason == ContextActionRequiredReason::PayloadTooLarge).then_some(request_payload);
        let mut details = if rollback_failed.is_some() {
            vec![
                "The provider request failed before output, but durable pending-turn rollback failed."
                    .to_string(),
                "The pending turn remains in authoritative history and must not be resubmitted."
                    .to_string(),
            ]
        } else {
            match (reason, output_started) {
            (ContextActionRequiredReason::ProviderContextLimit, false) => vec![
                "The provider rejected the request for context length before producing output."
                    .to_string(),
                "The unanswered pending turn was removed from authoritative history; edit context and submit manually."
                    .to_string(),
            ],
            (ContextActionRequiredReason::ProviderContextLimit, true) => vec![
                "The provider rejected a continuation after output had already begun."
                    .to_string(),
                "Partial output and the authoritative turn were preserved; edit context before continuing."
                    .to_string(),
            ],
            (ContextActionRequiredReason::PayloadTooLarge, false) => vec![
                "The provider rejected the serialized payload before producing output."
                    .to_string(),
                "Images, attachments, and the unanswered prompt were preserved without automatic resend."
                    .to_string(),
            ],
            (ContextActionRequiredReason::PayloadTooLarge, true) => vec![
                "The provider rejected a continuation payload after output had already begun."
                    .to_string(),
                "Partial output, images, attachments, and authoritative history were preserved."
                    .to_string(),
            ],
                (ContextActionRequiredReason::PreflightLimit, _) => unreachable!(),
            }
        };
        if output_started && !partial_output_checkpointed {
            details = vec![
                crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE.to_string(),
                "The authoritative pending turn was retained; edit context before continuing."
                    .to_string(),
            ];
        }
        if let Some(error) = partial_output_persistence_error.as_ref() {
            details.push(crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE.to_string());
            details.push(format!("Persistence error: {error}"));
        }
        self.emit_context_action_required(
            reason,
            preflight,
            payload,
            pending_input,
            details,
            event_tx,
        );
        let message = if let Some(error) = rollback_failed {
            return Ok(Some(anyhow::anyhow!(
                "Provider rejected the request and durable prompt restoration failed; the pending turn remains in authoritative history and must not be resubmitted: {error}"
            )));
        } else {
            match reason {
                ContextActionRequiredReason::ProviderContextLimit => {
                    "Provider context limit reached; no automatic compaction or retry was performed"
                }
                ContextActionRequiredReason::PayloadTooLarge => {
                    "Provider payload limit reached; images were preserved and no automatic retry was performed"
                }
                ContextActionRequiredReason::PreflightLimit => unreachable!(),
            }
        };
        if let Some(error) = partial_output_persistence_error {
            return Ok(Some(anyhow::anyhow!(
                "{message}; partial output persistence failed: {error}"
            )));
        }
        Ok(Some(anyhow::anyhow!(message)))
    }

    fn rollback_pending_turn_before_output(
        &mut self,
    ) -> Result<Option<ContextPendingInputMetadata>> {
        let Some(context) = self.active_turn_context.take() else {
            return Ok(None);
        };
        if context.provider_output_started {
            self.active_turn_context = Some(context);
            return Ok(None);
        }

        let removed_messages =
            self.session.messages[context.transcript_len_before_pending..].to_vec();
        self.session
            .truncate_messages(context.transcript_len_before_pending);
        self.tool_output_scan_index = self.tool_output_scan_index.min(self.session.messages.len());
        let provider_messages = self.session.messages_for_provider();
        self.reseed_context_budget_from_messages(
            &provider_messages,
            "prompt-safe pending-turn rollback",
        );
        if let Err(error) = self.session.save() {
            let mut restored = self.session.messages.clone();
            restored.extend(removed_messages);
            self.session.replace_messages(restored);
            let provider_messages = self.session.messages_for_provider();
            self.reseed_context_budget_from_messages(
                &provider_messages,
                "failed prompt-safe rollback restoration",
            );
            self.restore_pending_turn_runtime_state(&context);
            self.active_turn_context = Some(context);
            return Err(error);
        }
        self.restore_pending_turn_runtime_state(&context);
        if !context.reserved_alerts.is_empty() {
            let mut alerts = context.reserved_alerts;
            alerts.append(&mut self.pending_alerts);
            self.pending_alerts = alerts;
        }
        Ok(context.pending_input)
    }

    fn restore_pending_turn_runtime_state(&mut self, context: &ActiveTurnContext) {
        self.cache_tracker = context.cache_tracker_before_pending.clone();
        self.locked_tools = context.locked_tools_before_pending.clone();
        self.mcp_late_register_resolved = context.mcp_late_register_resolved_before_pending;
        self.tool_output_scan_index = context.tool_output_scan_index_before_pending;
    }

    fn emit_context_action_required(
        &self,
        reason: ContextActionRequiredReason,
        preflight: Option<ContextPreflightReport>,
        payload: Option<ContextPayloadPressure>,
        pending_input: Option<ContextPendingInputMetadata>,
        details: Vec<String>,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) {
        let Some(event_tx) = event_tx else {
            return;
        };
        let request_id = pending_input
            .as_ref()
            .map(|pending| pending.request_id)
            .or_else(|| {
                self.active_turn_context
                    .as_ref()
                    .and_then(|context| context.request_id)
            });
        let Some(request_id) = request_id else {
            return;
        };
        let required_reduction_tokens = preflight
            .as_ref()
            .map(|report| report.required_reduction_tokens)
            .unwrap_or_default();
        let _ = event_tx.send(ServerEvent::ContextActionRequired {
            id: request_id,
            session_id: self.session.id.clone(),
            context_revision: self.session.context_view.revision,
            reason,
            required_reduction_tokens,
            pending_input,
            preflight,
            payload,
            details,
            automatic_retry: false,
        });
    }
}

pub(super) fn stream_event_confirms_request_acceptance(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::ThinkingStart
            | StreamEvent::ThinkingDelta(_)
            | StreamEvent::TextDelta(_)
            | StreamEvent::ToolUseStart { .. }
            | StreamEvent::ToolResult { .. }
            | StreamEvent::GeneratedImage { .. }
            | StreamEvent::MessageEnd { .. }
            | StreamEvent::OpenAIReasoning { .. }
            | StreamEvent::NativeToolCall { .. }
    )
}

pub(super) fn stream_event_is_provider_output(event: &StreamEvent) -> bool {
    match event {
        StreamEvent::ThinkingStart | StreamEvent::ToolUseStart { .. } => true,
        StreamEvent::ThinkingDelta(text) | StreamEvent::TextDelta(text) => !text.is_empty(),
        StreamEvent::ToolResult { .. }
        | StreamEvent::GeneratedImage { .. }
        | StreamEvent::OpenAIReasoning { .. }
        | StreamEvent::NativeToolCall { .. } => true,
        _ => false,
    }
}
