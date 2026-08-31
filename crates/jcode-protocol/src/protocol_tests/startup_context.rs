#[test]
fn startup_context_requests_round_trip_with_bounded_fields() -> Result<()> {
    let requests = [
        Request::GetStartupContextStatus {
            id: 1,
            file_page_start: 4,
            file_page_size: Some(20),
            issue_page_start: 2,
            issue_page_size: Some(10),
        },
        Request::OpenStartupContextEditor { id: 2 },
        Request::RenewStartupContextEditorLease {
            id: 3,
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
        },
        Request::ListStartupContextDirectory {
            id: 4,
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
            directory: "docs".to_string(),
            page_start: 5,
            page_size: Some(50),
        },
        Request::SearchStartupContextFiles {
            id: 5,
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
            query: "plan".to_string(),
            max_results: Some(80),
        },
        Request::PreviewStartupContextFile {
            id: 6,
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
            path: "docs/PLAN.md".to_string(),
            start_char: 12,
            max_chars: Some(4096),
        },
        Request::GetStartupContextFileDetail {
            id: 7,
            batch_id: "batch".to_string(),
            spec_id: "spec".to_string(),
            message_id: "message".to_string(),
            expected_sha256: "abc".to_string(),
            start_char: 16,
            max_chars: Some(1024),
        },
        Request::PreviewStartupContextSelection {
            id: 8,
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
            selection: vec![StartupContextSelectionInput {
                existing_spec_id: None,
                path: "docs/PLAN.md".to_string(),
                approved_external_target: None,
            }],
        },
        Request::ApplyStartupContextSelection {
            id: 9,
            operation_id: "operation".to_string(),
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
            selection: vec![StartupContextSelectionInput {
                existing_spec_id: None,
                path: "docs/PLAN.md".to_string(),
                approved_external_target: None,
            }],
            save_project_default: true,
        },
        Request::CancelStartupContextApply {
            id: 10,
            operation_id: "operation".to_string(),
            lease_id: "lease".to_string(),
            project_key_digest: "digest".to_string(),
            expected_plan_revision: 7,
        },
        Request::GetStartupContextApplyStatus {
            id: 11,
            operation_id: "operation".to_string(),
        },
    ];

    for request in requests {
        let id = request.id();
        let value = serde_json::to_value(&request)?;
        let decoded: Request = serde_json::from_value(value.clone())?;
        assert_eq!(decoded.id(), id);
        assert_eq!(serde_json::to_value(decoded)?, value);
    }
    Ok(())
}

fn compact_startup_status() -> StartupContextCompactStatus {
    StartupContextCompactStatus {
        protocol_version: STARTUP_CONTEXT_PROTOCOL_VERSION,
        session_id: "session".to_string(),
        state: StartupContextStatusState::Prepared,
        project: Some(StartupContextProjectSnapshot {
            key_digest: "digest".to_string(),
            kind: StartupContextProjectKind::Git,
            active_root: "/project".to_string(),
        }),
        plan_revision: 8,
        plan_entry_count: 2,
        receipt_plan_revision: Some(7),
        receipt_file_count: 1,
        captured_bytes: 64,
        estimated_tokens: 16,
        blocked_issue_count: 0,
        pending_update_count: 0,
        stale_file_count: 0,
        lease: StartupContextLeaseAvailability::Available,
        error: None,
    }
}

#[test]
fn history_defaults_absent_startup_context_to_unsupported() -> Result<()> {
    let event =
        parse_event_json(r#"{"type":"history","id":1,"session_id":"session","messages":[]}"#)?;
    let ServerEvent::History {
        startup_context, ..
    } = event
    else {
        return Err(anyhow!("expected History"));
    };
    assert!(startup_context.is_none());
    Ok(())
}

#[test]
fn history_round_trip_preserves_bounded_startup_status_and_legacy_clients_ignore_it() -> Result<()>
{
    let mut value = serde_json::json!({
        "type": "history",
        "id": 4,
        "session_id": "session",
        "messages": [],
        "startup_context": compact_startup_status(),
    });
    let event: ServerEvent = serde_json::from_value(value.clone())?;
    let ServerEvent::History {
        startup_context: Some(status),
        ..
    } = event
    else {
        return Err(anyhow!("expected History with Startup Context status"));
    };
    assert_eq!(status.state, StartupContextStatusState::Prepared);
    assert_eq!(status.receipt_plan_revision, Some(7));

    #[derive(serde::Deserialize)]
    struct LegacyHistory {
        id: u64,
        session_id: String,
        messages: Vec<HistoryMessage>,
    }
    let legacy: LegacyHistory = serde_json::from_value(value.take())?;
    assert_eq!(legacy.id, 4);
    assert_eq!(legacy.session_id, "session");
    assert!(legacy.messages.is_empty());
    Ok(())
}

#[test]
fn ordinary_startup_events_have_no_raw_content_field() -> Result<()> {
    let status = ServerEvent::StartupContextStatus {
        id: 9,
        snapshot: StartupContextStatusSnapshot {
            compact: compact_startup_status(),
            total_files: 0,
            file_page_start: 0,
            file_page_end: 0,
            next_file_page_start: None,
            files: Vec::new(),
            total_issues: 0,
            issue_page_start: 0,
            issue_page_end: 0,
            next_issue_page_start: None,
            issues: Vec::new(),
        },
        action_required: None,
    };
    let encoded = serde_json::to_string(&status)?;
    assert!(!encoded.contains("content"));

    let apply = ServerEvent::StartupContextApplyStatus {
        id: 11,
        status: StartupContextApplyStatus {
            operation_id: "operation".to_string(),
            session_id: "session".to_string(),
            phase: StartupContextApplyPhase::Succeeded,
            session_target: StartupContextApplyTargetState::Applied { revision: None },
            project_default_target: StartupContextApplyTargetState::Applied { revision: Some(8) },
            batch_id: Some("batch".to_string()),
            file_count: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            failure: None,
        },
    };
    let apply_encoded = serde_json::to_string(&apply)?;
    assert!(!apply_encoded.contains("content"));

    let explicit = ServerEvent::StartupContextFilePreview {
        id: 10,
        preview: StartupContextFilePreview {
            project_key_digest: "digest".to_string(),
            plan_revision: 1,
            logical_path: "PLAN.md".to_string(),
            resolved_path: "/project/PLAN.md".to_string(),
            classification: StartupContextPathClassification::Project,
            requires_external_approval: false,
            sha256: "hash".to_string(),
            bytes: 6,
            estimated_tokens: 2,
            total_chars: 6,
            start_char: 0,
            end_char: 6,
            next_start_char: None,
            truncated: false,
            content: "secret".to_string(),
        },
    };
    assert!(serde_json::to_string(&explicit)?.contains("secret"));
    Ok(())
}

#[test]
fn startup_context_action_required_round_trips_without_raw_prompt_content() -> Result<()> {
    let mut status = compact_startup_status();
    status.state = StartupContextStatusState::Blocked;
    status.blocked_issue_count = 1;
    let event = ServerEvent::StartupContextStatus {
        id: 44,
        snapshot: StartupContextStatusSnapshot {
            compact: status,
            total_files: 0,
            file_page_start: 0,
            file_page_end: 0,
            next_file_page_start: None,
            files: Vec::new(),
            total_issues: 1,
            issue_page_start: 0,
            issue_page_end: 1,
            next_issue_page_start: None,
            issues: vec![StartupContextFileIssueSnapshot {
                input_index: Some(0),
                spec_id: Some("spec".to_string()),
                logical_path: Some("docs/MISSING.md".to_string()),
                kind: StartupContextFileIssueKind::Missing,
            }],
        },
        action_required: Some(StartupContextActionRequired {
            kind: StartupContextActionKind::RequirementsUnresolved,
            prompt_disposition: StartupContextPromptDisposition::RolledBack,
            pending_input: Some(ContextPendingInputMetadata::new(
                44,
                "synthetic secret prompt body",
                2,
            )),
            detail: "request was not sent".to_string(),
        }),
    };
    let encoded = serde_json::to_string(&event)?;
    assert!(!encoded.contains("synthetic secret prompt body"));
    assert!(!encoded.contains("content\":"));
    let decoded: ServerEvent = serde_json::from_str(&encoded)?;
    let ServerEvent::StartupContextStatus {
        id,
        snapshot,
        action_required: Some(action),
    } = decoded
    else {
        panic!("expected Startup Context action response");
    };
    assert_eq!(id, 44);
    assert_eq!(snapshot.total_issues, 1);
    assert_eq!(
        action.prompt_disposition,
        StartupContextPromptDisposition::RolledBack
    );
    assert_eq!(action.pending_input.unwrap().request_id, 44);
    Ok(())
}
