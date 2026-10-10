//! The session-work destination through the real native file tools.
use super::*;
use crate::session::Session;
use crate::session_work::{SessionWorkStore, SessionWorkSurface};
use serde_json::json;
use std::path::PathBuf;

struct WorkFixture {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _temporary: tempfile::TempDir,
    session: Session,
    root: PathBuf,
    surface: SessionWorkSurface,
}
impl Drop for WorkFixture {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(v) => crate::env::set_var(key, v),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

impl WorkFixture {
    /// A placed primary created with session work, in private state.
    fn new(activated: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let mut previous = Vec::new();
        for (key, subdir) in [
            ("HOME", "home"),
            ("JCODE_HOME", "jcode"),
            ("JCODE_RUNTIME_DIR", "runtime"),
            ("XDG_CONFIG_HOME", "config"),
        ] {
            previous.push((key, std::env::var_os(key)));
            std::fs::create_dir_all(temporary.path().join(subdir)).unwrap();
            crate::env::set_var(key, temporary.path().join(subdir));
        }
        previous.push(("JCODE_SCRATCH_DIR", std::env::var_os("JCODE_SCRATCH_DIR")));
        crate::env::remove_var("JCODE_SCRATCH_DIR");
        previous.push((
            "JCODE_SESSION_WORK_ENABLED",
            std::env::var_os("JCODE_SESSION_WORK_ENABLED"),
        ));
        crate::env::set_var(
            "JCODE_SESSION_WORK_ENABLED",
            if activated { "true" } else { "false" },
        );
        crate::config::invalidate_config_cache();
        let workspace =
            crate::workspace::WorkspaceService::new(&crate::storage::durable_state_dir());
        workspace
            .initialize(crate::workspace::RequestId::new())
            .unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let review = workspace
            .review_organization_change(
                workspace.status().unwrap().revision,
                crate::workspace::OrganizationChange::RegisterLocation {
                    name: "fixture".into(),
                    path: root.clone(),
                    registration: crate::workspace::Registration::Standalone,
                },
            )
            .unwrap();
        let location = match workspace
            .apply_organization_change(crate::workspace::RequestId::new(), review.id)
            .unwrap()
            .targets[0]
        {
            crate::workspace::EntityId::Location(id) => id,
            _ => panic!(),
        };
        let prepared = workspace
            .prepare_primary_location(
                crate::workspace::Placement::Standalone(location),
                Some(&root),
                crate::workspace::OperationId::new(),
            )
            .unwrap();
        let mut session = Session::create_with_id(
            format!("session_work_{}", uuid::Uuid::new_v4().simple()),
            None,
            None,
        );
        session.working_dir = Some(root.to_string_lossy().into_owned());
        session.location = Some(prepared.location);
        let activated_now = crate::session_work::activate_new_session(
            &mut session,
            crate::session_work::NewSessionWork::Primary,
            &crate::instruction::InstructionRepositoryService::new(),
        )
        .unwrap();
        assert_eq!(activated_now, activated);
        session.save().unwrap();
        let surface = SessionWorkSurface::new().unwrap();
        Self {
            previous,
            _temporary: temporary,
            session,
            root,
            surface,
        }
    }

    fn ctx(&self, call: &str) -> ToolContext {
        ToolContext {
            session_id: self.session.id.clone(),
            message_id: "session-work-message".into(),
            tool_call_id: call.into(),
            working_dir: Some(self.root.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
            invocation: Default::default(),
        }
    }

    fn workflow(&self) -> PathBuf {
        self.surface.workflow_path(&self.session.id).unwrap()
    }

    fn head(&self) -> Option<(u32, String)> {
        SessionWorkStore::new()
            .workflow_head(&self.session.id)
            .unwrap()
            .map(|head| (head.revision.revision, head.revision.text))
    }
}

async fn run(
    registry: &Registry,
    tool: &str,
    input: serde_json::Value,
    ctx: ToolContext,
) -> String {
    match registry.execute(tool, input, ctx).await {
        Ok(output) if !output.is_error => format!("ok: {}", output.output),
        Ok(output) => format!("error: {}", output.output),
        Err(error) => format!("error: {error:#}"),
    }
}

#[tokio::test]
async fn workflow_writes_are_validated_committed_and_written() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let workflow = f.workflow();

    let created = run(
        &registry,
        "write",
        json!({"file_path": workflow, "content": "- [>] a: Alpha\n- [ ] b: Beta\n"}),
        f.ctx("w1"),
    )
    .await;
    assert!(created.starts_with("ok"), "{created}");
    assert_eq!(
        f.head(),
        Some((1, "- [>] a: Alpha\n- [ ] b: Beta\n".into()))
    );
    assert_eq!(
        std::fs::read_to_string(&workflow).unwrap(),
        "- [>] a: Alpha\n- [ ] b: Beta\n"
    );
    let history = f.surface.history_dir(&f.session.id).unwrap();
    assert!(history.join("r01.md").is_file());

    // A replayed invocation converges on its revision.
    let replay = run(
        &registry,
        "write",
        json!({"file_path": workflow, "content": "- [>] a: Alpha\n- [ ] b: Beta\n"}),
        f.ctx("w1"),
    )
    .await;
    assert!(replay.starts_with("ok"), "{replay}");
    assert_eq!(f.head().unwrap().0, 1);

    // edit reads the store's text, not whatever the file holds.
    std::fs::write(&workflow, "- [>] a: Alpha\n- [ ] b: Shell text\n").unwrap();
    let edited = run(
        &registry,
        "edit",
        json!({"file_path": workflow, "old_string": "Shell text", "new_string": "Beta"}),
        f.ctx("e1"),
    )
    .await;
    assert!(
        edited.starts_with("error"),
        "the shell text is not the store's: {edited}"
    );
    let edited = run(
        &registry,
        "edit",
        json!({"file_path": workflow, "old_string": "- [>] a: Alpha\n- [ ] b", "new_string": "- [x] a: Alpha\n- [>] b"}),
        f.ctx("e2"),
    )
    .await;
    assert!(edited.starts_with("ok"), "{edited}");
    assert_eq!(
        f.head(),
        Some((2, "- [x] a: Alpha\n- [>] b: Beta\n".into()))
    );
    assert_eq!(
        std::fs::read_to_string(&workflow).unwrap(),
        "- [x] a: Alpha\n- [>] b: Beta\n"
    );

    // An invalid text changes nothing and names its line.
    let invalid = run(
        &registry,
        "write",
        json!({"file_path": workflow, "content": "- [x] a: Alpha\n- [>] b: Beta\n- [>] c: Gamma\n"}),
        f.ctx("w2"),
    )
    .await;
    assert!(
        invalid.starts_with("error") && invalid.contains("line 3"),
        "{invalid}"
    );
    let emptied = run(
        &registry,
        "write",
        json!({"file_path": workflow, "content": ""}),
        f.ctx("w3"),
    )
    .await;
    assert!(emptied.starts_with("error"), "{emptied}");
    assert_eq!(f.head().unwrap().0, 2);
    assert_eq!(
        std::fs::read_to_string(&workflow).unwrap(),
        "- [x] a: Alpha\n- [>] b: Beta\n"
    );

    // multiedit takes the same route.
    let multi = run(
        &registry,
        "multiedit",
        json!({"file_path": workflow, "edits": [
            {"old_string": "- [>] b: Beta", "new_string": "- [x] b: Beta"},
            {"old_string": "- [x] a: Alpha", "new_string": "- [x] a: Alpha one"}
        ]}),
        f.ctx("m1"),
    )
    .await;
    assert!(multi.starts_with("ok"), "{multi}");
    assert_eq!(f.head().unwrap().0, 3);

    // summary.md is a plain host-owned draft.
    let summary = f.surface.summary_path(&f.session.id).unwrap();
    let wrote = run(
        &registry,
        "write",
        json!({"file_path": summary, "content": "# Summary\n"}),
        f.ctx("s1"),
    )
    .await;
    assert!(wrote.starts_with("ok"), "{wrote}");
    assert_eq!(std::fs::read_to_string(&summary).unwrap(), "# Summary\n");
}

#[tokio::test]
async fn patches_span_the_destination_and_ordinary_roots_but_never_delete_it() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let workflow = f.workflow();
    let regular = f.root.join("notes.md");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+- [>] a: Alpha\n*** Add File: {}\n+notes\n*** End Patch",
        workflow.display(),
        regular.display()
    );
    let applied = run(
        &registry,
        "apply_patch",
        json!({"patch_text": patch}),
        f.ctx("p1"),
    )
    .await;
    assert!(applied.starts_with("ok"), "{applied}");
    assert_eq!(f.head(), Some((1, "- [>] a: Alpha\n".into())));
    assert_eq!(std::fs::read_to_string(&regular).unwrap(), "notes\n");

    let update = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-- [>] a: Alpha\n+- [x] a: Alpha\n*** End Patch",
        workflow.display()
    );
    let updated = run(
        &registry,
        "apply_patch",
        json!({"patch_text": update}),
        f.ctx("p2"),
    )
    .await;
    assert!(updated.starts_with("ok"), "{updated}");
    assert_eq!(f.head(), Some((2, "- [x] a: Alpha\n".into())));

    // Deleting or moving the workflow away is refused before any effect.
    let other = f.root.join("other.md");
    let delete = format!(
        "*** Begin Patch\n*** Add File: {}\n+other\n*** Delete File: {}\n*** End Patch",
        other.display(),
        workflow.display()
    );
    let refused = run(
        &registry,
        "apply_patch",
        json!({"patch_text": delete}),
        f.ctx("p3"),
    )
    .await;
    assert!(
        refused.starts_with("error") && refused.contains("can't be deleted"),
        "{refused}"
    );
    assert!(!other.exists(), "nothing in the refused patch ran");
    assert!(workflow.is_file());
    let moved = format!(
        "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-- [x] a: Alpha\n+- [x] a: Alpha moved\n*** End Patch",
        workflow.display(),
        other.display()
    );
    let refused = run(
        &registry,
        "apply_patch",
        json!({"patch_text": moved}),
        f.ctx("p4"),
    )
    .await;
    assert!(refused.starts_with("error"), "{refused}");
    assert!(!other.exists());
    assert_eq!(f.head().unwrap().0, 2);
}

#[tokio::test]
async fn history_other_files_and_other_sessions_are_refused() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    run(
        &registry,
        "write",
        json!({"file_path": f.workflow(), "content": "- [>] a: Alpha\n"}),
        f.ctx("w1"),
    )
    .await;
    let dir = f.surface.session_dir(&f.session.id).unwrap();
    for (index, (path, expected)) in [
        (dir.join("history/r01.md"), "read-only"),
        (dir.join("history/r99.md"), "read-only"),
        (dir.join("notes.md"), "holds only"),
        (
            f.surface.workflow_path("session_someone_else").unwrap(),
            "another session",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = run(
            &registry,
            "write",
            json!({"file_path": path, "content": "- [ ] x: X\n"}),
            f.ctx(&format!("refused-{index}")),
        )
        .await;
        assert!(
            refused.starts_with("error") && refused.contains(expected),
            "{refused}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("history/r01.md")).unwrap(),
        "- [>] a: Alpha\n"
    );
    assert!(!dir.join("notes.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_at_the_workflow_path_cannot_redirect_a_write() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let target = f.root.join("target.md");
    std::fs::write(&target, "untouched\n").unwrap();
    std::os::unix::fs::symlink(&target, f.workflow()).unwrap();
    let wrote = run(
        &registry,
        "write",
        json!({"file_path": f.workflow(), "content": "- [>] a: Alpha\n"}),
        f.ctx("w1"),
    )
    .await;
    assert!(wrote.starts_with("ok"), "{wrote}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched\n");
    assert!(std::fs::symlink_metadata(f.workflow()).unwrap().is_file());
    assert_eq!(f.head(), Some((1, "- [>] a: Alpha\n".into())));
}

#[tokio::test]
async fn sessions_without_session_work_cannot_write_there() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(false);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let refused = run(
        &registry,
        "write",
        json!({"file_path": f.workflow(), "content": "- [>] a: Alpha\n"}),
        f.ctx("w1"),
    )
    .await;
    assert!(refused.starts_with("error"), "{refused}");
    assert!(!f.workflow().exists());
    assert!(!SessionWorkStore::new().exists().unwrap());
}

#[tokio::test]
async fn an_unreadable_store_refuses_writes_before_any_effect() {
    let _guard = crate::storage::lock_test_env();
    let f = WorkFixture::new(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let store = SessionWorkStore::new();
    std::fs::write(store.path(), b"damaged on purpose, this is not a database").unwrap();
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-shm"));
    let regular = f.root.join("alongside.md");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+x\n*** Add File: {}\n+- [>] a: Alpha\n*** End Patch",
        regular.display(),
        f.workflow().display()
    );
    let refused = run(
        &registry,
        "apply_patch",
        json!({"patch_text": patch}),
        f.ctx("p1"),
    )
    .await;
    assert!(refused.starts_with("error"), "{refused}");
    assert!(!regular.exists());
    assert!(!f.workflow().exists());
}
