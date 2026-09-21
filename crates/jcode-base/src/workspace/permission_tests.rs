use super::*;
use crate::session::Session;

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
