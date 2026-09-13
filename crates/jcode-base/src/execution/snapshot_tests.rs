use super::*;
use crate::message::Role;
use crate::session::{CapturedSession, Session};
use jcode_tool_core::{OutputCapture, OutputStream};
use jcode_tool_types::{RunState, ToolOutput};

#[test]
fn inherited_snapshot_reads_keep_original_ownership_but_touch_only_the_actual_reader() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    source(root.path(), "original_reader", "parent history")?;
    source(root.path(), "target", "snapshot material")?;
    let descendant =
        Session::create_with_id("descendant".into(), Some("original_reader".into()), None);
    persist_source(root.path(), &descendant)?;
    source(root.path(), "unrelated", "unrelated history")?;
    let id = store.create_inspection_snapshot("original_reader", "target", 100)?;
    let inherited = store.read_inspection_snapshot("descendant", &id, 101)?;
    assert_eq!(inherited.manifest.reader, "original_reader");
    assert!(
        inherited
            .transcript(None, true)?
            .contains("snapshot material")
    );
    drop(inherited);
    assert_eq!(store.last_activity("original_reader")?, Some(100));
    assert_eq!(store.last_activity("descendant")?, Some(101));
    assert!(
        store
            .read_inspection_snapshot("unrelated", &id, 102)
            .is_err()
    );
    assert!(
        store
            .read_inspection_snapshot_for_client("unrelated", &id, 102, None)?
            .transcript(None, true)?
            .contains("snapshot material")
    );
    assert_eq!(store.last_activity("original_reader")?, Some(100));
    Ok(())
}

#[test]
fn pruning_reports_committed_ids_even_when_one_unreferenced_blob_cannot_be_reclaimed() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    source(root.path(), "target", "shared snapshot source")?;
    let mut ids = Vec::new();
    for time in 100..104 {
        ids.push(store.create_inspection_snapshot("reader", "target", time)?);
    }
    let shared = store
        .read_inspection_snapshot("reader", &ids[0], 103)?
        .manifest
        .messages[0]
        .digest
        .clone();
    let orphan = store.put_inspection_blob(&serde_json::json!({"unpublished":"synthetic"}))?;
    let orphan_path = store.root().join("snapshots/blobs").join(&orphan);
    let original = std::fs::read(&orphan_path)?;
    std::fs::write(&orphan_path, b"corrupt fixture")?;
    let now = 103 + super::super::IDLE_SECONDS;
    let outcome = store.prune_inspection_snapshots(now)?;
    assert_eq!(outcome.pruned, 2);
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(outcome.errors[0].id, orphan);
    assert!(outcome.reclaimed_blobs >= 2);
    assert_eq!(
        store.last_activity("reader")?,
        Some(103),
        "Housekeeping must not refresh reader activity"
    );
    assert!(store.root().join("snapshots/blobs").join(shared).is_file());
    assert!(
        store
            .read_inspection_snapshot("reader", &ids[0], now)
            .is_err()
    );
    assert!(
        store
            .read_inspection_snapshot("reader", &ids[3], now)?
            .transcript(None, true)?
            .contains("shared snapshot source")
    );
    std::fs::write(&orphan_path, original)?;
    let retried = store.prune_inspection_snapshots(now)?;
    assert_eq!(retried.pruned, 0);
    assert_eq!(retried.reclaimed_blobs, 1);
    assert!(retried.errors.is_empty() && !orphan_path.exists());
    Ok(())
}

#[test]
fn snapshots_preserve_raw_reasoning_media_distillation_and_active_control_identity() -> Result<()> {
    use jcode_session_types::*;
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    let mut session = Session::create_with_id("target".into(), None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "question".into(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "RAW_REASONING".into(),
            },
            ContentBlock::ReasoningTrace {
                text: "RAW_TRACE".into(),
            },
            ContentBlock::ToolUse {
                id: "call".into(),
                name: "fixture".into(),
                input: serde_json::json!({"value":1}),
                thought_signature: None,
            },
        ],
    );
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "call".into(),
                content: "ORIGINAL_RESULT".repeat(100),
                is_error: Some(false),
            },
            ContentBlock::Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
            },
        ],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer".into(),
            cache_control: None,
        }],
    );
    let now = chrono::Utc::now();
    let profile = crate::session::StoredAgentReference {
        scope: crate::instruction::InstructionScope::Global,
        id: "synthetic".into(),
        display_name: "Synthetic".into(),
    };
    session.install_system_prompt(crate::session::StoredSystemPromptState {
        text: "EXACT_SYSTEM".into(),
        active_agent: profile.clone(),
        first_provider_dispatch_at: Some(now),
        active_transition_message_id: None,
    });
    let directive =
        session.append_agent_profile_transition(crate::instruction::AgentProfileTransition {
            agent: profile,
            transition_sentence: "Synthetic transition".into(),
            complete_instructions: "ACTIVE_DIRECTIVE".into(),
            initialized_global_store: false,
        })?;
    let suppression = StoredReasoningSuppression {
        selection: StoredReasoningSelection::MessageRanges { ranges: vec![] },
        targets: vec![jcode_context_core::build_content_target(
            &session.messages,
            1,
            0,
        )?],
        assistant_turns_affected: 1,
        replay_block_kinds: vec![StoredContextBlockKind::Reasoning],
        original_token_estimate: 25,
        validation_evidence_version: 1,
        validation: vec![StoredProviderValidationEvidence {
            provider: "fixture".into(),
            model: "fixture".into(),
            request_builder: "fixture".into(),
            checked_at: now,
            outcome: StoredProviderValidationOutcome::Passed,
            warnings: vec![],
        }],
    };
    let distillation = StoredToolResultDistillation {
        target: jcode_context_core::build_content_target(&session.messages, 2, 0)?,
        tool_name: "fixture".into(),
        tool_call_id: "call".into(),
        replacement_content: "DISTILLED_RESULT".into(),
        original_token_estimate: 1000,
        replacement_token_estimate: 100,
        replacement_ratio_millionths: 100_000,
        preservation_rationale: "Synthetic fixture".into(),
        uncertainties: vec![],
        generator: StoredContextArtifactGenerator {
            provider: "fixture".into(),
            model: "fixture".into(),
            route: "fixture".into(),
            prompt_version: "synthetic".into(),
            effort: None,
            role: None,
            selection_source: None,
            transaction_instructions: None,
            task_instructions: None,
        },
        created_at: now,
    };
    session.context_view = StoredContextViewState {
        revision: 1,
        transactions: vec![StoredContextTransaction {
            id: "transform".into(),
            base_revision: 0,
            created_at: now,
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: vec![
                StoredContextOperation::ReasoningSuppression(suppression),
                StoredContextOperation::ToolResultDistillation(distillation),
            ],
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: now,
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: vec![],
            emergency_audit: None,
        }],
        ..Default::default()
    };
    persist_source(root.path(), &session)?;
    let before = std::fs::read(root.path().join("sessions/target.json"))?;
    let id = store.create_inspection_snapshot("reader", "target", 100)?;
    let snapshot = store.read_inspection_snapshot("reader", &id, 101)?;
    let projected = snapshot.transcript(None, false)?;
    assert!(projected.contains("DISTILLED_RESULT"));
    assert!(!projected.contains("RAW_REASONING"));
    assert!(!projected.contains("ORIGINAL_RESULT"));
    assert!(projected.contains("ACTIVE_DIRECTIVE"));
    let raw = snapshot.transcript(None, true)?;
    assert!(
        raw.contains("RAW_REASONING")
            && raw.contains("RAW_TRACE")
            && raw.contains("ORIGINAL_RESULT")
    );
    assert!(raw.contains("AA==") && raw.contains(&directive) && raw.contains("EXACT_SYSTEM"));
    assert!(
        snapshot
            .expand_tool("tool-2-3")?
            .contains("ORIGINAL_RESULT")
    );
    assert_eq!(
        std::fs::read(root.path().join("sessions/target.json"))?,
        before
    );
    Ok(())
}

fn source(root: &std::path::Path, id: &str, body: &str) -> Result<CapturedSession> {
    let mut session = Session::create_with_id(id.into(), None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: body.into(),
            cache_control: None,
        }],
    );
    persist_source(root, &session)
}
fn persist_source(root: &std::path::Path, session: &Session) -> Result<CapturedSession> {
    // Synthetic idle source, captured through the production read-only owner.
    crate::storage::write_json_secret(
        &root.join("sessions").join(format!("{}.json", session.id)),
        session,
    )?;
    Session::capture_readonly(root, &session.id)
}

#[test]
fn immutable_snapshots_share_content_and_survive_reopen_source_rewrite_and_pruning() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    let captured = source(
        root.path(),
        "target",
        &format!("{}TAIL", "α".repeat(50_000)),
    )?;
    let first = store.create_inspection_snapshot("reader", &captured.session().id, 100)?;
    let second = store.create_inspection_snapshot("reader", &captured.session().id, 101)?;
    let read = store.read_inspection_snapshot("reader", &first, 102)?;
    let original = read.transcript(None, true)?;
    let first_manifest = &read.manifest;
    let second_read = store.read_inspection_snapshot("reader", &second, 102)?;
    assert_eq!(
        first_manifest.messages[0].digest,
        second_read.manifest.messages[0].digest
    );
    assert_eq!(
        first_manifest.projected[0].digest,
        second_read.manifest.projected[0].digest
    );
    let snapshot_bytes = std::fs::read(root.path().join("sessions/target.json"))?;
    assert!(read.outline()?.contains("TAIL"));
    assert_eq!(
        std::fs::read(root.path().join("sessions/target.json"))?,
        snapshot_bytes
    );
    drop(second_read);
    drop(read);
    source(root.path(), "target", "rewound and replaced")?;
    let reopened = ExecutionStore::open(root.path())?;
    assert_eq!(
        reopened
            .read_inspection_snapshot("reader", &first, 103)?
            .transcript(None, true)?,
        original
    );
    assert!(
        reopened
            .read_inspection_snapshot("different-reader", &first, 103)
            .is_err()
    );
    assert!(
        reopened
            .read_inspection_snapshot("reader", "../target", 103)
            .is_err()
    );
    Ok(())
}

#[test]
fn pruning_keeps_newest_two_per_target_and_respects_reader_activity_and_live_reads() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    let mut ids = Vec::new();
    for target in 0..10 {
        let captured = source(
            root.path(),
            &format!("target_{target}"),
            "synthetic content",
        )?;
        for ordinal in 0..4 {
            ids.push(store.create_inspection_snapshot(
                "reader",
                &captured.session().id,
                100 + ordinal,
            )?);
        }
    }
    let now = 104 + super::super::IDLE_SECONDS;
    let protected = store.read_inspection_snapshot("reader", &ids[0], 104)?;
    let delivered = protected.transcript(None, false)?;
    assert_eq!(store.prune_inspection_snapshots(now)?.pruned, 0);
    drop(protected);
    assert_eq!(store.prune_inspection_snapshots(now)?.pruned, 20);
    let retained: i64 = store.connection()?.query_row(
        "SELECT count(*) FROM inspection_snapshots WHERE state='retained'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retained, 20);
    for group in ids.chunks(4) {
        assert!(
            store
                .read_inspection_snapshot("reader", &group[0], now)
                .is_err()
        );
        assert!(
            store
                .read_inspection_snapshot("reader", &group[1], now)
                .is_err()
        );
        assert!(
            store
                .read_inspection_snapshot("reader", &group[2], now)
                .is_ok()
        );
        assert!(
            store
                .read_inspection_snapshot("reader", &group[3], now)
                .is_ok()
        );
    }
    assert!(delivered.contains("synthetic content"));
    assert_eq!(store.prune_inspection_snapshots(now)?.pruned, 0);
    assert!(
        store.last_activity("target_0")?.is_none(),
        "inspection activity belongs only to reader"
    );
    Ok(())
}

#[test]
fn tool_expansion_freezes_running_prefix_and_uses_scoped_reference() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    let mut session = Session::create_with_id("target".into(), None, None);
    let input = serde_json::json!({"command":"synthetic", "intent":"fixture"});
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "same-provider-id".into(),
            name: "fixture".into(),
            input: input.clone(),
            thought_signature: None,
        }],
    );
    let invocation = Invocation {
        session_id: session.id.clone(),
        message_id: session.messages[0].id.clone(),
        call_path: vec!["same-provider-id".into()],
        tool: "fixture".into(),
        input,
        working_dir: None,
        received_result_digest: None,
    };
    let super::super::PreparedInvocation::New(run) = store.prepare(&invocation, "fixture")? else {
        panic!()
    };
    store.start(&run.id, "fixture")?;
    let capture = super::super::Capture::create(store.clone(), run, Default::default())?;
    capture.write(OutputStream::Text, b"PREFIX\n")?;
    let captured = persist_source(root.path(), &session)?;
    let id = store.create_inspection_snapshot("reader", &captured.session().id, 100)?;
    capture.write(OutputStream::Text, b"LATER_TAIL\n")?;
    let read = store.read_inspection_snapshot("reader", &id, 101)?;
    let expanded: serde_json::Value = serde_json::from_str(&read.expand_tool("tool-1-1")?)?;
    assert_eq!(expanded["retained_output"], "PREFIX\n");
    assert_eq!(expanded["as_of_run"]["state"], "running");
    assert!(read.expand_tool("same-provider-id").is_err());
    let mut result = ToolOutput::new("");
    result.source = jcode_tool_types::OutputSource::Retained(capture.reference()?);
    capture.seal(result, RunState::Completed)?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read.expand_tool("tool-1-1")?)?["retained_output"],
        "PREFIX\n"
    );
    Ok(())
}

#[test]
fn projected_range_returns_whole_intersecting_summary_and_raw_is_unchanged() -> Result<()> {
    use jcode_session_types::*;
    let root = tempfile::tempdir()?;
    let store = ExecutionStore::open(root.path())?;
    let mut session = Session::create_with_id("target".into(), None, None);
    for index in 0..4 {
        session.add_message(
            if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            vec![ContentBlock::Text {
                text: format!("RAW_{index}"),
                cache_control: None,
            }],
        );
    }
    let timestamp = chrono::Utc::now();
    let summary = StoredRangeSummary {
        source_range: jcode_context_core::build_message_range(&session.messages, 0, 2)?,
        summary_text: "SYNTHETIC_SUMMARY".into(),
        file_change_digest: String::new(),
        changed_files: vec![],
        change_evidence_complete: false,
        file_evidence: None,
        boundary_expansions: vec![],
        generator: None,
        source_token_estimate: 100,
        replacement_token_estimate: 10,
        warnings: vec![],
        created_at: timestamp,
        legacy_coverage: None,
    };
    session.context_view = StoredContextViewState {
        revision: 1,
        transactions: vec![StoredContextTransaction {
            id: "summary-transaction".into(),
            base_revision: 0,
            created_at: timestamp,
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: vec![StoredContextOperation::RangeSummary(summary)],
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp,
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: vec![],
            emergency_audit: None,
        }],
        ..Default::default()
    };
    let captured = persist_source(root.path(), &session)?;
    let id = store.create_inspection_snapshot("reader", &captured.session().id, 100)?;
    let read = store.read_inspection_snapshot("reader", &id, 101)?;
    let range = TranscriptRange { start: 2, end: 2 };
    let projected: serde_json::Value =
        serde_json::from_str(&read.transcript(Some(&range), false)?)?;
    assert_eq!(
        projected["messages"][0]["source_range"],
        serde_json::json!({"start":1,"end":3})
    );
    assert!(projected.to_string().contains("SYNTHETIC_SUMMARY"));
    assert!(!projected.to_string().contains("RAW_1"));
    let raw = read.transcript(Some(&range), true)?;
    assert!(raw.contains("RAW_1"));
    assert!(!raw.contains("SYNTHETIC_SUMMARY"));
    assert!(
        read.transcript(Some(&TranscriptRange { start: 0, end: 1 }), false)
            .is_err()
    );
    Ok(())
}
