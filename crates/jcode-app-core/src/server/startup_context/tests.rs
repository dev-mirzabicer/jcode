use super::*;
use jcode_base::startup_context::{StartupFailurePolicy, StartupSelectionInput};
use std::process::{Command, Stdio};
use std::thread;

struct Fixture {
    state: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            state: tempfile::tempdir().expect("temporary durable state"),
            project: tempfile::tempdir().expect("temporary project"),
        }
    }

    fn coordinator(&self, name: &str) -> StartupContextCoordinator {
        StartupContextCoordinator::for_test(
            self.state.path().to_path_buf(),
            name,
            Duration::from_secs(30),
        )
    }

    fn coordinator_with_lease(
        &self,
        name: &str,
        lease_duration: Duration,
    ) -> StartupContextCoordinator {
        StartupContextCoordinator::for_test(self.state.path().to_path_buf(), name, lease_duration)
    }

    fn working_dir(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }
}

fn opened(outcome: OpenEditorOutcome) -> StartupContextEditorSnapshot {
    match outcome {
        OpenEditorOutcome::Opened(editor) => editor,
        OpenEditorOutcome::Busy { owner, .. } => {
            panic!("expected opened editor, got busy owner {owner:?}")
        }
    }
}

fn lease_request_for(
    editor: &StartupContextEditorSnapshot,
    session_id: &str,
    connection_id: &str,
) -> LeaseRequest {
    lease_request(
        editor.lease.lease_id.clone(),
        editor.project.key_digest.clone(),
        Some(editor.plan_revision),
        session_id.to_string(),
        connection_id.to_string(),
    )
}

#[tokio::test]
async fn same_project_is_exclusive_across_sessions_and_named_coordinators() {
    let fixture = Fixture::new();
    let first = fixture.coordinator("first");
    let second = fixture.coordinator("second");
    let editor = opened(
        first
            .open_editor(
                "session-a".into(),
                "connection-a".into(),
                fixture.working_dir(),
            )
            .await
            .expect("open first editor"),
    );
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let state = first.inner.state.lock().unwrap();
        let record = state
            .leases
            .get(&editor.project.key_digest)
            .expect("live editor lease");
        let flags = unsafe { libc::fcntl(record.guard._file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    }

    let local_busy = first
        .open_editor(
            "session-b".into(),
            "connection-b".into(),
            fixture.working_dir(),
        )
        .await
        .expect("local contention response");
    let OpenEditorOutcome::Busy {
        owner: Some(owner), ..
    } = local_busy
    else {
        panic!("same coordinator must report a safe busy owner");
    };
    assert_eq!(owner.session_id, "session-a");
    assert_eq!(owner.server_name, "first");

    let cross_process_shape = second
        .open_editor(
            "session-c".into(),
            "connection-c".into(),
            fixture.working_dir(),
        )
        .await
        .expect("cross-coordinator contention response");
    let OpenEditorOutcome::Busy {
        owner: Some(owner), ..
    } = cross_process_shape
    else {
        panic!("second named coordinator must report busy");
    };
    assert_eq!(owner.session_id, "session-a");
    assert_eq!(owner.server_name, "first");

    let closed = first
        .close_editor(lease_request_for(&editor, "session-a", "connection-a"))
        .expect("close first editor");
    assert_eq!(closed, editor.lease.lease_id);
    assert!(matches!(
        second
            .open_editor(
                "session-c".into(),
                "connection-c".into(),
                fixture.working_dir()
            )
            .await
            .expect("open after close"),
        OpenEditorOutcome::Opened(_)
    ));
}

#[tokio::test]
async fn different_projects_can_hold_editor_leases_concurrently() {
    let state = tempfile::tempdir().expect("state");
    let first_project = tempfile::tempdir().expect("project one");
    let second_project = tempfile::tempdir().expect("project two");
    let coordinator = StartupContextCoordinator::for_test(
        state.path().to_path_buf(),
        "server",
        Duration::from_secs(30),
    );
    assert!(matches!(
        coordinator
            .open_editor(
                "session-a".into(),
                "connection-a".into(),
                first_project.path().to_string_lossy().into_owned(),
            )
            .await
            .unwrap(),
        OpenEditorOutcome::Opened(_)
    ));
    assert!(matches!(
        coordinator
            .open_editor(
                "session-b".into(),
                "connection-b".into(),
                second_project.path().to_string_lossy().into_owned(),
            )
            .await
            .unwrap(),
        OpenEditorOutcome::Opened(_)
    ));
}

#[tokio::test]
async fn lease_validation_covers_owner_revision_renew_close_disconnect_and_expiry() {
    let fixture = Fixture::new();
    let coordinator = fixture.coordinator_with_lease("server", Duration::from_millis(40));
    let editor = opened(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .unwrap(),
    );

    let wrong_owner = lease_request(
        editor.lease.lease_id.clone(),
        editor.project.key_digest.clone(),
        Some(editor.plan_revision),
        "other-session".to_string(),
        "connection".to_string(),
    );
    assert_eq!(
        coordinator
            .renew_lease(wrong_owner)
            .await
            .expect_err("wrong owner must fail")
            .kind,
        StartupContextFailureKind::LeaseOwnerMismatch
    );

    let stale_revision = lease_request(
        editor.lease.lease_id.clone(),
        editor.project.key_digest.clone(),
        Some(editor.plan_revision + 1),
        "session".to_string(),
        "connection".to_string(),
    );
    assert_eq!(
        coordinator
            .renew_lease(stale_revision)
            .await
            .expect_err("stale revision must fail")
            .kind,
        StartupContextFailureKind::StalePlanRevision
    );

    let renewed = coordinator
        .renew_lease(lease_request_for(&editor, "session", "connection"))
        .await
        .expect("renew live lease");
    assert!(renewed.expires_at > editor.lease.expires_at);
    thread::sleep(Duration::from_millis(55));
    assert_eq!(coordinator.expire_abandoned_leases(), 1);
    assert_eq!(
        coordinator
            .close_editor(lease_request_for(&editor, "session", "connection"))
            .expect_err("expired lease has already been released")
            .kind,
        StartupContextFailureKind::LeaseNotFound
    );

    let reopened = opened(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .expect("reopen after expiry"),
    );
    let close_cancel = Arc::new(AtomicBool::new(false));
    coordinator
        .lock_state()
        .searches
        .insert(("connection".to_string(), 91), Arc::clone(&close_cancel));
    coordinator
        .close_editor(lease_request_for(&reopened, "session", "connection"))
        .expect("explicit close");
    assert!(close_cancel.load(AtomicOrdering::Relaxed));
    assert!(coordinator.finish_search(&("connection".to_string(), 91)));

    let reopened = opened(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .expect("reopen after explicit close"),
    );
    let disconnect_cancel = Arc::new(AtomicBool::new(false));
    coordinator.lock_state().searches.insert(
        ("connection".to_string(), 92),
        Arc::clone(&disconnect_cancel),
    );
    assert_eq!(coordinator.release_connection("connection"), 1);
    assert!(disconnect_cancel.load(AtomicOrdering::Relaxed));
    assert!(coordinator.finish_search(&("connection".to_string(), 92)));
    assert_eq!(coordinator.release_connection("connection"), 0);
    assert!(matches!(
        coordinator
            .open_editor(
                "other".into(),
                "other-connection".into(),
                fixture.working_dir()
            )
            .await
            .expect("open after disconnect release"),
        OpenEditorOutcome::Opened(_)
    ));
    drop(reopened);
}

#[tokio::test]
async fn poisoned_in_memory_state_recovers_without_permanently_sticking_ownership() {
    let fixture = Fixture::new();
    let coordinator = fixture.coordinator("server");
    let inner = Arc::clone(&coordinator.inner);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _state = inner.state.lock().unwrap();
        panic!("synthetic interrupted owner task");
    }));

    assert_eq!(coordinator.expire_abandoned_leases(), 0);
    assert!(matches!(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .expect("coordinator must recover poisoned state"),
        OpenEditorOutcome::Opened(_)
    ));
}

#[tokio::test]
async fn stale_owner_metadata_is_overwritten_only_after_live_guard_acquisition() {
    let fixture = Fixture::new();
    let coordinator = fixture.coordinator("server");
    let project = coordinator
        .inner
        .engine
        .resolve_project(fixture.project.path())
        .unwrap();
    let digest = project.key().digest();
    let paths = coordinator.ownership_paths(&digest);
    let stale = EditorOwnerMetadata {
        schema_version: OWNER_METADATA_SCHEMA_VERSION,
        project_key_digest: digest.clone(),
        lease_id: "stale-lease".to_string(),
        server_id: "dead-server".to_string(),
        server_name: "dead".to_string(),
        session_id: "dead-session".to_string(),
        connection_id: "dead-connection".to_string(),
        pid: u32::MAX,
        process_start_identity: "stale".to_string(),
        acquired_at: Utc::now(),
        renewed_at: Utc::now(),
        expires_at: Utc::now(),
    };
    write_owner_metadata(&paths.owner, &stale, StartupContextOperation::OpenEditor).unwrap();

    let editor = opened(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .expect("stale metadata must not block a successfully acquired guard"),
    );
    let current = read_owner_metadata(&paths.owner).expect("current owner metadata");
    assert_eq!(current.lease_id, editor.lease.lease_id);
    assert_eq!(current.session_id, "session");
}

#[test]
fn fallback_owner_creation_is_exclusive_and_process_identity_rejects_reuse() {
    let root = tempfile::tempdir().expect("ownership root");
    let owner_path = root.path().join("owner.json");
    let metadata = EditorOwnerMetadata {
        schema_version: OWNER_METADATA_SCHEMA_VERSION,
        project_key_digest: "digest".to_string(),
        lease_id: "lease".to_string(),
        server_id: "server".to_string(),
        server_name: "server".to_string(),
        session_id: "session".to_string(),
        connection_id: "connection".to_string(),
        pid: std::process::id(),
        process_start_identity: current_process_start_identity(),
        acquired_at: Utc::now(),
        renewed_at: Utc::now(),
        expires_at: Utc::now() + ChronoDuration::seconds(30),
    };
    create_owner_metadata_exclusive(&owner_path, &metadata).expect("first exclusive create");
    assert!(create_owner_metadata_exclusive(&owner_path, &metadata).is_err());
    assert!(process_identity_matches(
        metadata.pid,
        &metadata.process_start_identity
    ));
    assert!(!process_identity_matches(metadata.pid, "wrong-start"));
}

fn installed_session(
    coordinator: &StartupContextCoordinator,
    project_root: &Path,
    text: &str,
) -> Session {
    std::fs::write(project_root.join("PLAN.md"), text).unwrap();
    let project = coordinator
        .inner
        .engine
        .resolve_project(project_root)
        .unwrap();
    let preview = coordinator
        .inner
        .engine
        .preview_selection(&project, [StartupSelectionInput::new("PLAN.md")]);
    let preparation = coordinator
        .inner
        .engine
        .prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)
        .unwrap();
    let mut session = Session::create(None, None);
    session.working_dir = Some(project_root.to_string_lossy().into_owned());
    session
        .install_prepared_startup_context(preparation)
        .expect("install startup context");
    session
}

#[tokio::test]
async fn status_and_lazy_detail_are_receipt_owned_exact_bounded_and_stale_safe() {
    let fixture = Fixture::new();
    let coordinator = fixture.coordinator("server");
    let text = format!("secret-prefix-{}-tail", "x".repeat(70_000));
    let session = installed_session(&coordinator, fixture.project.path(), &text);
    let snapshot = StartupContextSessionSnapshot::from_session(&session);
    let status = coordinator
        .status_snapshot(snapshot, 0, None, 0, None)
        .await;
    assert_eq!(status.compact.receipt_file_count, 1);
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].bytes, text.len() as u64);

    let file = &status.files[0];
    let detail = coordinator
        .file_detail(
            &session,
            &file.batch_id,
            &file.spec_id,
            &file.message_id,
            &file.sha256,
            0,
            Some(usize::MAX),
        )
        .expect("first detail chunk");
    assert_eq!(
        detail.content.chars().count(),
        STARTUP_CONTEXT_FILE_DETAIL_MAX_CHARS
    );
    assert_eq!(
        detail.next_start_char,
        Some(STARTUP_CONTEXT_FILE_DETAIL_MAX_CHARS)
    );
    let second = coordinator
        .file_detail(
            &session,
            &file.batch_id,
            &file.spec_id,
            &file.message_id,
            &file.sha256,
            detail.next_start_char.unwrap(),
            None,
        )
        .expect("second detail chunk");
    assert_eq!(format!("{}{}", detail.content, second.content), text);

    assert_eq!(
        coordinator
            .file_detail(
                &session,
                &file.batch_id,
                &file.spec_id,
                &file.message_id,
                "stale-digest",
                0,
                None,
            )
            .expect_err("stale digest must fail")
            .kind,
        StartupContextFailureKind::DigestMismatch
    );
    assert_eq!(
        coordinator
            .file_detail(
                &session,
                &file.batch_id,
                &file.spec_id,
                "wrong-message",
                &file.sha256,
                0,
                None,
            )
            .expect_err("wrong message identity must fail")
            .kind,
        StartupContextFailureKind::MessageMismatch
    );
}

#[test]
fn remote_history_projection_omits_receipt_owned_secret_bodies_without_mutating_source() {
    let fixture = Fixture::new();
    let coordinator = fixture.coordinator("server");
    let secret = "WP03_SYNTHETIC_HISTORY_SECRET";
    let session = installed_session(&coordinator, fixture.project.path(), secret);
    let source = serde_json::to_value(&session.messages).unwrap();
    let (rendered, _) = crate::session::render_messages_and_images_for_remote_history(&session);
    let payload = serde_json::to_string(&rendered).unwrap();
    assert!(!payload.contains(secret));
    assert_eq!(serde_json::to_value(&session.messages).unwrap(), source);
    assert_eq!(
        session.startup_context.as_ref().unwrap().batches[0]
            .files
            .len(),
        1
    );
}

#[tokio::test]
async fn cancellable_search_and_checked_event_bound_do_not_leak_oversized_content() {
    let fixture = Fixture::new();
    std::fs::write(fixture.project.path().join("PLAN.md"), "plan").unwrap();
    let coordinator = fixture.coordinator("server");
    let editor = opened(
        coordinator
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .unwrap(),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    coordinator
        .inner
        .state
        .lock()
        .unwrap()
        .searches
        .insert(("connection".to_string(), 77), Arc::clone(&cancel));
    assert!(coordinator.cancel_search("connection", 77));
    assert!(cancel.load(AtomicOrdering::Relaxed));
    assert!(coordinator.finish_search(&("connection".to_string(), 77)));

    {
        let mut state = coordinator.lock_state();
        for request_id in 100..100 + MAX_SEARCHES_PER_CONNECTION as u64 {
            state.searches.insert(
                ("connection".to_string(), request_id),
                Arc::new(AtomicBool::new(false)),
            );
        }
    }
    let capacity_error = coordinator
        .start_search(
            200,
            lease_request_for(&editor, "session", "connection"),
            "plan".to_string(),
            None,
            mpsc::unbounded_channel().0,
        )
        .expect_err("per-connection search capacity must be bounded");
    assert_eq!(
        capacity_error.kind,
        StartupContextFailureKind::InvalidRequest
    );
    coordinator.lock_state().searches.clear();

    let (tx, mut rx) = mpsc::unbounded_channel();

    emit_checked(
        &tx,
        88,
        StartupContextOperation::PreviewFile,
        ServerEvent::StartupContextFilePreview {
            id: 88,
            preview: StartupContextFilePreview {
                project_key_digest: editor.project.key_digest,
                plan_revision: editor.plan_revision,
                logical_path: "PLAN.md".to_string(),
                resolved_path: fixture
                    .project
                    .path()
                    .join("PLAN.md")
                    .to_string_lossy()
                    .into_owned(),
                classification: StartupContextPathClassification::Project,
                requires_external_approval: false,
                sha256: "hash".to_string(),
                bytes: (STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1) as u64,
                estimated_tokens: 0,
                total_chars: STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1,
                start_char: 0,
                end_char: STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1,
                next_start_char: None,
                truncated: false,
                content: "s".repeat(STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1),
            },
        },
    );
    let ServerEvent::StartupContextFailed { failure, .. } = rx.recv().await.unwrap() else {
        panic!("oversized response must become a bounded failure");
    };
    assert_eq!(failure.kind, StartupContextFailureKind::EventTooLarge);
    assert!(!failure.message.contains("ssssssssssssssss"));
}

#[tokio::test]
async fn subprocess_lease_holder() {
    let Ok(state) = std::env::var("JCODE_WP03_HOLDER_STATE") else {
        return;
    };
    let project = std::env::var("JCODE_WP03_HOLDER_PROJECT").unwrap();
    let ready = std::env::var("JCODE_WP03_HOLDER_READY").unwrap();
    let coordinator = StartupContextCoordinator::for_test(
        PathBuf::from(state),
        "holder",
        Duration::from_secs(30),
    );
    let _editor = opened(
        coordinator
            .open_editor("holder-session".into(), "holder-connection".into(), project)
            .await
            .expect("holder open"),
    );
    std::fs::write(ready, "ready").unwrap();
    tokio::time::sleep(Duration::from_secs(30)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn process_termination_releases_the_kernel_held_project_guard() {
    let fixture = Fixture::new();
    let ready = fixture.state.path().join("holder-ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("server::startup_context::tests::subprocess_lease_holder")
        .arg("--nocapture")
        .env("JCODE_WP03_HOLDER_STATE", fixture.state.path())
        .env("JCODE_WP03_HOLDER_PROJECT", fixture.project.path())
        .env("JCODE_WP03_HOLDER_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lease holder");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ready.exists(),
        "subprocess did not acquire the lease in time"
    );

    let contender = fixture.coordinator("contender");
    assert!(matches!(
        contender
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .unwrap(),
        OpenEditorOutcome::Busy { .. }
    ));
    child.kill().expect("kill holder");
    child.wait().expect("wait for holder");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match contender
            .open_editor("session".into(), "connection".into(), fixture.working_dir())
            .await
            .expect("retry after holder exit")
        {
            OpenEditorOutcome::Opened(_) => break,
            OpenEditorOutcome::Busy { .. } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            OpenEditorOutcome::Busy { .. } => panic!("kernel guard remained stuck after exit"),
        }
    }
}
