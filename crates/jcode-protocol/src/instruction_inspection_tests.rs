use super::*;

#[test]
fn instruction_inspection_wire_preserves_all_readonly_operations_and_exact_text() {
    let operations = vec![
        InstructionInspectionRequest::Open {
            filter: Default::default(),
        },
        InstructionInspectionRequest::Resources {
            snapshot: "snapshot".into(),
            filter: InstructionFilter {
                search: "界".into(),
                valid: Some(false),
                ..Default::default()
            },
            offset: 64,
        },
        InstructionInspectionRequest::Detail {
            snapshot: "snapshot".into(),
            target: InstructionInspectionTarget::Resource("opaque".into()),
            view: InstructionInspectionView::System,
            revision: None,
        },
        InstructionInspectionRequest::Detail {
            snapshot: "snapshot".into(),
            target: InstructionInspectionTarget::Repository("opaque".into()),
            view: InstructionInspectionView::WorkingDiff,
            revision: Some(InstructionRevisionSelection {
                from: "a".repeat(40),
                to: Some("b".repeat(40)),
            }),
        },
        InstructionInspectionRequest::Text {
            snapshot: "snapshot".into(),
            document: "document".into(),
            offset: 16_384,
        },
        InstructionInspectionRequest::History {
            snapshot: "snapshot".into(),
            target: InstructionInspectionTarget::Session,
            offset: 0,
        },
        InstructionInspectionRequest::Cancel,
        InstructionInspectionRequest::Close,
    ];
    for operation in operations {
        let request = Request::InspectInstructions {
            id: 17,
            request: operation.clone(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id(), 17);
        let Request::InspectInstructions { request, .. } = decoded else {
            panic!("inspection request")
        };
        assert_eq!(request, operation);
    }
    let text = "α界🙂\n".repeat(5000);
    let reply = InstructionInspectionReply {
        session_id: "session".into(),
        snapshot: Some("snapshot".into()),
        result: InstructionInspectionResult::Text(InstructionTextPage {
            document: "document".into(),
            title: "Fixture".into(),
            offset: 0,
            total_bytes: text.len(),
            next: None,
            text,
        }),
    };
    let event = ServerEvent::InstructionInspection {
        id: 18,
        reply: Box::new(reply.clone()),
    };
    let decoded: ServerEvent = serde_json::from_str(&encode_event(&event)).unwrap();
    let ServerEvent::InstructionInspection { id, reply: actual } = decoded else {
        panic!("inspection event")
    };
    assert_eq!(id, 18);
    assert_eq!(*actual, reply);
}
