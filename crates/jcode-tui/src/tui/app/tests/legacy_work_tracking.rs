use super::*;
use crate::config::feature_override::ScopedFeatureOverride;

/// Every retired command form, as typed. `/mission` and `/goal` keep their
/// separate rejection and are not listed here.
const RETIRED: &[&str] = &[
    "/commit",
    "/commit-push",
    "/commit-and-push",
    "/fast-release",
    "/cut-release",
    "/commit-push-release",
    "/fast-macos-release",
    "/remote-release",
    "/test",
    "/test synthetic claim",
    "/plan",
    "/plan synthetic goal",
    "/improve",
    "/improve plan",
    "/improve plan synthetic focus",
    "/improve status",
    "/improve stop",
    "/improve resume",
    "/initiatives",
    "/initiatives resume",
    "/initiatives show synthetic",
    "/goals",
    "/goals show synthetic",
];

/// The retirement notice, with any repeat counter the display adds removed.
fn last_system(app: &App) -> String {
    let message = app.display_messages().last().expect("a notice");
    assert_eq!(message.role, "system");
    match message.content.rsplit_once(" [×") {
        Some((notice, count)) if count.ends_with(']') => notice.to_string(),
        _ => message.content.clone(),
    }
}

#[test]
fn legacy_work_tracking_local_enter_claims_retired_commands_without_effects() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let mut app = create_test_app();
    app.session.improve_mode = Some(crate::session::SessionImproveMode::ImproveRun);
    let messages = serde_json::to_value(&app.session.messages).unwrap();
    let session = serde_json::to_value(&app.session).unwrap();
    let instructions = home.root().join("instructions").exists();
    for typed in RETIRED {
        app.input = (*typed).into();
        app.cursor_pos = app.input.len();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.input.is_empty(), "{typed}");
        assert!(!app.is_processing && !app.pending_turn, "{typed}");
        assert!(app.queued_messages.is_empty(), "{typed}");
        assert_eq!(
            last_system(&app),
            crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE,
            "{typed}"
        );
    }
    assert_eq!(
        serde_json::to_value(&app.session.messages).unwrap(),
        messages
    );
    assert_eq!(serde_json::to_value(&app.session).unwrap(), session);
    assert!(app.improve_mode.is_none());
    assert!(app.side_panel.pages.is_empty());
    assert!(!home.root().join("goals").exists());
    assert_eq!(home.root().join("instructions").exists(), instructions);
    // The separate mission rejection is unchanged.
    app.input = "/mission synthetic".into();
    app.cursor_pos = app.input.len();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(last_system(&app).contains("/mission and /goal"));
}

#[test]
fn legacy_work_tracking_remote_enter_sends_no_render_or_message_request() {
    use crossterm::event::KeyEvent;
    use tokio::io::AsyncBufReadExt;
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some("legacy-work-tracking-ui".into());
    let before = serde_json::to_value(&app.session.messages).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id("legacy-work-tracking-ui".into());
    remote.mark_history_loaded();
    let mut reader = tokio::io::BufReader::new(remote.take_dummy_peer().unwrap());
    runtime.block_on(async {
        for typed in RETIRED {
            app.input = (*typed).into();
            app.cursor_pos = app.input.len();
            crate::tui::app::remote::handle_remote_key_event(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut remote,
            )
            .await
            .unwrap();
            assert!(app.input.is_empty(), "{typed}");
            assert!(app.pending_workflow_commands.is_empty(), "{typed}");
            assert_eq!(
                last_system(&app),
                crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE,
                "{typed}"
            );
        }
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut line))
                .await
                .is_err(),
            "unexpected request: {line}"
        );
    });
    assert_eq!(serde_json::to_value(&app.session.messages).unwrap(), before);
    assert!(!home.root().join("goals").exists());
}

#[test]
fn legacy_work_tracking_restored_pending_preparation_cannot_dispatch() {
    use tokio::io::AsyncBufReadExt;
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some(app.session.id.clone());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id(app.session.id.clone());
    remote.mark_history_loaded();
    let mut reader = tokio::io::BufReader::new(remote.take_dummy_peer().unwrap());
    // Preparation saved by a build where the workflow was still available.
    app.pending_workflow_commands
        .push(crate::tui::app::commands_workflow::PendingCommand {
            command: crate::workflow::CommandWorkflow::Commit,
            original: "/commit".into(),
            session_id: app.session.id.clone(),
            working_dir: app.session.working_dir.clone(),
            cancelled: false,
            suspended: false,
            request_id: None,
            rendered: Some(Ok("PREPARED BODY".into())),
        });
    runtime.block_on(async {
        assert!(crate::tui::app::commands_workflow::poll(&mut app, &mut remote).await);
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut line))
                .await
                .is_err(),
            "unexpected request: {line}"
        );
    });
    assert!(app.pending_workflow_commands.is_empty());
    assert!(!app.is_processing);
    let last = app.display_messages().last().unwrap();
    assert_eq!(last.role, "error");
    assert!(
        last.content
            .contains(crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE)
    );
}

#[test]
fn legacy_work_tracking_discovery_help_and_overlay_omit_retired_commands() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let mut app = create_test_app();
    let retired_names = [
        "/commit",
        "/commit-push",
        "/fast-release",
        "/fast-macos-release",
        "/remote-release",
        "/test",
        "/plan",
        "/improve",
        "/initiatives",
        "/goals",
    ];
    for prefix in [
        "/", "/com", "/fa", "/rem", "/te", "/pl", "/im", "/in", "/go",
    ] {
        for (name, _) in app.get_suggestions_for(prefix) {
            let token = name.split_whitespace().next().unwrap_or_default();
            assert!(
                !crate::workflow::is_legacy_work_tracking_command(token),
                "{prefix} offered {name}"
            );
        }
    }
    for prefix in ["/improve ", "/goals ", "/goals show ", "/initiatives "] {
        assert!(app.get_suggestions_for(prefix).is_empty(), "{prefix}");
    }
    assert!(
        !crate::tui::app::state_ui_input_helpers::registered_command_entries().any(|(name, _)| {
            crate::workflow::is_legacy_work_tracking_command(
                name.split_whitespace().next().unwrap_or_default(),
            )
        })
    );
    for topic in retired_names {
        assert_eq!(
            app.command_help(topic).as_deref(),
            Some(crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE),
            "{topic}"
        );
    }
    app.help_scroll = Some(0);
    let backend = ratatui::backend::TestBackend::new(140, 400);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..400 {
        for x in 0..140 {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains("/transfer"), "help overlay did not render");
    for name in [
        "/improve",
        "/initiatives",
        "/test [claim]",
        "/plan [goal]",
        "/commit",
    ] {
        assert!(!text.contains(name), "help overlay lists {name}");
    }
    assert!(!home.root().join("goals").exists());
}

#[test]
fn legacy_work_tracking_saved_improve_mode_is_ignored_not_deleted() {
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let app = create_test_app();
    for (saved, restored) in [
        (crate::session::SessionImproveMode::ImproveRun, None),
        (crate::session::SessionImproveMode::ImprovePlan, None),
        (
            crate::session::SessionImproveMode::RefactorRun,
            Some(crate::tui::app::ImproveMode::RefactorRun),
        ),
    ] {
        let mut session = app.session.clone();
        session.improve_mode = Some(saved);
        session.save().unwrap();
        let reopened = App::new_minimal_with_session(
            std::sync::Arc::clone(&app.provider),
            app.registry.clone(),
            crate::session::Session::load(&session.id).unwrap(),
        );
        assert_eq!(reopened.improve_mode, restored, "{saved:?}");
        assert_eq!(
            crate::session::Session::load(&session.id)
                .unwrap()
                .improve_mode,
            Some(saved),
            "the saved mode is retained"
        );
    }
}

#[test]
fn legacy_work_tracking_active_mission_adds_no_turn_reminder_or_store_read() {
    let home = SkillTestHome::new();
    let mut app = create_test_app();
    {
        let _on = ScopedFeatureOverride::legacy_work_tracking(true);
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        crate::mission::set(&app.session.id, "SYNTHETIC MISSION", None).unwrap();
    }
    // Damaged mission prose would block a turn if the reminder were rendered.
    std::fs::write(
        home.path()
            .join("instructions/modules/mission-continuation.md"),
        "---\nid: mission-continuation\nkind: module\ntemplate: handlebars\n---\n{{missing}}",
    )
    .unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    app.input = "ORDINARY INPUT".into();
    app.cursor_pos = app.input.len();
    app.submit_input();
    assert!(app.is_processing, "the turn was blocked");
    let last_user = app
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == crate::message::Role::User)
        .expect("user turn");
    let text = serde_json::to_string(&last_user.content).unwrap();
    assert!(text.contains("ORDINARY INPUT"));
    assert!(!text.contains("SYNTHETIC MISSION"));
}
