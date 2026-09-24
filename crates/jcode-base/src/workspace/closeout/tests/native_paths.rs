use super::*;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[tokio::test]
async fn unusual_paths_and_opaque_link_targets_survive_inventory_restore_and_removal() {
    let fixture = Fixture::new();
    let names = [
        b"entry-\n".to_vec(),
        b"entry-\t".to_vec(),
        b"folder-\n/child\n\t".to_vec(),
    ];
    let paths = names
        .iter()
        .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes.clone())))
        .collect::<Vec<_>>();
    std::fs::create_dir(fixture.root.join(paths[2].parent().unwrap())).unwrap();
    for (index, path) in paths.iter().enumerate() {
        std::fs::write(fixture.root.join(path), format!("unique-content-{index}")).unwrap();
    }
    let link = PathBuf::from("opaque-target-link");
    let target = PathBuf::from(std::ffi::OsString::from_vec(b"/missing/\xff".to_vec()));
    std::os::unix::fs::symlink(&target, fixture.root.join(&link)).unwrap();
    let nested = fixture.root.join("nested\n\"repository");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    std::fs::write(nested.join("unique"), "nested unique Git history").unwrap();
    git(&nested, &["add", "."]);
    git(
        &nested,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qm",
            "nested",
        ],
    );
    let mut spec = fixture.spec(false);
    spec.full_archive = true;
    let started = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            spec,
        )
        .unwrap();
    let capture = capture(&fixture, started.operation);
    let refreshed = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let page = fixture
        .service
        .closeout_inventory(
            refreshed.operation,
            refreshed.inventory_digest.as_ref().unwrap(),
            0,
            200,
        )
        .unwrap();
    let json = serde_json::to_value(&page).unwrap();
    for path in &paths {
        let row = page
            .entries
            .iter()
            .find(|entry| entry.path == *path)
            .unwrap();
        let value = serde_json::to_value(row).unwrap();
        assert_eq!(value["path"], serde_json::json!(path.to_str().unwrap()));
    }
    let opaque = page
        .entries
        .iter()
        .find(|entry| entry.path == link)
        .unwrap();
    assert_eq!(
        serde_json::to_value(opaque).unwrap()["link_target"]["unix_bytes"],
        serde_json::json!(target.as_os_str().as_bytes())
    );
    assert_ne!(
        page.entries
            .iter()
            .find(|entry| entry.path == paths[0])
            .unwrap()
            .id,
        page.entries
            .iter()
            .find(|entry| entry.path == paths[1])
            .unwrap()
            .id
    );
    assert_eq!(
        serde_json::from_value::<CloseoutInventoryPage>(json).unwrap(),
        page
    );
    let record = fixture
        .service
        .preserve_closeout(refreshed.operation, refreshed.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let snapshots: Vec<git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.root == nested.canonicalize().unwrap())
    );
    let restored = stored
        .preservation
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join("verified-restore");
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(restored.join(path)).unwrap(),
            format!("unique-content-{index}")
        );
    }
    assert_eq!(std::fs::read_link(restored.join(&link)).unwrap(), target);
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let review = fixture
        .service
        .review_closeout_removal(record.operation, record.revision, &runtime)
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    fixture
        .service
        .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
        .await
        .unwrap();
    let closed = fixture
        .service
        .finish_closeout(record.operation, &runtime)
        .await
        .unwrap();
    assert_eq!(closed.stage, CloseoutStage::Closed);
    assert!(!fixture.root.exists());
    let progress = fixture
        .service
        .closeout_removal_progress(record.operation, closed.revision, 0, 200)
        .unwrap();
    assert_eq!(
        decode::<CloseoutRemovalPage>(&encode(&progress).unwrap()).unwrap(),
        progress
    );
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(restored.join(path)).unwrap(),
            format!("unique-content-{index}")
        );
    }
    verification::preservation(
        &load(&fixture.service.connection().unwrap(), record.operation).unwrap(),
    )
    .unwrap();
    finish_capture(&capture);
}

#[test]
fn opaque_path_codec_rejects_ambiguous_or_invalid_input() {
    for value in [
        serde_json::json!({"unix_bytes":[255,0]}),
        serde_json::json!({"unix_bytes":[97]}),
        serde_json::json!({"unix_bytes":[256]}),
        serde_json::json!({"unix_bytes":[255],"trusted":true}),
        serde_json::json!("bad\u{0}path"),
    ] {
        assert!(
            serde_json::from_value::<CloseoutDisposition>(
                serde_json::json!({"kind":"preserved","path":value})
            )
            .is_err()
        );
    }
    let normal = CloseoutDisposition::Preserved {
        path: "ordinary/Unicode-ç".into(),
    };
    assert_eq!(
        serde_json::to_value(&normal).unwrap(),
        serde_json::json!({"kind":"preserved","path":"ordinary/Unicode-ç"})
    );
    let opaque = CloseoutDisposition::Preserved {
        path: std::ffi::OsString::from_vec(b"original-\xff".to_vec()).into(),
    };
    assert_eq!(
        decode::<CloseoutDisposition>(&encode(&opaque).unwrap()).unwrap(),
        opaque
    );
}
