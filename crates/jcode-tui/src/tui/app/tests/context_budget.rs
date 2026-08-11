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
    app.messages.clear();
    app.reseed_context_runtime_from_provider_messages();

    let effective_messages = app.messages_for_provider().0;
    let stats = context_budget_stats_for_app(&app);
    let effective_tokens = estimated_app_message_tokens(&effective_messages);
    assert_eq!(stats.message_count, effective_messages.len());
    assert_eq!(stats.estimated_message_tokens, effective_tokens);
    assert!(effective_tokens < raw_tokens);
}
