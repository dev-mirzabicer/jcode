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
