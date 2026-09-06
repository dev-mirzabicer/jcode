#[test]
fn instruction_manager_local_commands_and_modal_keys_do_not_touch_session_instructions() {
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    let before = serde_json::to_value(&app.session).unwrap();
    let messages = app.messages.len();
    for command in ["/instructions", "/prompts", "/model-roster", "/agent instructions", "/skills instructions", "/swarm-prompt inspect"] {
        app.input = command.into(); app.cursor_pos = app.input.len();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.instruction_manager_visible(), "{command}");
        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE).unwrap();
        app.handle_key(KeyCode::Char('界'), KeyModifiers::NONE).unwrap();
        assert!(app.input.is_empty());
        assert_eq!(app.instruction_ui.manager.as_ref().unwrap().borrow().filter.search, "界");
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(!app.instruction_manager_visible());
    }
    assert_eq!(app.messages.len(), messages);
    assert_eq!(serde_json::to_value(&app.session).unwrap(), before);
}

#[test]
fn instruction_manager_remote_physical_enter_keys_and_replies_use_server_authority() {
    use tokio::io::AsyncBufReadExt;
    use crossterm::event::{KeyEvent, KeyEventKind};
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = super::AppRuntimeMode::RemoteClient;
    app.remote_session_id = Some("inspection-fixture".into());
    let original_system = app.session.system_prompt.clone();
    let original_messages = app.messages.len();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id("inspection-fixture".into());
    remote.mark_history_loaded();
    let peer = remote.take_dummy_peer().unwrap();
    let mut reader = tokio::io::BufReader::new(peer);
    runtime.block_on(async {
        app.input = "/instructions".into(); app.cursor_pos = app.input.len();
        super::remote::handle_remote_key_event(&mut app, KeyEvent::new_with_kind(KeyCode::Enter,KeyModifiers::NONE,KeyEventKind::Press), &mut remote).await.unwrap();
        let mut line=String::new();
        tokio::time::timeout(Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        let request:crate::protocol::Request=serde_json::from_str(&line).unwrap();
        let crate::protocol::Request::InspectInstructions {id,request:crate::protocol::InstructionInspectionRequest::Open {..}}=request else {panic!("{request:?}")};
        let snapshot=crate::protocol::InstructionInspectionSnapshot {snapshot:"authoritative".into(),session_id:"inspection-fixture".into(),active_agent:Some("global:fixture".into()),repositories:vec![],resources:crate::protocol::InstructionRowsPage {offset:0,total:1,next:None,rows:vec![crate::protocol::InstructionRow {key:"server-resource".into(),id:"fixture".into(),name:"Server".into(),kind:"agent".into(),scope:"project".into(),repository:"project".into(),origin:crate::protocol::InstructionOrigin::Managed,effective:true,valid:true,warning:None}]}};
        assert!(app.handle_server_event(crate::protocol::ServerEvent::InstructionInspection {id,reply:Box::new(crate::protocol::InstructionInspectionReply {session_id:"inspection-fixture".into(),snapshot:Some("authoritative".into()),result:crate::protocol::InstructionInspectionResult::Opened(snapshot)})},&mut remote));
        super::remote::handle_remote_key_event(&mut app,KeyEvent::new(KeyCode::Char('1'),KeyModifiers::NONE),&mut remote).await.unwrap();
        line.clear();tokio::time::timeout(Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        let request:crate::protocol::Request=serde_json::from_str(&line).unwrap();
        assert!(matches!(request,crate::protocol::Request::InspectInstructions {request:crate::protocol::InstructionInspectionRequest::Detail {target:crate::protocol::InstructionInspectionTarget::Resource(ref key),..},..} if key=="server-resource"));
        super::remote::handle_remote_key_event(&mut app,KeyEvent::new(KeyCode::Char('/'),KeyModifiers::NONE),&mut remote).await.unwrap();
        super::remote::handle_remote_key_event(&mut app,KeyEvent::new(KeyCode::Char('a'),KeyModifiers::NONE),&mut remote).await.unwrap();
        assert!(app.input.is_empty());
        app.reconnect_instruction_manager("new-authority");
        assert!(app.instruction_ui.manager.as_ref().unwrap().borrow().pending.is_none());
        assert_eq!(app.instruction_ui.manager.as_ref().unwrap().borrow().session,"new-authority");
    });
    assert_eq!(app.session.system_prompt,original_system);
    assert_eq!(app.messages.len(),original_messages);
}

#[test]
fn instruction_manager_history_updates_preserve_detail_but_reconnect_refreshes() {
    let _home = SkillTestHome::new();
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("inspection-history".into());
    assert!(app.handle_instruction_command("/instructions"));
    {
        let mut manager = app.instruction_ui.manager.as_ref().unwrap().borrow_mut();
        manager.queued = None;
        manager.filter.search = "retained filter".into();
        manager.text = Some(crate::protocol::InstructionTextPage { document:"detail".into(),title:"Fixture".into(),offset:0,total_bytes:4,next:None,text:"BODY".into() });
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let history = || serde_json::from_value::<crate::protocol::ServerEvent>(serde_json::json!({"type":"history","id":1,"session_id":"inspection-history","messages":[],"server_has_update":false})).unwrap();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.set_session_id("inspection-history".into()); remote.mark_history_loaded();
    app.handle_server_event(history(), &mut remote);
    assert!(app.instruction_ui.manager.as_ref().unwrap().borrow().text.is_some());
    assert!(app.instruction_ui.manager.as_ref().unwrap().borrow().queued.is_none());
    let mut reconnected = crate::tui::backend::RemoteConnection::dummy();
    reconnected.set_session_id("inspection-history".into());
    app.handle_server_event(history(), &mut reconnected);
    let manager = app.instruction_ui.manager.as_ref().unwrap().borrow();
    assert!(manager.text.is_none());
    assert!(matches!(manager.queued, Some(crate::protocol::InstructionInspectionRequest::Open { .. })));
    assert_eq!(manager.filter.search, "retained filter");
}
