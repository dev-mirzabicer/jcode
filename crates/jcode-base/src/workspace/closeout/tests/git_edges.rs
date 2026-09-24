use super::*;

#[tokio::test]
async fn sparse_conflict_and_orphan_indexes_restore_without_source() {
    for case in ["orphan", "sparse", "conflict"] {
        let fixture = Fixture::new();
        git(&fixture.root, &["config", "user.name", "Fixture"]);
        git(
            &fixture.root,
            &["config", "user.email", "fixture@localhost"],
        );
        std::fs::create_dir(fixture.root.join("included")).unwrap();
        std::fs::create_dir(fixture.root.join("excluded")).unwrap();
        std::fs::write(fixture.root.join("included/file"), "included").unwrap();
        std::fs::write(fixture.root.join("excluded/file"), "excluded").unwrap();
        std::fs::write(fixture.root.join("tracked"), "base\n").unwrap();
        git(&fixture.root, &["add", "."]);
        git(&fixture.root, &["commit", "-qm", "base tree"]);
        match case {
            "orphan" => {
                git(&fixture.root, &["checkout", "--orphan", "unborn"]);
                std::fs::write(fixture.root.join("unique-stage"), "unborn staged contents")
                    .unwrap();
                git(&fixture.root, &["add", "unique-stage"]);
                assert!(
                    !git_text(&fixture.root, &["for-each-ref", "--format=%(refname)"])
                        .contains("refs/heads/unborn")
                );
            }
            "sparse" => {
                git(
                    &fixture.root,
                    &["sparse-checkout", "init", "--cone", "--sparse-index"],
                );
                git(&fixture.root, &["sparse-checkout", "set", "included"]);
                assert!(
                    git_text(&fixture.root, &["ls-files", "--sparse", "--stage"])
                        .contains("040000")
                );
            }
            "conflict" => {
                let branch = git_text(&fixture.root, &["branch", "--show-current"]);
                git(&fixture.root, &["checkout", "-b", "other"]);
                std::fs::write(fixture.root.join("tracked"), "theirs\n").unwrap();
                git(&fixture.root, &["commit", "-qam", "other tree"]);
                git(&fixture.root, &["checkout", branch.trim()]);
                std::fs::write(fixture.root.join("tracked"), "ours\n").unwrap();
                git(&fixture.root, &["commit", "-qam", "our tree"]);
                let merged = std::process::Command::new("git")
                    .args(["merge", "--no-edit", "other"])
                    .current_dir(&fixture.root)
                    .output()
                    .unwrap();
                assert_eq!(merged.status.code(), Some(1));
                let staged = git_text(&fixture.root, &["ls-files", "--stage"]);
                assert!(
                    staged.contains(" 1\ttracked")
                        && staged.contains(" 2\ttracked")
                        && staged.contains(" 3\ttracked")
                );
            }
            _ => unreachable!(),
        }
        let original_index = git_text(&fixture.root, &["ls-files", "--stage", "-z"]);
        let original_refs = git_text(
            &fixture.root,
            &["for-each-ref", "--format=%(objectname) %(refname)"],
        );
        let started = fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                fixture.spec(false),
            )
            .unwrap();
        let capture = capture(&fixture, started.operation);
        let refreshed = fixture
            .service
            .refresh_closeout(started.operation, started.revision, &capture)
            .await
            .unwrap_or_else(|error| panic!("{case}: {error:?}"));
        let stored = load(&fixture.service.connection().unwrap(), refreshed.operation).unwrap();
        let snapshots: Vec<git::RepositorySnapshot> =
            storage::read_json(stored.history.as_ref().unwrap()).unwrap();
        if case == "orphan" {
            assert!(snapshots[0].head.is_none());
        }
        let archive_path = fixture._directory.path().join("preservation");
        std::fs::create_dir(&archive_path).unwrap();
        let archive = archive::Archive::open(&archive_path).unwrap();
        let bundle = git::preserve(
            &fixture.service,
            refreshed.operation,
            &snapshots[0],
            &archive_path,
            &capture,
            &archive,
        )
        .await
        .unwrap_or_else(|error| panic!("{case}: {error:?}"));
        assert_eq!(
            git_text(
                &fixture.root,
                &["for-each-ref", "--format=%(objectname) %(refname)"]
            ),
            original_refs
        );
        std::fs::rename(
            &fixture.root,
            fixture._directory.path().join("source-offline"),
        )
        .unwrap();
        let restored = bundle.parent().unwrap().join("restored.git");
        assert_eq!(
            git_text(&restored, &["ls-files", "--stage", "-z"]),
            original_index,
            "{case}"
        );
        git(&restored, &["fsck", "--full", "--no-reflogs"]);
        if case == "conflict" {
            assert!(
                bundle
                    .parent()
                    .unwrap()
                    .join("administration/files/MERGE_HEAD")
                    .is_file()
            );
        }
        finish_capture(&capture);
    }
}
