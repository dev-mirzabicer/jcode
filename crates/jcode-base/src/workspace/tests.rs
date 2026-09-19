use super::*;

fn fault(service: &WorkspaceService, stage: &'static str) -> WorkspaceService {
    let mut service = service.clone();
    service.fault = Some(std::sync::Arc::new(move |point| {
        if point == stage {
            Err(io("injected interruption"))
        } else {
            Ok(())
        }
    }));
    service
}

#[test]
fn verified_wal_snapshot_restore_and_corruption_repair() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let p = project(&service, "before");
    let connection = service.connection().unwrap();
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    change(
        &service,
        OrganizationChange::Rename {
            target: EntityId::Project(p),
            name: "in WAL".into(),
        },
    );
    assert!(
        std::fs::metadata(service.root.join("catalog.sqlite3-wal"))
            .unwrap()
            .len()
            > 0
    );
    let request = RequestId::new();
    let saved = service.backup(request, "named".into()).unwrap();
    assert_eq!(service.backup(request, "named".into()).unwrap(), saved);
    assert!(service.backup(request, "different".into()).is_err());
    drop(connection);
    change(
        &service,
        OrganizationChange::Rename {
            target: EntityId::Project(p),
            name: "after".into(),
        },
    );
    let review = service.review_restore(saved.id).unwrap();
    let request = RequestId::new();
    let restored = service.apply_restore(request, review.id).unwrap();
    assert_eq!(service.apply_restore(request, review.id).unwrap(), restored);
    match service.inspect(EntityId::Project(p)).unwrap() {
        Entity::Project(value) => assert_eq!(value.name, "in WAL"),
        _ => panic!(),
    }
    assert!(
        service
            .root
            .join("recovery")
            .join(request.to_string())
            .join("catalog.sqlite3")
            .is_file()
    );
    std::fs::write(
        service.root.join("catalog.sqlite3"),
        b"corrupt owned fixture",
    )
    .unwrap();
    let review = service.review_restore(saved.id).unwrap();
    assert!(review.current_revision.is_none());
    service.apply_restore(RequestId::new(), review.id).unwrap();
    assert!(service.inspect(EntityId::Project(p)).is_ok());
    std::fs::write(&saved.path, b"corrupt backup").unwrap();
    assert_eq!(
        service.review_restore(saved.id).unwrap_err().code,
        IssueCode::CorruptState
    );
}

#[test]
fn restore_interruption_is_recoverable_at_each_publication_boundary() {
    for stage in [
        "restore_intent",
        "restore_original_moved",
        "restore_published",
        "restore_receipt",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let service = WorkspaceService::new(dir.path());
        service.initialize(RequestId::new()).unwrap();
        let p = project(&service, "before");
        let saved = service.backup(RequestId::new(), "snapshot".into()).unwrap();
        change(
            &service,
            OrganizationChange::Rename {
                target: EntityId::Project(p),
                name: "after".into(),
            },
        );
        let review = service.review_restore(saved.id).unwrap();
        let request = RequestId::new();
        assert!(
            fault(&service, stage)
                .apply_restore(request, review.id)
                .is_err(),
            "{stage}"
        );
        service.apply_restore(request, review.id).unwrap();
        assert!(service.status().is_ok(), "{stage}");
        match service.inspect(EntityId::Project(p)).unwrap() {
            Entity::Project(value) => assert_eq!(value.name, "before"),
            _ => panic!(),
        }
    }
}

#[test]
fn portable_import_collision_new_identity_and_atomic_rollback() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = WorkspaceService::new(source_dir.path());
    source.initialize(RequestId::new()).unwrap();
    let p = project(&source, "project");
    let repo = repository(&source, "repo");
    change(
        &source,
        OrganizationChange::AssociateRepository {
            project: p,
            repository: repo,
        },
    );
    let grant = GrantDefinition {
        id: GrantId::new(),
        audience: Audience::Project(p),
        target: WriteTarget::ProjectMembers(p),
        state: GrantState::Active,
        revision: 1,
        copied_from: None,
    };
    portable::save_grant(&source.connection().unwrap(), &grant).unwrap();
    source
        .reconcile_session_index(&SessionIndex {
            session: "foreign-session".into(),
            placement: Placement::Project(p),
            session_revision: 1,
            operation: OperationId::new(),
            active: true,
            reconciled: true,
        })
        .unwrap();
    let export = source
        .export_project(RequestId::new(), p, "portable".into())
        .unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = WorkspaceService::new(dest_dir.path());
    dest.initialize(RequestId::new()).unwrap();
    let review = dest
        .review_import(export.clone(), 0, ImportCollisionPolicy::Reject, vec![])
        .unwrap();
    assert_eq!(review.disabled_grants, vec![grant.id]);
    let request = RequestId::new();
    assert!(
        fault(&dest, "import_before_commit")
            .apply_import(request, review.id)
            .is_err()
    );
    assert_eq!(dest.status().unwrap().revision, 0);
    assert_eq!(dest.list(Query::default(), None, 20).unwrap().total, 0);
    let receipt = dest.apply_import(request, review.id).unwrap();
    assert_eq!(dest.apply_import(request, review.id).unwrap(), receipt);
    assert_eq!(
        portable::grants(&dest.connection().unwrap()).unwrap()[0].state,
        GrantState::Disabled
    );
    assert!(dest.sessions(None, None, 20).unwrap().is_empty());
    let collision = dest
        .review_import(
            export.clone(),
            dest.status().unwrap().revision,
            ImportCollisionPolicy::Reject,
            vec![],
        )
        .unwrap();
    assert_eq!(collision.collisions.len(), 2);
    assert!(dest.apply_import(RequestId::new(), collision.id).is_err());
    let copy = dest
        .review_import(
            export,
            dest.status().unwrap().revision,
            ImportCollisionPolicy::NewIdentities,
            vec![],
        )
        .unwrap();
    dest.apply_import(RequestId::new(), copy.id).unwrap();
    assert_eq!(dest.list(Query::default(), None, 20).unwrap().total, 4);
    assert_eq!(source.list(Query::default(), None, 20).unwrap().total, 2);
}

#[test]
fn automatic_snapshot_retention_is_bounded_and_named_backups_remain() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let named = service.backup(RequestId::new(), "keep".into()).unwrap();
    for n in 0..13 {
        project(&service, &format!("{n}"));
    }
    let snapshots = service.snapshots().unwrap();
    assert_eq!(snapshots.iter().filter(|s| s.automatic).count(), 10);
    assert!(snapshots.contains(&named));
}

fn change(service: &WorkspaceService, change: OrganizationChange) -> Receipt {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
}
fn project(service: &WorkspaceService, name: &str) -> ProjectId {
    match change(
        service,
        OrganizationChange::CreateProject { name: name.into() },
    )
    .targets[0]
    {
        EntityId::Project(id) => id,
        _ => panic!("project"),
    }
}
fn repository(service: &WorkspaceService, name: &str) -> RepositoryId {
    match change(
        service,
        OrganizationChange::CreateRepository {
            name: name.into(),
            remotes: vec!["https://example.invalid/repo.git".into()],
        },
    )
    .targets[0]
    {
        EntityId::Repository(id) => id,
        _ => panic!("repository"),
    }
}

#[test]
fn organization_ids_replays_conflicts_and_paging() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let a = project(&service, "same");
    let b = project(&service, "same");
    assert_ne!(a, b);
    let r1 = repository(&service, "same");
    let r2 = repository(&service, "same");
    assert_ne!(r1, r2);
    for project in [a, b] {
        change(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository: r1,
            },
        );
    }
    let query = Query {
        kind: Some(EntityKind::Repository),
        project: Some(a),
        ..Query::default()
    };
    assert_eq!(service.list(query, None, 20).unwrap().items.len(), 1);
    let revision = service.status().unwrap().revision;
    let first = service
        .review_organization_change(
            revision,
            OrganizationChange::Rename {
                target: EntityId::Project(a),
                name: "renamed".into(),
            },
        )
        .unwrap();
    let stale = service
        .review_organization_change(
            revision,
            OrganizationChange::Rename {
                target: EntityId::Project(b),
                name: "stale".into(),
            },
        )
        .unwrap();
    let request = RequestId::new();
    let receipt = service
        .apply_organization_change(request, first.id)
        .unwrap();
    assert_eq!(
        service
            .apply_organization_change(request, first.id)
            .unwrap(),
        receipt
    );
    assert_eq!(
        service
            .apply_organization_change(request, stale.id)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        service
            .apply_organization_change(RequestId::new(), stale.id)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    let page = service.list(Query::default(), None, 2).unwrap();
    assert_eq!(page.total, 4);
    assert!(page.next.is_some());
    let tail = service
        .list(Query::default(), page.next.clone(), 2)
        .unwrap();
    assert_eq!(tail.items.len(), 2);
    assert!(tail.next.is_none());
    project(&service, "later");
    assert_eq!(
        service
            .list(Query::default(), page.next, 2)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert!(
        service
            .review_organization_change(
                service.status().unwrap().revision,
                OrganizationChange::CreateRepository {
                    name: "bad".into(),
                    remotes: vec!["https://secret@example.invalid/repo".into()]
                }
            )
            .is_err()
    );
}

#[test]
fn archive_keeps_active_sessions_and_retirement_keeps_references() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let p = project(&service, "project");
    let area = match change(
        &service,
        OrganizationChange::CreateWorkArea {
            project: p,
            name: "area".into(),
        },
    )
    .targets[0]
    {
        EntityId::WorkArea(id) => id,
        _ => panic!(),
    };
    let index = SessionIndex {
        session: "session-fixture".into(),
        placement: Placement::WorkArea(area),
        session_revision: 3,
        operation: OperationId::new(),
        active: true,
        reconciled: true,
    };
    service.reconcile_session_index(&index).unwrap();
    let revision = service.status().unwrap().revision;
    service.reconcile_session_index(&index).unwrap();
    assert_eq!(service.status().unwrap().revision, revision);
    let mut stale = index.clone();
    stale.session_revision = 2;
    assert_eq!(
        service.reconcile_session_index(&stale).unwrap_err().code,
        IssueCode::Conflict
    );
    change(
        &service,
        OrganizationChange::Archive {
            target: EntityId::Project(p),
            archived: true,
        },
    );
    assert_eq!(service.list(Query::default(), None, 20).unwrap().total, 2);
    assert_eq!(
        service
            .sessions(Some(EntityId::Project(p)), None, 20)
            .unwrap(),
        vec![index.clone()]
    );
    let mut idle = index;
    idle.active = false;
    service.reconcile_session_index(&idle).unwrap();
    assert_eq!(service.list(Query::default(), None, 20).unwrap().total, 0);
    change(
        &service,
        OrganizationChange::Archive {
            target: EntityId::Project(p),
            archived: false,
        },
    );
    assert_eq!(service.list(Query::default(), None, 20).unwrap().total, 2);
    let review = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::DiscardUnused {
                target: EntityId::WorkArea(area),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .apply_organization_change(RequestId::new(), review.id)
            .unwrap_err()
            .code,
        IssueCode::Referenced
    );
    change(
        &service,
        OrganizationChange::Retire {
            target: EntityId::WorkArea(area),
        },
    );
    assert_eq!(
        service
            .sessions(Some(EntityId::Project(p)), None, 20)
            .unwrap()
            .len(),
        1
    );
    let unused = project(&service, "unused");
    change(
        &service,
        OrganizationChange::DiscardUnused {
            target: EntityId::Project(unused),
        },
    );
    assert!(service.inspect(EntityId::Project(unused)).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn physical_roots_have_one_home_and_aliases_deduplicate() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let service = WorkspaceService::new(&state);
    service.initialize(RequestId::new()).unwrap();
    let p = project(&service, "one");
    let other = project(&service, "two");
    let r = repository(&service, "repo");
    change(
        &service,
        OrganizationChange::AssociateRepository {
            project: p,
            repository: r,
        },
    );
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    let id = match change(
        &service,
        OrganizationChange::RegisterLocation {
            name: "repo".into(),
            path: root.clone(),
            registration: Registration::Checkout {
                home: Home::Project(p),
                repository: r,
            },
        },
    )
    .targets[0]
    {
        EntityId::Location(id) => id,
        _ => panic!(),
    };
    let alias = dir.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert_eq!(
        change(
            &service,
            OrganizationChange::RegisterLocation {
                name: "alias".into(),
                path: alias,
                registration: Registration::Standalone
            }
        )
        .targets,
        vec![EntityId::Location(id)]
    );
    assert!(
        service
            .review_organization_change(
                service.status().unwrap().revision,
                OrganizationChange::MoveLocation {
                    location: id,
                    home: Home::Project(other),
                    associate_repository: false
                }
            )
            .is_err()
    );
    change(
        &service,
        OrganizationChange::MoveLocation {
            location: id,
            home: Home::Project(other),
            associate_repository: true,
        },
    );
    assert_eq!(
        service
            .list(
                Query {
                    project: Some(p),
                    kind: Some(EntityKind::Location),
                    ..Query::default()
                },
                None,
                20
            )
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        service
            .list(
                Query {
                    project: Some(other),
                    kind: Some(EntityKind::Location),
                    ..Query::default()
                },
                None,
                20
            )
            .unwrap()
            .total,
        1
    );
    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(plain.join("keep"), b"untouched").unwrap();
    let standalone = match change(
        &service,
        OrganizationChange::RegisterLocation {
            name: "plain".into(),
            path: plain.clone(),
            registration: Registration::Standalone,
        },
    )
    .targets[0]
    {
        EntityId::Location(id) => id,
        _ => panic!(),
    };
    change(
        &service,
        OrganizationChange::AdoptStandalone {
            location: standalone,
            home: Home::Project(p),
            repository: None,
            associate_repository: false,
        },
    );
    change(
        &service,
        OrganizationChange::Retire {
            target: EntityId::Location(standalone),
        },
    );
    assert_eq!(std::fs::read(plain.join("keep")).unwrap(), b"untouched");
    assert!(root.join(".git").is_dir());
    let lease = service.acquire_root(id).unwrap();
    assert!(service.acquire_root(id).is_err());
    drop(lease);
    assert!(service.acquire_root(id).is_ok());
}

#[test]
fn explicit_initialization_and_missing_catalog_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = WorkspaceService::new(dir.path());
    assert!(catalog.status().is_err());
    assert!(!catalog.root().exists());
    let request = RequestId::new();
    let initial = catalog.initialize(request).unwrap();
    assert_eq!(initial.revision, 0);
    assert!(!initial.managed_rollout);
    assert_eq!(catalog.initialize(request).unwrap(), initial);
    std::fs::rename(
        catalog.root().join("catalog.sqlite3"),
        catalog.root().join("saved.sqlite3"),
    )
    .unwrap();
    assert!(catalog.status().is_err());
    assert!(catalog.initialize(RequestId::new()).is_err());
    assert!(!catalog.root().join("catalog.sqlite3").exists());
}

#[test]
fn corrupt_and_unknown_schema_never_become_empty_catalogs() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = WorkspaceService::new(dir.path());
    catalog.initialize(RequestId::new()).unwrap();
    let connection = catalog.connection().unwrap();
    connection
        .pragma_update(None, "user_version", 9000)
        .unwrap();
    drop(connection);
    assert_eq!(catalog.status().unwrap_err().code, IssueCode::CorruptState);
    assert!(catalog.initialize(RequestId::new()).is_err());
    let db = catalog.root().join("catalog.sqlite3");
    std::fs::write(&db, b"broken fixture database").unwrap();
    assert_eq!(catalog.status().unwrap_err().code, IssueCode::CorruptState);
    assert_eq!(std::fs::read(db).unwrap(), b"broken fixture database");
}

#[test]
fn replacement_lease_excludes_readers_and_existing_connections() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = WorkspaceService::new(dir.path());
    catalog.initialize(RequestId::new()).unwrap();
    let reader = catalog.lease(false).unwrap();
    assert!(catalog.lease(true).is_err());
    let other_reader = catalog.lease(false).unwrap();
    drop(reader);
    assert!(catalog.lease(true).is_err());
    drop(other_reader);
    let replacement = catalog.lease(true).unwrap();
    assert_eq!(catalog.status().unwrap_err().code, IssueCode::Busy);
    drop(replacement);
    assert!(catalog.status().is_ok());
}

#[cfg(unix)]
#[test]
fn catalog_and_wal_are_private_and_database_symlinks_reject() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let catalog = WorkspaceService::new(dir.path());
    catalog.initialize(RequestId::new()).unwrap();
    let connection = catalog.connection().unwrap();
    connection
        .execute("UPDATE catalog SET revision=1", [])
        .unwrap();
    for leaf in [
        "catalog.sqlite3",
        "catalog.sqlite3-wal",
        "catalog.sqlite3-shm",
    ] {
        assert_eq!(
            std::fs::metadata(catalog.root().join(leaf))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    assert_eq!(
        std::fs::metadata(catalog.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    drop(connection);
    let db = catalog.root().join("catalog.sqlite3");
    let saved = catalog.root().join("original.sqlite3");
    std::fs::rename(&db, &saved).unwrap();
    symlink(&saved, &db).unwrap();
    assert!(catalog.status().is_err());
}
