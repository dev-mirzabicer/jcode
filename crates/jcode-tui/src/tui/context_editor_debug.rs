use super::*;
use chrono::{DateTime, Duration, Utc};
use jcode_session_types::{
    StoredContentTarget, StoredContextApplication, StoredContextArtifactGenerator,
    StoredContextBillingMode, StoredContextCacheWarmth, StoredContextEconomics,
    StoredContextInputPriceTier, StoredContextOperation, StoredContextPricingSnapshot,
    StoredContextStatusEvent, StoredContextTransaction, StoredMessageRange,
    StoredProviderValidationEvidence, StoredProviderValidationOutcome,
    StoredRangeBoundaryExpansion, StoredReasoningSelection, StoredReasoningSuppression,
    StoredToolResultDistillation,
};

const DEBUG_FIXTURE_NAMES: &[&str] = &[
    "empty",
    "loading",
    "paged-long-history",
    "search-stable-selection",
    "range-anchor",
    "structural-expansion",
    "shadowed-confirmation",
    "reasoning-menu",
    "keep-latest-statistics",
    "manual-reasoning-ranges",
    "invalid-reasoning-input",
    "trace-only-zero-saving",
    "tool-result-targeting",
    "candidate-scan",
    "curator-available",
    "curator-unavailable",
    "reasoning-only-no-curator",
    "draft-progress",
    "cancel-draft",
    "failure-retry",
    "reconnect-retained-draft",
    "expired-draft",
    "long-final-review",
    "selected-subset-economics",
    "unknown-pricing",
    "tiered-pricing",
    "processing-apply-disabled",
    "stale-review",
    "apply-confirmation",
    "history",
    "transaction-detail",
    "revert-reapply-refresh",
    "unicode-detail-chunks",
    "opaque-metadata-markers",
    "narrow-terminal",
    "emergency-policy-block",
    "emergency-policy-authorized",
];

impl ContextEditor {
    pub(crate) fn debug_fixture_names() -> &'static [&'static str] {
        DEBUG_FIXTURE_NAMES
    }

    pub(crate) fn apply_debug_fixture(&mut self, name: &str) -> Result<(), String> {
        if !DEBUG_FIXTURE_NAMES.contains(&name) {
            return Err(format!(
                "unknown Context Editor fixture {name:?}; available fixtures: {}",
                DEBUG_FIXTURE_NAMES.join(", ")
            ));
        }

        let mode = if matches!(
            name,
            "history" | "transaction-detail" | "revert-reapply-refresh"
        ) {
            ContextEditorOpenMode::History
        } else {
            ContextEditorOpenMode::Edit
        };
        *self = ContextEditor::new(mode);

        match name {
            "loading" => {
                self.status = Some("Loading authoritative Context Editor snapshot…".to_string());
            }
            "empty" => {
                self.apply_snapshot(debug_snapshot(0, 0, false, CuratorFixture::Available));
                self.status =
                    Some("No authoritative messages are available in this session.".to_string());
            }
            "paged-long-history" => {
                self.apply_snapshot(debug_snapshot(640, 250, false, CuratorFixture::Available));
                self.pending_auto_page = None;
                self.cursor = 238;
                self.status = Some(
                    "Showing the first bounded page of a 640-message authoritative transcript."
                        .to_string(),
                );
            }
            "search-stable-selection" => {
                self.apply_snapshot(debug_snapshot(80, 80, false, CuratorFixture::Available));
                self.search_query = "synthetic message 2".to_string();
                self.selected_message_ids
                    .insert("debug-message-2".to_string());
                self.selected_message_ids
                    .insert("debug-message-27".to_string());
                self.status = Some(
                    "Search filters visible rows without changing stable-ID selections."
                        .to_string(),
                );
            }
            "range-anchor" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.cursor = 11;
                self.summary_anchor = Some("debug-message-4".to_string());
                self.selected_message_ids
                    .insert("debug-message-4".to_string());
                self.status =
                    Some("Summary anchor set at message 4. Move and press s again.".to_string());
            }
            "structural-expansion" | "shadowed-confirmation" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.phase = ContextEditorPhase::ConfirmRangeClosure;
                self.pending_range_preview =
                    Some(debug_range_preview(name == "shadowed-confirmation"));
                self.status = Some(
                    "The selected range was expanded to preserve a complete tool call/result group."
                        .to_string(),
                );
            }
            "reasoning-menu" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.modal = Some(ContextEditorModal::ReasoningMenu);
            }
            "keep-latest-statistics" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                });
                self.status = Some(
                    "Resolved 11 replayed reasoning blocks across 7 assistant turns; latest 5 assistant turns remain protected."
                        .to_string(),
                );
            }
            "manual-reasoning-ranges" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.reasoning = Some(ContextReasoningSelectionRequest::MessageRanges {
                    ranges: vec![ContextMessageRangeSelection {
                        start_message_id: "debug-message-6".to_string(),
                        end_message_id: "debug-message-18".to_string(),
                    }],
                });
                self.selected_message_ids
                    .insert("debug-message-6".to_string());
                self.selected_message_ids
                    .insert("debug-message-18".to_string());
                self.status = Some(
                    "Manual reasoning selection resolves to explicit stable block targets at draft time."
                        .to_string(),
                );
            }
            "invalid-reasoning-input" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.modal = Some(ContextEditorModal::ReasoningKeepLatestInput);
                self.reasoning_input = "1001".to_string();
                self.error = Some(
                    "Protected recent assistant turns must be between 0 and 1000.".to_string(),
                );
            }
            "trace-only-zero-saving" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.status = Some(
                    "Visible ReasoningTrace text is transcript-only and saves zero provider tokens."
                        .to_string(),
                );
            }
            "tool-result-targeting" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.cursor = 2;
                self.block_cursor = 0;
                self.tool_targets.insert(("debug-message-2".to_string(), 0));
                self.status = Some(
                    "One tool result is marked for curator evaluation; no context has changed."
                        .to_string(),
                );
            }
            "candidate-scan" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.modal = Some(ContextEditorModal::ToolScan);
                self.tool_scan_input = "2000 5".to_string();
                self.status = Some(
                    "Mechanical scan only marks candidates. The curator remains the semantic gate."
                        .to_string(),
                );
            }
            "curator-available" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.status = Some(
                    "Independent curator route: Synthetic Curator · synthetic-curator-model · high."
                        .to_string(),
                );
            }
            "curator-unavailable" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Unavailable));
                self.error = Some(
                    "No independent curator route is available for summaries or tool-result evaluation."
                        .to_string(),
                );
            }
            "reasoning-only-no-curator" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Unavailable));
                self.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                });
                self.status = Some(
                    "Reasoning-only draft preparation remains available without a curator route."
                        .to_string(),
                );
            }
            "draft-progress" | "cancel-draft" | "reconnect-retained-draft" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                let phase = if name == "reconnect-retained-draft" {
                    ContextDraftPhase::ValidatingProjection
                } else {
                    ContextDraftPhase::PreparingArtifacts
                };
                self.apply_draft_state(ContextClientDraftState::Progress {
                    draft_id: "debug-draft-progress".to_string(),
                    progress: ContextDraftProgress {
                        phase,
                        completed_items: 3,
                        total_items: 8,
                    },
                });
                self.status = Some(match name {
                    "cancel-draft" => "Draft preparation is cancelable without changing context.",
                    "reconnect-retained-draft" => {
                        "Reconnected to a retained server draft through event-driven status recovery."
                    }
                    _ => "Preparing summaries and evaluating selected tool results.",
                }
                .to_string());
            }
            "failure-retry" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.phase = ContextEditorPhase::Editing;
                self.error = Some(
                    "Synthetic curator output omitted a requested artifact. Staged selections remain available for retry."
                        .to_string(),
                );
                self.status =
                    Some("Press g to retry after reviewing the staged operations.".to_string());
            }
            "expired-draft" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                self.apply_draft_state(ContextClientDraftState::Expired(debug_identity()));
            }
            "long-final-review"
            | "selected-subset-economics"
            | "unknown-pricing"
            | "tiered-pricing"
            | "processing-apply-disabled"
            | "stale-review"
            | "apply-confirmation" => {
                let processing = name == "processing-apply-disabled";
                self.apply_snapshot(debug_snapshot(
                    32,
                    32,
                    processing,
                    CuratorFixture::Available,
                ));
                let pricing = match name {
                    "tiered-pricing" => PricingFixture::Tiered,
                    "unknown-pricing" => PricingFixture::Unknown,
                    _ => PricingFixture::Metered,
                };
                let proposal_count = if name == "long-final-review" { 9 } else { 3 };
                let draft = debug_draft(pricing, proposal_count);
                self.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft)));
                if name == "selected-subset-economics" {
                    self.selected_distillation_ids =
                        ["debug-proposal-1".to_string()].into_iter().collect();
                    let mut preview = self
                        .draft
                        .as_ref()
                        .expect("debug fixture draft")
                        .preview
                        .clone();
                    preview.economics.projected_tokens_after = 118_000;
                    preview.economics.deleted_input_tokens = 96_000;
                    self.selection_preview = Some(ContextDraftSelectionPreview {
                        draft_id: "debug-draft-ready".to_string(),
                        selected_distillation_ids: vec!["debug-proposal-1".to_string()],
                        preview,
                    });
                    self.status = Some(
                        "Exact selected-subset economics were recalculated without another curator call."
                            .to_string(),
                    );
                }
                if name == "stale-review" {
                    self.mark_stale(
                        "Authoritative history changed after generation. Refresh and prepare a new review.",
                    );
                }
                if name == "apply-confirmation" {
                    self.modal = Some(ContextEditorModal::ApplyConfirmation);
                }
            }
            "history" | "revert-reapply-refresh" => {
                self.apply_debug_history(name == "revert-reapply-refresh");
            }
            "transaction-detail" => {
                self.apply_debug_history(false);
                self.transaction_detail = Some(debug_transaction_detail());
                self.phase = ContextEditorPhase::InspectTransaction;
                self.preview_scroll = 0;
            }
            "unicode-detail-chunks" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                let mut first = debug_detail(
                    ContextMessageDetailFormat::Text,
                    StoredContextBlockKind::Text,
                    "αβ🙂z",
                    false,
                );
                first.content.total_chars = 8;
                first.content.next_start_char = Some(4);
                let mut buffer = ContextDetailBuffer::new(first)?;
                buffer.merge(ContextMessageDetail {
                    content: ContextTextChunk {
                        start_char: 4,
                        end_char: 8,
                        total_chars: 8,
                        text: "漢字🚀!".to_string(),
                        next_start_char: None,
                    },
                    ..debug_detail(
                        ContextMessageDetailFormat::Text,
                        StoredContextBlockKind::Text,
                        "",
                        false,
                    )
                })?;
                self.detail_buffers
                    .insert(("debug-message-0".to_string(), 0), buffer);
                self.focus = ContextEditorPane::Preview;
                self.status = Some(
                    "Unicode detail chunks are merged by character offset, never byte offset."
                        .to_string(),
                );
            }
            "opaque-metadata-markers" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
                let detail = debug_detail(
                    ContextMessageDetailFormat::MetadataOnly,
                    StoredContextBlockKind::OpenAiReasoning,
                    "",
                    true,
                );
                self.detail_buffers.insert(
                    ("debug-message-0".to_string(), 0),
                    ContextDetailBuffer::new(detail)?,
                );
                self.focus = ContextEditorPane::Preview;
            }
            "narrow-terminal" => {
                self.apply_snapshot(debug_snapshot(48, 48, false, CuratorFixture::Available));
                self.narrow_layout = true;
                self.status = Some(
                    "Narrow layout stacks history and preview while preserving every keyboard action."
                        .to_string(),
                );
            }
            "emergency-policy-block" => {
                self.apply_snapshot(debug_snapshot(32, 32, false, CuratorFixture::Available));
            }
            "emergency-policy-authorized" => {
                let mut snapshot = debug_snapshot(32, 32, false, CuratorFixture::Available);
                snapshot.emergency_policy =
                    jcode_session_types::StoredContextEmergencyPolicy::Authorized {
                        protected_recent_assistant_turns: 7,
                        target_headroom_percent: 12,
                        allow_reasoning_suppression: true,
                        allow_tool_distillation: true,
                        allow_oldest_range_summary: false,
                        authorization_source: "synthetic-debug-source-not-rendered".to_string(),
                    };
                self.apply_snapshot(snapshot);
            }
            _ => unreachable!("validated fixture name"),
        }

        Ok(())
    }

    fn apply_debug_history(&mut self, refreshed: bool) {
        self.phase = ContextEditorPhase::History;
        self.history_session_id = Some("debug-session-context-editor".to_string());
        self.history_context_revision = Some(if refreshed { 13 } else { 12 });
        self.history_total = 36;
        self.history_offset = 0;
        self.history_next_offset = Some(12);
        self.history = (0..12)
            .map(|index| debug_transaction_summary(index, refreshed))
            .collect();
        self.history_cursor = if refreshed { 3 } else { 0 };
        self.status = Some(if refreshed {
            "Authoritative history refreshed once after revert/reapply outcome revision 13."
                .to_string()
        } else {
            "Transaction history preserves applied, reverted, and reapplied provenance.".to_string()
        });
    }
}

#[derive(Clone, Copy)]
enum CuratorFixture {
    Available,
    Unavailable,
}

#[derive(Clone, Copy)]
enum PricingFixture {
    Metered,
    Unknown,
    Tiered,
}

fn debug_timestamp() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
        .expect("static Context Editor debug timestamp")
        .with_timezone(&Utc)
}

fn debug_message(index: usize) -> ContextEditorMessage {
    let (kind, tool_name, tool_use_id, image, removable) = match index % 6 {
        1 => (StoredContextBlockKind::Reasoning, None, None, false, true),
        2 => (
            StoredContextBlockKind::ToolResult,
            Some("synthetic_read".to_string()),
            Some(format!("debug-tool-{index}")),
            false,
            false,
        ),
        3 => (
            StoredContextBlockKind::OpenAiReasoning,
            None,
            None,
            false,
            true,
        ),
        4 => (StoredContextBlockKind::Image, None, None, true, false),
        _ => (StoredContextBlockKind::Text, None, None, false, false),
    };
    ContextEditorMessage {
        message_id: format!("debug-message-{index}"),
        stored_index: index,
        role: if index.is_multiple_of(2) {
            Role::User
        } else {
            Role::Assistant
        },
        display_role: None,
        timestamp: Some(debug_timestamp() + Duration::seconds(index as i64)),
        raw_provider_tokens: 600 + index * 17,
        projected_provider_tokens: 420 + index * 11,
        preview: format!(
            "Synthetic message {index}: deterministic Context Editor visual fixture content"
        ),
        blocks: vec![crate::protocol::ContextEditorBlock {
            ordinal: 0,
            kind,
            semantic_id: tool_use_id.clone(),
            estimated_provider_tokens: 420 + index * 11,
            tool_name,
            tool_use_id: tool_use_id.clone(),
            tool_result_is_error: index.is_multiple_of(17) && index > 0,
            has_image_payload: image,
            has_tool_thought_signature: index.is_multiple_of(19) && index > 0,
            provider_removable_reasoning: removable,
            active_operations: Vec::new(),
        }],
        tool_group_ids: tool_use_id.into_iter().collect(),
        summary_coverage: None,
        active_operations: Vec::new(),
        removable_reasoning_kinds: removable.then_some(kind).into_iter().collect(),
    }
}

fn debug_snapshot(
    raw_message_count: usize,
    page_end: usize,
    processing: bool,
    curator: CuratorFixture,
) -> ContextEditorSnapshot {
    let page_end = page_end.min(raw_message_count);
    ContextEditorSnapshot {
        session_id: "debug-session-context-editor".to_string(),
        context_revision: 12,
        raw_message_count,
        transcript_digest: 0x5afe_cafe,
        processing,
        provider_name: "synthetic-provider".to_string(),
        provider_display_name: "Synthetic Provider".to_string(),
        model: "synthetic-context-model".to_string(),
        route: "synthetic-debug-route".to_string(),
        context_window: 372_000,
        projected_request_tokens: 348_000,
        message_page_start: 0,
        message_page_end: page_end,
        next_message_page_start: (page_end < raw_message_count).then_some(page_end),
        messages: (0..page_end).map(debug_message).collect(),
        active_transactions: vec![debug_transaction_summary(0, false)],
        emergency_policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
        curator_route: matches!(curator, CuratorFixture::Available).then(|| {
            crate::protocol::ContextCuratorRoutePreview {
                provider_name: "synthetic-curator".to_string(),
                provider_display_name: "Synthetic Curator".to_string(),
                model: "synthetic-curator-model".to_string(),
                route: "synthetic-curator-route".to_string(),
                effort: Some("high".to_string()),
            }
        }),
        curator_unavailable_reason: matches!(curator, CuratorFixture::Unavailable).then(|| {
            "No independent synthetic curator route is configured for this fixture.".to_string()
        }),
    }
}

fn debug_range_preview(shadowed: bool) -> ContextRangeClosurePreview {
    ContextRangeClosurePreview {
        session_id: "debug-session-context-editor".to_string(),
        context_revision: 12,
        transcript_digest: 0x5afe_cafe,
        ranges: vec![crate::protocol::ContextClosedRangePreview {
            requested: ContextMessageRangeSelection {
                start_message_id: "debug-message-4".to_string(),
                end_message_id: "debug-message-9".to_string(),
            },
            source_range: StoredMessageRange {
                start_message_id: "debug-message-4".to_string(),
                end_message_id: "debug-message-10".to_string(),
                start_index_hint: 4,
                end_index_hint: 10,
                source_digest: 0x4242,
                message_count: 7,
            },
            boundary_expansions: vec![StoredRangeBoundaryExpansion {
                message_id: "debug-message-10".to_string(),
                stored_index_hint: 10,
                reason: StoredRangeBoundaryExpansionReason::ToolPair {
                    tool_use_id: "debug-tool-8".to_string(),
                },
            }],
            source_tokens: 18_400,
        }],
        shadowed_active_operations: if shadowed {
            vec!["transaction debug-tx-0 reasoning operation 1".to_string()]
        } else {
            Vec::new()
        },
    }
}

fn debug_identity() -> crate::protocol::ContextDraftIdentity {
    crate::protocol::ContextDraftIdentity {
        draft_id: "debug-draft-ready".to_string(),
        session_id: "debug-session-context-editor".to_string(),
        base_context_revision: 12,
        raw_message_count: 32,
        transcript_digest: 0x5afe_cafe,
        provider_name: "synthetic-provider".to_string(),
        model: "synthetic-context-model".to_string(),
        route: "synthetic-debug-route".to_string(),
        created_at: debug_timestamp(),
        expires_at: debug_timestamp() + Duration::minutes(30),
    }
}

fn debug_economics(pricing: PricingFixture) -> StoredContextEconomics {
    let pricing_snapshot = match pricing {
        PricingFixture::Unknown => None,
        PricingFixture::Metered => Some(StoredContextPricingSnapshot {
            billing_mode: StoredContextBillingMode::Metered,
            input_usd_per_million: Some(5.0),
            output_usd_per_million: Some(30.0),
            cache_read_usd_per_million: Some(0.5),
            cache_write_usd_per_million: Some(6.25),
            input_price_tiers: Vec::new(),
            cache_warmth: StoredContextCacheWarmth::Warm,
        }),
        PricingFixture::Tiered => Some(StoredContextPricingSnapshot {
            billing_mode: StoredContextBillingMode::Metered,
            input_usd_per_million: Some(5.0),
            output_usd_per_million: Some(30.0),
            cache_read_usd_per_million: Some(0.5),
            cache_write_usd_per_million: Some(6.25),
            input_price_tiers: vec![StoredContextInputPriceTier {
                above_input_tokens: 272_000,
                input_usd_per_million: 10.0,
                output_usd_per_million: Some(45.0),
                cache_read_usd_per_million: Some(1.0),
                cache_write_usd_per_million: Some(12.5),
            }],
            cache_warmth: StoredContextCacheWarmth::Warm,
        }),
    };
    StoredContextEconomics {
        projected_tokens_before: 214_000,
        projected_tokens_after: 96_000,
        estimated_total_request_tokens_before: Some(348_000),
        estimated_total_request_tokens_after: Some(230_000),
        unchanged_prefix_items: 17,
        earliest_changed_provider_item: Some(17),
        old_affected_suffix_tokens: 186_000,
        new_affected_suffix_tokens: 68_000,
        deleted_input_tokens: 118_000,
        context_window: Some(372_000),
        safe_input_budget: Some(370_000),
        pricing: pricing_snapshot,
        first_request_delta_usd: (!matches!(pricing, PricingFixture::Unknown)).then_some(0.41),
        recurring_savings_per_turn_usd: (!matches!(pricing, PricingFixture::Unknown))
            .then_some(0.059),
        break_even_turns: (!matches!(pricing, PricingFixture::Unknown)).then_some(7),
        assumptions: vec![
            match pricing {
                PricingFixture::Unknown => {
                    "Pricing is unavailable; only token and cache-prefix effects are shown."
                }
                PricingFixture::Tiered => {
                    "The synthetic request crosses the 272K input pricing tier."
                }
                PricingFixture::Metered => {
                    "Synthetic metered pricing assumes the current cache prefix is warm."
                }
            }
            .to_string(),
        ],
    }
}

fn debug_validation() -> crate::provider::ContextProjectionValidationReport {
    crate::provider::ContextProjectionValidationReport {
        provider_family: crate::provider::ContextProviderFamily::OpenRouterCompatible,
        provider_name: "synthetic-provider".to_string(),
        provider_display_name: "Synthetic Provider".to_string(),
        model: "synthetic-context-model".to_string(),
        evidence_tag: "context-editor-debug-fixture-v1".to_string(),
        builder_status: crate::provider::ContextProjectionValidationStatus::Supported,
        normalized_item_count: 26,
        formatter_placeholder_count: 0,
        normalization_notes: vec![
            "Synthetic production-builder validation completed without repair.".to_string(),
        ],
        findings: Vec::new(),
    }
}

fn debug_draft(pricing: PricingFixture, proposal_count: usize) -> ContextDraft {
    let generator = StoredContextArtifactGenerator {
        provider: "synthetic-curator".to_string(),
        model: "synthetic-curator-model".to_string(),
        route: "synthetic-curator-route".to_string(),
        prompt_version: "context-curator-debug-v1".to_string(),
        effort: Some("high".to_string()),
    };
    let reasoning = StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
        selection: StoredReasoningSelection::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 5,
        },
        targets: Vec::new(),
        assistant_turns_affected: 7,
        replay_block_kinds: vec![
            StoredContextBlockKind::Reasoning,
            StoredContextBlockKind::ReasoningTrace,
        ],
        original_token_estimate: 24_000,
        validation_evidence_version: 1,
        validation: vec![StoredProviderValidationEvidence {
            provider: "synthetic-provider".to_string(),
            model: "synthetic-context-model".to_string(),
            request_builder: "synthetic-production-builder-v1".to_string(),
            checked_at: debug_timestamp(),
            outcome: StoredProviderValidationOutcome::Passed,
            warnings: Vec::new(),
        }],
    });
    let proposals = (0..proposal_count)
        .map(|index| {
            let original = 8_000 + index * 1_000;
            let replacement = 700 + index * 50;
            crate::protocol::ContextDistillationProposal {
                proposal_id: format!("debug-proposal-{index}"),
                selected_by_default: true,
                operation: StoredToolResultDistillation {
                    target: StoredContentTarget {
                        message_id: format!("debug-message-{}", index + 2),
                        stored_index_hint: index + 2,
                        block_ordinal_hint: 0,
                        kind: StoredContextBlockKind::ToolResult,
                        semantic_id: Some(format!("debug-tool-{}", index + 2)),
                        expected_hash: 0x1000 + index as u64,
                    },
                    tool_name: "synthetic_read".to_string(),
                    tool_call_id: format!("debug-tool-{}", index + 2),
                    replacement_content: format!(
                        "Synthetic distilled result {index}. Exact paths, failures, counts, and negative findings remain represented."
                    ),
                    original_token_estimate: original,
                    replacement_token_estimate: replacement,
                    replacement_ratio_millionths: ((replacement as u64 * 1_000_000)
                        / original as u64) as u32,
                    preservation_rationale: format!(
                        "Proposal {index} preserves every continuation-relevant synthetic fact below the strict twenty-percent gate."
                    ),
                    uncertainties: if index % 3 == 0 {
                        vec!["Synthetic uncertainty is explicitly retained.".to_string()]
                    } else {
                        Vec::new()
                    },
                    generator: generator.clone(),
                    created_at: debug_timestamp(),
                },
            }
        })
        .collect::<Vec<_>>();
    let economics = debug_economics(pricing);
    ContextDraft {
        identity: debug_identity(),
        authorization: StoredContextAuthorization::Manual { initiated_by: None },
        required_operations: vec![reasoning],
        distillation_proposals: proposals,
        ineligible_distillations: vec![crate::protocol::ContextIneligibleDistillation {
            request_id: "debug-ineligible-0".to_string(),
            tool_name: "synthetic_bash".to_string(),
            tool_call_id: "debug-tool-ineligible".to_string(),
            reason: "Safe reduction below twenty percent was not possible without losing exact failure output."
                .to_string(),
            uncertainties: vec!["The full synthetic result remains authoritative.".to_string()],
        }],
        preview: crate::protocol::ContextDraftPreview {
            raw_stored_message_count: 32,
            current_context_revision: 12,
            proposed_context_revision: 13,
            economics: economics.clone(),
            validation: debug_validation(),
            formatter_placeholder_count: 0,
            operation_previews: vec![
                crate::protocol::ContextOperationPreview::ReasoningSuppression {
                    target_count: 11,
                    assistant_turns_affected: 7,
                    replay_block_kinds: vec![StoredContextBlockKind::Reasoning],
                    removed_tokens: 24_000,
                },
                crate::protocol::ContextOperationPreview::ToolResultDistillation {
                    proposal_id: "debug-proposal-0".to_string(),
                    tool_name: "synthetic_read".to_string(),
                    tool_call_id: "debug-tool-2".to_string(),
                    original_tokens: 8_000,
                    replacement_tokens: 700,
                    selected_by_default: true,
                },
            ],
            notices: vec![
                "Raw stored message count remains exactly 32.".to_string(),
                "Provider continuation will restart from one fresh full request.".to_string(),
            ],
        },
        curator_usage: vec![jcode_session_types::StoredContextCuratorUsage {
            provider: "synthetic-curator".to_string(),
            model: "synthetic-curator-model".to_string(),
            route: "synthetic-curator-route".to_string(),
            input_tokens: 12_000,
            output_tokens: 1_400,
            cache_read_input_tokens: Some(4_000),
            cache_creation_input_tokens: Some(600),
            cost_usd: Some(0.19),
        }],
    }
}

fn debug_transaction_summary(index: usize, refreshed: bool) -> ContextTransactionSummary {
    let revision = 12 + index as u64;
    let active = index % 3 != 1;
    ContextTransactionSummary {
        id: format!("debug-transaction-{index}"),
        created_at: debug_timestamp() - Duration::minutes(index as i64 * 7),
        base_revision: revision.saturating_sub(1),
        active,
        latest_status: Some(if refreshed && index == 3 {
            StoredContextTransactionStatusKind::Reapplied
        } else if active {
            StoredContextTransactionStatusKind::Applied
        } else {
            StoredContextTransactionStatusKind::Reverted
        }),
        latest_status_revision: Some(revision),
        authorization: StoredContextAuthorization::Manual { initiated_by: None },
        operation_counts: crate::protocol::ContextOperationCounts {
            range_summaries: usize::from(index.is_multiple_of(2)),
            reasoning_suppressions: 1,
            tool_result_distillations: index % 4,
        },
        application: Some(StoredContextApplication {
            provider: "synthetic-provider".to_string(),
            model: "synthetic-context-model".to_string(),
            route: "synthetic-debug-route".to_string(),
            context_window: Some(372_000),
        }),
        economics: Some(debug_economics(PricingFixture::Metered)),
    }
}

fn debug_transaction_detail() -> ContextTransactionDetail {
    let draft = debug_draft(PricingFixture::Metered, 3);
    let mut operations = draft.required_operations;
    operations.extend(
        draft
            .distillation_proposals
            .into_iter()
            .map(|proposal| StoredContextOperation::ToolResultDistillation(proposal.operation)),
    );
    ContextTransactionDetail {
        session_id: "debug-session-context-editor".to_string(),
        context_revision: 12,
        transaction: StoredContextTransaction {
            id: "debug-transaction-detail".to_string(),
            base_revision: 11,
            created_at: debug_timestamp(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations,
            status_events: vec![
                StoredContextStatusEvent {
                    revision: 12,
                    timestamp: debug_timestamp(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: Some("Synthetic manual application completed atomically.".to_string()),
                },
                StoredContextStatusEvent {
                    revision: 13,
                    timestamp: debug_timestamp() + Duration::minutes(3),
                    kind: StoredContextTransactionStatusKind::Reapplied,
                    reason: Some(
                        "Synthetic transaction was reapplied after explicit review.".to_string(),
                    ),
                },
            ],
            application: Some(StoredContextApplication {
                provider: "synthetic-provider".to_string(),
                model: "synthetic-context-model".to_string(),
                route: "synthetic-debug-route".to_string(),
                context_window: Some(372_000),
            }),
            economics: Some(debug_economics(PricingFixture::Metered)),
            curator_usage: draft.curator_usage,
            emergency_audit: None,
        },
    }
}

fn debug_detail(
    format: ContextMessageDetailFormat,
    block_kind: StoredContextBlockKind,
    text: &str,
    opaque: bool,
) -> ContextMessageDetail {
    let total_chars = text.chars().count();
    ContextMessageDetail {
        session_id: "debug-session-context-editor".to_string(),
        context_revision: 12,
        transcript_digest: 0x5afe_cafe,
        message_id: "debug-message-0".to_string(),
        stored_index: 0,
        role: Role::User,
        display_role: None,
        timestamp: Some(debug_timestamp()),
        block_ordinal: 0,
        block_kind,
        format,
        content: ContextTextChunk {
            start_char: 0,
            end_char: total_chars,
            total_chars,
            text: text.to_string(),
            next_start_char: None,
        },
        semantic_id: opaque.then(|| "debug-opaque-item".to_string()),
        tool_name: None,
        tool_use_id: None,
        tool_result_is_error: None,
        provider_status: opaque.then(|| "completed".to_string()),
        image_media_type: opaque.then(|| "image/png".to_string()),
        image_encoded_bytes: opaque.then_some(24_576),
        opaque_signature_present: opaque,
        encrypted_state_present: opaque,
    }
}
