//! Read-only management queries over real private catalog fixtures.
use super::*;

fn git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkout(dir: &Path, name: &str) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    root.canonicalize().unwrap()
}

fn begin(service: &WorkspaceService, location: LocationId) -> CloseoutRecord {
    service
        .begin_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            service.status().unwrap().revision,
            CloseoutSpec {
                location,
                expected_generation: 1,
                preservation_directory: None,
                conditional_no_loss: false,
                full_archive: false,
            },
        )
        .unwrap()
}

#[test]
fn operation_discovery_finds_records_without_their_request_ids() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    let first = test_support::register_checkout(&service, &checkout(dir.path(), "first"));
    let started = begin(&service, first);
    let all = service
        .operations(OperationQuery::default(), None, 50)
        .unwrap();
    assert_eq!(all.total, 1);
    let entry = &all.items[0];
    assert_eq!(entry.state, OperationState::Pending);
    assert!(entry.targets.contains(&EntityId::Location(first)));
    let WorkspaceOperation::Closeout(record) = &entry.operation else {
        panic!("closeout record expected");
    };
    assert_eq!(record.operation, started.operation);
    assert_eq!(entry.operation.kind(), OperationKind::Closeout);

    let other = WorkspaceService::new(dir.path());
    let second_root = checkout(dir.path(), "second");
    let review = other
        .review_organization_change(
            other.status().unwrap().revision,
            OrganizationChange::RegisterLocation {
                name: "second".into(),
                path: second_root,
                registration: Registration::Standalone,
            },
        )
        .unwrap();
    let EntityId::Location(second) = other
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
    else {
        panic!("location");
    };
    let filtered = service
        .operations(
            OperationQuery {
                target: Some(EntityId::Location(second)),
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap();
    assert_eq!(filtered.total, 0, "another root's closeout is not listed");
    let kinds = service
        .operations(
            OperationQuery {
                kinds: vec![OperationKind::Clone],
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap();
    assert_eq!(kinds.total, 0);
}

#[test]
fn operation_pages_are_newest_first_and_reject_stale_continuations() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    let first = test_support::register_checkout(&service, &checkout(dir.path(), "a"));
    let older = begin(&service, first);
    let Entity::Location(Location {
        home: Some(home),
        kind: LocationKind::Checkout { repository, .. },
        ..
    }) = service.inspect(EntityId::Location(first)).unwrap()
    else {
        panic!("registered checkout");
    };
    let review = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::RegisterLocation {
                name: "b".into(),
                path: checkout(dir.path(), "b"),
                registration: Registration::Checkout { home, repository },
            },
        )
        .unwrap();
    let EntityId::Location(second) = service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
    else {
        panic!("location");
    };
    let newer = begin(&service, second);
    let page = service
        .operations(OperationQuery::default(), None, 1)
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].operation.operation(), newer.operation);
    let next = page.next.clone().expect("continuation");
    let rest = service
        .operations(OperationQuery::default(), Some(next.clone()), 1)
        .unwrap();
    assert_eq!(rest.items[0].operation.operation(), older.operation);
    assert!(rest.next.is_none());
    let error = service
        .operations(
            OperationQuery {
                unfinished_only: true,
                ..Default::default()
            },
            Some(next.clone()),
            1,
        )
        .unwrap_err();
    assert_eq!(
        error.code,
        IssueCode::Conflict,
        "cursor belongs to another query"
    );
    service
        .revoke_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            newer.operation,
            newer.revision,
        )
        .unwrap();
    let error = service
        .operations(OperationQuery::default(), Some(next), 1)
        .unwrap_err();
    assert_eq!(error.code, IssueCode::Conflict, "revision changed");
    let unfinished = service
        .operations(
            OperationQuery {
                unfinished_only: true,
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap();
    assert!(
        unfinished
            .items
            .iter()
            .any(|entry| entry.state == OperationState::Failed)
    );
    assert!(
        service
            .operations(OperationQuery::default(), None, 0)
            .is_err()
    );
    assert!(
        service
            .operations(OperationQuery::default(), None, 201)
            .is_err()
    );
}

#[test]
fn session_location_view_reports_legacy_directory_and_catalog_absence() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some("/synthetic/legacy".into());
    let view = service.session_location_view(&session);
    assert!(view.location.is_none());
    assert_eq!(
        view.legacy_working_dir.as_deref(),
        Some(Path::new("/synthetic/legacy"))
    );
    assert!(view.catalog_revision.is_none());
    assert!(
        view.catalog_issue.is_some(),
        "uninitialized catalog is reported"
    );
    service.initialize(RequestId::new()).unwrap();
    let view = service.session_location_view(&session);
    assert_eq!(view.catalog_revision, Some(0));
    assert!(view.catalog_issue.is_none());
    assert!(view.pending.is_empty());
}

#[test]
fn startup_copy_plans_require_a_physical_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    let target = test_support::register_checkout(&service, &checkout(dir.path(), "target"));
    let source = checkout(dir.path(), "source");
    let plans = service.startup_copy_plans(source.clone(), target).unwrap();
    assert_eq!(plans.source_entries, 0);
    assert_eq!(plans.catalog_revision, service.status().unwrap().revision);
    std::fs::create_dir_all(source.join("nested")).unwrap();
    let error = service
        .startup_copy_plans(source.join("nested"), target)
        .unwrap_err();
    assert_eq!(error.code, IssueCode::InvalidInput);
}
