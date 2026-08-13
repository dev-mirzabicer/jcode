fn context_timestamp() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-08-12T18:00:00Z")
        .expect("valid context fixture timestamp")
        .with_timezone(&chrono::Utc)
}

fn context_identity() -> ContextDraftIdentity {
    ContextDraftIdentity {
        draft_id: "draft-1".to_string(),
        session_id: "session-1".to_string(),
        base_context_revision: 4,
        raw_message_count: 3,
        transcript_digest: 99,
        provider_name: "openai".to_string(),
        model: "gpt-test".to_string(),
        route: "oauth".to_string(),
        created_at: context_timestamp(),
        expires_at: context_timestamp() + chrono::Duration::minutes(30),
    }
}

fn context_economics() -> jcode_session_types::StoredContextEconomics {
    jcode_session_types::StoredContextEconomics {
        projected_tokens_before: 10_000,
        projected_tokens_after: 4_000,
        estimated_total_request_tokens_before: Some(11_000),
        estimated_total_request_tokens_after: Some(5_000),
        unchanged_prefix_items: 2,
        earliest_changed_provider_item: Some(2),
        old_affected_suffix_tokens: 8_000,
        new_affected_suffix_tokens: 2_000,
        deleted_input_tokens: 6_000,
        context_window: Some(372_000),
        safe_input_budget: Some(370_000),
        pricing: None,
        first_request_delta_usd: None,
        recurring_savings_per_turn_usd: None,
        break_even_turns: None,
        assumptions: vec!["subscription route; dollars unknown".to_string()],
    }
}

fn context_validation() -> jcode_provider_core::ContextProjectionValidationReport {
    jcode_provider_core::ContextProjectionValidationReport {
        provider_family: jcode_provider_core::ContextProviderFamily::OpenAiResponses,
        provider_name: "openai".to_string(),
        provider_display_name: "OpenAI".to_string(),
        model: "gpt-test".to_string(),
        evidence_tag: "fixture-v1".to_string(),
        builder_status: jcode_provider_core::ContextProjectionValidationStatus::Supported,
        normalized_item_count: 3,
        formatter_placeholder_count: 0,
        normalization_notes: Vec::new(),
        findings: Vec::new(),
    }
}

fn context_transaction_summary() -> ContextTransactionSummary {
    ContextTransactionSummary {
        id: "transaction-1".to_string(),
        created_at: context_timestamp(),
        base_revision: 4,
        active: true,
        latest_status: Some(
            jcode_session_types::StoredContextTransactionStatusKind::Applied,
        ),
        latest_status_revision: Some(5),
        authorization: jcode_session_types::StoredContextAuthorization::Manual {
            initiated_by: Some("mirza".to_string()),
        },
        operation_counts: ContextOperationCounts {
            range_summaries: 1,
            reasoning_suppressions: 1,
            tool_result_distillations: 0,
        },
        application: None,
        economics: Some(context_economics()),
    }
}

fn context_transaction_result(
    status: jcode_session_types::StoredContextTransactionStatusKind,
) -> ContextTransactionResult {
    ContextTransactionResult {
        transaction: context_transaction_summary(),
        revision: 5,
        status,
        warnings: Vec::new(),
    }
}

fn context_draft() -> ContextDraft {
    ContextDraft {
        identity: context_identity(),
        authorization: jcode_session_types::StoredContextAuthorization::Manual {
            initiated_by: None,
        },
        required_operations: Vec::new(),
        distillation_proposals: Vec::new(),
        ineligible_distillations: Vec::new(),
        preview: ContextDraftPreview {
            raw_stored_message_count: 3,
            current_context_revision: 4,
            proposed_context_revision: 5,
            economics: context_economics(),
            validation: context_validation(),
            formatter_placeholder_count: 0,
            operation_previews: Vec::new(),
            notices: Vec::new(),
        },
        curator_usage: Vec::new(),
    }
}

fn context_snapshot() -> ContextEditorSnapshot {
    ContextEditorSnapshot {
        session_id: "session-1".to_string(),
        context_revision: 4,
        raw_message_count: 1,
        transcript_digest: 99,
        processing: false,
        provider_name: "openai".to_string(),
        provider_display_name: "OpenAI".to_string(),
        model: "gpt-test".to_string(),
        route: "oauth".to_string(),
        context_window: 372_000,
        projected_request_tokens: 10_000,
        message_page_start: 0,
        message_page_end: 1,
        next_message_page_start: None,
        messages: vec![ContextEditorMessage {
            message_id: "message-1".to_string(),
            stored_index: 0,
            role: jcode_message_types::Role::User,
            display_role: None,
            timestamp: Some(context_timestamp()),
            raw_provider_tokens: 4,
            projected_provider_tokens: 4,
            preview: "hello".to_string(),
            blocks: Vec::new(),
            tool_group_ids: Vec::new(),
            summary_coverage: None,
            active_operations: Vec::new(),
            removable_reasoning_kinds: Vec::new(),
        }],
        active_transactions: vec![context_transaction_summary()],
        emergency_policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
    }
}

fn context_detail() -> ContextMessageDetail {
    ContextMessageDetail {
        session_id: "session-1".to_string(),
        context_revision: 4,
        transcript_digest: 99,
        message_id: "message-1".to_string(),
        stored_index: 0,
        role: jcode_message_types::Role::User,
        display_role: None,
        timestamp: Some(context_timestamp()),
        block_ordinal: 0,
        block_kind: jcode_session_types::StoredContextBlockKind::Text,
        format: ContextMessageDetailFormat::Text,
        content: ContextTextChunk {
            start_char: 0,
            end_char: 5,
            total_chars: 5,
            text: "hello".to_string(),
            next_start_char: None,
        },
        semantic_id: None,
        tool_name: None,
        tool_use_id: None,
        tool_result_is_error: None,
        provider_status: None,
        image_media_type: None,
        image_encoded_bytes: None,
        opaque_signature_present: false,
        encrypted_state_present: false,
    }
}

#[test]
fn context_requests_roundtrip_preserve_ids_and_payloads() -> Result<()> {
    let requests = vec![
        Request::GetContextEditorSnapshot {
            id: 1,
            page_start: 500,
            page_size: Some(250),
        },
        Request::GetContextMessageDetail {
            id: 2,
            expected_context_revision: 4,
            expected_transcript_digest: 99,
            message_id: "message-1".to_string(),
            block_ordinal: 3,
            start_char: 20,
            max_chars: Some(1_024),
        },
        Request::PrepareContextDraft {
            id: 3,
            request: ContextDraftRequest {
                summary_ranges: vec![ContextMessageRangeSelection {
                    start_message_id: "message-1".to_string(),
                    end_message_id: "message-2".to_string(),
                }],
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                }),
                tool_results: vec![ContextToolResultSelection {
                    message_id: "message-3".to_string(),
                    block_ordinal: 1,
                }],
                allow_shadowing_active_operations: true,
                authorization: jcode_session_types::StoredContextAuthorization::Manual {
                    initiated_by: Some("mirza".to_string()),
                },
            },
        },
        Request::CancelContextDraft {
            id: 4,
            draft_id: "draft-1".to_string(),
        },
        Request::GetContextDraftStatus {
            id: 5,
            draft_id: "draft-1".to_string(),
        },
        Request::ApplyContextDraft {
            id: 6,
            draft_id: "draft-1".to_string(),
            selected_distillation_ids: Some(vec!["proposal-1".to_string()]),
        },
        Request::ListContextTransactions {
            id: 7,
            offset: 100,
            limit: Some(50),
        },
        Request::RevertContextTransaction {
            id: 8,
            transaction_id: "transaction-1".to_string(),
        },
        Request::ReapplyContextTransaction {
            id: 9,
            transaction_id: "transaction-1".to_string(),
        },
        Request::SetContextEmergencyPolicy {
            id: 10,
            policy: jcode_session_types::StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns: 5,
                target_headroom_percent: 15,
                allow_reasoning_suppression: true,
                allow_tool_distillation: true,
                allow_oldest_range_summary: false,
                authorization_source: "scheduled-task-1".to_string(),
            },
        },
    ];

    for request in requests {
        let expected_id = request.id();
        let json = serde_json::to_string(&request)?;
        let decoded = parse_request_json(&json)?;
        assert_eq!(decoded.id(), expected_id);
        assert_eq!(serde_json::to_value(decoded)?, serde_json::to_value(request)?);
    }
    Ok(())
}

#[test]
fn context_request_defaults_are_backward_compatible() -> Result<()> {
    let snapshot = parse_request_json(r#"{"type":"get_context_editor_snapshot","id":1}"#)?;
    assert!(matches!(
        snapshot,
        Request::GetContextEditorSnapshot {
            id: 1,
            page_start: 0,
            page_size: None
        }
    ));

    let detail = parse_request_json(
        r#"{"type":"get_context_message_detail","id":2,"expected_context_revision":4,"expected_transcript_digest":99,"message_id":"message-1","block_ordinal":0}"#,
    )?;
    assert!(matches!(
        detail,
        Request::GetContextMessageDetail {
            id: 2,
            start_char: 0,
            max_chars: None,
            ..
        }
    ));

    let history = parse_request_json(r#"{"type":"list_context_transactions","id":3}"#)?;
    assert!(matches!(
        history,
        Request::ListContextTransactions {
            id: 3,
            offset: 0,
            limit: None
        }
    ));

    let apply = parse_request_json(
        r#"{"type":"apply_context_draft","id":4,"draft_id":"draft-1"}"#,
    )?;
    assert!(matches!(
        apply,
        Request::ApplyContextDraft {
            id: 4,
            selected_distillation_ids: None,
            ..
        }
    ));
    Ok(())
}

fn context_event_id(event: &ServerEvent) -> u64 {
    match event {
        ServerEvent::ContextEditorSnapshot { id, .. }
        | ServerEvent::ContextMessageDetail { id, .. }
        | ServerEvent::ContextDraftProgress { id, .. }
        | ServerEvent::ContextDraftReady { id, .. }
        | ServerEvent::ContextDraftApplying { id, .. }
        | ServerEvent::ContextDraftFailed { id, .. }
        | ServerEvent::ContextDraftStale { id, .. }
        | ServerEvent::ContextDraftCanceled { id, .. }
        | ServerEvent::ContextDraftExpired { id, .. }
        | ServerEvent::ContextDraftApplied { id, .. }
        | ServerEvent::ContextTransactionHistory { id, .. }
        | ServerEvent::ContextTransactionApplied { id, .. }
        | ServerEvent::ContextTransactionReverted { id, .. }
        | ServerEvent::ContextTransactionReapplied { id, .. }
        | ServerEvent::ContextRequestRejected { id, .. }
        | ServerEvent::ContextActionRequired { id, .. }
        | ServerEvent::ContextEmergencyPolicyChanged { id, .. } => *id,
        _ => panic!("expected context protocol event"),
    }
}

#[test]
fn context_events_roundtrip_preserve_request_and_draft_correlation() -> Result<()> {
    let identity = context_identity();
    let events = vec![
        ServerEvent::ContextEditorSnapshot {
            id: 1,
            snapshot: context_snapshot(),
        },
        ServerEvent::ContextMessageDetail {
            id: 2,
            detail: context_detail(),
        },
        ServerEvent::ContextDraftProgress {
            id: 3,
            draft_id: "draft-1".to_string(),
            progress: ContextDraftProgress {
                phase: ContextDraftPhase::PreparingArtifacts,
                completed_items: 1,
                total_items: 2,
            },
        },
        ServerEvent::ContextDraftReady {
            id: 4,
            draft: Box::new(context_draft()),
        },
        ServerEvent::ContextDraftApplying {
            id: 5,
            identity: identity.clone(),
        },
        ServerEvent::ContextDraftFailed {
            id: 6,
            identity: identity.clone(),
            error: ContextServiceError::Curator("provider unavailable".to_string()),
        },
        ServerEvent::ContextDraftStale {
            id: 7,
            identity: identity.clone(),
            error: ContextServiceError::Stale("revision changed".to_string()),
        },
        ServerEvent::ContextDraftCanceled {
            id: 8,
            identity: identity.clone(),
        },
        ServerEvent::ContextDraftExpired {
            id: 9,
            identity: identity.clone(),
        },
        ServerEvent::ContextDraftApplied {
            id: 10,
            identity,
            transaction_id: "transaction-1".to_string(),
            revision: 5,
        },
        ServerEvent::ContextTransactionHistory {
            id: 11,
            context_revision: 5,
            total_transactions: 1,
            offset: 0,
            next_offset: None,
            transactions: vec![context_transaction_summary()],
        },
        ServerEvent::ContextTransactionApplied {
            id: 12,
            draft_id: "draft-1".to_string(),
            result: context_transaction_result(
                jcode_session_types::StoredContextTransactionStatusKind::Applied,
            ),
        },
        ServerEvent::ContextTransactionReverted {
            id: 13,
            transaction_id: "transaction-1".to_string(),
            result: context_transaction_result(
                jcode_session_types::StoredContextTransactionStatusKind::Reverted,
            ),
        },
        ServerEvent::ContextTransactionReapplied {
            id: 14,
            transaction_id: "transaction-1".to_string(),
            result: context_transaction_result(
                jcode_session_types::StoredContextTransactionStatusKind::Reapplied,
            ),
        },
        ServerEvent::ContextRequestRejected {
            id: 15,
            request: ContextRequestKind::LegacyCompact,
            draft_id: None,
            transaction_id: None,
            error: ContextServiceError::InvalidSelection("use /context edit".to_string()),
        },
        ServerEvent::ContextActionRequired {
            id: 16,
            session_id: "session-1".to_string(),
            context_revision: 5,
            reason: ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1_024,
            pending_input: Some(ContextPendingInputMetadata {
                request_id: 77,
                content_chars: 12,
                content_digest: 123,
                image_count: 1,
            }),
            details: vec!["input exceeds safe budget".to_string()],
            automatic_retry: false,
        },
        ServerEvent::ContextEmergencyPolicyChanged {
            id: 17,
            session_id: "session-1".to_string(),
            policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
        },
    ];

    for event in events {
        let expected_id = context_event_id(&event);
        let json = encode_event(&event);
        let decoded = parse_event_json(json.trim())?;
        assert_eq!(context_event_id(&decoded), expected_id);
        assert_eq!(serde_json::to_value(decoded)?, serde_json::to_value(event)?);
    }
    Ok(())
}

#[test]
fn context_action_required_contains_metadata_not_raw_prompt_content() {
    let event = ServerEvent::ContextActionRequired {
        id: 9,
        session_id: "session-1".to_string(),
        context_revision: 5,
        reason: ContextActionRequiredReason::ProviderContextLimit,
        required_reduction_tokens: 512,
        pending_input: Some(ContextPendingInputMetadata {
            request_id: 41,
            content_chars: 25,
            content_digest: 0xfeed,
            image_count: 0,
        }),
        details: Vec::new(),
        automatic_retry: false,
    };
    let json = encode_event(&event);
    assert!(json.contains("\"content_chars\":25"));
    assert!(json.contains("\"content_digest\":65261"));
    assert!(!json.contains("raw prompt must stay private"));
    assert!(!json.contains("\"content\":"));
}
