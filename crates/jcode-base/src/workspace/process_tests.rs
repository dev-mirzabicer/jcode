use super::*;
use std::process::{Child, Command};

fn helper(root: &Path, mode: &str, request: RequestId, review: ReviewId, stage: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "workspace::process_tests::process_helper",
            "--nocapture",
        ])
        .env("JCODE_CATALOG_FIXTURE_ROOT", root)
        .env("JCODE_CATALOG_FIXTURE_MODE", mode)
        .env("JCODE_CATALOG_FIXTURE_REQUEST", request.to_string())
        .env("JCODE_CATALOG_FIXTURE_REVIEW", review.to_string())
        .env("JCODE_CATALOG_FIXTURE_STAGE", stage)
        .spawn()
        .unwrap()
}
#[test]
fn process_helper() {
    let Ok(root) = std::env::var("JCODE_CATALOG_FIXTURE_ROOT") else {
        return;
    };
    let mode = std::env::var("JCODE_CATALOG_FIXTURE_MODE").unwrap();
    let request: RequestId = std::env::var("JCODE_CATALOG_FIXTURE_REQUEST")
        .unwrap()
        .parse()
        .unwrap();
    let review: ReviewId = std::env::var("JCODE_CATALOG_FIXTURE_REVIEW")
        .unwrap()
        .parse()
        .unwrap();
    let stage = std::env::var("JCODE_CATALOG_FIXTURE_STAGE").unwrap();
    let mut service = WorkspaceService::new(Path::new(&root));
    if mode == "race" {
        let outcome = service.apply_organization_change(request, review);
        std::fs::write(
            Path::new(&root).join(format!("{request}.result")),
            encode(&outcome).unwrap(),
        )
        .unwrap();
    } else if mode == "lease" {
        let _lease = service.lease(true).unwrap();
        std::fs::write(Path::new(&root).join("lease-ready"), b"ready").unwrap();
        while !Path::new(&root).join("lease-release").exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    } else {
        service.fault = Some(std::sync::Arc::new(move |point| {
            if point == stage {
                std::process::exit(42);
            }
            Ok(())
        }));
        if mode == "crash-import" {
            service.apply_import(request, review).unwrap();
        } else {
            service.apply_restore(request, review).unwrap();
        }
        panic!("Expected process-loss checkpoint was not reached");
    }
}

#[test]
fn simultaneous_process_writers_have_one_revision_winner_and_exact_retry() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let review = service
        .review_organization_change(0, OrganizationChange::CreateProject { name: "one".into() })
        .unwrap();
    let r1 = RequestId::new();
    let r2 = RequestId::new();
    let mut a = helper(dir.path(), "race", r1, review.id, "");
    let mut b = helper(dir.path(), "race", r2, review.id, "");
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    let outcomes = [r1, r2].map(|r| {
        decode::<Result<Receipt>>(
            &std::fs::read_to_string(dir.path().join(format!("{r}.result"))).unwrap(),
        )
        .unwrap()
    });
    assert_eq!(outcomes.iter().filter(|o| o.is_ok()).count(), 1);
    assert_eq!(service.status().unwrap().revision, 1);
    assert_eq!(
        outcomes.iter().find_map(|o| o.as_ref().err()).unwrap().code,
        IssueCode::Conflict
    );
    let winner = outcomes.into_iter().find_map(|o| o.ok()).unwrap();
    let mut retry = helper(dir.path(), "race", winner.request, review.id, "");
    assert!(retry.wait().unwrap().success());
    assert_eq!(service.status().unwrap().revision, 1);
    assert_eq!(service.inspect_receipt(winner.request).unwrap(), winner);
}

#[test]
fn kernel_lease_excludes_another_process_and_releases_on_exit() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let mut child = helper(dir.path(), "lease", RequestId::new(), ReviewId::new(), "");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !dir.path().join("lease-ready").exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Lease helper did not become ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(service.status().unwrap_err().code, IssueCode::Busy);
    std::fs::write(dir.path().join("lease-release"), b"release").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(service.status().is_ok());
}

#[test]
fn process_loss_during_import_rolls_back_and_after_restore_rename_recovers() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = WorkspaceService::new(source_dir.path());
    source.initialize(RequestId::new()).unwrap();
    let review = source
        .review_organization_change(
            0,
            OrganizationChange::CreateProject {
                name: "source".into(),
            },
        )
        .unwrap();
    let created = source
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap();
    let EntityId::Project(p) = created.targets[0] else {
        panic!()
    };
    let export = source
        .export_project(RequestId::new(), p, "fixture".into())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let import = service
        .review_import(export, 0, ImportCollisionPolicy::Reject, vec![])
        .unwrap();
    let request = RequestId::new();
    let mut child = helper(
        dir.path(),
        "crash-import",
        request,
        import.id,
        "import_before_commit",
    );
    assert_eq!(child.wait().unwrap().code(), Some(42));
    assert_eq!(service.status().unwrap().revision, 0);
    assert!(service.inspect(EntityId::Project(p)).is_err());
    service.apply_import(request, import.id).unwrap();
    let snapshot = service.backup(RequestId::new(), "before".into()).unwrap();
    let review = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::Rename {
                target: EntityId::Project(p),
                name: "after".into(),
            },
        )
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap();
    let restore = service.review_restore(snapshot.id).unwrap();
    let request = RequestId::new();
    let mut child = helper(
        dir.path(),
        "crash-restore",
        request,
        restore.id,
        "restore_original_moved",
    );
    assert_eq!(child.wait().unwrap().code(), Some(42));
    assert_eq!(
        service.status().unwrap_err().code,
        IssueCode::RecoveryRequired
    );
    service.apply_restore(request, restore.id).unwrap();
    let Entity::Project(value) = service.inspect(EntityId::Project(p)).unwrap() else {
        panic!()
    };
    assert_eq!(value.name, "source");
}
