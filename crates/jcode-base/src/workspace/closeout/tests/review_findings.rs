//! Reproductions of the independent read-only safety review. Owned fixtures only.
use super::*;

async fn approved(fixture: &Fixture) -> (CloseoutRecord, crate::execution::Capture) {
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
    let capture = capture(fixture, started.operation);
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let record = fixture
        .service
        .refresh_closeout_in(started.operation, started.revision, &sessions, &capture)
        .await
        .unwrap();
    let record = fixture
        .service
        .preserve_closeout(record.operation, record.revision, &capture)
        .await
        .unwrap();
    let review = fixture
        .service
        .review_closeout_removal(record.operation, record.revision, &runtime)
        .await
        .unwrap();
    let record = fixture
        .service
        .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
        .await
        .unwrap();
    (record, capture)
}
fn set_attribute(path: &Path, value: &str) {
    assert!(
        std::process::Command::new("xattr")
            .args(["-w", "jcode.review-fixture", value])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}
fn interrupted(service: &WorkspaceService, boundary: &'static str) -> WorkspaceService {
    let mut result = service.clone();
    result.fault = Some(std::sync::Arc::new(move |stage| {
        if stage == boundary {
            Err(io("independent review fixture interruption"))
        } else {
            Ok(())
        }
    }));
    result
}

#[tokio::test]
async fn review_finding_f1_captured_metadata_change_cannot_be_deleted_under_old_approval() {
    metadata_replay_case("directory").await;
}
#[tokio::test]
async fn review_finding_f1_captured_regular_file_metadata_is_not_old_preservation() {
    metadata_replay_case("file").await;
}
#[tokio::test]
async fn review_finding_f1_hardlink_metadata_is_not_excused_by_own_link_count_changes() {
    metadata_replay_case("hardlink").await;
}
async fn metadata_replay_case(kind: &str) {
    let fixture = Fixture::new();
    let directory = fixture.root.join("zz-metadata");
    if kind == "directory" {
        std::fs::create_dir(&directory).unwrap();
    } else {
        std::fs::write(&directory, "metadata-bearing information").unwrap();
    }
    if kind == "hardlink" {
        std::fs::hard_link(&directory, fixture.root.join("aa-alias")).unwrap();
    }
    set_attribute(&directory, "before");
    let (record, capture) = approved(&fixture).await;
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    assert!(
        interrupted(&fixture.service, "closeout_entry_captured")
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    assert_eq!(
        serde_json::to_value(&stored).unwrap()["removal"]["pending"]["item"]["entry"]["path"],
        "zz-metadata"
    );
    let slot = stored.removal.as_ref().unwrap().control_paths()[1].join("entry");
    set_attribute(&slot, "after-new-information");
    let outcome = fixture
        .service
        .finish_closeout(record.operation, &runtime)
        .await;
    assert_eq!(outcome.unwrap_err().code, IssueCode::Conflict);
    assert!(slot.exists());
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let resume = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::ResumeRemoval,
            &runtime,
        )
        .await
        .unwrap();
    assert!(
        resume
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::Conflict),
        "{resume:?}"
    );
    assert!(
        fixture
            .service
            .apply_closeout_recovery(
                &fixture.client,
                RequestId::new(),
                CloseoutReviewTarget {
                    operation: record.operation,
                    review: resume.id
                },
                &runtime
            )
            .await
            .is_err()
    );
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let retain = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await
        .unwrap();
    assert!(retain.issues.is_empty(), "{retain:?}");
    assert_eq!(
        fixture
            .service
            .apply_closeout_recovery(
                &fixture.client,
                RequestId::new(),
                CloseoutReviewTarget {
                    operation: record.operation,
                    review: retain.id
                },
                &runtime
            )
            .await
            .unwrap()
            .stage,
        CloseoutStage::Retained
    );
    let actual = std::process::Command::new("xattr")
        .args(["-p", "jcode.review-fixture"])
        .arg(&slot)
        .output()
        .unwrap();
    assert!(actual.status.success());
    assert_eq!(
        String::from_utf8(actual.stdout).unwrap().trim(),
        "after-new-information"
    );
    let mut legacy = serde_json::to_value(&stored).unwrap()["removal"]["pending"]["item"].clone();
    legacy.as_object_mut().unwrap().remove("removal_metadata");
    let legacy: inventory::Item = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        inventory::verify_removal_metadata(&legacy, &slot)
            .unwrap_err()
            .code,
        IssueCode::IncompleteCapture
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn review_finding_f2_quarantine_cannot_acquire_an_unresolved_independent_checkout() {
    let fixture = Fixture::new();
    let nested = fixture.root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    std::fs::write(nested.join("valuable"), "independent information").unwrap();
    git(&nested, &["add", "valuable"]);
    git(
        &nested,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "nested",
        ],
    );
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
    let prior = fixture
        .service
        .review_organization_change(
            fixture.service.status().unwrap().revision,
            OrganizationChange::RegisterLocation {
                name: "prior-review".into(),
                path: nested.clone(),
                registration: Registration::Checkout {
                    home: parent.home.unwrap(),
                    repository,
                },
            },
        )
        .unwrap();
    let (record, capture) = approved(&fixture).await;
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    assert!(
        interrupted(&fixture.service, "closeout_quarantine_renamed")
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .apply_organization_change(RequestId::new(), prior.id)
            .unwrap_err()
            .code,
        IssueCode::LiveWork
    );
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let nested = stored.removal.as_ref().unwrap().control_paths()[0].join("nested");
    let review = fixture.service.review_organization_change(
        fixture.service.status().unwrap().revision,
        OrganizationChange::RegisterLocation {
            name: "new-independent".into(),
            path: nested.clone(),
            registration: Registration::Checkout {
                home: parent.home.unwrap(),
                repository,
            },
        },
    );
    assert_eq!(review.unwrap_err().code, IssueCode::LiveWork);
    assert!(nested.join("valuable").is_file());
    finish_capture(&capture);
}

#[tokio::test]
async fn review_finding_f3_damaged_reference_evidence_still_allows_truthful_file_retention() {
    damaged_evidence_retention(false).await;
}
#[tokio::test]
async fn review_finding_f3_missing_reference_evidence_still_allows_truthful_file_retention() {
    damaged_evidence_retention(true).await;
}
async fn damaged_evidence_retention(missing: bool) {
    let (fixture, record, capture) = prepared_approval_fixture(false, false).await;
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
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
    assert!(
        interrupted(&fixture.service, "closeout_quarantine_renamed")
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let root = stored.removal.as_ref().unwrap().control_paths()[0].to_path_buf();
    let (path, _) = stored
        .removal
        .as_ref()
        .unwrap()
        .reference_snapshot()
        .unwrap();
    if missing {
        std::fs::remove_file(path).unwrap();
    } else {
        std::fs::write(path, "lost reference evidence").unwrap();
    }
    assert!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let resume = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::ResumeRemoval,
            &runtime,
        )
        .await
        .unwrap();
    assert!(!resume.issues.is_empty());
    assert!(resume.evidence_issues.is_empty());
    assert!(
        fixture
            .service
            .apply_closeout_recovery(
                &fixture.client,
                RequestId::new(),
                CloseoutReviewTarget {
                    operation: record.operation,
                    review: resume.id
                },
                &runtime
            )
            .await
            .is_err()
    );
    let held = std::fs::File::open(root.join("payload")).unwrap();
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let live = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await
        .unwrap();
    assert!(
        live.issues
            .iter()
            .any(|issue| issue.code == IssueCode::LiveWork),
        "{live:?}"
    );
    drop(held);
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let review = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await;
    assert!(
        review.is_ok(),
        "Non-destructive retention is stranded: {review:?}"
    );
    let review = review.unwrap();
    assert!(review.issues.is_empty(), "{review:?}");
    assert!(!review.evidence_issues.is_empty());
    std::fs::write(path, "changed damaged evidence").unwrap();
    assert_eq!(
        fixture
            .service
            .apply_closeout_recovery(
                &fixture.client,
                RequestId::new(),
                CloseoutReviewTarget {
                    operation: record.operation,
                    review: review.id
                },
                &runtime
            )
            .await
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    if missing {
        std::fs::remove_file(path).unwrap();
    }
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let review = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            current.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await
        .unwrap();
    assert!(review.issues.is_empty());
    let outcome = fixture
        .service
        .apply_closeout_recovery(
            &fixture.client,
            RequestId::new(),
            CloseoutReviewTarget {
                operation: record.operation,
                review: review.id,
            },
            &runtime,
        )
        .await
        .unwrap();
    assert_eq!(outcome.stage, CloseoutStage::Retained);
    assert!(outcome.issues.iter().any(|issue| issue.code
        == if missing {
            IssueCode::Io
        } else {
            IssueCode::CorruptState
        }));
    let report: serde_json::Value = storage::read_json(
        &fixture
            .service
            .closed_checkout_history(fixture.location)
            .unwrap()
            .report
            .unwrap(),
    )
    .unwrap();
    if missing {
        assert!(!path.exists());
        assert!(report["unavailable_reference_evidence"]["observed_digest"].is_null());
        assert!(report["unavailable_reference_evidence"]["witness"].is_null());
    } else {
        assert_eq!(
            report["unavailable_reference_evidence"]["observed_digest"],
            digest(b"changed damaged evidence")
        );
    }
    assert_eq!(report["no_filesystem_removal_performed"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("payload")).unwrap(),
        "preserved data"
    );
    finish_capture(&capture);
}

#[test]
fn review_finding_f2_new_link_under_quarantine_needs_current_preservation() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fixture = Fixture::new();
            std::fs::write(
                fixture.root.join("discard-me.md"),
                "data now used by a linked page",
            )
            .unwrap();
            let started = fixture.begin(false);
            let capture = capture(&fixture, started.operation);
            let home = crate::storage::jcode_dir().unwrap();
            let execution =
                crate::execution::ExecutionStore::open(&fixture._directory.path().join("output"))
                    .unwrap();
            let runtime =
                CloseoutRuntime::new(&home, &execution, fixture._directory.path(), &capture);
            let mut record = fixture
                .service
                .refresh_closeout_in(started.operation, started.revision, &home, &capture)
                .await
                .unwrap();
            let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
            let mut decisions = Vec::new();
            inventory::visit(&stored, |item| {
                if item.witness.is_some() && item.entry.kind != CloseoutEntryKind::Directory {
                    decisions.push((
                        item.entry.id,
                        if item.entry.path == Path::new("discard-me.md") {
                            CloseoutDisposition::Redundant {
                                reason: "synthetic generated data decision before any link".into(),
                            }
                        } else {
                            CloseoutDisposition::Preserve
                        },
                    ));
                }
                Ok(())
            })
            .unwrap();
            for (entry, disposition) in decisions {
                record = fixture
                    .service
                    .record_closeout_disposition(
                        record.operation,
                        record.revision,
                        CloseoutDecision {
                            entry,
                            disposition,
                            recorded_by: "fixture decision".into(),
                        },
                    )
                    .unwrap();
            }
            record = fixture
                .service
                .preserve_closeout(record.operation, record.revision, &capture)
                .await
                .unwrap();
            let review = fixture
                .service
                .review_closeout_removal(record.operation, record.revision, &runtime)
                .await
                .unwrap();
            fixture
                .service
                .approve_closeout_removal(
                    &fixture.client,
                    RequestId::new(),
                    review.target(),
                    &runtime,
                )
                .await
                .unwrap();
            assert!(
                interrupted(&fixture.service, "closeout_quarantine_renamed")
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .is_err()
            );
            let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
            let linked = stored.removal.as_ref().unwrap().control_paths()[0].join("discard-me.md");
            let mut session = crate::session::Session::create(None, None);
            session.working_dir = Some(
                fixture
                    ._directory
                    .path()
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string(),
            );
            session.save().unwrap();
            crate::side_panel::load_markdown_file(
                &session.id,
                "linked-after-quarantine",
                None,
                &linked,
                false,
            )
            .unwrap();
            let before = fixture
                .service
                .inspect_closeout(record.operation)
                .unwrap()
                .removed_entries;
            assert_eq!(
                fixture
                    .service
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .unwrap_err()
                    .code,
                IssueCode::PreservationIncomplete
            );
            assert_eq!(
                fixture
                    .service
                    .inspect_closeout(record.operation)
                    .unwrap()
                    .removed_entries,
                before
            );
            assert_eq!(
                std::fs::read_to_string(&linked).unwrap(),
                "data now used by a linked page"
            );
            let current = fixture.service.inspect_closeout(record.operation).unwrap();
            let recovery = fixture
                .service
                .review_closeout_recovery(
                    &fixture.client,
                    record.operation,
                    current.revision,
                    CloseoutRecoveryAction::ResumeRemoval,
                    &runtime,
                )
                .await
                .unwrap();
            assert!(
                recovery
                    .issues
                    .iter()
                    .any(|issue| issue.code == IssueCode::PreservationIncomplete),
                "{recovery:?}"
            );
            finish_capture(&capture);
        });
}
