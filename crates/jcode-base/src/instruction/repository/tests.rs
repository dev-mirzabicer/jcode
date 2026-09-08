use super::git::GitRepository;
use super::lease::acquire_mutation_lease;
use super::mutation::{atomic_write, fingerprint};
use super::*;
use crate::instruction::{
    InstructionId, InstructionKind, InstructionMetadata, InstructionScope, TemplateMode,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    service: InstructionRepositoryService,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary instruction repository fixture");
        let home = root.path().join("jcode-home");
        let state = root.path().join("state");
        std::fs::create_dir_all(&home).expect("create fixture home");
        std::fs::create_dir_all(&state).expect("create fixture state");
        Self {
            service: InstructionRepositoryService::from_paths(&home, &state),
            _root: root,
            home,
            state,
        }
    }

    fn initialize(&self) -> InstructionStoreInitialization {
        self.service
            .initialize_global(&seed(), &[])
            .expect("initialize global repository")
    }
}

fn seed() -> InstructionStoreSeed {
    let files = [
        (
            "modules/common.md",
            managed("common", "module", "seed common body"),
        ),
        (
            "modules/other.md",
            managed("other", "module", "seed other body"),
        ),
        (
            "agents/worker.md",
            "---\nid: worker\nkind: agent\nname: Worker\ndescription: Synthetic worker\navailability: both\n---\n\nworker body"
                .to_string(),
        ),
        (
            "addenda/worker-project.md",
            "---\nid: worker-project\nkind: agent-addendum\ntarget: worker\n---\n\nproject addendum"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(path, content)| InstructionSeedFile {
        relative_path: PathBuf::from(path),
        content: content.into_bytes(),
    })
    .collect();
    InstructionStoreSeed {
        manifest: InstructionStoreManifest::current(),
        files,
    }
}

fn managed(id: &str, kind: &str, body: &str) -> String {
    format!("---\nid: {id}\nkind: {kind}\n---\n\n{body}")
}

#[test]
fn git_metadata_alias_reads_and_drafts_are_rejected() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    for path in [".GiT/config", ".GIT/HEAD", "skills/example/.Git/config"] {
        for policy in [
            InstructionReadPolicy::WorkingTreeOnly,
            InstructionReadPolicy::AllowHeadFallback,
        ] {
            let error = fixture
                .service
                .read_file(&repository, path, policy)
                .expect_err("Git metadata is not managed instruction content");
            assert_eq!(error.kind, InstructionRepositoryErrorKind::InvalidPath);
        }
        let error = fixture.service.open_draft(&repository, path).unwrap_err();
        assert_eq!(error.kind, InstructionRepositoryErrorKind::InvalidPath);
    }
    for path in [".github/guidance.md", ".gitignore", ".gitmodules"] {
        super::mutation::validate_relative_path(Path::new(path)).unwrap();
    }
}

#[test]
fn git_metadata_alias_commit_is_rejected_before_writing() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let head = git(&repository.root, &["rev-parse", "HEAD"]);
    let index = std::fs::read(repository.root.join(".git/index")).unwrap();
    let config = std::fs::read(repository.root.join(".git/config")).unwrap();
    let path = PathBuf::from(".GiT/instruction-write-probe");
    let request = InstructionCommitRequest {
        operation_id: "git-metadata-alias-probe".into(),
        message: "Synthetic reserved path probe".into(),
        expected_head: head.clone(),
        expected_files: vec![InstructionFileState {
            relative_path: path.clone(),
            fingerprint: InstructionTargetFingerprint::Missing,
            executable: false,
        }],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: path.clone(),
            content: b"synthetic metadata write".to_vec(),
        }],
    };
    let error = fixture.service.commit(&repository, &request).unwrap_err();
    assert!(
        !repository.root.join(&path).exists(),
        "Rejected commits must not write through a Git-directory alias"
    );
    assert_eq!(error.kind, InstructionRepositoryErrorKind::InvalidPath);
    assert!(error.existing_state_unchanged);
    assert_eq!(git(&repository.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        std::fs::read(repository.root.join(".git/config")).unwrap(),
        config
    );
}

#[test]
fn git_metadata_alias_seed_is_rejected_before_materialization() {
    let fixture = Fixture::new();
    let mut seed = seed();
    seed.files.push(InstructionSeedFile {
        relative_path: ".GiT/instruction-seed-probe".into(),
        content: b"synthetic metadata seed".to_vec(),
    });
    let error = fixture.service.initialize_global(&seed, &[]).unwrap_err();
    assert_eq!(error.kind, InstructionRepositoryErrorKind::InvalidPath);
    let repository = fixture.service.global_repository().unwrap();
    assert!(!repository.root.join(".GiT/instruction-seed-probe").exists());
}

#[test]
fn blob_batch_rejects_missing_non_blob_and_unsafe_entries() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let git = GitRepository::new(&repository.root);
    let head = git.head().unwrap().unwrap();
    let entry = git.tree_entries(&head).unwrap().into_iter().next().unwrap();
    for object_id in ["0".repeat(head.len()), head.clone()] {
        let output = tempfile::tempdir_in(&fixture.state).unwrap();
        let mut invalid = entry.clone();
        invalid.object_id = object_id;
        assert!(git.materialize_blobs(&[invalid], output.path()).is_err());
        assert_eq!(std::fs::read_dir(output.path()).unwrap().count(), 0);
    }
    for path in ["../escape", ".git/config", ".GiT/config"] {
        let output = tempfile::tempdir_in(&fixture.state).unwrap();
        let mut invalid = entry.clone();
        invalid.path = path.into();
        assert!(git.materialize_blobs(&[invalid], output.path()).is_err());
        assert_eq!(std::fs::read_dir(output.path()).unwrap().count(), 0);
    }
    let output = tempfile::tempdir_in(&fixture.state).unwrap();
    let mut invalid = entry;
    invalid.mode = "120000".into();
    assert!(git.materialize_blobs(&[invalid], output.path()).is_err());
    assert_eq!(git.head().unwrap().as_deref(), Some(head.as_str()));
}

#[test]
fn committed_snapshot_batches_literal_blobs_without_touching_working_state() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let assets = repository.root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let binary = [0xff, 0, b'\n', b'X'].repeat(600_000);
    std::fs::write(assets.join("hidden.bin"), &binary).unwrap();
    std::fs::write(assets.join("subst.txt"), "$Format:%H$\n").unwrap();
    std::fs::write(assets.join("empty"), []).unwrap();
    // The query stream exceeds normal pipe capacity; file-backed batch I/O
    // must still complete even when the first blob is larger than a pipe.
    for index in 0..1700 {
        std::fs::write(
            assets.join(format!("file-{index:04}")),
            format!("SYNTHETIC {index}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        repository.root.join(".gitattributes"),
        "assets/hidden.bin export-ignore\nassets/subst.txt export-subst\nassets/* filter=fixture\n",
    )
    .unwrap();
    git(&repository.root, &["add", "."]);
    git(
        &repository.root,
        &["commit", "-m", "synthetic snapshot fixtures"],
    );
    git(
        &repository.root,
        &["config", "filter.fixture.smudge", "false"],
    );
    std::fs::write(assets.join("subst.txt"), "UNCOMMITTED").unwrap();
    let before_head = git(&repository.root, &["rev-parse", "HEAD"]);
    let before_index = std::fs::read(repository.root.join(".git/index")).unwrap();
    let before_status = git(&repository.root, &["status", "--porcelain"]);
    let started = Instant::now();
    let (snapshot, issues) = fixture
        .service
        .committed_validation_snapshot(&repository)
        .unwrap();
    assert!(issues.is_empty());
    assert_eq!(
        std::fs::read(snapshot.path().join("assets/hidden.bin")).unwrap(),
        binary
    );
    assert_eq!(
        std::fs::read_to_string(snapshot.path().join("assets/subst.txt")).unwrap(),
        "$Format:%H$\n"
    );
    assert!(
        std::fs::read(snapshot.path().join("assets/empty"))
            .unwrap()
            .is_empty()
    );
    for index in 0..1700 {
        assert_eq!(
            std::fs::read_to_string(snapshot.path().join(format!("assets/file-{index:04}")))
                .unwrap(),
            format!("SYNTHETIC {index}\n")
        );
    }
    assert_eq!(git(&repository.root, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        before_index
    );
    assert_eq!(
        git(&repository.root, &["status", "--porcelain"]),
        before_status
    );
    println!(
        "1700-file exact snapshot completed in {:?}",
        started.elapsed()
    );
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_string()
}

fn init_plain_git(root: &Path) {
    std::fs::create_dir_all(root).expect("create Git fixture root");
    git(root, &["init", "--initial-branch", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(root.join("README.md"), "fixture\n").expect("write initial fixture");
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-m", "fixture: initialize"]);
}

fn draft_commit(
    service: &InstructionRepositoryService,
    repository: &InstructionRepositoryRef,
    path: &str,
    content: &str,
    operation: &str,
) -> InstructionCommitOutcome {
    let draft = service.open_draft(repository, path).expect("open draft");
    service
        .commit(
            repository,
            &InstructionCommitRequest {
                operation_id: operation.to_string(),
                message: format!("instruction: {operation}"),
                expected_head: draft.base_head,
                expected_files: vec![draft.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from(path),
                    content: content.as_bytes().to_vec(),
                }],
            },
        )
        .expect("commit draft")
}

#[test]
fn roster_validation_participates_in_initialization_commits_retry_and_project_scope() {
    use crate::model_roster::ROSTER_PATH;
    let fixture = Fixture::new();
    let mut seed = seed();
    let valid = "[aliases.fixture]\ndescription='synthetic'\nmodels=['openai-oauth:fixture']\n";
    seed.files.push(InstructionSeedFile {
        relative_path: ROSTER_PATH.into(),
        content: valid.as_bytes().to_vec(),
    });
    let initialized = fixture.service.initialize_global(&seed, &[]).unwrap();
    let repo = &initialized.repository;
    let draft = fixture.service.open_draft(repo, ROSTER_PATH).unwrap();
    let request = InstructionCommitRequest {
        operation_id: "invalid-roster".into(),
        message: "fixture invalid roster".into(),
        expected_head: draft.base_head,
        expected_files: vec![draft.base],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: ROSTER_PATH.into(),
            content: b"[aliases.fixture]\ndescription='synthetic'\nmodels=[]\n".to_vec(),
        }],
    };
    assert!(fixture.service.commit(repo, &request).is_err());
    assert!(
        fixture.service.commit(repo, &request).is_err(),
        "retry cannot bless a failed working edit"
    );
    assert_eq!(git(&repo.root, &["rev-parse", "HEAD"]), initialized.commit);
    assert!(
        !fixture
            .service
            .validate_repository(repo)
            .unwrap()
            .diagnostics
            .is_empty()
    );
    draft_commit(
        &fixture.service,
        repo,
        "modules/other.md",
        &managed("other", "module", "independent edit"),
        "independent-roster-error",
    );
    let mut project = repo.clone();
    project.kind = InstructionRepositoryKind::ProjectExternal;
    assert!(
        !fixture
            .service
            .validate_repository(&project)
            .unwrap()
            .diagnostics
            .is_empty()
    );
    let invalid_fixture = Fixture::new();
    seed.files.last_mut().unwrap().content =
        b"[aliases.bad]\ndescription='synthetic'\nmodels=[]\n".to_vec();
    assert!(
        invalid_fixture
            .service
            .initialize_global(&seed, &[])
            .is_err()
    );
}

#[test]
fn global_initialization_is_private_idempotent_and_damage_is_explicit() {
    let fixture = Fixture::new();
    let repository = fixture.service.global_repository().unwrap();
    let before = fixture.service.inspect(&repository).unwrap();
    assert_eq!(before.health, InstructionRepositoryHealth::Uninitialized);

    let initialized = fixture.initialize();
    assert!(initialized.created);
    let ready = fixture.service.inspect(&repository).unwrap();
    assert_eq!(ready.health, InstructionRepositoryHealth::Ready);
    assert_eq!(ready.branch.as_deref(), Some("main"));
    assert!(ready.changes.is_empty());
    assert_eq!(ready.head.as_deref(), Some(initialized.commit.as_str()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&repository.root)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(repository.root.join("instruction-store.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let repeated = fixture
        .service
        .initialize_global(&seed(), &[])
        .expect("repeat initialization");
    assert!(!repeated.created);
    assert_eq!(repeated.commit, initialized.commit);

    std::fs::remove_file(repository.root.join("instruction-store.toml")).expect("remove manifest");
    let damaged = fixture.service.inspect(&repository).unwrap();
    let InstructionRepositoryHealth::Damaged(damage) = damaged.health else {
        panic!("expected damaged store");
    };
    assert_eq!(
        damage.kind,
        InstructionRepositoryDamageKind::MissingManifest
    );
    assert!(damage.git_head_recovery_available);
    let init_error = fixture
        .service
        .initialize_global(&seed(), &[])
        .expect_err("initialized damage must not recreate silently");
    assert_eq!(
        init_error.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );

    fixture
        .service
        .restore_working_file_from_head(&repository, "instruction-store.toml", "repair-manifest")
        .expect("restore committed manifest");
    assert_eq!(
        fixture.service.inspect(&repository).unwrap().health,
        InstructionRepositoryHealth::Ready
    );

    std::fs::write(repository.root.join("instruction-store.toml"), "broken = [")
        .expect("corrupt manifest");
    let recreated = fixture
        .service
        .recreate_from_seed(&repository, &seed(), &[], "main")
        .expect("explicit seed recreation");
    assert!(
        recreated
            .damaged_backup
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    assert_eq!(
        fixture.service.inspect(&repository).unwrap().health,
        InstructionRepositoryHealth::Ready
    );
}

#[test]
fn shipped_seed_upgrade_adds_only_new_resources_and_never_recreates_user_deletions() {
    let fixture = Fixture::new();
    let mut initial_seed = seed();
    initial_seed.manifest.seed_version = 1;
    let initialized = fixture
        .service
        .initialize_global(&initial_seed, &[])
        .expect("initialize prior shipped seed");
    let repository = initialized.repository;

    let user_edit = managed("common", "module", "user working edit");
    std::fs::write(repository.root.join("modules/common.md"), &user_edit)
        .expect("write unrelated valid working edit");

    let mut upgraded_seed = initial_seed.clone();
    upgraded_seed.manifest.seed_version = 2;
    upgraded_seed.files.push(InstructionSeedFile {
        relative_path: PathBuf::from("notifications/agent-transition.md"),
        content: managed(
            "agent-transition",
            "notification",
            "synthetic transition prose",
        )
        .into_bytes(),
    });
    let upgraded = fixture
        .service
        .ensure_shipped_seed(&repository, &upgraded_seed)
        .expect("upgrade shipped seed")
        .expect("upgrade commit");
    assert_eq!(upgraded.disposition, InstructionCommitDisposition::Created);
    assert_eq!(
        std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
        user_edit,
        "seed upgrades must not overwrite existing working resources"
    );
    assert_eq!(
        fixture
            .service
            .load_manifest(&repository)
            .unwrap()
            .seed_version,
        2
    );
    assert!(
        repository
            .root
            .join("notifications/agent-transition.md")
            .is_file()
    );
    assert!(
        fixture
            .service
            .ensure_shipped_seed(&repository, &upgraded_seed)
            .expect("repeat upgrade")
            .is_none(),
        "an adopted seed must be a no-op"
    );

    let draft = fixture
        .service
        .open_draft(&repository, "notifications/agent-transition.md")
        .expect("open seeded notification");
    fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "delete-seeded-notification".to_string(),
                message: "instruction: delete seeded notification".to_string(),
                expected_head: draft.base_head,
                expected_files: vec![draft.base],
                mutations: vec![InstructionFileMutation::Delete {
                    relative_path: PathBuf::from("notifications/agent-transition.md"),
                }],
            },
        )
        .expect("delete adopted seeded resource");
    assert!(
        fixture
            .service
            .ensure_shipped_seed(&repository, &upgraded_seed)
            .expect("check adopted seed after deletion")
            .is_none()
    );
    assert!(
        !repository
            .root
            .join("notifications/agent-transition.md")
            .exists(),
        "a later user deletion must remain authoritative"
    );
    upgraded_seed.manifest.seed_version = 3;
    fixture
        .service
        .ensure_shipped_seed(&repository, &upgraded_seed)
        .expect("adopt next seed after committed deletion");
    assert!(
        !repository
            .root
            .join("notifications/agent-transition.md")
            .exists(),
        "a later seed version must not resurrect a committed user deletion"
    );
}

#[test]
fn recreation_preflights_invalid_seed_and_branch_before_moving_the_store() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    let repository = initialized.repository;
    let original_head = initialized.commit;
    let invalid_seed = InstructionStoreSeed {
        manifest: InstructionStoreManifest::current(),
        files: vec![InstructionSeedFile {
            relative_path: PathBuf::from("system/invalid.md"),
            content: b"---\nid: invalid\nkind: system\nincludes:\n  - missing\n---\n\ninvalid"
                .to_vec(),
        }],
    };
    let invalid = fixture
        .service
        .recreate_from_seed(&repository, &invalid_seed, &[], "main")
        .expect_err("invalid replacement seed must fail before moving the store");
    assert!(invalid.existing_state_unchanged);
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        Some(original_head.clone())
    );
    let invalid_branch = fixture
        .service
        .recreate_from_seed(&repository, &seed(), &[], "bad..branch")
        .expect_err("invalid replacement branch must fail before moving the store");
    assert!(invalid_branch.existing_state_unchanged);
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        Some(original_head)
    );
}

#[test]
fn recreation_failure_after_backup_reports_changed_state_and_backup_path() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    super::service::fail_recreation_after_backup_once();
    let error = fixture
        .service
        .recreate_from_seed(&repository, &seed(), &[], "main")
        .expect_err("injected post-backup recreation failure");
    assert!(!error.existing_state_unchanged);
    let backup = error.path.expect("reported backup path");
    assert!(backup.exists());
    assert!(error.detail.contains(&backup.display().to_string()));
    assert!(!repository.root.exists());
}

#[test]
fn interrupted_initialization_resumes_only_with_its_durable_attempt_record() {
    let fixture = Fixture::new();
    let repository = fixture.service.global_repository().unwrap();
    std::fs::create_dir_all(repository.root.join("modules")).unwrap();
    std::fs::write(
        repository.root.join("modules/common.md"),
        managed("common", "module", "partial interrupted bytes"),
    )
    .unwrap();
    let unowned = fixture
        .service
        .initialize_global(&seed(), &[])
        .expect_err("unowned nonempty directory must not be adopted");
    assert_eq!(
        unowned.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );

    let attempt = fixture
        .state
        .join("instruction-repositories/initializing/global.json");
    crate::storage::write_json_secret(
        &attempt,
        &serde_json::json!({
            "repository_id": "global",
            "root": repository.root.clone(),
            "branch": "main",
        }),
    )
    .unwrap();
    let resumed = fixture
        .service
        .initialize_global(&seed(), &[])
        .expect("resume owned interrupted initialization");
    assert!(resumed.created);
    assert!(!attempt.exists());
    assert_eq!(
        fixture.service.inspect(&resumed.repository).unwrap().health,
        InstructionRepositoryHealth::Ready
    );
    assert!(
        std::fs::read_to_string(resumed.repository.root.join("modules/common.md"))
            .unwrap()
            .contains("seed common body")
    );
}

#[test]
fn initialization_reuse_rejects_invalid_working_resources() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    std::fs::write(
        repository.root.join("modules/common.md"),
        "---\nid: common\nkind: module\nincludes:\n  - missing\n---\n\ninvalid graph",
    )
    .unwrap();
    let error = fixture
        .service
        .initialize_global(&seed(), &[])
        .expect_err("initialization reuse must validate the complete working store");
    assert_eq!(
        error.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
}

#[test]
fn scoped_commit_preserves_unrelated_index_and_worktree_and_is_idempotent() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    let repository = initialized.repository;

    std::fs::write(repository.root.join("unrelated.txt"), "staged unrelated\n").unwrap();
    git(&repository.root, &["add", "unrelated.txt"]);
    std::fs::write(
        repository.root.join("modules/other.md"),
        managed("other", "module", "external dirty other"),
    )
    .unwrap();

    let draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let request = InstructionCommitRequest {
        operation_id: "save-common".to_string(),
        message: "instruction: update common".to_string(),
        expected_head: draft.base_head,
        expected_files: vec![draft.base],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: PathBuf::from("modules/common.md"),
            content: managed("common", "module", "updated common body").into_bytes(),
        }],
    };
    let committed = fixture.service.commit(&repository, &request).unwrap();
    assert_eq!(committed.disposition, InstructionCommitDisposition::Created);
    assert_eq!(
        git(&repository.root, &["diff", "--cached", "--name-only"]),
        "unrelated.txt"
    );
    assert!(
        git(&repository.root, &["status", "--short"])
            .lines()
            .any(|line| line.ends_with("modules/other.md"))
    );
    let committed_paths = git(
        &repository.root,
        &["show", "--format=", "--name-only", &committed.commit],
    );
    assert_eq!(committed_paths, "modules/common.md");
    assert!(
        git(
            &repository.root,
            &["show", &format!("{}:modules/common.md", committed.commit)]
        )
        .contains("updated common body")
    );
    let repeated = fixture.service.commit(&repository, &request).unwrap();
    assert_eq!(
        repeated.disposition,
        InstructionCommitDisposition::AlreadyCommitted
    );
    assert_eq!(repeated.commit, committed.commit);

    let current = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let unchanged = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "save-common-noop".to_string(),
                message: "instruction: no-op".to_string(),
                expected_head: current.base_head,
                expected_files: vec![current.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: current.content.unwrap().into_bytes(),
                }],
            },
        )
        .unwrap();
    assert_eq!(
        unchanged.disposition,
        InstructionCommitDisposition::NoChange
    );

    let interrupted = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let interrupted_content = managed("common", "module", "written before interrupted commit");
    atomic_write(
        &repository,
        Path::new("modules/common.md"),
        interrupted_content.as_bytes(),
    )
    .unwrap();
    let recovered = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "recover-precommit-write".to_string(),
                message: "instruction: recover interrupted write".to_string(),
                expected_head: interrupted.base_head,
                expected_files: vec![interrupted.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: interrupted_content.into_bytes(),
                }],
            },
        )
        .expect("retry after write-before-commit interruption");
    assert_eq!(recovered.disposition, InstructionCommitDisposition::Created);

    let stale = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    std::fs::write(
        repository.root.join("modules/common.md"),
        managed("common", "module", "external edit wins"),
    )
    .unwrap();
    let stale_error = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "stale-save".to_string(),
                message: "instruction: stale".to_string(),
                expected_head: stale.base_head,
                expected_files: vec![stale.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: b"stale replacement".to_vec(),
                }],
            },
        )
        .expect_err("stale draft must fail");
    assert_eq!(stale_error.kind, InstructionRepositoryErrorKind::StaleDraft);
    assert!(
        std::fs::read_to_string(repository.root.join("modules/common.md"))
            .unwrap()
            .contains("external edit wins")
    );
}

#[test]
fn generic_commit_rejects_an_invalid_store_manifest() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let draft = fixture
        .service
        .open_draft(&repository, "instruction-store.toml")
        .unwrap();
    let head = draft.base_head.clone();
    let error = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "invalid-manifest".to_string(),
                message: "instruction: invalid manifest".to_string(),
                expected_head: draft.base_head,
                expected_files: vec![draft.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("instruction-store.toml"),
                    content: b"schema_version = 999\n".to_vec(),
                }],
            },
        )
        .expect_err("invalid manifest must not be published");
    assert_eq!(
        error.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        Some(head)
    );
}

#[test]
fn completed_operation_lookup_requires_exact_identity() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    draft_commit(
        &fixture.service,
        &repository,
        "modules/common.md",
        &managed("common", "module", "first"),
        "save-longer",
    );
    assert_eq!(
        fixture
            .service
            .completed_operation_commit(&repository, "save")
            .unwrap(),
        None
    );
}

#[test]
fn completed_save_retry_preserves_newer_staged_target() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let request = InstructionCommitRequest {
        operation_id: "save-preserve-stage".into(),
        message: "instruction: update common".into(),
        expected_head: draft.base_head,
        expected_files: vec![draft.base],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: "modules/common.md".into(),
            content: managed("common", "module", "saved").into_bytes(),
        }],
    };
    let committed = fixture.service.commit(&repository, &request).unwrap();
    std::fs::write(
        repository.root.join("modules/common.md"),
        managed("common", "module", "new staged intent"),
    )
    .unwrap();
    git(&repository.root, &["add", "modules/common.md"]);
    let index = std::fs::read(repository.root.join(".git/index")).unwrap();
    let retry = fixture.service.commit(&repository, &request).unwrap();
    assert_eq!(retry.commit, committed.commit);
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        index
    );
}

#[test]
fn commit_review_is_complete_non_mutating_and_validates_published_dependencies() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let path = "modules/common.md";
    let draft = fixture.service.open_draft(&repository, path).unwrap();
    // The live renderer can find this source, but the scoped commit will not
    // contain it unless the user explicitly includes it in the transaction.
    let extra = managed("uncommitted", "module", "uncommitted dependency");
    std::fs::write(repository.root.join("modules/uncommitted.md"), &extra).unwrap();
    std::fs::write(repository.root.join("unrelated.txt"), "staged sentinel").unwrap();
    git(&repository.root, &["add", "unrelated.txt"]);
    let index = std::fs::read(repository.root.join(".git/index")).unwrap();
    let original = std::fs::read(repository.root.join(path)).unwrap();
    let content = "---\nid: common\nkind: module\ntemplate: handlebars\n---\n{{> uncommitted}}";
    let mut request = InstructionCommitRequest {
        operation_id: "review-dependencies".into(),
        message: "instruction: update synthetic module".into(),
        expected_head: draft.base_head.clone(),
        expected_files: vec![draft.base],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: path.into(),
            content: content.as_bytes().to_vec(),
        }],
    };
    let invalid = fixture
        .service
        .review_commit(&repository, &request)
        .unwrap();
    assert!(!invalid.errors.is_empty());
    assert_eq!(invalid.files[0].working.as_ref().unwrap(), &original);
    assert_eq!(
        invalid.files[0].proposed.as_deref(),
        Some(content.as_bytes())
    );
    assert_eq!(std::fs::read(repository.root.join(path)).unwrap(), original);
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        git(&repository.root, &["rev-parse", "HEAD"]),
        draft.base_head
    );
    let extra_draft = fixture
        .service
        .open_draft(&repository, "modules/uncommitted.md")
        .unwrap();
    request.expected_files.push(extra_draft.base);
    request.mutations.push(InstructionFileMutation::Write {
        relative_path: "modules/uncommitted.md".into(),
        content: extra.into_bytes(),
    });
    let valid = fixture
        .service
        .review_commit(&repository, &request)
        .unwrap();
    assert!(valid.errors.is_empty(), "{:?}", valid.errors);
    assert_eq!(valid.files.len(), 2);
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        index
    );
    std::fs::write(
        repository.root.join(path),
        managed("common", "module", "newer intent"),
    )
    .unwrap();
    let error = fixture
        .service
        .review_commit(&repository, &request)
        .unwrap_err();
    assert_eq!(error.kind, InstructionRepositoryErrorKind::StaleDraft);
    assert!(error.existing_state_unchanged);
}

#[test]
fn draft_branch_identity_and_large_complete_review_are_preserved() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let content = managed("common", "module", &"合成 🦀\n".repeat(200_000));
    let review = fixture
        .service
        .review_commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "review-large".into(),
                message: "instruction: review complete source".into(),
                expected_head: draft.base_head.clone(),
                expected_files: vec![draft.base.clone()],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: draft.relative_path.clone(),
                    content: content.as_bytes().to_vec(),
                }],
            },
        )
        .unwrap();
    assert!(review.errors.is_empty());
    assert_eq!(
        review.files[0].proposed.as_deref(),
        Some(content.as_bytes())
    );
    git(&repository.root, &["switch", "-c", "other"]);
    assert_eq!(
        git(&repository.root, &["rev-parse", "HEAD"]),
        draft.base_head
    );
    assert_eq!(
        fixture.service.validate_draft(&draft).unwrap_err().kind,
        InstructionRepositoryErrorKind::StaleDraft
    );
}

#[test]
fn editing_drafts_persist_without_saving_and_block_branch_changes_until_closed() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    let opened = workspace
        .begin(
            &fixture.service,
            &repository,
            "session-a",
            &["modules/common.md".into()],
            "instruction: edit common",
        )
        .unwrap()
        .clone();
    let original = std::fs::read(repository.root.join("modules/common.md")).unwrap();
    let revised = workspace
        .revise(
            &fixture.service,
            &opened.id,
            0,
            vec![InstructionFileMutation::Write {
                relative_path: "modules/common.md".into(),
                content: managed("common", "module", "draft only").into_bytes(),
            }],
        )
        .unwrap()
        .clone();
    assert!(workspace.save(&fixture.service, &opened.id, 0).is_err());
    assert!(workspace.save(&fixture.service, &opened.id, 1).is_err());
    assert_eq!(
        fixture
            .service
            .checkout_branch(&repository, "branch-during-draft", "other", true, None)
            .unwrap_err()
            .kind,
        InstructionRepositoryErrorKind::MutationBusy
    );
    assert_eq!(
        std::fs::read(repository.root.join("modules/common.md")).unwrap(),
        original
    );
    assert!(
        fixture
            .service
            .read_editing_draft(&repository, "wrong-session", &opened.id)
            .is_err()
    );
    workspace.close();
    let restored = workspace
        .resume(&fixture.service, &repository, "session-a", &opened.id)
        .unwrap();
    assert_eq!(restored.request, revised.request);
    assert_eq!(restored.generation, 1);
    let mut second = InstructionDraftWorkspace::default();
    assert!(
        second
            .resume(&fixture.service, &repository, "session-a", &opened.id)
            .is_err()
    );
    drop(workspace);
    fixture
        .service
        .checkout_branch(&repository, "branch-after-disconnect", "other", true, None)
        .unwrap();
    second
        .resume(&fixture.service, &repository, "session-a", &opened.id)
        .unwrap();
    assert!(second.review(&fixture.service, &opened.id, 1).is_err());
    assert_eq!(
        std::fs::read(repository.root.join("modules/common.md")).unwrap(),
        original
    );
}

#[test]
fn editing_drafts_recover_lost_commit_receipt_and_reject_competing_save() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut first = InstructionDraftWorkspace::default();
    let mut second = InstructionDraftWorkspace::default();
    let first_id = first
        .begin(
            &fixture.service,
            &repository,
            "session-a",
            &["modules/common.md".into()],
            "instruction: first",
        )
        .unwrap()
        .id
        .clone();
    let second_id = second
        .begin(
            &fixture.service,
            &repository,
            "session-b",
            &["modules/common.md".into()],
            "instruction: second",
        )
        .unwrap()
        .id
        .clone();
    for (workspace, id, body) in [
        (&mut first, &first_id, "first"),
        (&mut second, &second_id, "second"),
    ] {
        workspace
            .revise(
                &fixture.service,
                id,
                0,
                vec![InstructionFileMutation::Write {
                    relative_path: "modules/common.md".into(),
                    content: managed("common", "module", body).into_bytes(),
                }],
            )
            .unwrap();
        assert!(
            workspace
                .review(&fixture.service, id, 1)
                .unwrap()
                .errors
                .is_empty()
        );
    }
    let record_path = fixture
        .state
        .join("instruction-repositories/drafts/global")
        .join(format!("{first_id}.json"));
    let pre_commit_record = std::fs::read(&record_path).unwrap();
    let committed = first.save(&fixture.service, &first_id, 1).unwrap();
    assert_eq!(committed.disposition, InstructionCommitDisposition::Created);
    assert!(second.save(&fixture.service, &second_id, 1).is_err());
    assert!(second.draft().is_some());
    // A process died after Git publication but before its draft receipt write.
    std::fs::write(&record_path, pre_commit_record).unwrap();
    drop(first);
    let mut recovered = InstructionDraftWorkspace::default();
    recovered
        .resume(&fixture.service, &repository, "session-a", &first_id)
        .unwrap();
    let replay = recovered.save(&fixture.service, &first_id, 1).unwrap();
    assert_eq!(replay.commit, committed.commit);
    assert_eq!(
        replay.disposition,
        InstructionCommitDisposition::AlreadyCommitted
    );
    assert_eq!(git(&repository.root, &["rev-list", "--count", "HEAD"]), "2");
}

#[test]
fn editing_drafts_resume_persisted_partial_write_without_overwriting_newer_intent() {
    for diverged in [false, true] {
        let fixture = Fixture::new();
        let repository = fixture.initialize().repository;
        let mut workspace = InstructionDraftWorkspace::default();
        let id = workspace
            .begin(
                &fixture.service,
                &repository,
                "session-a",
                &["modules/common.md".into()],
                "instruction: recover edit",
            )
            .unwrap()
            .id
            .clone();
        let proposed = managed("common", "module", "proposed");
        workspace
            .revise(
                &fixture.service,
                &id,
                0,
                vec![InstructionFileMutation::Write {
                    relative_path: "modules/common.md".into(),
                    content: proposed.as_bytes().to_vec(),
                }],
            )
            .unwrap();
        workspace.review(&fixture.service, &id, 1).unwrap();
        let record_path = fixture
            .state
            .join("instruction-repositories/drafts/global")
            .join(format!("{id}.json"));
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        record["save_started"] = true.into();
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let written = if diverged {
            managed("common", "module", "newer external edit")
        } else {
            proposed
        };
        std::fs::write(repository.root.join("modules/common.md"), &written).unwrap();
        drop(workspace);
        let mut recovered = InstructionDraftWorkspace::default();
        recovered
            .resume(&fixture.service, &repository, "session-a", &id)
            .unwrap();
        let outcome = recovered.save(&fixture.service, &id, 1);
        assert_eq!(outcome.is_err(), diverged);
        assert_eq!(
            std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
            written
        );
        assert_eq!(
            git(&repository.root, &["rev-list", "--count", "HEAD"]),
            if diverged { "1" } else { "2" }
        );
    }
}

#[test]
fn editing_draft_noop_is_durable_and_repeated_without_a_commit() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    let id = workspace
        .begin(
            &fixture.service,
            &repository,
            "session-a",
            &["modules/common.md".into()],
            "instruction: no change",
        )
        .unwrap()
        .id
        .clone();
    workspace.review(&fixture.service, &id, 0).unwrap();
    assert_eq!(
        workspace
            .save(&fixture.service, &id, 0)
            .unwrap()
            .disposition,
        InstructionCommitDisposition::NoChange
    );
    drop(workspace);
    let mut resumed = InstructionDraftWorkspace::default();
    resumed
        .resume(&fixture.service, &repository, "session-a", &id)
        .unwrap();
    assert_eq!(
        resumed.save(&fixture.service, &id, 0).unwrap().disposition,
        InstructionCommitDisposition::NoChange
    );
    assert_eq!(git(&repository.root, &["rev-list", "--count", "HEAD"]), "1");
}

#[test]
fn draft_review_validates_default_agent_without_mutating_manifest() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let draft = fixture
        .service
        .open_draft(&repository, "instruction-store.toml")
        .unwrap();
    let original = draft.content.clone().unwrap();
    for (selector, valid) in [("worker", true), ("global:missing", false)] {
        let mut manifest = fixture.service.load_manifest(&repository).unwrap();
        manifest.default_agent = Some(selector.into());
        let review = fixture
            .service
            .review_commit(
                &repository,
                &InstructionCommitRequest {
                    operation_id: "review-default".into(),
                    message: "instruction: set default agent".into(),
                    expected_head: draft.base_head.clone(),
                    expected_files: vec![draft.base.clone()],
                    mutations: vec![InstructionFileMutation::Write {
                        relative_path: draft.relative_path.clone(),
                        content: toml::to_string(&manifest).unwrap().into_bytes(),
                    }],
                },
            )
            .unwrap();
        assert_eq!(review.errors.is_empty(), valid);
        assert_eq!(
            std::fs::read_to_string(repository.root.join("instruction-store.toml")).unwrap(),
            original
        );
    }
}

#[cfg(unix)]
#[test]
fn instruction_draft_and_working_read_reject_fifo_without_waiting_for_a_writer() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    assert!(
        Command::new("mkfifo")
            .arg(repository.root.join("modules/pipe.md"))
            .status()
            .unwrap()
            .success()
    );
    assert!(
        fixture
            .service
            .open_draft(&repository, "modules/pipe.md")
            .is_err()
    );
    assert!(
        fixture
            .service
            .read_file(
                &repository,
                "modules/pipe.md",
                InstructionReadPolicy::WorkingTreeOnly
            )
            .is_err()
    );
}

#[test]
fn git_mutation_lock_is_shared_across_aliases_and_service_state_roots() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let _owner = acquire_mutation_lease(&fixture.state, &repository, "shared-owner").unwrap();
    let mut alias = repository.clone();
    alias.id = "different-project-configuration".into();
    assert!(acquire_mutation_lease(&fixture.state, &alias, "alias-contender").is_err());
    assert!(
        acquire_mutation_lease(
            &fixture._root.path().join("other-state"),
            &alias,
            "other-state-contender"
        )
        .is_err()
    );
}

#[test]
fn attached_drafts_block_branch_changes_from_another_service_state_root() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    workspace
        .begin(
            &fixture.service,
            &repository,
            "owner-session",
            &["modules/common.md".into()],
            "instruction: edit",
        )
        .unwrap();
    let other = InstructionRepositoryService::from_paths(
        &fixture.home,
        fixture._root.path().join("other-state"),
    );
    let mut alias = repository.clone();
    alias.id = "alias-project".into();
    assert_eq!(
        other
            .checkout_branch(&alias, "other-branch", "other", true, None)
            .unwrap_err()
            .kind,
        InstructionRepositoryErrorKind::MutationBusy
    );
    workspace.close();
    other
        .checkout_branch(&alias, "other-branch", "other", true, None)
        .unwrap();
}

#[test]
fn unmanaged_unborn_git_directory_is_not_overwritten_by_seed_initialization() {
    let fixture = Fixture::new();
    let repository = fixture.service.global_repository().unwrap();
    std::fs::create_dir_all(&repository.root).unwrap();
    git(&repository.root, &["init", "--initial-branch", "main"]);
    std::fs::write(
        repository.root.join("user-file.md"),
        "preserved user content",
    )
    .unwrap();
    assert!(fixture.service.initialize_global(&seed(), &[]).is_err());
    assert_eq!(
        std::fs::read_to_string(repository.root.join("user-file.md")).unwrap(),
        "preserved user content"
    );
    assert!(!repository.root.join("instruction-store.toml").exists());
}

#[test]
fn relative_submodule_urls_keep_git_resolution_and_idempotent_setup() {
    let fixture = Fixture::new();
    let source = fixture.initialize().repository;
    let remote = fixture._root.path().join("instructions.git");
    git(
        fixture._root.path(),
        &[
            "clone",
            "--bare",
            source.root.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    let parent = fixture._root.path().join("parent");
    init_plain_git(&parent);
    git(
        &parent,
        &[
            "remote",
            "add",
            "origin",
            fixture._root.path().join("parent.git").to_str().unwrap(),
        ],
    );
    let head = git(&parent, &["rev-parse", "HEAD"]);
    let first = fixture
        .service
        .configure_submodule(
            &parent,
            "relative-setup",
            "../instructions.git",
            "main",
            None,
        )
        .unwrap();
    let repeated = fixture
        .service
        .configure_submodule(
            &parent,
            "relative-setup",
            "../instructions.git",
            "main",
            None,
        )
        .unwrap();
    assert_eq!(first.root, repeated.root);
    assert_eq!(git(&parent, &["rev-parse", "HEAD"]), head);
    assert!(
        fixture
            .service
            .inspect(&first)
            .unwrap()
            .parent_gitlink
            .unwrap()
            .gitlink_changed
    );
}

#[test]
fn instruction_draft_never_adopts_an_enclosing_parent_git_repository() {
    let fixture = Fixture::new();
    init_plain_git(&fixture.home);
    let repository = fixture.service.global_repository().unwrap();
    std::fs::create_dir_all(repository.root.join("modules")).unwrap();
    std::fs::write(
        repository.root.join("instruction-store.toml"),
        "schema_version=1\nseed_version=27\n",
    )
    .unwrap();
    std::fs::write(
        repository.root.join("modules/example.md"),
        managed("example", "module", "nested source"),
    )
    .unwrap();
    let parent_head = git(&fixture.home, &["rev-parse", "HEAD"]);
    assert!(
        fixture
            .service
            .open_draft(&repository, "modules/example.md")
            .is_err()
    );
    assert_eq!(git(&fixture.home, &["rev-parse", "HEAD"]), parent_head);
}

#[cfg(unix)]
#[test]
fn instruction_git_branch_actions_do_not_run_repository_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let marker = fixture._root.path().join("hook-executed");
    let hook = repository.root.join(".git/hooks/post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture
        .service
        .checkout_branch(&repository, "branch-with-hook", "work", true, None)
        .unwrap();
    assert!(
        !marker.exists(),
        "repository-provided checkout hook executed"
    );
}

#[cfg(unix)]
#[test]
fn instruction_commits_are_raw_bytes_and_never_execute_clean_filters() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let marker = fixture._root.path().join("filter-executed");
    git(
        &repository.root,
        &[
            "config",
            "filter.fixture.clean",
            &format!("touch '{}'; printf 'FILTERED'", marker.display()),
        ],
    );
    git(
        &repository.root,
        &["config", "filter.fixture.required", "true"],
    );
    std::fs::write(
        repository.root.join(".gitattributes"),
        "*.md filter=fixture text eol=lf\n",
    )
    .unwrap();
    let content = managed("common", "module", "exact\r\nsynthetic bytes\r\n");
    let outcome = draft_commit(
        &fixture.service,
        &repository,
        "modules/common.md",
        &content,
        "raw-filter-free-save",
    );
    assert!(
        !marker.exists(),
        "repository-configured clean filter executed"
    );
    let committed = fixture
        .service
        .content_at_revision(&repository, &outcome.commit, "modules/common.md")
        .unwrap();
    assert_eq!(committed.content, content);
}

#[test]
fn history_compare_restore_clear_rename_and_multi_delete_create_new_commits() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    let repository = initialized.repository;
    let first = initialized.commit;
    let second = draft_commit(
        &fixture.service,
        &repository,
        "modules/common.md",
        &managed("common", "module", "second version sentinel"),
        "second-version",
    );
    let history = fixture
        .service
        .history(&repository, Some(Path::new("modules/common.md")))
        .unwrap();
    assert!(history.len() >= 2);
    let comparison = fixture
        .service
        .compare_revisions(
            &repository,
            &first,
            &second.commit,
            Some(Path::new("modules/common.md")),
        )
        .unwrap();
    assert!(comparison.patch.contains("second version sentinel"));

    let restored = fixture
        .service
        .restore_revision(
            &repository,
            &first,
            "modules/common.md",
            "restore-first",
            "instruction: restore first",
        )
        .unwrap();
    assert_eq!(restored.disposition, InstructionCommitDisposition::Created);
    assert_ne!(restored.commit, first);
    assert!(
        std::fs::read_to_string(repository.root.join("modules/common.md"))
            .unwrap()
            .contains("seed common body")
    );

    let cleared = fixture
        .service
        .clear_resource_body(
            &repository,
            "modules/common.md",
            "clear-common",
            "instruction: clear common",
        )
        .unwrap();
    assert_eq!(cleared.disposition, InstructionCommitDisposition::Created);
    let runtime = crate::instruction::InstructionRuntime::discover(
        crate::instruction::InstructionSources::new(&repository.root),
    );
    let rendered = runtime
        .render(
            &crate::instruction::InstructionSelector::global(InstructionKind::Module, "common")
                .unwrap(),
            &serde_json::json!({}),
        )
        .unwrap();
    assert!(rendered.text.is_empty());

    let rename_from = fingerprint(&repository, Path::new("modules/common.md")).unwrap();
    let rename_to = fingerprint(&repository, Path::new("modules/renamed.md")).unwrap();
    let rename_head = GitRepository::new(&repository.root)
        .head()
        .unwrap()
        .unwrap();
    let renamed = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "rename-common".to_string(),
                message: "instruction: rename common".to_string(),
                expected_head: rename_head,
                expected_files: vec![rename_from, rename_to],
                mutations: vec![InstructionFileMutation::Rename {
                    from: PathBuf::from("modules/common.md"),
                    to: PathBuf::from("modules/renamed.md"),
                }],
            },
        )
        .unwrap();
    assert_eq!(renamed.disposition, InstructionCommitDisposition::Created);
    assert!(!repository.root.join("modules/common.md").exists());
    assert!(repository.root.join("modules/renamed.md").exists());

    let delete_paths = ["agents/worker.md", "addenda/worker-project.md"];
    let delete_head = GitRepository::new(&repository.root)
        .head()
        .unwrap()
        .unwrap();
    let agent_only = fingerprint(&repository, Path::new(delete_paths[0])).unwrap();
    let blocked_delete = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "delete-agent-with-live-reference".to_string(),
                message: "instruction: invalid partial delete".to_string(),
                expected_head: delete_head.clone(),
                expected_files: vec![agent_only],
                mutations: vec![InstructionFileMutation::Delete {
                    relative_path: PathBuf::from(delete_paths[0]),
                }],
            },
        )
        .expect_err("deleting a referenced agent must require reference repair");
    assert_eq!(
        blocked_delete.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
    assert!(!blocked_delete.existing_state_unchanged);
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        Some(delete_head.clone())
    );
    let expected_files = delete_paths
        .iter()
        .map(|path| fingerprint(&repository, Path::new(path)).unwrap())
        .collect();
    let deleted = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "delete-agent-and-reference".to_string(),
                message: "instruction: delete agent and addendum".to_string(),
                expected_head: delete_head,
                expected_files,
                mutations: delete_paths
                    .iter()
                    .map(|path| InstructionFileMutation::Delete {
                        relative_path: PathBuf::from(path),
                    })
                    .collect(),
            },
        )
        .unwrap();
    assert_eq!(deleted.disposition, InstructionCommitDisposition::Created);
    assert!(
        delete_paths
            .iter()
            .all(|path| !repository.root.join(path).exists())
    );
}

#[test]
fn legacy_import_is_exact_idempotent_and_commit_precedes_inactivation() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    let repository = initialized.repository;
    let legacy_path = fixture.home.join("prompt-overlay.md");
    let exact = "  exact legacy text\nwith unicode π\n";
    std::fs::write(&legacy_path, exact).unwrap();
    let project = fixture._root.path().join("legacy-project");
    std::fs::create_dir_all(project.join(".jcode")).unwrap();
    std::fs::write(project.join(".jcode/swarm-prompt.md"), "project swarm").unwrap();
    std::fs::write(project.join("AGENTS.md"), "dedicated ecosystem input").unwrap();
    let discovered = fixture
        .service
        .discover_known_legacy_sources(Some(&project))
        .unwrap();
    assert_eq!(discovered.len(), 8);
    assert!(discovered.iter().any(|source| {
        source.scope == InstructionScope::Global
            && source.source_kind == LegacyInstructionSourceKind::PromptOverlay
            && source.content.as_deref() == Some(exact)
    }));
    assert!(discovered.iter().any(|source| {
        source.scope == InstructionScope::Project
            && source.source_kind == LegacyInstructionSourceKind::SwarmPrompt
            && source.content.as_deref() == Some("project swarm")
    }));
    assert!(
        discovered
            .iter()
            .all(|source| source.path.file_name() != Some(std::ffi::OsStr::new("AGENTS.md")))
    );
    let spec = InstructionLegacyImportSpec {
        import_id: "global-prompt-overlay".to_string(),
        source_kind: LegacyInstructionSourceKind::PromptOverlay,
        source_path: legacy_path.clone(),
        target: InstructionLegacyImportTarget {
            relative_path: PathBuf::from("system/legacy-overlay.md"),
            id: InstructionId::parse("legacy-overlay").unwrap(),
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
        },
    };
    let plan = fixture.service.plan_legacy_import(&spec).unwrap().unwrap();
    assert_eq!(plan.source_content, exact);
    assert!(!plan.source_was_empty);
    let imported = fixture
        .service
        .import_legacy(&repository, &spec, "import-overlay")
        .unwrap();
    let (receipt, commit) = match imported {
        InstructionLegacyImportOutcome::Imported {
            receipt,
            commit,
            working_changes_preserved,
        } => {
            assert!(working_changes_preserved.is_empty());
            (receipt, commit)
        }
        other => panic!("unexpected import outcome: {other:?}"),
    };
    assert_eq!(std::fs::read_to_string(&legacy_path).unwrap(), exact);
    assert_eq!(
        receipt.source_sha256,
        super::mutation::sha256(exact.as_bytes())
    );
    let committed_manifest = fixture
        .service
        .load_committed_manifest(&repository)
        .unwrap();
    assert_eq!(
        committed_manifest
            .legacy_imports
            .get("global-prompt-overlay"),
        Some(&receipt)
    );
    let runtime = crate::instruction::InstructionRuntime::discover(
        crate::instruction::InstructionSources::new(&repository.root),
    );
    let document = runtime
        .resolve(
            &crate::instruction::InstructionSelector::global(
                InstructionKind::System,
                "legacy-overlay",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(document.body, exact);

    // Simulate a process dying after commit publication but before working-file
    // materialization. Retrying the same operation proves commit identity and
    // repairs both files without creating another commit.
    std::fs::remove_file(repository.root.join("instruction-store.toml")).unwrap();
    std::fs::remove_file(repository.root.join("system/legacy-overlay.md")).unwrap();
    let recovered = fixture
        .service
        .import_legacy(&repository, &spec, "import-overlay")
        .unwrap();
    match recovered {
        InstructionLegacyImportOutcome::AlreadyImported {
            receipt: recovered,
            commit: recovered_commit,
            working_changes_preserved,
        } => {
            assert_eq!(recovered, receipt);
            assert_eq!(recovered_commit, commit);
            assert!(working_changes_preserved.is_empty());
        }
        other => panic!("unexpected recovery outcome: {other:?}"),
    }
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        Some(commit.clone())
    );
    assert!(repository.root.join("instruction-store.toml").exists());
    assert!(repository.root.join("system/legacy-overlay.md").exists());

    let later_working_edit = "later user working-tree edit";
    std::fs::write(
        repository.root.join("system/legacy-overlay.md"),
        later_working_edit,
    )
    .unwrap();
    let repeated = fixture
        .service
        .import_legacy(&repository, &spec, "import-overlay")
        .expect("completed import retry");
    match repeated {
        InstructionLegacyImportOutcome::AlreadyImported {
            working_changes_preserved,
            ..
        } => assert_eq!(
            working_changes_preserved,
            vec![PathBuf::from("system/legacy-overlay.md")]
        ),
        other => panic!("unexpected repeated import outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(repository.root.join("system/legacy-overlay.md")).unwrap(),
        later_working_edit
    );

    let empty_path = fixture.home.join("preferred-tools.md");
    std::fs::write(&empty_path, "  \n").unwrap();
    let empty_spec = InstructionLegacyImportSpec {
        import_id: "global-preferred-tools".to_string(),
        source_kind: LegacyInstructionSourceKind::PreferredTools,
        source_path: empty_path,
        target: InstructionLegacyImportTarget {
            relative_path: PathBuf::from("tools/legacy-preferred-tools.md"),
            id: InstructionId::parse("legacy-preferred-tools").unwrap(),
            kind: InstructionKind::ToolGuidance,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
        },
    };
    let empty_plan = fixture
        .service
        .plan_legacy_import(&empty_spec)
        .unwrap()
        .unwrap();
    assert!(!empty_plan.source_was_empty);
    assert!(empty_plan.source_was_blank);
    fixture
        .service
        .import_legacy(&repository, &empty_spec, "import-empty-tools")
        .unwrap();

    let invalid_path = fixture.home.join("invalid-legacy.md");
    std::fs::write(&invalid_path, "invalid dependency import").unwrap();
    let invalid_spec = InstructionLegacyImportSpec {
        import_id: "invalid-dependency-import".to_string(),
        source_kind: LegacyInstructionSourceKind::InventoryApproved,
        source_path: invalid_path,
        target: InstructionLegacyImportTarget {
            relative_path: PathBuf::from("system/invalid-import.md"),
            id: InstructionId::parse("invalid-import").unwrap(),
            kind: InstructionKind::System,
            scope: InstructionScope::Global,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata {
                includes: vec![
                    crate::instruction::InstructionSelector::unqualified(
                        InstructionKind::Module,
                        "missing-import-dependency",
                    )
                    .unwrap(),
                ],
                ..InstructionMetadata::default()
            },
        },
    };
    let before_invalid = GitRepository::new(&repository.root).head().unwrap();
    let invalid = fixture
        .service
        .import_legacy(&repository, &invalid_spec, "import-invalid-dependency")
        .expect_err("legacy import with unresolved dependency must not commit");
    assert_eq!(
        invalid.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
    assert_eq!(
        GitRepository::new(&repository.root).head().unwrap(),
        before_invalid
    );
    assert!(!repository.root.join("system/invalid-import.md").exists());
}

#[test]
fn project_submodule_external_and_non_git_modes_preserve_parent_authority() {
    let fixture = Fixture::new();
    let source = fixture.initialize().repository;
    let remote = fixture._root.path().join("instructions.git");
    git(
        fixture._root.path(),
        &[
            "clone",
            "--bare",
            source.root.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );

    let parent = fixture._root.path().join("parent");
    init_plain_git(&parent);
    let parent_head = git(&parent, &["rev-parse", "HEAD"]);
    let submodule = fixture
        .service
        .configure_submodule(
            &parent,
            "setup-submodule",
            remote.to_str().unwrap(),
            "main",
            None,
        )
        .unwrap();
    let repeated_submodule = fixture
        .service
        .configure_submodule(
            &parent,
            "setup-submodule-retry",
            remote.to_str().unwrap(),
            "main",
            None,
        )
        .expect("submodule setup retry is idempotent");
    assert_eq!(repeated_submodule.root, submodule.root);
    let submodule_mismatch = fixture
        .service
        .configure_submodule(
            &parent,
            "setup-submodule-mismatch",
            remote.to_str().unwrap(),
            "other",
            None,
        )
        .expect_err("existing submodule branch mismatch must be explicit");
    assert_eq!(
        submodule_mismatch.kind,
        InstructionRepositoryErrorKind::Configuration
    );
    assert_eq!(submodule.kind, InstructionRepositoryKind::ProjectSubmodule);
    assert_eq!(git(&parent, &["rev-parse", "HEAD"]), parent_head);
    let parent_status = git(&parent, &["status", "--short"]);
    assert!(parent_status.contains(".gitmodules"));
    assert!(parent_status.contains(".jcode/instructions"));
    let resolved = fixture
        .service
        .resolve_project_repository(&parent)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.root, submodule.root);

    draft_commit(
        &fixture.service,
        &submodule,
        "modules/common.md",
        &managed("common", "module", "submodule update"),
        "submodule-update",
    );
    assert_eq!(git(&parent, &["rev-parse", "HEAD"]), parent_head);
    let state = fixture.service.inspect(&submodule).unwrap();
    let parent_gitlink = state.parent_gitlink.expect("parent gitlink state");
    assert!(parent_gitlink.gitlink_changed);
    assert_ne!(
        parent_gitlink.recorded_commit,
        parent_gitlink.checked_out_commit
    );

    let external_parent = fixture._root.path().join("external-parent");
    init_plain_git(&external_parent);
    let external = fixture
        .service
        .configure_external_remote(
            &external_parent,
            "setup-external",
            remote.to_str().unwrap(),
            "main",
        )
        .unwrap();
    assert_eq!(external.kind, InstructionRepositoryKind::ProjectExternal);
    assert!(external.owner_only);
    assert_eq!(
        fixture.service.inspect(&external).unwrap().health,
        InstructionRepositoryHealth::Ready
    );
    let external_mismatch = fixture
        .service
        .configure_external_remote(
            &external_parent,
            "setup-external-mismatch",
            remote.to_str().unwrap(),
            "other",
        )
        .expect_err("existing external branch mismatch must be explicit");
    assert_eq!(
        external_mismatch.kind,
        InstructionRepositoryErrorKind::Configuration
    );
    let setup_lease =
        acquire_mutation_lease(&fixture.state, &external, "held-setup-lease").unwrap();
    let busy_setup = fixture
        .service
        .configure_external_remote(
            &external_parent,
            "blocked-external-setup",
            remote.to_str().unwrap(),
            "main",
        )
        .expect_err("repository setup must honor the cross-process mutation lease");
    assert_eq!(
        busy_setup.kind,
        InstructionRepositoryErrorKind::MutationBusy
    );
    drop(setup_lease);

    let local_parent = fixture._root.path().join("local-parent");
    init_plain_git(&local_parent);
    let local = fixture
        .service
        .configure_external_local(
            &local_parent,
            "setup-local",
            &source.root,
            Some("main".to_string()),
        )
        .unwrap();
    assert_eq!(local.root, std::fs::canonicalize(&source.root).unwrap());
    let local_mismatch = fixture
        .service
        .configure_external_local(
            &local_parent,
            "setup-local-mismatch",
            &source.root,
            Some("other".to_string()),
        )
        .expect_err("existing local branch mismatch must be explicit");
    assert_eq!(
        local_mismatch.kind,
        InstructionRepositoryErrorKind::Configuration
    );

    std::fs::remove_dir_all(&external.root).unwrap();
    let missing_external = fixture.service.inspect(&external).unwrap();
    assert!(matches!(
        missing_external.health,
        InstructionRepositoryHealth::Damaged(InstructionRepositoryDamage {
            kind: InstructionRepositoryDamageKind::MissingCheckout,
            ..
        })
    ));

    let non_git = fixture._root.path().join("plain-project");
    std::fs::create_dir_all(&non_git).unwrap();
    let standalone = fixture
        .service
        .configure_non_git_project(&non_git, "setup-standalone", None, &seed(), &[])
        .unwrap();
    let repeated_standalone = fixture
        .service
        .configure_non_git_project(&non_git, "setup-standalone-retry", None, &seed(), &[])
        .expect("standalone setup retry is idempotent");
    assert!(!repeated_standalone.created);
    assert_eq!(repeated_standalone.commit, standalone.commit);
    assert_eq!(
        standalone.repository.kind,
        InstructionRepositoryKind::NonGitProject
    );
    assert!(!non_git.join(".git").exists());
    assert!(non_git.join(".jcode/instructions/.git").exists());
    let resolved_standalone = fixture
        .service
        .resolve_project_repository(&non_git)
        .unwrap()
        .unwrap();
    assert_eq!(resolved_standalone.root, standalone.repository.root);
}

#[test]
fn explicit_branch_and_local_remote_operations_cover_fast_forward_and_conflict() {
    let fixture = Fixture::new();
    let primary = fixture.initialize().repository;
    let bare = fixture._root.path().join("sync.git");
    git(
        fixture._root.path(),
        &["init", "--bare", bare.to_str().unwrap()],
    );
    fixture
        .service
        .configure_remote(&primary, "remote-primary", "origin", bare.to_str().unwrap())
        .unwrap();
    fixture
        .service
        .push(&primary, "push-primary", "origin", "main", true)
        .unwrap();

    fixture
        .service
        .checkout_branch(&primary, "create-topic", "topic", true, Some("main"))
        .unwrap();
    assert_eq!(
        fixture.service.inspect(&primary).unwrap().branch.as_deref(),
        Some("topic")
    );
    fixture
        .service
        .checkout_branch(&primary, "back-main", "main", false, None)
        .unwrap();

    let consumer_parent = fixture._root.path().join("consumer-parent");
    init_plain_git(&consumer_parent);
    let consumer = fixture
        .service
        .configure_external_remote(
            &consumer_parent,
            "setup-consumer",
            bare.to_str().unwrap(),
            "main",
        )
        .unwrap();
    let remote_before = git(&bare, &["rev-parse", "refs/heads/main"]);
    let local_only = draft_commit(
        &fixture.service,
        &primary,
        "modules/common.md",
        &managed("common", "module", "primary fast-forward update"),
        "primary-ff-update",
    );
    assert_ne!(local_only.commit, remote_before);
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/main"]), remote_before);
    fixture
        .service
        .push(&primary, "push-ff", "origin", "main", false)
        .unwrap();
    fixture
        .service
        .fetch(&consumer, "fetch-consumer", "origin")
        .unwrap();
    fixture
        .service
        .pull(
            &consumer,
            "pull-consumer",
            "origin",
            "main",
            InstructionPullStrategy::FastForwardOnly,
        )
        .unwrap();
    assert!(
        std::fs::read_to_string(consumer.root.join("modules/common.md"))
            .unwrap()
            .contains("primary fast-forward update")
    );

    draft_commit(
        &fixture.service,
        &consumer,
        "modules/common.md",
        &managed("common", "module", "consumer divergent update"),
        "consumer-divergence",
    );
    draft_commit(
        &fixture.service,
        &primary,
        "modules/common.md",
        &managed("common", "module", "primary divergent update"),
        "primary-divergence",
    );
    fixture
        .service
        .push(&primary, "push-divergence", "origin", "main", false)
        .unwrap();
    let conflict = fixture
        .service
        .pull(
            &consumer,
            "pull-conflict",
            "origin",
            "main",
            InstructionPullStrategy::Merge,
        )
        .expect_err("divergent same-file pull should conflict");
    assert_eq!(conflict.kind, InstructionRepositoryErrorKind::GitCommand);
    assert!(!conflict.existing_state_unchanged);
    assert!(
        !fixture
            .service
            .inspect(&consumer)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn detached_head_lease_read_fallback_and_symlink_boundaries_fail_closed() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    let repository = initialized.repository;
    let draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    git(&repository.root, &["checkout", "--detach"]);
    let detached = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "detached-save".to_string(),
                message: "instruction: detached".to_string(),
                expected_head: draft.base_head,
                expected_files: vec![draft.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: managed("common", "module", "detached body").into_bytes(),
                }],
            },
        )
        .expect_err("detached Save must fail");
    assert_eq!(detached.kind, InstructionRepositoryErrorKind::DetachedHead);
    git(&repository.root, &["switch", "main"]);

    let _lease = acquire_mutation_lease(&fixture.state, &repository, "held-by-test").unwrap();
    let busy_draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let busy = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "blocked-by-lease".to_string(),
                message: "instruction: blocked".to_string(),
                expected_head: busy_draft.base_head,
                expected_files: vec![busy_draft.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: managed("common", "module", "blocked").into_bytes(),
                }],
            },
        )
        .expect_err("concurrent mutation must fail");
    assert_eq!(busy.kind, InstructionRepositoryErrorKind::MutationBusy);
    drop(_lease);

    std::fs::remove_file(repository.root.join("modules/common.md")).unwrap();
    let working_only = fixture.service.read_file(
        &repository,
        "modules/common.md",
        InstructionReadPolicy::WorkingTreeOnly,
    );
    assert!(working_only.is_err());
    let fallback = fixture
        .service
        .read_file(
            &repository,
            "modules/common.md",
            InstructionReadPolicy::AllowHeadFallback,
        )
        .unwrap();
    assert_eq!(fallback.source, InstructionFileSource::GitHead);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        fixture
            .service
            .restore_working_file_from_head(&repository, "modules/common.md", "restore-common")
            .unwrap();
        let outside = fixture._root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(repository.root.join("tools")).ok();
        symlink(&outside, repository.root.join("tools")).unwrap();
        let error = atomic_write(
            &repository,
            Path::new("tools/escape.md"),
            b"must not escape",
        )
        .expect_err("symlink escape must fail");
        assert_eq!(error.kind, InstructionRepositoryErrorKind::SymlinkEscape);
        assert!(!outside.join("escape.md").exists());
    }
}

#[test]
fn invalid_utf8_head_restore_leaves_the_working_file_unchanged() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    git(&repository.root, &["config", "user.name", "Fixture"]);
    git(
        &repository.root,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(repository.root.join("modules/common.md"), [0xff, 0xfe]).unwrap();
    git(&repository.root, &["add", "modules/common.md"]);
    git(
        &repository.root,
        &["commit", "-m", "fixture: invalid UTF-8"],
    );
    let sentinel = managed("common", "module", "working sentinel");
    std::fs::write(repository.root.join("modules/common.md"), &sentinel).unwrap();
    let error = fixture
        .service
        .restore_working_file_from_head(&repository, "modules/common.md", "restore-invalid-utf8")
        .expect_err("invalid UTF-8 HEAD content must fail before write");
    assert_eq!(error.kind, InstructionRepositoryErrorKind::InvalidUtf8);
    assert!(error.existing_state_unchanged);
    assert_eq!(
        std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
        sentinel
    );
}

#[test]
fn invalid_explicit_project_configuration_never_falls_back_to_global_only() {
    let fixture = Fixture::new();
    let project = fixture._root.path().join("configured-project");
    init_plain_git(&project);
    std::fs::create_dir_all(project.join(".jcode")).unwrap();
    std::fs::write(
        project.join(".jcode/instructions.toml"),
        "schema_version = 1\n[repository]\nmode = 'external-remote'\nurl = 3\nbranch = 'main'\n",
    )
    .unwrap();
    let error = fixture
        .service
        .resolve_project_repository(&project)
        .expect_err("invalid explicit configuration must fail visibly");
    assert_eq!(error.kind, InstructionRepositoryErrorKind::Configuration);
}

#[test]
fn external_local_setup_rejects_a_git_repository_without_instruction_manifest() {
    let fixture = Fixture::new();
    let project = fixture._root.path().join("manifest-project");
    let ordinary = fixture._root.path().join("ordinary-git");
    init_plain_git(&project);
    init_plain_git(&ordinary);
    let error = fixture
        .service
        .configure_external_local(
            &project,
            "attach-ordinary-git",
            &ordinary,
            Some("main".to_string()),
        )
        .expect_err("ordinary Git checkout must not become an instruction store");
    assert_eq!(
        error.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
    assert!(!project.join(".jcode/instructions.toml").exists());
}

#[cfg(unix)]
#[test]
fn project_configuration_write_never_escapes_through_dot_jcode_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let source = fixture.initialize().repository;
    let project = fixture._root.path().join("symlink-project");
    init_plain_git(&project);
    let outside = fixture._root.path().join("outside-config");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, project.join(".jcode")).unwrap();
    let error = fixture
        .service
        .configure_external_local(
            &project,
            "setup-symlink-project",
            &source.root,
            Some("main".to_string()),
        )
        .expect_err("configuration write must reject a symlinked .jcode directory");
    assert_eq!(error.kind, InstructionRepositoryErrorKind::SymlinkEscape);
    assert!(!outside.join("instructions.toml").exists());
}

#[cfg(unix)]
#[test]
fn dangling_project_configuration_symlink_fails_visibly() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let project = fixture._root.path().join("dangling-config-project");
    init_plain_git(&project);
    std::fs::create_dir_all(project.join(".jcode")).unwrap();
    symlink(
        fixture._root.path().join("missing-config-target"),
        project.join(".jcode/instructions.toml"),
    )
    .unwrap();
    let error = fixture
        .service
        .resolve_project_repository(&project)
        .expect_err("dangling explicit configuration must not look absent");
    assert_eq!(error.kind, InstructionRepositoryErrorKind::SymlinkEscape);
}

#[test]
fn repository_validation_reports_invalid_resources_without_hiding_valid_ones() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    std::fs::write(
        repository.root.join("modules/other.md"),
        "---\nid: other\nkind: module\nunknown-field: true\n---\n\nbroken",
    )
    .unwrap();
    let validation = fixture.service.validate_repository(&repository).unwrap();
    assert!(!validation.is_valid());
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path.ends_with("modules/other.md"))
    );
    assert!(validation.resources.iter().any(|resource| {
        resource.resource.id.as_str() == "common"
            && matches!(
                resource.state,
                crate::instruction::ResourceValidationState::Valid
            )
    }));
    assert_eq!(
        fixture.service.inspect(&repository).unwrap().health,
        InstructionRepositoryHealth::Ready
    );

    let draft = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let invalid_request = InstructionCommitRequest {
        operation_id: "write-invalid-common".to_string(),
        message: "instruction: invalid common".to_string(),
        expected_head: draft.base_head,
        expected_files: vec![draft.base],
        mutations: vec![InstructionFileMutation::Write {
            relative_path: PathBuf::from("modules/common.md"),
            content: b"missing frontmatter".to_vec(),
        }],
    };
    let invalid = fixture
        .service
        .commit(&repository, &invalid_request)
        .expect_err("new invalid resource must not be committed");
    assert_eq!(
        invalid.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );
    assert!(!invalid.existing_state_unchanged);
    let retry = fixture
        .service
        .commit(&repository, &invalid_request)
        .expect_err("retry must not reclassify rejected invalid content as pre-existing");
    assert_eq!(
        retry.kind,
        InstructionRepositoryErrorKind::RepositoryDamaged
    );

    let repair = fixture
        .service
        .open_draft(&repository, "modules/common.md")
        .unwrap();
    let repaired = fixture
        .service
        .commit(
            &repository,
            &InstructionCommitRequest {
                operation_id: "repair-invalid-common".to_string(),
                message: "instruction: repair common".to_string(),
                expected_head: repair.base_head,
                expected_files: vec![repair.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: PathBuf::from("modules/common.md"),
                    content: managed("common", "module", "repaired common").into_bytes(),
                }],
            },
        )
        .expect("repairing an existing invalid resource is allowed");
    assert_eq!(repaired.disposition, InstructionCommitDisposition::Created);

    std::fs::write(
        repository.root.join("modules/renamed.md"),
        "---\nid: renamed\nkind: module\nincludes:\n  - missing-module\n---\n\ngraph failure",
    )
    .unwrap();
    let graph_validation = fixture.service.validate_repository(&repository).unwrap();
    assert!(!graph_validation.is_valid());
    assert!(graph_validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path.ends_with("modules/renamed.md")
            && diagnostic.detail.contains("missing-module")
    }));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = fixture._root.path().join("validation-outside");
        std::fs::write(&outside, "outside").unwrap();
        symlink(&outside, repository.root.join("modules/escape.md")).unwrap();
        let path_validation = fixture.service.validate_repository(&repository).unwrap();
        assert!(!path_validation.is_valid());
        assert!(path_validation.diagnostics.iter().any(|diagnostic| {
            diagnostic.path.ends_with("modules/escape.md") && diagnostic.detail.contains("symlink")
        }));
    }
}

#[test]
fn server_service_construction_does_not_initialize_the_live_store() {
    let fixture = Fixture::new();
    let service = InstructionRepositoryService::from_paths(&fixture.home, &fixture.state);
    let repository = service.global_repository().unwrap();
    assert!(!repository.root.exists());
    assert_eq!(
        service.inspect(&repository).unwrap().health,
        InstructionRepositoryHealth::Uninitialized
    );
}

#[test]
fn mutation_lease_is_exclusive_across_processes_and_recovers_on_exit() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let ready = fixture._root.path().join("lease-child-ready");
    let release = fixture._root.path().join("lease-child-release");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "instruction::repository::tests::mutation_lease_child_process_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("JCODE_INSTRUCTION_LEASE_CHILD_STATE", &fixture.state)
        .env("JCODE_INSTRUCTION_LEASE_CHILD_ROOT", &repository.root)
        .env("JCODE_INSTRUCTION_LEASE_CHILD_READY", &ready)
        .env("JCODE_INSTRUCTION_LEASE_CHILD_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lease helper process");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            panic!(
                "lease helper exited early with {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ready.exists(),
        "lease helper did not acquire its lock in time"
    );
    let busy = acquire_mutation_lease(&fixture.state, &repository, "parent-operation")
        .expect_err("second process must observe the live mutation lease");
    assert_eq!(busy.kind, InstructionRepositoryErrorKind::MutationBusy);

    std::fs::write(&release, b"release").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "lease helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    acquire_mutation_lease(&fixture.state, &repository, "after-child-exit")
        .expect("kernel releases the mutation lease when the child exits");
}

#[test]
fn mutation_lease_child_process_helper() {
    let Some(state) = std::env::var_os("JCODE_INSTRUCTION_LEASE_CHILD_STATE") else {
        return;
    };
    let root = PathBuf::from(
        std::env::var_os("JCODE_INSTRUCTION_LEASE_CHILD_ROOT").expect("child repository root"),
    );
    let ready = PathBuf::from(
        std::env::var_os("JCODE_INSTRUCTION_LEASE_CHILD_READY").expect("child ready path"),
    );
    let release = PathBuf::from(
        std::env::var_os("JCODE_INSTRUCTION_LEASE_CHILD_RELEASE").expect("child release path"),
    );
    let repository = InstructionRepositoryRef {
        id: "global".to_string(),
        kind: InstructionRepositoryKind::Global,
        root,
        project_root: None,
        project_config_path: None,
        configured_branch: Some("main".to_string()),
        configured_remote: None,
        owner_only: true,
    };
    let _lease = acquire_mutation_lease(Path::new(&state), &repository, "child-process-operation")
        .expect("child acquires mutation lease");
    std::fs::write(&ready, b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(release.exists(), "parent did not release helper in time");
}

mod operation_tests;

mod recovery_tests;

#[test]
fn scoped_save_preserves_pending_merge_even_after_conflicts_are_staged() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    git(&repository.root, &["switch", "-c", "other"]);
    draft_commit(
        &fixture.service,
        &repository,
        "modules/common.md",
        &managed("common", "module", "OTHER"),
        "other-edit",
    );
    git(&repository.root, &["switch", "main"]);
    draft_commit(
        &fixture.service,
        &repository,
        "modules/common.md",
        &managed("common", "module", "MAIN"),
        "main-edit",
    );
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "merge",
            "--no-edit",
            "other",
        ])
        .current_dir(&repository.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(repository.root.join(".git/MERGE_HEAD").exists());
    for staged in [false, true] {
        if staged {
            std::fs::write(
                repository.root.join("modules/common.md"),
                managed("common", "module", "USER RESOLUTION"),
            )
            .unwrap();
            git(&repository.root, &["add", "modules/common.md"]);
        }
        let source = std::fs::read(repository.root.join("modules/common.md")).unwrap();
        let index = std::fs::read(repository.root.join(".git/index")).unwrap();
        let draft = fixture
            .service
            .open_draft(&repository, "modules/common.md")
            .unwrap();
        let error = fixture
            .service
            .commit(
                &repository,
                &InstructionCommitRequest {
                    operation_id: format!("save-during-merge-{staged}"),
                    message: "instruction: must not bypass merge".into(),
                    expected_head: draft.base_head.clone(),
                    expected_files: vec![draft.base],
                    mutations: vec![InstructionFileMutation::Write {
                        relative_path: "modules/common.md".into(),
                        content: managed("common", "module", "MANAGER PROPOSAL").into_bytes(),
                    }],
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, InstructionRepositoryErrorKind::Conflict);
        assert!(error.existing_state_unchanged);
        assert_eq!(
            std::fs::read(repository.root.join("modules/common.md")).unwrap(),
            source
        );
        assert_eq!(
            std::fs::read(repository.root.join(".git/index")).unwrap(),
            index
        );
        assert_eq!(
            git(&repository.root, &["rev-parse", "HEAD"]),
            draft.base_head
        );
    }
}
