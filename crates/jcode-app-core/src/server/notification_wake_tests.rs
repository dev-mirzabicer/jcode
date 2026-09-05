#[tokio::test]
async fn managed_wake_turn_renders_recipient_scope_and_fails_visibly_before_dispatch() {
    use super::live_turn::{LiveTurnReminder, LiveTurnSwarmContext, run_live_turn_if_idle};
    use crate::instruction::notification::Notification;
    use crate::instruction::{
        InstructionRepositoryService, SystemPromptComposer, shipped_instruction_seed,
    };
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = ScopedEnvVar::set("JCODE_HOME", home.path());
    SystemPromptComposer::new().ensure_global_store().unwrap();
    let project = tempfile::tempdir().unwrap();
    let service = InstructionRepositoryService::new();
    let configured = service
        .configure_non_git_project(
            project.path(),
            "wake-fixture",
            None,
            &shipped_instruction_seed().unwrap(),
            &[],
        )
        .unwrap();
    let source = configured
        .repository
        .root
        .join("notifications/background-task-completed.md");
    let write_source = |body: &str| {
        std::fs::write(&source, format!("---\nid: background-task-completed\nkind: notification\ntemplate: handlebars\n---\n{body}")).unwrap()
    };
    write_source("{{unknown}}");
    let provider = Arc::new(StreamingMockProvider::default());
    provider.queue_response(vec![
        StreamEvent::TextDelta("synthetic completion".into()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let agent = test_agent(provider.clone()).await;
    agent
        .lock()
        .await
        .set_working_dir(project.path().to_str().unwrap());
    let session_id = agent.lock().await.session_id().to_string();
    let before_count = agent.lock().await.message_count();
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        agent.clone(),
    )])));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let members = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        attached_swarm_member(&session_id, tx),
    )])));
    let (swarms, history, counter, events) = empty_swarm_status_state();
    let context = || LiveTurnSwarmContext::new(&members, &swarms, &history, &counter, &events);
    assert!(
        run_live_turn_if_idle(
            &session_id,
            "synthetic task result",
            Some(LiveTurnReminder::Managed(
                Notification::BackgroundTaskCompleted
            )),
            &sessions,
            context()
        )
        .await
    );
    let error = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(ServerEvent::Error { message, .. }) = rx.recv().await {
                break message;
            }
        }
    })
    .await
    .unwrap();
    assert!(error.contains("background-task-completed"));
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(agent.lock().await.message_count(), before_count);
    assert_eq!(members.read().await[&session_id].status, "failed");
    // New occurrence uses current project content rather than the global seed
    // or a cached failed render. Member metadata deliberately has no cwd.
    write_source("PROJECT-SYNTHETIC");
    assert!(
        run_live_turn_if_idle(
            &session_id,
            "synthetic task result",
            Some(LiveTurnReminder::Managed(
                Notification::BackgroundTaskCompleted
            )),
            &sessions,
            context()
        )
        .await
    );
    timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Some(ServerEvent::Done { .. }) => break,
                Some(ServerEvent::Error { message, .. }) => {
                    panic!("unexpected wake failure: {message}")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].iter().flat_map(|message| &message.content).any(|block| matches!(block, crate::message::ContentBlock::Text { text, .. } if text.contains("PROJECT-SYNTHETIC"))));
    drop(requests);
    // A busy recipient does not render a wake-only reminder or consume work.
    write_source("{{invalid}}");
    let _busy = agent.lock().await;
    assert!(
        !run_live_turn_if_idle(
            &session_id,
            "not dispatched",
            Some(LiveTurnReminder::Managed(
                Notification::BackgroundTaskCompleted
            )),
            &sessions,
            context()
        )
        .await
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}
