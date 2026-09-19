use super::*;

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
