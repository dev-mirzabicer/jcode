use super::*;
use jcode_instruction_types::{
    InstructionEditScope as Scope, InstructionRepositoryAction as Action,
};

fn perform(
    fixture: &Fixture,
    project: Option<&Path>,
    scope: Scope,
    action: Action,
) -> jcode_instruction_types::InstructionRepositoryReceipt {
    let plan = fixture
        .service
        .plan_repository_operation("operation-session", project, scope, action)
        .unwrap();
    let receipt = fixture
        .service
        .apply_repository_operation("operation-session", project, &plan.id)
        .unwrap();
    assert!(receipt.completed, "{}", receipt.detail);
    receipt
}

#[test]
fn reviewed_operations_preserve_source_until_apply_and_recover_lost_replies() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::ConfigureRemote {
                name: "origin".into(),
                url: "/synthetic/remote.git".into(),
            },
        )
        .unwrap();
    assert!(git(&repository.root, &["remote"]).is_empty());
    let applied = fixture
        .service
        .apply_repository_operation("operation-session", None, &plan.id)
        .unwrap();
    assert!(applied.completed);
    let index = std::fs::read(repository.root.join(".git/index")).unwrap();
    assert_eq!(
        fixture
            .service
            .apply_repository_operation("operation-session", None, &plan.id)
            .unwrap(),
        applied
    );
    assert_eq!(
        fixture
            .service
            .repository_operation_receipt("operation-session", None, &plan.id)
            .unwrap(),
        applied
    );
    assert_eq!(
        std::fs::read(repository.root.join(".git/index")).unwrap(),
        index
    );
    assert!(
        fixture
            .service
            .repository_operation_receipt("other-session", None, &plan.id)
            .is_err()
    );
}

#[test]
fn stale_operation_and_started_operation_never_overwrite_or_blindly_replay() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::Checkout {
                branch: "new".into(),
                create: true,
                start: None,
            },
        )
        .unwrap();
    std::fs::write(
        repository.root.join("modules/common.md"),
        managed("common", "module", "newer work"),
    )
    .unwrap();
    let result = fixture
        .service
        .apply_repository_operation("operation-session", None, &plan.id)
        .unwrap();
    assert!(!result.completed);
    assert!(result.source_unchanged);
    assert_eq!(git(&repository.root, &["branch", "--show-current"]), "main");
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::ConfigureRemote {
                name: "origin".into(),
                url: "/synthetic/remote".into(),
            },
        )
        .unwrap();
    let path = fixture
        .state
        .join("instruction-repositories/operations")
        .join(super::super::mutation::sha256(b"operation-session"))
        .join(format!("{}.json", plan.id));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["started"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let result = fixture
        .service
        .apply_repository_operation("operation-session", None, &plan.id)
        .unwrap();
    assert!(!result.completed && result.outcome_uncertain);
    assert!(git(&repository.root, &["remote"]).is_empty());
}

#[test]
fn repository_controls_push_fetch_pull_and_select_an_unfetched_branch() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let remote = fixture._root.path().join("remote.git");
    git(
        fixture._root.path(),
        &[
            "init",
            "--bare",
            "--initial-branch",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    perform(
        &fixture,
        None,
        Scope::Global,
        Action::ConfigureRemote {
            name: "origin".into(),
            url: remote.to_str().unwrap().into(),
        },
    );
    let push = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::Push {
                remote: "origin".into(),
                branch: "main".into(),
            },
        )
        .unwrap();
    assert_eq!(push.outgoing_commits.len(), 1);
    assert!(
        fixture
            .service
            .apply_repository_operation("operation-session", None, &push.id)
            .unwrap()
            .completed
    );
    let writer = fixture._root.path().join("writer");
    git(
        fixture._root.path(),
        &["clone", remote.to_str().unwrap(), writer.to_str().unwrap()],
    );
    git(&writer, &["config", "user.name", "Fixture"]);
    git(
        &writer,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(
        writer.join("modules/common.md"),
        managed("common", "module", "remote change"),
    )
    .unwrap();
    git(&writer, &["add", "modules/common.md"]);
    git(&writer, &["commit", "-m", "fixture: remote change"]);
    git(&writer, &["push", "origin", "main"]);
    let before = git(&repository.root, &["rev-parse", "HEAD"]);
    perform(
        &fixture,
        None,
        Scope::Global,
        Action::Fetch {
            remote: "origin".into(),
        },
    );
    assert_eq!(git(&repository.root, &["rev-parse", "HEAD"]), before);
    perform(
        &fixture,
        None,
        Scope::Global,
        Action::Pull {
            remote: "origin".into(),
            branch: "main".into(),
        },
    );
    assert_ne!(git(&repository.root, &["rev-parse", "HEAD"]), before);
    git(&writer, &["switch", "-c", "feature"]);
    std::fs::write(
        writer.join("modules/other.md"),
        managed("other", "module", "feature"),
    )
    .unwrap();
    git(&writer, &["add", "modules/other.md"]);
    git(&writer, &["commit", "-m", "fixture: feature"]);
    git(&writer, &["push", "origin", "feature"]);
    git(
        &repository.root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/main:refs/remotes/origin/main",
        ],
    );
    perform(
        &fixture,
        None,
        Scope::Global,
        Action::FetchCheckout {
            remote: "origin".into(),
            branch: "feature".into(),
            local_branch: "feature-work".into(),
        },
    );
    assert_eq!(
        git(&repository.root, &["branch", "--show-current"]),
        "feature-work"
    );
}

#[test]
fn reviewed_project_setup_rejects_changed_configuration_and_recovers_missing_external_checkout() {
    let fixture = Fixture::new();
    let source = fixture.initialize().repository;
    let project = fixture._root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            Some(&project),
            Scope::Project,
            Action::Standalone {
                path: ".jcode/instructions".into(),
            },
        )
        .unwrap();
    std::fs::create_dir_all(project.join(".jcode")).unwrap();
    std::fs::write(
        project.join(".jcode/instructions.toml"),
        "changed externally",
    )
    .unwrap();
    let result = fixture
        .service
        .apply_repository_operation("operation-session", Some(&project), &plan.id)
        .unwrap();
    assert!(!result.completed);
    assert!(!project.join(".jcode/instructions").exists());
    let remote = fixture._root.path().join("remote.git");
    git(
        fixture._root.path(),
        &[
            "clone",
            "--bare",
            source.root.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    perform(
        &fixture,
        Some(&project),
        Scope::Project,
        Action::CloneExternal {
            url: remote.to_str().unwrap().into(),
            branch: "main".into(),
        },
    );
    let repository = fixture
        .service
        .resolve_project_repository(&project)
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(&repository.root).unwrap();
    perform(
        &fixture,
        Some(&project),
        Scope::Project,
        Action::RepairCheckout,
    );
    assert!(repository.root.join("instruction-store.toml").is_file());
}

#[test]
fn reviewed_submodule_repair_restores_checkout_without_committing_parent() {
    let fixture = Fixture::new();
    let source = fixture.initialize().repository;
    let remote = fixture._root.path().join("remote.git");
    git(
        fixture._root.path(),
        &[
            "clone",
            "--bare",
            source.root.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    let project = fixture._root.path().join("project");
    init_plain_git(&project);
    let parent_head = git(&project, &["rev-parse", "HEAD"]);
    perform(
        &fixture,
        Some(&project),
        Scope::Project,
        Action::Submodule {
            path: ".jcode/instructions".into(),
            url: remote.to_str().unwrap().into(),
            branch: "main".into(),
        },
    );
    let repository = fixture
        .service
        .resolve_project_repository(&project)
        .unwrap()
        .unwrap();
    git(
        &project,
        &["submodule", "absorbgitdirs", ".jcode/instructions"],
    );
    std::fs::remove_dir_all(&repository.root).unwrap();
    perform(
        &fixture,
        Some(&project),
        Scope::Project,
        Action::RepairCheckout,
    );
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), parent_head);
    assert!(repository.root.join("modules/common.md").is_file());
}

#[test]
fn detached_branch_creation_preserves_unrelated_working_files() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    git(&repository.root, &["checkout", "--detach"]);
    std::fs::write(repository.root.join("unrelated.txt"), "preserved").unwrap();
    perform(
        &fixture,
        None,
        Scope::Global,
        Action::Checkout {
            branch: "my-edits".into(),
            create: true,
            start: None,
        },
    );
    assert_eq!(
        std::fs::read_to_string(repository.root.join("unrelated.txt")).unwrap(),
        "preserved"
    );
    assert_eq!(
        git(&repository.root, &["branch", "--show-current"]),
        "my-edits"
    );
}

#[test]
fn credential_bearing_urls_never_enter_operation_records() {
    let fixture = Fixture::new();
    fixture.initialize();
    let result = fixture.service.plan_repository_operation(
        "operation-session",
        None,
        Scope::Global,
        Action::ConfigureRemote {
            name: "origin".into(),
            url: "https://secret-token@example.invalid/repository".into(),
        },
    );
    assert!(result.is_err());
    assert!(!result.unwrap_err().to_string().contains("secret-token"));
    assert!(
        !fixture
            .state
            .join("instruction-repositories/operations")
            .exists()
    );
}

#[test]
fn branch_choices_are_literal_and_running_receipts_are_not_reported_as_interrupted() {
    assert!(super::super::git::validate_branch("@{-1}").is_err());
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::ConfigureRemote {
                name: "origin".into(),
                url: "/synthetic/remote".into(),
            },
        )
        .unwrap();
    let path = fixture
        .state
        .join("instruction-repositories/operations")
        .join(super::super::mutation::sha256(b"operation-session"))
        .join(format!("{}.json", plan.id));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["started"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let owner = super::super::lease::acquire_operation_lease(&fixture.state, &repository, &plan.id)
        .unwrap();
    let running = fixture
        .service
        .repository_operation_receipt("operation-session", None, &plan.id)
        .unwrap();
    assert!(running.running);
    assert!(!running.outcome_uncertain);
    drop(owner);
    let interrupted = fixture
        .service
        .repository_operation_receipt("operation-session", None, &plan.id)
        .unwrap();
    assert!(!interrupted.running);
    assert!(interrupted.outcome_uncertain);
}

#[test]
fn reviewed_seed_recreation_preserves_old_history_and_never_repeats_after_response_loss() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let original_head = git(&repository.root, &["rev-parse", "HEAD"]);
    let original = std::fs::read(repository.root.join("modules/common.md")).unwrap();
    let plan = fixture
        .service
        .plan_repository_operation(
            "operation-session",
            None,
            Scope::Global,
            Action::RecreateGlobal,
        )
        .unwrap();
    let receipt = fixture
        .service
        .apply_repository_operation("operation-session", None, &plan.id)
        .unwrap();
    assert!(receipt.completed, "{}", receipt.detail);
    let backup = fixture
        .home
        .join(format!("instructions.damaged-{}", plan.id));
    assert_eq!(git(&backup, &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        std::fs::read(backup.join("modules/common.md")).unwrap(),
        original
    );
    let current = git(&repository.root, &["rev-parse", "HEAD"]);
    assert_eq!(
        fixture
            .service
            .apply_repository_operation("operation-session", None, &plan.id)
            .unwrap(),
        receipt
    );
    assert_eq!(git(&repository.root, &["rev-parse", "HEAD"]), current);
}
