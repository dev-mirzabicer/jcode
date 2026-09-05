#[test]
fn typed_remote_queue_preserves_intent_without_reading_client_instruction_sources() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore { fn drop(&mut self) { match self.0.take() { Some(old) => crate::env::set_var("JCODE_HOME", old), None => crate::env::remove_var("JCODE_HOME") }; crate::config::Config::invalidate_cache(); } }
    let _restore = Restore(std::env::var_os("JCODE_HOME"));
    crate::env::set_var("JCODE_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let mut app = create_test_app();
    let had_store = home.path().join("instructions").exists();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    std::fs::create_dir_all(home.path().join("project/.jcode")).unwrap();
    std::fs::write(home.path().join("project/.jcode/instructions.toml"), "invalid project configuration").unwrap();
    app.session.working_dir = Some(home.path().join("project").display().to_string());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    let peer = remote.take_dummy_peer().unwrap();
    remote.mark_history_loaded();
    let entries: crate::todo::QueuedMessages = vec![
        crate::todo::QueuedMessage::todo(crate::todo::TodoNoticeRequest::Incomplete { count: 2 }),
        crate::todo::QueuedMessage::from("HUMAN-SENTINEL"),
    ].into();
    let id = runtime.block_on(super::input_dispatch::begin_remote_queued_send(&mut app, &mut remote, entries.clone(), None, 0, false)).unwrap();
    let request = runtime.block_on(async {
        use tokio::io::AsyncBufReadExt;
        let mut reader = tokio::io::BufReader::new(peer);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str::<crate::protocol::Request>(&line).unwrap()
    });
    let crate::protocol::Request::QueuedMessages { entries: sent, observe_startup_context, .. } = request else { panic!("typed controls must use typed request"); };
    assert_eq!(sent, entries.clone().into_entries());
    assert!(!observe_startup_context);
    assert_eq!(app.rate_limit_pending_message.as_ref().unwrap().queued_messages.as_ref(), Some(&entries));
    assert_eq!(home.path().join("instructions").exists(), had_store);
    app.handle_server_event(ServerEvent::QueuedMessagesRejected { id, message: "synthetic server-side source failure".into() }, &mut remote);
    app.handle_server_event(ServerEvent::Error { id, message: "synthetic terminal error".into(), retry_after_secs: None }, &mut remote);
    assert_eq!(app.queued_messages, entries);
    assert!(app.queued_instruction_error.is_some());
    assert!(!app.is_processing);
    assert!(app.rate_limit_pending_message.is_none());
    assert!(matches!(crate::tui::app::commands::activate_auto_poke(&mut app), crate::tui::app::commands::PokeActivation::Queued));
    assert_eq!(app.queued_messages, entries);
    assert!(app.queued_instruction_error.is_none());
    app.save_input_for_reload("typed-queue-fixture");
    let restored = crate::tui::app::App::restore_input_for_reload("typed-queue-fixture").unwrap();
    assert_eq!(restored.queued_messages, entries);
    let damaged = home.path().join("client-input-invalid-queue-fixture");
    std::fs::write(&damaged, r#"{"input":"preserved user input","queued_messages":[{"kind":"todo","request":{"kind":"unknown"}}]}"#).unwrap();
    assert!(crate::tui::app::App::restore_input_for_reload("invalid-queue-fixture").is_none());
    assert!(damaged.exists());
}
