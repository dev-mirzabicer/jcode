fn write_notification_fixture(home: &Path, id: &str, body: &str) {
    std::fs::write(
        home.join(format!("instructions/notifications/{id}.md")),
        format!("---\nid: {id}\nkind: notification\ntemplate: handlebars\n---\n{body}"),
    )
    .unwrap();
}

#[test]
fn managed_startup_occurrences_preserve_receipts_and_earlier_messages() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("JCODE_HOME", home.path());
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    write_notification_fixture(home.path(), "startup-context-initial", "INITIAL-SYNTHETIC");
    write_notification_fixture(
        home.path(),
        "startup-context-stale-changed",
        "OLD-SYNTHETIC",
    );
    let fixture = prepared_fixture(&[("A.md", "alpha")], &["A.md"]);
    let startup = StartupContext::from_durable_state_dir(fixture.root_path.join("state"));
    let mut session = Session::create(None, None);
    session.working_dir = Some(fixture.root_path.display().to_string());
    session
        .install_prepared_startup_context(fixture.outcome.clone())
        .unwrap();
    assert_eq!(
        text_block(&session.messages[0], 0),
        "<jcode_startup_context version=\"1\">\nINITIAL-SYNTHETIC\n</jcode_startup_context>"
    );
    session.mark_startup_context_dispatched().unwrap();
    let original = serde_json::to_value(&session.messages).unwrap();
    let original_len = session.messages.len();
    std::fs::write(fixture.root_path.join("A.md"), "beta").unwrap();
    assert_eq!(
        session
            .observe_startup_context_before_user_turn(&startup)
            .unwrap()
            .marker_count,
        1
    );
    let first = session.messages.last().unwrap().clone();
    assert_eq!(first.role, Role::User);
    assert_eq!(first.display_role, Some(StoredDisplayRole::System));
    assert!(first.timestamp.is_none());
    assert!(text_block(&first, 0).ends_with("\nOLD-SYNTHETIC\n</jcode_startup_file_stale>"));
    write_notification_fixture(
        home.path(),
        "startup-context-stale-changed",
        "NEW-SYNTHETIC",
    );
    std::fs::write(fixture.root_path.join("A.md"), "gamma").unwrap();
    assert_eq!(
        session
            .observe_startup_context_before_user_turn(&startup)
            .unwrap()
            .marker_count,
        1
    );
    assert!(
        text_block(session.messages.last().unwrap(), 0)
            .ends_with("\nNEW-SYNTHETIC\n</jcode_startup_file_stale>")
    );
    assert_eq!(
        serde_json::to_value(&session.messages[..original_len]).unwrap(),
        original
    );
    assert_eq!(
        serde_json::to_value(&session.messages[original_len]).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    let file = &session.startup_context.as_ref().unwrap().batches[0].files[0];
    assert_eq!(file.notification_count, 2);
    assert_eq!(file.stale_marker_message_ids.len(), 2);
    // Reaching the existing limit suppresses rendering, not just delivery.
    write_notification_fixture(home.path(), "startup-context-stale-changed", "{{invalid}}");
    std::fs::write(fixture.root_path.join("A.md"), "delta").unwrap();
    assert_eq!(
        session
            .observe_startup_context_before_user_turn(&startup)
            .unwrap()
            .marker_count,
        0
    );
}

#[test]
fn managed_startup_render_failure_rolls_back_and_empty_prose_still_delivers() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("JCODE_HOME", home.path());
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    let fixture = prepared_fixture(&[("A.md", "alpha")], &["A.md"]);
    let mut session = Session::create(None, None);
    session.working_dir = Some(fixture.root_path.display().to_string());
    write_notification_fixture(home.path(), "startup-context-initial", "{{invalid}}");
    let before = serde_json::to_value(&session).unwrap();
    assert!(matches!(
        session.install_prepared_startup_context(fixture.outcome.clone()),
        Err(StartupContextInstallError::Instruction(_))
    ));
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
    write_notification_fixture(home.path(), "startup-context-initial", "");
    session
        .install_prepared_startup_context(fixture.outcome.clone())
        .unwrap();
    assert_eq!(
        text_block(&session.messages[0], 0),
        "<jcode_startup_context version=\"1\">\n\n</jcode_startup_context>"
    );
    session.mark_startup_context_dispatched().unwrap();
    let startup = StartupContext::from_durable_state_dir(fixture.root_path.join("state"));
    let before = serde_json::to_value(&session).unwrap();
    write_notification_fixture(home.path(), "startup-context-stale-changed", "{{invalid}}");
    std::fs::write(fixture.root_path.join("A.md"), "beta").unwrap();
    assert!(matches!(
        session.observe_startup_context_before_user_turn(&startup),
        Err(StartupContextObservationError::Instruction(_))
    ));
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
    write_notification_fixture(home.path(), "startup-context-stale-changed", "");
    assert_eq!(
        session
            .observe_startup_context_before_user_turn(&startup)
            .unwrap()
            .marker_count,
        1
    );
    assert!(
        text_block(session.messages.last().unwrap(), 0)
            .ends_with(">\n\n</jcode_startup_file_stale>")
    );
}

#[test]
fn empty_startup_and_receipt_only_apply_do_not_render_control_prose() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("JCODE_HOME", home.path());
    let empty = prepared_fixture(&[], &[]);
    let mut session = Session::create(None, None);
    session.working_dir = Some(empty.root_path.display().to_string());
    session
        .install_prepared_startup_context(empty.outcome)
        .unwrap();
    assert!(!home.path().join("instructions").exists());
    let fixture = prepared_fixture(&[("A.md", "alpha")], &["A.md"]);
    let mut session = installed_dispatched_session(&fixture, &TestPersistence::default());
    let prepared = session
        .prepare_startup_context_session_apply("same-selection", fixture.outcome.clone())
        .unwrap();
    assert!(!home.path().join("instructions").exists());
    session
        .apply_prepared_startup_context_session(&prepared)
        .unwrap();
    assert_eq!(session.startup_context.as_ref().unwrap().batches.len(), 1);
}

#[test]
fn managed_late_control_is_rendered_once_before_prepared_apply() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("JCODE_HOME", home.path());
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    let fixture = prepared_fixture(&[("A.md", "alpha")], &["A.md"]);
    let mut session = installed_dispatched_session(&fixture, &TestPersistence::default());
    std::fs::write(fixture.root_path.join("B.md"), "beta").unwrap();
    let startup = StartupContext::from_durable_state_dir(fixture.root_path.join("state"));
    let project = startup.resolve_project(&fixture.root_path).unwrap();
    let preview = startup.preview_selection(
        &project,
        [
            StartupSelectionInput::new("A.md"),
            StartupSelectionInput::new("B.md"),
        ],
    );
    let outcome = startup
        .prepare_selection(&project, 8, &preview, StartupFailurePolicy::Block)
        .unwrap();
    write_notification_fixture(home.path(), "startup-context-update", "{{invalid}}");
    let before = serde_json::to_value(&session).unwrap();
    assert!(matches!(
        session.prepare_startup_context_session_apply("late", outcome.clone()),
        Err(StartupContextSessionApplyError::Instruction(_))
    ));
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
    write_notification_fixture(home.path(), "startup-context-update", "PREPARED-SYNTHETIC");
    let prepared = session
        .prepare_startup_context_session_apply("late", outcome)
        .unwrap();
    write_notification_fixture(home.path(), "startup-context-update", "LATER-SYNTHETIC");
    session
        .apply_prepared_startup_context_session(&prepared)
        .unwrap();
    let batch = &session.startup_context.as_ref().unwrap().batches[1];
    let control_index = session
        .messages
        .iter()
        .position(|message| message.id == batch.control_message_id)
        .unwrap();
    let message = &session.messages[control_index];
    assert_eq!(
        text_block(message, 0),
        "<jcode_startup_context_update version=\"1\">\nPREPARED-SYNTHETIC\n</jcode_startup_context_update>"
    );
    assert_eq!(message.role, Role::User);
    assert_eq!(message.display_role, Some(StoredDisplayRole::System));
    assert!(message.timestamp.is_none());
    assert_eq!(
        session.messages[control_index + 1].id,
        batch.files[0].message_id
    );
    let restored = Session::load(&session.id).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.messages).unwrap(),
        serde_json::to_value(&session.messages).unwrap()
    );
}
