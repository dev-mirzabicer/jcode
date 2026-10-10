use super::*;
use jcode_session_work_types::{ActivationOrigin, SessionWorkActivation, SessionWorkRole};

struct Fixture {
    _temp: tempfile::TempDir,
    surface: SessionWorkSurface,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionWorkStore::at(temp.path().join("state/session-work"));
        let surface = SessionWorkSurface::at(temp.path().join("home/session-work"), store);
        let fixture = Self {
            _temp: temp,
            surface,
        };
        fixture.activate("s1");
        fixture
    }
    fn activate(&self, session: &str) {
        self.surface
            .store()
            .activate(
                &SessionWorkActivation {
                    session: session.into(),
                    role: SessionWorkRole::Primary,
                    origin: ActivationOrigin::Fresh {},
                    activated_at: Utc::now(),
                    module_types: Vec::new(),
                },
                None,
            )
            .unwrap();
        self.surface.materialize(session).unwrap();
    }
    fn file(&self) -> String {
        std::fs::read_to_string(self.surface.workflow_path("s1").unwrap()).unwrap()
    }
    fn history(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.surface.history_dir("s1").unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
    fn write(&self, request: &str, text: &str) -> Result<CommitReceipt, SessionWorkError> {
        self.surface.write_workflow("s1", request, text, Utc::now())
    }
}

#[test]
fn a_valid_write_commits_then_writes_the_file_and_history() {
    let fixture = Fixture::new();
    assert!(fixture.surface.session_dir("s1").unwrap().is_dir());
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::NoWorkflow
    );
    let receipt = fixture.write("w1", "- [>] a: A\n").unwrap();
    assert_eq!(receipt.revision, 1);
    assert_eq!(fixture.file(), "- [>] a: A\n");
    assert_eq!(fixture.history(), ["r01.md"]);
    let head = fixture
        .surface
        .store()
        .workflow_head("s1")
        .unwrap()
        .unwrap();
    assert_eq!(head.synced, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: std::path::PathBuf| {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode(fixture.surface.workflow_path("s1").unwrap()), 0o600);
        assert_eq!(
            mode(fixture.surface.history_dir("s1").unwrap().join("r01.md")),
            0o400
        );
        assert_eq!(mode(fixture.surface.session_dir("s1").unwrap()), 0o700);
    }
}

#[test]
fn an_invalid_write_changes_neither_store_nor_files() {
    let fixture = Fixture::new();
    fixture.write("w1", "- [>] a: A\n").unwrap();
    let error = fixture.write("w2", "- [>] a: A\n- [>] b: B\n").unwrap_err();
    assert!(error.to_string().contains("line 2"), "{error}");
    assert_eq!(fixture.file(), "- [>] a: A\n");
    assert_eq!(fixture.history(), ["r01.md"]);
    assert!(
        fixture.write("w3", "").is_err(),
        "a workflow cannot be emptied"
    );
    assert_eq!(
        fixture
            .surface
            .store()
            .workflow_head("s1")
            .unwrap()
            .unwrap()
            .revision
            .revision,
        1
    );
}

#[test]
fn history_keeps_the_newest_forty_revisions_as_read_only_files() {
    let fixture = Fixture::new();
    for step in 1..=45 {
        fixture
            .write(&format!("w{step}"), &format!("- [>] a: Step {step}\n"))
            .unwrap();
    }
    let history = fixture.history();
    assert_eq!(history.len(), HISTORY_FILES);
    assert_eq!(history.first().unwrap(), "r06.md");
    assert_eq!(history.last().unwrap(), "r45.md");
    let dir = fixture.surface.history_dir("s1").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("r20.md")).unwrap(),
        "- [>] a: Step 20\n"
    );
    assert_eq!(
        fixture
            .surface
            .store()
            .recent_revisions("s1", 1000)
            .unwrap()
            .len(),
        45
    );
}

#[test]
fn shell_changes_are_restored_at_the_next_safe_point() {
    let fixture = Fixture::new();
    fixture.write("w1", "- [>] a: A\n").unwrap();
    let path = fixture.surface.workflow_path("s1").unwrap();
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Unchanged
    );

    std::fs::write(&path, "- [x] a: A\n- [ ] b: sneaked in\n").unwrap();
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Restored { revision: 1 }
    );
    assert_eq!(fixture.file(), "- [>] a: A\n");

    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Restored { revision: 1 }
    );
    assert_eq!(fixture.file(), "- [>] a: A\n");

    #[cfg(unix)]
    {
        let elsewhere = fixture._temp.path().join("elsewhere.md");
        std::fs::write(&elsewhere, "outside").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert_eq!(
            fixture.surface.reconcile("s1").unwrap(),
            Reconciled::Restored { revision: 1 }
        );
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "outside");
    }
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Unchanged
    );
}

#[test]
fn tampered_history_files_are_rewritten_silently() {
    let fixture = Fixture::new();
    fixture.write("w1", "- [>] a: A\n").unwrap();
    let dir = fixture.surface.history_dir("s1").unwrap();
    std::fs::remove_file(dir.join("r01.md")).unwrap();
    std::fs::write(dir.join("r09.md"), "stale").unwrap();
    std::fs::write(dir.join("notes.txt"), "someone's notes").unwrap();
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Unchanged
    );
    assert_eq!(fixture.history(), ["notes.txt", "r01.md"]);
}

#[test]
fn a_directory_in_the_files_place_is_never_deleted() {
    let fixture = Fixture::new();
    fixture.write("w1", "- [>] a: A\n").unwrap();
    let path = fixture.surface.workflow_path("s1").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), "keep").unwrap();
    assert!(fixture.surface.reconcile("s1").is_err());
    assert_eq!(
        std::fs::read_to_string(path.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn an_interrupted_write_is_finished_without_a_restore_notice() {
    let fixture = Fixture::new();
    fixture.write("w1", "- [>] a: A\n").unwrap();
    // The store committed revision 2, then the process stopped before the file.
    fixture
        .surface
        .store()
        .commit_workflow(
            "s1",
            "w2",
            "- [x] a: A\n",
            &RevisionSource::Agent {},
            Utc::now(),
        )
        .unwrap();
    assert_eq!(fixture.file(), "- [>] a: A\n");
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Synced { revision: 2 }
    );
    assert_eq!(fixture.file(), "- [x] a: A\n");
    assert_eq!(fixture.history(), ["r01.md", "r02.md"]);
    // Replaying the interrupted invocation converges on the same revision.
    let replay = fixture.write("w2", "- [x] a: A\n").unwrap();
    assert_eq!(
        replay,
        CommitReceipt {
            revision: 2,
            replayed: true
        }
    );
    assert_eq!(
        fixture.surface.reconcile("s1").unwrap(),
        Reconciled::Unchanged
    );
}

#[test]
fn paths_are_classified_by_session_and_file() {
    let fixture = Fixture::new();
    let root = fixture.surface.resolved_root();
    let classify = |relative: &str| fixture.surface.classify(&root.join(relative));
    let file = |relative: &str| classify(relative).unwrap().file;
    assert_eq!(file("s1/workflow.md"), SurfaceFile::Workflow);
    assert_eq!(file("s1/summary.md"), SurfaceFile::Summary);
    assert_eq!(file("s1/history/r01.md"), SurfaceFile::History);
    assert_eq!(file("s1/history"), SurfaceFile::History);
    assert_eq!(file("s1/other.md"), SurfaceFile::Other);
    assert_eq!(file("s1/nested/workflow.md"), SurfaceFile::Other);
    assert_eq!(file("s1"), SurfaceFile::Directory);
    assert_eq!(classify("s2/workflow.md").unwrap().session, "s2");
    assert_eq!(
        fixture.surface.classify(&root.parent().unwrap().join("x")),
        None
    );
}

#[test]
fn summaries_are_plain_host_owned_drafts() {
    let fixture = Fixture::new();
    assert_eq!(fixture.surface.read_summary("s1").unwrap(), None);
    fixture.surface.write_summary("s1", b"# Done\n").unwrap();
    assert_eq!(
        fixture.surface.read_summary("s1").unwrap().as_deref(),
        Some(&b"# Done\n"[..])
    );
}

#[test]
fn unpublished_sessions_lose_their_files_and_rows_only() {
    let fixture = Fixture::new();
    fixture.activate("s2");
    fixture.write("w1", "- [>] a: A\n").unwrap();
    fixture
        .surface
        .write_workflow("s2", "w1", "- [>] b: B\n", Utc::now())
        .unwrap();
    fixture.surface.remove_unpublished("s2").unwrap();
    assert!(!fixture.surface.session_dir("s2").unwrap().exists());
    assert_eq!(fixture.surface.store().activation("s2").unwrap(), None);
    assert_eq!(fixture.file(), "- [>] a: A\n");
    fixture.surface.remove_unpublished("../escape").unwrap();
    assert!(fixture.surface.session_dir("../escape").is_err());
    assert!(fixture.surface.session_dir("a.b").is_err());
}
