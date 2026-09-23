use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    service: WorkspaceService,
    root: PathBuf,
    location: LocationId,
    client: WorkspaceClientAuthority,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        );
        let service = WorkspaceService::new(&directory.path().join("state"));
        service.initialize(RequestId::new()).unwrap();
        let EntityId::Project(project) = change(
            &service,
            OrganizationChange::CreateProject {
                name: "project".into(),
            },
        ) else {
            panic!()
        };
        let EntityId::Repository(repository) = change(
            &service,
            OrganizationChange::CreateRepository {
                name: "repository".into(),
                remotes: vec![],
            },
        ) else {
            panic!()
        };
        change(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        );
        let EntityId::Location(location) = change(
            &service,
            OrganizationChange::RegisterLocation {
                name: "checkout".into(),
                path: root.clone(),
                registration: Registration::Checkout {
                    home: Home::Project(project),
                    repository,
                },
            },
        ) else {
            panic!()
        };
        Self {
            _directory: directory,
            service,
            root,
            location,
            client: WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
        }
    }
    fn spec(&self, conditional: bool) -> CloseoutSpec {
        let Entity::Location(location) = self
            .service
            .inspect(EntityId::Location(self.location))
            .unwrap()
        else {
            panic!()
        };
        CloseoutSpec {
            location: self.location,
            expected_generation: location.binding_generation,
            preservation_directory: None,
            conditional_no_loss: conditional,
            full_archive: false,
        }
    }
    fn begin(&self, conditional: bool) -> CloseoutRecord {
        self.service
            .begin_closeout(
                &self.client,
                RequestId::new(),
                self.service.status().unwrap().revision,
                self.spec(conditional),
            )
            .unwrap()
    }
}
fn change(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
}
fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn authorization_is_exact_default_off_revocable_and_request_idempotent() {
    let fixture = Fixture::new();
    let spec = fixture.spec(false);
    let request = RequestId::new();
    let revision = fixture.service.status().unwrap().revision;
    let first = fixture
        .service
        .begin_closeout(&fixture.client, request, revision, spec.clone())
        .unwrap();
    assert!(!first.spec.conditional_no_loss);
    assert_eq!(
        first,
        fixture
            .service
            .begin_closeout(&fixture.client, request, revision, spec.clone())
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.conditional_no_loss = true;
    assert_eq!(
        fixture
            .service
            .begin_closeout(&fixture.client, request, revision, changed)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                spec
            )
            .unwrap_err()
            .code,
        IssueCode::Busy
    );
    let revoked = fixture
        .service
        .revoke_closeout(
            &fixture.client,
            RequestId::new(),
            first.operation,
            first.revision,
        )
        .unwrap();
    assert_eq!(revoked.stage, CloseoutStage::Revoked);
    assert!(
        fixture
            .service
            .inventory_closeout(first.operation, revoked.revision)
            .is_err()
    );
    let next = fixture.begin(true);
    assert!(next.spec.conditional_no_loss);
    assert_ne!(first.operation, next.operation);
    assert!(fixture.root.exists());
}

#[test]
fn inventory_captures_ignored_hidden_index_links_and_rejects_stale_decisions() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join(".gitignore"), "ignored\n").unwrap();
    std::fs::write(fixture.root.join("ignored"), "unique ignored data").unwrap();
    std::fs::write(fixture.root.join(".hidden"), "hidden data").unwrap();
    std::fs::write(fixture.root.join("staged"), "index version").unwrap();
    git(&fixture.root, &["add", "staged"]);
    std::fs::write(fixture.root.join("staged"), "working version").unwrap();
    symlink("/outside/not-followed", fixture.root.join("link")).unwrap();
    std::fs::hard_link(fixture.root.join("ignored"), fixture.root.join("hardlink")).unwrap();
    let started = fixture.begin(false);
    let captured = fixture
        .service
        .inventory_closeout(started.operation, started.revision)
        .unwrap();
    let page = fixture
        .service
        .closeout_inventory(
            captured.operation,
            captured.inventory_digest.as_ref().unwrap(),
            0,
            200,
        )
        .unwrap();
    assert!(page.entries.iter().any(
        |e| e.path == Path::new("ignored") && e.sha256 == Some(digest(b"unique ignored data"))
    ));
    assert!(
        page.entries
            .iter()
            .any(|e| e.path == Path::new(".git/index"))
    );
    assert!(page.entries.iter().any(|e| e.path == Path::new(".hidden")));
    assert!(page.entries.iter().any(|e| e.path == Path::new("link")
        && e.link_target.as_deref() == Some(Path::new("/outside/not-followed"))));
    assert!(
        page.entries
            .iter()
            .any(|e| e.path == Path::new("hardlink") && e.links == 2)
    );
    assert!(
        !page
            .entries
            .iter()
            .any(|e| e.path != Path::new("link") && e.path.starts_with("link"))
    );
    let item = page
        .entries
        .iter()
        .find(|e| e.path == Path::new("ignored"))
        .unwrap();
    let decision = CloseoutDecision {
        entry: item.id.clone(),
        disposition: CloseoutDisposition::Preserve,
        recorded_by: "fixture-agent".into(),
    };
    assert_eq!(
        fixture
            .service
            .record_closeout_disposition(captured.operation, started.revision, decision.clone())
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    let updated = fixture
        .service
        .record_closeout_disposition(captured.operation, captured.revision, decision)
        .unwrap();
    assert_eq!(updated.stage, CloseoutStage::NeedsDecision);
    assert!(!updated.spec.conditional_no_loss);
    std::fs::write(fixture.root.join("ignored"), "later data").unwrap();
    let rescan = fixture
        .service
        .inventory_closeout(updated.operation, updated.revision)
        .unwrap();
    assert_ne!(rescan.inventory_digest, captured.inventory_digest);
    assert_eq!(
        fixture
            .service
            .closeout_inventory(
                rescan.operation,
                captured.inventory_digest.as_ref().unwrap(),
                0,
                20
            )
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert!(
        load(&fixture.service.connection().unwrap(), rescan.operation)
            .unwrap()
            .decisions
            .is_empty()
    );
}

#[test]
fn replaced_root_and_inside_preservation_are_not_authorized() {
    let fixture = Fixture::new();
    let mut spec = fixture.spec(false);
    spec.preservation_directory = Some(fixture.root.clone());
    assert_eq!(
        fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                spec
            )
            .unwrap_err()
            .code,
        IssueCode::InvalidInput
    );
    let started = fixture.begin(false);
    std::fs::rename(&fixture.root, fixture.root.with_extension("original")).unwrap();
    std::fs::create_dir(&fixture.root).unwrap();
    std::fs::write(fixture.root.join("replacement"), "must survive").unwrap();
    assert!(
        fixture
            .service
            .inventory_closeout(started.operation, started.revision)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.join("replacement")).unwrap(),
        b"must survive"
    );
}

#[test]
fn interrupted_scan_preserves_last_inventory_and_authorization() {
    let fixture = Fixture::new();
    let started = fixture.begin(true);
    let captured = fixture
        .service
        .inventory_closeout(started.operation, started.revision)
        .unwrap();
    let mut broken = fixture.service.clone();
    broken.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_inventory_written" {
            Err(io("fixture disk/checkpoint failure"))
        } else {
            Ok(())
        }
    }));
    assert!(
        broken
            .inventory_closeout(captured.operation, captured.revision)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .inspect_closeout(captured.operation)
            .unwrap(),
        captured
    );
}

#[test]
fn catalog_restore_cannot_reactivate_historical_conditional_authority() {
    let fixture = Fixture::new();
    let started = fixture.begin(true);
    let snapshot = fixture
        .service
        .backup(RequestId::new(), "pending-closeout".into())
        .unwrap();
    fixture
        .service
        .revoke_closeout(
            &fixture.client,
            RequestId::new(),
            started.operation,
            started.revision,
        )
        .unwrap();
    let review = fixture.service.review_restore(snapshot.id).unwrap();
    fixture
        .service
        .apply_restore(RequestId::new(), review.id)
        .unwrap();
    let restored = fixture.service.inspect_closeout(started.operation).unwrap();
    assert_eq!(restored.stage, CloseoutStage::RecoveryRequired);
    assert!(!restored.spec.conditional_no_loss);
    assert_eq!(
        fixture
            .service
            .inventory_closeout(restored.operation, restored.revision)
            .unwrap_err()
            .code,
        IssueCode::RecoveryRequired
    );
    assert!(fixture.root.exists());
}

fn capture(fixture: &Fixture, operation: OperationId) -> crate::execution::Capture {
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, StorageConfig,
    };
    let store = ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let invocation = Invocation {
        session_id: "workspace".into(),
        message_id: operation.to_string(),
        call_path: vec![RequestId::new().to_string()],
        tool: "workspace_closeout".into(),
        input: serde_json::json!({"operation":operation}),
        working_dir: None,
        received_result_digest: None,
    };
    let PreparedInvocation::New(run) = store.prepare(&invocation, "fixture").unwrap() else {
        panic!()
    };
    store.start(&run.id, "fixture").unwrap();
    Capture::create(store, run, StorageConfig::default()).unwrap()
}

#[tokio::test]
async fn bundle_restores_acquired_refs_detached_stash_and_reflog_without_source() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "user.name", "Fixture"]);
    git(
        &fixture.root,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(fixture.root.join("tracked"), "first").unwrap();
    git(&fixture.root, &["add", "tracked"]);
    git(&fixture.root, &["commit", "-qm", "first"]);
    git(
        &fixture.root,
        &[
            "update-ref",
            "refs/jcode/checkout-acquired/prior/feature",
            "HEAD",
        ],
    );
    git(
        &fixture.root,
        &["commit", "--allow-empty", "-qm", "reflog-only"],
    );
    let lost = git_text(&fixture.root, &["rev-parse", "HEAD"]);
    git(&fixture.root, &["reset", "--soft", "HEAD~1"]);
    std::fs::write(fixture.root.join("tracked"), "stash data").unwrap();
    git(&fixture.root, &["stash", "push", "-qm", "fixture"]);
    git(&fixture.root, &["checkout", "--detach", "-q"]);
    git(
        &fixture.root,
        &["commit", "--allow-empty", "-qm", "detached"],
    );
    let detached = git_text(&fixture.root, &["rev-parse", "HEAD"]);
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let record = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].reflog.contains(lost.trim()));
    assert!(
        snapshots[0]
            .refs
            .contains_key("refs/jcode/checkout-acquired/prior/feature")
    );
    let source_refs = git_text(&fixture.root, &["show-ref"]);
    let bundle = super::git::preserve(
        &fixture.service,
        record.operation,
        &snapshots[0],
        &fixture._directory.path().join("preserved"),
        &capture,
    )
    .await
    .unwrap();
    assert_eq!(source_refs, git_text(&fixture.root, &["show-ref"]));
    std::fs::rename(&fixture.root, fixture.root.with_extension("offline")).unwrap();
    let restored = bundle.parent().unwrap().join("restored.git");
    git(&restored, &["fsck", "--full"]);
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", lost.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", detached.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(&restored, &["show", "refs/stash:tracked"]).trim(),
        "stash data"
    );
    assert!(!restored.join("objects/info/alternates").exists());
    finish_capture(&capture);
}

fn finish_capture(capture: &crate::execution::Capture) {
    use jcode_tool_core::OutputCapture;
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
    capture
        .seal(output, crate::execution::RunState::Completed)
        .unwrap();
}

fn git_text(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn full_archive_restores_ignored_files_symlinks_hardlinks_and_metadata() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join(".gitignore"), "ignored\n").unwrap();
    let file = fixture.root.join("ignored");
    std::fs::write(&file, vec![91; 300_000]).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.fixture", "preserved"])
            .arg(&file)
            .status()
            .unwrap()
            .success()
    );
    std::fs::hard_link(&file, fixture.root.join("second-link")).unwrap();
    symlink("ignored", fixture.root.join("symlink")).unwrap();
    let mut spec = fixture.spec(false);
    spec.full_archive = true;
    let started = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            spec,
        )
        .unwrap();
    let capture = capture(&fixture, started.operation);
    let refreshed = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let preserved = fixture
        .service
        .preserve_closeout(refreshed.operation, refreshed.revision, &capture)
        .await
        .unwrap();
    assert_eq!(preserved.stage, CloseoutStage::NeedsDecision);
    assert!(preserved.preservation_digest.is_some());
    let stored = load(&fixture.service.connection().unwrap(), preserved.operation).unwrap();
    let restored = stored
        .preservation
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join("verified-restore");
    assert_eq!(
        std::fs::read(restored.join("ignored")).unwrap(),
        vec![91; 300_000]
    );
    assert_eq!(
        std::fs::read_link(restored.join("symlink")).unwrap(),
        PathBuf::from("ignored")
    );
    assert_eq!(
        std::fs::metadata(restored.join("ignored")).unwrap().ino(),
        std::fs::metadata(restored.join("second-link"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        std::fs::metadata(restored.join("ignored")).unwrap().mode() & 0o777,
        0o750
    );
    let xattr = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", "jcode.fixture"])
        .arg(restored.join("ignored"))
        .output()
        .unwrap();
    assert!(xattr.status.success());
    assert_eq!(xattr.stdout, b"preserved\n");
    assert!(fixture.root.join("ignored").exists());
    finish_capture(&capture);
}

#[tokio::test]
async fn historical_lfs_payloads_restore_and_missing_payload_blocks_preservation() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "user.name", "Fixture"]);
    git(
        &fixture.root,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(
        fixture.root.join(".gitattributes"),
        "asset filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    let mut objects = Vec::new();
    for data in [
        b"older-lfs-content".as_slice(),
        b"newer-lfs-content".as_slice(),
    ] {
        let oid = digest(data);
        let path = fixture
            .root
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(&oid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
        std::fs::write(
            fixture.root.join("asset"),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
                data.len()
            ),
        )
        .unwrap();
        git(
            &fixture.root,
            &[
                "-c",
                "filter.lfs.clean=",
                "-c",
                "filter.lfs.required=false",
                "add",
                "asset",
                ".gitattributes",
            ],
        );
        git(&fixture.root, &["commit", "-qm", "lfs-version"]);
        objects.push((oid, path, data.to_vec()));
    }
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let refreshed = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), refreshed.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    let bundle = super::git::preserve(
        &fixture.service,
        refreshed.operation,
        &snapshots[0],
        &fixture._directory.path().join("history"),
        &capture,
    )
    .await
    .unwrap();
    for (oid, _, data) in &objects {
        assert_eq!(
            std::fs::read(bundle.parent().unwrap().join("lfs").join(oid)).unwrap(),
            *data
        );
        assert_eq!(
            std::fs::read(
                bundle
                    .parent()
                    .unwrap()
                    .join("restored.git/lfs/objects")
                    .join(&oid[..2])
                    .join(&oid[2..4])
                    .join(oid)
            )
            .unwrap(),
            *data
        );
    }
    std::fs::remove_file(&objects[0].1).unwrap();
    let error = super::git::preserve(
        &fixture.service,
        refreshed.operation,
        &snapshots[0],
        &fixture._directory.path().join("missing-history"),
        &capture,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, IssueCode::PreservationIncomplete);
    assert!(fixture.root.exists());
    finish_capture(&capture);
}
