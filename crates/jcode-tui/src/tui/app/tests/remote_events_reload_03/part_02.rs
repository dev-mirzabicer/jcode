#[test]
fn test_metadata_only_history_preserves_fast_restored_startup_state() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("create temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    let session_id = "session_fast_resume_meta_42";
    let mut session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("resume me".to_string()),
    );
    session.model = Some("gpt-5.4".to_string());
    session.append_stored_message(crate::session::StoredMessage {
        id: "msg-fast-resume".to_string(),
        role: crate::message::Role::Assistant,
        content: vec![crate::message::ContentBlock::Text {
            text: "restored locally before connect".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });
    session.save().expect("save fast resume session");

    let mut app = App::new_for_remote(Some(session_id.to_string()));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard_rt = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: session_id.to_string(),
            messages: vec![],
            images: vec![],
            provider_name: Some("openai".to_string()),
            provider_model: Some("gpt-5.4".to_string()),
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec![],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec![session_id.to_string()],
            client_count: Some(1),
            is_canary: Some(false),
            server_version: None,
            server_name: None,
            server_icon: None,
            server_has_update: None,
            was_interrupted: None,
            reload_recovery: None,
            connection_type: Some("https".to_string()),
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            context_revision: 0,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
            startup_context: None,
        },
        &mut remote,
    );

    let assistant_messages: Vec<_> = app
        .display_messages()
        .iter()
        .filter(|m| m.role == "assistant")
        .collect();
    assert_eq!(assistant_messages.len(), 1);
    assert_eq!(
        assistant_messages[0].content,
        "restored locally before connect"
    );
    assert_eq!(app.remote_session_id.as_deref(), Some(session_id));
    assert_eq!(app.connection_type.as_deref(), Some("https"));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_duplicate_history_for_same_session_is_ignored_after_fast_path_restore() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.remote_session_id = Some("ses_fast_path".to_string());
    app.context_revision = 7;
    app.context_protocol.accept_history("ses_fast_path", 7);
    app.push_display_message(DisplayMessage::assistant(
        "local restored state".to_string(),
    ));
    remote.mark_history_loaded();

    app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "ses_fast_path".to_string(),
            messages: vec![crate::protocol::HistoryMessage {
                role: "assistant".to_string(),
                content: "server history replay".to_string(),
                tool_calls: None,
                tool_data: None,
            }],
            images: vec![],
            provider_name: Some("claude".to_string()),
            provider_model: Some("claude-sonnet-4-20250514".to_string()),
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec![],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec![],
            client_count: None,
            is_canary: None,
            reload_recovery: None,
            server_version: None,
            server_name: None,
            server_icon: None,
            server_has_update: None,
            was_interrupted: Some(true),
            connection_type: Some("websocket".to_string()),
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            context_revision: 0,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
            startup_context: None,
        },
        &mut remote,
    );

    let assistant_messages: Vec<_> = app
        .display_messages()
        .iter()
        .filter(|m| m.role == "assistant")
        .collect();
    assert_eq!(assistant_messages.len(), 1);
    assert_eq!(assistant_messages[0].content, "local restored state");
    assert_eq!(app.connection_type.as_deref(), Some("websocket"));
    assert!(app.context_revision >= 7);
    assert_eq!(app.context_protocol.accepted_context_revision, Some(7));
    assert!(app.queued_messages().is_empty());
    assert_eq!(app.hidden_queued_system_messages.len(), 1);
    assert!(app.hidden_queued_system_messages[0].contains("interrupted by a server reload"));
    assert!(
        app.display_messages()
            .iter()
            .any(|m| m.role == "system" && m.content.starts_with("Reload complete - continuing"))
    );
}

#[test]
fn test_compacted_history_marker_scroll_queues_lazy_load() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.replace_display_messages(vec![DisplayMessage::system(
        "Earlier conversation compacted - 128 historical messages hidden from the UI. Scroll to the top to load older history.",
    )]);

    let state = app.compacted_history_lazy_state();
    assert_eq!(state.total_messages, 128);
    assert_eq!(state.visible_messages, 0);
    assert_eq!(state.remaining_messages, 128);

    app.auto_scroll_paused = true;
    app.scroll_offset = 5;
    app.scroll_up(5);

    assert_eq!(app.scroll_offset, 0);
    assert_eq!(app.take_pending_compacted_history_load(), Some(64));
}

#[test]
fn test_local_compacted_history_marker_scroll_expands_from_session() {
    // Truncation only applies to genuinely large compacted prefixes: at least
    // 80 renderable messages AND more than 5 user turns (smaller histories are
    // always shown whole). Build 7 turns x 14 messages = 98 compacted
    // messages so the lazy-load path actually engages.
    let mut app = create_test_app();
    const TURNS: usize = 7;
    const MESSAGES_PER_TURN: usize = 14;
    for turn in 0..TURNS {
        app.session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: format!("old prompt {turn}"),
                cache_control: None,
            }],
        );
        for reply in 0..(MESSAGES_PER_TURN - 1) {
            app.session.add_message(
                crate::message::Role::Assistant,
                vec![crate::message::ContentBlock::Text {
                    text: format!("old response {turn}-{reply}"),
                    cache_control: None,
                }],
            );
        }
    }
    let compacted_count = app.session.messages.len();
    app.session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );
    app.session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old prompts and responses".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: TURNS,
        original_turn_count: TURNS,
        compacted_count,
    });

    let (rendered_messages, _images, _compacted_info) =
        crate::session::render_messages_and_images_with_compacted_history(&app.session, 0);
    let rendered = rendered_messages
        .into_iter()
        .map(|msg| DisplayMessage {
            role: msg.role,
            content: msg.content,
            tool_calls: msg.tool_calls,
            duration_secs: None,
            title: None,
            tool_data: msg.tool_data,
        })
        .collect();
    app.replace_display_messages(rendered);
    // total/remaining count *renderable* messages; the test session may carry
    // non-renderable bootstrap entries, so use the parsed marker as truth.
    let total = app.compacted_history_lazy_state().total_messages;
    assert!(
        total >= TURNS * MESSAGES_PER_TURN,
        "all added messages should be renderable, got total {total}"
    );
    assert_eq!(app.compacted_history_lazy_state().visible_messages, 0);
    assert_eq!(
        app.compacted_history_lazy_state().remaining_messages,
        total,
        "requesting 0 visible should hide the whole compacted prefix"
    );

    app.auto_scroll_paused = true;
    app.scroll_offset = 0;
    app.scroll_up(1);

    // Local sessions expand in place (no remote round-trip).
    assert_eq!(app.take_pending_compacted_history_load(), None);
    let state = app.compacted_history_lazy_state();
    assert!(
        state.visible_messages >= 64,
        "one chunk (turn-snapped) should be visible, got {}",
        state.visible_messages
    );
    assert_eq!(state.remaining_messages, total - state.visible_messages);
    // The newest old turn is in the visible window; the oldest is still hidden.
    assert!(
        app.display_messages()
            .iter()
            .any(|message| message.content == "old response 6-0")
    );
    assert!(
        !app.display_messages()
            .iter()
            .any(|message| message.content == "old prompt 0")
    );
}

#[test]
fn test_compacted_history_event_applies_expanded_window() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_lazy_history".to_string());
    app.push_display_message(DisplayMessage::assistant("existing tail"));
    app.scroll_offset = 12;
    app.auto_scroll_paused = false;

    let needs_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::CompactedHistory {
            id: 8,
            session_id: "session_lazy_history".to_string(),
            messages: vec![
                crate::protocol::HistoryMessage {
                    role: "system".to_string(),
                    content: "Earlier conversation compacted - 64 older historical messages hidden. Showing 64 of 128 compacted messages. Scroll to the top to load more.".to_string(),
                    tool_calls: None,
                    tool_data: None,
                },
                crate::protocol::HistoryMessage {
                    role: "assistant".to_string(),
                    content: "older response".to_string(),
                    tool_calls: None,
                    tool_data: None,
                },
                crate::protocol::HistoryMessage {
                    role: "user".to_string(),
                    content: "current prompt".to_string(),
                    tool_calls: None,
                    tool_data: None,
                },
            ],
            images: vec![],
            compacted_total: 128,
            compacted_visible: 64,
            compacted_remaining: 64,
            compacted_hidden_prompts: 0,
        },
        &mut remote,
    );

    assert!(needs_redraw);
    assert_eq!(app.display_messages().len(), 3);
    assert_eq!(app.display_messages()[1].content, "older response");
    assert_eq!(app.display_messages()[2].content, "current prompt");
    assert!(app.auto_scroll_paused);
    assert_eq!(app.scroll_offset, 0);
    let state = app.compacted_history_lazy_state();
    assert_eq!(state.total_messages, 128);
    assert_eq!(state.visible_messages, 64);
    assert_eq!(state.remaining_messages, 64);
}

#[test]
fn test_remote_error_with_retry_after_keeps_pending_for_auto_retry() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "retry me".to_string(),
        images: vec![],
        is_system: false,
        system_reminder: None,
        auto_retry: false,
        retry_attempts: 0,
        retry_at: None,
    });
    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(9);

    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 9,
            message: "rate limited".to_string(),
            retry_after_secs: Some(3),
        },
        &mut remote,
    );

    assert!(!app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Idle));
    assert!(app.current_message_id.is_none());
    assert!(app.rate_limit_reset.is_some());
    assert!(app.rate_limit_pending_message.is_some());

    let last = app
        .display_messages()
        .last()
        .expect("missing rate-limit status message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("Will auto-retry in 3 seconds"));
}

#[test]
fn context_protocol_events_reduce_with_exact_correlation_and_prompt_safe_action_handling() {
    use chrono::{Duration, TimeZone, Utc};
    use jcode_provider_core::{
        ContextProjectionValidationReport, ContextProjectionValidationStatus, ContextProviderFamily,
    };
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextEconomics, StoredContextTransactionStatusKind,
    };

    fn timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, 18, 0, 0)
            .single()
            .expect("valid timestamp")
    }

    fn economics() -> StoredContextEconomics {
        StoredContextEconomics {
            projected_tokens_before: 100,
            projected_tokens_after: 50,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            unchanged_prefix_items: 0,
            earliest_changed_provider_item: Some(0),
            old_affected_suffix_tokens: 100,
            new_affected_suffix_tokens: 50,
            deleted_input_tokens: 50,
            context_window: Some(1_000),
            safe_input_budget: Some(950),
            pricing: None,
            first_request_delta_usd: None,
            recurring_savings_per_turn_usd: None,
            break_even_turns: None,
            assumptions: Vec::new(),
        }
    }

    fn identity() -> crate::protocol::ContextDraftIdentity {
        crate::protocol::ContextDraftIdentity {
            draft_id: "draft-app-1".to_string(),
            session_id: "session-app-context".to_string(),
            base_context_revision: 4,
            raw_message_count: 1,
            transcript_digest: 99,
            provider_name: "openai".to_string(),
            model: "gpt-test".to_string(),
            route: "oauth".to_string(),
            created_at: timestamp(),
            expires_at: timestamp() + Duration::minutes(30),
        }
    }

    fn draft() -> crate::protocol::ContextDraft {
        crate::protocol::ContextDraft {
            identity: identity(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            required_operations: Vec::new(),
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview: crate::protocol::ContextDraftPreview {
                raw_stored_message_count: 1,
                current_context_revision: 4,
                proposed_context_revision: 5,
                economics: economics(),
                validation: ContextProjectionValidationReport {
                    provider_family: ContextProviderFamily::OpenAiResponses,
                    provider_name: "openai".to_string(),
                    provider_display_name: "OpenAI".to_string(),
                    model: "gpt-test".to_string(),
                    evidence_tag: "app-context-event-fixture-v1".to_string(),
                    builder_status: ContextProjectionValidationStatus::Supported,
                    normalized_item_count: 1,
                    formatter_placeholder_count: 0,
                    normalization_notes: Vec::new(),
                    findings: Vec::new(),
                },
                formatter_placeholder_count: 0,
                operation_previews: Vec::new(),
                notices: Vec::new(),
            },
            curator_usage: Vec::new(),
        }
    }

    fn transaction_summary() -> crate::protocol::ContextTransactionSummary {
        crate::protocol::ContextTransactionSummary {
            id: "transaction-app-1".to_string(),
            created_at: timestamp(),
            base_revision: 4,
            active: true,
            latest_status: Some(StoredContextTransactionStatusKind::Applied),
            latest_status_revision: Some(5),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operation_counts: crate::protocol::ContextOperationCounts::default(),
            application: None,
            economics: Some(economics()),
        }
    }

    fn transaction_result() -> crate::protocol::ContextTransactionResult {
        crate::protocol::ContextTransactionResult {
            transaction: transaction_summary(),
            revision: 5,
            status: StoredContextTransactionStatusKind::Applied,
            warnings: Vec::new(),
        }
    }

    fn snapshot(request_session: &str) -> crate::protocol::ContextEditorSnapshot {
        crate::protocol::ContextEditorSnapshot {
            session_id: request_session.to_string(),
            context_revision: 4,
            raw_message_count: 1,
            transcript_digest: 99,
            processing: false,
            provider_name: "openai".to_string(),
            provider_display_name: "OpenAI".to_string(),
            model: "gpt-test".to_string(),
            route: "oauth".to_string(),
            context_window: 1_000,
            projected_request_tokens: 100,
            message_page_start: 0,
            message_page_end: 1,
            next_message_page_start: None,
            messages: vec![crate::protocol::ContextEditorMessage {
                message_id: "message-app-1".to_string(),
                stored_index: 0,
                role: crate::message::Role::User,
                display_role: None,
                timestamp: Some(timestamp()),
                raw_provider_tokens: 4,
                projected_provider_tokens: 4,
                preview: "hello".to_string(),
                blocks: Vec::new(),
                tool_group_ids: Vec::new(),
                summary_coverage: None,
                active_operations: Vec::new(),
                removable_reasoning_kinds: Vec::new(),
                active_agent_profile: false,
            }],
            active_transactions: Vec::new(),
            emergency_policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
            curator_route: None,
            curator_unavailable_reason: None,
            curator_default: Default::default(),
            curator_route_options: Vec::new(),
        }
    }

    fn detail() -> crate::protocol::ContextMessageDetail {
        crate::protocol::ContextMessageDetail {
            session_id: "session-app-context".to_string(),
            context_revision: 4,
            transcript_digest: 99,
            message_id: "message-app-1".to_string(),
            stored_index: 0,
            role: crate::message::Role::User,
            display_role: None,
            timestamp: Some(timestamp()),
            block_ordinal: 0,
            block_kind: jcode_session_types::StoredContextBlockKind::Text,
            format: crate::protocol::ContextMessageDetailFormat::Text,
            content: crate::protocol::ContextTextChunk {
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

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    app.remote_session_id = Some("session-app-context".to_string());
    app.context_protocol
        .accept_history("session-app-context", 4);

    app.context_protocol.begin_snapshot_request(10);
    app.context_protocol.begin_snapshot_request(11);
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextEditorSnapshot {
            id: 10,
            snapshot: snapshot("session-app-context"),
        },
        &mut remote,
    );
    assert!(app.context_protocol.snapshot.is_none());
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextEditorSnapshot {
            id: 11,
            snapshot: snapshot("session-app-context"),
        },
        &mut remote,
    );
    assert_eq!(
        app.context_protocol
            .snapshot
            .as_ref()
            .map(|value| value.transcript_digest),
        Some(99)
    );

    app.context_protocol.begin_detail_request(
        12,
        "session-app-context".to_string(),
        4,
        99,
        "message-app-1".to_string(),
        0,
    );
    let mut stale_detail = detail();
    stale_detail.transcript_digest = 100;
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextMessageDetail {
            id: 12,
            detail: stale_detail,
        },
        &mut remote,
    );
    assert!(app.context_protocol.detail.is_none());
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextMessageDetail {
            id: 12,
            detail: detail(),
        },
        &mut remote,
    );
    assert!(app.context_protocol.detail.is_some());

    app.context_protocol.begin_prepare_draft(20);
    let progress = crate::protocol::ContextDraftProgress {
        phase: crate::protocol::ContextDraftPhase::PreparingArtifacts,
        completed_items: 1,
        total_items: 2,
    };
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextDraftProgress {
            id: 20,
            draft_id: "draft-app-1".to_string(),
            progress: progress.clone(),
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextDraftProgress {
            id: 20,
            draft_id: "wrong-draft".to_string(),
            progress: progress.clone(),
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextDraftReady {
            id: 20,
            draft: Box::new(draft()),
        },
        &mut remote,
    );
    assert!(matches!(
        app.context_protocol.draft,
        Some(context_protocol::ContextClientDraftState::Ready(_))
    ));
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextDraftProgress {
            id: 20,
            draft_id: "draft-app-1".to_string(),
            progress,
        },
        &mut remote,
    );
    assert!(matches!(
        app.context_protocol.draft,
        Some(context_protocol::ContextClientDraftState::Ready(_))
    ));

    app.context_protocol
        .begin_draft_monitor(21, "draft-app-1".to_string());
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextDraftReady {
            id: 21,
            draft: Box::new(draft()),
        },
        &mut remote,
    );
    assert!(matches!(
        app.context_protocol.draft,
        Some(context_protocol::ContextClientDraftState::Ready(_))
    ));

    let app_context_revision_before = app.context_revision;
    app.context_protocol.begin_transaction_request(
        30,
        crate::protocol::ContextRequestKind::ApplyDraft,
        "draft-app-1".to_string(),
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextTransactionApplied {
            id: 30,
            draft_id: "draft-app-1".to_string(),
            result: transaction_result(),
        },
        &mut remote,
    );
    assert_eq!(app.context_protocol.accepted_context_revision, Some(5));
    assert!(app.context_protocol.snapshot.is_none());
    assert!(app.context_protocol.detail.is_none());
    assert!(app.context_protocol.draft.is_none());
    assert!(app.context_revision > app_context_revision_before);

    app.context_protocol
        .begin_history_request(31, "session-app-context".to_string());
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextTransactionHistory {
            id: 31,
            context_revision: 5,
            total_transactions: 1,
            offset: 0,
            next_offset: None,
            transactions: vec![transaction_summary()],
        },
        &mut remote,
    );
    assert_eq!(
        app.context_protocol
            .history
            .as_ref()
            .map(|history| history.total_transactions),
        Some(1)
    );

    app.handle_server_event(
        crate::protocol::ServerEvent::ContextEmergencyPolicyChanged {
            id: 32,
            session_id: "session-app-context".to_string(),
            policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
        },
        &mut remote,
    );
    assert!(matches!(
        app.context_protocol.emergency_policy,
        Some(jcode_session_types::StoredContextEmergencyPolicy::Block)
    ));

    app.context_protocol.begin_snapshot_request(33);
    let mut updated_snapshot = snapshot("session-app-context");
    updated_snapshot.context_revision = 5;
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextEditorSnapshot {
            id: 33,
            snapshot: updated_snapshot,
        },
        &mut remote,
    );
    app.context_protocol
        .begin_history_request(34, "session-app-context".to_string());
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextRequestRejected {
            id: 34,
            request: crate::protocol::ContextRequestKind::TransactionHistory,
            draft_id: None,
            transaction_id: None,
            error: crate::protocol::ContextServiceError::Runtime("test rejection".to_string()),
        },
        &mut remote,
    );
    assert!(app.context_protocol.snapshot.is_some());
    assert!(app.context_protocol.last_rejection.is_some());

    app.input = "composer must remain unchanged".to_string();
    let input_before = app.input.clone();
    let display_count_before = app.display_messages().len();
    let queued_count_before = app.queued_messages().len();
    app.handle_server_event(
        crate::protocol::ServerEvent::ContextActionRequired {
            id: 35,
            session_id: "session-app-context".to_string(),
            context_revision: 5,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1_024,
            pending_input: Some(crate::protocol::ContextPendingInputMetadata {
                request_id: 900,
                content_chars: input_before.chars().count(),
                content_digest: 123,
                content_sha256: String::new(),
                image_count: 0,
            }),
            preflight: None,
            payload: None,
            details: vec!["metadata only".to_string()],
            automatic_retry: false,
        },
        &mut remote,
    );
    assert_eq!(app.input, input_before);
    assert_eq!(app.display_messages().len(), display_count_before);
    assert_eq!(app.queued_messages().len(), queued_count_before);
    assert!(app.context_protocol.action_required.is_none());

    let session_change = crate::protocol::ServerEvent::History {
        id: 36,
        session_id: "session-app-context-2".to_string(),
        messages: Vec::new(),
        images: Vec::new(),
        provider_name: Some("openai".to_string()),
        provider_model: Some("gpt-test".to_string()),
        subagent_model: None,
        autoreview_enabled: None,
        autojudge_enabled: None,
        available_models: Vec::new(),
        available_model_routes: Vec::new(),
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        total_tokens: None,
        token_usage_totals: None,
        all_sessions: Vec::new(),
        client_count: None,
        is_canary: None,
        reload_recovery: None,
        server_version: None,
        server_name: None,
        server_icon: None,
        server_has_update: None,
        was_interrupted: None,
        connection_type: None,
        status_detail: None,
        upstream_provider: None,
        resolved_credential: None,
        reasoning_effort: None,
        service_tier: None,
        context_revision: 8,
        activity: None,
        side_panel: crate::side_panel::SidePanelSnapshot::default(),
            startup_context: None,
    };
    app.handle_server_event(session_change, &mut remote);
    assert_eq!(
        app.context_protocol.accepted_session_id.as_deref(),
        Some("session-app-context-2")
    );
    assert_eq!(app.context_protocol.accepted_context_revision, Some(8));
    assert!(app.context_protocol.snapshot.is_none());
    assert!(app.context_protocol.last_rejection.is_none());
    assert!(app.context_protocol.action_required.is_none());
}
