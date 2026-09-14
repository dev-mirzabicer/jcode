#[test]
fn task_monitor_local_physical_keys_and_paste_preserve_composer_and_session() {
    let _home=SkillTestHome::new();let mut app=create_test_app();
    let session=serde_json::to_value(&app.session).unwrap();
    app.input="/tasks".into();app.cursor_pos=app.input.len();app.handle_key(KeyCode::Enter,KeyModifiers::NONE).unwrap();
    assert!(app.task_ui.monitor.as_ref().unwrap().borrow().visible);
    app.input="PRESERVED DRAFT".into();app.cursor_pos=app.input.len();
    for key in ['x','z','?', 'j']{app.handle_key(KeyCode::Char(key),KeyModifiers::NONE).unwrap();}
    app.handle_paste("not composer input".into());assert_eq!(app.input,"PRESERVED DRAFT");
    app.handle_key(KeyCode::Esc,KeyModifiers::NONE).unwrap();app.handle_key(KeyCode::Esc,KeyModifiers::NONE).unwrap();
    assert!(!app.task_ui.monitor.as_ref().unwrap().borrow().visible);assert_eq!(serde_json::to_value(&app.session).unwrap(),session);
}

#[test]
fn task_monitor_remote_physical_dispatch_and_control_done_preserve_busy_parent() {
    use tokio::io::AsyncBufReadExt;
    let _home=SkillTestHome::new();let mut app=create_test_app();app.is_remote=true;app.runtime_mode=super::AppRuntimeMode::RemoteClient;app.remote_session_id=Some("parent-monitor".into());
    let runtime=tokio::runtime::Runtime::new().unwrap();let _entered=runtime.enter();
    let mut remote=crate::tui::backend::RemoteConnection::dummy();remote.set_session_id("parent-monitor".into());remote.mark_history_loaded();
    let peer=remote.take_dummy_peer().unwrap();let mut reader=tokio::io::BufReader::new(peer);
    runtime.block_on(async {
        app.input="/tasks".into();app.cursor_pos=app.input.len();app.handle_remote_key(KeyCode::Enter,KeyModifiers::NONE,&mut remote).await.unwrap();
        let mut line=String::new();tokio::time::timeout(Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        let crate::protocol::Request::TaskMonitorProbe{id}=serde_json::from_str(&line).unwrap() else{panic!("{line}")};
        assert!(app.handle_server_event(crate::protocol::ServerEvent::TaskMonitorCapabilities{id,version:1,child_context:true},&mut remote));
        app.dispatch_remote_task_requests(&mut remote).await;
        line.clear();tokio::time::timeout(Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        assert!(matches!(serde_json::from_str::<crate::protocol::Request>(&line).unwrap(),crate::protocol::Request::TaskMonitor{request:jcode_tool_types::task_monitor::TaskMonitorRequest::List{all_sessions:false,..},..}));
        let monitor=app.task_ui.monitor.as_ref().unwrap();monitor.borrow_mut().queued.push_back(crate::tui::task_monitor::Operation::Execution(jcode_tool_types::execution::ExecutionRequest::Stop{run_id:"run-selected".into()}));
        app.dispatch_remote_task_requests(&mut remote).await;
        line.clear();tokio::time::timeout(Duration::from_secs(2),reader.read_line(&mut line)).await.unwrap().unwrap();
        let crate::protocol::Request::Execution{id,..}=serde_json::from_str(&line).unwrap() else{panic!("{line}")};
        app.is_processing=true;app.current_message_id=None;app.status=ProcessingStatus::Streaming;
        app.handle_server_event(crate::protocol::ServerEvent::ExecutionResponse{id,response:jcode_tool_types::execution::ExecutionResponse::Control{run_id:"run-selected".into(),accepted:true,state:jcode_tool_types::RunState::Running}},&mut remote);
        app.handle_server_event(crate::protocol::ServerEvent::Done{id},&mut remote);assert!(app.is_processing,"Monitor Done must not finish a resumed parent turn");
        app.handle_remote_key(KeyCode::Char('x'),KeyModifiers::NONE,&mut remote).await.unwrap();assert!(app.input.is_empty());
    });
}

#[test]
fn task_monitor_child_editor_correlates_target_and_keeps_primary_protocol_separate() {
    use crate::protocol::{ServerEvent,Request};
    use crate::tui::context_editor::ContextEditorAction;
    let _home=SkillTestHome::new();let mut app=create_test_app();app.remote_session_id=Some("parent-monitor".into());
    app.context_protocol.accept_history("parent-monitor",17);let primary=app.context_protocol.test_signature();
    app.handle_task_command("/tasks");app.task_ui.monitor.as_ref().unwrap().borrow_mut().child_context=Some("child-target".into());app.open_requested_child_context();
    let request=app.prepare_context_editor_action(31,ContextEditorAction::LoadSnapshot{page_start:0,page_size:250});
    assert!(matches!(request.request,Request::ChildContext{child_id,..} if child_id=="child-target"));
    let mut snapshot=parity_snapshot();snapshot.session_id="child-target".into();
    let event=ServerEvent::ContextEditorSnapshot{id:31,snapshot};
    assert!(!app.reduce_task_event(ServerEvent::ChildContextResponse{id:31,child_id:"another-child".into(),event:Box::new(event.clone())}).unwrap());
    assert!(app.reduce_task_event(ServerEvent::ChildContextResponse{id:31,child_id:"child-target".into(),event:Box::new(event)}).unwrap());
    assert_eq!(app.context_protocol.test_signature(),primary);assert_eq!(app.task_ui.child.as_ref().unwrap().protocol.accepted_session_id.as_deref(),Some("child-target"));
    app.context_editor_actions.clear();app.handle_context_editor_key(KeyCode::Esc,KeyModifiers::NONE);assert!(app.task_ui.child.is_none());assert_eq!(app.context_protocol.test_signature(),primary);
}

#[test]
fn task_monitor_fast_reconnect_renegotiates_without_resetting_primary_editor() {
    let _home=SkillTestHome::new();let mut app=create_test_app();app.is_remote=true;app.runtime_mode=super::AppRuntimeMode::RemoteClient;app.remote_session_id=Some("parent-monitor".into());
    app.handle_task_command("/tasks");app.task_ui.monitor.as_ref().unwrap().borrow_mut().capability=Some(true);
    app.open_context_editor(crate::tui::context_editor::ContextEditorOpenMode::Edit);
    let generation=app.task_ui.monitor.as_ref().unwrap().borrow().debug()["generation"].as_u64().unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();let _entered=runtime.enter();
    let mut remote=crate::tui::backend::RemoteConnection::dummy();remote.set_session_id("parent-monitor".into());remote.mark_history_loaded();
    let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(80,24)).unwrap();
    let mut state=super::remote::RemoteRunState{reconnect_attempts:1,..Default::default()};
    runtime.block_on(super::remote::handle_post_connect(&mut app,&mut terminal,&mut remote,&mut state,Some("parent-monitor"))).unwrap();
    let monitor=app.task_ui.monitor.as_ref().unwrap().borrow();assert!(monitor.debug()["generation"].as_u64().unwrap()>generation);assert_eq!(monitor.capability,None);drop(monitor);
    assert!(app.context_editor_overlay.is_some());
    app.reconnect_task_monitor("another-parent");assert!(app.context_editor_overlay.is_some(),"A dormant monitor must not close the primary editor");
}

#[test]
fn task_monitor_active_local_dispatch_observes_real_store_without_waiting_for_parent() {
    let _home=SkillTestHome::new();let mut app=create_test_app();
    let root=crate::storage::jcode_dir().unwrap();let store=crate::execution::ExecutionStore::open(&root).unwrap();
    let crate::execution::PreparedInvocation::New(record)=store.prepare(&crate::execution::Invocation{session_id:app.session.id.clone(),message_id:"m".into(),call_path:vec!["active-local".into()],tool:"fixture".into(),input:serde_json::json!({}),working_dir:None,received_result_digest:None},"fixture-owner").unwrap() else{panic!()};
    store.start(&record.id,"fixture-owner").unwrap();
    app.is_processing=true;app.input="PRESERVE".into();app.cursor_pos=app.input.len();app.handle_task_command("/tasks");
    let primary=serde_json::to_value(&app.session).unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();runtime.block_on(async {
        app.dispatch_active_local_task_ui();
        let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
        loop {
            app.dispatch_active_local_task_ui();
            if app.task_ui.monitor.as_ref().unwrap().borrow().debug()["selected"]==record.id {break;}
            assert!(tokio::time::Instant::now()<deadline,"active local dispatcher never loaded its metadata");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(app.is_processing);assert_eq!(app.input,"PRESERVE");
        app.handle_key(KeyCode::Esc,KeyModifiers::NONE).unwrap();
        // Drain only already-owned fixture replies before releasing its store.
        tokio::time::sleep(Duration::from_millis(50)).await;app.dispatch_active_local_task_ui();
    });
    assert_eq!(serde_json::to_value(&app.session).unwrap(),primary);
    store.retain(record,crate::tool::ToolOutput::new("fixture done"),jcode_tool_types::RunState::Completed).unwrap();
}
