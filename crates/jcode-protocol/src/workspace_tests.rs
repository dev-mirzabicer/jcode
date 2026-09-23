use super::*;
use jcode_workspace_types::{RequestId, WorkspaceRequest, WorkspaceResponse};

#[test]
fn workspace_catalog_control_is_session_independent_and_roundtrips() {
    let request = Request::Workspace {
        id: 42,
        request: Box::new(WorkspaceRequest::Initialize {
            request: RequestId::new(),
        }),
    };
    let bytes = serde_json::to_vec(&request).unwrap();
    let decoded: Request = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.id(), 42);
    assert!(matches!(decoded, Request::Workspace { .. }));
    let capabilities = ServerEvent::WorkspaceCapabilities {
        id: 42,
        catalog_version: 1,
        permissions_version: Some(1),
        checkout_version: Some(1),
        managed_rollout: false,
    };
    let bytes = serde_json::to_vec(&capabilities).unwrap();
    assert!(matches!(
        serde_json::from_slice::<ServerEvent>(&bytes).unwrap(),
        ServerEvent::WorkspaceCapabilities {
            managed_rollout: false,
            ..
        }
    ));
    let response = ServerEvent::WorkspaceResponse {
        id: 42,
        response: Box::new(WorkspaceResponse::Error(jcode_workspace_types::Issue {
            code: jcode_workspace_types::IssueCode::Conflict,
            detail: "synthetic".into(),
        })),
    };
    assert!(serde_json::from_slice::<ServerEvent>(&serde_json::to_vec(&response).unwrap()).is_ok());
}

#[test]
fn catalog_rejects_unreviewed_approval_and_session_authority_mutations() {
    for action in [
        "approve_grant",
        "close_checkout",
        "set_session_location",
        "reconcile_session_index",
    ] {
        let request = serde_json::json!({"type":"workspace","id":3,"request":{"action":action}});
        assert!(serde_json::from_value::<Request>(request).is_err());
    }
    assert!(
        serde_json::from_value::<WorkspaceRequest>(
            serde_json::json!({"action":"status","trusted":true})
        )
        .is_err()
    );
}

#[test]
fn permissions_negotiate_without_enabling_rollout_or_accepting_forged_authority() {
    use jcode_workspace_types::{
        Audience, GrantChange, LocationId, PermissionRequest, ProjectId, WriteTarget,
    };
    let request = WorkspaceRequest::Permissions {
        request: PermissionRequest::Review {
            expected_revision: 2,
            change: GrantChange::Issue {
                audience: Audience::Project(ProjectId::new()),
                target: WriteTarget::Root(LocationId::new()),
                proposal: None,
            },
        },
    };
    let mut value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<WorkspaceRequest>(value.clone()).unwrap(),
        request
    );
    value["request"]["trusted"] = serde_json::json!(true);
    assert!(serde_json::from_value::<WorkspaceRequest>(value).is_err());
    let old = serde_json::json!({"type":"workspace_capabilities","id":1,"catalog_version":1,"managed_rollout":false});
    assert!(matches!(
        serde_json::from_value::<ServerEvent>(old).unwrap(),
        ServerEvent::WorkspaceCapabilities {
            permissions_version: None,
            checkout_version: None,
            managed_rollout: false,
            ..
        }
    ));
    let forged = serde_json::json!({"action":"propose","session":"s","request":RequestId::new(),"target":{"kind":"root","id":LocationId::new()},"reason":"synthetic","audience":{"kind":"project","id":ProjectId::new()}});
    assert!(serde_json::from_value::<PermissionRequest>(forged).is_err());
}

#[test]
fn checkout_clone_request_requires_review_identity_and_does_not_confer_human_authority() {
    use jcode_workspace_types::{
        CloneBase, CloneBranch, CloneDestination, CloneSource, CloneSpec, Home, ProjectId,
        RepositoryId, ReviewId,
    };
    let request = WorkspaceRequest::ReviewClone {
        expected_revision: 7,
        spec: CloneSpec {
            home: Home::Project(ProjectId::new()),
            repository: RepositoryId::new(),
            name: "synthetic".into(),
            source: CloneSource::Local {
                path: "/fixture/source".into(),
            },
            base: CloneBase::Branch {
                name: "main".into(),
            },
            branch: CloneBranch::Detached,
            remotes: vec![],
            destination: CloneDestination::Custom {
                volume_uuid: "00000000-0000-0000-0000-000000000001".into(),
                path: "/fixture/checkout".into(),
            },
            submodules: true,
            lfs: true,
            trusted_local_submodule_urls: vec![],
            trusted_lfs_urls: vec![],
        },
    };
    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<WorkspaceRequest>(wire.clone()).unwrap(),
        request
    );
    let mut forged = wire;
    forged["trusted"] = serde_json::json!(true);
    assert!(serde_json::from_value::<WorkspaceRequest>(forged).is_err());
    let begin = WorkspaceRequest::BeginClone {
        request: RequestId::new(),
        review: ReviewId::new(),
    };
    assert_eq!(
        serde_json::from_value::<WorkspaceRequest>(serde_json::to_value(&begin).unwrap()).unwrap(),
        begin
    );
    assert!(
        serde_json::from_value::<WorkspaceRequest>(
            serde_json::json!({"action":"begin_clone","request":RequestId::new()})
        )
        .is_err()
    );
}

#[test]
fn checkout_rebind_and_startup_copy_require_typed_reviews_and_external_approvals() {
    use jcode_workspace_types::{LocationId, OrganizationChange, ReviewId, StartupCopyApproval};
    let target = LocationId::new();
    let rebind = WorkspaceRequest::Review {
        expected_revision: 5,
        change: OrganizationChange::RebindLocation {
            location: target,
            expected_old_path: "/fixture/old".into(),
            expected_generation: 1,
            new_path: "/fixture/new".into(),
        },
    };
    let copy = WorkspaceRequest::ReviewStartupCopy {
        expected_catalog_revision: 5,
        source: "/fixture/source".into(),
        target,
        expected_source_plan_revision: 2,
        expected_target_plan_revision: 0,
        external_approvals: vec![StartupCopyApproval {
            source_spec_id: "a".repeat(64),
            approved_resolved_target: "/fixture/shared".into(),
        }],
    };
    let apply = WorkspaceRequest::ApplyStartupCopy {
        request: RequestId::new(),
        review: ReviewId::new(),
    };
    for operation in [rebind, copy, apply] {
        let wire = serde_json::to_value(&operation).unwrap();
        assert_eq!(
            serde_json::from_value::<WorkspaceRequest>(wire.clone()).unwrap(),
            operation
        );
        let mut forged = wire;
        forged["trusted"] = serde_json::json!(true);
        assert!(serde_json::from_value::<WorkspaceRequest>(forged).is_err());
    }
    assert!(
        serde_json::from_value::<WorkspaceRequest>(serde_json::json!({
            "action": "apply_startup_copy", "request": RequestId::new()
        }))
        .is_err()
    );
}
