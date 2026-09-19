use super::*;

#[cfg(target_os = "macos")]
#[test]
fn replaced_standalone_cannot_be_adopted_under_old_identity() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let service = WorkspaceService::new(&state);
    service.initialize(RequestId::new()).unwrap();
    let p = project(&service, "project");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let EntityId::Location(id) = change(
        &service,
        OrganizationChange::RegisterLocation {
            name: "root".into(),
            path: root.clone(),
            registration: Registration::Standalone,
        },
    )
    .targets[0] else {
        panic!()
    };
    let review = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::AdoptStandalone {
                location: id,
                home: Home::Project(p),
                repository: None,
                associate_repository: false,
            },
        )
        .unwrap();
    std::fs::rename(&root, dir.path().join("original")).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        service
            .apply_organization_change(RequestId::new(), review.id)
            .unwrap_err()
            .code,
        IssueCode::ReplacedRoot
    );
    assert!(
        service
            .review_organization_change(
                service.status().unwrap().revision,
                OrganizationChange::AdoptStandalone {
                    location: id,
                    home: Home::Project(p),
                    repository: None,
                    associate_repository: false
                }
            )
            .is_err()
    );
    let Entity::Location(loc) = service.inspect(EntityId::Location(id)).unwrap() else {
        panic!()
    };
    assert!(loc.home.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn closed_catalog_history_survives_export_without_resurrecting_roots() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let service = WorkspaceService::new(&state);
    service.initialize(RequestId::new()).unwrap();
    let p = project(&service, "project");
    let r = repository(&service, "repo");
    change(
        &service,
        OrganizationChange::AssociateRepository {
            project: p,
            repository: r,
        },
    );
    let root = dir.path().join("repo");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    let EntityId::Location(id) = change(
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
    .targets[0] else {
        panic!()
    };
    // Synthetic completed-closeout metadata, not a claim of a removal executor.
    let connection = service.connection().unwrap();
    let Entity::Location(mut loc) = entity(&connection, EntityId::Location(id)).unwrap() else {
        panic!()
    };
    loc.lifecycle = LocationLifecycle::Closed;
    organization::save_entity(&connection, &Entity::Location(loc)).unwrap();
    connection
        .execute(
            "UPDATE bindings SET live_key=NULL WHERE location=?1",
            [id.to_string()],
        )
        .unwrap();
    let history = portable::ClosedReference {
        location: id,
        operation: OperationId::new(),
        preservation_paths: vec![],
        report: None,
    };
    connection
        .execute(
            "INSERT INTO closed_history VALUES(?1,?2)",
            rusqlite::params![id.to_string(), encode(&history).unwrap()],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        service
            .list(
                Query {
                    visibility: Visibility::Closed,
                    ..Query::default()
                },
                None,
                20
            )
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        service
            .list(
                Query {
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
    let export = service
        .export_project(RequestId::new(), p, "closed".into())
        .unwrap();
    let other = tempfile::tempdir().unwrap();
    let target = WorkspaceService::new(other.path());
    target.initialize(RequestId::new()).unwrap();
    let review = target
        .review_import(export, 0, ImportCollisionPolicy::Reject, vec![])
        .unwrap();
    target.apply_import(RequestId::new(), review.id).unwrap();
    assert_eq!(
        target
            .list(
                Query {
                    visibility: Visibility::Closed,
                    ..Query::default()
                },
                None,
                20
            )
            .unwrap()
            .total,
        1
    );
    assert!(root.join(".git").is_dir());
}

#[test]
fn interrupted_initialization_resumes_only_its_recorded_request() {
    for stage in ["initialize_marker", "initialize_schema"] {
        let dir = tempfile::tempdir().unwrap();
        let service = WorkspaceService::new(dir.path());
        let request = RequestId::new();
        assert!(fault(&service, stage).initialize(request).is_err());
        assert_eq!(
            service.initialize(RequestId::new()).unwrap_err().code,
            IssueCode::RecoveryRequired
        );
        assert!(service.status().is_err());
        let state = service.initialize(request).unwrap();
        assert_eq!(state.revision, 0);
    }
}

#[test]
fn failed_automatic_backup_reports_committed_mutation_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(dir.path());
    service.initialize(RequestId::new()).unwrap();
    let snapshots = service.root.join("snapshots");
    std::fs::rename(&snapshots, service.root.join("retained-snapshots")).unwrap();
    std::fs::write(&snapshots, b"owned fixture obstruction").unwrap();
    let review = service
        .review_organization_change(
            0,
            OrganizationChange::CreateProject {
                name: "committed".into(),
            },
        )
        .unwrap();
    let request = RequestId::new();
    let receipt = service
        .apply_organization_change(request, review.id)
        .unwrap();
    assert_eq!(receipt.issues[0].code, IssueCode::BackupFailed);
    assert_eq!(service.status().unwrap().revision, 1);
    assert_eq!(
        service
            .apply_organization_change(request, review.id)
            .unwrap(),
        receipt
    );
    assert_eq!(service.list(Query::default(), None, 20).unwrap().total, 1);
}

#[test]
fn named_backup_retries_preserve_one_snapshot_after_interruption() {
    for stage in ["backup_prepared", "backup_published", "backup_receipt"] {
        let dir = tempfile::tempdir().unwrap();
        let service = WorkspaceService::new(dir.path());
        service.initialize(RequestId::new()).unwrap();
        let request = RequestId::new();
        assert!(
            fault(&service, stage)
                .backup(request, "named".into())
                .is_err()
        );
        let saved = service.backup(request, "named".into()).unwrap();
        assert_eq!(service.backup(request, "named".into()).unwrap(), saved);
        assert_eq!(
            service
                .snapshots()
                .unwrap()
                .iter()
                .filter(|s| !s.automatic)
                .count(),
            1
        );
    }
}

#[test]
fn foreign_snapshot_and_unrecognized_schema_leave_original_state_untouched() {
    let a = tempfile::tempdir().unwrap();
    let source = WorkspaceService::new(a.path());
    source.initialize(RequestId::new()).unwrap();
    project(&source, "foreign");
    let mut snapshot = source.backup(RequestId::new(), "foreign".into()).unwrap();
    let b = tempfile::tempdir().unwrap();
    let target = WorkspaceService::new(b.path());
    target.initialize(RequestId::new()).unwrap();
    let p = project(&target, "local");
    let old = target.inspect(EntityId::Project(p)).unwrap();
    let destination = target
        .root
        .join("snapshots")
        .join(format!("{}.sqlite3", snapshot.id));
    std::fs::copy(&snapshot.path, &destination).unwrap();
    snapshot.path = destination;
    storage::atomic_json(
        &target
            .root
            .join("snapshots")
            .join(format!("snapshot-{}.json", snapshot.id)),
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        target.review_restore(snapshot.id).unwrap_err().code,
        IssueCode::CorruptState
    );
    assert_eq!(target.inspect(EntityId::Project(p)).unwrap(), old);
    let own = target.backup(RequestId::new(), "own".into()).unwrap();
    let connection = storage::raw_connection(&own.path, false).unwrap();
    connection
        .execute_batch("CREATE TABLE unknown_authority(value TEXT);")
        .unwrap();
    drop(connection);
    let mut forged = own.clone();
    forged.sha256 = backup::file_digest(&own.path).unwrap();
    storage::atomic_json(
        &target
            .root
            .join("snapshots")
            .join(format!("snapshot-{}.json", own.id)),
        &forged,
    )
    .unwrap();
    assert_eq!(
        target.review_restore(own.id).unwrap_err().code,
        IssueCode::CorruptState
    );
    assert_eq!(target.inspect(EntityId::Project(p)).unwrap(), old);
}

#[cfg(target_os = "macos")]
#[test]
fn portable_offline_locations_and_reviewed_remap_do_not_create_paths_or_activate_grants() {
    let a = tempfile::tempdir().unwrap();
    let state = a.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let source = WorkspaceService::new(&state);
    source.initialize(RequestId::new()).unwrap();
    let p = project(&source, "project");
    let root = a.path().join("directory");
    std::fs::create_dir(&root).unwrap();
    let EntityId::Location(location) = change(
        &source,
        OrganizationChange::RegisterLocation {
            name: "directory".into(),
            path: root.clone(),
            registration: Registration::Directory {
                home: Home::Project(p),
            },
        },
    )
    .targets[0] else {
        panic!()
    };
    let grant = GrantDefinition {
        id: GrantId::new(),
        audience: Audience::Project(p),
        target: WriteTarget::Root(location),
        state: GrantState::Active,
        revision: 1,
        copied_from: None,
    };
    portable::save_grant(&source.connection().unwrap(), &grant).unwrap();
    let export = source
        .export_project(RequestId::new(), p, "fixture".into())
        .unwrap();
    std::fs::rename(&root, a.path().join("moved-outside-jcode")).unwrap();
    let b = tempfile::tempdir().unwrap();
    let target = WorkspaceService::new(b.path());
    target.initialize(RequestId::new()).unwrap();
    let review = target
        .review_import(export.clone(), 0, ImportCollisionPolicy::Reject, vec![])
        .unwrap();
    assert_eq!(review.unavailable, vec![location]);
    target.apply_import(RequestId::new(), review.id).unwrap();
    let Entity::Location(imported) = target.inspect(EntityId::Location(location)).unwrap() else {
        panic!()
    };
    assert_eq!(imported.lifecycle, LocationLifecycle::Unavailable);
    assert!(!root.exists());
    assert_eq!(
        portable::grants(&target.connection().unwrap()).unwrap()[0].state,
        GrantState::Disabled
    );
    let replacement = a.path().join("explicit-new-root");
    std::fs::create_dir(&replacement).unwrap();
    let review = target
        .review_import(
            export,
            target.status().unwrap().revision,
            ImportCollisionPolicy::NewIdentities,
            vec![LocationRemap {
                location,
                path: replacement.clone(),
            }],
        )
        .unwrap();
    let receipt = target.apply_import(RequestId::new(), review.id).unwrap();
    let id = *receipt
        .targets
        .iter()
        .find(|id| matches!(id, EntityId::Location(_)))
        .unwrap();
    let Entity::Location(imported) = target.inspect(id).unwrap() else {
        panic!()
    };
    assert_ne!(imported.id, location);
    assert_eq!(imported.lifecycle, LocationLifecycle::Ready);
    assert_eq!(imported.observed_path, replacement.canonicalize().unwrap());
}

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
        "restore_staged",
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
    let reexport = dest
        .export_project(RequestId::new(), p, "roundtrip".into())
        .unwrap();
    let reexport: serde_json::Value =
        serde_json::from_slice(&std::fs::read(reexport).unwrap()).unwrap();
    assert_eq!(
        reexport["catalog"]["inherited_session_references"][0]["sessions"][0]["session"],
        "foreign-session"
    );
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
