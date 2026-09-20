use super::*;

#[test]
#[cfg(target_os = "macos")]
fn location_survives_checkpoint_journal_stub_and_continuation() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path().join("state"));
    let cwd = temp.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    let binding = crate::location::volume::LocationResolver::new()
        .bind_directory(&cwd)
        .unwrap();
    let mut session = Session::create(None, None);
    session.working_dir = Some(binding.observed_path().to_str().unwrap().into());
    assert!(session.location.is_none());
    session.location = Some(StoredSessionLocation {
        placement: jcode_workspace_types::Placement::Standalone(
            jcode_workspace_types::LocationId::new(),
        ),
        initial_cwd: binding.observed_path().into(),
        cwd: binding,
        revision: 1,
        last_operation: Some(jcode_workspace_types::OperationId::new()),
    });
    session.save().unwrap();
    let original = session.location.clone();
    let messages = serde_json::to_vec(&session.messages).unwrap();
    assert_eq!(Session::load(&session.id).unwrap().location, original);
    assert_eq!(
        Session::load_startup_stub(&session.id).unwrap().location,
        original
    );
    session.append_stored_message(StoredMessage {
        id: crate::id::new_id("message"),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "synthetic location persistence input".into(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
        origin: None,
    });
    session.save().unwrap();
    assert_eq!(Session::load(&session.id).unwrap().location, original);
    assert_eq!(
        Session::load_startup_stub(&session.id).unwrap().location,
        original
    );
    let before = session.journal_meta();
    session.location.as_mut().unwrap().revision = 2;
    session.location.as_mut().unwrap().placement =
        jcode_workspace_types::Placement::Project(jcode_workspace_types::ProjectId::new());
    assert!(journal::metadata_requires_snapshot(
        &before,
        &session.journal_meta()
    ));
    session.save().unwrap();
    assert_eq!(
        Session::load(&session.id).unwrap().location,
        session.location
    );
    assert_eq!(
        Session::load_startup_stub(&session.id).unwrap().location,
        session.location
    );
    let mut child = Session::create(Some(session.id.clone()), None);
    child.inherit_continuation_state_from(&session);
    assert_eq!(child.location, session.location);
    assert_eq!(
        serde_json::to_vec(&child.messages).unwrap(),
        serde_json::to_vec(&session.messages).unwrap()
    );
    let mut legacy = serde_json::to_value(&session).unwrap();
    legacy.as_object_mut().unwrap().remove("location");
    let decoded: Session = serde_json::from_value(legacy).unwrap();
    assert!(decoded.location.is_none());
    assert_eq!(
        serde_json::to_vec(&decoded.messages).unwrap(),
        serde_json::to_vec(&session.messages).unwrap()
    );
    assert_eq!(messages, b"[]");
    session.require_published_primary().unwrap();
    let saved = std::fs::read(session_path(&session.id).unwrap()).unwrap();
    std::fs::rename(&cwd, temp.path().join("moved-work")).unwrap();
    assert!(session.require_published_primary().is_err());
    std::fs::create_dir(&cwd).unwrap();
    assert!(session.require_published_primary().is_err());
    assert_eq!(
        std::fs::read(session_path(&session.id).unwrap()).unwrap(),
        saved
    );
}
