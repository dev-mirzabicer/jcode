use super::*;

fn project(id: ProjectId) -> Entity {
    Entity::Project(Project {
        id,
        name: "p".into(),
        state: OrganizationState::Active,
        revision: 1,
    })
}

fn receipt(request: RequestId) -> Receipt {
    Receipt {
        operation: OperationId::new(),
        request,
        revision: 2,
        targets: vec![],
        issues: vec![],
    }
}

#[test]
fn inspect_and_receipts_bind_their_exact_identity() {
    let target = ProjectId::new();
    let inspect = WorkspaceRequest::Inspect {
        target: EntityId::Project(target),
    };
    assert!(inspect.matches_response(&WorkspaceResponse::Entity(project(target))));
    assert!(!inspect.matches_response(&WorkspaceResponse::Entity(project(ProjectId::new()))));
    let request = RequestId::new();
    let apply = WorkspaceRequest::Apply {
        request,
        review: ReviewId::new(),
    };
    assert!(apply.matches_response(&WorkspaceResponse::Receipt(receipt(request))));
    assert!(!apply.matches_response(&WorkspaceResponse::Receipt(receipt(RequestId::new()))));
    // A different response kind is a foreign reply, not success.
    assert!(!apply.matches_response(&WorkspaceResponse::Entity(project(target))));
}

#[test]
fn domain_rejection_always_belongs_to_the_request() {
    let issue = WorkspaceResponse::Error(Issue {
        code: IssueCode::Conflict,
        detail: "stale".into(),
    });
    assert!(WorkspaceRequest::Status {}.matches_response(&issue));
}

#[test]
fn pages_cannot_exceed_the_requested_limit() {
    let list = WorkspaceRequest::List {
        query: Query::default(),
        after: None,
        limit: 1,
    };
    let page = |items: Vec<Entity>| {
        WorkspaceResponse::Page(Page {
            revision: 1,
            total: 2,
            items,
            next: None,
        })
    };
    assert!(list.matches_response(&page(vec![project(ProjectId::new())])));
    assert!(!list.matches_response(&page(vec![
        project(ProjectId::new()),
        project(ProjectId::new())
    ])));
}

#[test]
fn proposals_bind_session_target_and_request() {
    let session = "session_a".to_string();
    let request = RequestId::new();
    let target = WriteTarget::Root(LocationId::new());
    let propose = WorkspaceRequest::Permissions {
        request: PermissionRequest::Propose {
            session: session.clone(),
            request,
            target: target.clone(),
            reason: "edit".into(),
        },
    };
    let mutation = |session: &str, target: WriteTarget| {
        WorkspaceResponse::Permissions(Box::new(PermissionResponse::Mutation(PermissionMutation {
            receipt: receipt(request),
            grant: None,
            proposal: Some(AccessProposal {
                id: ProposalId::new(),
                session: session.into(),
                target,
                revision: 2,
                state: AccessProposalState::Pending,
                reason: "edit".into(),
                grant: None,
            }),
        })))
    };
    assert!(propose.matches_response(&mutation(&session, target.clone())));
    assert!(!propose.matches_response(&mutation("session_b", target)));
    assert!(!propose.matches_response(&mutation(&session, WriteTarget::Root(LocationId::new()))));
}

#[test]
fn every_request_names_one_negotiated_contract() {
    use WorkspaceCapability as C;
    assert_eq!(
        WorkspaceRequest::Status {}.required_capability(),
        C::Catalog
    );
    assert_eq!(
        WorkspaceRequest::Volumes {}.required_capability(),
        C::Checkout
    );
    assert_eq!(
        WorkspaceRequest::Operations {
            query: OperationQuery::default(),
            after: None,
            limit: 1
        }
        .required_capability(),
        C::Management
    );
    assert_eq!(
        WorkspaceRequest::Permissions {
            request: PermissionRequest::Grant {
                grant: GrantId::new()
            }
        }
        .required_capability(),
        C::Permissions
    );
    let old = WorkspaceVersions {
        catalog_version: 1,
        permissions_version: Some(1),
        ..Default::default()
    };
    assert!(old.supports(C::Catalog) && old.supports(C::Permissions));
    assert!(!old.supports(C::Management) && !old.supports(C::Checkout));
    let future = WorkspaceVersions {
        catalog_version: 2,
        ..Default::default()
    };
    assert!(!future.supports(C::Catalog));
}

#[test]
fn versions_decode_from_older_runtimes() {
    let versions: WorkspaceVersions =
        serde_json::from_str(r#"{"catalog_version":1,"managed_rollout":false}"#).unwrap();
    assert_eq!(versions.management_version, None);
}

/// Shared Rust/TypeScript correlation fixture. Both SDKs evaluate the same
/// committed cases; `JCODE_WRITE_WORKSPACE_MATRIX=1` regenerates the file.
fn matrix() -> Vec<serde_json::Value> {
    use serde_json::json;
    let project_id = ProjectId::new();
    let request = RequestId::new();
    let session = "session_matrix".to_string();
    let location = LocationId::new();
    let proposal_id = ProposalId::new();
    let snapshot = SnapshotId::new();
    let target = WriteTarget::Root(location);
    let proposal = |session: &str, target: WriteTarget, id: ProposalId| AccessProposal {
        id,
        session: session.into(),
        target,
        revision: 3,
        state: AccessProposalState::Pending,
        reason: "edit".into(),
        grant: None,
    };
    let mutation = |proposal: Option<AccessProposal>, request: RequestId| {
        WorkspaceResponse::Permissions(Box::new(PermissionResponse::Mutation(PermissionMutation {
            receipt: receipt(request),
            grant: None,
            proposal,
        })))
    };
    let spec = CloneSpec {
        home: Home::Project(project_id),
        repository: RepositoryId::new(),
        name: "clone".into(),
        source: CloneSource::Remote {
            url: "https://example.invalid/repo.git".into(),
        },
        base: CloneBase::Branch {
            name: "main".into(),
        },
        branch: CloneBranch::KeepName,
        remotes: vec![],
        destination: CloneDestination::Custom {
            volume_uuid: "VOLUME".into(),
            path: "/tmp/clone".into(),
        },
        submodules: true,
        lfs: true,
        trusted_submodule_urls: vec![],
        trusted_lfs_urls: vec![],
    };
    let clone_record = |request: RequestId| {
        WorkspaceResponse::Clone(Box::new(CloneRecord {
            operation: OperationId::new(),
            request,
            location,
            review: CloneReview {
                id: ReviewId::new(),
                revision: 4,
                spec: spec.clone(),
                source_commit: "0".repeat(40),
                destination: "/tmp/clone".into(),
                volume_uuid: "VOLUME".into(),
                issues: vec![],
            },
            state: CloneState::Ready,
            cancel_requested: false,
            stage: None,
            output_runs: vec![],
            discovered_sources: vec![],
            pending_trust: vec![],
            trust_approvals: vec![],
            output_issue: None,
            issue: None,
            revision: 5,
        }))
    };
    let snapshot_record = |id: SnapshotId| Snapshot {
        id,
        name: "named".into(),
        path: "/tmp/snapshot".into(),
        sha256: "0".repeat(64),
        revision: 2,
        automatic: false,
    };
    let page = |count: usize| {
        WorkspaceResponse::Page(Page {
            revision: 1,
            total: count as u64,
            items: (0..count).map(|_| project(ProjectId::new())).collect(),
            next: None,
        })
    };
    let propose = WorkspaceRequest::Permissions {
        request: PermissionRequest::Propose {
            session: session.clone(),
            request,
            target: target.clone(),
            reason: "edit".into(),
        },
    };
    let decide = WorkspaceRequest::Permissions {
        request: PermissionRequest::DecideProposal {
            request,
            proposal: proposal_id,
            expected_revision: 3,
            decision: ProposalDecision::Decline,
        },
    };
    let scope = |session: &str| {
        WorkspaceResponse::Permissions(Box::new(PermissionResponse::Scope(SessionWriteScope {
            session: session.into(),
            placement: Placement::Project(project_id),
            session_revision: 1,
            catalog_revision: 1,
            roots: vec![],
            grants: vec![],
        })))
    };
    let case = |name: &str, request: &WorkspaceRequest, response: WorkspaceResponse| {
        json!({
            "name": name,
            "request": request,
            "response": response,
            "accepted": request.matches_response(&response),
        })
    };
    let inspect = WorkspaceRequest::Inspect {
        target: EntityId::Project(project_id),
    };
    let apply = WorkspaceRequest::Apply {
        request,
        review: ReviewId::new(),
    };
    let list = WorkspaceRequest::List {
        query: Query::default(),
        after: None,
        limit: 2,
    };
    let begin = WorkspaceRequest::BeginClone {
        request,
        review: ReviewId::new(),
    };
    let restore = WorkspaceRequest::ReviewRestore { snapshot };
    let scope_request = WorkspaceRequest::Permissions {
        request: PermissionRequest::Scope {
            session: session.clone(),
        },
    };
    let issue = WorkspaceResponse::Error(Issue {
        code: IssueCode::Conflict,
        detail: "stale revision".into(),
    });
    let restore_review = |id: SnapshotId| {
        WorkspaceResponse::RestoreReview(RestoreReview {
            id: ReviewId::new(),
            snapshot: snapshot_record(id),
            current_revision: Some(2),
            grants: vec![],
            issues: vec![],
        })
    };
    vec![
        case(
            "inspect exact entity",
            &inspect,
            WorkspaceResponse::Entity(project(project_id)),
        ),
        case(
            "inspect foreign entity",
            &inspect,
            WorkspaceResponse::Entity(project(ProjectId::new())),
        ),
        case(
            "apply receipt",
            &apply,
            WorkspaceResponse::Receipt(receipt(request)),
        ),
        case(
            "apply foreign receipt",
            &apply,
            WorkspaceResponse::Receipt(receipt(RequestId::new())),
        ),
        case("domain rejection", &apply, issue),
        case("kind mismatch", &WorkspaceRequest::Status {}, page(0)),
        case("page within limit", &list, page(2)),
        case("page above limit", &list, page(3)),
        case(
            "proposal for session and target",
            &propose,
            mutation(
                Some(proposal(&session, target.clone(), proposal_id)),
                request,
            ),
        ),
        case(
            "proposal for another session",
            &propose,
            mutation(
                Some(proposal("other", target.clone(), proposal_id)),
                request,
            ),
        ),
        case(
            "proposal without proposal body",
            &propose,
            mutation(None, request),
        ),
        case(
            "decision on exact proposal",
            &decide,
            mutation(
                Some(proposal(&session, target.clone(), proposal_id)),
                request,
            ),
        ),
        case(
            "decision on another proposal",
            &decide,
            mutation(Some(proposal(&session, target, ProposalId::new())), request),
        ),
        case("clone record", &begin, clone_record(request)),
        case(
            "foreign clone record",
            &begin,
            clone_record(RequestId::new()),
        ),
        case("restore review", &restore, restore_review(snapshot)),
        case(
            "foreign restore review",
            &restore,
            restore_review(SnapshotId::new()),
        ),
        case("session scope", &scope_request, scope(&session)),
        case("foreign session scope", &scope_request, scope("other")),
    ]
}

#[test]
fn shared_workspace_correlation_matrix() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workspace_correlation.json");
    if std::env::var_os("JCODE_WRITE_WORKSPACE_MATRIX").is_some() {
        let mut body = serde_json::to_string_pretty(&matrix()).unwrap();
        body.push('\n');
        std::fs::write(&path, body).unwrap();
    }
    let committed: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(committed.len() >= 19);
    let mut accepted = 0;
    for item in committed {
        let request: WorkspaceRequest = serde_json::from_value(item["request"].clone()).unwrap();
        let response: WorkspaceResponse = serde_json::from_value(item["response"].clone()).unwrap();
        let expected = item["accepted"].as_bool().unwrap();
        accepted += usize::from(expected);
        assert_eq!(
            request.matches_response(&response),
            expected,
            "{}",
            item["name"]
        );
    }
    // The fixture exercises both outcomes, not only acceptance.
    assert!(accepted > 5 && accepted < 15);
}
