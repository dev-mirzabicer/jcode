use super::*;

#[test]
fn instruction_management_round_trips_complete_intent_and_correlated_failures() {
    let requests = [
        InstructionManagementRequest::Begin {
            snapshot: "snapshot".into(),
            target: InstructionInspectionTarget::Resource("opaque".into()),
            action: InstructionEditAction::Edit,
        },
        InstructionManagementRequest::Update {
            draft: "draft".into(),
            generation: 3,
            change: InstructionDraftChange::Body {
                file: "opaque-file".into(),
                body: "合成 {{literal}}\n".repeat(4000),
            },
        },
        InstructionManagementRequest::Review {
            draft: "draft".into(),
            generation: 4,
        },
        InstructionManagementRequest::Save {
            draft: "draft".into(),
            generation: 4,
        },
        InstructionManagementRequest::Resume {
            scope: InstructionEditScope::Project,
            draft: "draft".into(),
        },
        InstructionManagementRequest::Discard {
            draft: "draft".into(),
            generation: 4,
        },
        InstructionManagementRequest::Close,
    ];
    for request in requests {
        let wire = Request::ManageInstructions {
            id: 73,
            request: Box::new(request.clone()),
        };
        assert_eq!(wire.id(), 73);
        let restored: Request =
            serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
        let Request::ManageInstructions {
            id,
            request: actual,
        } = restored
        else {
            panic!("request kind")
        };
        assert_eq!(id, 73);
        assert_eq!(*actual, request);
    }
    let reply = InstructionManagementReply {
        session_id: "session".into(),
        result: InstructionManagementResult::Failed(InstructionManagementFailure {
            operation: "publish".into(),
            detail: "synthetic interruption".into(),
            draft: Some("draft".into()),
            source_unchanged: false,
        }),
    };
    let event = ServerEvent::InstructionManagement {
        id: 73,
        reply: Box::new(reply.clone()),
    };
    let restored: ServerEvent =
        serde_json::from_slice(&serde_json::to_vec(&event).unwrap()).unwrap();
    let ServerEvent::InstructionManagement { id, reply: actual } = restored else {
        panic!("reply kind")
    };
    assert_eq!(id, 73);
    assert_eq!(*actual, reply);
}
