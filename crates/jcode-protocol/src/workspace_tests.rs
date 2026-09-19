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
fn catalog_cannot_decode_grant_approval_or_session_authority_mutations() {
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
