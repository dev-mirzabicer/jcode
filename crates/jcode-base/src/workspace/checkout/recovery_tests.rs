use super::lfs_test_server::LfsTestServer;
use super::*;
use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, StorageConfig};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn organization(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
}
struct Fixture {
    temp: tempfile::TempDir,
    service: WorkspaceService,
    source: PathBuf,
    destination: PathBuf,
    project: ProjectId,
    repository: RepositoryId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let service = WorkspaceService::new(&state);
        service.initialize(RequestId::new()).unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        std::fs::write(source.join("tracked.txt"), "committed").unwrap();
        git(&source, &["add", "tracked.txt"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "source",
            ],
        );
        let project = match organization(
            &service,
            OrganizationChange::CreateProject { name: "P".into() },
        ) {
            EntityId::Project(id) => id,
            _ => unreachable!(),
        };
        let repository = match organization(
            &service,
            OrganizationChange::CreateRepository {
                name: "R".into(),
                remotes: vec![],
            },
        ) {
            EntityId::Repository(id) => id,
            _ => unreachable!(),
        };
        organization(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        );
        let destination = temp.path().join("checkout");
        Self {
            temp,
            service,
            source,
            destination,
            project,
            repository,
        }
    }

    fn spec(&self) -> CloneSpec {
        let volume = self
            .service
            .resolver
            .containing_volume(self.temp.path())
            .unwrap();
        CloneSpec {
            home: Home::Project(self.project),
            repository: self.repository,
            name: "checkout".into(),
            source: CloneSource::Local {
                path: self.source.clone(),
            },
            base: CloneBase::Branch {
                name: "main".into(),
            },
            branch: CloneBranch::Detached,
            remotes: vec![],
            destination: CloneDestination::Custom {
                volume_uuid: volume.identity.as_str().into(),
                path: self.destination.clone(),
            },
            submodules: false,
            lfs: false,
            trusted_local_submodule_urls: vec![],
            trusted_lfs_urls: vec![],
        }
    }

    fn begin(&self) -> RequestId {
        let review = self
            .service
            .review_clone(self.service.status().unwrap().revision, self.spec())
            .unwrap();
        let request = RequestId::new();
        self.service.begin_clone(request, review.id).unwrap();
        request
    }

    fn capture(&self, request: RequestId) -> Capture {
        let store = ExecutionStore::open(self.temp.path()).unwrap();
        let invocation = Invocation {
            session_id: "workspace".into(),
            message_id: request.to_string(),
            call_path: vec![RequestId::new().to_string()],
            tool: "workspace_clone".into(),
            input: serde_json::json!({"request": request}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(run) = store.prepare(&invocation, "fixture").unwrap() else {
            panic!()
        };
        store.start(&run.id, "fixture").unwrap();
        Capture::create(store, run, StorageConfig::default()).unwrap()
    }
}

#[tokio::test]
async fn rename_and_catalog_commit_faults_reconcile_without_second_clone() {
    for stage in ["clone_renamed", "clone_catalog_committed"] {
        let fixture = Fixture::new();
        let request = fixture.begin();
        let capture = fixture.capture(request);
        let once = Arc::new(AtomicBool::new(false));
        let mut interrupted = fixture.service.clone();
        interrupted.fault = Some(Arc::new({
            let once = once.clone();
            move |at| {
                if at == stage && !once.swap(true, Ordering::SeqCst) {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        format!("synthetic stop at {stage}"),
                    ));
                }
                Ok(())
            }
        }));
        let failure = interrupted
            .execute_clone(request, &capture)
            .await
            .unwrap_err();
        assert!(failure.detail.contains(stage));
        assert!(
            fixture.destination.exists(),
            "rename already published physical checkout"
        );
        let before = git(&fixture.destination, &["rev-parse", "HEAD"]);
        let ready = fixture
            .service
            .execute_clone(request, &capture)
            .await
            .unwrap();
        assert_eq!(ready.state, CloneState::Ready);
        assert_eq!(before, ready.review.source_commit);
        assert_eq!(git(&fixture.destination, &["rev-parse", "HEAD"]), before);
        assert!(matches!(
            fixture
                .service
                .inspect(EntityId::Location(ready.location))
                .unwrap(),
            Entity::Location(Location {
                lifecycle: LocationLifecycle::Ready,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn cancellation_removes_only_empty_owned_stage_and_retains_nonempty_stage() {
    for (checkpoint, empty) in [("clone_stage_bound", true), ("clone_acquired", false)] {
        let fixture = Fixture::new();
        let request = fixture.begin();
        let capture = fixture.capture(request);
        let mut interrupted = fixture.service.clone();
        let service = fixture.service.clone();
        interrupted.fault = Some(Arc::new(move |at| {
            if at == checkpoint {
                service.request_clone_cancel(request)?;
            }
            Ok(())
        }));
        assert!(interrupted.execute_clone(request, &capture).await.is_err());
        let stopped = fixture.service.inspect_clone(request).unwrap();
        assert_eq!(stopped.state, CloneState::Cancelled);
        assert!(stopped.cancel_requested);
        assert!(!fixture.destination.exists());
        assert_eq!(stopped.stage.is_none(), empty);
        if let Some(path) = &stopped.stage {
            assert!(path.join(".git").exists());
            assert!(stopped.issue.as_ref().unwrap().detail.contains("retained"));
        }
        let review = fixture
            .service
            .review_clone(fixture.service.status().unwrap().revision, fixture.spec())
            .unwrap();
        let next = fixture
            .service
            .begin_clone(RequestId::new(), review.id)
            .unwrap();
        assert_ne!(stopped.operation, next.operation);
        assert_eq!(stopped, fixture.service.inspect_clone(request).unwrap());
    }
}

#[tokio::test]
async fn recursive_submodules_and_lfs_payloads_are_materialized_without_project_setup() {
    let fixture = Fixture::new();
    let lfs = LfsTestServer::start();
    let grand = fixture.temp.path().join("grand");
    std::fs::create_dir(&grand).unwrap();
    git(&grand, &["init", "-q", "-b", "main"]);
    std::fs::write(grand.join("deep.txt"), "recorded grandchild\n").unwrap();
    git(&grand, &["add", "deep.txt"]);
    git(
        &grand,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "grand",
        ],
    );
    let grand_bare = fixture.temp.path().join("grand.git");
    git(
        fixture.temp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            grand.to_str().unwrap(),
            grand_bare.to_str().unwrap(),
        ],
    );

    let child = fixture.temp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    git(&child, &["init", "-q", "-b", "main"]);
    git(&child, &["lfs", "install", "--local"]);
    git(&child, &["lfs", "track", "*.bin"]);
    std::fs::write(
        child.join(".lfsconfig"),
        format!("[lfs]\n\turl = {}\n", lfs.url()),
    )
    .unwrap();
    let child_bytes = b"nested binary payload, not a Git LFS pointer\n";
    std::fs::write(child.join("payload.bin"), child_bytes).unwrap();
    git(
        &child,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "../grand.git",
            "nested/grand",
        ],
    );
    git(&child, &["add", "."]);
    git(
        &child,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "child",
        ],
    );
    lfs.add_repo_objects(&child);
    let child_bare = fixture.temp.path().join("child.git");
    git(
        fixture.temp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            child.to_str().unwrap(),
            child_bare.to_str().unwrap(),
        ],
    );

    git(&fixture.source, &["lfs", "install", "--local"]);
    git(&fixture.source, &["lfs", "track", "*.bin"]);
    std::fs::write(
        fixture.source.join(".lfsconfig"),
        format!("[lfs]\n\turl = {}\n", lfs.url()),
    )
    .unwrap();
    let root_bytes = b"root binary payload, not an LFS pointer\n";
    std::fs::write(fixture.source.join("root.bin"), root_bytes).unwrap();
    std::fs::write(fixture.source.join("setup.sh"), "touch SHOULD_NOT_RUN\n").unwrap();
    git(
        &fixture.source,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "../child.git",
            "libs/child",
        ],
    );
    git(&fixture.source, &["add", "."]);
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "parent",
        ],
    );
    lfs.add_repo_objects(&fixture.source);
    let mut spec = fixture.spec();
    spec.submodules = true;
    spec.lfs = true;
    spec.trusted_local_submodule_urls = vec!["../child.git".into(), "../grand.git".into()];
    spec.trusted_lfs_urls = vec![lfs.url()];
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, spec)
        .unwrap();
    let request = RequestId::new();
    fixture.service.begin_clone(request, review.id).unwrap();
    let capture = fixture.capture(request);
    let ready = fixture
        .service
        .execute_clone(request, &capture)
        .await
        .unwrap_or_else(|error| panic!("clone: {error:?}; LFS requests: {:?}", lfs.events()));
    assert_eq!(ready.state, CloneState::Ready);
    assert_eq!(
        std::fs::read(fixture.destination.join("root.bin")).unwrap(),
        root_bytes
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("libs/child/payload.bin")).unwrap(),
        child_bytes
    );
    assert_eq!(
        std::fs::read_to_string(fixture.destination.join("libs/child/nested/grand/deep.txt"))
            .unwrap(),
        "recorded grandchild\n"
    );
    assert!(!fixture.destination.join("SHOULD_NOT_RUN").exists());
    assert!(!fixture.temp.path().join("SHOULD_NOT_RUN").exists());
    let submodule = fixture.destination.join("libs/child");
    let grandchild = submodule.join("nested/grand");
    for root in [&fixture.destination, &submodule, &grandchild] {
        git(root, &["fsck", "--full"]);
        let git_dir = git_dir(root);
        assert!(!git_dir.join("objects/info/alternates").exists());
    }
    #[cfg(unix)]
    {
        assert_no_shared_objects(&git_dir(&fixture.destination), &[git_dir(&fixture.source)]);
        assert_no_shared_objects(&git_dir(&submodule), &[child_bare.clone(), git_dir(&child)]);
        assert_no_shared_objects(
            &git_dir(&grandchild),
            &[grand_bare.clone(), git_dir(&grand)],
        );
    }
    std::fs::rename(&fixture.source, fixture.temp.path().join("source-offline")).unwrap();
    std::fs::rename(child_bare, fixture.temp.path().join("child-offline.git")).unwrap();
    std::fs::rename(grand_bare, fixture.temp.path().join("grand-offline.git")).unwrap();
    for root in [&fixture.destination, &submodule, &grandchild] {
        git(root, &["fsck", "--full"]);
    }
    let requests = lfs.events();
    assert!(
        requests
            .iter()
            .filter(|event| event.starts_with("POST /lfs/objects/batch"))
            .count()
            >= 2,
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .filter(|event| event.starts_with("GET /objects/"))
            .count()
            >= 2,
        "{requests:?}"
    );
}

fn git_dir(root: &Path) -> PathBuf {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        .canonicalize()
        .unwrap()
}

#[cfg(unix)]
fn assert_no_shared_objects(clone_git: &Path, sources: &[PathBuf]) {
    use std::os::unix::fs::MetadataExt;
    fn objects(root: &Path) -> Vec<(u64, u64)> {
        let mut stack = vec![root.join("objects")];
        let mut found = Vec::new();
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    stack.push(entry.path());
                } else if meta.is_file() {
                    found.push((meta.dev(), meta.ino()));
                }
            }
        }
        found
    }
    let original: std::collections::HashSet<_> =
        sources.iter().flat_map(|root| objects(root)).collect();
    for inode in objects(clone_git) {
        assert!(
            !original.contains(&inode),
            "clone object shares source inode {inode:?}"
        );
    }
}

#[tokio::test]
async fn file_remote_bare_repository_keeps_reviewed_commit_and_never_publishes_upstream() {
    let fixture = Fixture::new();
    let bare = fixture.temp.path().join("remote.git");
    git(
        fixture.temp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            fixture.source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "tag",
            "-am",
            "signed-off",
            "reviewed",
        ],
    );
    git(
        &fixture.source,
        &["push", bare.to_str().unwrap(), "refs/tags/reviewed"],
    );
    let before = git(&bare, &["rev-parse", "refs/heads/main"]);
    let mut spec = fixture.spec();
    let url = format!("file://{}", bare.canonicalize().unwrap().display());
    spec.source = CloneSource::Remote { url: url.clone() };
    spec.base = CloneBase::Tag {
        name: "reviewed".into(),
    };
    spec.branch = CloneBranch::Detached;
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, spec)
        .unwrap();
    assert_eq!(review.source_commit, before);
    assert_eq!(
        review.spec.remotes,
        vec![CloneRemote {
            name: "origin".into(),
            url: url.clone()
        }]
    );
    let request = RequestId::new();
    fixture.service.begin_clone(request, review.id).unwrap();
    let capture = fixture.capture(request);
    let record = fixture
        .service
        .execute_clone(request, &capture)
        .await
        .unwrap();
    assert_eq!(record.state, CloneState::Ready);
    assert_eq!(git(&fixture.destination, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        git(&fixture.destination, &["remote", "get-url", "origin"]),
        url
    );
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/main"]), before);
    assert!(
        !git_dir(&fixture.destination)
            .join("objects/info/alternates")
            .exists()
    );
    std::fs::rename(&bare, fixture.temp.path().join("offline.git")).unwrap();
    git(&fixture.destination, &["fsck", "--full"]);
}

#[test]
fn moving_source_ref_or_populating_destination_invalidates_review_before_effects() {
    let fixture = Fixture::new();
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, fixture.spec())
        .unwrap();
    std::fs::write(fixture.source.join("later.txt"), "later commit").unwrap();
    git(&fixture.source, &["add", "later.txt"]);
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "later",
        ],
    );
    let request = RequestId::new();
    assert_eq!(
        fixture
            .service
            .begin_clone(request, review.id)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert!(!fixture.destination.exists());
    assert!(fixture.service.inspect_clone(request).is_err());

    let fresh = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, fixture.spec())
        .unwrap();
    std::fs::create_dir(&fixture.destination).unwrap();
    std::fs::write(fixture.destination.join("sentinel"), "user-owned").unwrap();
    assert!(
        fixture
            .service
            .begin_clone(RequestId::new(), fresh.id)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("sentinel")).unwrap(),
        b"user-owned"
    );
}

#[tokio::test]
async fn injected_enospc_retains_owned_stage_and_ready_output_failure_is_separate() {
    let fixture = Fixture::new();
    let request = fixture.begin();
    let capture = fixture.capture(request);
    let once = Arc::new(AtomicBool::new(false));
    let mut interrupted = fixture.service.clone();
    interrupted.fault = Some(Arc::new({
        let once = once.clone();
        move |point| {
            if point == "clone_acquired" && !once.swap(true, Ordering::SeqCst) {
                Err(io(std::io::Error::from_raw_os_error(libc::ENOSPC)))
            } else {
                Ok(())
            }
        }
    }));
    let error = interrupted
        .execute_clone(request, &capture)
        .await
        .unwrap_err();
    assert_eq!(error.code, IssueCode::Io);
    let stopped = fixture.service.inspect_clone(request).unwrap();
    assert_eq!(stopped.state, CloneState::PreparationFailed);
    assert_eq!(stopped.issue.as_ref().unwrap().code, IssueCode::Io);
    assert!(!fixture.destination.exists());
    let stage = stopped.stage.unwrap();
    let before = fixture.service.resolver.bind_directory(&stage).unwrap();
    assert!(stage.join(".git").exists());
    let ready = fixture
        .service
        .execute_clone(request, &capture)
        .await
        .unwrap();
    assert_eq!(ready.state, CloneState::Ready);
    assert_eq!(ready.location, stopped.location);
    let after = fixture
        .service
        .resolver
        .bind_directory(&fixture.destination)
        .unwrap();
    assert_eq!(before.root_witness(), after.root_witness());
    let warned = fixture
        .service
        .record_clone_launch_failure(
            request,
            issue(IssueCode::Io, "injected retained-output seal failure"),
        )
        .unwrap();
    assert_eq!(warned.state, CloneState::Ready);
    assert_eq!(
        warned.output_issue.as_ref().unwrap().code,
        IssueCode::RecoveryRequired
    );
    assert!(warned.issue.is_none());
    assert_eq!(fixture.service.inspect_clone(request).unwrap(), warned);
    git(&fixture.destination, &["fsck", "--full"]);
}
