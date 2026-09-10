use super::*;

struct AvailabilityGuard(Option<std::ffi::OsString>);

impl AvailabilityGuard {
    fn disabled() -> Self {
        let previous = std::env::var_os("JCODE_SWARM_ENABLED");
        crate::env::set_var("JCODE_SWARM_ENABLED", "false");
        crate::config::invalidate_config_cache();
        Self(previous)
    }
}

impl Drop for AvailabilityGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.0 {
            crate::env::set_var("JCODE_SWARM_ENABLED", value);
        } else {
            crate::env::remove_var("JCODE_SWARM_ENABLED");
        }
        crate::config::invalidate_config_cache();
    }
}

const RETIRED_COMMANDS: &[&str] = &[
    "/review",
    "/judge",
    "/autoreview on",
    "/autojudge on",
    "/refactor",
    "/refactor plan",
    "/triage",
    "/overnight 1h",
    "/swarm on",
    "/swarm status",
    "/swarm off",
    "/swarm-prompt",
    "/agent-models swarm",
    "/agents subagent",
    "/effort swarm",
    "/effort swarm-deep",
];

#[test]
fn swarm_retirement_legacy_startup_receipts_are_preserved_without_replaying_any_input() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = AvailabilityGuard::disabled();
    for (session, data) in [
        (
            "legacy-review",
            serde_json::json!({"input":"unsent user input","startup_display_message_title":"SYNTHETIC TITLE", "hidden_queued_system_messages":["SYNTHETIC INSTRUCTIONS"]}),
        ),
        (
            "legacy-overnight",
            serde_json::json!({"input":"unsent user input","queued_messages":["Overnight auto-poke for run `synthetic`. retained body"]}),
        ),
    ] {
        let path = home.root().join(format!("client-input-{session}"));
        let bytes = serde_json::to_vec(&data).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let restored = App::restore_input_for_reload(session).unwrap();
        assert!(!restored.submit_on_restore);
        assert!(
            restored.queued_messages.is_empty()
                && restored.hidden_queued_system_messages.is_empty()
        );
        assert!(restored.startup_display_message.is_some());
        assert!(!path.exists());
        let retained = std::fs::read_dir(home.root())
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("client-input-{session}.retired-swarm-"))
            })
            .unwrap()
            .path();
        assert_eq!(std::fs::read(retained).unwrap(), bytes);
    }
}

#[test]
fn swarm_retirement_legacy_automatic_flags_remain_dormant_without_rewriting_session_metadata() {
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = AvailabilityGuard::disabled();
    let mut app = create_test_app();
    app.autoreview_enabled = true;
    app.autojudge_enabled = true;
    app.session.autoreview_enabled = Some(true);
    app.session.autojudge_enabled = Some(true);
    app.session.improve_mode = Some(crate::session::SessionImproveMode::RefactorRun);
    let before = serde_json::to_value(&app.session).unwrap();
    crate::tui::app::commands::maybe_trigger_autoreview_local(&mut app);
    crate::tui::app::commands::maybe_trigger_autojudge_local(&mut app);
    assert!(!app.autoreview_enabled && !app.autojudge_enabled);
    assert!(!app.schedule_auto_poke_followup_if_needed());
    assert!(!app.schedule_overnight_poke_followup_if_needed());
    assert!(!app.pending_split_request && app.pending_split_workflow.is_none());
    assert_eq!(serde_json::to_value(&app.session).unwrap(), before);
}

#[test]
fn swarm_retirement_local_enter_and_discovery_preserve_session_and_dormant_sources() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = AvailabilityGuard::disabled();
    let mut app = create_test_app();
    let before = serde_json::to_value(&app.session.messages).unwrap();
    let system = app.session.system_prompt.clone();
    for command in RETIRED_COMMANDS {
        app.swarm_enabled = true; // Stale client state cannot exceed global availability.
        app.input = (*command).into();
        app.cursor_pos = app.input.len();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.swarm_enabled, "{command}");
        assert!(app.input.is_empty(), "{command}");
        assert!(app.inline_interactive_state.is_none(), "{command}");
    }
    assert_eq!(serde_json::to_value(&app.session.messages).unwrap(), before);
    assert_eq!(app.session.system_prompt, system);
    assert!(!home.root().join("swarm-prompt.md").exists());
    assert!(!home.root().join("config.toml").exists());
    for prefix in ["/swarm", "/swarm ", "/swarm-pro"] {
        assert!(
            !app.get_suggestions_for(prefix)
                .iter()
                .any(|(name, _)| name.starts_with("/swarm"))
        );
    }
    for prefix in ["/agent-models ", "/agents "] {
        assert!(
            !app.get_suggestions_for(prefix)
                .iter()
                .any(|(name, _)| name.ends_with(" swarm"))
        );
    }
    assert!(
        !crate::tui::app::state_ui_input_helpers::registered_command_entries()
            .any(|(name, _)| name.starts_with("/swarm"))
    );
    app.open_agents_picker();
    let picker = app.inline_interactive_state.as_ref().unwrap();
    assert!(!picker.entries.iter().any(|entry| matches!(
        entry.action,
        crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Swarm)
    )));
    app.inline_interactive_state = None;
    app.open_agent_model_picker(crate::tui::AgentModelTarget::Swarm);
    assert!(app.inline_interactive_state.is_none());
    let efforts =
        crate::tui::app::helpers::inferred_reasoning_efforts(Some("openai"), Some("gpt-5.4"));
    assert!(efforts.contains(&"high"));
    assert_eq!(
        crate::tui::app::helpers::effort_display_label("swarm-deep"),
        "Legacy effort (Swarm unavailable)"
    );
    assert!(
        !efforts
            .iter()
            .any(|effort| crate::prompt::is_swarm_effort(effort))
    );
    assert_eq!(
        app.command_help("swarm").as_deref(),
        Some(crate::config::SWARM_UNAVAILABLE)
    );
}

#[test]
fn swarm_retirement_remote_physical_enter_sends_no_enable_editor_or_effort_request() {
    use crossterm::event::KeyEvent;
    use tokio::io::AsyncBufReadExt;
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _off = AvailabilityGuard::disabled();
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some("retirement-ui".into());
    let before = serde_json::to_value(&app.session.messages).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id("retirement-ui".into());
    remote.mark_history_loaded();
    let peer = remote.take_dummy_peer().unwrap();
    let mut reader = tokio::io::BufReader::new(peer);
    runtime.block_on(async {
        for command in RETIRED_COMMANDS {
            app.swarm_enabled = true;
            app.input = (*command).into();
            app.cursor_pos = app.input.len();
            crate::tui::app::remote::handle_remote_key_event(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut remote,
            )
            .await
            .unwrap();
            assert!(!app.swarm_enabled, "{command}");
            assert!(app.input.is_empty(), "{command}");
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
}
