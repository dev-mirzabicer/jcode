use super::*;
use jcode_base::startup_context::{
    StartupContext, StartupFailurePolicy, StartupPreparationOutcome, StartupSelectionInput,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Fixture {
    temp: tempfile::TempDir,
    service: WorkspaceService,
    engine: StartupContext,
    coordinator: crate::server::startup_context::StartupContextCoordinator,
    source: PathBuf,
    target: PathBuf,
    target_id: LocationId,
    external: PathBuf,
}

fn git(root: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn change(service: &WorkspaceService, change: OrganizationChange) -> Receipt {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let service = WorkspaceService::new(&state);
        service.initialize(RequestId::new()).unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        for (root, text) in [(&source, "source guide"), (&target, "target guide")] {
            std::fs::create_dir(root).unwrap();
            git(root, &["init", "-q", "-b", "main"]);
            std::fs::write(root.join("guide.md"), text).unwrap();
            git(root, &["add", "guide.md"]);
            git(
                root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "guide",
                ],
            );
        }
        let EntityId::Project(project) = change(
            &service,
            OrganizationChange::CreateProject { name: "P".into() },
        )
        .targets[0] else {
            panic!()
        };
        let EntityId::Repository(repository) = change(
            &service,
            OrganizationChange::CreateRepository {
                name: "R".into(),
                remotes: vec![],
            },
        )
        .targets[0] else {
            panic!()
        };
        change(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        );
        let EntityId::Location(target_id) = change(
            &service,
            OrganizationChange::RegisterLocation {
                name: "target".into(),
                path: target.clone(),
                registration: Registration::Checkout {
                    home: Home::Project(project),
                    repository,
                },
            },
        )
        .targets[0] else {
            panic!()
        };
        let external = temp.path().join("external.txt");
        std::fs::write(&external, "approved external").unwrap();
        let engine = StartupContext::from_durable_state_dir(state.clone());
        let source_project = engine.resolve_project(&source).unwrap();
        let preview = engine.preview_selection(
            &source_project,
            vec![
                StartupSelectionInput::new("guide.md"),
                StartupSelectionInput::new(&external)
                    .with_external_approval(external.canonicalize().unwrap()),
            ],
        );
        assert!(preview.is_valid());
        engine
            .save_project_plan(&source_project, 0, &preview)
            .unwrap();
        let coordinator = crate::server::startup_context::StartupContextCoordinator::for_test(
            state,
            "workspace-copy",
            Duration::from_secs(90),
        );
        Self {
            temp,
            service,
            engine,
            coordinator,
            source,
            target,
            target_id,
            external,
        }
    }

    fn approved(&self) -> Vec<StartupCopyApproval> {
        let source_project = self.engine.resolve_project(&self.source).unwrap();
        let plan = self.engine.load_project_plan(&source_project).unwrap();
        vec![StartupCopyApproval {
            source_spec_id: plan.plan().entries()[1].id().to_string(),
            approved_resolved_target: self.external.canonicalize().unwrap(),
        }]
    }

    fn review(&self, approvals: Vec<StartupCopyApproval>) -> Result<StartupCopyReview> {
        self.service.review_startup_copy(
            self.service.status().unwrap().revision,
            self.source.clone(),
            self.target_id,
            1,
            0,
            approvals,
        )
    }
}

#[test]
fn copied_selection_uses_target_files_and_fresh_external_approval_with_one_receipt() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.review(vec![]).unwrap_err().code,
        IssueCode::PermissionRequired
    );
    let mut approval = fixture.approved();
    approval[0].approved_resolved_target = fixture.source.canonicalize().unwrap();
    assert_eq!(
        fixture.review(approval).unwrap_err().code,
        IssueCode::PermissionRequired
    );
    let review = fixture.review(fixture.approved()).unwrap();
    assert_eq!(review.entries.len(), 2);
    assert!(!review.entries[0].external);
    assert!(review.entries[1].external);
    assert_eq!(review.target_plan_revision, 0);
    let request = RequestId::new();
    let first = commit(
        &fixture.service,
        request,
        review.id,
        "fixture-client",
        &fixture.coordinator,
    )
    .unwrap();
    assert_eq!(first.state, StartupCopyState::Complete);
    assert_eq!(
        first,
        commit(
            &fixture.service,
            request,
            review.id,
            "fixture-client",
            &fixture.coordinator
        )
        .unwrap()
    );
    let target_project = fixture.engine.resolve_project(&fixture.target).unwrap();
    let plan = fixture.engine.load_project_plan(&target_project).unwrap();
    assert_eq!(plan.plan().revision(), 1);
    assert_eq!(plan.plan().entries().len(), 2);
    let StartupPreparationOutcome::Ready(prepared) = fixture
        .engine
        .prepare_project_plan(&target_project, plan.plan(), StartupFailurePolicy::Block)
        .unwrap()
    else {
        panic!("target capture blocked")
    };
    let texts = prepared
        .captured_files()
        .map(|file| file.text().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["target guide", "approved external"]);
    let source_project = fixture.engine.resolve_project(&fixture.source).unwrap();
    assert_eq!(
        fixture
            .engine
            .load_project_plan(&source_project)
            .unwrap()
            .plan()
            .revision(),
        1
    );
    assert_eq!(
        fixture.service.inspect_startup_copy(request).unwrap(),
        first
    );
}

#[cfg(unix)]
#[test]
fn changed_external_symlink_blocks_copy_before_any_plan_mutation() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let alias = fixture.temp.path().join("alias.txt");
    symlink(&fixture.external, &alias).unwrap();
    let source_project = fixture.engine.resolve_project(&fixture.source).unwrap();
    let preview = fixture.engine.preview_selection(
        &source_project,
        vec![
            StartupSelectionInput::new(&alias)
                .with_external_approval(fixture.external.canonicalize().unwrap()),
        ],
    );
    assert!(preview.is_valid());
    fixture
        .engine
        .save_project_plan(&source_project, 1, &preview)
        .unwrap();
    let source_id = fixture
        .engine
        .load_project_plan(&source_project)
        .unwrap()
        .plan()
        .entries()[0]
        .id()
        .to_string();
    let review = fixture
        .service
        .review_startup_copy(
            fixture.service.status().unwrap().revision,
            fixture.source.clone(),
            fixture.target_id,
            2,
            0,
            vec![StartupCopyApproval {
                source_spec_id: source_id,
                approved_resolved_target: fixture.external.canonicalize().unwrap(),
            }],
        )
        .unwrap();
    let new_target = fixture.temp.path().join("different.txt");
    std::fs::write(&new_target, "unapproved different content").unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&new_target, &alias).unwrap();
    let request = RequestId::new();
    assert!(
        commit(
            &fixture.service,
            request,
            review.id,
            "fixture-client",
            &fixture.coordinator
        )
        .is_err()
    );
    let target = fixture.engine.resolve_project(&fixture.target).unwrap();
    assert_eq!(
        fixture
            .engine
            .load_project_plan(&target)
            .unwrap()
            .plan()
            .revision(),
        0
    );
    assert_eq!(
        fixture
            .engine
            .load_project_plan(&source_project)
            .unwrap()
            .plan()
            .revision(),
        2
    );
}

#[test]
fn plan_commit_without_catalog_receipt_reconciles_one_copy_after_restart() {
    let fixture = Fixture::new();
    let review = fixture.review(fixture.approved()).unwrap();
    let request = RequestId::new();
    let intent = fixture
        .service
        .reserve_startup_copy(request, review.id)
        .unwrap();
    assert_eq!(intent.record.state, StartupCopyState::Pending);
    fixture
        .coordinator
        .commit_workspace_copy(
            &intent.target_path,
            &intent.transition,
            request,
            "fixture-client",
            || Ok(()),
        )
        .unwrap();
    assert_eq!(
        fixture.service.inspect_startup_copy(request).unwrap().state,
        StartupCopyState::Pending
    );
    let target = fixture.engine.resolve_project(&fixture.target).unwrap();
    assert_eq!(
        fixture
            .engine
            .load_project_plan(&target)
            .unwrap()
            .plan()
            .revision(),
        1
    );
    // Reconstruct the coordinator over the same private state. Retry finds the
    // exact already-applied transition instead of authoring a second plan.
    let coordinator = crate::server::startup_context::StartupContextCoordinator::for_test(
        fixture.temp.path().join("state"),
        "copy-restarted",
        Duration::from_secs(90),
    );
    let ready = commit(
        &fixture.service,
        request,
        review.id,
        "fixture-client",
        &coordinator,
    )
    .unwrap();
    assert_eq!(ready.state, StartupCopyState::Complete);
    assert_eq!(
        fixture
            .engine
            .load_project_plan(&target)
            .unwrap()
            .plan()
            .revision(),
        1
    );
    assert_eq!(
        fixture.service.inspect_startup_copy(request).unwrap(),
        ready
    );
}

#[test]
fn completed_copy_replay_is_historical_after_source_and_target_move() {
    let fixture = Fixture::new();
    let review = fixture.review(fixture.approved()).unwrap();
    let request = RequestId::new();
    let original = commit(
        &fixture.service,
        request,
        review.id,
        "fixture-client",
        &fixture.coordinator,
    )
    .unwrap();
    assert_eq!(original.state, StartupCopyState::Complete);
    std::fs::rename(
        &fixture.source,
        fixture.temp.path().join("source-relocated"),
    )
    .unwrap();
    std::fs::rename(
        &fixture.target,
        fixture.temp.path().join("target-relocated"),
    )
    .unwrap();
    let replay = commit(
        &fixture.service,
        request,
        review.id,
        "fixture-client",
        &fixture.coordinator,
    )
    .unwrap();
    assert_eq!(replay, original);
    assert_eq!(
        fixture.service.inspect_startup_copy(request).unwrap(),
        original
    );
}
