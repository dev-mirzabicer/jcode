fn parity_timestamp() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-08-14T11:00:00Z")
        .expect("valid parity timestamp")
        .with_timezone(&chrono::Utc)
}

fn parity_economics() -> jcode_session_types::StoredContextEconomics {
    jcode_session_types::StoredContextEconomics {
        projected_tokens_before: 10_000,
        projected_tokens_after: 4_000,
        estimated_total_request_tokens_before: Some(11_000),
        estimated_total_request_tokens_after: Some(5_000),
        unchanged_prefix_items: 1,
        earliest_changed_provider_item: Some(1),
        old_affected_suffix_tokens: 9_000,
        new_affected_suffix_tokens: 3_000,
        deleted_input_tokens: 6_000,
        context_window: Some(100_000),
        safe_input_budget: Some(98_000),
        pricing: None,
        first_request_delta_usd: None,
        recurring_savings_per_turn_usd: None,
        break_even_turns: None,
        assumptions: vec!["parity fixture".to_string()],
    }
}

fn parity_validation() -> crate::provider::ContextProjectionValidationReport {
    crate::provider::ContextProjectionValidationReport {
        provider_family: crate::provider::ContextProviderFamily::OpenAiResponses,
        provider_name: "openai".to_string(),
        provider_display_name: "OpenAI".to_string(),
        model: "gpt-parity".to_string(),
        evidence_tag: "parity-v1".to_string(),
        builder_status: crate::provider::ContextProjectionValidationStatus::Supported,
        normalized_item_count: 0,
        formatter_placeholder_count: 0,
        normalization_notes: Vec::new(),
        findings: Vec::new(),
    }
}

fn parity_identity() -> crate::protocol::ContextDraftIdentity {
    crate::protocol::ContextDraftIdentity {
        draft_id: "draft-parity".to_string(),
        session_id: "session-parity".to_string(),
        base_context_revision: 4,
        raw_message_count: 0,
        transcript_digest: 77,
        provider_name: "openai".to_string(),
        model: "gpt-parity".to_string(),
        route: "oauth".to_string(),
        created_at: parity_timestamp(),
        expires_at: parity_timestamp() + chrono::Duration::minutes(30),
    }
}

fn parity_draft() -> crate::protocol::ContextDraft {
    crate::protocol::ContextDraft {
        identity: parity_identity(),
        authorization: jcode_session_types::StoredContextAuthorization::Manual { initiated_by: None },
        required_operations: Vec::new(),
        distillation_proposals: Vec::new(),
        ineligible_distillations: Vec::new(),
        preview: crate::protocol::ContextDraftPreview {
            raw_stored_message_count: 0,
            current_context_revision: 4,
            proposed_context_revision: 5,
            economics: parity_economics(),
            validation: parity_validation(),
            formatter_placeholder_count: 0,
            operation_previews: Vec::new(),
            notices: Vec::new(),
        },
        curator_usage: Vec::new(),
    }
}

fn parity_snapshot() -> crate::protocol::ContextEditorSnapshot {
    crate::protocol::ContextEditorSnapshot {
        session_id: "session-parity".to_string(),
        context_revision: 4,
        raw_message_count: 0,
        transcript_digest: 77,
        processing: false,
        provider_name: "openai".to_string(),
        provider_display_name: "OpenAI".to_string(),
        model: "gpt-parity".to_string(),
        route: "oauth".to_string(),
        context_window: 100_000,
        projected_request_tokens: 10_000,
        message_page_start: 0,
        message_page_end: 0,
        next_message_page_start: None,
        messages: Vec::new(),
        active_transactions: Vec::new(),
        emergency_policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
        curator_route: None,
        curator_unavailable_reason: Some("curator unavailable in parity fixture".to_string()),
    }
}

fn parity_transaction_summary() -> crate::protocol::ContextTransactionSummary {
    crate::protocol::ContextTransactionSummary {
        id: "transaction-parity".to_string(),
        created_at: parity_timestamp(),
        base_revision: 4,
        active: true,
        latest_status: Some(jcode_session_types::StoredContextTransactionStatusKind::Applied),
        latest_status_revision: Some(5),
        authorization: jcode_session_types::StoredContextAuthorization::Manual { initiated_by: None },
        operation_counts: crate::protocol::ContextOperationCounts::default(),
        application: None,
        economics: Some(parity_economics()),
    }
}

fn parity_transaction() -> jcode_session_types::StoredContextTransaction {
    jcode_session_types::StoredContextTransaction {
        id: "transaction-parity".to_string(),
        base_revision: 4,
        created_at: parity_timestamp(),
        authorization: jcode_session_types::StoredContextAuthorization::Manual { initiated_by: None },
        operations: Vec::new(),
        status_events: vec![jcode_session_types::StoredContextStatusEvent {
            revision: 5,
            timestamp: parity_timestamp(),
            kind: jcode_session_types::StoredContextTransactionStatusKind::Applied,
            reason: Some("parity transition".to_string()),
        }],
        application: None,
        economics: Some(parity_economics()),
        curator_usage: Vec::new(),
    }
}

fn parity_transaction_result(
    status: jcode_session_types::StoredContextTransactionStatusKind,
) -> crate::protocol::ContextTransactionResult {
    crate::protocol::ContextTransactionResult {
        transaction: parity_transaction_summary(),
        revision: 5,
        status,
        warnings: Vec::new(),
    }
}

fn parity_detail() -> crate::protocol::ContextMessageDetail {
    crate::protocol::ContextMessageDetail {
        session_id: "session-parity".to_string(),
        context_revision: 4,
        transcript_digest: 77,
        message_id: "message-parity".to_string(),
        stored_index: 0,
        role: crate::message::Role::User,
        display_role: None,
        timestamp: Some(parity_timestamp()),
        block_ordinal: 0,
        block_kind: jcode_session_types::StoredContextBlockKind::Text,
        format: crate::protocol::ContextMessageDetailFormat::Text,
        content: crate::protocol::ContextTextChunk {
            start_char: 0,
            end_char: 4,
            total_chars: 4,
            text: "safe".to_string(),
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

fn parity_events() -> Vec<(&'static str, crate::protocol::ServerEvent)> {
    use crate::protocol::{ContextDraftPhase, ContextRequestKind, ContextServiceError, ServerEvent};
    let identity = parity_identity();
    vec![
        (
            "snapshot",
            ServerEvent::ContextEditorSnapshot {
                id: 1,
                snapshot: parity_snapshot(),
            },
        ),
        (
            "detail",
            ServerEvent::ContextMessageDetail {
                id: 2,
                detail: parity_detail(),
            },
        ),
        (
            "range_preview",
            ServerEvent::ContextRangeClosurePreview {
                id: 3,
                preview: crate::protocol::ContextRangeClosurePreview {
                    session_id: "session-parity".to_string(),
                    context_revision: 4,
                    transcript_digest: 77,
                    ranges: Vec::new(),
                    shadowed_active_operations: Vec::new(),
                },
            },
        ),
        (
            "draft_progress",
            ServerEvent::ContextDraftProgress {
                id: 4,
                draft_id: "draft-parity".to_string(),
                progress: crate::protocol::ContextDraftProgress {
                    phase: ContextDraftPhase::PreparingArtifacts,
                    completed_items: 1,
                    total_items: 2,
                },
            },
        ),
        (
            "draft_ready",
            ServerEvent::ContextDraftReady {
                id: 5,
                draft: Box::new(parity_draft()),
            },
        ),
        (
            "draft_applying",
            ServerEvent::ContextDraftApplying {
                id: 6,
                identity: identity.clone(),
            },
        ),
        (
            "draft_failed",
            ServerEvent::ContextDraftFailed {
                id: 7,
                identity: identity.clone(),
                error: ContextServiceError::Curator("safe failure".to_string()),
            },
        ),
        (
            "draft_stale",
            ServerEvent::ContextDraftStale {
                id: 8,
                identity: identity.clone(),
                error: ContextServiceError::Stale("safe stale".to_string()),
            },
        ),
        (
            "draft_canceled",
            ServerEvent::ContextDraftCanceled {
                id: 9,
                identity: identity.clone(),
            },
        ),
        (
            "draft_expired",
            ServerEvent::ContextDraftExpired {
                id: 10,
                identity: identity.clone(),
            },
        ),
        (
            "draft_applied",
            ServerEvent::ContextDraftApplied {
                id: 11,
                identity: identity.clone(),
                transaction_id: "transaction-parity".to_string(),
                revision: 5,
            },
        ),
        (
            "selection_preview",
            ServerEvent::ContextDraftSelectionPreview {
                id: 12,
                preview: crate::protocol::ContextDraftSelectionPreview {
                    draft_id: "draft-parity".to_string(),
                    selected_distillation_ids: Vec::new(),
                    preview: parity_draft().preview,
                },
            },
        ),
        (
            "history",
            ServerEvent::ContextTransactionHistory {
                id: 13,
                context_revision: 4,
                total_transactions: 1,
                offset: 0,
                next_offset: None,
                transactions: vec![parity_transaction_summary()],
            },
        ),
        (
            "transaction_detail",
            ServerEvent::ContextTransactionDetail {
                id: 14,
                detail: Box::new(crate::protocol::ContextTransactionDetail {
                    session_id: "session-parity".to_string(),
                    context_revision: 4,
                    transaction: parity_transaction(),
                }),
            },
        ),
        (
            "transaction_applied",
            ServerEvent::ContextTransactionApplied {
                id: 15,
                draft_id: "draft-parity".to_string(),
                result: parity_transaction_result(
                    jcode_session_types::StoredContextTransactionStatusKind::Applied,
                ),
            },
        ),
        (
            "transaction_reverted",
            ServerEvent::ContextTransactionReverted {
                id: 16,
                transaction_id: "transaction-parity".to_string(),
                result: parity_transaction_result(
                    jcode_session_types::StoredContextTransactionStatusKind::Reverted,
                ),
            },
        ),
        (
            "transaction_reapplied",
            ServerEvent::ContextTransactionReapplied {
                id: 17,
                transaction_id: "transaction-parity".to_string(),
                result: parity_transaction_result(
                    jcode_session_types::StoredContextTransactionStatusKind::Reapplied,
                ),
            },
        ),
        (
            "rejection",
            ServerEvent::ContextRequestRejected {
                id: 18,
                request: ContextRequestKind::MessageDetail,
                draft_id: None,
                transaction_id: None,
                error: ContextServiceError::Stale("safe rejected detail".to_string()),
            },
        ),
        (
            "action_required",
            ServerEvent::ContextActionRequired {
                id: 19,
                session_id: "session-parity".to_string(),
                context_revision: 4,
                reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
                required_reduction_tokens: 1_024,
                pending_input: Some(crate::protocol::ContextPendingInputMetadata {
                    request_id: 91,
                    content_chars: 12,
                    content_digest: 123,
                    image_count: 1,
                }),
                details: vec!["safe pressure metadata".to_string()],
                automatic_retry: false,
            },
        ),
        (
            "policy",
            ServerEvent::ContextEmergencyPolicyChanged {
                id: 20,
                session_id: "session-parity".to_string(),
                policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
            },
        ),
    ]
}

fn prepare_context_parity_app(app: &mut App, event: &crate::protocol::ServerEvent) {
    use crate::protocol::{ContextDraftPhase, ContextRequestKind, ServerEvent};
    app.remote_session_id = Some("session-parity".to_string());
    app.context_revision = 4;
    app.context_protocol.accepted_session_id = Some("session-parity".to_string());
    app.context_protocol.accepted_context_revision = Some(4);
    app.context_protocol.accepted_transcript_digest = Some(77);
    app.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
    app.context_editor_actions.clear();

    if !matches!(event, ServerEvent::ContextEditorSnapshot { .. }) {
        app.context_protocol.begin_snapshot_request(900);
        assert!(app.context_protocol.accept_snapshot(900, parity_snapshot()));
        app.sync_context_editor_from_protocol();
        app.context_editor_actions.clear();
    }

    match event {
        ServerEvent::ContextEditorSnapshot { id, .. } => {
            app.context_protocol.begin_snapshot_request(*id);
        }
        ServerEvent::ContextMessageDetail { id, detail } => {
            app.context_protocol.begin_detail_request(
                *id,
                detail.session_id.clone(),
                detail.context_revision,
                detail.transcript_digest,
                detail.message_id.clone(),
                detail.block_ordinal,
            );
        }
        ServerEvent::ContextRangeClosurePreview { id, preview } => {
            app.context_protocol.begin_range_preview_request(
                *id,
                preview.session_id.clone(),
                preview.context_revision,
                preview.transcript_digest,
                Vec::new(),
            );
        }
        ServerEvent::ContextDraftProgress { id, .. }
        | ServerEvent::ContextDraftReady { id, .. }
        | ServerEvent::ContextDraftApplying { id, .. }
        | ServerEvent::ContextDraftFailed { id, .. }
        | ServerEvent::ContextDraftStale { id, .. }
        | ServerEvent::ContextDraftCanceled { id, .. }
        | ServerEvent::ContextDraftExpired { id, .. }
        | ServerEvent::ContextDraftApplied { id, .. } => {
            app.context_protocol.begin_prepare_draft(*id);
        }
        ServerEvent::ContextDraftSelectionPreview { id, preview } => {
            app.context_protocol.begin_prepare_draft(901);
            assert!(app.context_protocol.accept_draft_progress(
                901,
                preview.draft_id.clone(),
                crate::protocol::ContextDraftProgress {
                    phase: ContextDraftPhase::CalculatingEconomics,
                    completed_items: 0,
                    total_items: 1,
                },
            ));
            app.context_protocol.begin_selection_preview_request(
                *id,
                preview.draft_id.clone(),
                preview.selected_distillation_ids.clone(),
            );
        }
        ServerEvent::ContextTransactionHistory { id, .. } => {
            app.context_protocol
                .begin_history_request(*id, "session-parity".to_string());
        }
        ServerEvent::ContextTransactionDetail { id, detail } => {
            app.context_protocol.begin_transaction_detail_request(
                *id,
                detail.session_id.clone(),
                detail.context_revision,
                detail.transaction.id.clone(),
            );
        }
        ServerEvent::ContextTransactionApplied { id, draft_id, .. } => {
            app.context_protocol.begin_transaction_request(
                *id,
                ContextRequestKind::ApplyDraft,
                draft_id.clone(),
            );
        }
        ServerEvent::ContextTransactionReverted {
            id, transaction_id, ..
        } => {
            app.context_protocol.begin_transaction_request(
                *id,
                ContextRequestKind::RevertTransaction,
                transaction_id.clone(),
            );
        }
        ServerEvent::ContextTransactionReapplied {
            id, transaction_id, ..
        } => {
            app.context_protocol.begin_transaction_request(
                *id,
                ContextRequestKind::ReapplyTransaction,
                transaction_id.clone(),
            );
        }
        ServerEvent::ContextRequestRejected { id, request, .. } => match request {
            ContextRequestKind::MessageDetail => {
                let detail = parity_detail();
                app.context_protocol.begin_detail_request(
                    *id,
                    detail.session_id,
                    detail.context_revision,
                    detail.transcript_digest,
                    detail.message_id,
                    detail.block_ordinal,
                );
            }
            _ => panic!("unsupported parity rejection fixture: {request:?}"),
        },
        ServerEvent::ContextActionRequired { .. } => {}
        ServerEvent::ContextEmergencyPolicyChanged { .. } => {}
        _ => panic!("unexpected non-context parity event"),
    }
}

fn assert_context_apps_equal(label: &str, local: &App, remote: &App) {
    assert_eq!(
        local.context_protocol.test_signature(),
        remote.context_protocol.test_signature(),
        "protocol state diverged for {label}"
    );
    assert_eq!(
        local.context_editor_debug_summary(),
        remote.context_editor_debug_summary(),
        "editor state diverged for {label}"
    );
    assert_eq!(
        local.context_editor_actions, remote.context_editor_actions,
        "follow-up actions diverged for {label}"
    );
    assert_eq!(
        local.status_notice(),
        remote.status_notice(),
        "status copy diverged for {label}"
    );
    assert_eq!(
        local.context_revision, remote.context_revision,
        "UI-only revision diverged for {label}"
    );
}

fn deliver_context_event_pair(
    local: &mut App,
    remote_app: &mut App,
    event: crate::protocol::ServerEvent,
) -> (bool, bool) {
    local
        .local_context_event_tx
        .send(event.clone())
        .expect("local context event channel");
    let local_changed = local.drain_local_context_events();

    let rt = tokio::runtime::Runtime::new().expect("parity runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    let remote_changed = remote_app.handle_server_event(event, &mut remote);
    (local_changed, remote_changed)
}

fn assert_context_event_rejected_identically(
    label: &str,
    expected_event: crate::protocol::ServerEvent,
    rejected_event: crate::protocol::ServerEvent,
) {
    assert_context_event_rejected_identically_after_setup(
        label,
        expected_event,
        rejected_event,
        |_| {},
    );
}

fn assert_context_event_rejected_identically_after_setup(
    label: &str,
    expected_event: crate::protocol::ServerEvent,
    rejected_event: crate::protocol::ServerEvent,
    setup: impl Fn(&mut App),
) {
    let mut local = create_test_app();
    let mut remote_app = create_test_app();
    remote_app.is_remote = true;
    prepare_context_parity_app(&mut local, &expected_event);
    prepare_context_parity_app(&mut remote_app, &expected_event);
    setup(&mut local);
    setup(&mut remote_app);
    local.set_status_notice("preserve parity status".to_string());
    remote_app.set_status_notice("preserve parity status".to_string());

    let local_protocol_before = local.context_protocol.test_signature();
    let remote_protocol_before = remote_app.context_protocol.test_signature();
    let local_editor_before = local.context_editor_debug_summary();
    let remote_editor_before = remote_app.context_editor_debug_summary();
    let local_actions_before = local.context_editor_actions.clone();
    let remote_actions_before = remote_app.context_editor_actions.clone();
    let local_status_before = local.status_notice();
    let remote_status_before = remote_app.status_notice();
    let local_revision_before = local.context_revision;
    let remote_revision_before = remote_app.context_revision;
    let local_resets_before = local.context_reset_counters;
    let remote_resets_before = remote_app.context_reset_counters;

    let (local_changed, remote_changed) =
        deliver_context_event_pair(&mut local, &mut remote_app, rejected_event);
    assert!(!local_changed, "local mismatched event was accepted: {label}");
    assert!(!remote_changed, "remote mismatched event was accepted: {label}");
    assert_eq!(
        local.context_protocol.test_signature(),
        local_protocol_before,
        "local protocol state changed for rejected {label}"
    );
    assert_eq!(
        remote_app.context_protocol.test_signature(),
        remote_protocol_before,
        "remote protocol state changed for rejected {label}"
    );
    assert_eq!(
        local.context_editor_debug_summary(),
        local_editor_before,
        "local editor state changed for rejected {label}"
    );
    assert_eq!(
        remote_app.context_editor_debug_summary(),
        remote_editor_before,
        "remote editor state changed for rejected {label}"
    );
    assert_eq!(
        local.context_editor_actions, local_actions_before,
        "local follow-up actions changed for rejected {label}"
    );
    assert_eq!(
        remote_app.context_editor_actions, remote_actions_before,
        "remote follow-up actions changed for rejected {label}"
    );
    assert_eq!(
        local.status_notice(),
        local_status_before,
        "local status changed for rejected {label}"
    );
    assert_eq!(
        remote_app.status_notice(),
        remote_status_before,
        "remote status changed for rejected {label}"
    );
    assert_eq!(
        local.context_revision, local_revision_before,
        "local UI revision changed for rejected {label}"
    );
    assert_eq!(
        remote_app.context_revision, remote_revision_before,
        "remote UI revision changed for rejected {label}"
    );
    assert_eq!(
        local.context_reset_counters, local_resets_before,
        "local reset hook ran for rejected {label}"
    );
    assert_eq!(
        remote_app.context_reset_counters, remote_resets_before,
        "remote reset hook ran for rejected {label}"
    );
    assert_context_apps_equal(label, &local, &remote_app);
}

#[test]
fn context_events_reduce_identically_through_local_channel_and_remote_dispatch() {
    for (label, event) in parity_events() {
        let mut local = create_test_app();
        let mut remote_app = create_test_app();
        remote_app.is_remote = true;
        prepare_context_parity_app(&mut local, &event);
        prepare_context_parity_app(&mut remote_app, &event);

        local
            .local_context_event_tx
            .send(event.clone())
            .expect("local context event channel");
        assert!(local.drain_local_context_events(), "local event rejected: {label}");

        let rt = tokio::runtime::Runtime::new().expect("parity runtime");
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        remote_app.handle_server_event(event.clone(), &mut remote);
        assert_context_apps_equal(label, &local, &remote_app);

        local
            .local_context_event_tx
            .send(event.clone())
            .expect("duplicate local context event");
        let _ = local.drain_local_context_events();
        remote_app.handle_server_event(event, &mut remote);
        assert_context_apps_equal(&format!("{label} duplicate"), &local, &remote_app);
        assert_eq!(
            local.context_reset_counters,
            ContextResetCounters::default(),
            "local event reduction ran provider reset orchestration for {label}"
        );
        assert_eq!(
            remote_app.context_reset_counters,
            ContextResetCounters::default(),
            "remote event reduction ran local provider reset orchestration for {label}"
        );
    }
}

#[test]
fn mismatched_context_events_are_ignored_identically_without_side_effects() {
    use crate::protocol::ServerEvent;

    let expected_detail = ServerEvent::ContextMessageDetail {
        id: 2,
        detail: parity_detail(),
    };
    assert_context_event_rejected_identically(
        "wrong request id",
        expected_detail.clone(),
        ServerEvent::ContextMessageDetail {
            id: 2002,
            detail: parity_detail(),
        },
    );

    let expected_snapshot = ServerEvent::ContextEditorSnapshot {
        id: 1,
        snapshot: parity_snapshot(),
    };
    let mut wrong_session_snapshot = parity_snapshot();
    wrong_session_snapshot.session_id = "session-other".to_string();
    assert_context_event_rejected_identically(
        "wrong session",
        expected_snapshot,
        ServerEvent::ContextEditorSnapshot {
            id: 1,
            snapshot: wrong_session_snapshot,
        },
    );

    let mut wrong_revision_detail = parity_detail();
    wrong_revision_detail.context_revision += 1;
    assert_context_event_rejected_identically(
        "wrong revision",
        expected_detail.clone(),
        ServerEvent::ContextMessageDetail {
            id: 2,
            detail: wrong_revision_detail,
        },
    );

    let mut wrong_digest_detail = parity_detail();
    wrong_digest_detail.transcript_digest += 1;
    assert_context_event_rejected_identically(
        "wrong transcript digest",
        expected_detail,
        ServerEvent::ContextMessageDetail {
            id: 2,
            detail: wrong_digest_detail,
        },
    );

    let expected_draft = ServerEvent::ContextDraftReady {
        id: 5,
        draft: Box::new(parity_draft()),
    };
    let mut wrong_draft = parity_draft();
    wrong_draft.identity.draft_id = "draft-other".to_string();
    assert_context_event_rejected_identically_after_setup(
        "wrong draft id",
        expected_draft,
        ServerEvent::ContextDraftReady {
            id: 5,
            draft: Box::new(wrong_draft),
        },
        |app| {
            app.context_protocol
                .begin_draft_monitor(5, "draft-parity".to_string());
        },
    );

    let expected_transaction = ServerEvent::ContextTransactionApplied {
        id: 15,
        draft_id: "draft-parity".to_string(),
        result: parity_transaction_result(
            jcode_session_types::StoredContextTransactionStatusKind::Applied,
        ),
    };
    assert_context_event_rejected_identically(
        "wrong transaction correlation",
        expected_transaction,
        ServerEvent::ContextTransactionApplied {
            id: 15,
            draft_id: "draft-other".to_string(),
            result: parity_transaction_result(
                jcode_session_types::StoredContextTransactionStatusKind::Applied,
            ),
        },
    );

    let expected_rejection = ServerEvent::ContextRequestRejected {
        id: 18,
        request: crate::protocol::ContextRequestKind::MessageDetail,
        draft_id: None,
        transaction_id: None,
        error: crate::protocol::ContextServiceError::Stale("safe rejection".to_string()),
    };
    assert_context_event_rejected_identically(
        "uncorrelated rejection",
        expected_rejection,
        ServerEvent::ContextRequestRejected {
            id: 2018,
            request: crate::protocol::ContextRequestKind::MessageDetail,
            draft_id: None,
            transaction_id: None,
            error: crate::protocol::ContextServiceError::Stale(
                "ignored safe rejection".to_string(),
            ),
        },
    );

    let expected_action = ServerEvent::ContextActionRequired {
        id: 19,
        session_id: "session-parity".to_string(),
        context_revision: 4,
        reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
        required_reduction_tokens: 1_024,
        pending_input: None,
        details: Vec::new(),
        automatic_retry: false,
    };
    assert_context_event_rejected_identically(
        "action required for another session",
        expected_action,
        ServerEvent::ContextActionRequired {
            id: 19,
            session_id: "session-other".to_string(),
            context_revision: 4,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1_024,
            pending_input: None,
            details: Vec::new(),
            automatic_retry: false,
        },
    );
}

fn prepare_transaction_order_pair() -> (App, App) {
    let mut local = create_test_app();
    let mut remote_app = create_test_app();
    remote_app.is_remote = true;
    for app in [&mut local, &mut remote_app] {
        app.remote_session_id = Some("session-parity".to_string());
        app.context_revision = 4;
        app.context_protocol.accepted_session_id = Some("session-parity".to_string());
        app.context_protocol.accepted_context_revision = Some(4);
        app.context_protocol.accepted_transcript_digest = Some(77);
        app.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
        app.context_editor_actions.clear();
        app.context_protocol.begin_snapshot_request(900);
        assert!(app.context_protocol.accept_snapshot(900, parity_snapshot()));
        app.sync_context_editor_from_protocol();
        app.context_editor_actions.clear();
        app.context_protocol.begin_prepare_draft(11);
        app.context_protocol.begin_transaction_request(
            15,
            crate::protocol::ContextRequestKind::ApplyDraft,
            "draft-parity".to_string(),
        );
    }
    (local, remote_app)
}

fn draft_applied_event() -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::ContextDraftApplied {
        id: 11,
        identity: parity_identity(),
        transaction_id: "transaction-parity".to_string(),
        revision: 5,
    }
}

fn transaction_applied_event() -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::ContextTransactionApplied {
        id: 15,
        draft_id: "draft-parity".to_string(),
        result: parity_transaction_result(
            jcode_session_types::StoredContextTransactionStatusKind::Applied,
        ),
    }
}

fn history_refresh_count(app: &App) -> usize {
    app.context_editor_actions
        .iter()
        .filter(|action| {
            matches!(
                action,
                crate::tui::context_editor::ContextEditorAction::LoadHistory {
                    offset: 0,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn transaction_event_ordering_produces_one_revision_transition_and_history_refresh() {
    let (mut local, mut remote_app) = prepare_transaction_order_pair();
    let changed = deliver_context_event_pair(&mut local, &mut remote_app, draft_applied_event());
    assert_eq!(changed, (true, true));
    assert_context_apps_equal("draft then transaction: draft", &local, &remote_app);
    assert_eq!(local.context_revision, 4);
    assert_eq!(history_refresh_count(&local), 1);

    let changed =
        deliver_context_event_pair(&mut local, &mut remote_app, transaction_applied_event());
    assert_eq!(changed, (true, true));
    assert_context_apps_equal("draft then transaction: transaction", &local, &remote_app);
    assert_eq!(local.context_revision, 5);
    assert_eq!(history_refresh_count(&local), 1);

    let state_after_apply = local.context_protocol.test_signature();
    let editor_after_apply = local.context_editor_debug_summary();
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, draft_applied_event()),
        (false, false)
    );
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, transaction_applied_event()),
        (false, false)
    );
    assert_eq!(local.context_revision, 5);
    assert_eq!(history_refresh_count(&local), 1);
    assert_eq!(local.context_protocol.test_signature(), state_after_apply);
    assert_eq!(local.context_editor_debug_summary(), editor_after_apply);
    assert_context_apps_equal("draft then transaction: duplicates", &local, &remote_app);

    let (mut local, mut remote_app) = prepare_transaction_order_pair();
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, transaction_applied_event()),
        (true, true)
    );
    assert_eq!(local.context_revision, 5);
    assert_eq!(history_refresh_count(&local), 1);
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, draft_applied_event()),
        (false, false)
    );
    assert_eq!(local.context_revision, 5);
    assert_eq!(history_refresh_count(&local), 1);
    assert_context_apps_equal("transaction then draft", &local, &remote_app);

    let (mut local, mut remote_app) = prepare_transaction_order_pair();
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, transaction_applied_event()),
        (true, true)
    );
    assert_eq!(local.context_revision, 5);
    assert_eq!(history_refresh_count(&local), 1);
    assert_context_apps_equal("missing draft applied", &local, &remote_app);

    let (mut local, mut remote_app) = prepare_transaction_order_pair();
    for app in [&mut local, &mut remote_app] {
        app.context_protocol
            .begin_draft_monitor(11, "draft-parity".to_string());
        app.context_editor_actions.clear();
    }
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, draft_applied_event()),
        (true, true)
    );
    assert_eq!(local.context_revision, 4);
    assert_eq!(history_refresh_count(&local), 1);
    assert_context_apps_equal("reconnect after draft applied", &local, &remote_app);

    let (mut local, mut remote_app) = prepare_transaction_order_pair();
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, transaction_applied_event()),
        (true, true)
    );
    for app in [&mut local, &mut remote_app] {
        app.context_editor_actions.clear();
        app.context_protocol
            .begin_history_request(100, "session-parity".to_string());
        app.context_protocol
            .begin_history_request(101, "session-parity".to_string());
    }
    let old_page = crate::protocol::ServerEvent::ContextTransactionHistory {
        id: 100,
        context_revision: 5,
        total_transactions: 1,
        offset: 25,
        next_offset: None,
        transactions: vec![parity_transaction_summary()],
    };
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, old_page),
        (false, false)
    );
    let page_zero = crate::protocol::ServerEvent::ContextTransactionHistory {
        id: 101,
        context_revision: 5,
        total_transactions: 1,
        offset: 0,
        next_offset: None,
        transactions: vec![parity_transaction_summary()],
    };
    assert_eq!(
        deliver_context_event_pair(&mut local, &mut remote_app, page_zero),
        (true, true)
    );
    assert_context_apps_equal("page zero beats older history page", &local, &remote_app);
}

fn parity_snapshot_with_selected_message() -> crate::protocol::ContextEditorSnapshot {
    let mut snapshot = parity_snapshot();
    snapshot.raw_message_count = 1;
    snapshot.message_page_end = 1;
    snapshot.messages = vec![crate::protocol::ContextEditorMessage {
        message_id: "message-selected".to_string(),
        stored_index: 0,
        role: crate::message::Role::User,
        display_role: None,
        timestamp: Some(parity_timestamp()),
        raw_provider_tokens: 8,
        projected_provider_tokens: 8,
        preview: "safe selected preview".to_string(),
        blocks: vec![crate::protocol::ContextEditorBlock {
            ordinal: 0,
            kind: jcode_session_types::StoredContextBlockKind::Text,
            semantic_id: None,
            estimated_provider_tokens: 8,
            tool_name: None,
            tool_use_id: None,
            tool_result_is_error: false,
            has_image_payload: false,
            has_tool_thought_signature: false,
            provider_removable_reasoning: false,
            active_operations: Vec::new(),
        }],
        tool_group_ids: Vec::new(),
        summary_coverage: None,
        active_operations: Vec::new(),
        removable_reasoning_kinds: Vec::new(),
    }];
    snapshot
}

fn parity_draft_request() -> crate::protocol::ContextDraftRequest {
    crate::protocol::ContextDraftRequest {
        summary_ranges: Vec::new(),
        reasoning: Some(
            crate::protocol::ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            },
        ),
        tool_results: Vec::new(),
        allow_shadowing_active_operations: false,
        authorization: jcode_session_types::StoredContextAuthorization::Manual {
            initiated_by: None,
        },
    }
}

fn remote_failure_actions() -> Vec<(
    &'static str,
    crate::tui::context_editor::ContextEditorAction,
    crate::protocol::ContextRequestKind,
)> {
    use crate::protocol::{ContextMessageRangeSelection, ContextRequestKind};
    use crate::tui::context_editor::ContextEditorAction;
    vec![
        (
            "snapshot",
            ContextEditorAction::LoadSnapshot {
                page_start: 0,
                page_size: 250,
            },
            ContextRequestKind::Snapshot,
        ),
        (
            "detail",
            ContextEditorAction::LoadDetail {
                context_revision: 4,
                transcript_digest: 77,
                message_id: "message-selected".to_string(),
                block_ordinal: 0,
                start_char: 0,
                max_chars: 1_024,
            },
            ContextRequestKind::MessageDetail,
        ),
        (
            "range preview",
            ContextEditorAction::PreviewRanges {
                context_revision: 4,
                transcript_digest: 77,
                ranges: vec![ContextMessageRangeSelection {
                    start_message_id: "message-selected".to_string(),
                    end_message_id: "message-selected".to_string(),
                }],
            },
            ContextRequestKind::RangeClosurePreview,
        ),
        (
            "prepare draft",
            ContextEditorAction::PrepareDraft(parity_draft_request()),
            ContextRequestKind::PrepareDraft,
        ),
        (
            "cancel draft",
            ContextEditorAction::CancelDraft {
                draft_id: "draft-parity".to_string(),
            },
            ContextRequestKind::CancelDraft,
        ),
        (
            "monitor draft",
            ContextEditorAction::MonitorDraft {
                draft_id: "draft-parity".to_string(),
            },
            ContextRequestKind::DraftStatus,
        ),
        (
            "selection preview",
            ContextEditorAction::PreviewDraftSelection {
                draft_id: "draft-parity".to_string(),
                selected_distillation_ids: vec!["proposal-parity".to_string()],
            },
            ContextRequestKind::DraftSelectionPreview,
        ),
        (
            "apply draft",
            ContextEditorAction::ApplyDraft {
                draft_id: "draft-parity".to_string(),
                selected_distillation_ids: vec!["proposal-parity".to_string()],
            },
            ContextRequestKind::ApplyDraft,
        ),
        (
            "history",
            ContextEditorAction::LoadHistory {
                offset: 0,
                limit: 50,
            },
            ContextRequestKind::TransactionHistory,
        ),
        (
            "transaction detail",
            ContextEditorAction::LoadTransactionDetail {
                context_revision: 4,
                transaction_id: "transaction-parity".to_string(),
            },
            ContextRequestKind::TransactionDetail,
        ),
        (
            "revert",
            ContextEditorAction::RevertTransaction {
                transaction_id: "transaction-parity".to_string(),
            },
            ContextRequestKind::RevertTransaction,
        ),
        (
            "reapply",
            ContextEditorAction::ReapplyTransaction {
                transaction_id: "transaction-parity".to_string(),
            },
            ContextRequestKind::ReapplyTransaction,
        ),
    ]
}

fn pending_request_id_for_kind(
    signature: &serde_json::Value,
    kind: crate::protocol::ContextRequestKind,
) -> Option<u64> {
    use crate::protocol::ContextRequestKind;
    let key = match kind {
        ContextRequestKind::Snapshot => "snapshot_request_id",
        ContextRequestKind::MessageDetail => "detail_request_id",
        ContextRequestKind::RangeClosurePreview => "range_request_id",
        ContextRequestKind::PrepareDraft
        | ContextRequestKind::CancelDraft
        | ContextRequestKind::DraftStatus => "draft_monitor_request_id",
        ContextRequestKind::DraftSelectionPreview => "selection_request_id",
        ContextRequestKind::ApplyDraft
        | ContextRequestKind::RevertTransaction
        | ContextRequestKind::ReapplyTransaction => "transaction_request_id",
        ContextRequestKind::TransactionHistory => "history_request_id",
        ContextRequestKind::TransactionDetail => "transaction_detail_request_id",
        ContextRequestKind::SetEmergencyPolicy => return None,
        ContextRequestKind::LegacyCompact | ContextRequestKind::LegacySetCompactionMode => {
            return None;
        }
    };
    signature.get(key).and_then(serde_json::Value::as_u64)
}

#[test]
fn remote_context_dispatch_correlates_before_write_and_clears_only_failed_request() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::task::Poll;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("remote failure runtime");
    for (label, action, request_kind) in remote_failure_actions() {
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_session_id = Some("session-parity".to_string());
        app.context_protocol.accepted_session_id = Some("session-parity".to_string());
        app.context_protocol.accepted_context_revision = Some(4);
        app.context_protocol.accepted_transcript_digest = Some(77);
        app.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
        app.context_editor_actions.clear();
        app.context_protocol.begin_snapshot_request(900);
        assert!(app
            .context_protocol
            .accept_snapshot(900, parity_snapshot_with_selected_message()));
        app.sync_context_editor_from_protocol();
        app.context_editor_actions.clear();
        assert!(app.handle_context_editor_key(KeyCode::Char(' '), KeyModifiers::NONE));
        let selected_count_before =
            app.context_editor_debug_summary()["selected_messages"].clone();
        assert_eq!(selected_count_before, serde_json::json!(1));

        let unrelated_key = if request_kind == crate::protocol::ContextRequestKind::Snapshot {
            app.context_protocol
                .begin_history_request(9_001, "session-parity".to_string());
            "history_request_id"
        } else {
            app.context_protocol.begin_snapshot_request(9_001);
            "snapshot_request_id"
        };

        let mut remote = runtime.block_on(async {
            crate::tui::backend::RemoteConnection::dummy()
        });
        let peer = remote
            .take_dummy_peer()
            .expect("dummy remote should retain peer stream");
        let prepared = app.prepare_remote_context_editor_action(&mut remote, action);
        let signature = app.context_protocol.test_signature();
        assert_eq!(
            pending_request_id_for_kind(&signature, request_kind),
            Some(prepared.id),
            "exact reducer correlation was not installed before the {label} write"
        );
        assert_eq!(signature[unrelated_key], serde_json::json!(9_001));

        let writer = remote.writer();
        runtime.block_on(async {
            let guard = writer.lock().await;
            let mut send = Box::pin(app.send_prepared_remote_context_request(&remote, prepared));
            assert!(
                matches!(futures::poll!(send.as_mut()), Poll::Pending),
                "{label} write did not block behind the owned writer lock"
            );
            drop(peer);
            drop(guard);
            assert!(!send.as_mut().await, "{label} write unexpectedly succeeded");
            drop(send);
        });

        let signature = app.context_protocol.test_signature();
        assert_eq!(
            pending_request_id_for_kind(&signature, request_kind),
            None,
            "failed {label} write retained its exact pending correlation"
        );
        assert_eq!(
            signature[unrelated_key],
            serde_json::json!(9_001),
            "failed {label} write cleared an unrelated pending request"
        );
        assert_eq!(
            app.context_editor_debug_summary()["selected_messages"],
            selected_count_before,
            "failed {label} write discarded staged stable selections"
        );
        assert_eq!(app.context_revision, 0, "failed {label} write bumped UI revision");
        assert_eq!(
            app.context_reset_counters,
            ContextResetCounters::default(),
            "failed {label} write ran local provider reset orchestration"
        );
    }
}

#[test]
fn remote_context_commands_send_only_typed_context_requests() {
    use crate::protocol::Request;
    use crossterm::event::{KeyCode, KeyModifiers};
    use tokio::io::AsyncBufReadExt;

    let cases = [
        ("/compact", "edit", true),
        ("/context edit", "edit", true),
        ("/context history", "history", false),
        ("/context restore", "restore", false),
        ("/context undo", "undo_latest", false),
    ];

    for (command, expected_mode, expects_snapshot) in cases {
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_session_id = Some("session-parity".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("remote context command runtime");
        let (mut remote, peer) = runtime.block_on(async {
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let peer = remote.take_dummy_peer().expect("dummy peer");
            (remote, peer)
        });

        app.input = command.to_string();
        runtime
            .block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote))
            .expect("remote context command");
        assert_eq!(
            app.context_editor_debug_summary()["open_mode"],
            serde_json::json!(expected_mode),
            "wrong remote editor mode for {command}"
        );

        let request: Request = runtime.block_on(async {
            app.dispatch_remote_context_editor_actions(&mut remote).await;
            let mut reader = tokio::io::BufReader::new(peer);
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).await.expect("request line");
            assert!(bytes > 0, "{command} sent no context request");
            serde_json::from_str(&line).expect("typed context request")
        });

        match request {
            Request::GetContextEditorSnapshot {
                page_start,
                page_size,
                ..
            } if expects_snapshot => {
                assert_eq!(page_start, 0);
                assert_eq!(page_size, Some(250));
            }
            Request::ListContextTransactions { offset, limit, .. } if !expects_snapshot => {
                assert_eq!(offset, 0);
                assert_eq!(limit, Some(250));
            }
            other => panic!("{command} sent obsolete or incorrect request: {other:?}"),
        }
    }
}

#[test]
fn remote_compact_mode_commands_send_nothing_and_do_not_change_state() {
    use crossterm::event::{KeyCode, KeyModifiers};

    for command in ["/compact mode", "/compact mode status", "/compact mode semantic"] {
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_session_id = Some("session-parity".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("remote obsolete-command runtime");
        let (mut remote, peer) = runtime.block_on(async {
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let peer = remote.take_dummy_peer().expect("dummy peer");
            (remote, peer)
        });

        app.input = command.to_string();
        runtime
            .block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote))
            .expect("obsolete remote compact command");

        assert!(app.context_editor_overlay.is_none());
        assert!(app.context_editor_actions.is_empty());
        assert!(app.context_protocol.test_signature()["snapshot_request_id"].is_null());
        assert!(app.context_protocol.test_signature()["history_request_id"].is_null());
        let last = app.display_messages().last().expect("migration response");
        assert_eq!(
            last.content,
            "Compaction modes are obsolete. Use /compact or /context edit to review and apply one explicit context transaction."
        );

        let mut byte = [0_u8; 1];
        let error = peer
            .try_read(&mut byte)
            .expect_err("obsolete compact command unexpectedly wrote a protocol request");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }
}

#[test]
fn remote_context_report_uses_authoritative_protocol_metadata_without_fake_zeroes() {
    use crossterm::event::{KeyCode, KeyModifiers};

    fn submit_remote_context_report(app: &mut App) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("remote context report runtime");
        let mut remote = runtime.block_on(async {
            crate::tui::backend::RemoteConnection::dummy()
        });
        app.input = "/context".to_string();
        runtime
            .block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote))
            .expect("remote context report");
    }

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session-parity".to_string());
    submit_remote_context_report(&mut app);
    let unloaded = &app.display_messages().last().expect("unloaded report").content;
    assert!(unloaded.contains("- revision: not loaded"));
    assert!(unloaded.contains("- transactions: not loaded total, not loaded active"));
    assert!(unloaded.contains("- authoritative stored messages: not loaded"));
    assert!(!unloaded.contains("- revision: 0\n- transactions: 0 total, 0 active"));

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session-parity".to_string());
    let mut snapshot = parity_snapshot_with_selected_message();
    snapshot.active_transactions = vec![parity_transaction_summary()];
    app.context_protocol.begin_snapshot_request(31);
    assert!(app.context_protocol.accept_snapshot(31, snapshot));
    app.context_protocol
        .begin_history_request(32, "session-parity".to_string());
    assert!(app.context_protocol.accept_transaction_history(
        32,
        4,
        3,
        0,
        None,
        vec![parity_transaction_summary()],
    ));
    submit_remote_context_report(&mut app);
    let loaded = &app.display_messages().last().expect("loaded report").content;
    assert!(loaded.contains("- revision: 4"));
    assert!(loaded.contains("- transactions: 3 total, 1 active"));
    assert!(loaded.contains("- authoritative stored messages: 1"));
    assert!(loaded.contains("source: authoritative remote context protocol metadata"));
}
