#[test]
fn workspace_manager_local_command_traps_keys_and_paste_without_touching_session() {
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    let before = serde_json::to_value(&app.session).unwrap();
    let messages = app.messages.len();
    app.input = "/workspace".into();
    app.cursor_pos = app.input.len();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.workspace_manager_visible());
    assert!(app.input.is_empty());
    // A private local client cannot administer the shared runtime; it says so.
    let state = app.workspace_debug();
    assert_eq!(state["remote"], false);
    app.input = "PRESERVED".into();
    app.cursor_pos = app.input.len();
    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    app.handle_paste("NOT A COMPOSER PASTE".into());
    assert_eq!(app.input, "PRESERVED", "manager owns physical input while visible");
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(!app.workspace_manager_visible());
    app.input = "/runtime".into();
    app.cursor_pos = app.input.len();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert_eq!(app.workspace_debug()["section"], "runtime");
    app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE).unwrap();
    assert!(!app.workspace_manager_visible());
    app.input = "/workspace bogus".into();
    app.cursor_pos = app.input.len();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.workspace_manager_visible(), "unknown section shows usage instead");
    assert!(app.display_messages.iter().any(|m| m.content.contains("/niri")));
    assert_eq!(app.messages.len(), messages);
    assert_eq!(serde_json::to_value(&app.session).unwrap(), before);
}

#[test]
fn workspace_manager_remote_physical_keys_send_only_management_requests() {
    use crossterm::event::{KeyEvent, KeyEventKind};
    use tokio::io::AsyncBufReadExt;
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = super::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some("workspace-fixture".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id("workspace-fixture".into());
    remote.mark_history_loaded();
    let peer = remote.take_dummy_peer().unwrap();
    let mut reader = tokio::io::BufReader::new(peer);
    runtime.block_on(async {
        app.input = "/workspace".into();
        app.cursor_pos = app.input.len();
        super::remote::handle_remote_key_event(
            &mut app,
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Press),
            &mut remote,
        )
        .await
        .unwrap();
        let mut probes = Vec::new();
        for _ in 0..4 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            probes.push(serde_json::from_str::<crate::protocol::Request>(&line).unwrap());
        }
        let workspace_probe = probes
            .iter()
            .find_map(|r| match r {
                crate::protocol::Request::WorkspaceProbe { id } => Some(*id),
                _ => None,
            })
            .expect("workspace probe");
        assert!(probes.iter().any(|r| matches!(r, crate::protocol::Request::RuntimeProbe { .. })));
        assert!(probes.iter().all(|r| !matches!(r, crate::protocol::Request::Message { .. })));
        // A foreign id with the same event kind is not consumed by the manager.
        assert!(app.reduce_workspace_event(crate::protocol::ServerEvent::WorkspaceCapabilities {
            id: workspace_probe + 1000,
            catalog_version: 1,
            permissions_version: Some(1),
            checkout_version: Some(1),
            closeout_version: Some(2),
            management_version: Some(1),
            managed_rollout: false,
        }).is_err());
        // Transport acknowledgement keeps the correlation; the typed reply completes it.
        app.handle_server_event(crate::protocol::ServerEvent::Ack { id: workspace_probe }, &mut remote);
        app.handle_server_event(
            crate::protocol::ServerEvent::WorkspaceCapabilities {
                id: workspace_probe,
                catalog_version: 1,
                permissions_version: Some(1),
                checkout_version: Some(1),
                closeout_version: Some(2),
                management_version: Some(1),
                managed_rollout: false,
            },
            &mut remote,
        );
        assert_eq!(app.workspace_debug()["capabilities"]["management"], true);
        super::remote::handle_tick(&mut app, &mut remote).await;
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let request: crate::protocol::Request = serde_json::from_str(&line).unwrap();
        let crate::protocol::Request::Workspace { id, request } = request else {
            panic!("{request:?}")
        };
        assert!(matches!(*request, crate::workspace::WorkspaceRequest::Status {}));
        app.handle_server_event(
            crate::protocol::ServerEvent::WorkspaceResponse {
                id,
                response: Box::new(crate::workspace::WorkspaceResponse::Error(crate::workspace::Issue {
                    code: crate::workspace::IssueCode::RecoveryRequired,
                    detail: "Workspace is not initialized".into(),
                })),
            },
            &mut remote,
        );
        app.input.clear();
        // Enhanced keyboard reporting delivers Shift+i as 'i' plus SHIFT.
        super::remote::handle_remote_key_event(&mut app, KeyEvent::new(KeyCode::Char('i'), KeyModifiers::SHIFT), &mut remote).await.unwrap();
        assert_eq!(app.workspace_debug()["confirm"]["title"], "Initialize the workspace catalog");
        assert!(app.input.is_empty(), "management keys never reach the composer");
        super::remote::handle_remote_key_event(&mut app, KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE), &mut remote).await.unwrap();
        assert!(app.workspace_debug()["confirm"].is_null(), "declined review sends nothing: {}", app.workspace_debug());
        assert!(app.input.is_empty());
    });
}

#[test]
fn workspace_runtime_opens_while_disconnected_instead_of_queueing_a_message() {
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = super::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some("offline-fixture".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let queued = app.queued_messages.len();
    app.input = "/runtime".into();
    app.cursor_pos = app.input.len();
    super::remote::handle_disconnected_key_event(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.queued_messages.len(), queued, "not queued as a model message");
    assert!(app.workspace_manager_visible());
    let state = app.workspace_debug();
    assert_eq!(state["connected"], false);
    assert_eq!(state["section"], "runtime");
    assert!(state["actions"].as_array().unwrap().iter().any(|a| a == "S Start runtime"));
    assert!(state["runtime"]["offline"].is_string(), "durable intent read without starting anything");
    assert!(app.handle_workspace_key_disconnected(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.workspace_manager_visible());
    app.input = "an ordinary message".into();
    app.cursor_pos = app.input.len();
    assert!(!app.open_workspace_while_disconnected());
}

#[test]
fn scoped_context_rejection_is_visible_and_ends_the_pending_launch() {
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    app.is_remote = true;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    app.pending_split_label = Some("Split".into());
    app.is_processing = true;
    app.handle_server_event(
        crate::protocol::ServerEvent::ScopedContextRejected {
            id: 7,
            source_session: "source".into(),
            issue: crate::workspace::Issue {
                code: crate::workspace::IssueCode::Conflict,
                detail: "Grant-carry review is stale".into(),
            },
        },
        &mut remote,
    );
    assert!(app.pending_split_label.is_none());
    assert!(app.display_messages.iter().any(|m| m.content.contains("Grant-carry review is stale") && m.content.contains("unchanged")));
}
