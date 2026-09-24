//! Catalog-only reference changes must not revive resolved findings on recovery.
use super::*;

#[tokio::test]
async fn resolved_nested_reference_stays_resolved_through_quarantine_recovery() {
    let fixture = Fixture::new();
    let nested = fixture.root.join("documents");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    git(
        &nested,
        &["commit", "--allow-empty", "-qm", "nested fixture"],
    );
    std::fs::write(
        nested.join("valuable.md"),
        "retained directory-reference information",
    )
    .unwrap();
    let Entity::Location(parent) = fixture
        .service
        .inspect(EntityId::Location(fixture.location))
        .unwrap()
    else {
        panic!()
    };
    let LocationKind::Checkout { repository, .. } = parent.kind else {
        panic!()
    };
    let EntityId::Location(directory) = change(
        &fixture.service,
        OrganizationChange::RegisterLocation {
            name: "nested-reference".into(),
            path: nested.clone(),
            registration: Registration::Checkout {
                home: parent.home.unwrap(),
                repository,
            },
        },
    ) else {
        panic!()
    };
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
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let current = fixture
        .service
        .refresh_closeout_in(started.operation, started.revision, &sessions, &capture)
        .await
        .unwrap();
    let preserved = fixture
        .service
        .preserve_closeout(current.operation, current.revision, &capture)
        .await
        .unwrap();
    let blocked = fixture
        .service
        .review_closeout_removal(preserved.operation, preserved.revision, &runtime)
        .await
        .unwrap();
    assert!(
        blocked
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::Referenced)
    );
    let stored = load(&fixture.service.connection().unwrap(), started.operation).unwrap();
    let original_reference = stored.references.clone().unwrap();
    let original_bytes = std::fs::read(&original_reference.0).unwrap();
    let mut nested_spec = fixture.spec(false);
    nested_spec.location = directory;
    let nested_record = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            nested_spec,
        )
        .unwrap();
    let nested_capture = super::capture(&fixture, nested_record.operation);
    let nested_runtime = CloseoutRuntime::new(
        &sessions,
        &execution,
        fixture._directory.path(),
        &nested_capture,
    );
    let retained = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            nested_record.operation,
            nested_record.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &nested_runtime,
        )
        .await
        .unwrap();
    assert!(retained.issues.is_empty(), "{:?}", retained.issues);
    fixture
        .service
        .apply_closeout_recovery(
            &fixture.client,
            RequestId::new(),
            CloseoutReviewTarget {
                operation: nested_record.operation,
                review: retained.id,
            },
            &nested_runtime,
        )
        .await
        .unwrap();
    finish_capture(&nested_capture);
    assert!(nested.join("valuable.md").is_file());
    let review = fixture
        .service
        .review_closeout_removal(started.operation, blocked.revision, &runtime)
        .await
        .unwrap();
    assert!(
        review.issues.is_empty(),
        "Resolved current references remain blocked: {:?}",
        review.issues
    );
    fixture
        .service
        .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
        .await
        .unwrap();
    let mut interrupted = fixture.service.clone();
    interrupted.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_quarantine_renamed" {
            Err(io("fixture interruption"))
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
    assert!(!fixture.root.exists());
    let partial = fixture.service.inspect_closeout(started.operation).unwrap();
    let recovery = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            started.operation,
            partial.revision,
            CloseoutRecoveryAction::ResumeRemoval,
            &runtime,
        )
        .await
        .unwrap();
    assert!(
        recovery.issues.is_empty(),
        "Historical reference findings returned: {:?}",
        recovery.issues
    );
    fixture
        .service
        .apply_closeout_recovery(
            &fixture.client,
            RequestId::new(),
            CloseoutReviewTarget {
                operation: started.operation,
                review: recovery.id,
            },
            &runtime,
        )
        .await
        .unwrap();
    let closed = fixture
        .service
        .finish_closeout(started.operation, &runtime)
        .await
        .unwrap();
    assert_eq!(closed.stage, CloseoutStage::Closed);
    assert_eq!(
        std::fs::read(&original_reference.0).unwrap(),
        original_bytes,
        "Captured history must not be rewritten"
    );
    let capture_root = std::fs::read_dir(&closed.preservation_directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.join("verified-restore/documents/valuable.md")
                .is_file()
        })
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(capture_root.join("verified-restore/documents/valuable.md"))
            .unwrap(),
        "retained directory-reference information"
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn removal_reference_snapshot_corruption_and_legacy_absence_fail_closed() {
    for scenario in 0..3 {
        let legacy = scenario != 0;
        let original_intact = scenario == 2;
        let (fixture, record, capture) = prepared_approval_fixture(false, false).await;
        let sessions = fixture._directory.path().join("sessions-state");
        let execution =
            crate::execution::ExecutionStore::open(&fixture._directory.path().join("output"))
                .unwrap();
        let runtime =
            CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
        let review = fixture
            .service
            .review_closeout_removal(record.operation, record.revision, &runtime)
            .await
            .unwrap();
        fixture
            .service
            .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
            .await
            .unwrap();
        let mut interrupted = fixture.service.clone();
        interrupted.fault = Some(std::sync::Arc::new(move |stage| {
            if stage
                == if legacy && !original_intact {
                    "closeout_quarantine_renamed"
                } else {
                    "closeout_removal_intent"
                }
            {
                Err(io("fixture interruption"))
            } else {
                Ok(())
            }
        }));
        assert!(
            interrupted
                .finish_closeout(record.operation, &runtime)
                .await
                .is_err()
        );
        let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
        if legacy {
            // Exact previous persisted shape, not a public authority mutation.
            let mut old = serde_json::to_value(&stored).unwrap();
            old["removal"]
                .as_object_mut()
                .unwrap()
                .remove("reference_snapshot");
            fixture
                .service
                .connection()
                .unwrap()
                .execute(
                    "UPDATE operations SET body=?1 WHERE id=?2",
                    rusqlite::params![old.to_string(), record.operation.to_string()],
                )
                .unwrap();
            if original_intact {
                let (path, _) = stored
                    .removal
                    .as_ref()
                    .unwrap()
                    .reference_snapshot()
                    .unwrap();
                std::fs::remove_file(path).unwrap();
                assert!(fixture.root.join("payload").is_file());
                assert_eq!(
                    fixture
                        .service
                        .finish_closeout(record.operation, &runtime)
                        .await
                        .unwrap()
                        .stage,
                    CloseoutStage::Closed
                );
                let refreshed =
                    load(&fixture.service.connection().unwrap(), record.operation).unwrap();
                let (path, digest) = refreshed
                    .removal
                    .as_ref()
                    .unwrap()
                    .reference_snapshot()
                    .unwrap();
                assert_eq!(backup::file_digest(path).unwrap(), *digest);
                finish_capture(&capture);
                continue;
            }
            let blocked = fixture
                .service
                .review_closeout_recovery(
                    &fixture.client,
                    record.operation,
                    stored.record.revision,
                    CloseoutRecoveryAction::ResumeRemoval,
                    &runtime,
                )
                .await
                .unwrap();
            assert!(
                blocked
                    .issues
                    .iter()
                    .any(|issue| issue.code == IssueCode::IncompleteCapture)
            );
            let retained = fixture
                .service
                .review_closeout_recovery(
                    &fixture.client,
                    record.operation,
                    blocked.revision,
                    CloseoutRecoveryAction::UnregisterRetainFiles,
                    &runtime,
                )
                .await
                .unwrap();
            assert!(retained.issues.is_empty(), "{:?}", retained.issues);
            let quarantine = retained
                .paths
                .iter()
                .find(|path| path.present && path.matches_recorded_root)
                .unwrap()
                .path
                .clone();
            let outcome = fixture
                .service
                .apply_closeout_recovery(
                    &fixture.client,
                    RequestId::new(),
                    CloseoutReviewTarget {
                        operation: record.operation,
                        review: retained.id,
                    },
                    &runtime,
                )
                .await
                .unwrap();
            assert_eq!(outcome.stage, CloseoutStage::Retained);
            assert_eq!(
                std::fs::read_to_string(quarantine.join("payload")).unwrap(),
                "preserved data"
            );
            assert!(
                fixture
                    .service
                    .closed_checkout_history(fixture.location)
                    .unwrap()
                    .preservation_paths
                    .contains(&quarantine)
            );
        } else {
            let (path, _) = stored
                .removal
                .as_ref()
                .unwrap()
                .reference_snapshot()
                .unwrap();
            let bytes = std::fs::read(path).unwrap();
            std::fs::write(path, "damaged reference evidence").unwrap();
            assert_eq!(
                fixture
                    .service
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .unwrap_err()
                    .code,
                IssueCode::CorruptState
            );
            assert_eq!(
                std::fs::read_to_string(fixture.root.join("payload")).unwrap(),
                "preserved data"
            );
            std::fs::write(path, bytes).unwrap();
            assert_eq!(
                fixture
                    .service
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .unwrap()
                    .stage,
                CloseoutStage::Closed
            );
        }
        finish_capture(&capture);
    }
}
