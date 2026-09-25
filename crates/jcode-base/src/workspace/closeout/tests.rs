use super::*;
mod combined_disposal;
mod git_edges;
mod native_paths;
mod reference_freshness;
mod review_findings;
mod work_admission;

struct Fixture {
    _directory: tempfile::TempDir,
    service: WorkspaceService,
    root: PathBuf,
    location: LocationId,
    client: WorkspaceClientAuthority,
}

#[tokio::test]
async fn trusted_recovery_does_not_invent_effects_missing_from_an_old_snapshot() {
    let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
        .declare_closeout_no_loss(
            "fixture",
            RequestId::new(),
            review.target(),
            "synthetic assessment",
            &runtime,
        )
        .await
        .unwrap();
    let old = fixture
        .service
        .backup(RequestId::new(), "before-effects".into())
        .unwrap();
    let mut interrupted = fixture.service.clone();
    interrupted.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_entry_unlinked" {
            Err(issue(IssueCode::Io, "fixture interruption"))
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
    let quarantine = fixture
        .service
        .inspect_closeout(record.operation)
        .unwrap()
        .quarantine
        .unwrap();
    let retained_file = quarantine.join(".git/HEAD");
    let bytes = std::fs::read(&retained_file).unwrap();
    let restore = fixture.service.review_restore(old.id).unwrap();
    fixture
        .service
        .apply_restore(RequestId::new(), restore.id)
        .unwrap();
    let restored = fixture.service.inspect_closeout(record.operation).unwrap();
    let unsafe_resume = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            restored.operation,
            restored.revision,
            CloseoutRecoveryAction::ResumeRemoval,
            &runtime,
        )
        .await
        .unwrap();
    assert!(
        unsafe_resume
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::RecoveryRequired)
    );
    assert!(
        fixture
            .service
            .apply_closeout_recovery(
                &fixture.client,
                RequestId::new(),
                CloseoutReviewTarget {
                    operation: record.operation,
                    review: unsafe_resume.id
                },
                &runtime
            )
            .await
            .is_err()
    );
    let current = fixture.service.inspect_closeout(record.operation).unwrap();
    let keep = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            current.operation,
            current.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await
        .unwrap();
    assert!(keep.issues.is_empty(), "{:?}", keep.issues);
    assert_eq!(
        fixture
            .service
            .pending_closeout_recovery(record.operation)
            .unwrap()
            .unwrap()
            .id,
        keep.id
    );
    let kept = fixture
        .service
        .apply_closeout_recovery(
            &fixture.client,
            RequestId::new(),
            CloseoutReviewTarget {
                operation: record.operation,
                review: keep.id,
            },
            &runtime,
        )
        .await
        .unwrap();
    assert_eq!(kept.stage, CloseoutStage::Retained);
    assert!(
        fixture
            .service
            .pending_closeout_recovery(record.operation)
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(&retained_file).unwrap(), bytes);
    assert!(
        fixture
            .service
            .closed_checkout_history(fixture.location)
            .unwrap()
            .preservation_paths
            .contains(&quarantine)
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn trusted_recovery_after_catalog_restore_requires_fresh_human_decision() {
    for new_data in [false, true] {
        let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
            .declare_closeout_no_loss(
                "fixture",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime,
            )
            .await
            .unwrap();
        let mut interrupted = fixture.service.clone();
        interrupted.fault = Some(std::sync::Arc::new(|stage| {
            if stage == "closeout_entry_unlinked" {
                Err(issue(IssueCode::Io, "fixture interruption"))
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
        let snapshot = fixture
            .service
            .backup(RequestId::new(), "partial-removal".into())
            .unwrap();
        let restore = fixture.service.review_restore(snapshot.id).unwrap();
        fixture
            .service
            .apply_restore(RequestId::new(), restore.id)
            .unwrap();
        let restored = fixture.service.inspect_closeout(record.operation).unwrap();
        assert!(restored.authorization.is_none());
        assert!(!restored.spec.conditional_no_loss);
        assert!(
            fixture
                .service
                .finish_closeout(record.operation, &runtime)
                .await
                .is_err()
        );
        let review = fixture
            .service
            .review_closeout_recovery(
                &fixture.client,
                restored.operation,
                restored.revision,
                CloseoutRecoveryAction::ResumeRemoval,
                &runtime,
            )
            .await
            .unwrap();
        assert!(review.issues.is_empty(), "{:?}", review.issues);
        let target = CloseoutReviewTarget {
            operation: record.operation,
            review: review.id,
        };
        if new_data {
            let path = restored
                .quarantine
                .as_ref()
                .unwrap()
                .join("new-unpreserved-data");
            std::fs::write(&path, "keep these new bytes").unwrap();
            assert!(
                fixture
                    .service
                    .apply_closeout_recovery(&fixture.client, RequestId::new(), target, &runtime)
                    .await
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "keep these new bytes"
            );
            let current = fixture.service.inspect_closeout(record.operation).unwrap();
            let keep = fixture
                .service
                .review_closeout_recovery(
                    &fixture.client,
                    current.operation,
                    current.revision,
                    CloseoutRecoveryAction::UnregisterRetainFiles,
                    &runtime,
                )
                .await
                .unwrap();
            assert!(keep.issues.is_empty(), "{:?}", keep.issues);
            let retained = fixture
                .service
                .apply_closeout_recovery(
                    &fixture.client,
                    RequestId::new(),
                    CloseoutReviewTarget {
                        operation: current.operation,
                        review: keep.id,
                    },
                    &runtime,
                )
                .await
                .unwrap();
            assert_eq!(retained.stage, CloseoutStage::Retained);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "keep these new bytes"
            );
            assert!(
                fixture
                    .service
                    .closed_checkout_history(fixture.location)
                    .unwrap()
                    .preservation_paths
                    .contains(restored.quarantine.as_ref().unwrap())
            );
        } else {
            let renewed = fixture
                .service
                .apply_closeout_recovery(&fixture.client, RequestId::new(), target, &runtime)
                .await
                .unwrap();
            assert!(matches!(
                renewed.authorization.unwrap().source,
                CloseoutAuthorizationSource::Human { .. }
            ));
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

#[tokio::test]
async fn trusted_recovery_restart_and_unregister_retain_files_preserve_history() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("unknown-data"), "must remain").unwrap();
    let record = fixture.begin(true);
    let capture = capture(&fixture, record.operation);
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let review = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            record.revision,
            CloseoutRecoveryAction::RestartPreparation,
            &runtime,
        )
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    let restarted = fixture
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
    assert_eq!(restarted.stage, CloseoutStage::Preparing);
    assert!(!restarted.spec.conditional_no_loss);
    let review = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            record.operation,
            restarted.revision,
            CloseoutRecoveryAction::UnregisterRetainFiles,
            &runtime,
        )
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    let target = CloseoutReviewTarget {
        operation: record.operation,
        review: review.id,
    };
    let request = RequestId::new();
    let retained = fixture
        .service
        .apply_closeout_recovery(&fixture.client, request, target, &runtime)
        .await
        .unwrap();
    assert_eq!(retained.stage, CloseoutStage::Retained);
    assert!(retained.authorization.is_none());
    assert_eq!(
        fixture
            .service
            .apply_closeout_recovery(&fixture.client, request, target, &runtime)
            .await
            .unwrap(),
        retained
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("unknown-data")).unwrap(),
        "must remain"
    );
    let history = fixture
        .service
        .closed_checkout_history(fixture.location)
        .unwrap();
    assert_eq!(history.location.lifecycle, LocationLifecycle::Unregistered);
    assert!(history.location.retired);
    let Home::Project(project) = history.location.home.unwrap() else {
        panic!()
    };
    let export = fixture
        .service
        .export_project(RequestId::new(), project, "retained-files".into())
        .unwrap();
    let imported = WorkspaceService::new(&fixture._directory.path().join("imported"));
    imported.initialize(RequestId::new()).unwrap();
    let import = imported
        .review_import(
            export,
            imported.status().unwrap().revision,
            ImportCollisionPolicy::Reject,
            vec![],
        )
        .unwrap();
    imported.apply_import(RequestId::new(), import.id).unwrap();
    let imported_history = imported.closed_checkout_history(fixture.location).unwrap();
    assert!(imported_history.record.is_none());
    assert_eq!(
        imported_history.location.lifecycle,
        LocationLifecycle::Unregistered
    );
    assert_eq!(
        imported_history.preservation_paths,
        history.preservation_paths
    );
    portable::validate_graph(&imported.connection().unwrap()).unwrap();
    assert!(
        history
            .preservation_paths
            .contains(&fixture.root.canonicalize().unwrap())
    );
    portable::validate_graph(&fixture.service.connection().unwrap()).unwrap();
    assert_eq!(
        fixture
            .service
            .list(
                Query {
                    visibility: Visibility::Closed,
                    ..Default::default()
                },
                None,
                50
            )
            .unwrap()
            .total,
        1
    );
    let LocationKind::Checkout { repository, .. } = history.location.kind else {
        panic!()
    };
    let registration = fixture
        .service
        .review_organization_change(
            fixture.service.status().unwrap().revision,
            OrganizationChange::RegisterLocation {
                name: "explicitly readopted".into(),
                path: fixture.root.clone(),
                registration: Registration::Checkout {
                    home: history.location.home.unwrap(),
                    repository,
                },
            },
        )
        .unwrap();
    let receipt = fixture
        .service
        .apply_organization_change(RequestId::new(), registration.id)
        .unwrap();
    assert!(
        !receipt
            .targets
            .contains(&EntityId::Location(fixture.location))
    );
    assert!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("unknown-data")).unwrap(),
        "must remain"
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn trusted_recovery_reviews_empty_unconfirmed_holding_before_resuming() {
    let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
        .declare_closeout_no_loss(
            "fixture",
            RequestId::new(),
            review.target(),
            "synthetic assessment",
            &runtime,
        )
        .await
        .unwrap();
    let mut interrupted = fixture.service.clone();
    interrupted.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_holding_created" {
            Err(issue(IssueCode::Io, "fixture interruption"))
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
    let partial = fixture.service.inspect_closeout(record.operation).unwrap();
    assert!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .is_err()
    );
    let partial = fixture.service.inspect_closeout(partial.operation).unwrap();
    let review = fixture
        .service
        .review_closeout_recovery(
            &fixture.client,
            partial.operation,
            partial.revision,
            CloseoutRecoveryAction::ResumeRemoval,
            &runtime,
        )
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    assert!(review.adopt_empty_holding.is_some());
    let authorized = fixture
        .service
        .apply_closeout_recovery(
            &fixture.client,
            RequestId::new(),
            CloseoutReviewTarget {
                operation: partial.operation,
                review: review.id,
            },
            &runtime,
        )
        .await
        .unwrap();
    assert!(matches!(
        authorized.authorization.unwrap().source,
        CloseoutAuthorizationSource::Human { .. }
    ));
    assert!(!authorized.spec.conditional_no_loss);
    assert_eq!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap()
            .stage,
        CloseoutStage::Closed
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn removal_honors_durable_execution_stop_and_resumes_with_new_capture() {
    use jcode_tool_core::OutputCapture;
    let (fixture, record, first_capture) = prepared_approval_fixture(true, false).await;
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(
        &sessions,
        &execution,
        fixture._directory.path(),
        &first_capture,
    );
    let review = fixture
        .service
        .review_closeout_removal(record.operation, record.revision, &runtime)
        .await
        .unwrap();
    fixture
        .service
        .declare_closeout_no_loss(
            "fixture-agent",
            RequestId::new(),
            review.target(),
            "synthetic assessment",
            &runtime,
        )
        .await
        .unwrap();
    let run = execution
        .inspect(&first_capture.reference().unwrap().invocation_id)
        .unwrap()
        .unwrap();
    let stop_store = execution.clone();
    let mut controlled = fixture.service.clone();
    controlled.fault = Some(std::sync::Arc::new(move |stage| {
        if stage == "closeout_entry_unlinked" {
            assert!(
                stop_store
                    .request_stop(
                        &run.id,
                        &run.owner,
                        jcode_tool_types::StopCause::HumanCancellation
                    )
                    .unwrap()
            );
        }
        Ok(())
    }));
    assert_eq!(
        controlled
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap_err()
            .code,
        IssueCode::Busy
    );
    let partial = fixture.service.inspect_closeout(record.operation).unwrap();
    assert_eq!(partial.stage, CloseoutStage::RecoveryRequired);
    assert_eq!(partial.removed_entries, 1);
    assert!(partial.quarantine.as_ref().unwrap().is_dir());
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(first_capture.reference().unwrap());
    first_capture
        .seal(output, crate::execution::RunState::Cancelled)
        .unwrap();
    let second_capture = capture(&fixture, record.operation);
    let resumed = CloseoutRuntime::new(
        &sessions,
        &execution,
        fixture._directory.path(),
        &second_capture,
    );
    assert_eq!(
        fixture
            .service
            .finish_closeout(record.operation, &resumed)
            .await
            .unwrap()
            .stage,
        CloseoutStage::Closed
    );
    finish_capture(&second_capture);
}

#[tokio::test]
async fn clone_sources_observe_closing_and_retain_physical_use_until_acquisition_finishes() {
    for file_transport in [false, true] {
        let fixture = Fixture::new();
        let Entity::Location(location) = fixture
            .service
            .inspect(EntityId::Location(fixture.location))
            .unwrap()
        else {
            panic!()
        };
        let LocationKind::Checkout { repository, .. } = location.kind else {
            panic!()
        };
        let parent = fixture
            .service
            .resolver
            .bind_directory(fixture._directory.path())
            .unwrap();
        let source_binding = fixture
            .service
            .resolver
            .bind_directory(&fixture.root)
            .unwrap();
        let destination = fixture._directory.path().join("independent");
        let source = if file_transport {
            CloneSource::Remote {
                url: url::Url::from_directory_path(fixture.root.canonicalize().unwrap())
                    .unwrap()
                    .to_string(),
            }
        } else {
            CloneSource::Local {
                path: fixture.root.clone(),
            }
        };
        let spec = CloneSpec {
            home: location.home.unwrap(),
            repository,
            name: "independent".into(),
            source,
            base: CloneBase::Branch {
                name: git_text(&fixture.root, &["symbolic-ref", "--short", "HEAD"])
                    .trim()
                    .into(),
            },
            branch: CloneBranch::Detached,
            remotes: vec![],
            destination: CloneDestination::Custom {
                volume_uuid: parent.volume().as_str().into(),
                path: destination.clone(),
            },
            submodules: false,
            lfs: false,
            trusted_submodule_urls: vec![],
            trusted_lfs_urls: vec![],
        };
        let review = fixture
            .service
            .review_clone(fixture.service.status().unwrap().revision, spec)
            .unwrap();
        let clone = fixture
            .service
            .begin_clone(RequestId::new(), review.id)
            .unwrap();
        let closeout = fixture.begin(false);
        fixture
            .service
            .fence_closeout(closeout.operation, closeout.revision)
            .unwrap();
        let capture = capture(&fixture, clone.operation);
        assert_eq!(
            fixture
                .service
                .execute_clone(clone.request, &capture)
                .await
                .unwrap_err()
                .code,
            IssueCode::LiveWork
        );
        assert!(!destination.exists());
        let closeout = fixture
            .service
            .inspect_closeout(closeout.operation)
            .unwrap();
        fixture
            .service
            .revoke_closeout(
                &fixture.client,
                RequestId::new(),
                closeout.operation,
                closeout.revision,
            )
            .unwrap();
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_copy = observed.clone();
        let other = fixture.service.clone();
        let mut producer = fixture.service.clone();
        producer.fault = Some(std::sync::Arc::new(move |stage| {
            if stage == "clone_source_admitted" {
                assert!(
                    matches!(other.acquire_binding(&source_binding), Err(error) if error.code == IssueCode::Busy)
                );
                observed_copy.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }));
        let _ordinary_parent_use = fixture.service.acquire_mutation_binding(&parent).unwrap();
        assert_eq!(
            producer
                .execute_clone(clone.request, &capture)
                .await
                .unwrap()
                .state,
            CloneState::Ready
        );
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(destination.join(".git").is_dir());
        assert!(fixture.root.join(".git").is_dir());
        finish_capture(&capture);
    }
}

#[test]
fn work_observation_includes_other_namespace_and_tracks_committed_location() {
    let _environment = crate::storage::lock_test_env();
    let fixture = Fixture::new();
    let foreign_root = crate::storage::jcode_dir().unwrap();
    let local_root = fixture._directory.path().join("local-sessions");
    let local_execution = crate::execution::ExecutionStore::open(&local_root).unwrap();
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(fixture.root.canonicalize().unwrap().display().to_string());
    session.save().unwrap();
    let before = session.messages.clone();
    let activity = crate::session::StreamingGuard::for_session(&session).unwrap();
    let started = fixture.begin(false);
    let stored = load(&fixture.service.connection().unwrap(), started.operation).unwrap();
    let mut findings = Vec::new();
    fixture
        .service
        .observe_closeout_work(
            &stored,
            &local_root,
            &local_execution,
            fixture._directory.path(),
            &mut findings,
        )
        .unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == CloseoutWorkKind::Session
                && finding.identity.contains(&session.id)),
        "{findings:?}"
    );
    let projections = crate::execution::active_session_locations().unwrap();
    assert!(projections.iter().any(|entry| entry.session == session.id
        && entry.session_root == foreign_root.canonicalize().unwrap()));
    // Synthetic committed location change through the real persistence owner.
    // This checks observation, not the public move/notice workflow tested elsewhere.
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
    findings.clear();
    fixture
        .service
        .observe_closeout_work(
            &stored,
            &local_root,
            &local_execution,
            fixture._directory.path(),
            &mut findings,
        )
        .unwrap();
    assert!(
        !findings
            .iter()
            .any(|finding| finding.kind == CloseoutWorkKind::Session
                && finding.identity.contains(&session.id)),
        "{findings:?}"
    );
    assert_eq!(
        serde_json::to_value(&session.messages).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    drop(activity);
    assert!(
        !crate::execution::active_session_locations()
            .unwrap()
            .iter()
            .any(|entry| entry.session == session.id)
    );
}

#[tokio::test]
async fn removal_rechecks_authority_after_concurrent_human_revocation() {
    let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
        .declare_closeout_no_loss(
            "fixture-agent",
            RequestId::new(),
            review.target(),
            "synthetic assessment",
            &runtime,
        )
        .await
        .unwrap();
    let other = fixture.service.clone();
    let operation = record.operation;
    let mut racing = fixture.service.clone();
    racing.fault = Some(std::sync::Arc::new(move |stage| {
        if stage == "closeout_before_removal_lease" {
            let current = other.inspect_closeout(operation)?;
            let client = WorkspaceClientAuthority::authenticated("fixture-human")?;
            other.revoke_closeout(&client, RequestId::new(), operation, current.revision)?;
        }
        Ok(())
    }));
    assert!(racing.finish_closeout(operation, &runtime).await.is_err());
    assert_eq!(
        fixture.service.inspect_closeout(operation).unwrap().stage,
        CloseoutStage::Revoked
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("payload")).unwrap(),
        "preserved data"
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn removal_preserves_replacement_paths_and_new_entries_after_interruption() {
    for replace_original in [false, true] {
        let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime,
            )
            .await
            .unwrap();
        let mut broken = fixture.service.clone();
        broken.fault = Some(std::sync::Arc::new(|stage| {
            if stage == "closeout_quarantine_renamed" {
                Err(io("fixture stop after quarantine"))
            } else {
                Ok(())
            }
        }));
        assert!(
            broken
                .finish_closeout(record.operation, &runtime)
                .await
                .is_err()
        );
        let quarantine = fixture
            .root
            .parent()
            .unwrap()
            .join(format!(".jcode-closeout-{}", record.operation));
        let new_path = if replace_original {
            std::fs::create_dir(&fixture.root).unwrap();
            fixture.root.join("new-data")
        } else {
            quarantine.join("new-data")
        };
        std::fs::write(&new_path, "retain replacement information").unwrap();
        assert!(
            fixture
                .service
                .finish_closeout(record.operation, &runtime)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "retain replacement information"
        );
        assert_eq!(
            std::fs::read_to_string(quarantine.join("payload")).unwrap(),
            "preserved data"
        );
        assert_eq!(
            fixture
                .service
                .inspect_closeout(record.operation)
                .unwrap()
                .removed_entries,
            0
        );
        assert!(
            fixture
                .service
                .acquire_location_use(Some(&quarantine), &[])
                .is_err()
        );
        if replace_original {
            let retained = fixture._directory.path().join("retained-replacement");
            std::fs::rename(&fixture.root, &retained).unwrap();
            assert_eq!(
                fixture
                    .service
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .unwrap()
                    .stage,
                CloseoutStage::Closed
            );
            assert_eq!(
                std::fs::read_to_string(retained.join("new-data")).unwrap(),
                "retain replacement information"
            );
        }
        finish_capture(&capture);
    }
}

#[tokio::test]
async fn removal_of_linked_worktree_preserves_shared_repository_and_reconciles_git_effect() {
    for interrupted in [false, true] {
        let mut fixture = Fixture::new();
        let main = fixture.root.clone();
        let linked = fixture._directory.path().join("linked");
        git(
            &main,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        std::fs::write(main.join("main-only"), "retain main work").unwrap();
        std::fs::write(linked.join("linked-only"), "preserve linked work").unwrap();
        let Entity::Location(location) = fixture
            .service
            .inspect(EntityId::Location(fixture.location))
            .unwrap()
        else {
            panic!()
        };
        let LocationKind::Checkout { repository, .. } = location.kind else {
            panic!()
        };
        let EntityId::Location(id) = change(
            &fixture.service,
            OrganizationChange::RegisterLocation {
                name: "linked".into(),
                path: linked.clone(),
                registration: Registration::Checkout {
                    home: location.home.unwrap(),
                    repository,
                },
            },
        ) else {
            panic!()
        };
        fixture.root = linked;
        fixture.location = id;
        let before_refs = git_text(&main, &["show-ref"]);
        let mut spec = fixture.spec(true);
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
        assert!(review.issues.is_empty(), "{:?}", review.issues);
        fixture
            .service
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime,
            )
            .await
            .unwrap();
        if interrupted {
            let mut broken = fixture.service.clone();
            broken.fault = Some(std::sync::Arc::new(|stage| {
                if stage == "closeout_worktree_retired" {
                    Err(io("fixture interrupted after Git retirement"))
                } else {
                    Ok(())
                }
            }));
            assert!(
                broken
                    .finish_closeout(record.operation, &runtime)
                    .await
                    .is_err()
            );
        }
        let closed = fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap();
        assert_eq!(closed.stage, CloseoutStage::Closed);
        assert!(!fixture.root.exists());
        assert_eq!(
            std::fs::read_to_string(main.join("main-only")).unwrap(),
            "retain main work"
        );
        assert_eq!(git_text(&main, &["show-ref"]), before_refs);
        assert_eq!(
            git_text(&main, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
        verification::preservation(
            &load(&fixture.service.connection().unwrap(), record.operation).unwrap(),
        )
        .unwrap();
        finish_capture(&capture);
    }
}

#[tokio::test]
async fn removal_closes_disposable_checkout_and_retains_history_and_preservation() {
    let (fixture, record, capture) = prepared_approval_fixture(false, false).await;
    let sessions = fixture._directory.path().join("sessions-state");
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    assert_eq!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap_err()
            .code,
        IssueCode::PermissionRequired
    );
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
    let closed = fixture
        .service
        .finish_closeout(record.operation, &runtime)
        .await
        .unwrap();
    assert_eq!(closed.stage, CloseoutStage::Closed);
    assert!(closed.removed_entries > 0);
    assert!(!fixture.root.exists());
    assert!(!closed.quarantine.as_ref().unwrap().exists());
    let Entity::Location(location) = fixture
        .service
        .inspect(EntityId::Location(fixture.location))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(location.lifecycle, LocationLifecycle::Closed);
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    verification::preservation(&stored).unwrap();
    let history: String = fixture
        .service
        .connection()
        .unwrap()
        .query_row(
            "SELECT body FROM closed_history WHERE location=?1",
            [fixture.location.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let history: portable::ClosedReference = decode(&history).unwrap();
    assert!(history.report.unwrap().is_file());
    let history = fixture
        .service
        .closed_checkout_history(fixture.location)
        .unwrap();
    assert_eq!(history.record.as_ref(), Some(&closed));
    let progress = fixture
        .service
        .closeout_removal_progress(record.operation, closed.revision, 0, 3)
        .unwrap();
    assert_eq!(progress.total, closed.removed_entries);
    assert_eq!(progress.completed, progress.total);
    assert!(progress.pending.is_none());
    assert!(
        progress
            .entries
            .iter()
            .all(|entry| entry.progress == CloseoutEntryProgress::Removed)
    );
    assert_eq!(progress.entries.len(), 3);
    assert_eq!(progress.next, Some(3));
    assert_eq!(
        fixture
            .service
            .closeout_removal_progress(record.operation, closed.revision - 1, 0, 3)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap(),
        closed
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn removal_recovers_each_journaled_effect_without_repeating_unrelated_effects() {
    for boundary in [
        "closeout_removal_intent",
        "closeout_quarantine_renamed",
        "closeout_entry_intent",
        "closeout_entry_captured",
        "closeout_entry_unlinked",
        "closeout_removed_before_closed",
    ] {
        let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
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
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime,
            )
            .await
            .unwrap();
        let mut broken = fixture.service.clone();
        broken.fault = Some(std::sync::Arc::new(move |stage| {
            if stage == boundary {
                Err(io("owned fixture checkpoint interruption"))
            } else {
                Ok(())
            }
        }));
        assert!(
            broken
                .finish_closeout(record.operation, &runtime)
                .await
                .is_err(),
            "{boundary}"
        );
        assert_ne!(
            fixture
                .service
                .inspect_closeout(record.operation)
                .unwrap()
                .stage,
            CloseoutStage::Closed
        );
        let interrupted = fixture.service.inspect_closeout(record.operation).unwrap();
        let progress = fixture
            .service
            .closeout_removal_progress(record.operation, interrupted.revision, 0, 3)
            .unwrap();
        if matches!(
            boundary,
            "closeout_entry_captured" | "closeout_entry_unlinked"
        ) {
            assert_eq!(
                progress.pending.as_ref().unwrap().progress,
                CloseoutEntryProgress::Unconfirmed
            );
        }
        assert!(
            fixture
                .service
                .revoke_closeout(
                    &fixture.client,
                    RequestId::new(),
                    record.operation,
                    interrupted.revision
                )
                .is_err()
        );
        let completed = fixture
            .service
            .finish_closeout(record.operation, &runtime)
            .await
            .unwrap_or_else(|error| panic!("{boundary}: {error}"));
        assert_eq!(completed.stage, CloseoutStage::Closed, "{boundary}");
        assert!(!fixture.root.exists());
        verification::preservation(
            &load(&fixture.service.connection().unwrap(), record.operation).unwrap(),
        )
        .unwrap();
        finish_capture(&capture);
    }
}

#[tokio::test]
async fn final_authorization_requires_preserved_linked_content() {
    let fixture = Fixture::new();
    let linked = Path::new(".jcode/instructions/linked.md");
    std::fs::create_dir_all(fixture.root.join(linked.parent().unwrap())).unwrap();
    std::fs::write(fixture.root.join(linked), "synthetic linked content").unwrap();
    let sessions = fixture._directory.path().join("sessions-state");
    let instructions = crate::instruction::InstructionRepositoryService::from_paths(
        &sessions,
        fixture.service.root.parent().unwrap(),
    );
    assert!(
        instructions
            .resolve_project_repository(&fixture.root)
            .unwrap()
            .is_none()
    );
    let started = fixture.begin(true);
    let capture = capture(&fixture, started.operation);
    let mut current = fixture
        .service
        .refresh_closeout_in(started.operation, started.revision, &sessions, &capture)
        .await
        .unwrap();
    let page = fixture
        .service
        .closeout_inventory(
            current.operation,
            current.inventory_digest.as_ref().unwrap(),
            0,
            200,
        )
        .unwrap();
    assert!(page.next.is_none());
    for entry in page.entries {
        if matches!(
            entry.kind,
            CloseoutEntryKind::File | CloseoutEntryKind::Symlink
        ) {
            let disposition = if entry.path == linked {
                CloseoutDisposition::Redundant {
                    reason: "synthetic judgment".into(),
                }
            } else {
                CloseoutDisposition::Preserve
            };
            current = fixture
                .service
                .record_closeout_disposition(
                    current.operation,
                    current.revision,
                    CloseoutDecision {
                        entry: entry.id,
                        disposition,
                        recorded_by: "fixture-agent".into(),
                    },
                )
                .unwrap();
        }
    }
    current = fixture
        .service
        .preserve_closeout(current.operation, current.revision, &capture)
        .await
        .unwrap();
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let review = fixture
        .service
        .review_closeout_removal(current.operation, current.revision, &runtime)
        .await
        .unwrap();
    assert!(
        review
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::PreservationIncomplete)
    );
    assert_eq!(
        fixture
            .service
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime
            )
            .await
            .unwrap_err()
            .code,
        IssueCode::PreservationIncomplete
    );
    assert!(fixture.root.join(linked).is_file());
    finish_capture(&capture);
}

#[tokio::test]
async fn final_authorization_review_reports_committed_backup_failure() {
    let (fixture, record, capture) = prepared_approval_fixture(false, false).await;
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let sessions = fixture._directory.path().join("sessions-state");
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let snapshots = fixture.service.root.join("snapshots");
    let retained = fixture.service.root.join("fixture-retained-snapshots");
    std::fs::rename(&snapshots, &retained).unwrap();
    std::fs::write(&snapshots, "owned fixture I/O blocker").unwrap();
    let result = fixture
        .service
        .review_closeout_removal(record.operation, record.revision, &runtime)
        .await;
    std::fs::remove_file(&snapshots).unwrap();
    std::fs::rename(&retained, &snapshots).unwrap();
    assert_eq!(result.unwrap_err().code, IssueCode::BackupFailed);
    let review = fixture
        .service
        .inspect_closeout_review(record.operation)
        .unwrap()
        .unwrap();
    assert!(review.revision > record.revision);
    fixture
        .service
        .backup(RequestId::new(), "recovered-review".into())
        .unwrap();
    let authorized = fixture
        .service
        .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
        .await
        .unwrap();
    assert_eq!(authorized.stage, CloseoutStage::Authorized);
    assert!(fixture.root.exists());
    finish_capture(&capture);
}

#[tokio::test]
async fn final_authorization_rejects_shared_metadata_and_incomplete_preservation() {
    let fixture = Fixture::new();
    let other = fixture._directory.path().join("dependent-worktree");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-b",
            "dependent",
            other.to_str().unwrap(),
        ],
    );
    let started = fixture.begin(true);
    let capture = capture(&fixture, started.operation);
    let sessions = fixture._directory.path().join("sessions-state");
    let current = fixture
        .service
        .refresh_closeout_in(started.operation, started.revision, &sessions, &capture)
        .await
        .unwrap();
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let review = fixture
        .service
        .review_closeout_removal(current.operation, current.revision, &runtime)
        .await
        .unwrap();
    assert!(
        review
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::Referenced)
    );
    assert!(
        review
            .issues
            .iter()
            .any(|issue| issue.code == IssueCode::PreservationIncomplete)
    );
    assert_eq!(
        fixture
            .service
            .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
            .await
            .unwrap_err()
            .code,
        IssueCode::PreservationIncomplete
    );
    assert_eq!(
        fixture
            .service
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime
            )
            .await
            .unwrap_err()
            .code,
        IssueCode::PreservationIncomplete
    );
    assert!(fixture.root.join(".git").is_dir());
    assert!(other.join(".git").is_file());
    finish_capture(&capture);
}

async fn prepared_approval_fixture(
    conditional: bool,
    split_index: bool,
) -> (Fixture, CloseoutRecord, crate::execution::Capture) {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("payload"), "preserved data").unwrap();
    if split_index {
        git(&fixture.root, &["add", "payload"]);
        git(&fixture.root, &["update-index", "--split-index"]);
    }
    let mut spec = fixture.spec(conditional);
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
    let current = fixture
        .service
        .refresh_closeout_in(
            started.operation,
            started.revision,
            &fixture._directory.path().join("sessions-state"),
            &capture,
        )
        .await
        .unwrap();
    let preserved = fixture
        .service
        .preserve_closeout(current.operation, current.revision, &capture)
        .await
        .unwrap();
    (fixture, preserved, capture)
}

#[tokio::test]
async fn final_authorization_is_review_bound_conditional_only_and_revocable() {
    for (conditional, split_index) in [(false, false), (true, false), (true, true)] {
        let (fixture, record, capture) = prepared_approval_fixture(conditional, split_index).await;
        let execution =
            crate::execution::ExecutionStore::open(&fixture._directory.path().join("output"))
                .unwrap();
        let sessions = fixture._directory.path().join("sessions-state");
        let runtime =
            CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
        let review = fixture
            .service
            .review_closeout_removal(record.operation, record.revision, &runtime)
            .await
            .unwrap();
        assert!(review.issues.is_empty(), "{:?}", review.issues);
        let foreign = CloseoutReviewTarget {
            operation: record.operation,
            review: ReviewId::new(),
        };
        assert_eq!(
            fixture
                .service
                .approve_closeout_removal(&fixture.client, RequestId::new(), foreign, &runtime)
                .await
                .unwrap_err()
                .code,
            IssueCode::Conflict
        );
        let request = RequestId::new();
        let authorized = if conditional {
            fixture
                .service
                .declare_closeout_no_loss(
                    "fixture-agent",
                    request,
                    review.target(),
                    "synthetic assessment",
                    &runtime,
                )
                .await
                .unwrap()
        } else {
            assert_eq!(
                fixture
                    .service
                    .declare_closeout_no_loss(
                        "fixture-agent",
                        RequestId::new(),
                        review.target(),
                        "synthetic assessment",
                        &runtime
                    )
                    .await
                    .unwrap_err()
                    .code,
                IssueCode::PermissionRequired
            );
            fixture
                .service
                .approve_closeout_removal(&fixture.client, request, review.target(), &runtime)
                .await
                .unwrap()
        };
        assert_eq!(authorized.stage, CloseoutStage::Authorized);
        assert_eq!(
            matches!(
                authorized.authorization.as_ref().unwrap().source,
                CloseoutAuthorizationSource::Conditional { .. }
            ),
            conditional
        );
        let replay = if conditional {
            fixture
                .service
                .declare_closeout_no_loss(
                    "fixture-agent",
                    request,
                    review.target(),
                    "synthetic assessment",
                    &runtime,
                )
                .await
                .unwrap()
        } else {
            fixture
                .service
                .approve_closeout_removal(&fixture.client, request, review.target(), &runtime)
                .await
                .unwrap()
        };
        assert_eq!(replay.revision, authorized.revision);
        let revoked = fixture
            .service
            .revoke_closeout(
                &fixture.client,
                RequestId::new(),
                record.operation,
                authorized.revision,
            )
            .unwrap();
        assert_eq!(revoked.stage, CloseoutStage::Revoked);
        assert!(revoked.authorization.is_none());
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("payload")).unwrap(),
            "preserved data"
        );
        finish_capture(&capture);
    }
}

#[tokio::test]
async fn final_authorization_rechecks_preservation_and_source() {
    use std::io::BufRead;
    let (fixture, record, capture) = prepared_approval_fixture(true, false).await;
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let sessions = fixture._directory.path().join("sessions-state");
    let runtime = CloseoutRuntime::new(&sessions, &execution, fixture._directory.path(), &capture);
    let review = fixture
        .service
        .review_closeout_removal(record.operation, record.revision, &runtime)
        .await
        .unwrap();
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let manifest: preservation::PreservationManifest =
        storage::read_json(stored.preservation.as_ref().unwrap()).unwrap();
    let saved = std::io::BufReader::new(std::fs::File::open(manifest.files).unwrap())
        .lines()
        .map(|line| decode::<files::PreservedItem>(&line.unwrap()).unwrap())
        .find(|item| item.item.entry.path == Path::new("payload"))
        .unwrap()
        .saved
        .unwrap();
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.approval-fixture", "changed metadata only"])
            .arg(&saved)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fixture
            .service
            .approve_closeout_removal(&fixture.client, RequestId::new(), review.target(), &runtime)
            .await
            .unwrap_err()
            .code,
        IssueCode::PreservationIncomplete
    );
    assert!(
        fixture
            .service
            .inspect_closeout(record.operation)
            .unwrap()
            .authorization
            .is_none()
    );
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "preserved data");
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-d", "jcode.approval-fixture"])
            .arg(&saved)
            .status()
            .unwrap()
            .success()
    );
    verification::preservation(&stored).unwrap();
    std::fs::write(&saved, "corrupt archive").unwrap();
    assert!(
        fixture
            .service
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .inspect_closeout(record.operation)
            .unwrap()
            .authorization
            .is_none()
    );
    std::fs::write(&saved, "preserved data").unwrap();
    std::fs::write(fixture.root.join("payload"), "new source data").unwrap();
    assert_eq!(
        fixture
            .service
            .declare_closeout_no_loss(
                "fixture-agent",
                RequestId::new(),
                review.target(),
                "synthetic assessment",
                &runtime
            )
            .await
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("payload")).unwrap(),
        "new source data"
    );
    finish_capture(&capture);
}

#[tokio::test]
async fn configured_external_volume_preserves_and_reports_actual_protection() {
    let Some(mount) = std::env::var_os("JCODE_WP07_EXTERNAL_MOUNT") else {
        eprintln!("External-volume fixture not selected; no external-volume acceptance claim");
        return;
    };
    let fixture = Fixture::new();
    let mount = PathBuf::from(mount);
    let binding = fixture.service.resolver.bind_directory(&mount).unwrap();
    assert_eq!(
        binding.volume().as_str(),
        std::env::var("JCODE_WP07_EXPECTED_VOLUME_UUID").unwrap()
    );
    let destination = tempfile::Builder::new()
        .prefix(".jcode-wp07-owned-")
        .tempdir_in(&mount)
        .unwrap();
    let owned_binding = fixture
        .service
        .resolver
        .bind_directory(destination.path())
        .unwrap();
    std::fs::write(
        destination.path().join("OWNER.json"),
        b"{\"fixture\":\"SP-58-C01/WP-07\"}",
    )
    .unwrap();
    let data = vec![37u8; 128 * 1024];
    std::fs::write(fixture.root.join("retained.bin"), &data).unwrap();
    let mut spec = fixture.spec(false);
    spec.full_archive = true;
    spec.preservation_directory = Some(destination.path().into());
    let started = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            spec,
        )
        .unwrap();
    let expected = std::env::var("JCODE_WP07_EXPECTED_OWNERSHIP")
        .unwrap()
        .parse::<bool>()
        .unwrap();
    assert_eq!(started.preservation_volume_ownership, Some(expected));
    let capture = capture(&fixture, started.operation);
    let current = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let preserved = fixture
        .service
        .preserve_closeout(current.operation, current.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), preserved.operation).unwrap();
    let manifest: preservation::PreservationManifest =
        storage::read_json(stored.preservation.as_ref().unwrap()).unwrap();
    let restored = manifest
        .files
        .parent()
        .unwrap()
        .join("verified-restore/retained.bin");
    assert_eq!(std::fs::read(restored).unwrap(), data);
    assert_eq!(
        std::fs::read(fixture.root.join("retained.bin")).unwrap(),
        data
    );
    fixture
        .service
        .resolver
        .resolve_directory(&owned_binding)
        .unwrap();
    finish_capture(&capture);
    let path = destination.path().to_path_buf();
    destination.close().unwrap();
    assert!(!path.exists());
    eprintln!(
        "Owned external fixture completed and cleaned: {}",
        path.display()
    );
}

#[test]
fn reference_inventory_retains_session_and_linked_document_without_hydration_or_rewrite() {
    let _environment = crate::storage::lock_test_env();
    let fixture = Fixture::new();
    let home = crate::storage::jcode_dir().unwrap();
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = Some(fixture.root.canonicalize().unwrap().display().to_string());
    let block = serde_json::from_value(
        serde_json::json!({"type":"text","text":"synthetic retained transcript"}),
    )
    .unwrap();
    session.add_message(crate::message::Role::User, vec![block]);
    session.save().unwrap();
    let session_path = home.join("sessions").join(format!("{}.json", session.id));
    let session_before = std::fs::read(&session_path).unwrap();
    let document = fixture.root.join("linked.md");
    std::fs::write(&document, "synthetic linked document").unwrap();
    crate::side_panel::load_markdown_file(&session.id, "linked", None, &document, false).unwrap();
    let index = home.join("side_panel").join(&session.id).join("index.json");
    let index_before = std::fs::read(&index).unwrap();
    std::fs::remove_file(&document).unwrap();
    let metadata = crate::side_panel::references_for_session_in(&home, &session.id).unwrap();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].id, "linked");
    assert!(!document.exists());
    std::fs::write(&document, "synthetic linked document").unwrap();
    let started = fixture.begin(false);
    let stored = load(&fixture.service.connection().unwrap(), started.operation).unwrap();
    let references = fixture.service.closeout_references(&stored, &home).unwrap();
    assert!(
        references
            .sessions
            .iter()
            .any(|reference| reference.session == session.id)
    );
    assert!(
        references
            .links
            .iter()
            .any(|link| link.owner == format!("{}/linked", session.id)
                && link.path == document.canonicalize().unwrap())
    );
    assert_eq!(std::fs::read(session_path).unwrap(), session_before);
    assert_eq!(std::fs::read(index).unwrap(), index_before);
    assert!(!fixture.root.join(".jcode").exists());
}

#[test]
fn archive_does_not_recreate_missing_destination_or_follow_replaced_ancestors() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("archive");
    std::fs::create_dir(&root).unwrap();
    let archive = archive::Archive::open(&root).unwrap();
    let nested = archive.subtree(&root.join("a/b")).unwrap();
    let outside = temporary.path().join("outside");
    std::fs::rename(root.join("a"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("a")).unwrap();
    assert!(nested.file(&root.join("a/b/should-not-exist")).is_err());
    assert!(!outside.join("b/should-not-exist").exists());
    std::fs::rename(&root, temporary.path().join("offline")).unwrap();
    assert!(archive.directory(&root.join("fallback")).is_err());
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    assert!(archive.file(&root.join("replacement-file")).is_err());
    assert!(!root.join("replacement-file").exists());
}

#[tokio::test]
async fn linked_worktree_staged_blob_and_split_index_restore_without_source() {
    split_index_restore(true).await;
}

#[tokio::test]
async fn ordinary_checkout_staged_blob_and_split_index_restore_without_source() {
    split_index_restore(false).await;
}

async fn split_index_restore(linked: bool) {
    let mut fixture = Fixture::new();
    let main = fixture.root.clone();
    let worktree = if linked {
        fixture._directory.path().join("linked")
    } else {
        main.clone()
    };
    if linked {
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                worktree.to_str().unwrap(),
            ],
        );
    }
    std::fs::write(worktree.join("staged"), "staged-only contents\n").unwrap();
    git(&worktree, &["add", "staged"]);
    git(&worktree, &["update-index", "--split-index"]);
    std::fs::write(worktree.join("staged"), "different unstaged contents\n").unwrap();
    let expected_index = git_text(&worktree, &["ls-files", "--stage"]);
    let blob = git_text(&worktree, &["rev-parse", ":staged"]);
    if linked {
        let Entity::Location(original) = fixture
            .service
            .inspect(EntityId::Location(fixture.location))
            .unwrap()
        else {
            panic!()
        };
        let LocationKind::Checkout { repository, .. } = original.kind else {
            panic!()
        };
        let EntityId::Location(location) = change(
            &fixture.service,
            OrganizationChange::RegisterLocation {
                name: "linked".into(),
                path: worktree.clone(),
                registration: Registration::Checkout {
                    home: original.home.unwrap(),
                    repository,
                },
            },
        ) else {
            panic!()
        };
        fixture.root = worktree;
        fixture.location = location;
    }
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let record = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert!(snapshots[0].index_blobs.contains(blob.trim()));
    assert_eq!(
        snapshots[0].git_directory != snapshots[0].common_directory,
        linked
    );
    let bundle = super::git::preserve(
        &fixture.service,
        record.operation,
        &snapshots[0],
        &fixture._directory.path().join("preserved-linked"),
        &capture,
        &archive::Archive::open(fixture._directory.path()).unwrap(),
    )
    .await
    .unwrap();
    let stage = bundle.parent().unwrap();
    let restored = stage.join("restored.git");
    assert!(
        stage
            .join("administration/verified-restore/index")
            .is_file()
    );
    assert!(restored.join("index").is_file());
    std::fs::rename(&fixture.root, fixture.root.with_extension("offline")).unwrap();
    if linked {
        std::fs::rename(&main, main.with_extension("offline")).unwrap();
    }
    assert_eq!(
        git_text(&restored, &["cat-file", "blob", blob.trim()]),
        "staged-only contents\n"
    );
    assert_eq!(
        git_text(&restored, &["ls-files", "--stage"]),
        expected_index
    );
    git(&restored, &["fsck", "--full", "--no-reflogs"]);
    finish_capture(&capture);
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        );
        let service = WorkspaceService::new(&directory.path().join("state"));
        service.initialize(RequestId::new()).unwrap();
        let EntityId::Project(project) = change(
            &service,
            OrganizationChange::CreateProject {
                name: "project".into(),
            },
        ) else {
            panic!()
        };
        let EntityId::Repository(repository) = change(
            &service,
            OrganizationChange::CreateRepository {
                name: "repository".into(),
                remotes: vec![],
            },
        ) else {
            panic!()
        };
        change(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        );
        let EntityId::Location(location) = change(
            &service,
            OrganizationChange::RegisterLocation {
                name: "checkout".into(),
                path: root.clone(),
                registration: Registration::Checkout {
                    home: Home::Project(project),
                    repository,
                },
            },
        ) else {
            panic!()
        };
        Self {
            _directory: directory,
            service,
            root,
            location,
            client: WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
        }
    }
    fn spec(&self, conditional: bool) -> CloseoutSpec {
        let Entity::Location(location) = self
            .service
            .inspect(EntityId::Location(self.location))
            .unwrap()
        else {
            panic!()
        };
        CloseoutSpec {
            location: self.location,
            expected_generation: location.binding_generation,
            preservation_directory: None,
            conditional_no_loss: conditional,
            full_archive: false,
        }
    }
    fn begin(&self, conditional: bool) -> CloseoutRecord {
        self.service
            .begin_closeout(
                &self.client,
                RequestId::new(),
                self.service.status().unwrap().revision,
                self.spec(conditional),
            )
            .unwrap()
    }
}
fn change(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
}
fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn authorization_is_exact_default_off_revocable_and_request_idempotent() {
    let fixture = Fixture::new();
    let spec = fixture.spec(false);
    let request = RequestId::new();
    let revision = fixture.service.status().unwrap().revision;
    let first = fixture
        .service
        .begin_closeout(&fixture.client, request, revision, spec.clone())
        .unwrap();
    assert!(!first.spec.conditional_no_loss);
    assert_eq!(
        first,
        fixture
            .service
            .begin_closeout(&fixture.client, request, revision, spec.clone())
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.conditional_no_loss = true;
    assert_eq!(
        fixture
            .service
            .begin_closeout(&fixture.client, request, revision, changed)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                spec
            )
            .unwrap_err()
            .code,
        IssueCode::Busy
    );
    let revoked = fixture
        .service
        .revoke_closeout(
            &fixture.client,
            RequestId::new(),
            first.operation,
            first.revision,
        )
        .unwrap();
    assert_eq!(revoked.stage, CloseoutStage::Revoked);
    assert!(
        fixture
            .service
            .inventory_closeout(first.operation, revoked.revision)
            .is_err()
    );
    let next = fixture.begin(true);
    assert!(next.spec.conditional_no_loss);
    assert_ne!(first.operation, next.operation);
    assert!(fixture.root.exists());
}

#[test]
fn inventory_captures_ignored_hidden_index_links_and_rejects_stale_decisions() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join(".gitignore"), "ignored\n").unwrap();
    std::fs::write(fixture.root.join("ignored"), "unique ignored data").unwrap();
    std::fs::write(fixture.root.join(".hidden"), "hidden data").unwrap();
    std::fs::write(fixture.root.join("staged"), "index version").unwrap();
    git(&fixture.root, &["add", "staged"]);
    std::fs::write(fixture.root.join("staged"), "working version").unwrap();
    symlink("/outside/not-followed", fixture.root.join("link")).unwrap();
    std::fs::hard_link(fixture.root.join("ignored"), fixture.root.join("hardlink")).unwrap();
    let started = fixture.begin(false);
    let captured = fixture
        .service
        .inventory_closeout(started.operation, started.revision)
        .unwrap();
    let page = fixture
        .service
        .closeout_inventory(
            captured.operation,
            captured.inventory_digest.as_ref().unwrap(),
            0,
            200,
        )
        .unwrap();
    assert!(page.entries.iter().any(
        |e| e.path == Path::new("ignored") && e.sha256 == Some(digest(b"unique ignored data"))
    ));
    assert!(
        page.entries
            .iter()
            .any(|e| e.path == Path::new(".git/index"))
    );
    assert!(page.entries.iter().any(|e| e.path == Path::new(".hidden")));
    assert!(page.entries.iter().any(|e| e.path == Path::new("link")
        && e.link_target.as_deref() == Some(Path::new("/outside/not-followed"))));
    assert!(
        page.entries
            .iter()
            .any(|e| e.path == Path::new("hardlink") && e.links == 2)
    );
    assert!(
        !page
            .entries
            .iter()
            .any(|e| e.path != Path::new("link") && e.path.starts_with("link"))
    );
    let item = page
        .entries
        .iter()
        .find(|e| e.path == Path::new("ignored"))
        .unwrap();
    let decision = CloseoutDecision {
        entry: item.id.clone(),
        disposition: CloseoutDisposition::Preserve,
        recorded_by: "fixture-agent".into(),
    };
    assert_eq!(
        fixture
            .service
            .record_closeout_disposition(captured.operation, started.revision, decision.clone())
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    let updated = fixture
        .service
        .record_closeout_disposition(captured.operation, captured.revision, decision)
        .unwrap();
    assert_eq!(updated.stage, CloseoutStage::NeedsDecision);
    assert!(!updated.spec.conditional_no_loss);
    std::fs::write(fixture.root.join("ignored"), "later data").unwrap();
    let rescan = fixture
        .service
        .inventory_closeout(updated.operation, updated.revision)
        .unwrap();
    assert_ne!(rescan.inventory_digest, captured.inventory_digest);
    assert_eq!(
        fixture
            .service
            .closeout_inventory(
                rescan.operation,
                captured.inventory_digest.as_ref().unwrap(),
                0,
                20
            )
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert!(
        load(&fixture.service.connection().unwrap(), rescan.operation)
            .unwrap()
            .decisions
            .is_empty()
    );
}

#[test]
fn replaced_root_and_inside_preservation_are_not_authorized() {
    let fixture = Fixture::new();
    let mut spec = fixture.spec(false);
    spec.preservation_directory = Some(fixture.root.clone());
    assert_eq!(
        fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                spec
            )
            .unwrap_err()
            .code,
        IssueCode::InvalidInput
    );
    let started = fixture.begin(false);
    std::fs::rename(&fixture.root, fixture.root.with_extension("original")).unwrap();
    std::fs::create_dir(&fixture.root).unwrap();
    std::fs::write(fixture.root.join("replacement"), "must survive").unwrap();
    assert!(
        fixture
            .service
            .inventory_closeout(started.operation, started.revision)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.join("replacement")).unwrap(),
        b"must survive"
    );
}

#[test]
fn interrupted_scan_preserves_last_inventory_and_authorization() {
    let fixture = Fixture::new();
    let started = fixture.begin(true);
    let captured = fixture
        .service
        .inventory_closeout(started.operation, started.revision)
        .unwrap();
    let mut broken = fixture.service.clone();
    broken.fault = Some(std::sync::Arc::new(|stage| {
        if stage == "closeout_inventory_written" {
            Err(io("fixture disk/checkpoint failure"))
        } else {
            Ok(())
        }
    }));
    assert!(
        broken
            .inventory_closeout(captured.operation, captured.revision)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .inspect_closeout(captured.operation)
            .unwrap(),
        captured
    );
}

#[test]
fn catalog_restore_cannot_reactivate_historical_conditional_authority() {
    for interrupted in [false, true] {
        let fixture = Fixture::new();
        let mut started = fixture.begin(true);
        if interrupted {
            started = fixture
                .service
                .fence_closeout(started.operation, started.revision)
                .unwrap();
            let mut stored =
                load(&fixture.service.connection().unwrap(), started.operation).unwrap();
            stored.record.stage = CloseoutStage::RecoveryRequired;
            stored.record.authorization = Some(CloseoutAuthorization {
                review: ReviewId::new(),
                seal: "synthetic prior authorization".into(),
                source: CloseoutAuthorizationSource::Conditional {
                    session: "fixture-agent".into(),
                    assessment: "synthetic assessment".into(),
                },
            });
            save(&fixture.service.connection().unwrap(), &stored).unwrap();
        }
        let snapshot = fixture
            .service
            .backup(RequestId::new(), "pending-closeout".into())
            .unwrap();
        fixture
            .service
            .revoke_closeout(
                &fixture.client,
                RequestId::new(),
                started.operation,
                started.revision,
            )
            .unwrap();
        let review = fixture.service.review_restore(snapshot.id).unwrap();
        fixture
            .service
            .apply_restore(RequestId::new(), review.id)
            .unwrap();
        let restored = fixture.service.inspect_closeout(started.operation).unwrap();
        assert_eq!(restored.stage, CloseoutStage::RecoveryRequired);
        assert!(!restored.spec.conditional_no_loss);
        assert!(restored.authorization.is_none());
        assert_eq!(
            fixture
                .service
                .inventory_closeout(restored.operation, restored.revision)
                .unwrap_err()
                .code,
            IssueCode::RecoveryRequired
        );
        assert!(fixture.root.exists());
    }
}

#[test]
fn closing_fence_blocks_aliases_and_ancestor_reads_but_retains_admitted_work() {
    let fixture = Fixture::new();
    let started = fixture.begin(false);
    let alias = fixture._directory.path().join("checkout-alias");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let admitted = fixture
        .service
        .acquire_location_use(Some(&alias), &[])
        .unwrap();
    let launch = fixture
        .service
        .prepare_primary_location(
            Placement::Checkout(fixture.location),
            Some(&fixture.root),
            OperationId::new(),
        )
        .unwrap();
    assert_eq!(launch.root, fixture.location);
    drop(launch);
    assert_eq!(
        fixture
            .service
            .acquire_root(fixture.location)
            .err()
            .unwrap()
            .code,
        IssueCode::Busy
    );
    let fenced = fixture
        .service
        .fence_closeout(started.operation, started.revision)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .acquire_location_use(Some(&alias), &[])
            .err()
            .unwrap()
            .code,
        IssueCode::LiveWork
    );
    assert_eq!(
        fixture
            .service
            .acquire_location_use(None, &[fixture._directory.path().to_path_buf()])
            .err()
            .unwrap()
            .code,
        IssueCode::LiveWork
    );
    assert!(fixture.service.acquire_location_use(None, &[]).is_ok());
    assert_eq!(
        fixture
            .service
            .acquire_root(fixture.location)
            .err()
            .unwrap()
            .code,
        IssueCode::Busy
    );
    drop(admitted);
    drop(fixture.service.acquire_root(fixture.location).unwrap());
    fixture
        .service
        .revoke_closeout(
            &fixture.client,
            RequestId::new(),
            fenced.operation,
            fenced.revision,
        )
        .unwrap();
    assert!(
        fixture
            .service
            .acquire_location_use(Some(&fixture.root), &[])
            .is_ok()
    );
}

#[test]
fn stale_authorization_can_be_revoked_without_reopening_the_rebound_location() {
    let fixture = Fixture::new();
    let started = fixture.begin(true);
    let Entity::Location(prior) = fixture
        .service
        .inspect(EntityId::Location(fixture.location))
        .unwrap()
    else {
        panic!()
    };
    let relocated = fixture.root.with_file_name("relocated");
    std::fs::rename(&fixture.root, &relocated).unwrap();
    let relocated = relocated.canonicalize().unwrap();
    change(
        &fixture.service,
        OrganizationChange::RebindLocation {
            location: fixture.location,
            expected_old_path: prior.observed_path,
            expected_generation: prior.binding_generation,
            new_path: relocated.clone(),
        },
    );
    assert_eq!(
        fixture
            .service
            .inventory_closeout(started.operation, started.revision)
            .unwrap_err()
            .code,
        IssueCode::ReplacedRoot
    );
    let revoked = fixture
        .service
        .revoke_closeout(
            &fixture.client,
            RequestId::new(),
            started.operation,
            started.revision,
        )
        .unwrap();
    assert_eq!(revoked.stage, CloseoutStage::Revoked);
    let Entity::Location(location) = fixture
        .service
        .inspect(EntityId::Location(fixture.location))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(location.observed_path, relocated);
    assert_eq!(location.binding_generation, 2);
    let mut spec = fixture.spec(false);
    spec.expected_generation = 2;
    fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            spec,
        )
        .unwrap();
    assert!(relocated.is_dir());
}

#[test]
fn new_top_level_data_invalidates_the_complete_inventory() {
    let fixture = Fixture::new();
    let started = fixture.begin(false);
    let record = fixture
        .service
        .inventory_closeout(started.operation, started.revision)
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    inventory::verify_source(&stored).unwrap();
    std::fs::write(fixture.root.join("new-important-data"), "must survive").unwrap();
    assert_eq!(
        inventory::verify_source(&stored).unwrap_err().code,
        IssueCode::Conflict
    );
    assert!(fixture.root.join("new-important-data").exists());
}

#[test]
fn explicit_directory_retention_overrides_archive_defaults() {
    for full_archive in [false, true] {
        let fixture = Fixture::new();
        let mut spec = fixture.spec(false);
        spec.full_archive = full_archive;
        let started = fixture
            .service
            .begin_closeout(
                &fixture.client,
                RequestId::new(),
                fixture.service.status().unwrap().revision,
                spec,
            )
            .unwrap();
        let inventoried = fixture
            .service
            .inventory_closeout(started.operation, started.revision)
            .unwrap();
        let page = fixture
            .service
            .closeout_inventory(
                started.operation,
                inventoried.inventory_digest.as_ref().unwrap(),
                0,
                200,
            )
            .unwrap();
        let first = page.entries.first().unwrap();
        assert_eq!(first.kind, CloseoutEntryKind::Directory);
        fixture
            .service
            .record_closeout_disposition(
                started.operation,
                inventoried.revision,
                CloseoutDecision {
                    entry: first.id.clone(),
                    disposition: CloseoutDisposition::Retain {
                        reason: "Keep this fixture's directory in place".into(),
                    },
                    recorded_by: "fixture".into(),
                },
            )
            .unwrap();
        let stored = load(&fixture.service.connection().unwrap(), started.operation).unwrap();
        let destination = fixture._directory.path().join("preservation");
        storage::private_dir(&destination).unwrap();
        assert_eq!(
            files::preserve(&stored, &destination).unwrap_err().code,
            IssueCode::PreservationIncomplete
        );
        // The first inventory entry is the root itself. Capture creates its
        // empty container before evaluating dispositions, but must copy no data.
        assert_eq!(
            std::fs::read_dir(destination.join("files"))
                .unwrap()
                .count(),
            0
        );
        assert!(fixture.root.join(&first.path).is_dir());
    }
}

#[tokio::test]
async fn inventory_does_not_execute_repository_configured_filters() {
    let fixture = Fixture::new();
    let marker = fixture._directory.path().join("filter-executed");
    std::fs::write(
        fixture.root.join(".gitattributes"),
        "filtered filter=fixture\n",
    )
    .unwrap();
    std::fs::write(fixture.root.join("filtered"), "before").unwrap();
    git(&fixture.root, &["add", "."]);
    git(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "filtered file",
        ],
    );
    git(
        &fixture.root,
        &[
            "config",
            "filter.fixture.clean",
            &format!("sh -c 'echo executed > {}; cat'", marker.display()),
        ],
    );
    git(
        &fixture.root,
        &["config", "filter.fixture.required", "true"],
    );
    std::fs::write(fixture.root.join("filtered"), "different worktree bytes").unwrap();
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    assert!(
        !marker.exists(),
        "Closeout must never invoke an adopted checkout's clean/process filters"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("filtered")).unwrap(),
        b"different worktree bytes"
    );
    finish_capture(&capture);
}

struct FixtureProcess(std::process::Child);
impl Drop for FixtureProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn work_review_observes_external_file_and_cwd_without_stopping_them() {
    let fixture = Fixture::new();
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    std::fs::write(fixture.root.join("tracked"), "open-file fixture").unwrap();
    let file = std::fs::File::open(fixture.root.join("tracked")).unwrap();
    let mut process = FixtureProcess(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&fixture.root)
            .spawn()
            .unwrap(),
    );
    let pid = process.0.id();
    let result = fixture
        .service
        .prepare_closeout_work(
            started.operation,
            started.revision,
            &fixture._directory.path().join("sessions-state"),
            &execution,
            &fixture.root,
            &capture,
        )
        .await;
    let still_running = process.0.try_wait().unwrap().is_none();
    drop(process);
    drop(file);
    let (record, report) = result.unwrap();
    assert!(still_running, "Review must not cancel observed work");
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.kind == CloseoutWorkKind::Executor)
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.kind == CloseoutWorkKind::ExternalProcess
                && f.identity.starts_with(&format!("{pid}:"))),
        "{report:?}"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.kind == CloseoutWorkKind::ExternalProcess
                && f.identity.starts_with(&format!("{}:", std::process::id()))),
        "{report:?}"
    );
    let (_, quiet) = fixture
        .service
        .prepare_closeout_work(
            record.operation,
            record.revision,
            &fixture._directory.path().join("sessions-state"),
            &execution,
            fixture._directory.path(),
            &capture,
        )
        .await
        .unwrap();
    assert!(quiet.findings.is_empty(), "{quiet:?}");
    finish_capture(&capture);
}

fn capture(fixture: &Fixture, operation: OperationId) -> crate::execution::Capture {
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, StorageConfig,
    };
    let store = ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let invocation = Invocation {
        session_id: "workspace".into(),
        message_id: operation.to_string(),
        call_path: vec![RequestId::new().to_string()],
        tool: "workspace_closeout".into(),
        input: serde_json::json!({"operation":operation}),
        working_dir: None,
        received_result_digest: None,
    };
    let PreparedInvocation::New(run) = store.prepare(&invocation, "fixture").unwrap() else {
        panic!()
    };
    store.start(&run.id, "fixture").unwrap();
    Capture::create(store, run, StorageConfig::default()).unwrap()
}

#[tokio::test]
async fn bundle_restores_acquired_refs_detached_stash_and_reflog_without_source() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "user.name", "Fixture"]);
    git(
        &fixture.root,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(fixture.root.join("tracked"), "first").unwrap();
    git(&fixture.root, &["add", "tracked"]);
    git(&fixture.root, &["commit", "-qm", "first"]);
    git(
        &fixture.root,
        &[
            "update-ref",
            "refs/jcode/checkout-acquired/prior/feature",
            "HEAD",
        ],
    );
    let branch = git_text(&fixture.root, &["branch", "--show-current"]);
    git(&fixture.root, &["checkout", "-qb", "squash-feature"]);
    std::fs::write(
        fixture.root.join("squash-info"),
        "squash-integrated information",
    )
    .unwrap();
    git(&fixture.root, &["add", "squash-info"]);
    git(&fixture.root, &["commit", "-qm", "original feature"]);
    let squashed = git_text(&fixture.root, &["rev-parse", "HEAD"]);
    git(&fixture.root, &["checkout", "-q", branch.trim()]);
    git(&fixture.root, &["merge", "--squash", "squash-feature"]);
    git(&fixture.root, &["commit", "-qm", "squash integration"]);
    assert_eq!(
        std::process::Command::new("git")
            .current_dir(&fixture.root)
            .args(["merge-base", "--is-ancestor", squashed.trim(), "HEAD"])
            .status()
            .unwrap()
            .code(),
        Some(1)
    );
    git(
        &fixture.root,
        &["commit", "--allow-empty", "-qm", "reflog-only"],
    );
    let lost = git_text(&fixture.root, &["rev-parse", "HEAD"]);
    git(&fixture.root, &["reset", "--soft", "HEAD~1"]);
    std::fs::write(fixture.root.join("tracked"), "stash data").unwrap();
    git(&fixture.root, &["stash", "push", "-qm", "fixture"]);
    git(&fixture.root, &["checkout", "--detach", "-q"]);
    git(
        &fixture.root,
        &["commit", "--allow-empty", "-qm", "detached"],
    );
    let detached = git_text(&fixture.root, &["rev-parse", "HEAD"]);
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let record = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].reflog.contains(lost.trim()));
    assert!(
        snapshots[0]
            .refs
            .contains_key("refs/jcode/checkout-acquired/prior/feature")
    );
    let source_refs = git_text(&fixture.root, &["show-ref"]);
    let bundle = super::git::preserve(
        &fixture.service,
        record.operation,
        &snapshots[0],
        &fixture._directory.path().join("preserved"),
        &capture,
        &archive::Archive::open(fixture._directory.path()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(source_refs, git_text(&fixture.root, &["show-ref"]));
    std::fs::rename(&fixture.root, fixture.root.with_extension("offline")).unwrap();
    let restored = bundle.parent().unwrap().join("restored.git");
    git(&restored, &["fsck", "--full"]);
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", squashed.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(
            &restored,
            &["show", &format!("{}:squash-info", squashed.trim())]
        ),
        "squash-integrated information"
    );
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", lost.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", detached.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(&restored, &["show", "refs/stash:tracked"]).trim(),
        "stash data"
    );
    assert!(!restored.join("objects/info/alternates").exists());
    finish_capture(&capture);
}

fn finish_capture(capture: &crate::execution::Capture) {
    use jcode_tool_core::OutputCapture;
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
    capture
        .seal(output, crate::execution::RunState::Completed)
        .unwrap();
}

fn git_text(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn full_archive_restores_ignored_files_symlinks_hardlinks_and_metadata() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join(".gitignore"), "ignored\n").unwrap();
    let file = fixture.root.join("ignored");
    std::fs::write(&file, vec![91; 300_000]).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.fixture", "preserved"])
            .arg(&file)
            .status()
            .unwrap()
            .success()
    );
    std::fs::hard_link(&file, fixture.root.join("second-link")).unwrap();
    symlink("ignored", fixture.root.join("symlink")).unwrap();
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
    let preserved = fixture
        .service
        .preserve_closeout(refreshed.operation, refreshed.revision, &capture)
        .await
        .unwrap();
    assert_eq!(preserved.stage, CloseoutStage::NeedsDecision);
    assert!(preserved.preservation_digest.is_some());
    let stored = load(&fixture.service.connection().unwrap(), preserved.operation).unwrap();
    let restored = stored
        .preservation
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join("verified-restore");
    assert_eq!(
        std::fs::read(restored.join("ignored")).unwrap(),
        vec![91; 300_000]
    );
    assert_eq!(
        std::fs::read_link(restored.join("symlink")).unwrap(),
        PathBuf::from("ignored")
    );
    assert_eq!(
        std::fs::metadata(restored.join("ignored")).unwrap().ino(),
        std::fs::metadata(restored.join("second-link"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        std::fs::metadata(restored.join("ignored")).unwrap().mode() & 0o777,
        0o750
    );
    let xattr = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", "jcode.fixture"])
        .arg(restored.join("ignored"))
        .output()
        .unwrap();
    assert!(xattr.status.success());
    assert_eq!(xattr.stdout, b"preserved\n");
    assert!(fixture.root.join("ignored").exists());
    finish_capture(&capture);
}

#[test]
fn external_reference_retains_original_metadata_and_verified_local_restoration() {
    use std::io::BufRead;
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let source = fixture.root.join("important");
    std::fs::write(&source, "original information").unwrap();
    let external = fixture._directory.path().join("existing-copy");
    std::fs::write(&external, "original information").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o750)).unwrap();
    std::fs::write(source.join("..namedfork/rsrc"), vec![37u8; 200_000]).unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity"
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.fixture", "source-only-metadata"])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let record = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            fixture.spec(false),
        )
        .unwrap();
    let record = fixture
        .service
        .inventory_closeout(record.operation, record.revision)
        .unwrap();
    let mut stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let mut decisions = BTreeMap::new();
    inventory::visit(&stored, |item| {
        if item.entry.kind != CloseoutEntryKind::Directory {
            let disposition = if item.entry.path == Path::new("important") {CloseoutDisposition::Preserved {path:external.clone()}} else {CloseoutDisposition::Redundant {reason:"synthetic fixture excludes unrelated Git internals from this file-copy test".into()}};
            decisions.insert(item.entry.id.clone(), CloseoutDecision {entry:item.entry.id, disposition, recorded_by:"fixture".into()});
        }
        Ok(())
    }).unwrap();
    stored.decisions = decisions;
    let archive = fixture._directory.path().join("archive");
    std::fs::create_dir(&archive).unwrap();
    let manifest = files::preserve(&stored, &archive).unwrap();
    let restored = archive.join("verified-restore/important");
    let value = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", "jcode.fixture"])
        .arg(&restored)
        .output()
        .unwrap();
    assert!(
        value.status.success(),
        "{}",
        String::from_utf8_lossy(&value.stderr)
    );
    assert_eq!(value.stdout, b"source-only-metadata\n");
    let saved = std::io::BufReader::new(std::fs::File::open(manifest).unwrap())
        .lines()
        .map(|line| decode::<files::PreservedItem>(&line.unwrap()).unwrap())
        .find(|item| item.item.entry.path == Path::new("important"))
        .unwrap();
    assert!(saved.saved.as_ref().unwrap().starts_with(&archive));
    std::fs::remove_file(&external).unwrap();
    files::verify_saved(&saved, saved.saved.as_ref().unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&restored).unwrap(),
        "original information"
    );
    assert_eq!(
        std::fs::read(restored.join("..namedfork/rsrc")).unwrap(),
        vec![37u8; 200_000]
    );
    assert_eq!(
        std::fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.fixture", "changed"])
            .arg(&restored)
            .status()
            .unwrap()
            .success()
    );
    files::verify_copy(&saved.item, &restored).unwrap();
    assert_eq!(
        files::verify_saved(&saved, &restored).unwrap_err().code,
        IssueCode::PreservationIncomplete
    );
    assert!(
        std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "jcode.fixture", "source-only-metadata"])
            .arg(&restored)
            .status()
            .unwrap()
            .success()
    );
    files::verify_saved(&saved, &restored).unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&restored)
            .status()
            .unwrap()
            .success()
    );
    files::verify_copy(&saved.item, &restored).unwrap();
    assert_eq!(
        files::verify_saved(&saved, &restored).unwrap_err().code,
        IssueCode::PreservationIncomplete
    );
    assert!(source.exists());
}

#[tokio::test]
async fn git_restores_pseudoref_and_unreferenced_graph_roots_without_source() {
    let fixture = Fixture::new();
    let tree = git_text(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
    let lost = git_text(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit-tree",
            tree.trim(),
            "-m",
            "unique original operation",
        ],
    );
    std::fs::write(fixture.root.join(".git/ORIG_HEAD"), &lost).unwrap();
    let input = fixture._directory.path().join("object-source");
    std::fs::write(&input, "unique loose object").unwrap();
    let blob = git_text(
        &fixture.root,
        &["hash-object", "-w", input.to_str().unwrap()],
    );
    std::fs::create_dir(fixture.root.join(".git/saved-operation")).unwrap();
    std::fs::write(fixture.root.join(".git/saved-operation/object"), &blob).unwrap();
    let before_refs = git_text(&fixture.root, &["show-ref"]);
    let record = fixture
        .service
        .begin_closeout(
            &fixture.client,
            RequestId::new(),
            fixture.service.status().unwrap().revision,
            fixture.spec(false),
        )
        .unwrap();
    let capture = capture(&fixture, record.operation);
    let record = fixture
        .service
        .refresh_closeout(record.operation, record.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), record.operation).unwrap();
    let snapshots: Vec<git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    assert!(!snapshots[0].reflog.contains(lost.trim()));
    let archive = fixture._directory.path().join("git-preservation");
    std::fs::create_dir(&archive).unwrap();
    let bundle = git::preserve(
        &fixture.service,
        record.operation,
        &snapshots[0],
        &archive,
        &capture,
        &archive::Archive::open(&archive).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(git_text(&fixture.root, &["show-ref"]), before_refs);
    std::fs::rename(
        &fixture.root,
        fixture._directory.path().join("source-offline"),
    )
    .unwrap();
    let restored = bundle.parent().unwrap().join("restored.git");
    assert_eq!(
        git_text(&restored, &["cat-file", "-t", lost.trim()]).trim(),
        "commit"
    );
    assert_eq!(
        git_text(&restored, &["cat-file", "-p", blob.trim()]).trim(),
        "unique loose object"
    );
    git_text(&restored, &["fsck", "--full", "--no-reflogs"]);
    finish_capture(&capture);
}

#[tokio::test]
async fn historical_lfs_payloads_restore_and_missing_payload_blocks_preservation() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "user.name", "Fixture"]);
    git(
        &fixture.root,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(
        fixture.root.join(".gitattributes"),
        "asset filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    let mut objects = Vec::new();
    for data in [
        b"older-lfs-content".as_slice(),
        b"newer-lfs-content".as_slice(),
    ] {
        let oid = digest(data);
        let path = fixture
            .root
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(&oid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
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
        git(&fixture.root, &["commit", "-qm", "lfs-version"]);
        objects.push((oid, path, data.to_vec()));
    }
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let refreshed = fixture
        .service
        .refresh_closeout(started.operation, started.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), refreshed.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    let bundle = super::git::preserve(
        &fixture.service,
        refreshed.operation,
        &snapshots[0],
        &fixture._directory.path().join("history"),
        &capture,
        &archive::Archive::open(fixture._directory.path()).unwrap(),
    )
    .await
    .unwrap();
    for (oid, _, data) in &objects {
        assert_eq!(
            std::fs::read(bundle.parent().unwrap().join("lfs").join(oid)).unwrap(),
            *data
        );
        assert_eq!(
            std::fs::read(
                bundle
                    .parent()
                    .unwrap()
                    .join("restored.git/lfs/objects")
                    .join(&oid[..2])
                    .join(&oid[2..4])
                    .join(oid)
            )
            .unwrap(),
            *data
        );
    }
    std::fs::remove_file(&objects[0].1).unwrap();
    let error = super::git::preserve(
        &fixture.service,
        refreshed.operation,
        &snapshots[0],
        &fixture._directory.path().join("missing-history"),
        &capture,
        &archive::Archive::open(fixture._directory.path()).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, IssueCode::Conflict);
    let current = fixture
        .service
        .refresh_closeout(refreshed.operation, refreshed.revision, &capture)
        .await
        .unwrap();
    let stored = load(&fixture.service.connection().unwrap(), current.operation).unwrap();
    let snapshots: Vec<super::git::RepositorySnapshot> =
        storage::read_json(stored.history.as_ref().unwrap()).unwrap();
    let error = super::git::preserve(
        &fixture.service,
        current.operation,
        &snapshots[0],
        &fixture._directory.path().join("missing-history-fresh"),
        &capture,
        &archive::Archive::open(fixture._directory.path()).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, IssueCode::PreservationIncomplete);
    assert!(fixture.root.exists());
    finish_capture(&capture);
}
