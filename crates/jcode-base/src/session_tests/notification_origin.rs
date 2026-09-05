use jcode_session_types::{StoredMessageOrigin, StoredMessagePart, TodoNoticeKind};

#[test]
fn typed_control_origin_preserves_provider_bytes_and_mixed_user_text_through_history() {
    let _lock = lock_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", home.path());
    let control = "SYNTHETIC CONTROL 日本語";
    let human = "USER-TEXT-SENTINEL";
    let text = format!("{control}\n\n{human}");
    let origin = StoredMessageOrigin::Composed(vec![
        StoredMessagePart { start: 0, end: control.len(), notice: Some(TodoNoticeKind::Ownership), incomplete_count: None },
        StoredMessagePart { start: control.len()+2, end: text.len(), notice: None, incomplete_count: None },
    ]);
    let mut session = Session::create(None,None);
    session.messages.clear();
    let id = session.add_user_message_with_origin(vec![ContentBlock::Text {text:text.clone(),cache_control:None}],None,Some(origin.clone())).unwrap();
    let message = session.messages.last().unwrap();
    let mut legacy = message.clone();
    legacy.origin = None;
    assert_eq!(serde_json::to_value(message.to_message()).unwrap(),serde_json::to_value(legacy.to_message()).unwrap());
    assert_eq!(message.role,Role::User);
    assert!(message.display_role.is_none());
    assert!(message.timestamp.is_some());
    let (rendered,_) = render_messages_and_images(&session);
    assert_eq!(rendered.len(),2);
    assert_eq!(rendered[0].role,"system");
    assert_eq!(rendered[1].role,"user");
    assert_eq!(rendered[1].content,human);
    session.save().unwrap();
    session.add_human_message(vec![ContentBlock::Text{text:"later user".into(),cache_control:None}]);
    session.save().unwrap();
    let loaded = Session::load(&session.id).unwrap();
    assert_eq!(loaded.messages.iter().find(|message|message.id==id).unwrap().origin,Some(origin));
    let mut child = Session::create(Some(session.id.clone()),None);
    child.inherit_continuation_state_from(&loaded);
    assert_eq!(serde_json::to_value(&child.messages).unwrap(),serde_json::to_value(&loaded.messages).unwrap());
    let (rendered,_) = render_messages_and_images(&loaded);
    assert!(rendered.iter().any(|message|message.role=="user" && message.content==human));
}

#[test]
fn invalid_origin_cannot_hide_user_text_and_explicit_human_text_is_not_legacy_control() {
    let mut session=Session::create(None,None);
    session.messages.clear();
    let text=crate::todo::TODO_OWNERSHIP_CONTINUATION_MESSAGE.to_string();
    session.add_human_message(vec![ContentBlock::Text{text:text.clone(),cache_control:None}]);
    let (rendered,_)=render_messages_and_images(&session);
    assert_eq!(rendered[0].role,"user");
    assert_eq!(rendered[0].content,text);
    let before=serde_json::to_value(&session.messages).unwrap();
    let invalid=StoredMessageOrigin::Composed(vec![StoredMessagePart{start:1,end:usize::MAX,notice:Some(TodoNoticeKind::Ownership),incomplete_count:None}]);
    assert!(session.add_user_message_with_origin(vec![ContentBlock::Text{text:"日本語".into(),cache_control:None}],None,Some(invalid.clone())).is_err());
    assert_eq!(serde_json::to_value(&session.messages).unwrap(),before);
    session.messages[0].origin=Some(invalid);
    let (rendered,_)=render_messages_and_images(&session);
    assert_eq!(rendered[0].role,"user");
    assert_eq!(rendered[0].content,text);
}

#[test]
fn export_redaction_rebases_mixed_origin_without_mutating_source() {
    let mut session=Session::create(None,None);
    session.messages.clear();
    let control="SYNTHETIC CONTROL";
    let human="OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789\nUSER-AFTER-SENTINEL";
    let text=format!("{control}\n\n{human}");
    session.add_user_message_with_origin(vec![ContentBlock::Text{text:text.clone(),cache_control:None}],None,Some(StoredMessageOrigin::Composed(vec![
        StoredMessagePart{start:0,end:control.len(),notice:Some(TodoNoticeKind::Ownership),incomplete_count:None},
        StoredMessagePart{start:control.len()+2,end:text.len(),notice:None,incomplete_count:None},
    ]))).unwrap();
    let before=serde_json::to_value(&session.messages).unwrap();
    let exported=session.redacted_for_export();
    assert_eq!(serde_json::to_value(&session.messages).unwrap(),before);
    assert!(exported.messages[0].origin_parts().is_some(), "origin={:?}, content={:?}", exported.messages[0].origin, exported.messages[0].content);
    let (rendered,_)=render_messages_and_images(&exported);
    assert!(rendered.iter().any(|message|message.role=="user" && message.content.contains("USER-AFTER-SENTINEL")));
    assert!(!rendered.iter().any(|message|message.content.contains("abcdefghijklmnopqrstuvwxyz0123456789")));
}
