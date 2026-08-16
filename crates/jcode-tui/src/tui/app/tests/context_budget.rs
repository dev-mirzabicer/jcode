fn context_budget_stats_for_app(app: &App) -> crate::context_budget::ContextBudgetStats {
    let context_budget = app.registry.context_budget();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async { context_budget.read().await.stats() })
}

fn set_app_context_observation(app: &App, tokens: u64) {
    let context_budget = app.registry.context_budget();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async { context_budget.write().await.update_observed_input_tokens(tokens) });
}

fn estimated_app_message_tokens(messages: &[Message]) -> usize {
    let mut tracker = crate::context_budget::ContextBudgetTracker::new().with_budget(1);
    tracker.seed_messages(messages);
    tracker.estimated_message_tokens()
}

#[derive(Clone)]
struct ContextBudgetTestProvider {
    window: StdArc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Provider for ContextBudgetTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("ContextBudgetTestProvider")
    }

    fn name(&self) -> &str {
        "context-budget-test"
    }

    fn context_window(&self) -> usize {
        self.window.load(Ordering::SeqCst)
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_context_budget_test_app(window: StdArc<AtomicUsize>) -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(ContextBudgetTestProvider { window });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

#[test]
fn context_budget_local_replacement_append_and_clear_are_exact() {
    let mut app = create_test_app();
    app.context_limit = 10_000;
    let initial = vec![Message::user("first"), Message::assistant_text("second")];
    app.replace_provider_messages(initial.clone());

    let seeded = context_budget_stats_for_app(&app);
    assert_eq!(seeded.token_budget, 10_000);
    assert_eq!(seeded.message_count, initial.len());
    assert_eq!(
        seeded.estimated_message_tokens,
        estimated_app_message_tokens(&initial)
    );

    let appended = Message::user("third message");
    let append_tokens = estimated_app_message_tokens(std::slice::from_ref(&appended));
    app.add_provider_message(appended);
    let after_append = context_budget_stats_for_app(&app);
    assert_eq!(after_append.message_count, seeded.message_count + 1);
    assert_eq!(
        after_append.estimated_message_tokens,
        seeded.estimated_message_tokens.saturating_add(append_tokens)
    );

    set_app_context_observation(&app, 9_000);
    app.session.replace_messages(Vec::new());
    app.clear_provider_messages();
    let cleared = context_budget_stats_for_app(&app);
    assert_eq!(cleared.message_count, 0);
    assert_eq!(cleared.estimated_message_chars, 0);
    assert_eq!(cleared.estimated_message_tokens, 0);
    assert_eq!(cleared.observed_input_tokens, None);
}

#[test]
fn context_budget_remote_mode_keeps_only_server_observation_and_resets_on_history_replacement() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.context_limit = 20_000;
    app.replace_provider_messages(vec![Message::user("client history")]);

    let initial = context_budget_stats_for_app(&app);
    assert_eq!(initial.message_count, 0);
    assert_eq!(initial.estimated_message_tokens, 0);
    assert_eq!(initial.observed_input_tokens, None);

    app.streaming.streaming_input_tokens = 7_000;
    app.streaming.streaming_cache_read_tokens = None;
    app.streaming.streaming_cache_creation_tokens = None;
    app.update_context_usage_from_stream();
    app.add_provider_message(Message::assistant_text("remote append"));
    let observed = context_budget_stats_for_app(&app);
    assert_eq!(observed.message_count, 0);
    assert_eq!(observed.observed_input_tokens, Some(7_000));

    app.replace_provider_messages(vec![Message::user("new remote history")]);
    let replaced = context_budget_stats_for_app(&app);
    assert_eq!(replaced.message_count, 0);
    assert_eq!(replaced.observed_input_tokens, None);

    app.input = "/context".to_string();
    app.submit_input();
    let report = &app.display_messages().last().expect("context report").content;
    assert!(report.contains("remote server projection is not exposed to this client"));
    assert!(report.contains("latest server-observed context tokens: n/a"));
}

#[test]
fn context_budget_recover_session_without_tools_reseeds_exactly() {
    let mut app = create_test_app();
    app.context_limit = 50_000;
    app.session.replace_messages(Vec::new());
    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "retained user text".to_string(),
            cache_control: None,
        }],
    );
    app.session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Text {
                text: "retained assistant text".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "recovery-tool".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: None,
            },
        ],
    );
    app.session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "recovery-tool".to_string(),
            content: "discarded tool output".to_string(),
            is_error: Some(false),
        }],
    );
    app.replace_provider_messages(app.session.raw_messages_for_provider_uncached());
    set_app_context_observation(&app, 49_000);

    app.recover_session_without_tools();

    let recovered = app.materialized_provider_messages();
    let stats = context_budget_stats_for_app(&app);
    assert_eq!(recovered.len(), 2);
    assert_eq!(stats.token_budget, 50_000);
    assert_eq!(stats.observed_input_tokens, None);
    assert_eq!(stats.message_count, recovered.len());
    assert_eq!(
        stats.estimated_message_tokens,
        estimated_app_message_tokens(&recovered)
    );
    assert!(app.session.messages.iter().all(|message| {
        message
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::Text { .. }))
    }));
}

#[test]
fn context_budget_model_switch_updates_budget_clears_observation_and_preserves_messages() {
    let window = StdArc::new(AtomicUsize::new(10_000));
    let mut app = create_context_budget_test_app(window.clone());
    let messages = vec![Message::user("stable provider history")];
    app.replace_provider_messages(messages.clone());
    set_app_context_observation(&app, 9_000);
    {
        let legacy_compaction = app.registry.legacy_compaction();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            legacy_compaction
                .write()
                .await
                .update_observed_input_tokens(9_000);
        });
    }
    let messages_before = serde_json::to_vec(&app.messages).unwrap();
    let session_before = serde_json::to_vec(&app.session.messages).unwrap();

    window.store(50_000, Ordering::SeqCst);
    app.update_context_limit_for_model("large");

    let stats = context_budget_stats_for_app(&app);
    assert_eq!(stats.token_budget, 50_000);
    assert_eq!(stats.observed_input_tokens, None);
    let provider_messages = app.materialized_provider_messages();
    let legacy_observation = {
        let legacy_compaction = app.registry.legacy_compaction();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            legacy_compaction
                .read()
                .await
                .stats_with(&provider_messages)
                .observed_input_tokens
        })
    };
    assert_eq!(legacy_observation, None);
    assert_eq!(serde_json::to_vec(&app.messages).unwrap(), messages_before);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).unwrap(),
        session_before
    );
}

fn phase10_pressure_report(
    context_revision: u64,
    context_window: usize,
    projected_input_tokens: usize,
) -> crate::protocol::ContextPreflightReport {
    crate::context::evaluate_context_preflight(
        context_revision,
        jcode_provider_core::ContextRequestBudget::unknown(context_window),
        crate::protocol::ContextRequestTokenBreakdown {
            system_tokens: 0,
            tool_definition_tokens: 0,
            historical_message_tokens: projected_input_tokens,
            pending_input_tokens: 0,
            memory_tokens: 0,
        },
    )
}

fn phase10_render_text(app: &App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn phase10_display_snapshot(app: &App) -> Vec<(String, String)> {
    app.display_messages
        .iter()
        .map(|message| (message.role.clone(), message.content.clone()))
        .collect()
}

#[test]
fn phase10_model_switch_recalculates_pressure_without_mutating_messages() {
    let window = StdArc::new(AtomicUsize::new(10_000));
    let mut app = create_context_budget_test_app(window.clone());
    let raw_before = serde_json::to_vec(&app.session.messages).unwrap();
    let display_before = phase10_display_snapshot(&app);
    let report = phase10_pressure_report(app.session.context_view.revision, 10_000, 9_400);
    assert_eq!(report.pressure, crate::protocol::ContextPressureLevel::Urgent);
    app.set_local_context_pressure(report);

    window.store(50_000, Ordering::SeqCst);
    app.update_context_limit_for_model("larger");

    let recalculated = app.context_pressure.as_ref().expect("pressure report");
    assert_eq!(recalculated.context_window, 50_000);
    assert_eq!(recalculated.pressure, crate::protocol::ContextPressureLevel::Normal);
    assert_eq!(serde_json::to_vec(&app.session.messages).unwrap(), raw_before);
    assert_eq!(phase10_display_snapshot(&app), display_before);
}

#[test]
fn phase10_pressure_banner_is_non_transcript_and_mouse_opens_editor_without_touching_composer() {
    let mut app = create_test_app();
    app.input = "composer draft".to_string();
    app.cursor_pos = app.input.len();
    app.pasted_contents = vec!["pasted backing value".to_string()];
    app.pending_images = vec![("image/png".to_string(), "image-data".to_string())];
    let raw_before = serde_json::to_vec(&app.session.messages).unwrap();
    let display_before = phase10_display_snapshot(&app);
    let input_before = app.input.clone();
    let pasted_before = app.pasted_contents.clone();
    let images_before = app.pending_images.clone();
    app.set_local_context_pressure(phase10_pressure_report(
        app.session.context_view.revision,
        100_000,
        80_000,
    ));

    let rendered = phase10_render_text(&app, 120, 30);
    assert!(rendered.contains("Context notice"));
    assert!(rendered.contains("Open Context Editor"));
    assert_eq!(serde_json::to_vec(&app.session.messages).unwrap(), raw_before);
    assert_eq!(phase10_display_snapshot(&app), display_before);
    assert!(app.streaming.streaming_text.is_empty());

    let area = crate::tui::ui::last_context_pressure_area().expect("pressure hit area");
    app.handle_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.context_editor_overlay.is_some());
    assert_eq!(app.input, input_before);
    assert_eq!(app.pasted_contents, pasted_before);
    assert_eq!(app.pending_images, images_before);
}

#[test]
fn phase10_pressure_debug_fixtures_are_sensitive_safe_and_render_all_states() {
    let cases = [
        ("normal", None),
        ("notice", Some("Context notice")),
        ("urgent", Some("Context urgent")),
        ("blocked", Some("Request not sent: safe context budget exceeded")),
        ("payload", Some("Request not sent: payload too large")),
    ];
    for (fixture, expected_banner) in cases {
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_session_id = Some("fixture-remote-session".to_string());
        app.context_protocol.accepted_session_id = Some("fixture-remote-session".to_string());
        let response = app.handle_debug_command(&format!("context-pressure-fixture:{fixture}"));
        assert!(response.contains("\"ok\": true"));
        assert!(!response.contains("Synthetic preserved composer draft"));
        assert!(!response.contains("synthetic paste backing"));
        assert!(!response.contains("synthetic-image-data"));
        let state = app.handle_debug_command("context-pressure-state");
        assert!(!state.contains("Synthetic preserved composer draft"));
        let rendered = phase10_render_text(&app, 120, 30);
        match expected_banner {
            Some(expected) => assert!(rendered.contains(expected), "fixture {fixture}"),
            None => assert!(!rendered.contains("Open Context Editor"), "fixture {fixture}"),
        }
        assert!(rendered.contains("Synthetic preserved composer draft"));
        let narrow = phase10_render_text(&app, 52, 30);
        if expected_banner.is_some() {
            assert!(narrow.contains("/context edit"), "narrow fixture {fixture}");
        } else {
            assert!(!narrow.contains("/context edit"), "narrow fixture {fixture}");
        }
        assert!(narrow.contains("Synthetic preserved composer draft"));
    }
}

#[test]
fn phase10_remote_action_restores_exact_prompt_pastes_and_images_only_for_matching_request() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(700);
    let raw = "review [paste 1]".to_string();
    let expanded = "review exact pasted body 🦀".to_string();
    let images = vec![("image/png".to_string(), "exact-image-data".to_string())];
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(700),
        raw_input: raw.clone(),
        expanded: expanded.clone(),
        pasted_contents: vec!["exact pasted body 🦀".to_string()],
        pending_input_tokens: 42,
        image_count: images.len(),
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: false,
    });
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: expanded.clone(),
        images: images.clone(),
        is_system: false,
        system_reminder: None,
        auto_retry: true,
        retry_attempts: 0,
        retry_at: None,
    });
    app.push_display_message(DisplayMessage::user(raw.clone()));
    let metadata = crate::protocol::ContextPendingInputMetadata::new(700, &expanded, images.len());
    let stale = crate::protocol::ServerEvent::ContextActionRequired {
        id: 701,
        session_id: session_id.clone(),
        context_revision: revision,
        reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
        required_reduction_tokens: 1,
        pending_input: Some(metadata.clone()),
        preflight: None,
        payload: None,
        details: Vec::new(),
        automatic_retry: false,
    };
    assert!(!app.reduce_context_server_event(stale).unwrap());
    assert!(app.input.is_empty());

    let event = crate::protocol::ServerEvent::ContextActionRequired {
        id: 700,
        session_id,
        context_revision: revision,
        reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
        required_reduction_tokens: 1,
        pending_input: Some(metadata),
        preflight: None,
        payload: None,
        details: Vec::new(),
        automatic_retry: false,
    };
    assert!(app.reduce_context_server_event(event).unwrap());
    assert_eq!(app.input, raw);
    assert_eq!(app.pasted_contents, vec!["exact pasted body 🦀"]);
    assert_eq!(app.pending_images, images);
    assert!(app.rate_limit_pending_message.is_none());
    assert!(app.pending_composer_input.is_none());
    assert!(app
        .display_messages()
        .iter()
        .all(|message| !(message.role == "user" && message.content == "review [paste 1]")));
}

#[test]
fn phase10_blocked_prompt_waits_for_an_occupied_composer_and_cannot_be_bypassed() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(707);
    app.input = "newer unsent draft".to_string();
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(707),
        raw_input: "blocked [paste 1]".to_string(),
        expanded: "blocked exact paste".to_string(),
        pasted_contents: vec!["exact paste".to_string()],
        pending_input_tokens: 8,
        image_count: 1,
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: false,
    });
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "blocked exact paste".to_string(),
        images: vec![("image/png".to_string(), "blocked-image".to_string())],
        is_system: false,
        system_reminder: None,
        auto_retry: false,
        retry_attempts: 0,
        retry_at: None,
    });
    app.push_display_message(DisplayMessage::user("blocked [paste 1]"));

    assert!(app
        .reduce_context_server_event(crate::protocol::ServerEvent::ContextActionRequired {
            id: 707,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1,
            pending_input: Some(crate::protocol::ContextPendingInputMetadata::new(
                707,
                "blocked exact paste",
                1,
            )),
            preflight: None,
            payload: None,
            details: Vec::new(),
            automatic_retry: false,
        })
        .unwrap());
    assert_eq!(app.input, "newer unsent draft");
    assert!(app.blocked_composer_restore_pending);

    app.submit_input();
    assert_eq!(app.input, "newer unsent draft");
    assert!(app.pending_composer_input.is_some());

    app.input.clear();
    app.submit_input();
    assert_eq!(app.input, "blocked [paste 1]");
    assert_eq!(app.pasted_contents, vec!["exact paste"]);
    assert_eq!(app.pending_images.len(), 1);
    assert!(!app.blocked_composer_restore_pending);
    assert!(app.pending_composer_input.is_none());
}

#[test]
fn phase10_post_output_remote_action_preserves_turn_and_suppresses_terminal_error_retry() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(702);
    app.is_processing = true;
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(702),
        raw_input: "authoritative prompt".to_string(),
        expanded: "authoritative prompt".to_string(),
        pasted_contents: Vec::new(),
        pending_input_tokens: 12,
        image_count: 0,
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: true,
    });
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "authoritative prompt".to_string(),
        images: Vec::new(),
        is_system: false,
        system_reminder: None,
        auto_retry: true,
        retry_attempts: 0,
        retry_at: None,
    });
    app.push_display_message(DisplayMessage::user("authoritative prompt"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    app.handle_server_event(
        crate::protocol::ServerEvent::TextDelta {
            text: "partial answer".to_string(),
        },
        &mut remote,
    );
    assert!(
        app.pending_composer_input
            .as_ref()
            .is_some_and(|pending| pending.output_started),
        "real remote output must cross the no-rollback boundary"
    );

    app.handle_server_event(
        crate::protocol::ServerEvent::ContextActionRequired {
            id: 702,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::ProviderContextLimit,
            required_reduction_tokens: 1,
            pending_input: None,
            preflight: None,
            payload: None,
            details: vec!["partial output retained".to_string()],
            automatic_retry: false,
        },
        &mut remote,
    );
    let display_after_action = phase10_display_snapshot(&app);
    assert!(display_after_action.iter().any(|(role, content)| {
        role == "assistant" && content.contains("partial answer")
    }));
    assert!(app.pending_composer_input.is_none());
    assert_eq!(app.context_action_request_id, Some(702));

    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 702,
            message: "maximum context length exceeded".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    );
    assert_eq!(phase10_display_snapshot(&app), display_after_action);
    assert!(app.rate_limit_pending_message.is_none());
    assert!(app.pending_fallback_resend.is_none());
    assert_eq!(
        app.status_notice(),
        Some("Context action required · partial output preserved".to_string())
    );
}

#[test]
fn phase10_missing_pending_metadata_retains_authoritative_turn_without_composer_restore() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(703);
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(703),
        raw_input: "retained authoritative prompt".to_string(),
        expanded: "retained authoritative prompt".to_string(),
        pasted_contents: Vec::new(),
        pending_input_tokens: 16,
        image_count: 0,
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: false,
    });
    app.push_display_message(DisplayMessage::user("retained authoritative prompt"));
    let display_before = phase10_display_snapshot(&app);

    assert!(app
        .reduce_context_server_event(crate::protocol::ServerEvent::ContextActionRequired {
            id: 703,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1,
            pending_input: None,
            preflight: None,
            payload: None,
            details: vec!["durable rollback failed".to_string()],
            automatic_retry: false,
        })
        .unwrap());
    assert!(app.input.is_empty());
    assert!(app.pending_composer_input.is_none());
    assert_eq!(phase10_display_snapshot(&app), display_before);
    assert_eq!(
        app.status_notice(),
        Some("Context action required · authoritative turn retained".to_string())
    );
}

#[test]
fn phase10_system_remote_block_is_terminal_without_composer_restoration() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(704);
    app.is_processing = true;
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "system follow-up".to_string(),
        images: Vec::new(),
        is_system: true,
        system_reminder: None,
        auto_retry: true,
        retry_attempts: 2,
        retry_at: None,
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ContextActionRequired {
            id: 704,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 128,
            pending_input: Some(crate::protocol::ContextPendingInputMetadata::new(
                704,
                "system follow-up",
                0,
            )),
            preflight: None,
            payload: None,
            details: vec!["system request blocked".to_string()],
            automatic_retry: false,
        },
        &mut remote,
    );
    assert!(app.rate_limit_pending_message.is_none());
    assert!(app.pending_composer_input.is_none());
    assert_eq!(app.context_action_request_id, Some(704));
    assert_eq!(
        app.status_notice(),
        Some("Context action required · request blocked".to_string())
    );

    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 704,
            message: "maximum context length exceeded".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    );
    assert!(app.pending_fallback_resend.is_none());
    assert!(app.rate_limit_pending_message.is_none());
    assert_eq!(
        app.status_notice(),
        Some("Context action required · request blocked".to_string())
    );
}

#[test]
fn phase10_legacy_pending_fingerprint_fails_closed_without_inexact_restoration() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(706);
    app.is_processing = true;
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(706),
        raw_input: "legacy correlated prompt".to_string(),
        expanded: "legacy correlated prompt".to_string(),
        pasted_contents: Vec::new(),
        pending_input_tokens: 8,
        image_count: 0,
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: false,
    });
    app.push_display_message(DisplayMessage::user("legacy correlated prompt"));
    let display_before = phase10_display_snapshot(&app);
    let mut metadata = crate::protocol::ContextPendingInputMetadata::new(
        706,
        "legacy correlated prompt",
        0,
    );
    metadata.content_sha256.clear();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ContextActionRequired {
            id: 706,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            required_reduction_tokens: 1,
            pending_input: Some(metadata),
            preflight: None,
            payload: None,
            details: vec!["legacy metadata".to_string()],
            automatic_retry: false,
        },
        &mut remote,
    );

    assert!(app.input.is_empty());
    assert!(app.pending_composer_input.is_none());
    assert_eq!(phase10_display_snapshot(&app), display_before);
    assert_eq!(
        app.status_notice(),
        Some("Context action required · exact prompt restoration unavailable".to_string())
    );
}

#[test]
fn phase10_local_partial_output_persistence_failure_is_explicit_and_terminal() {
    let mut app = create_test_app();
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(705),
        raw_input: "authoritative local prompt".to_string(),
        expanded: "authoritative local prompt".to_string(),
        pasted_contents: Vec::new(),
        pending_input_tokens: 8,
        image_count: 0,
        local_session_len_before: Some(app.session.messages.len()),
        local_display_len_before: Some(app.display_messages.len()),
        local_provider_len_before: Some(app.messages.len()),
        restoration_images: None,
        request_payload_pressure: None,
        output_started: true,
    });
    app.partial_output_checkpointed = true;
    app.partial_output_persistence_error = Some("injected persistence failure".to_string());

    assert!(app.handle_local_provider_size_error("maximum context length exceeded"));
    assert!(app.pending_composer_input.is_none());
    assert!(app.pending_fallback_resend.is_none());
    assert!(app
        .context_protocol
        .action_required
        .as_ref()
        .is_some_and(|action| action.details.iter().any(|detail|
            detail == crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE)));
    assert!(app.display_messages().iter().any(|message| {
        message.role == "error" && message.content.contains("could not be saved durably")
    }));
}

#[test]
fn phase10_incomplete_remote_output_retains_authoritative_turn_without_false_preservation_copy() {
    let mut app = create_test_app();
    app.is_remote = true;
    let session_id = app.session.id.clone();
    let revision = app.session.context_view.revision;
    app.context_protocol.accepted_session_id = Some(session_id.clone());
    app.context_protocol.accepted_context_revision = Some(revision);
    app.current_message_id = Some(708);
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(708),
        raw_input: "authoritative prompt".to_string(),
        expanded: "authoritative prompt".to_string(),
        pasted_contents: Vec::new(),
        pending_input_tokens: 8,
        image_count: 0,
        local_session_len_before: None,
        local_display_len_before: None,
        local_provider_len_before: None,
        restoration_images: None,
        request_payload_pressure: None,
        output_started: true,
    });

    assert!(app
        .reduce_context_server_event(crate::protocol::ServerEvent::ContextActionRequired {
            id: 708,
            session_id,
            context_revision: revision,
            reason: crate::protocol::ContextActionRequiredReason::ProviderContextLimit,
            required_reduction_tokens: 1,
            pending_input: None,
            preflight: None,
            payload: None,
            details: vec![
                crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE.to_string(),
            ],
            automatic_retry: false,
        })
        .unwrap());
    assert!(app.pending_composer_input.is_none());
    assert_eq!(
        app.status_notice(),
        Some(
            "Provider output began, but no complete partial response could be retained".to_string()
        )
    );
}

#[test]
fn phase10_local_payload_rejection_restores_exact_turn_and_manual_resend_appends_once() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    app.replace_provider_messages(Vec::new());
    app.display_messages.clear();
    let raw = "inspect [paste 1]".to_string();
    let expanded = "inspect exact local paste".to_string();
    let pasted = vec!["exact local paste".to_string()];
    let images = vec![("image/png".to_string(), "local-image-data".to_string())];
    let session_before = app.session.messages.len();
    let display_before = app.display_messages.len();
    let provider_before = app.messages.len();
    let blocks = vec![
        ContentBlock::Image {
            media_type: images[0].0.clone(),
            data: images[0].1.clone(),
        },
        ContentBlock::Text {
            text: expanded.clone(),
            cache_control: None,
        },
    ];
    app.session.add_message(Role::User, blocks.clone());
    app.add_provider_message(Message {
        role: Role::User,
        content: blocks,
        timestamp: Some(chrono::Utc::now()),
        tool_duration_ms: None,
    });
    app.push_display_message(DisplayMessage::user(raw.clone()));
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(704),
        raw_input: raw.clone(),
        expanded: expanded.clone(),
        pasted_contents: pasted.clone(),
        pending_input_tokens: crate::context::estimate_pending_input_tokens(&expanded, 1),
        image_count: 1,
        local_session_len_before: Some(session_before),
        local_display_len_before: Some(display_before),
        local_provider_len_before: Some(provider_before),
        restoration_images: None,
        request_payload_pressure: Some(crate::protocol::ContextPayloadPressure {
            image_count: 2,
            estimated_base64_bytes: 123,
        }),
        output_started: false,
    });
    app.set_local_context_pressure(phase10_pressure_report(
        app.session.context_view.revision,
        100_000,
        90_000,
    ));

    assert!(app.handle_local_provider_size_error("HTTP 413 payload too large"));
    assert_eq!(app.session.messages.len(), session_before);
    assert_eq!(app.messages.len(), provider_before);
    assert_eq!(app.input, raw);
    assert_eq!(app.pasted_contents, pasted);
    assert_eq!(app.pending_images, images);
    assert!(app.pending_composer_input.is_none());
    assert!(app.context_protocol.action_required.is_some());
    let payload = app
        .context_protocol
        .action_required
        .as_ref()
        .and_then(|action| action.payload.as_ref())
        .expect("payload pressure retained");
    assert_eq!(payload.image_count, 2);
    assert_eq!(payload.estimated_base64_bytes, 123);

    super::local::finish_turn(&mut app);
    app.submit_input();
    let matching_users = app
        .session
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .count();
    assert_eq!(matching_users, 1);
}

#[test]
fn context_budget_rewind_undo_and_repair_reseed_exactly() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    for index in 1..=3 {
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("message-{index}"),
                cache_control: None,
            }],
        );
    }
    app.replace_provider_messages(app.session.raw_messages_for_provider_uncached());
    set_app_context_observation(&app, 9_000);

    app.input = "/rewind 1".to_string();
    app.submit_input();
    let rewound = app.materialized_provider_messages();
    let rewound_stats = context_budget_stats_for_app(&app);
    assert_eq!(rewound_stats.observed_input_tokens, None);
    assert_eq!(rewound_stats.message_count, rewound.len());
    assert_eq!(
        rewound_stats.estimated_message_tokens,
        estimated_app_message_tokens(&rewound)
    );

    set_app_context_observation(&app, 8_000);
    app.input = "/rewind undo".to_string();
    app.submit_input();
    let restored = app.materialized_provider_messages();
    let restored_stats = context_budget_stats_for_app(&app);
    assert_eq!(restored_stats.observed_input_tokens, None);
    assert_eq!(restored_stats.message_count, restored.len());
    assert_eq!(
        restored_stats.estimated_message_tokens,
        estimated_app_message_tokens(&restored)
    );

    let context_before_repair = serde_json::to_vec(&app.session.context_view).unwrap();
    app.session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "missing-tui-result".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
            thought_signature: None,
        }],
    );
    app.replace_provider_messages(app.session.raw_messages_for_provider_uncached());
    set_app_context_observation(&app, 9_500);
    assert_eq!(app.repair_missing_tool_outputs(), 1);

    let repaired = app.materialized_provider_messages();
    let repaired_stats = context_budget_stats_for_app(&app);
    assert_eq!(repaired_stats.observed_input_tokens, None);
    assert_eq!(repaired_stats.message_count, repaired.len());
    assert_eq!(
        repaired_stats.estimated_message_tokens,
        estimated_app_message_tokens(&repaired)
    );
    assert_eq!(
        serde_json::to_vec(&app.session.context_view).unwrap(),
        context_before_repair
    );
}

#[test]
fn context_budget_legacy_summary_reseed_uses_summary_plus_tail() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    for index in 0..4 {
        app.session.add_message(
            if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            vec![ContentBlock::Text {
                text: format!("raw-{index}-{}", "x".repeat(8_000)),
                cache_control: None,
            }],
        );
    }
    let raw_messages = app.session.raw_messages_for_provider_uncached();
    let raw_tokens = estimated_app_message_tokens(&raw_messages);
    app.session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "concise local summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 2,
        original_turn_count: 4,
        compacted_count: 2,
    });
    let migration = app.session.migrate_legacy_compaction_state();
    assert!(migration.changed_state());
    let effective_messages = app
        .session
        .projected_messages_for_provider()
        .expect("migrated legacy summary should project");
    app.reseed_context_budget_from_messages(&effective_messages, "legacy summary migration test");
    let stats = context_budget_stats_for_app(&app);
    let effective_tokens = estimated_app_message_tokens(&effective_messages);
    assert_eq!(stats.message_count, effective_messages.len());
    assert_eq!(stats.estimated_message_tokens, effective_tokens);
    assert!(effective_tokens < raw_tokens);
}

#[test]
fn local_provider_send_projection_failure_is_actionable_and_preserves_raw_transcript() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "authoritative prompt must survive".to_string(),
            cache_control: None,
        }],
    );
    let raw_before = serde_json::to_vec(&app.session.messages).expect("raw transcript");
    let transaction = |revision: u64| jcode_session_types::StoredContextTransaction {
        id: "duplicate-transaction-id".to_string(),
        base_revision: revision.saturating_sub(1),
        created_at: chrono::Utc::now(),
        authorization: jcode_session_types::StoredContextAuthorization::Manual {
            initiated_by: None,
        },
        operations: Vec::new(),
        status_events: vec![jcode_session_types::StoredContextStatusEvent {
            revision,
            timestamp: chrono::Utc::now(),
            kind: jcode_session_types::StoredContextTransactionStatusKind::Applied,
            reason: None,
        }],
        application: None,
        economics: None,
        curator_usage: Vec::new(),
    };
    app.session.context_view = jcode_session_types::StoredContextViewState {
        revision: 2,
        transactions: vec![transaction(1), transaction(2)],
        ..jcode_session_types::StoredContextViewState::default()
    };

    let error = app
        .projected_messages_for_provider_send()
        .expect_err("invalid projection must block the provider request");

    assert!(error.contains("provider request was not sent"));
    assert!(error.contains("/context history"));
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript"),
        raw_before
    );
}

#[derive(Clone, Default)]
struct LocalInvocationRecordingProvider {
    requests: StdArc<StdMutex<Vec<Vec<Message>>>>,
    invalidations: StdArc<AtomicUsize>,
    reject_context_validation: StdArc<AtomicBool>,
}

impl LocalInvocationRecordingProvider {
    fn requests(&self) -> Vec<Vec<Message>> {
        self.requests.lock().unwrap().clone()
    }

    fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::SeqCst)
    }

    fn set_context_validation_rejected(&self, rejected: bool) {
        self.reject_context_validation
            .store(rejected, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Provider for LocalInvocationRecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        self.requests.lock().unwrap().push(messages.to_vec());
        Ok(Box::pin(futures::stream::empty::<
            Result<crate::message::StreamEvent>,
        >()))
    }

    fn name(&self) -> &str {
        "local-invocation-recording"
    }

    fn display_name(&self) -> String {
        "Local Invocation Recording".to_string()
    }

    fn model(&self) -> String {
        "local-invocation-model".to_string()
    }

    fn context_window(&self) -> usize {
        100_000
    }

    fn validate_projected_context(
        &self,
        messages: &[Message],
        operations: &[crate::provider::ContextProjectionValidationOperation],
    ) -> crate::provider::ContextProjectionValidationReport {
        let builder = if self.reject_context_validation.load(Ordering::SeqCst) {
            Err("injected local Context Editor provider validation failure".to_string())
        } else {
            Ok(crate::provider::ContextRequestBuilderValidation::new(
                messages.len(),
            ))
        };
        crate::provider::context_projection_validation_report(
            crate::provider::ContextProviderValidationIdentity {
                family: crate::provider::ContextProviderFamily::OpenRouterCompatible,
                provider_name: self.name().to_string(),
                provider_display_name: self.display_name(),
                model: self.model(),
                evidence_tag: "local_context_editor_reset_fixture_v1".to_string(),
            },
            operations,
            Some(crate::provider::ContextReasoningBlockKind::GenericReasoning),
            builder,
        )
    }

    fn invalidate_context_continuation(&self, _reason: &str) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_local_invocation_recording_app(
    provider: LocalInvocationRecordingProvider,
) -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app
}

fn provider_text(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prepare_local_reasoning_only_draft(
    runtime: &tokio::runtime::Runtime,
    app: &mut App,
    reasoning_message_id: String,
) -> String {
    runtime.block_on(async {
        app.context_editor_actions.push_back(
            crate::tui::context_editor::ContextEditorAction::PrepareDraft(
                crate::protocol::ContextDraftRequest {
                    summary_ranges: Vec::new(),
                    reasoning: Some(
                        crate::protocol::ContextReasoningSelectionRequest::MessageRanges {
                            ranges: vec![crate::protocol::ContextMessageRangeSelection {
                                start_message_id: reasoning_message_id.clone(),
                                end_message_id: reasoning_message_id,
                            }],
                        },
                    ),
                    tool_results: Vec::new(),
                    allow_shadowing_active_operations: false,
                    authorization: jcode_session_types::StoredContextAuthorization::Manual {
                        initiated_by: None,
                    },
                },
            ),
        );
        assert!(app.dispatch_local_context_editor_actions());

        loop {
            let event = tokio::time::timeout(
                Duration::from_secs(2),
                app.local_context_event_rx.recv(),
            )
            .await
            .expect("reasoning-only draft monitor timed out")
            .expect("reasoning-only draft monitor channel closed");
            let ready_draft_id = match &event {
                crate::protocol::ServerEvent::ContextDraftReady { draft, .. } => {
                    Some(draft.identity.draft_id.clone())
                }
                crate::protocol::ServerEvent::ContextDraftFailed { error, .. }
                | crate::protocol::ServerEvent::ContextDraftStale { error, .. } => {
                    panic!("reasoning-only draft unexpectedly failed: {error}")
                }
                crate::protocol::ServerEvent::ContextRequestRejected { error, .. } => {
                    panic!("reasoning-only draft was unexpectedly rejected: {error}")
                }
                _ => None,
            };
            assert!(
                app.reduce_context_server_event(event)
                    .expect("local draft monitor emitted a context event"),
                "local draft monitor event was rejected"
            );
            if let Some(draft_id) = ready_draft_id {
                return draft_id;
            }
        }
    })
}

fn append_local_replayable_reasoning_message(app: &mut App, suffix: &str) -> String {
    app.session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: format!("replayed reasoning {suffix}"),
            },
            ContentBlock::Text {
                text: format!("visible answer {suffix}"),
                cache_control: None,
            },
        ],
    );
    app.session
        .messages
        .last()
        .expect("replayable reasoning message")
        .id
        .clone()
}

fn assert_context_reset_totals(
    app: &App,
    expected: usize,
    label: &str,
) {
    assert_eq!(
        app.context_reset_counters,
        ContextResetCounters {
            hook_calls: expected,
            invalidation_records: expected,
            cache_generation_advances: expected,
            continuation_invalidations: expected,
            projected_rebuild_attempts: expected,
            budget_reseeds: expected,
        },
        "unexpected App reset multiplicity after {label}"
    );
}

fn assert_projected_reasoning_presence(app: &mut App, expected: bool, label: &str) {
    let projected = app
        .session
        .projected_messages_for_provider()
        .expect("valid projected provider messages");
    let has_reasoning = projected.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
    });
    assert_eq!(has_reasoning, expected, "projected reasoning mismatch after {label}");
    let budget = context_budget_stats_for_app(app);
    assert_eq!(
        budget.message_count,
        projected.len(),
        "context budget was not reseeded from the projected view after {label}"
    );
}

fn dispatch_local_transaction_action(
    app: &mut App,
    action: crate::tui::context_editor::ContextEditorAction,
    label: &str,
) {
    app.context_editor_actions.push_back(action);
    assert!(
        app.dispatch_local_context_editor_actions(),
        "{label} action was not dispatched"
    );
    assert!(
        app.drain_local_context_events(),
        "{label} transaction outcome was not accepted"
    );
    let revision = app.context_revision;
    assert!(
        !app.drain_local_context_events(),
        "{label} produced a duplicate accepted transaction outcome"
    );
    assert_eq!(
        app.context_revision, revision,
        "{label} duplicate drain changed the UI revision"
    );
    assert!(
        app.context_editor_actions.is_empty(),
        "{label} queued an unexpected Context Editor follow-up action"
    );
}

#[test]
fn local_provider_invocation_uses_applied_reverted_and_reapplied_projection() {
    let provider = LocalInvocationRecordingProvider::default();
    let mut app = create_local_invocation_recording_app(provider.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("local provider invocation runtime");
    app.session.replace_messages(Vec::new());
    for (role, text) in [
        (Role::User, "raw prompt before summary"),
        (Role::Assistant, "raw answer before summary"),
        (Role::User, "raw prompt after summary"),
    ] {
        app.session.add_message(
            role,
            vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        );
    }
    let raw_before = serde_json::to_vec(&app.session.messages).expect("raw transcript");
    app.session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "authoritative migrated context summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 2,
        original_turn_count: 3,
        compacted_count: 2,
    });
    assert!(app.session.migrate_legacy_compaction_state().changed_state());

    let applied_invocation = runtime
        .block_on(app.prepare_local_provider_invocation())
        .expect("applied projection should prepare");
    let _applied_stream = runtime
        .block_on(applied_invocation.invoke())
        .expect("applied projection should reach provider");

    app.session.context_view.revision = 2;
    app.session.context_view.transactions[0]
        .status_events
        .push(jcode_session_types::StoredContextStatusEvent {
            revision: 2,
            timestamp: chrono::Utc::now(),
            kind: jcode_session_types::StoredContextTransactionStatusKind::Reverted,
            reason: Some("local provider invocation revert".to_string()),
        });
    let reverted_invocation = runtime
        .block_on(app.prepare_local_provider_invocation())
        .expect("reverted projection should prepare");
    let _reverted_stream = runtime
        .block_on(reverted_invocation.invoke())
        .expect("reverted projection should reach provider");

    app.session.context_view.revision = 3;
    app.session.context_view.transactions[0]
        .status_events
        .push(jcode_session_types::StoredContextStatusEvent {
            revision: 3,
            timestamp: chrono::Utc::now(),
            kind: jcode_session_types::StoredContextTransactionStatusKind::Reapplied,
            reason: Some("local provider invocation reapply".to_string()),
        });
    let reapplied_invocation = runtime
        .block_on(app.prepare_local_provider_invocation())
        .expect("reapplied projection should prepare");
    let _reapplied_stream = runtime
        .block_on(reapplied_invocation.invoke())
        .expect("reapplied projection should reach provider");

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let applied = provider_text(&requests[0]);
    let reverted = provider_text(&requests[1]);
    let reapplied = provider_text(&requests[2]);
    assert!(applied.contains("authoritative migrated context summary"));
    assert!(!applied.contains("raw prompt before summary"));
    assert!(applied.contains("raw prompt after summary"));
    assert!(reverted.contains("raw prompt before summary"));
    assert!(reverted.contains("raw answer before summary"));
    assert!(!reverted.contains("authoritative migrated context summary"));
    assert_eq!(reapplied, applied);
    for request in &requests {
        let text = provider_text(request);
        assert!(!text.contains("context curator"));
        assert!(!text.contains("distill_output"));
        assert!(!text.contains("strict target ratio"));
    }
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript"),
        raw_before
    );

    let duplicate = app.session.context_view.transactions[0].clone();
    app.session.context_view.revision = 4;
    app.session.context_view.transactions.push(duplicate);
    let request_count_before = provider.requests().len();
    let error = match runtime.block_on(app.prepare_local_provider_invocation()) {
        Ok(_) => panic!("invalid projection unexpectedly prepared a provider invocation"),
        Err(error) => error,
    };
    assert!(error.contains("provider request was not sent"));
    assert_eq!(provider.requests().len(), request_count_before);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript"),
        raw_before
    );
}

#[test]
fn local_context_apply_revert_and_reapply_reset_every_app_boundary_exactly_once() {
    let provider = LocalInvocationRecordingProvider::default();
    let mut app = create_local_invocation_recording_app(provider.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("local context reset runtime");
    app.session.replace_messages(Vec::new());
    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "retain this authoritative prompt".to_string(),
            cache_control: None,
        }],
    );
    let reasoning_message_id = append_local_replayable_reasoning_message(&mut app, "for reset proof");
    let raw_before = serde_json::to_vec(&app.session.messages).expect("raw transcript");
    let cache_generation_before = app.kv_cache.cache_generation;
    let ui_revision_before = app.context_revision;

    let draft_id =
        prepare_local_reasoning_only_draft(&runtime, &mut app, reasoning_message_id.clone());
    assert_context_reset_totals(&app, 0, "reasoning-only draft preparation");
    assert_eq!(provider.invalidations(), 0);
    assert_eq!(app.kv_cache.cache_generation, cache_generation_before);
    assert_eq!(app.context_revision, ui_revision_before);
    assert_eq!(app.session.context_view.revision, 0);

    app.context_editor_actions.push_back(
        crate::tui::context_editor::ContextEditorAction::PreviewDraftSelection {
            draft_id: draft_id.clone(),
            selected_distillation_ids: Vec::new(),
        },
    );
    assert!(app.dispatch_local_context_editor_actions());
    assert!(app.drain_local_context_events());
    assert_context_reset_totals(&app, 0, "selected-proposal preview");
    assert_eq!(provider.invalidations(), 0);
    assert_eq!(app.kv_cache.cache_generation, cache_generation_before);
    assert_eq!(app.context_revision, ui_revision_before);
    assert_eq!(app.session.context_view.revision, 0);

    app.provider_session_id = Some("app-provider-session-before-apply".to_string());
    app.session.provider_session_id = Some("stored-provider-session-before-apply".to_string());
    dispatch_local_transaction_action(
        &mut app,
        crate::tui::context_editor::ContextEditorAction::ApplyDraft {
            draft_id: draft_id.clone(),
            selected_distillation_ids: Vec::new(),
        },
        "apply",
    );
    assert_context_reset_totals(&app, 1, "apply");
    assert_eq!(provider.invalidations(), 1);
    assert_eq!(
        app.kv_cache.cache_generation,
        cache_generation_before.wrapping_add(1)
    );
    assert_eq!(app.context_revision, ui_revision_before.wrapping_add(1));
    assert_eq!(app.session.context_view.revision, 1);
    assert_eq!(app.session.context_view.transactions.len(), 1);
    assert_eq!(app.provider_session_id, None);
    assert_eq!(app.session.provider_session_id, None);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript after apply"),
        raw_before
    );
    assert_projected_reasoning_presence(&mut app, false, "apply");
    let transaction_id = app.session.context_view.transactions[0].id.clone();
    let outcome = app
        .context_protocol
        .transaction_result
        .as_ref()
        .expect("accepted apply outcome");
    assert_eq!(outcome.request, crate::protocol::ContextRequestKind::ApplyDraft);
    assert_eq!(outcome.correlation_id, draft_id);
    assert_eq!(outcome.result.revision, 1);

    app.provider_session_id = Some("app-provider-session-before-revert".to_string());
    app.session.provider_session_id = Some("stored-provider-session-before-revert".to_string());
    dispatch_local_transaction_action(
        &mut app,
        crate::tui::context_editor::ContextEditorAction::RevertTransaction {
            transaction_id: transaction_id.clone(),
        },
        "revert",
    );
    assert_context_reset_totals(&app, 2, "revert");
    assert_eq!(provider.invalidations(), 2);
    assert_eq!(
        app.kv_cache.cache_generation,
        cache_generation_before.wrapping_add(2)
    );
    assert_eq!(app.context_revision, ui_revision_before.wrapping_add(2));
    assert_eq!(app.session.context_view.revision, 2);
    assert_eq!(app.session.context_view.transactions.len(), 1);
    assert_eq!(app.provider_session_id, None);
    assert_eq!(app.session.provider_session_id, None);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript after revert"),
        raw_before
    );
    assert_projected_reasoning_presence(&mut app, true, "revert");
    let outcome = app
        .context_protocol
        .transaction_result
        .as_ref()
        .expect("accepted revert outcome");
    assert_eq!(
        outcome.request,
        crate::protocol::ContextRequestKind::RevertTransaction
    );
    assert_eq!(outcome.correlation_id, transaction_id);
    assert_eq!(outcome.result.revision, 2);

    app.provider_session_id = Some("app-provider-session-before-reapply".to_string());
    app.session.provider_session_id = Some("stored-provider-session-before-reapply".to_string());
    dispatch_local_transaction_action(
        &mut app,
        crate::tui::context_editor::ContextEditorAction::ReapplyTransaction {
            transaction_id: transaction_id.clone(),
        },
        "reapply",
    );
    assert_context_reset_totals(&app, 3, "reapply");
    assert_eq!(provider.invalidations(), 3);
    assert_eq!(
        app.kv_cache.cache_generation,
        cache_generation_before.wrapping_add(3)
    );
    assert_eq!(app.context_revision, ui_revision_before.wrapping_add(3));
    assert_eq!(app.session.context_view.revision, 3);
    assert_eq!(app.session.context_view.transactions.len(), 1);
    assert_eq!(app.provider_session_id, None);
    assert_eq!(app.session.provider_session_id, None);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).expect("raw transcript after reapply"),
        raw_before
    );
    assert_projected_reasoning_presence(&mut app, false, "reapply");
    let outcome = app
        .context_protocol
        .transaction_result
        .as_ref()
        .expect("accepted reapply outcome");
    assert_eq!(
        outcome.request,
        crate::protocol::ContextRequestKind::ReapplyTransaction
    );
    assert_eq!(outcome.correlation_id, transaction_id);
    assert_eq!(outcome.result.revision, 3);

    let status_events = &app.session.context_view.transactions[0].status_events;
    assert_eq!(status_events.len(), 3);
    assert_eq!(status_events[0].revision, 1);
    assert_eq!(status_events[1].revision, 2);
    assert_eq!(status_events[2].revision, 3);
    assert_eq!(
        status_events[0].kind,
        jcode_session_types::StoredContextTransactionStatusKind::Applied
    );
    assert_eq!(
        status_events[1].kind,
        jcode_session_types::StoredContextTransactionStatusKind::Reverted
    );
    assert_eq!(
        status_events[2].kind,
        jcode_session_types::StoredContextTransactionStatusKind::Reapplied
    );
}

#[derive(Default)]
struct LocalContextNoopAgentPersistence;

impl crate::context::ContextPersistence for LocalContextNoopAgentPersistence {
    fn persist(&self, _agent: &mut crate::agent::Agent) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct LocalContextFailingDirectPersistence {
    calls: AtomicUsize,
}

impl crate::context::DirectContextSessionPersistence for LocalContextFailingDirectPersistence {
    fn persist(&self, _session: &mut Session) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("injected TUI direct Session persistence failure")
    }
}

fn dispatch_local_rejected_transaction_action(
    app: &mut App,
    action: crate::tui::context_editor::ContextEditorAction,
    expected_error: &str,
) {
    app.context_editor_actions.push_back(action);
    assert!(app.dispatch_local_context_editor_actions());
    assert!(app.drain_local_context_events());
    assert!(!app.drain_local_context_events());
    let rejection = app
        .context_protocol
        .last_rejection
        .as_ref()
        .expect("accepted local transaction rejection");
    assert!(
        rejection.error.to_string().contains(expected_error),
        "unexpected rejection: {}",
        rejection.error
    );
}

#[test]
fn local_noncommitting_context_paths_perform_zero_app_resets() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("local zero-reset runtime");

    let provider = LocalInvocationRecordingProvider::default();
    let mut stale_app = create_local_invocation_recording_app(provider.clone());
    stale_app.session.replace_messages(Vec::new());
    let reasoning_id = append_local_replayable_reasoning_message(&mut stale_app, "for stale proof");
    let stale_draft =
        prepare_local_reasoning_only_draft(&runtime, &mut stale_app, reasoning_id);
    stale_app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "append after draft capture".to_string(),
            cache_control: None,
        }],
    );
    let stale_raw_before =
        serde_json::to_vec(&stale_app.session.messages).expect("stale raw transcript");
    let stale_cache_generation = stale_app.kv_cache.cache_generation;
    let stale_ui_revision = stale_app.context_revision;
    dispatch_local_rejected_transaction_action(
        &mut stale_app,
        crate::tui::context_editor::ContextEditorAction::ApplyDraft {
            draft_id: stale_draft,
            selected_distillation_ids: Vec::new(),
        },
        "raw message count changed",
    );
    assert_context_reset_totals(&stale_app, 0, "stale draft rejection");
    assert_eq!(provider.invalidations(), 0);
    assert_eq!(stale_app.kv_cache.cache_generation, stale_cache_generation);
    assert_eq!(stale_app.context_revision, stale_ui_revision);
    assert_eq!(stale_app.session.context_view.revision, 0);
    assert_eq!(
        serde_json::to_vec(&stale_app.session.messages).expect("stale raw transcript after reject"),
        stale_raw_before
    );

    let provider = LocalInvocationRecordingProvider::default();
    let mut validation_app = create_local_invocation_recording_app(provider.clone());
    validation_app.session.replace_messages(Vec::new());
    let reasoning_id =
        append_local_replayable_reasoning_message(&mut validation_app, "for validation proof");
    let validation_draft =
        prepare_local_reasoning_only_draft(&runtime, &mut validation_app, reasoning_id);
    let validation_raw_before = serde_json::to_vec(&validation_app.session.messages)
        .expect("validation raw transcript");
    let validation_cache_generation = validation_app.kv_cache.cache_generation;
    let validation_ui_revision = validation_app.context_revision;
    provider.set_context_validation_rejected(true);
    dispatch_local_rejected_transaction_action(
        &mut validation_app,
        crate::tui::context_editor::ContextEditorAction::ApplyDraft {
            draft_id: validation_draft,
            selected_distillation_ids: Vec::new(),
        },
        "provider validation",
    );
    assert_context_reset_totals(&validation_app, 0, "provider validation rejection");
    assert_eq!(provider.invalidations(), 0);
    assert_eq!(
        validation_app.kv_cache.cache_generation,
        validation_cache_generation
    );
    assert_eq!(validation_app.context_revision, validation_ui_revision);
    assert_eq!(validation_app.session.context_view.revision, 0);
    assert_eq!(
        serde_json::to_vec(&validation_app.session.messages)
            .expect("validation raw transcript after reject"),
        validation_raw_before
    );

    let provider = LocalInvocationRecordingProvider::default();
    let mut persistence_app = create_local_invocation_recording_app(provider.clone());
    persistence_app.session.replace_messages(Vec::new());
    let direct_persistence = StdArc::new(LocalContextFailingDirectPersistence::default());
    persistence_app.context_transactions = Arc::new(
        crate::context::ContextTransactionService::with_persistence_boundaries(
            crate::context::ContextServiceLimits::default(),
            Arc::new(LocalContextNoopAgentPersistence),
            direct_persistence.clone(),
        ),
    );
    let reasoning_id =
        append_local_replayable_reasoning_message(&mut persistence_app, "for persistence proof");
    let persistence_draft =
        prepare_local_reasoning_only_draft(&runtime, &mut persistence_app, reasoning_id);
    persistence_app.provider_session_id = Some("app-provider-session-preserved".to_string());
    persistence_app.session.provider_session_id =
        Some("stored-provider-session-preserved".to_string());
    let persistence_raw_before = serde_json::to_vec(&persistence_app.session.messages)
        .expect("persistence raw transcript");
    let persistence_state_before = persistence_app.session.context_view.clone();
    let persistence_cache_generation = persistence_app.kv_cache.cache_generation;
    let persistence_ui_revision = persistence_app.context_revision;
    dispatch_local_rejected_transaction_action(
        &mut persistence_app,
        crate::tui::context_editor::ContextEditorAction::ApplyDraft {
            draft_id: persistence_draft,
            selected_distillation_ids: Vec::new(),
        },
        "persistence",
    );
    assert_eq!(direct_persistence.calls.load(Ordering::SeqCst), 1);
    assert_context_reset_totals(&persistence_app, 0, "persistence rollback");
    assert_eq!(provider.invalidations(), 0);
    assert_eq!(
        persistence_app.kv_cache.cache_generation,
        persistence_cache_generation
    );
    assert_eq!(persistence_app.context_revision, persistence_ui_revision);
    assert_eq!(persistence_app.session.context_view, persistence_state_before);
    assert_eq!(
        persistence_app.provider_session_id.as_deref(),
        Some("app-provider-session-preserved")
    );
    assert_eq!(
        persistence_app.session.provider_session_id.as_deref(),
        Some("stored-provider-session-preserved")
    );
    assert_eq!(
        serde_json::to_vec(&persistence_app.session.messages)
            .expect("persistence raw transcript after reject"),
        persistence_raw_before
    );
}

#[test]
fn context_editor_debug_socket_command_returns_only_metadata_safe_summary() {
    let mut app = create_test_app();
    app.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
    app.context_editor_actions.clear();
    let expected = app.context_editor_debug_summary();

    let output = app.handle_debug_command("context-editor-state");
    let actual: serde_json::Value =
        serde_json::from_str(&output).expect("Context Editor debug command JSON");

    assert_eq!(actual, expected);
    assert_eq!(actual["open"], serde_json::json!(true));
    assert_eq!(actual["phase"], serde_json::json!("loading"));
    assert!(!output.contains("\"input\""));
    assert!(!output.contains("authorization_source"));
    assert!(!output.contains("summary_text"));
    assert!(!output.contains("replacement_content"));

    let fixture_list: serde_json::Value = serde_json::from_str(
        &app.handle_debug_command("context-editor-fixtures"),
    )
    .expect("Context Editor fixture list JSON");
    assert!(
        fixture_list["fixtures"]
            .as_array()
            .is_some_and(|fixtures| fixtures.len() >= 35)
    );

    let fixture: serde_json::Value = serde_json::from_str(
        &app.handle_debug_command("context-editor-fixture:long-final-review"),
    )
    .expect("Context Editor fixture response JSON");
    assert_eq!(fixture["ok"], serde_json::json!(true));
    assert_eq!(fixture["fixture"], serde_json::json!("long-final-review"));
    assert_eq!(fixture["state"]["phase"], serde_json::json!("review_draft"));
    assert_eq!(fixture["state"]["proposal_selections"], serde_json::json!(9));
    let fixture_json = fixture.to_string();
    assert!(!fixture_json.contains("synthetic-debug-source-not-rendered"));
    assert!(!fixture_json.contains("replacement_content"));

    let unknown: serde_json::Value = serde_json::from_str(
        &app.handle_debug_command("context-editor-fixture:not-a-real-fixture"),
    )
    .expect("unknown Context Editor fixture response JSON");
    assert_eq!(unknown["ok"], serde_json::json!(false));
    assert!(unknown["error"].as_str().is_some_and(|error| {
        error.contains("unknown Context Editor fixture")
    }));
}
