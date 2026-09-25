//! Combined real Git/filesystem closeout, with non-disruptive volume faults.
use super::*;
use std::os::unix::fs::{MetadataExt, symlink};

fn identity(root: &Path) {
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
}

#[tokio::test]
async fn combined_disposal_preserves_submodule_lfs_links_and_recovers_volume_loss() {
    let fixture = Fixture::new();
    identity(&fixture.root);
    let source = fixture._directory.path().join("submodule-source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    identity(&source);
    std::fs::write(source.join("committed"), "submodule committed information").unwrap();
    git(&source, &["add", "committed"]);
    git(&source, &["commit", "-qm", "submodule"]);
    let source_refs = git_text(&source, &["show-ref"]);
    git(
        &fixture.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--",
            source.to_str().unwrap(),
            "component",
        ],
    );
    git(&fixture.root, &["commit", "-qm", "recorded gitlink"]);
    let component = fixture.root.join("component").canonicalize().unwrap();
    std::fs::write(component.join("index-only"), "staged submodule information").unwrap();
    git(&component, &["add", "index-only"]);
    std::fs::write(component.join("committed"), "dirty submodule information").unwrap();
    let sub_index = git_text(&component, &["ls-files", "--stage"]);
    let staged_blob = git_text(&component, &["rev-parse", ":index-only"])
        .trim()
        .to_string();

    std::fs::write(
        fixture.root.join(".gitattributes"),
        "asset filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    let mut objects = Vec::new();
    for data in [vec![17; 65536], vec![29; 65536]] {
        let oid = digest(&data);
        let path = fixture
            .root
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(&oid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &data).unwrap();
        std::fs::write(
            fixture.root.join("asset"),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
                data.len()
            ),
        )
        .unwrap();
        git(
            &fixture.root,
            &[
                "-c",
                "filter.lfs.clean=",
                "-c",
                "filter.lfs.required=false",
                "add",
                "asset",
                ".gitattributes",
            ],
        );
        git(&fixture.root, &["commit", "-qm", "LFS fixture version"]);
        objects.push((oid, data));
    }
    std::fs::write(fixture.root.join("asset"), &objects[1].1).unwrap();
    std::fs::write(fixture.root.join("hard-a"), "hardlink information").unwrap();
    std::fs::hard_link(fixture.root.join("hard-a"), fixture.root.join("hard-b")).unwrap();
    let outside_hard = fixture._directory.path().join("outside-hardlink");
    std::fs::hard_link(fixture.root.join("hard-a"), &outside_hard).unwrap();
    let outside = fixture._directory.path().join("outside-symlink-target");
    std::fs::write(&outside, "outside remains untouched").unwrap();
    symlink(&outside, fixture.root.join("symlink")).unwrap();

    let external = std::env::var_os("JCODE_WP07_EXTERNAL_MOUNT").map(|mount| {
        let mount = PathBuf::from(mount);
        let binding = fixture.service.resolver.bind_directory(&mount).unwrap();
        assert_eq!(
            binding.volume().as_str(),
            std::env::var("JCODE_WP07_EXPECTED_VOLUME_UUID").unwrap()
        );
        let directory = tempfile::Builder::new()
            .prefix(".jcode-wp07-combined-owned-")
            .tempdir_in(mount)
            .unwrap();
        std::fs::write(
            directory.path().join("OWNER.json"),
            b"{\"fixture\":\"WP07-combined-disposal\"}",
        )
        .unwrap();
        directory
    });
    let mut spec = fixture.spec(false);
    spec.full_archive = true;
    spec.preservation_directory = external
        .as_ref()
        .map(|directory| directory.path().to_path_buf());
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
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let refreshed = fixture
        .service
        .refresh_closeout_in(started.operation, started.revision, &sessions, &capture)
        .await
        .unwrap();
    let preserved = fixture
        .service
        .preserve_closeout(started.operation, refreshed.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), started.operation).unwrap();
    let snapshots: Vec<super::super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert_eq!(snapshots.len(), 2);
    let manifest: preservation::PreservationManifest =
        storage::read_json(stored.preservation.as_ref().unwrap()).unwrap();
    let restored = manifest.files.parent().unwrap().join("verified-restore");
    let review = fixture
        .service
        .review_closeout_removal(started.operation, preserved.revision, &runtime)
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    fixture
        .service
        .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
        .await
        .unwrap();

    // Inject only the volume observation. Never unmount or disrupt a real disk.
    let mut offline = fixture.service.clone();
    offline.resolver = offline
        .resolver
        .without_volume_for_test(stored.destination.volume().clone());
    assert!(
        offline
            .finish_closeout(started.operation, &runtime)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.join("asset")).unwrap(),
        objects[1].1
    );
    assert!(
        load(&fixture.service.connection().unwrap(), started.operation)
            .unwrap()
            .removal
            .is_none()
    );
    let mut interrupted = fixture.service.clone();
    interrupted.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_entry_unlinked" {
            Err(io("combined fixture interrupted effect"))
        } else {
            Ok(())
        }
    }));
    assert!(
        interrupted
            .finish_closeout(started.operation, &runtime)
            .await
            .is_err()
    );
    let partial = fixture.service.inspect_closeout(started.operation).unwrap();
    assert!(partial.quarantine.as_ref().unwrap().exists());
    assert!(
        offline
            .finish_closeout(started.operation, &runtime)
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .inspect_closeout(started.operation)
            .unwrap()
            .removed_entries,
        partial.removed_entries
    );
    let closed = fixture
        .service
        .finish_closeout(started.operation, &runtime)
        .await
        .unwrap();
    assert_eq!(closed.stage, CloseoutStage::Closed);
    assert!(!fixture.root.exists() && !closed.quarantine.as_ref().unwrap().exists());
    verification::preservation(
        &load(&fixture.service.connection().unwrap(), started.operation).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(restored.join("component/committed")).unwrap(),
        "dirty submodule information"
    );
    assert_eq!(
        std::fs::read_to_string(restored.join("component/index-only")).unwrap(),
        "staged submodule information"
    );
    assert_eq!(std::fs::read(restored.join("asset")).unwrap(), objects[1].1);
    assert_eq!(
        std::fs::metadata(restored.join("hard-a")).unwrap().ino(),
        std::fs::metadata(restored.join("hard-b")).unwrap().ino()
    );
    assert_eq!(
        std::fs::read_link(restored.join("symlink")).unwrap(),
        outside
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "outside remains untouched"
    );
    assert_eq!(
        std::fs::read_to_string(&outside_hard).unwrap(),
        "hardlink information"
    );
    assert_eq!(git_text(&source, &["show-ref"]), source_refs);
    git(&source, &["fsck", "--full", "--no-reflogs"]);
    for (snapshot, (bundle, _)) in snapshots.iter().zip(&manifest.bundles) {
        let repository = bundle.parent().unwrap().join("restored.git");
        git(&repository, &["fsck", "--full", "--no-reflogs"]);
        if snapshot.root == component {
            assert_eq!(git_text(&repository, &["ls-files", "--stage"]), sub_index);
            assert_eq!(
                git_text(&repository, &["cat-file", "blob", &staged_blob]),
                "staged submodule information"
            );
        } else {
            for (oid, data) in &objects {
                assert_eq!(
                    std::fs::read(
                        repository
                            .join("lfs/objects")
                            .join(&oid[..2])
                            .join(&oid[2..4])
                            .join(oid)
                    )
                    .unwrap(),
                    *data
                );
            }
        }
    }
    assert_eq!(
        fixture
            .service
            .closed_checkout_history(fixture.location)
            .unwrap()
            .record,
        Some(closed)
    );
    finish_capture(&capture);
    if let Some(external) = external {
        let path = external.path().to_path_buf();
        external.close().unwrap();
        assert!(!path.exists());
        eprintln!(
            "Owned external combined fixture cleaned: {}",
            path.display()
        );
    }
}
