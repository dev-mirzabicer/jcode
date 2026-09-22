use super::*;
use crate::session::Session;

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Drop for Environment {
    fn drop(&mut self) {
        crate::config::Config::invalidate_cache();
        for (key, value) in &self.0 {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
    }
}

struct Fixture {
    _environment: Environment,
    _temporary: tempfile::TempDir,
    service: WorkspaceService,
    project: ProjectId,
    area: WorkAreaId,
    a: LocationId,
    b: LocationId,
    outside: LocationId,
    session: Session,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let environment = Environment(vec![
            ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
            ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
        ]);
        crate::env::set_var("JCODE_HOME", temporary.path().join("home"));
        crate::env::set_var("JCODE_RUNTIME_DIR", temporary.path().join("runtime"));
        crate::config::Config::invalidate_cache();
        let service = WorkspaceService::new(&temporary.path().join("state"));
        service.initialize(RequestId::new()).unwrap();
        let project = match change(
            &service,
            OrganizationChange::CreateProject {
                name: "project".into(),
            },
        )
        .targets[0]
        {
            EntityId::Project(id) => id,
            _ => panic!(),
        };
        let area = match change(
            &service,
            OrganizationChange::CreateWorkArea {
                project,
                name: "area".into(),
            },
        )
        .targets[0]
        {
            EntityId::WorkArea(id) => id,
            _ => panic!(),
        };
        let a = register(
            &service,
            &temporary.path().join("a"),
            Registration::Directory {
                home: Home::WorkArea(area),
            },
        );
        let b = register(
            &service,
            &temporary.path().join("b"),
            Registration::Directory {
                home: Home::Project(project),
            },
        );
        let outside = register(
            &service,
            &temporary.path().join("outside"),
            Registration::Standalone,
        );
        let prepared = service
            .prepare_primary_location(
                Placement::Directory(a),
                Some(&temporary.path().join("a")),
                OperationId::new(),
            )
            .unwrap();
        let mut session =
            Session::create_with_id(format!("session_scope_{}", RequestId::new()), None, None);
        session.working_dir = Some(
            prepared
                .location
                .cwd
                .observed_path()
                .to_string_lossy()
                .into(),
        );
        session.location = Some(prepared.location);
        Self {
            _environment: environment,
            _temporary: temporary,
            service,
            project,
            area,
            a,
            b,
            outside,
            session,
        }
    }
    fn scope(&self) -> SessionWriteScope {
        self.service.session_write_scope(&self.session).unwrap()
    }
    fn issue(&self, audience: Audience, target: WriteTarget) -> PermissionMutation {
        apply(
            &self.service,
            GrantChange::Issue {
                audience,
                target,
                proposal: None,
            },
        )
    }
}
fn change(service: &WorkspaceService, change: OrganizationChange) -> Receipt {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
}
fn register(service: &WorkspaceService, path: &Path, registration: Registration) -> LocationId {
    std::fs::create_dir(path).unwrap();
    match change(
        service,
        OrganizationChange::RegisterLocation {
            name: path.file_name().unwrap().to_string_lossy().into(),
            path: path.into(),
            registration,
        },
    )
    .targets[0]
    {
        EntityId::Location(id) => id,
        _ => panic!(),
    }
}
fn apply(service: &WorkspaceService, change: GrantChange) -> PermissionMutation {
    let review = service
        .review_grant_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_grant_change(
            &WorkspaceClientAuthority::authenticated("fixture-human-client").unwrap(),
            RequestId::new(),
            review.id,
        )
        .unwrap()
}
fn writable(scope: &SessionWriteScope, id: LocationId) -> bool {
    scope
        .roots
        .iter()
        .any(|root| root.location.id == id && root.issue.is_none())
}

#[test]
#[cfg(target_os = "macos")]
fn scope_follows_placement_current_members_and_explicit_audiences_not_cwd() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    let cwd = f.session.working_dir.clone();
    assert!(writable(&f.scope(), f.a));
    assert!(!writable(&f.scope(), f.b));
    f.session.location.as_mut().unwrap().placement = Placement::Project(f.project);
    assert!(writable(&f.scope(), f.a));
    assert!(writable(&f.scope(), f.b));
    f.session.location.as_mut().unwrap().placement = Placement::WorkArea(f.area);
    assert!(writable(&f.scope(), f.a));
    assert!(!writable(&f.scope(), f.b));
    let permission = f.issue(Audience::WorkArea(f.area), WriteTarget::Root(f.b));
    let id = permission.grant.unwrap().id;
    assert!(writable(&f.scope(), f.b));
    f.session.location.as_mut().unwrap().placement = Placement::Directory(f.a);
    assert!(
        writable(&f.scope(), f.b),
        "explicit ancestor audience applies to descendant"
    );
    let later = register(
        &f.service,
        &f._temporary.path().join("later"),
        Registration::Directory {
            home: Home::WorkArea(f.area),
        },
    );
    f.session.location.as_mut().unwrap().placement = Placement::Directory(later);
    assert!(writable(&f.scope(), f.b), "future descendant");
    assert!(
        !writable(&f.scope(), f.a),
        "ordinary ancestor access is not inherited"
    );
    change(
        &f.service,
        OrganizationChange::Archive {
            target: EntityId::Project(f.project),
            archived: true,
        },
    );
    assert!(writable(&f.scope(), f.b));
    change(
        &f.service,
        OrganizationChange::MoveLocation {
            location: later,
            home: Home::Project(f.project),
            associate_repository: false,
        },
    );
    assert!(
        !writable(&f.scope(), f.b),
        "current home removed inherited audience"
    );
    f.session.location.as_mut().unwrap().placement = Placement::Directory(f.a);
    apply(&f.service, GrantChange::Revoke { grant: id });
    assert!(!writable(&f.scope(), f.b));
    assert_eq!(f.session.working_dir, cwd);
    f.issue(Audience::Project(f.project), WriteTarget::Root(f.b));
    // A grant targeted at B does not inherit a grant available to B's audience.
    let definition = GrantDefinition {
        id: GrantId::new(),
        audience: Audience::Session("unrelated".into()),
        target: WriteTarget::Root(f.outside),
        state: GrantState::Active,
        revision: 1,
        copied_from: None,
        authorization: Some(GrantAuthorization {
            client: "fixture".into(),
            request: RequestId::new(),
            installation: f.service.status().unwrap().installation,
        }),
    };
    portable::save_grant(&f.service.connection().unwrap(), &definition).unwrap();
    assert!(!writable(&f.scope(), f.outside));
}

#[test]
#[cfg(target_os = "macos")]
fn grant_review_conflicts_replay_and_checkpoint_faults_are_atomic() {
    let _lock = crate::storage::lock_test_env();
    let f = Fixture::new();
    let client = WorkspaceClientAuthority::authenticated("fixture-human-client").unwrap();
    let make_review = || {
        f.service
            .review_grant_change(
                f.service.status().unwrap().revision,
                GrantChange::Issue {
                    audience: Audience::Project(f.project),
                    target: WriteTarget::Root(f.outside),
                    proposal: None,
                },
            )
            .unwrap()
    };
    let stale = make_review();
    change(
        &f.service,
        OrganizationChange::Rename {
            target: EntityId::Project(f.project),
            name: "renamed".into(),
        },
    );
    assert_eq!(
        f.service
            .apply_grant_change(&client, RequestId::new(), stale.id)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert!(!writable(&f.scope(), f.outside));
    for stage in ["grant_before_commit", "grant_committed"] {
        let review = make_review();
        let request = RequestId::new();
        let mut broken = f.service.clone();
        broken.fault = Some(std::sync::Arc::new(move |point| {
            if point == stage {
                Err(io("synthetic interruption"))
            } else {
                Ok(())
            }
        }));
        assert!(
            broken
                .apply_grant_change(&client, request, review.id)
                .is_err()
        );
        assert_eq!(
            f.service.inspect_grant(review.grant.id).is_ok(),
            stage == "grant_committed"
        );
        let result = f
            .service
            .apply_grant_change(&client, request, review.id)
            .unwrap();
        let snapshots = f.service.snapshots().unwrap();
        assert!(
            snapshots
                .iter()
                .any(|snapshot| snapshot.revision >= result.receipt.revision)
        );
        assert!(writable(&f.scope(), f.outside));
        assert_eq!(
            f.service
                .apply_grant_change(&client, request, review.id)
                .unwrap(),
            result
        );
        assert_eq!(
            f.service.snapshots().unwrap(),
            snapshots,
            "a completed retry does not repeat backup publication"
        );
        let other = make_review();
        assert_eq!(
            f.service
                .apply_grant_change(&client, request, other.id)
                .unwrap_err()
                .code,
            IssueCode::Conflict
        );
        apply(
            &f.service,
            GrantChange::Revoke {
                grant: review.grant.id,
            },
        );
        assert!(!writable(&f.scope(), f.outside));
        assert_eq!(
            f.service
                .apply_grant_change(&client, request, review.id)
                .unwrap(),
            result,
            "retry returns original result, never reactivates revoked grant"
        );
    }
}

#[test]
#[cfg(target_os = "macos")]
fn access_proposal_never_authorizes_and_legacy_definitions_fail_closed() {
    let _lock = crate::storage::lock_test_env();
    let f = Fixture::new();
    let request = RequestId::new();
    let result = f
        .service
        .request_access(
            &f.session,
            request,
            WriteTarget::Root(f.outside),
            "synthetic reason".into(),
        )
        .unwrap();
    assert_eq!(
        result.proposal.as_ref().unwrap().state,
        AccessProposalState::Pending
    );
    assert!(!writable(&f.scope(), f.outside));
    assert_eq!(
        f.service
            .request_access(
                &f.session,
                request,
                WriteTarget::Root(f.outside),
                "synthetic reason".into()
            )
            .unwrap(),
        result
    );
    assert_eq!(
        f.service
            .request_access(
                &f.session,
                request,
                WriteTarget::Root(f.b),
                "synthetic reason".into()
            )
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    let mut legacy = GrantDefinition {
        id: GrantId::new(),
        audience: Audience::Project(f.project),
        target: WriteTarget::Root(f.outside),
        state: GrantState::Active,
        revision: 1,
        copied_from: None,
        authorization: None,
    };
    portable::save_grant(&f.service.connection().unwrap(), &legacy).unwrap();
    assert!(
        !writable(&f.scope(), f.outside),
        "old foundation definition is not authorization"
    );
    let issued = apply(
        &f.service,
        GrantChange::ActivateImported { grant: legacy.id },
    );
    assert!(writable(&f.scope(), f.outside));
    legacy = issued.grant.unwrap();
    assert!(legacy.authorization.is_some());
    let Entity::Location(root) = f.service.inspect(EntityId::Location(f.outside)).unwrap() else {
        panic!()
    };
    std::fs::rename(
        &root.observed_path,
        root.observed_path.with_extension("moved"),
    )
    .unwrap();
    std::fs::create_dir(&root.observed_path).unwrap();
    assert!(
        !writable(&f.scope(), f.outside),
        "replacement bytes cannot inherit permission by spelling"
    );
    assert!(
        f.scope()
            .roots
            .iter()
            .find(|r| r.location.id == f.outside)
            .unwrap()
            .issue
            .is_some()
    );
    // Revocation remains possible even when its target is unavailable.
    apply(&f.service, GrantChange::Revoke { grant: legacy.id });
    assert!(!writable(&f.scope(), f.outside));
}

#[test]
#[cfg(target_os = "macos")]
fn direct_proposal_approval_is_exact_and_survives_reconnect_and_paging() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.save().unwrap();
    let pending = f
        .service
        .request_access(
            &f.session,
            RequestId::new(),
            WriteTarget::Root(f.outside),
            "synthetic".into(),
        )
        .unwrap()
        .proposal
        .unwrap();
    assert!(
        f.service
            .review_grant_change(
                f.service.status().unwrap().revision,
                GrantChange::Issue {
                    audience: Audience::Session(f.session.id.clone()),
                    target: WriteTarget::Root(f.b),
                    proposal: Some(pending.id)
                }
            )
            .is_err()
    );
    let review = f
        .service
        .review_grant_change(
            f.service.status().unwrap().revision,
            GrantChange::Issue {
                audience: Audience::Session(f.session.id.clone()),
                target: pending.target.clone(),
                proposal: Some(pending.id),
            },
        )
        .unwrap();
    let request = RequestId::new();
    let approved = f
        .service
        .apply_grant_change(
            &WorkspaceClientAuthority::authenticated("first-connection").unwrap(),
            request,
            review.id,
        )
        .unwrap();
    assert_eq!(
        approved.proposal.as_ref().unwrap().state,
        AccessProposalState::Approved
    );
    assert!(writable(&f.scope(), f.outside));
    assert_eq!(
        f.service
            .apply_grant_change(
                &WorkspaceClientAuthority::authenticated("reconnected-client").unwrap(),
                request,
                review.id
            )
            .unwrap(),
        approved
    );
    let restored = Session::load_startup_stub(&f.session.id).unwrap();
    assert!(writable(
        &f.service.session_write_scope(&restored).unwrap(),
        f.outside
    ));
    f.session.id = "session_other".into();
    assert!(
        !writable(&f.scope(), f.outside),
        "grant is session-specific"
    );
    f.issue(Audience::Project(f.project), WriteTarget::Root(f.b));
    let query = PermissionQuery::Grants { audience: None };
    let page = f.service.list_permissions(query.clone(), None, 1).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    let next = f
        .service
        .list_permissions(query.clone(), page.next.clone(), 1)
        .unwrap();
    assert!(next.next.is_none());
    assert_ne!(page.items, next.items);
    assert_eq!(
        f.service
            .list_permissions(
                PermissionQuery::Proposals {
                    session: Some(restored.id.clone()),
                    state: Some(AccessProposalState::Pending)
                },
                None,
                1
            )
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        f.service
            .list_permissions(
                PermissionQuery::Proposals {
                    session: Some(restored.id),
                    state: Some(AccessProposalState::Approved)
                },
                None,
                1
            )
            .unwrap()
            .total,
        1
    );
    apply(
        &f.service,
        GrantChange::Revoke {
            grant: approved.grant.unwrap().id,
        },
    );
    assert_eq!(
        f.service
            .list_permissions(query, page.next, 1)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
}

#[test]
#[cfg(target_os = "macos")]
fn grants_do_not_chain_through_target_organization() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    let EntityId::Project(other) = change(
        &f.service,
        OrganizationChange::CreateProject {
            name: "other".into(),
        },
    )
    .targets[0] else {
        panic!()
    };
    change(
        &f.service,
        OrganizationChange::MoveLocation {
            location: f.b,
            home: Home::Project(other),
            associate_repository: false,
        },
    );
    f.issue(
        Audience::Project(f.project),
        WriteTarget::ProjectMembers(other),
    );
    f.issue(Audience::Project(other), WriteTarget::Root(f.outside));
    assert!(writable(&f.scope(), f.b));
    assert!(!writable(&f.scope(), f.outside));
    let later = register(
        &f.service,
        &f._temporary.path().join("future-target"),
        Registration::Directory {
            home: Home::Project(other),
        },
    );
    assert!(
        writable(&f.scope(), later),
        "member target follows future roots"
    );
    f.session.location.as_mut().unwrap().placement = Placement::Directory(f.b);
    assert!(
        writable(&f.scope(), f.outside),
        "directly applicable audience differs from grant chaining"
    );
    assert!(!writable(&f.scope(), f.a));
}

#[cfg(target_os = "macos")]
#[test]
fn native_admission_rejects_root_replacement_between_validation_and_pinning() {
    let _lock = crate::storage::lock_test_env();
    let f = Fixture::new();
    let root = f
        .session
        .location
        .as_ref()
        .unwrap()
        .cwd
        .observed_path()
        .to_path_buf();
    let original = root.join("original");
    std::fs::write(&original, "retained").unwrap();
    let moved = root.with_file_name("moved-before-pin");
    let mut service = f.service.clone();
    let replace = root.clone();
    let moved_target = moved.clone();
    service.fault = Some(std::sync::Arc::new(move |stage| {
        if stage == "native_before_root_pin" {
            std::fs::rename(&replace, &moved_target).map_err(io)?;
            std::fs::create_dir(&replace).map_err(io)?;
        }
        Ok(())
    }));
    assert!(
        service
            .acquire_native_mutation(&f.session, &[root.join("new")])
            .is_err()
    );
    assert!(!root.join("new").exists());
    assert_eq!(
        std::fs::read_to_string(moved.join("original")).unwrap(),
        "retained"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn legacy_adoption_checkpoints_one_notice_without_rewriting_history_or_frozen_state() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.location = None;
    f.session.ensure_initial_session_context_message();
    f.session
        .install_system_prompt(crate::session::StoredSystemPromptState {
            text: "SYNTHETIC FROZEN SYSTEM".into(),
            active_agent: crate::session::StoredAgentReference {
                scope: crate::instruction::InstructionScope::Global,
                id: "fixture".into(),
                display_name: "Fixture".into(),
            },
            first_provider_dispatch_at: None,
            active_transition_message_id: None,
        });
    f.session.save().unwrap();
    let before = crate::session::Session::load(&f.session.id).unwrap();
    let input = LegacyLocationAdoptionRequest {
        request: RequestId::new(),
        session: f.session.id.clone(),
        expected_working_dir: before.working_dir.as_ref().map(PathBuf::from),
        expected_catalog_revision: f.service.status().unwrap().revision,
        placement: Placement::Directory(f.b),
        cwd: f._temporary.path().join("b"),
    };
    let record = f.service.request_legacy_adoption(input.clone()).unwrap();
    let mut stale = input.clone();
    stale.request = RequestId::new();
    stale.expected_working_dir = None;
    assert_eq!(
        f.service.request_legacy_adoption(stale).unwrap_err().code,
        IssueCode::Conflict
    );
    let mut cancelled = input.clone();
    cancelled.request = RequestId::new();
    let pending = f
        .service
        .request_legacy_adoption(cancelled.clone())
        .unwrap();
    let cancelled_record = f.service.cancel_location_change(pending.operation).unwrap();
    assert_eq!(cancelled_record.state, LocationChangeState::Cancelled);
    assert_eq!(
        f.service.request_legacy_adoption(cancelled).unwrap(),
        cancelled_record
    );
    assert_eq!(record.state, LocationChangeState::Pending);
    assert_eq!(
        serde_json::to_value(
            crate::session::Session::load(&f.session.id)
                .unwrap()
                .messages
        )
        .unwrap(),
        serde_json::to_value(&before.messages).unwrap()
    );
    assert!(
        crate::session::Session::load_startup_stub(&f.session.id)
            .unwrap()
            .location
            .is_none()
    );
    assert_eq!(
        f.service.request_legacy_adoption(input.clone()).unwrap(),
        record
    );
    let mut conflict = input.clone();
    conflict.cwd = f._temporary.path().join("outside");
    assert_eq!(
        f.service
            .request_legacy_adoption(conflict)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    let mut tampered = record.clone();
    tampered.input.placement = Placement::Project(f.project);
    assert!(
        matches!(f.service.prepare_legacy_adoption(&tampered, &f.session), Err(problem) if problem.code == IssueCode::Conflict)
    );
    let prepared = f
        .service
        .prepare_legacy_adoption(&record, &f.session)
        .unwrap();
    let candidate = f
        .session
        .stage_legacy_location_adoption(
            prepared.location.clone(),
            "SYNTHETIC ADOPTION NOTICE".into(),
        )
        .unwrap();
    f.session.commit_location_candidate(candidate).unwrap();
    drop(prepared);
    // This is the crash-after-checkpoint/before-index recovery path.
    let complete = f
        .service
        .reconcile_location_change(record.operation)
        .unwrap();
    assert_eq!(complete.state, LocationChangeState::Complete);
    assert_eq!(f.service.request_legacy_adoption(input).unwrap(), complete);
    let after = crate::session::Session::load(&f.session.id).unwrap();
    assert_eq!(after.messages.len(), before.messages.len() + 1);
    assert_eq!(
        serde_json::to_value(&after.messages[..before.messages.len()]).unwrap(),
        serde_json::to_value(&before.messages).unwrap()
    );
    assert_eq!(after.system_prompt, before.system_prompt);
    assert_eq!(after.active_skill, before.active_skill);
    assert_eq!(after.startup_context, before.startup_context);
    assert_eq!(
        serde_json::to_value(&after.context_view).unwrap(),
        serde_json::to_value(&before.context_view).unwrap()
    );
    assert_eq!(
        after.location.as_ref().unwrap().initial_cwd,
        PathBuf::from(before.working_dir.unwrap())
    );
    assert_eq!(
        f.service
            .sessions(None, None, 100)
            .unwrap()
            .iter()
            .filter(|s| s.session == after.id)
            .count(),
        1
    );
    let mut unknown = Session::create_with_id(
        format!("session_unknown_cwd_{}", RequestId::new()),
        None,
        None,
    );
    unknown.working_dir = None;
    unknown.save().unwrap();
    let request = LegacyLocationAdoptionRequest {
        request: RequestId::new(),
        session: unknown.id.clone(),
        expected_working_dir: None,
        expected_catalog_revision: f.service.status().unwrap().revision,
        placement: Placement::Directory(f.b),
        cwd: f._temporary.path().join("b"),
    };
    let record = f.service.request_legacy_adoption(request).unwrap();
    assert_eq!(record.legacy_origin.as_ref().unwrap().working_dir, None);
    let prepared = f
        .service
        .prepare_legacy_adoption(&record, &unknown)
        .unwrap();
    let candidate = unknown
        .stage_legacy_location_adoption(
            prepared.location.clone(),
            "SYNTHETIC UNKNOWN ORIGIN".into(),
        )
        .unwrap();
    unknown.commit_location_candidate(candidate).unwrap();
    drop(prepared);
    assert_eq!(
        f.service
            .reconcile_location_change(record.operation)
            .unwrap()
            .legacy_origin
            .unwrap()
            .working_dir,
        None
    );
}

#[cfg(target_os = "macos")]
#[test]
fn context_scope_carry_is_reviewed_checkpointed_independent_and_replay_safe() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.save().unwrap();
    let auth = WorkspaceClientAuthority::authenticated("context-fixture").unwrap();
    let inherited = f
        .issue(Audience::Project(f.project), WriteTarget::Root(f.b))
        .grant
        .unwrap();
    let direct = f
        .issue(
            Audience::Session(f.session.id.clone()),
            WriteTarget::Root(f.outside),
        )
        .grant
        .unwrap();
    assert!(
        matches!(f.service.prepare_context_scope(&f.session,None,&auth),Err(e) if e.code == IssueCode::NeedsGrantChoice)
    );
    for (kind, carry) in [
        (NewContextKind::Split, true),
        (NewContextKind::Clear, false),
        (NewContextKind::Transfer, true),
    ] {
        let review = f.service.review_grant_carry(&f.session.id).unwrap();
        assert_eq!(review.direct_grants, vec![direct.clone()]);
        let plan = f
            .service
            .prepare_context_scope(
                &f.session,
                Some(GrantCarryChoice {
                    review: review.id,
                    carry,
                }),
                &auth,
            )
            .unwrap()
            .unwrap();
        let mut child = Session::create(Some(f.session.id.clone()), None);
        if kind == NewContextKind::Split {
            child.inherit_continuation_state_from(&f.session);
        }
        f.service
            .stage_context_scope(&plan, &mut child, kind)
            .unwrap();
        assert!(f.service.validate_context_scope(&child).is_err());
        child.save().unwrap();
        assert!(f.service.reconcile_context_scope(&child.id).is_err());
        let mut racing = Session::create(None, None);
        assert!(
            matches!(f.service.stage_context_scope(&plan, &mut racing, kind), Err(e) if e.code == IssueCode::Conflict)
        );
        assert_eq!(f.service.context_scope_status(review.id).unwrap().len(), 1);
        child.seal_context_scope();
        child.save().unwrap();
        let loaded = Session::load_startup_stub(&child.id).unwrap();
        assert_eq!(loaded.scope_copy, child.scope_copy);
        assert!(
            f.service
                .session_write_scope(&loaded)
                .unwrap()
                .grants
                .iter()
                .all(|g| g.id != direct.id)
        );
        let faulty = WorkspaceService {
            fault: Some(std::sync::Arc::new(|point| {
                if point == "context_scope_committed" {
                    Err(io("synthetic post-commit loss"))
                } else {
                    Ok(())
                }
            })),
            ..f.service.clone()
        };
        assert!(faulty.reconcile_context_scope(&child.id).is_err());
        f.service.reconcile_context_scope(&child.id).unwrap();
        f.service.validate_context_scope(&child).unwrap();
        let before = f.service.session_write_scope(&child).unwrap();
        assert!(before.grants.iter().any(|g| g.id == inherited.id));
        let copies = before
            .grants
            .iter()
            .filter(|g| g.copied_from == Some(direct.id))
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), usize::from(carry));
        if carry {
            let copy = copies[0];
            assert_ne!(copy.id, direct.id);
            assert_eq!(copy.audience, Audience::Session(child.id.clone()));
            apply(&f.service, GrantChange::Revoke { grant: copy.id });
            assert_eq!(
                f.service.inspect_grant(direct.id).unwrap().state,
                GrantState::Active
            );
        }
        f.service.reconcile_context_scope(&child.id).unwrap();
        assert_eq!(
            f.service
                .session_write_scope(&child)
                .unwrap()
                .grants
                .iter()
                .filter(|g| g.copied_from == Some(direct.id))
                .count(),
            0
        );
    }
    let review = f.service.review_grant_carry(&f.session.id).unwrap();
    let plan = f
        .service
        .prepare_context_scope(
            &f.session,
            Some(GrantCarryChoice {
                review: review.id,
                carry: true,
            }),
            &auth,
        )
        .unwrap()
        .unwrap();
    let mut child = Session::create(None, None);
    f.service
        .stage_context_scope(&plan, &mut child, NewContextKind::Clear)
        .unwrap();
    child.seal_context_scope();
    child.save().unwrap();
    apply(&f.service, GrantChange::Revoke { grant: direct.id });
    assert!(f.service.reconcile_context_scope(&child.id).is_err());
    assert!(f.service.validate_context_scope(&child).is_err());
    assert!(
        matches!(f.service.prepare_context_scope(&f.session,Some(GrantCarryChoice{review:review.id,carry:false}),&auth),Err(e) if e.code==IssueCode::Conflict)
    );
    assert!(
        f.service
            .prepare_context_scope(&f.session, None, &auth)
            .unwrap()
            .is_some()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn context_cwd_uses_live_inherited_or_reviewed_direct_grants_not_a_scope_override() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.save().unwrap();
    let auth = WorkspaceClientAuthority::authenticated("context-cwd-fixture").unwrap();
    let cwd = f._temporary.path().join("outside");
    assert!(
        f.service
            .prepare_primary_location(Placement::Directory(f.a), Some(&cwd), OperationId::new())
            .is_err()
    );
    let direct = f
        .issue(
            Audience::Session(f.session.id.clone()),
            WriteTarget::Root(f.outside),
        )
        .grant
        .unwrap();
    let prepared = f
        .service
        .prepare_session_location(
            &f.session.id,
            Placement::Directory(f.a),
            Some(&cwd),
            OperationId::new(),
        )
        .unwrap();
    f.session.location = Some(prepared.location.clone());
    f.session.working_dir = Some(
        prepared
            .location
            .cwd
            .observed_path()
            .to_string_lossy()
            .into(),
    );
    drop(prepared);
    f.session.save().unwrap();
    let review = f.service.review_grant_carry(&f.session.id).unwrap();
    assert!(
        matches!(f.service.prepare_context_scope(&f.session,Some(GrantCarryChoice{review:review.id,carry:false}),&auth),Err(e) if e.code==IssueCode::PermissionRequired)
    );
    let plan = f
        .service
        .prepare_context_scope(
            &f.session,
            Some(GrantCarryChoice {
                review: review.id,
                carry: true,
            }),
            &auth,
        )
        .unwrap()
        .unwrap();
    let mut child = Session::create(None, None);
    f.service
        .stage_context_scope(&plan, &mut child, NewContextKind::Clear)
        .unwrap();
    child.seal_context_scope();
    child.save().unwrap();
    f.service.reconcile_context_scope(&child.id).unwrap();
    drop(plan);
    assert_eq!(
        child.location.as_ref().unwrap().placement,
        Placement::Directory(f.a)
    );
    let copy = f
        .service
        .session_write_scope(&child)
        .unwrap()
        .grants
        .into_iter()
        .find(|g| g.copied_from == Some(direct.id))
        .unwrap();
    assert_eq!(copy.target, WriteTarget::Root(f.outside));
    apply(&f.service, GrantChange::Revoke { grant: direct.id });
    assert!(
        f.service
            .prepare_session_location(
                &f.session.id,
                Placement::Directory(f.a),
                Some(&cwd),
                OperationId::new()
            )
            .is_err()
    );
    f.issue(Audience::Project(f.project), WriteTarget::Root(f.outside));
    let future = f
        .service
        .prepare_primary_location(Placement::Directory(f.a), Some(&cwd), OperationId::new())
        .unwrap();
    assert_eq!(future.location.placement, Placement::Directory(f.a));
    assert_eq!(future.root, f.outside);
}

#[cfg(target_os = "macos")]
#[test]
fn scope_explanation_receipts_follow_policy_not_organization_noise_or_history() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.save().unwrap();
    let initial = f
        .service
        .observe_session_scope(&f.session)
        .unwrap()
        .unwrap();
    let before = serde_json::to_value(&f.session.messages).unwrap();
    let candidate = f
        .session
        .stage_scope_notice(&initial, "SYNTHETIC SCOPE".into())
        .unwrap();
    f.session.commit_scope_notice(candidate).unwrap();
    assert_eq!(
        serde_json::to_value(&f.session.messages[..f.session.messages.len() - 1]).unwrap(),
        before
    );
    assert!(
        f.service
            .observe_session_scope(&Session::load_startup_stub(&f.session.id).unwrap())
            .unwrap()
            .is_none()
    );
    change(
        &f.service,
        OrganizationChange::Archive {
            target: EntityId::Project(f.project),
            archived: true,
        },
    );
    assert!(
        f.service
            .observe_session_scope(&f.session)
            .unwrap()
            .is_none()
    );
    let grant = f
        .issue(
            Audience::Session(f.session.id.clone()),
            WriteTarget::Root(f.outside),
        )
        .grant
        .unwrap();
    let granted = f
        .service
        .observe_session_scope(&f.session)
        .unwrap()
        .unwrap();
    assert_eq!(granted.additional_roots, 1);
    assert_eq!(granted.explicit_grants, 1);
    f.session
        .commit_scope_notice(
            f.session
                .stage_scope_notice(&granted, "SYNTHETIC GRANT".into())
                .unwrap(),
        )
        .unwrap();
    apply(&f.service, GrantChange::Revoke { grant: grant.id });
    assert!(!writable(&f.scope(), f.outside));
    let revoked = f
        .service
        .observe_session_scope(&f.session)
        .unwrap()
        .unwrap();
    assert_eq!(revoked.additional_roots, 0);
    // Prior prose and a copied transcript do not acknowledge another identity's scope.
    let mut split = Session::create(Some(f.session.id.clone()), None);
    split.inherit_continuation_state_from(&f.session);
    assert!(split.scope_notice.is_none());
    assert!(f.service.observe_session_scope(&split).unwrap().is_some());
    f.session
        .commit_scope_notice(
            f.session
                .stage_scope_notice(&revoked, "SYNTHETIC REVOKE".into())
                .unwrap(),
        )
        .unwrap();
    let restored = Session::load(&f.session.id).unwrap();
    assert_eq!(restored.scope_notice, f.session.scope_notice);
    assert!(
        f.service
            .observe_session_scope(&restored)
            .unwrap()
            .is_none()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn permission_decisions_and_import_bindings_retain_history_without_implicit_authority() {
    let _lock = crate::storage::lock_test_env();
    let mut f = Fixture::new();
    f.session.save().unwrap();
    let auth = WorkspaceClientAuthority::authenticated("human-permission-fixture").unwrap();
    for decision in [ProposalDecision::Decline, ProposalDecision::Cancel] {
        let pending = f
            .service
            .request_access(
                &f.session,
                RequestId::new(),
                WriteTarget::Root(f.outside),
                "synthetic reason".into(),
            )
            .unwrap()
            .proposal
            .unwrap();
        let revision = f.service.status().unwrap().revision;
        let request = RequestId::new();
        let fault = WorkspaceService {
            fault: Some(std::sync::Arc::new(|stage| {
                if stage == "proposal_decision_committed" {
                    Err(io("synthetic lost reply"))
                } else {
                    Ok(())
                }
            })),
            ..f.service.clone()
        };
        assert!(
            fault
                .decide_access_proposal(&auth, request, pending.id, revision, decision)
                .is_err()
        );
        let result = f
            .service
            .decide_access_proposal(&auth, request, pending.id, revision, decision)
            .unwrap();
        assert_eq!(
            result,
            f.service
                .decide_access_proposal(&auth, request, pending.id, revision, decision)
                .unwrap()
        );
        assert_eq!(result.proposal.unwrap().reason, pending.reason);
        assert!(!writable(&f.scope(), f.outside));
        assert!(
            f.service
                .review_grant_change(
                    f.service.status().unwrap().revision,
                    GrantChange::Issue {
                        audience: Audience::Session(f.session.id.clone()),
                        target: pending.target,
                        proposal: Some(pending.id)
                    }
                )
                .is_err()
        );
    }
    // Source export has a cross-project grant whose audience is deliberately foreign.
    let source_root = tempfile::tempdir().unwrap();
    let source = WorkspaceService::new(source_root.path());
    source.initialize(RequestId::new()).unwrap();
    let EntityId::Project(a) = change(
        &source,
        OrganizationChange::CreateProject {
            name: "source-a".into(),
        },
    )
    .targets[0] else {
        panic!()
    };
    let EntityId::Project(b) = change(
        &source,
        OrganizationChange::CreateProject {
            name: "source-b".into(),
        },
    )
    .targets[0] else {
        panic!()
    };
    let foreign = apply(
        &source,
        GrantChange::Issue {
            audience: Audience::Project(a),
            target: WriteTarget::ProjectMembers(b),
            proposal: None,
        },
    )
    .grant
    .unwrap();
    let export = source
        .export_project(RequestId::new(), a, "portable permission".into())
        .unwrap();
    let review = f
        .service
        .review_import(
            export,
            f.service.status().unwrap().revision,
            ImportCollisionPolicy::Reject,
            vec![],
        )
        .unwrap();
    f.service.apply_import(RequestId::new(), review.id).unwrap();
    let PermissionResponse::ImportedGrants { items, total, .. } =
        f.service.list_imported_grants(None, 1).unwrap()
    else {
        panic!()
    };
    assert_eq!(total, 1);
    let retained = &items[0];
    assert_eq!(retained.grant.id, foreign.id);
    assert!(retained.bound_grant.is_none());
    assert!(!writable(&f.scope(), f.outside));
    let binding = GrantChange::BindImported {
        reference: retained.reference,
        installation: retained.installation,
        grant: foreign.id,
        audience: Audience::Session(f.session.id.clone()),
        target: WriteTarget::Root(f.outside),
    };
    let mut wrong = binding.clone();
    if let GrantChange::BindImported { installation, .. } = &mut wrong {
        *installation = InstallationId::new();
    }
    assert!(
        f.service
            .review_grant_change(f.service.status().unwrap().revision, wrong)
            .is_err()
    );
    let review = f
        .service
        .review_grant_change(f.service.status().unwrap().revision, binding.clone())
        .unwrap();
    let request = RequestId::new();
    let receipt = f
        .service
        .apply_grant_change(&auth, request, review.id)
        .unwrap();
    let bound = receipt.grant.as_ref().unwrap();
    assert_ne!(bound.id, foreign.id);
    assert_eq!(bound.copied_from, Some(foreign.id));
    assert!(writable(&f.scope(), f.outside));
    assert_eq!(
        f.service
            .apply_grant_change(&auth, request, review.id)
            .unwrap(),
        receipt
    );
    assert!(
        f.service
            .review_grant_change(f.service.status().unwrap().revision, binding)
            .is_err()
    );
    let PermissionResponse::ImportedGrants { items, .. } =
        f.service.list_imported_grants(None, 1).unwrap()
    else {
        panic!()
    };
    assert_eq!(items[0].grant.state, GrantState::Disabled);
    assert_eq!(items[0].bound_grant, Some(bound.id));
    apply(&f.service, GrantChange::Revoke { grant: bound.id });
    assert!(!writable(&f.scope(), f.outside));
    assert_eq!(
        source.inspect_grant(foreign.id).unwrap().state,
        GrantState::Active
    );
}

#[test]
#[cfg(target_os = "macos")]
fn member_grant_reviews_exclude_retired_history_without_reviving_its_files() {
    let _lock = crate::storage::lock_test_env();
    let f = Fixture::new();
    change(
        &f.service,
        OrganizationChange::Retire {
            target: EntityId::Location(f.b),
        },
    );
    let review = f
        .service
        .review_grant_change(
            f.service.status().unwrap().revision,
            GrantChange::Issue {
                audience: Audience::WorkArea(f.area),
                target: WriteTarget::ProjectMembers(f.project),
                proposal: None,
            },
        )
        .unwrap();
    assert_eq!(
        review
            .excluded_roots
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![f.b]
    );
    assert!(!review.roots.iter().any(|r| r.id == f.b));
    f.service
        .apply_grant_change(
            &WorkspaceClientAuthority::authenticated("fixture").unwrap(),
            RequestId::new(),
            review.id,
        )
        .unwrap();
    assert!(!writable(&f.scope(), f.b));
    assert!(f._temporary.path().join("b").exists());
}
