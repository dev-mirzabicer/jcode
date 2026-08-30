use super::*;
use crate::message::{ContentBlock, Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use jcode_session_types::{StoredStartupBatchKind, StoredStartupContextState};
use std::ffi::OsString;

struct MockProvider;

struct FailingSessionPersistence;

impl StartupContextSessionPersistence for FailingSessionPersistence {
    fn persist(&self, _session: &mut Session) -> Result<()> {
        anyhow::bail!("injected WP-04 session persistence failure")
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("WP-04 coordinator tests never call the provider")
    }

    fn name(&self) -> &str {
        "wp04-mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    fn model(&self) -> String {
        "wp04-mock-model".to_string()
    }
}

struct TestEnv {
    previous_home: Option<OsString>,
    previous_runtime: Option<OsString>,
    home: tempfile::TempDir,
    runtime: tempfile::TempDir,
    state: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temporary Jcode home");
        let runtime = tempfile::tempdir().expect("temporary runtime");
        let state = tempfile::tempdir().expect("temporary Startup Context state");
        let project = tempfile::tempdir().expect("temporary project");
        let previous_home = std::env::var_os("JCODE_HOME");
        let previous_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        Self {
            previous_home,
            previous_runtime,
            home,
            runtime,
            state,
            project,
        }
    }

    fn coordinator(&self) -> StartupContextCoordinator {
        StartupContextCoordinator::for_test(
            self.state.path().to_path_buf(),
            "wp04-test",
            Duration::from_secs(30),
        )
    }

    fn working_dir(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if let Some(previous) = self.previous_home.take() {
            crate::env::set_var("JCODE_HOME", previous);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        if let Some(previous) = self.previous_runtime.take() {
            crate::env::set_var("JCODE_RUNTIME_DIR", previous);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
        let _ = (&self.home, &self.runtime);
    }
}

fn wire_selection(paths: &[&str]) -> Vec<StartupContextSelectionInput> {
    paths
        .iter()
        .map(|path| StartupContextSelectionInput {
            existing_spec_id: None,
            path: (*path).to_string(),
            approved_external_target: None,
        })
        .collect()
}

fn install_dispatched_session(
    coordinator: &StartupContextCoordinator,
    project_root: &Path,
    session_id: &str,
    selected: &[&str],
) -> Session {
    let project = coordinator
        .inner
        .engine
        .resolve_project(project_root)
        .expect("resolve project");
    let preview = coordinator.inner.engine.preview_selection(
        &project,
        selected.iter().map(|path| DomainSelectionInput::new(*path)),
    );
    let preparation = coordinator
        .inner
        .engine
        .prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)
        .expect("prepare initial Startup Context");
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    session.working_dir = Some(project_root.to_string_lossy().into_owned());
    session
        .install_prepared_startup_context(preparation)
        .expect("install initial Startup Context");
    session
        .mark_startup_context_dispatched()
        .expect("mark initial dispatch");
    assert!(matches!(
        session.mark_startup_context_provider_accepted(),
        crate::session::StartupContextAcceptanceOutcome::Persisted { .. }
    ));
    session
}

async fn agent_for(session: Session) -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )))
}

async fn opened_editor(
    coordinator: &StartupContextCoordinator,
    session_id: &str,
    connection_id: &str,
    working_dir: String,
) -> StartupContextEditorSnapshot {
    match coordinator
        .open_editor(
            session_id.to_string(),
            connection_id.to_string(),
            working_dir,
        )
        .await
        .expect("open Startup Context editor")
    {
        OpenEditorOutcome::Opened(editor) => editor,
        OpenEditorOutcome::Busy { .. } => panic!("test editor unexpectedly busy"),
    }
}

fn apply_request(
    editor: &StartupContextEditorSnapshot,
    session_id: &str,
    connection_id: &str,
    operation_id: &str,
    selection: Vec<StartupContextSelectionInput>,
    save_project_default: bool,
) -> ApplySelectionRequest {
    ApplySelectionRequest {
        lease: lease_request(
            editor.lease.lease_id.clone(),
            editor.project.key_digest.clone(),
            Some(editor.plan_revision),
            session_id.to_string(),
            connection_id.to_string(),
        ),
        operation_id: operation_id.to_string(),
        selection,
        save_project_default,
    }
}

fn editor_at_revision(
    editor: &StartupContextEditorSnapshot,
    revision: u64,
) -> StartupContextEditorSnapshot {
    let mut editor = editor.clone();
    editor.plan_revision = revision;
    editor.lease.plan_revision = revision;
    editor
}

fn project_plan_revision(coordinator: &StartupContextCoordinator, root: &Path) -> u64 {
    let project = coordinator.inner.engine.resolve_project(root).unwrap();
    coordinator
        .inner
        .engine
        .load_project_plan(&project)
        .unwrap()
        .plan()
        .revision()
}

fn exact_text_occurrences(session: &Session, expected: &str) -> usize {
    session
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter(|block| matches!(block, ContentBlock::Text { text, .. } if text == expected))
        .count()
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the transaction test serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn idle_combined_apply_is_atomic_idempotent_and_cleans_sensitive_recovery_material() {
    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    std::fs::write(env.project.path().join("A.md"), "alpha").unwrap();
    std::fs::write(env.project.path().join("B.md"), "WP04_SECRET_BETA_AT_APPLY").unwrap();
    let coordinator = env.coordinator();
    let session = install_dispatched_session(
        &coordinator,
        env.project.path(),
        "session-wp04-idle",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &coordinator,
        "session-wp04-idle",
        "connection-idle",
        env.working_dir(),
    )
    .await;
    let mixed_preview = coordinator
        .preview_selection(
            lease_request(
                editor.lease.lease_id.clone(),
                editor.project.key_digest.clone(),
                Some(0),
                "session-wp04-idle".to_string(),
                "connection-idle".to_string(),
            ),
            wire_selection(&["A.md", "missing.md"]),
        )
        .await
        .expect("mixed preview returns per-entry issues rather than failing wholesale");
    assert_eq!(mixed_preview.selected_count, 1);
    assert_eq!(mixed_preview.issue_count, 1);
    assert!(mixed_preview.aggregate_estimated_tokens > 0);
    let spoofed_identity = coordinator
        .preview_selection(
            lease_request(
                editor.lease.lease_id.clone(),
                editor.project.key_digest.clone(),
                Some(0),
                "session-wp04-idle".to_string(),
                "connection-idle".to_string(),
            ),
            vec![StartupContextSelectionInput {
                existing_spec_id: Some("b".repeat(64)),
                path: "A.md".to_string(),
                approved_external_target: None,
            }],
        )
        .await
        .expect_err("wire identity cannot be rebound to a normalized path");
    assert_eq!(
        spoofed_identity.kind,
        StartupContextFailureKind::InvalidRequest
    );
    let selection = wire_selection(&["A.md", "B.md"]);
    let preview = coordinator
        .preview_selection(
            lease_request(
                editor.lease.lease_id.clone(),
                editor.project.key_digest.clone(),
                Some(0),
                "session-wp04-idle".to_string(),
                "connection-idle".to_string(),
            ),
            selection.clone(),
        )
        .await
        .expect("preview complete selection");
    assert_eq!(preview.selected_count, 2);
    assert_eq!(preview.issue_count, 0);
    assert!(preview.aggregate_estimated_tokens > 0);

    let request = apply_request(
        &editor,
        "session-wp04-idle",
        "connection-idle",
        "operation-idle-combined",
        selection.clone(),
        true,
    );
    let status = coordinator
        .apply_selection(request.clone(), Arc::clone(&agent), false)
        .await
        .expect("apply combined transition");
    assert_eq!(status.phase, StartupContextApplyPhase::Succeeded);
    assert!(matches!(
        status.session_target,
        StartupContextApplyTargetState::Applied { .. }
    ));
    assert!(matches!(
        status.project_default_target,
        StartupContextApplyTargetState::Applied { revision: Some(1) }
    ));
    assert_eq!(
        coordinator
            .lock_state()
            .leases
            .get(&editor.project.key_digest)
            .unwrap()
            .plan_revision,
        1
    );

    let duplicate = coordinator
        .apply_selection(request, Arc::clone(&agent), false)
        .await
        .expect("replay duplicate operation");
    assert_eq!(duplicate, status);

    let mut editor_v1 = editor.clone();
    editor_v1.plan_revision = 1;
    editor_v1.lease.plan_revision = 1;
    let future_only = coordinator
        .apply_selection(
            apply_request(
                &editor_v1,
                "session-wp04-idle",
                "connection-idle",
                "operation-future-only-reorder",
                wire_selection(&["B.md", "A.md"]),
                true,
            ),
            Arc::clone(&agent),
            false,
        )
        .await
        .expect("apply future-only reorder");
    assert_eq!(future_only.phase, StartupContextApplyPhase::Succeeded);
    assert_eq!(
        future_only.session_target,
        StartupContextApplyTargetState::Unchanged
    );
    assert!(matches!(
        future_only.project_default_target,
        StartupContextApplyTargetState::Applied { revision: Some(2) }
    ));
    let guard = agent.lock().await;
    let receipt = guard
        .startup_context_session()
        .startup_context
        .as_ref()
        .unwrap();
    assert_eq!(receipt.batches.len(), 2);
    assert_eq!(receipt.batches[1].kind, StoredStartupBatchKind::Late);
    let last = guard.startup_context_session().messages.last().unwrap();
    assert!(matches!(
        &last.content[1],
        ContentBlock::Text { text, .. } if text == "WP04_SECRET_BETA_AT_APPLY"
    ));
    drop(guard);

    let project = coordinator
        .inner
        .engine
        .resolve_project(env.project.path())
        .unwrap();
    let plan = coordinator
        .inner
        .engine
        .load_project_plan(&project)
        .unwrap();
    assert_eq!(plan.plan().revision(), 2);
    assert_eq!(plan.plan().entries().len(), 2);

    let record_path = coordinator.apply_record_path("operation-idle-combined");
    let record_text = std::fs::read_to_string(&record_path).unwrap();
    assert!(!record_text.contains("WP04_SECRET_BETA_AT_APPLY"));
    assert!(!record_path.with_extension("bak").exists());
    std::fs::write(
        record_path.with_extension("bak"),
        "synthetic stale captured recovery material",
    )
    .unwrap();
    coordinator.recover_interrupted_transactions();
    assert!(!record_path.with_extension("bak").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&record_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the transaction test serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn busy_apply_captures_at_drain_and_cancel_or_capture_failure_preserves_session() {
    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    std::fs::write(env.project.path().join("A.md"), "alpha").unwrap();
    std::fs::write(env.project.path().join("B.md"), "before-queue").unwrap();
    std::fs::write(env.project.path().join("C.md"), "cancel-me").unwrap();
    std::fs::write(env.project.path().join("D.md"), "delete-before-drain").unwrap();
    let coordinator = env.coordinator();
    let session = install_dispatched_session(
        &coordinator,
        env.project.path(),
        "session-wp04-busy",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &coordinator,
        "session-wp04-busy",
        "connection-busy",
        env.working_dir(),
    )
    .await;

    let queued = coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-busy",
                "connection-busy",
                "operation-queued",
                wire_selection(&["A.md", "B.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect("queue busy apply");
    assert_eq!(queued.phase, StartupContextApplyPhase::Queued);
    assert_eq!(coordinator.pending_apply_count("session-wp04-busy"), 1);
    let second_pending = coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-busy",
                "connection-busy",
                "operation-second-pending",
                wire_selection(&["A.md", "C.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect_err("one session may not accumulate ambiguous pending applies");
    assert_eq!(
        second_pending.kind,
        StartupContextFailureKind::OperationConflict
    );
    std::fs::write(env.project.path().join("B.md"), "captured-at-safe-drain").unwrap();
    let drained = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].phase, StartupContextApplyPhase::Succeeded);
    let guard = agent.lock().await;
    let last = guard.startup_context_session().messages.last().unwrap();
    assert!(matches!(
        &last.content[1],
        ContentBlock::Text { text, .. } if text == "captured-at-safe-drain"
    ));
    let established_batch_count = guard
        .startup_context_session()
        .startup_context
        .as_ref()
        .unwrap()
        .batches
        .len();
    drop(guard);

    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-busy",
                "connection-busy",
                "operation-cancel",
                wire_selection(&["A.md", "B.md", "C.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect("queue cancelable apply");
    let canceled = coordinator
        .cancel_apply(
            lease_request(
                editor.lease.lease_id.clone(),
                editor.project.key_digest.clone(),
                Some(0),
                "session-wp04-busy".to_string(),
                "connection-busy".to_string(),
            ),
            "operation-cancel",
        )
        .expect("cancel queued apply");
    assert_eq!(canceled.phase, StartupContextApplyPhase::Canceled);

    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-busy",
                "connection-busy",
                "operation-invalid-at-drain",
                wire_selection(&["A.md", "B.md", "D.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect("queue eventually invalid apply");
    std::fs::remove_file(env.project.path().join("D.md")).unwrap();
    let drained = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].phase, StartupContextApplyPhase::Failed);
    let guard = agent.lock().await;
    assert_eq!(
        guard
            .startup_context_session()
            .startup_context
            .as_ref()
            .unwrap()
            .batches
            .len(),
        established_batch_count
    );
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the recovery test serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn restart_recovery_converges_from_queued_prepared_plan_and_session_commit_stages() {
    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    std::fs::write(env.project.path().join("A.md"), "alpha").unwrap();
    std::fs::write(env.project.path().join("B.md"), "recovery-beta").unwrap();
    let coordinator = env.coordinator();
    let session = install_dispatched_session(
        &coordinator,
        env.project.path(),
        "session-wp04-recovery",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &coordinator,
        "session-wp04-recovery",
        "connection-recovery",
        env.working_dir(),
    )
    .await;

    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-recovery",
                "connection-recovery",
                "operation-restart",
                wire_selection(&["A.md", "B.md"]),
                true,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect("persist queued recovery intent");

    let mut record = coordinator
        .load_apply_record("operation-restart")
        .expect("load queued record");
    let session_snapshot = agent.lock().await.startup_context_session().clone();
    record = coordinator
        .prepare_record_for_session(record, &session_snapshot)
        .expect("persist prepared session transition");
    assert!(record.prepared_session.is_some());
    let prepared_record_text =
        std::fs::read_to_string(coordinator.apply_record_path("operation-restart")).unwrap();
    assert!(prepared_record_text.contains("recovery-beta"));

    let project = coordinator.resolve_record_project(&record).unwrap();
    coordinator
        .inner
        .engine
        .commit_project_plan_transition(&project, record.plan_transition.as_ref().unwrap())
        .expect("simulate crash after plan commit");
    drop(coordinator);

    let restarted = env.coordinator();
    let statuses = {
        let mut guard = agent.lock().await;
        restarted.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].phase, StartupContextApplyPhase::Succeeded);
    assert!(
        !std::fs::read_to_string(restarted.apply_record_path("operation-restart"))
            .unwrap()
            .contains("recovery-beta")
    );

    let final_session = agent.lock().await.startup_context_session().clone();
    assert_eq!(
        final_session.startup_context.as_ref().unwrap().state,
        StoredStartupContextState::ProviderAccepted
    );
    assert_eq!(
        final_session
            .startup_context
            .as_ref()
            .unwrap()
            .batches
            .len(),
        2
    );
    let persisted = Session::load("session-wp04-recovery").expect("reload recovered session");
    assert_eq!(persisted.startup_context.as_ref().unwrap().batches.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the ownership test serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn apply_claim_pins_live_lease_and_recovery_guard_serializes_named_coordinators() {
    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    std::fs::write(env.project.path().join("A.md"), "alpha").unwrap();
    std::fs::write(env.project.path().join("B.md"), "beta").unwrap();
    let first = StartupContextCoordinator::for_test(
        env.state.path().to_path_buf(),
        "first",
        Duration::from_millis(20),
    );
    let second = StartupContextCoordinator::for_test(
        env.state.path().to_path_buf(),
        "second",
        Duration::from_secs(30),
    );
    let session = install_dispatched_session(
        &first,
        env.project.path(),
        "session-wp04-ownership",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &first,
        "session-wp04-ownership",
        "connection-ownership",
        env.working_dir(),
    )
    .await;

    let claim = first
        .claim_apply("synthetic-pin", &editor.project.key_digest)
        .expect("claim active apply");
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(first.expire_abandoned_leases(), 0);
    drop(claim);
    assert_eq!(first.expire_abandoned_leases(), 1);

    let reopened = opened_editor(
        &first,
        "session-wp04-ownership",
        "connection-ownership",
        env.working_dir(),
    )
    .await;
    first
        .apply_selection(
            apply_request(
                &reopened,
                "session-wp04-ownership",
                "connection-ownership",
                "operation-cross-server-guard",
                wire_selection(&["A.md", "B.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .expect("queue operation under first coordinator");
    first
        .close_editor(lease_request(
            reopened.lease.lease_id.clone(),
            reopened.project.key_digest.clone(),
            Some(reopened.plan_revision),
            "session-wp04-ownership".to_string(),
            "connection-ownership".to_string(),
        ))
        .expect("release editor before recovery ownership test");

    let record = second
        .load_apply_record("operation-cross-server-guard")
        .expect("load shared durable apply record");
    let guard = second
        .acquire_project_guard_for_record(&record)
        .expect("second coordinator acquires recovery guard")
        .expect("no live editor means a temporary guard is required");
    let contention = match first.acquire_project_guard_for_record(&record) {
        Ok(_) => panic!("first coordinator must observe second coordinator ownership"),
        Err(error) => error,
    };
    assert_eq!(contention.kind, StartupContextFailureKind::LeaseBusy);
    drop(guard);

    let drained = {
        let mut agent = agent.lock().await;
        first.drain_pending_for_agent(&mut agent)
    };
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].phase, StartupContextApplyPhase::Succeeded);
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the persistence matrix serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn persistence_failure_matrix_converges_without_silent_partial_success() {
    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    for (name, text) in [
        ("A.md", "alpha"),
        ("B.md", "record-preparation"),
        ("C.md", "plan-record-stage"),
        ("D.md", "session-write-stage"),
        ("E.md", "plan-write-stage"),
        ("F.md", "final-record-stage"),
    ] {
        std::fs::write(env.project.path().join(name), text).unwrap();
    }
    let coordinator = env.coordinator();
    let session = install_dispatched_session(
        &coordinator,
        env.project.path(),
        "session-wp04-failure-matrix",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &coordinator,
        "session-wp04-failure-matrix",
        "connection-failure-matrix",
        env.working_dir(),
    )
    .await;

    // Recovery intent write failure happens before any target is touched.
    let before_intent = serde_json::to_vec(agent.lock().await.startup_context_session()).unwrap();
    coordinator.fail_apply_record_save_on_nth(1);
    let intent_error = coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-failure-matrix",
                "connection-failure-matrix",
                "operation-intent-write-failure",
                wire_selection(&["A.md", "B.md"]),
                false,
            ),
            Arc::clone(&agent),
            false,
        )
        .await
        .expect_err("intent persistence failure must reject the apply");
    assert_eq!(intent_error.kind, StartupContextFailureKind::Recovery);
    assert!(
        !coordinator
            .apply_record_path("operation-intent-write-failure")
            .exists()
    );
    assert_eq!(
        serde_json::to_vec(agent.lock().await.startup_context_session()).unwrap(),
        before_intent
    );
    assert_eq!(project_plan_revision(&coordinator, env.project.path()), 0);

    // A failure while persisting complete prepared material leaves only the
    // path-only queued intent. A later drain recaptures and completes safely.
    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-failure-matrix",
                "connection-failure-matrix",
                "operation-prepared-record-failure",
                wire_selection(&["A.md", "B.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .unwrap();
    coordinator.fail_apply_record_save_on_nth(1);
    let first_prepared_drain = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert!(first_prepared_drain.is_empty());
    let queued = coordinator
        .load_apply_record("operation-prepared-record-failure")
        .unwrap();
    assert_eq!(queued.phase, StartupContextApplyPhase::Queued);
    assert!(queued.prepared_session.is_none());
    let prepared_retry = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(prepared_retry[0].phase, StartupContextApplyPhase::Succeeded);
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "record-preparation"
        ),
        1
    );

    // Crash/failure after the plan write but before its target-state record is
    // recoverable because the plan transition itself is idempotent.
    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-failure-matrix",
                "connection-failure-matrix",
                "operation-plan-record-failure",
                wire_selection(&["A.md", "B.md", "C.md"]),
                true,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .unwrap();
    let mut plan_record = coordinator
        .load_apply_record("operation-plan-record-failure")
        .unwrap();
    let snapshot = agent.lock().await.startup_context_session().clone();
    plan_record = coordinator
        .prepare_record_for_session(plan_record, &snapshot)
        .unwrap();
    let before_plan_record_batches = snapshot.startup_context.as_ref().unwrap().batches.len();
    coordinator.fail_apply_record_save_on_nth(1);
    assert_eq!(
        coordinator
            .commit_record(
                &mut plan_record,
                agent.lock().await.startup_context_session_mut(),
            )
            .expect_err("plan target-state record failure must remain recoverable")
            .kind,
        StartupContextFailureKind::Recovery
    );
    assert_eq!(project_plan_revision(&coordinator, env.project.path()), 1);
    assert_eq!(
        agent
            .lock()
            .await
            .startup_context_session()
            .startup_context
            .as_ref()
            .unwrap()
            .batches
            .len(),
        before_plan_record_batches
    );
    let plan_record_retry = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(
        plan_record_retry[0].phase,
        StartupContextApplyPhase::Succeeded
    );
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "plan-record-stage"
        ),
        1
    );

    // Session save failure after the project default commits reports both
    // target states truthfully and completes forward on retry.
    let editor_v1 = editor_at_revision(&editor, 1);
    coordinator
        .apply_selection(
            apply_request(
                &editor_v1,
                "session-wp04-failure-matrix",
                "connection-failure-matrix",
                "operation-session-write-failure",
                wire_selection(&["A.md", "B.md", "C.md", "D.md"]),
                true,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .unwrap();
    let mut session_record = coordinator
        .load_apply_record("operation-session-write-failure")
        .unwrap();
    let snapshot = agent.lock().await.startup_context_session().clone();
    session_record = coordinator
        .prepare_record_for_session(session_record, &snapshot)
        .unwrap();
    let before_session_failure = serde_json::to_vec(&snapshot).unwrap();
    let session_error = coordinator
        .commit_record_with(
            &mut session_record,
            agent.lock().await.startup_context_session_mut(),
            &FailingSessionPersistence,
        )
        .expect_err("session persistence failure must remain recoverable");
    assert_eq!(session_error.kind, StartupContextFailureKind::Recovery);
    assert_eq!(project_plan_revision(&coordinator, env.project.path()), 2);
    assert_eq!(
        serde_json::to_vec(agent.lock().await.startup_context_session()).unwrap(),
        before_session_failure
    );
    assert!(matches!(
        session_record.project_default_target,
        StartupContextApplyTargetState::Applied { revision: Some(2) }
    ));
    assert!(matches!(
        session_record.session_target,
        StartupContextApplyTargetState::Failed { .. }
    ));
    coordinator
        .commit_record(
            &mut session_record,
            agent.lock().await.startup_context_session_mut(),
        )
        .unwrap();
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "session-write-stage"
        ),
        1
    );

    // Final recovery-record failure occurs after the session target is durable.
    // Replaying the old prepared record recognizes the operation identity and
    // never appends the late batch twice.
    let editor_v2 = editor_at_revision(&editor, 2);
    coordinator
        .apply_selection(
            apply_request(
                &editor_v2,
                "session-wp04-failure-matrix",
                "connection-failure-matrix",
                "operation-final-record-failure",
                wire_selection(&["A.md", "B.md", "C.md", "D.md", "E.md", "F.md"]),
                false,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .unwrap();
    let mut final_record = coordinator
        .load_apply_record("operation-final-record-failure")
        .unwrap();
    let snapshot = agent.lock().await.startup_context_session().clone();
    final_record = coordinator
        .prepare_record_for_session(final_record, &snapshot)
        .unwrap();
    coordinator.fail_apply_record_save_on_nth(1);
    assert_eq!(
        coordinator
            .commit_record(
                &mut final_record,
                agent.lock().await.startup_context_session_mut(),
            )
            .expect_err("final record failure must remain recoverable")
            .kind,
        StartupContextFailureKind::Recovery
    );
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "final-record-stage"
        ),
        1
    );
    let mut replay = coordinator
        .load_apply_record("operation-final-record-failure")
        .unwrap();
    coordinator
        .commit_record(
            &mut replay,
            agent.lock().await.startup_context_session_mut(),
        )
        .unwrap();
    assert_eq!(replay.phase, StartupContextApplyPhase::Succeeded);
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "final-record-stage"
        ),
        1
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "the plan-write test serializes process-global JCODE_HOME and JCODE_RUNTIME_DIR overrides"
)]
async fn project_plan_write_failure_is_a_session_noop_and_retries_forward() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::storage::lock_test_env();
    let env = TestEnv::new();
    std::fs::write(env.project.path().join("A.md"), "alpha").unwrap();
    std::fs::write(env.project.path().join("B.md"), "plan-write-retry").unwrap();
    let coordinator = env.coordinator();
    let session = install_dispatched_session(
        &coordinator,
        env.project.path(),
        "session-wp04-plan-write",
        &["A.md"],
    );
    let agent = agent_for(session).await;
    let editor = opened_editor(
        &coordinator,
        "session-wp04-plan-write",
        "connection-plan-write",
        env.working_dir(),
    )
    .await;
    coordinator
        .apply_selection(
            apply_request(
                &editor,
                "session-wp04-plan-write",
                "connection-plan-write",
                "operation-plan-write",
                wire_selection(&["A.md", "B.md"]),
                true,
            ),
            Arc::clone(&agent),
            true,
        )
        .await
        .unwrap();
    let mut record = coordinator
        .load_apply_record("operation-plan-write")
        .unwrap();
    let snapshot = agent.lock().await.startup_context_session().clone();
    record = coordinator
        .prepare_record_for_session(record, &snapshot)
        .unwrap();
    let before = serde_json::to_vec(&snapshot).unwrap();

    let startup_root = env.state.path().join("startup-context");
    assert!(!startup_root.join("projects").exists());
    let original_mode = std::fs::metadata(&startup_root)
        .unwrap()
        .permissions()
        .mode();
    std::fs::set_permissions(&startup_root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = coordinator.commit_record(
        &mut record,
        agent.lock().await.startup_context_session_mut(),
    );
    std::fs::set_permissions(
        &startup_root,
        std::fs::Permissions::from_mode(original_mode),
    )
    .unwrap();

    assert_eq!(
        result
            .expect_err("project plan write must fail under an unwritable private namespace")
            .kind,
        StartupContextFailureKind::Recovery
    );
    assert_eq!(project_plan_revision(&coordinator, env.project.path()), 0);
    assert_eq!(
        serde_json::to_vec(agent.lock().await.startup_context_session()).unwrap(),
        before
    );
    let retry = {
        let mut guard = agent.lock().await;
        coordinator.drain_pending_for_agent(&mut guard)
    };
    assert_eq!(retry[0].phase, StartupContextApplyPhase::Succeeded);
    assert_eq!(project_plan_revision(&coordinator, env.project.path()), 1);
    assert_eq!(
        exact_text_occurrences(
            agent.lock().await.startup_context_session(),
            "plan-write-retry"
        ),
        1
    );
}
