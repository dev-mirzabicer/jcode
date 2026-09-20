use super::*;

#[test]
#[cfg(target_os = "macos")]
fn primary_launch_journal_reconciles_checkpoint_without_duplicate_publication() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => crate::env::set_var(key, value),
                    None => crate::env::remove_var(key),
                }
            }
        }
    }
    let _env = Environment(vec![
        ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
        ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
    ]);
    crate::env::set_var("JCODE_HOME", temp.path().join("state"));
    crate::env::set_var("JCODE_RUNTIME_DIR", temp.path().join("runtime"));
    let service = WorkspaceService::new(&crate::storage::durable_state_dir());
    service.initialize(RequestId::new()).unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let review = service
        .review_organization_change(
            0,
            OrganizationChange::RegisterLocation {
                name: "fixture".into(),
                path: work.clone(),
                registration: Registration::Standalone,
            },
        )
        .unwrap();
    let receipt = service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap();
    let EntityId::Location(root) = receipt.targets[0] else {
        panic!("location")
    };
    let input = PrimaryLaunchInput {
        placement: PrimaryPlacement::Existing {
            placement: Placement::Standalone(root),
        },
        cwd: Some(PrimaryCwd::Existing { path: work.clone() }),
        agent: None,
        model: None,
        selfdev: false,
    };
    let model = PrimaryModel {
        model: "synthetic".into(),
        provider: "fixture".into(),
        api_method: "fixture".into(),
        effort: None,
    };
    for stage in ["primary_before_index", "primary_index_committed"] {
        let request = RequestId::new();
        let record = service
            .reserve_primary_launch(
                request,
                service.status().unwrap().revision,
                input.clone(),
                model.clone(),
            )
            .unwrap();
        let peers = (0..4)
            .map(|_| {
                let service = service.clone();
                let input = input.clone();
                let model = model.clone();
                let revision = record.reviewed_revision;
                std::thread::spawn(move || {
                    service
                        .reserve_primary_launch(request, revision, input, model)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for peer in peers {
            assert_eq!(peer.join().unwrap(), record);
        }
        let mut changed_default = model.clone();
        changed_default.model = "changed-default".into();
        assert_eq!(
            service
                .reserve_primary_launch(
                    request,
                    record.reviewed_revision,
                    input.clone(),
                    changed_default
                )
                .unwrap(),
            record
        );
        let mut different = input.clone();
        different.agent = Some("different".into());
        assert_eq!(
            service
                .reserve_primary_launch(request, record.reviewed_revision, different, model.clone())
                .unwrap_err()
                .code,
            IssueCode::Conflict
        );
        assert!(!crate::session::session_exists(&record.session));
        let pending_snapshot = service
            .backup(RequestId::new(), format!("pending-{stage}"))
            .unwrap();
        service
            .record_primary_launch_failure(request, "synthetic failure before restore".into())
            .unwrap();
        let restore = service.review_restore(pending_snapshot.id).unwrap();
        service.apply_restore(RequestId::new(), restore.id).unwrap();
        assert_eq!(
            service.inspect_primary_launch(request).unwrap().state,
            PrimaryLaunchState::RecoveryRequired
        );
        let failed = service
            .record_primary_launch_failure(request, "synthetic preparation failure".into())
            .unwrap();
        assert_eq!(failed.state, PrimaryLaunchState::Failed);
        assert_eq!(failed.session, record.session);
        let lease = service.primary_launch_lease(request).unwrap();
        assert!(service.primary_launch_lease(request).is_err());
        drop(lease);
        let prepared = service
            .prepare_primary_location(Placement::Standalone(root), Some(&work), record.operation)
            .unwrap();
        let mut session =
            crate::session::Session::create_with_id(record.session.clone(), None, None);
        session.working_dir = Some(
            prepared
                .location
                .cwd
                .observed_path()
                .to_str()
                .unwrap()
                .into(),
        );
        session.location = Some(prepared.location.clone());
        session.primary_creation = Some(crate::session::StoredPrimaryCreation {
            request,
            operation: record.operation,
            ready: false,
        });
        session.save().unwrap();
        drop(prepared);
        assert_eq!(
            service
                .record_primary_launch_failure(request, "synthetic checkpoint interruption".into())
                .unwrap()
                .state,
            PrimaryLaunchState::RecoveryRequired
        );
        assert!(session.require_published_primary().is_err());
        assert_eq!(
            service.reconcile_primary_launch(request).unwrap_err().code,
            IssueCode::RecoveryRequired
        );
        session.primary_creation.as_mut().unwrap().ready = true;
        session.save().unwrap();
        let source = std::fs::read(crate::session::session_path(&session.id).unwrap()).unwrap();
        assert!(session.require_published_primary().is_err());
        let mut interrupted = service.clone();
        interrupted.fault = Some(std::sync::Arc::new(move |observed| {
            if observed == stage {
                Err(io("synthetic interruption"))
            } else {
                Ok(())
            }
        }));
        assert!(interrupted.reconcile_primary_launch(request).is_err());
        let completed = service.reconcile_primary_launch(request).unwrap();
        assert_eq!(completed.state, PrimaryLaunchState::Complete);
        assert!(!completed.backup_pending, "{completed:?}");
        assert_eq!(completed.session, record.session);
        assert!(completed.published_revision.unwrap() > completed.reviewed_revision);
        assert_eq!(
            service
                .record_primary_launch_failure(request, "late transport error".into())
                .unwrap(),
            completed
        );
        session.require_published_primary().unwrap();
        assert_eq!(
            service.reconcile_primary_launch(request).unwrap(),
            completed
        );
        assert_eq!(
            std::fs::read(crate::session::session_path(&session.id).unwrap()).unwrap(),
            source
        );
        assert_eq!(
            service
                .sessions(None, None, 100)
                .unwrap()
                .iter()
                .filter(|index| index.session == record.session)
                .count(),
            1
        );
        // A launch retry is a historical receipt, not a later location rewrite.
        let published_snapshot = service
            .backup(RequestId::new(), format!("published-{stage}"))
            .unwrap();
        session.location.as_mut().unwrap().revision += 1;
        session.location.as_mut().unwrap().last_operation = Some(OperationId::new());
        session.save().unwrap();
        assert_eq!(
            service.reconcile_primary_launch(request).unwrap(),
            completed
        );
        let moved_source =
            std::fs::read(crate::session::session_path(&session.id).unwrap()).unwrap();
        let restore = service.review_restore(published_snapshot.id).unwrap();
        service.apply_restore(RequestId::new(), restore.id).unwrap();
        assert_eq!(
            service.inspect_primary_launch(request).unwrap().state,
            PrimaryLaunchState::Complete
        );
        assert!(
            !service
                .sessions(None, None, 100)
                .unwrap()
                .into_iter()
                .find(|index| index.session == session.id)
                .unwrap()
                .reconciled
        );
        session.require_published_primary().unwrap();
        let index = service
            .sessions(None, None, 100)
            .unwrap()
            .into_iter()
            .find(|index| index.session == session.id)
            .unwrap();
        assert_eq!(
            index.session_revision,
            session.location.as_ref().unwrap().revision
        );
        assert_eq!(
            Some(index.operation),
            session.location.as_ref().unwrap().last_operation
        );
        assert_eq!(
            std::fs::read(crate::session::session_path(&session.id).unwrap()).unwrap(),
            moved_source
        );
    }
}
