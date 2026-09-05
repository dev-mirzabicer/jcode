type QueuedRecordedRequests = Vec<(Vec<Message>, String)>;

#[derive(Clone, Default)]
struct QueuedNoticeRecordingProvider {
    requests: Arc<std::sync::Mutex<QueuedRecordedRequests>>,
}
#[async_trait]
impl Provider for QueuedNoticeRecordingProvider {
    async fn complete(&self, messages: &[Message], _tools: &[ToolDefinition], system: &str, _resume: Option<&str>) -> Result<EventStream> {
        self.requests.lock().unwrap().push((messages.to_vec(), system.to_string()));
        Ok(Box::pin(tokio_stream::iter(vec![Ok(StreamEvent::TextDelta("fixture done".into())), Ok(StreamEvent::MessageEnd { stop_reason: Some("end_turn".into()) })])))
    }
    fn name(&self) -> &str { "test" }
    fn fork(&self) -> Arc<dyn Provider> { Arc::new(self.clone()) }
}

#[tokio::test]
async fn queued_notice_uses_recipient_project_and_persists_exact_history() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let service = crate::instruction::InstructionRepositoryService::new();
    crate::instruction::SystemPromptComposer::new().ensure_global_store().unwrap();
    let project = tempfile::tempdir().unwrap();
    let configured = service.configure_non_git_project(project.path(), "queued-fixture", None, &crate::instruction::shipped_instruction_seed().unwrap(), &[]).unwrap();
    let source = configured.repository.root.join("notifications/todo-auto-poke.md");
    let write = |body: &str| std::fs::write(&source, format!("---\nid: todo-auto-poke\nkind: notification\ntemplate: handlebars\n---\n{body}")).unwrap();
    write("PROJECT-OLD {{count}}");
    let provider = QueuedNoticeRecordingProvider::default();
    let provider_dyn: Arc<dyn Provider> = Arc::new(provider.clone());
    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    agent.set_working_dir(project.path().to_str().unwrap());
    let entries = vec![crate::todo::QueuedMessage::todo(crate::todo::TodoNoticeRequest::Incomplete { count: 2 }), crate::todo::QueuedMessage::from("HUMAN-SENTINEL")];
    agent.run_queued_capture(&entries).await.unwrap();
    let original = agent.session.messages.iter().find(|message| message.origin_parts().is_some()).unwrap().clone();
    assert!(matches!(&original.content[0], ContentBlock::Text { text, .. } if text == "PROJECT-OLD 2\n\nHUMAN-SENTINEL"));
    write("PROJECT-NEW {{count}}");
    agent.run_queued_capture(&entries).await.unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].1, requests[1].1);
    let first = requests[0].0.iter().find(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text { text, .. } if text.contains("PROJECT-OLD 2\n\nHUMAN-SENTINEL")))).expect("first provider request includes complete queued text");
    assert!(requests[1].0.iter().any(|message| serde_json::to_value(&message.content).unwrap() == serde_json::to_value(&first.content).unwrap()), "earlier provider-visible bytes, including owner timestamps, stay exact");
    assert!(requests[1].0.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text { text, .. } if text.contains("PROJECT-NEW 2\n\nHUMAN-SENTINEL")))));
    drop(requests);
    let restored = Session::load(agent.session_id()).unwrap();
    assert_eq!(serde_json::to_value(restored.messages.iter().find(|message|message.id==original.id).unwrap()).unwrap(), serde_json::to_value(&original).unwrap());
    let before = serde_json::to_value(&agent.session.messages).unwrap();
    write("{{invalid}}");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(agent.run_queued_streaming_mpsc(Some(44), &entries, None, tx).await.is_err());
    assert!(matches!(rx.try_recv().unwrap(), ServerEvent::QueuedMessagesRejected { id:44, .. }));
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(serde_json::to_value(&agent.session.messages).unwrap(), before);
    write("");
    assert!(agent.run_queued_capture(&entries).await.is_err());
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(serde_json::to_value(&agent.session.messages).unwrap(), before);
    crate::config::Config::invalidate_cache();
}
