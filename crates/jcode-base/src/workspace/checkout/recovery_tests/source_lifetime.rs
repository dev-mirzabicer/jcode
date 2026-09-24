use super::*;

async fn close_source(fixture: &Fixture, record: &CloseoutRecord) {
    let execution = ExecutionStore::open(fixture.temp.path()).unwrap();
    let capture = fixture.capture(RequestId::new());
    fixture
        .service
        .prepare_closeout_work(
            record.operation,
            record.revision,
            &fixture.temp.path().join("sessions-state"),
            &execution,
            fixture.temp.path(),
            &capture,
        )
        .await
        .unwrap();
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
    capture
        .seal(output, crate::execution::RunState::Completed)
        .unwrap();
}

fn register_source(fixture: &Fixture, path: &Path) -> LocationId {
    let EntityId::Location(id) = organization(
        &fixture.service,
        OrganizationChange::RegisterLocation {
            name: "read source".into(),
            path: path.to_path_buf(),
            registration: Registration::Checkout {
                home: Home::Project(fixture.project),
                repository: fixture.repository,
            },
        },
    ) else {
        panic!()
    };
    id
}
fn begin_closeout(fixture: &Fixture, location: LocationId) -> CloseoutRecord {
    fixture
        .service
        .begin_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            fixture.service.status().unwrap().revision,
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
fn add_child(fixture: &Fixture) -> PathBuf {
    let child = fixture.temp.path().join("child-source");
    std::fs::create_dir(&child).unwrap();
    git(&child, &["init", "-b", "main"]);
    std::fs::write(child.join("child-data"), "submodule committed bytes").unwrap();
    git(&child, &["add", "."]);
    git(
        &child,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qm",
            "child",
        ],
    );
    git(
        &fixture.source,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "child=one",
            "../child-source",
            "libs/child",
        ],
    );
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qam",
            "parent",
        ],
    );
    child
}

#[tokio::test]
async fn materialization_submodule_closing_source_blocks_then_retries_under_a_live_lease() {
    let fixture = Fixture::new();
    let child = add_child(&fixture);
    let location = register_source(&fixture, &child);
    let closeout = begin_closeout(&fixture, location);
    close_source(&fixture, &closeout).await;
    let mut spec = fixture.spec();
    spec.submodules = true;
    spec.trusted_submodule_urls = vec!["../child-source".into()];
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, spec)
        .unwrap();
    let request = RequestId::new();
    fixture.service.begin_clone(request, review.id).unwrap();
    let capture = fixture.capture(request);
    assert_eq!(
        fixture
            .service
            .execute_clone(request, &capture)
            .await
            .unwrap_err()
            .code,
        IssueCode::LiveWork
    );
    assert!(!fixture.destination.exists());
    assert_eq!(
        std::fs::read_to_string(child.join("child-data")).unwrap(),
        "submodule committed bytes"
    );
    let current = fixture
        .service
        .inspect_closeout(closeout.operation)
        .unwrap();
    fixture
        .service
        .revoke_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            closeout.operation,
            current.revision,
        )
        .unwrap();
    let binding = fixture.service.resolver.bind_directory(&child).unwrap();
    let service = fixture.service.clone();
    let seen = Arc::new(AtomicBool::new(false));
    let mut executing = fixture.service.clone();
    executing.fault = Some(Arc::new({
        let seen = seen.clone();
        move |at| {
            if at == "clone_submodule_source_admitted" {
                assert!(matches!(
                    service.acquire_binding(&binding),
                    Err(Issue {
                        code: IssueCode::Busy,
                        ..
                    })
                ));
                seen.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }));
    assert_eq!(
        executing
            .execute_clone(request, &capture)
            .await
            .unwrap()
            .state,
        CloneState::Ready
    );
    assert!(seen.load(Ordering::SeqCst));
    assert_eq!(
        std::fs::read_to_string(fixture.destination.join("libs/child/child-data")).unwrap(),
        "submodule committed bytes"
    );
}

#[tokio::test]
async fn materialization_lfs_source_closing_after_acquisition_blocks_until_explicit_repair() {
    let fixture = Fixture::new();
    git(&fixture.source, &["lfs", "install", "--local"]);
    git(&fixture.source, &["lfs", "track", "*.bin"]);
    std::fs::write(fixture.source.join("payload.bin"), "local LFS unique bytes").unwrap();
    git(&fixture.source, &["add", "."]);
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qm",
            "LFS",
        ],
    );
    let location = register_source(&fixture, &fixture.source);
    let closeout = begin_closeout(&fixture, location);
    let mut spec = fixture.spec();
    spec.lfs = true;
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, spec)
        .unwrap();
    let request = RequestId::new();
    fixture.service.begin_clone(request, review.id).unwrap();
    let capture = fixture.capture(request);
    let once = Arc::new(AtomicBool::new(false));
    let mut executing = fixture.service.clone();
    executing.fault = Some(Arc::new(move |at| {
        if at == "clone_acquired" && !once.swap(true, Ordering::SeqCst) {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Fixture pause after acquisition",
            ));
        }
        Ok(())
    }));
    assert_eq!(
        executing
            .execute_clone(request, &capture)
            .await
            .unwrap_err()
            .code,
        IssueCode::RecoveryRequired
    );
    close_source(&fixture, &closeout).await;
    assert_eq!(
        executing
            .execute_clone(request, &capture)
            .await
            .unwrap_err()
            .code,
        IssueCode::LiveWork
    );
    assert!(!fixture.destination.exists());
    let current = fixture
        .service
        .inspect_closeout(closeout.operation)
        .unwrap();
    fixture
        .service
        .revoke_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            closeout.operation,
            current.revision,
        )
        .unwrap();
    let binding = fixture
        .service
        .resolver
        .bind_directory(&fixture.source)
        .unwrap();
    let service = fixture.service.clone();
    let seen = Arc::new(AtomicBool::new(false));
    executing.fault = Some(Arc::new({
        let seen = seen.clone();
        move |at| {
            if at == "clone_lfs_source_admitted" {
                assert!(matches!(
                    service.acquire_binding(&binding),
                    Err(Issue {
                        code: IssueCode::Busy,
                        ..
                    })
                ));
                seen.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }));
    assert_eq!(
        executing
            .execute_clone(request, &capture)
            .await
            .unwrap()
            .state,
        CloneState::Ready
    );
    assert!(seen.load(Ordering::SeqCst));
    assert_eq!(
        std::fs::read_to_string(fixture.destination.join("payload.bin")).unwrap(),
        "local LFS unique bytes"
    );
}

#[tokio::test]
async fn materialization_redirected_submodule_requires_exact_new_source_review() {
    let fixture = Fixture::new();
    let child = add_child(&fixture);
    let redirected = fixture.temp.path().join("redirected.git");
    git(
        fixture.temp.path(),
        &[
            "clone",
            "--bare",
            child.to_str().unwrap(),
            redirected.to_str().unwrap(),
        ],
    );
    let mut spec = fixture.spec();
    spec.submodules = true;
    spec.trusted_submodule_urls = vec!["../child-source".into()];
    let review = fixture
        .service
        .review_clone(fixture.service.status().unwrap().revision, spec)
        .unwrap();
    let request = RequestId::new();
    fixture.service.begin_clone(request, review.id).unwrap();
    let capture = fixture.capture(request);
    let service = fixture.service.clone();
    let target = redirected.clone();
    let mut executing = fixture.service.clone();
    executing.fault = Some(Arc::new(move |at| {
        if at == "clone_acquired" {
            let stage = service.inspect_clone(request)?.stage.unwrap();
            git(
                &stage,
                &[
                    "config",
                    "submodule.child=one.url",
                    target.to_str().unwrap(),
                ],
            );
        }
        Ok(())
    }));
    assert_eq!(
        executing
            .execute_clone(request, &capture)
            .await
            .unwrap_err()
            .code,
        IssueCode::PermissionRequired
    );
    let paused = fixture.service.inspect_clone(request).unwrap();
    assert_eq!(paused.pending_trust[0].url, redirected.to_str().unwrap());
    assert!(
        !paused
            .stage
            .as_ref()
            .unwrap()
            .join("libs/child/child-data")
            .exists()
    );
    let review = fixture
        .service
        .review_clone_trust(request, paused.revision)
        .unwrap();
    fixture
        .service
        .apply_clone_trust(
            RequestId::new(),
            review.id,
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .execute_clone(request, &capture)
            .await
            .unwrap()
            .state,
        CloneState::Ready
    );
    assert_eq!(
        std::fs::read_to_string(fixture.destination.join("libs/child/child-data")).unwrap(),
        "submodule committed bytes"
    );
}

#[tokio::test]
async fn materialization_lfs_external_cache_is_part_of_source_lifetime() {
    let fixture = Fixture::new();
    let cache = add_child(&fixture);
    git(
        &fixture.source,
        &["config", "lfs.storage", cache.to_str().unwrap()],
    );
    std::fs::create_dir(cache.join("objects")).unwrap();
    let location = register_source(&fixture, &cache);
    let closeout = begin_closeout(&fixture, location);
    close_source(&fixture, &closeout).await;
    let endpoint = url::Url::from_directory_path(fixture.source.join(".git"))
        .unwrap()
        .to_string();
    assert!(matches!(
        fixture
            .service
            .acquire_lfs_source(&fixture.source, &endpoint),
        Err(Issue {
            code: IssueCode::LiveWork,
            ..
        })
    ));
    let current = fixture
        .service
        .inspect_closeout(closeout.operation)
        .unwrap();
    fixture
        .service
        .revoke_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            closeout.operation,
            current.revision,
        )
        .unwrap();
    let leases = fixture
        .service
        .acquire_lfs_source(&fixture.source, &endpoint)
        .unwrap();
    let binding = fixture.service.resolver.bind_directory(&cache).unwrap();
    assert!(matches!(
        fixture.service.acquire_binding(&binding),
        Err(Issue {
            code: IssueCode::Busy,
            ..
        })
    ));
    drop(leases);
    fixture.service.acquire_binding(&binding).unwrap();
    assert_eq!(
        std::fs::read_to_string(cache.join("child-data")).unwrap(),
        "submodule committed bytes"
    );
}

#[test]
fn materialization_relative_source_inspection_is_read_only_and_origin_bound() {
    let fixture = Fixture::new();
    add_child(&fixture);
    git(
        &fixture.source,
        &[
            "remote",
            "add",
            "origin",
            fixture.source.canonicalize().unwrap().to_str().unwrap(),
        ],
    );
    let config = std::fs::read(fixture.source.join(".git/config")).unwrap();
    let sources = materialize::submodule_sources(&fixture.source, &fixture.source).unwrap();
    assert_eq!(sources[0].url, "../child-source");
    assert_eq!(
        std::fs::read(fixture.source.join(".git/config")).unwrap(),
        config
    );
    git(
        &fixture.source,
        &[
            "remote",
            "add",
            "unreviewed",
            "https://fixture.invalid/other/source",
        ],
    );
    git(
        &fixture.source,
        &["config", "branch.main.remote", "unreviewed"],
    );
    assert_eq!(
        materialize::submodule_sources(&fixture.source, &fixture.source)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
}

#[tokio::test]
async fn materialization_recovery_reuses_verified_local_objects_without_original_sources() {
    for lfs in [false, true] {
        let fixture = Fixture::new();
        let child = if lfs {
            git(&fixture.source, &["lfs", "install", "--local"]);
            git(&fixture.source, &["lfs", "track", "*.bin"]);
            std::fs::write(
                fixture.source.join("payload.bin"),
                "offline materialized payload",
            )
            .unwrap();
            git(&fixture.source, &["add", "."]);
            git(
                &fixture.source,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@localhost",
                    "commit",
                    "-qm",
                    "payload",
                ],
            );
            None
        } else {
            Some(add_child(&fixture))
        };
        let mut spec = fixture.spec();
        spec.lfs = lfs;
        spec.submodules = !lfs;
        spec.trusted_submodule_urls = vec!["../child-source".into()];
        let review = fixture
            .service
            .review_clone(fixture.service.status().unwrap().revision, spec)
            .unwrap();
        let request = RequestId::new();
        fixture.service.begin_clone(request, review.id).unwrap();
        let capture = fixture.capture(request);
        let mut interrupted = fixture.service.clone();
        interrupted.fault = Some(Arc::new(move |at| {
            if at
                == if lfs {
                    "clone_lfs_fetched"
                } else {
                    "clone_submodule_materialized"
                }
            {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Fixture pause after independent acquisition",
                ));
            }
            Ok(())
        }));
        assert_eq!(
            interrupted
                .execute_clone(request, &capture)
                .await
                .unwrap_err()
                .code,
            IssueCode::RecoveryRequired
        );
        std::fs::rename(&fixture.source, fixture.temp.path().join("source-offline")).unwrap();
        if let Some(child) = child {
            std::fs::rename(child, fixture.temp.path().join("child-offline")).unwrap();
        }
        let ready = fixture
            .service
            .execute_clone(request, &capture)
            .await
            .unwrap();
        assert_eq!(ready.state, CloneState::Ready);
        let (path, bytes) = if lfs {
            ("payload.bin", "offline materialized payload")
        } else {
            ("libs/child/child-data", "submodule committed bytes")
        };
        assert_eq!(
            std::fs::read_to_string(fixture.destination.join(path)).unwrap(),
            bytes
        );
        git(&fixture.destination, &["fsck", "--full"]);
    }
}
