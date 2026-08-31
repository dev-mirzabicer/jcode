fn render_startup_context_fixture(app: &App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, app))
        .expect("render Startup Context fixture");
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

#[test]
fn startup_context_compact_status_renders_every_required_state_at_wide_and_narrow_sizes() {
    let cases = [
        ("loading", "Loading"),
        ("none", "none"),
        ("ready", "Captured"),
        ("blocked", "Blocked"),
        ("dispatched", "Dispatched"),
        ("accepted", "Accepted"),
        ("queued", "Queued"),
        ("stale", "Stale"),
        ("busy", "editor busy"),
        ("unsupported", "Unsupported"),
        ("metadata-repair", "Metadata repair"),
        ("storage-error", "Storage error"),
    ];
    for (fixture, expected) in cases {
        let mut app = create_test_app();
        app.apply_startup_context_debug_fixture(fixture)
            .expect("apply fixture");
        for (width, height) in [(120, 30), (72, 24), (38, 12)] {
            let rendered = render_startup_context_fixture(&app, width, height);
            assert!(
                rendered.contains("Startup"),
                "{fixture} at {width}x{height}"
            );
            assert!(
                rendered.contains(expected),
                "{fixture} at {width}x{height} should show {expected:?}\n{rendered}"
            );
            assert!(!rendered.contains("Synthetic request was not sent"));
        }
    }
}

#[test]
fn startup_context_strip_mouse_and_slash_routes_preserve_composer_and_selection() {
    let mut app = create_test_app();
    app.apply_startup_context_debug_fixture("ready")
        .expect("ready fixture");
    app.input = "composer draft".to_string();
    app.cursor_pos = 4;
    app.pasted_contents = vec!["paste backing".to_string()];
    app.pending_images = vec![("image/png".to_string(), "image-data".to_string())];
    app.copy_selection_anchor = Some(crate::tui::CopySelectionPoint {
        pane: crate::tui::CopySelectionPane::Input,
        abs_line: 0,
        column: 1,
    });
    app.copy_selection_cursor = Some(crate::tui::CopySelectionPoint {
        pane: crate::tui::CopySelectionPane::Input,
        abs_line: 0,
        column: 5,
    });
    let input = app.input.clone();
    let cursor = app.cursor_pos;
    let pastes = app.pasted_contents.clone();
    let images = app.pending_images.clone();
    let selection = (app.copy_selection_anchor, app.copy_selection_cursor);

    let rendered = render_startup_context_fixture(&app, 120, 30);
    assert!(rendered.contains("4 files"));
    assert!(rendered.contains("22.4K tokens"));
    let area = crate::tui::ui::last_startup_context_area().expect("Startup Context hit area");
    assert!(
        crate::tui::ui::last_context_pressure_area().is_none_or(|pressure| pressure != area),
        "Startup Context and context-pressure hit regions must not overlap"
    );
    app.handle_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.startup_context_overlay_scroll().is_some());
    assert_eq!(app.input, input);
    assert_eq!(app.cursor_pos, cursor);
    assert_eq!(app.pasted_contents, pastes);
    assert_eq!(app.pending_images, images);
    assert_eq!(
        (app.copy_selection_anchor, app.copy_selection_cursor),
        selection
    );

    let overlay = render_startup_context_fixture(&app, 72, 24);
    assert!(overlay.contains("/startup"));
    assert!(overlay.contains("docs/PLAN.md"));
    super::input::handle_modal_key(
        &mut app,
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    )
    .expect("close overlay");
    assert!(app.startup_context_overlay_scroll().is_none());

    app.input = "/startup".to_string();
    app.cursor_pos = app.input.len();
    app.submit_input();
    assert!(app.startup_context_overlay_scroll().is_some());
    assert!(app.input.is_empty());
}

#[test]
fn startup_context_blocked_action_restores_exact_composer_and_ignores_stale_status() {
    let mut app = create_test_app();
    app.apply_startup_context_debug_fixture("blocked")
        .expect("blocked fixture");
    let snapshot = app.startup_context_detail().expect("detail").clone();
    let request_id = 700;
    let raw = "review [paste 1] carefully".to_string();
    let expanded = "review exact pasted body carefully".to_string();
    let images = vec![("image/png".to_string(), "exact-image-data".to_string())];
    app.current_message_id = Some(request_id);
    app.is_processing = true;
    app.pending_composer_input = Some(PendingComposerInput {
        request_id: Some(request_id),
        raw_input: raw.clone(),
        cursor_pos: 7,
        expanded: expanded.clone(),
        pasted_contents: vec!["exact pasted body".to_string()],
        pending_input_tokens: 12,
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
        auto_retry: false,
        retry_attempts: 0,
        retry_at: None,
    });
    app.push_display_message(DisplayMessage::user(raw.clone()));
    let selection = (
        Some(crate::tui::CopySelectionPoint {
            pane: crate::tui::CopySelectionPane::Input,
            abs_line: 0,
            column: 2,
        }),
        Some(crate::tui::CopySelectionPoint {
            pane: crate::tui::CopySelectionPane::Input,
            abs_line: 0,
            column: 6,
        }),
    );
    app.copy_selection_anchor = selection.0;
    app.copy_selection_cursor = selection.1;
    let action = crate::protocol::StartupContextActionRequired {
        kind: crate::protocol::StartupContextActionKind::RequirementsUnresolved,
        prompt_disposition: crate::protocol::StartupContextPromptDisposition::RolledBack,
        pending_input: Some(crate::protocol::ContextPendingInputMetadata::new(
            request_id,
            &expanded,
            images.len(),
        )),
        detail: "request blocked by required startup file".to_string(),
    };

    let mut stale = snapshot.clone();
    stale.compact.session_id = "stale-session".to_string();
    assert!(!app.accept_remote_startup_context_status(request_id, stale, Some(action.clone())));
    assert!(app.input.is_empty());
    assert_eq!(app.current_message_id, Some(request_id));

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    assert!(app.handle_server_event(
        crate::protocol::ServerEvent::StartupContextStatus {
            id: request_id,
            snapshot,
            action_required: Some(action),
        },
        &mut remote,
    ));
    assert_eq!(app.input, raw);
    assert_eq!(app.cursor_pos, 7);
    assert_eq!(app.pasted_contents, vec!["exact pasted body"]);
    assert_eq!(app.pending_images, images);
    assert_eq!(
        (app.copy_selection_anchor, app.copy_selection_cursor),
        selection
    );
    assert!(app.startup_context_overlay_scroll().is_some());
    assert!(!app.is_processing);
    assert!(app.current_message_id.is_none());
    assert!(
        app.display_messages()
            .iter()
            .all(|message| !(message.role == "user" && message.content == raw))
    );
    let display_len = app.display_messages().len();
    assert!(app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: request_id,
            message: "terminal wrapper error must remain hidden".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    ));
    assert_eq!(app.display_messages().len(), display_len);
    assert!(!app.consume_startup_context_terminal_error(request_id));

    let overlay = render_startup_context_fixture(&app, 38, 12);
    assert!(overlay.contains("Request not sent"));
    app.set_startup_context_details_scroll(8);
    let scrolled = render_startup_context_fixture(&app, 38, 12);
    assert!(scrolled.contains("MISSING.md"));
}

#[test]
fn startup_context_status_pages_merge_and_stale_request_ids_are_ignored() {
    let mut app = create_test_app();
    app.apply_startup_context_debug_fixture("blocked")
        .expect("blocked fixture");
    let session_id = app
        .startup_context_compact_status()
        .expect("status")
        .session_id
        .clone();
    let issue = app.startup_context_detail().expect("detail").issues[0].clone();
    let mut first = app.startup_context_detail().expect("detail").clone();
    first.total_issues = 2;
    first.issue_page_start = 0;
    first.issue_page_end = 1;
    first.next_issue_page_start = Some(1);
    first.issues = vec![issue.clone()];
    app.expect_startup_context_status_response_for_test(10, &session_id);
    assert!(!app.accept_remote_startup_context_status(9, first.clone(), None));
    assert!(app.accept_remote_startup_context_status(10, first, None));
    assert_eq!(app.startup_context_detail().unwrap().issues.len(), 1);

    let mut second = app.startup_context_detail().expect("detail").clone();
    second.issue_page_start = 1;
    second.issue_page_end = 2;
    second.next_issue_page_start = None;
    second.issues = vec![crate::protocol::StartupContextFileIssueSnapshot {
        input_index: Some(1),
        spec_id: Some("second".to_string()),
        logical_path: Some("docs/SECOND.md".to_string()),
        kind: crate::protocol::StartupContextFileIssueKind::Missing,
    }];
    app.expect_startup_context_status_response_for_test(11, &session_id);
    assert!(app.accept_remote_startup_context_status(11, second, None));
    assert_eq!(app.startup_context_detail().unwrap().issues.len(), 2);
}

#[test]
fn startup_context_debug_commands_and_help_are_registered_and_content_safe() {
    let mut app = create_test_app();
    let listed = app.handle_debug_command("startup-context-fixtures");
    for fixture in [
        "none",
        "ready",
        "blocked",
        "queued",
        "unsupported",
        "accepted",
    ] {
        assert!(listed.contains(fixture));
    }
    let response = app.handle_debug_command("startup-context-fixture:blocked-action");
    assert!(response.contains("\"ok\": true"));
    assert!(!response.contains("synthetic-image-data"));
    assert!(!response.contains("content\":"));
    let state = app.handle_debug_command("startup-context-state");
    assert!(state.contains("Blocked"));
    assert!(!state.contains("Synthetic request was not sent"));

    let help = app.command_help("startup").expect("Startup Context help");
    assert!(help.starts_with("/startup"));
    assert!(help.contains("blocking issues"));
    assert!(super::registered_command_entries().any(|(command, _)| command == "/startup"));
}
