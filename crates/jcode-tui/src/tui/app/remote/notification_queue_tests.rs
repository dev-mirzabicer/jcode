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

#[test]
fn durable_legacy_input_recovery_preserves_original_without_automatic_replay() {
    let _environment=crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let home=crate::storage::jcode_dir().unwrap();
    let original=serde_json::json!({"input":"uncertain Ω", "cursor":5,"pending_images":[{"media_type":"image/png","data":"complete"}],"submit_on_restore":true,"queued_messages":["also uncertain"],"rate_limit_reset_in_ms":0});
    let bytes=serde_json::to_vec(&original).unwrap();
    let path=home.join("client-input-legacy-fixture");
    std::fs::write(&path,&bytes).unwrap();
    let restored=crate::tui::app::App::restore_input_for_reload("legacy-fixture").unwrap();
    assert!(!restored.submit_on_restore && restored.queued_messages.is_empty());
    assert_eq!(restored.input,"uncertain Ω");
    assert_eq!(restored.pending_images,vec![("image/png".into(),"complete".into())]);
    let retained=std::fs::read_dir(&home).unwrap().filter_map(Result::ok).map(|entry|entry.path()).find(|path|path.file_name().unwrap().to_string_lossy().starts_with("client-input-legacy-fixture.legacy-")).unwrap();
    assert_eq!(std::fs::read(retained).unwrap(),bytes);
    crate::client_input::save_startup_submission_for_session("fresh-fixture","new intent".into(),vec![("image/png".into(),"image".into())]);
    let fresh=crate::tui::app::App::restore_input_for_reload("fresh-fixture").unwrap();
    assert!(fresh.submit_on_restore);
    assert_eq!(fresh.pending_images.len(),1);
}

#[test]
fn durable_remote_image_path_enters_interleave_before_slash_routing() {
    let _environment=crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let image=crate::storage::jcode_dir().unwrap().join("fixture.png");
    std::fs::write(&image,b"synthetic image bytes").unwrap();
    let mut app=create_test_app(); app.is_remote=true; app.is_processing=true;
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        app.input=image.to_string_lossy().into_owned(); app.cursor_pos=app.input.len();
        let mut remote=crate::tui::backend::RemoteConnection::dummy();
        let peer=remote.take_dummy_peer().unwrap(); remote.mark_history_loaded();
        super::handle_remote_key(&mut app,crossterm::event::KeyCode::Enter,crossterm::event::KeyModifiers::NONE,&mut remote).await.unwrap();
        use tokio::io::AsyncBufReadExt;
        let mut reader=tokio::io::BufReader::new(peer); let mut line=String::new();
        tokio::time::timeout(std::time::Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        let request:crate::protocol::Request=serde_json::from_str(&line).unwrap();
        let crate::protocol::Request::SoftInterrupt{images,..}=request else {panic!("image path must not be a slash command");};
        assert_eq!(images.len(),1); assert_eq!(images[0].0,"image/png");
    });
}
