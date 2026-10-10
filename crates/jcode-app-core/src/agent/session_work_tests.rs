//! Session-work safe points through both real turn loops: a workflow file
//! changed outside the native file tools is restored before the next provider
//! request with one appended notice, and an unreadable store stops the turn.
use super::*;
use crate::provider::EventStream;
use std::sync::Mutex as StdMutex;

#[derive(Clone, Default)]
struct TextProvider {
    requests: Arc<StdMutex<Vec<Vec<Message>>>>,
}

#[async_trait::async_trait]
impl Provider for TextProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.requests.lock().unwrap().push(messages.to_vec());
        let events = vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            },
        ];
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
    fn name(&self) -> &str {
        "session-work-text"
    }
    fn model(&self) -> String {
        "session-work-text-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn restore_notices(session: &crate::session::Session) -> usize {
    session
        .messages
        .iter()
        .filter_map(crate::session::StoredMessage::context_delivery)
        .filter(|(channel, _)| *channel == jcode_session_types::ContextDeliveryChannel::SessionWork)
        .count()
}

fn request_mentions(provider: &TextProvider, index: usize, needle: &str) -> bool {
    let requests = provider.requests.lock().unwrap();
    serde_json::to_string(&requests[index])
        .unwrap()
        .contains(needle)
}

#[tokio::test]
async fn shell_changes_are_restored_before_requests_on_both_turn_loops() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    let _on = crate::config::feature_override::ScopedFeatureOverride::session_work(true);
    let project = tempfile::tempdir()?;
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(project.path().to_string_lossy().into_owned());
    assert!(crate::session_work::activate_new_session(
        &mut session,
        crate::session_work::NewSessionWork::Primary,
        &crate::instruction::InstructionRepositoryService::new(),
    )?);
    let surface = crate::session_work::SessionWorkSurface::new()?;
    surface.write_workflow(&session.id, "fixture", "- [>] a: A\n", chrono::Utc::now())?;
    let workflow = surface.workflow_path(&session.id)?;
    let provider = TextProvider::default();
    let shared: Arc<dyn Provider> = Arc::new(provider.clone());
    let registry = Registry::new(Arc::clone(&shared)).await;
    let mut agent = Agent::new_with_session(shared, registry, session, None);
    assert!(
        format!("{:?}", agent.session.messages).contains(&*workflow.to_string_lossy()),
        "the Session Context names the workflow file"
    );

    // Streaming loop.
    std::fs::write(&workflow, "- [x] a: edited by a shell\n")?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("first", Vec::new(), None, tx)
        .await?;
    assert_eq!(std::fs::read_to_string(&workflow)?, "- [>] a: A\n");
    assert_eq!(restore_notices(&agent.session), 1);
    assert!(request_mentions(&provider, 0, "restored to revision 1"));

    // Nothing changed: no further notice.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("second", Vec::new(), None, tx)
        .await?;
    assert_eq!(restore_notices(&agent.session), 1);

    // The blocking loop restores a deleted file the same way.
    std::fs::remove_file(&workflow)?;
    agent.run_once("third").await?;
    assert_eq!(std::fs::read_to_string(&workflow)?, "- [>] a: A\n");
    assert_eq!(restore_notices(&agent.session), 2);
    let requests = provider.requests.lock().unwrap().len();
    assert!(request_mentions(
        &provider,
        requests - 1,
        "restored to revision 1"
    ));

    // An unreadable store stops the turn before any request.
    let store = crate::session_work::SessionWorkStore::new();
    std::fs::write(
        store.path(),
        b"not a database, damaged on purpose for this test",
    )?;
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-shm"));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let error = agent
        .run_once_streaming_mpsc("fourth", Vec::new(), None, tx)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Session work is unavailable"),
        "{error:#}"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), requests);
    Ok(())
}

#[tokio::test]
async fn sessions_without_session_work_are_untouched() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let _runtime = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    let _off = crate::config::feature_override::ScopedFeatureOverride::session_work(false);
    let mut session = crate::session::Session::create(None, None);
    assert!(!crate::session_work::activate_new_session(
        &mut session,
        crate::session_work::NewSessionWork::Primary,
        &crate::instruction::InstructionRepositoryService::new(),
    )?);
    let provider = TextProvider::default();
    let shared: Arc<dyn Provider> = Arc::new(provider.clone());
    let registry = Registry::new(Arc::clone(&shared)).await;
    let mut agent = Agent::new_with_session(shared, registry, session, None);
    agent.run_once("hello").await?;
    assert_eq!(restore_notices(&agent.session), 0);
    assert!(!crate::session_work::SessionWorkStore::new().exists()?);
    assert!(!format!("{:?}", agent.session.messages).contains("session-work"));
    Ok(())
}
