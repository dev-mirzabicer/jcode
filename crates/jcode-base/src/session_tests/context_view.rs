use super::*;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use jcode_context_core::{build_content_target, build_message_range};
use jcode_session_types::{
    StoredCompactionState, StoredContextApplication, StoredContextArtifactGenerator,
    StoredContextAuthorization, StoredContextBillingMode, StoredContextCacheWarmth,
    StoredContextEconomics, StoredContextEmergencyAudit, StoredContextEmergencyOperationKind,
    StoredContextEmergencyPolicy, StoredContextEmergencyRetryOutcome,
    StoredContextEmergencyTriggerKind, StoredContextOperation, StoredContextPricingSnapshot,
    StoredContextStatusEvent, StoredContextTransaction, StoredContextTransactionStatusKind,
    StoredContextViewState, StoredLegacyCompactionCoverage, StoredLegacyContextSource,
    StoredProviderValidationEvidence, StoredProviderValidationOutcome, StoredRangeSummary,
    StoredReasoningSelection, StoredReasoningSuppression, StoredToolResultDistillation,
};

struct IsolatedHome {
    _home: EnvVarGuard,
    temp: tempfile::TempDir,
    _env_lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn new(prefix: &str) -> Result<Self> {
        let env_lock = lock_env();
        let temp = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| anyhow!(error))?;
        let home = EnvVarGuard::set("JCODE_HOME", temp.path().as_os_str());
        Ok(Self {
            _home: home,
            temp,
            _env_lock: env_lock,
        })
    }
}

fn timestamp() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-10T12:34:56Z")
        .expect("valid test timestamp")
        .with_timezone(&Utc)
}

fn stored(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        id: id.to_string(),
        role,
        content,
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn text(id: &str, role: Role, value: &str) -> StoredMessage {
    stored(
        id,
        role,
        vec![ContentBlock::Text {
            text: value.to_string(),
            cache_control: None,
        }],
    )
}

fn text_transcript(count: usize) -> Vec<StoredMessage> {
    (0..count)
        .map(|index| {
            text(
                &format!("message-{index}"),
                if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                &format!("message body {index}"),
            )
        })
        .collect()
}

fn append_all(session: &mut Session, messages: &[StoredMessage]) {
    for message in messages {
        session.append_stored_message(message.clone());
    }
}

fn assert_serialized_eq<T: serde::Serialize>(left: &T, right: &T) -> Result<()> {
    assert_eq!(serde_json::to_vec(left)?, serde_json::to_vec(right)?);
    Ok(())
}

fn generator() -> StoredContextArtifactGenerator {
    StoredContextArtifactGenerator {
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        route: "test-route".to_string(),
        prompt_version: "context-curator-v1".to_string(),
        effort: Some("high".to_string()),
    }
}

fn range_summary(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
    summary_text: &str,
) -> StoredRangeSummary {
    StoredRangeSummary {
        source_range: build_message_range(messages, start, end).expect("valid test range"),
        summary_text: summary_text.to_string(),
        file_change_digest: String::new(),
        changed_files: Vec::new(),
        change_evidence_complete: false,
        boundary_expansions: Vec::new(),
        generator: Some(generator()),
        source_token_estimate: 1_000,
        replacement_token_estimate: 100,
        warnings: Vec::new(),
        created_at: timestamp(),
        legacy_coverage: None,
    }
}

fn applied_state_with_id(
    id: &str,
    operations: Vec<StoredContextOperation>,
) -> StoredContextViewState {
    StoredContextViewState {
        revision: 1,
        transactions: vec![StoredContextTransaction {
            id: id.to_string(),
            base_revision: 0,
            created_at: timestamp(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations,
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: timestamp(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        }],
        ..StoredContextViewState::default()
    }
}

fn applied_summary_state(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
    summary_text: &str,
) -> StoredContextViewState {
    applied_state_with_id(
        "transaction-summary",
        vec![StoredContextOperation::RangeSummary(range_summary(
            messages,
            start,
            end,
            summary_text,
        ))],
    )
}

fn reverted_state(mut state: StoredContextViewState) -> StoredContextViewState {
    state.revision = 2;
    state.transactions[0]
        .status_events
        .push(StoredContextStatusEvent {
            revision: 2,
            timestamp: timestamp(),
            kind: StoredContextTransactionStatusKind::Reverted,
            reason: Some("test revert".to_string()),
        });
    state
}

fn legacy_compaction(
    summary_text: &str,
    compacted_count: usize,
    encrypted: Option<&str>,
) -> StoredCompactionState {
    StoredCompactionState {
        summary_text: summary_text.to_string(),
        openai_encrypted_content: encrypted.map(str::to_string),
        covers_up_to_turn: compacted_count,
        original_turn_count: compacted_count.saturating_add(2),
        compacted_count,
    }
}

fn assert_projected_contains_summary(
    session: &mut Session,
    expected_summary: &str,
    expected_message_count: usize,
) -> Result<()> {
    let projected = session.projected_provider_messages()?;
    assert_eq!(projected.len(), expected_message_count);
    let summary_text = projected[0]
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .expect("projected summary text");
    assert!(summary_text.contains(expected_summary));
    Ok(())
}

#[test]
fn default_context_state_is_backward_compatible_omitted_and_raw_immutable() -> Result<()> {
    let mut session = Session::create_with_id(
        "session_context_default_compatibility".to_string(),
        None,
        Some("context compatibility".to_string()),
    );
    append_all(&mut session, &text_transcript(3));

    let raw_before = serde_json::to_vec(&session.messages)?;
    let serialized = serde_json::to_value(&session)?;
    assert!(serialized.get("context_view").is_none());

    let mut old_shape = serialized;
    old_shape
        .as_object_mut()
        .expect("session object")
        .remove("context_view");
    let mut restored: Session = serde_json::from_value(old_shape)?;
    assert!(restored.context_view.is_default());
    assert_eq!(serde_json::to_vec(&restored.messages)?, raw_before);

    // Authoritative load finalization invalidates every skipped derived cache before use.
    restored.reset_provider_messages_cache();
    let raw_provider = restored.raw_messages_for_provider_uncached();
    let projected = restored.projected_provider_messages()?.to_vec();
    assert_serialized_eq(&projected, &raw_provider)?;
    assert_eq!(serde_json::to_vec(&restored.messages)?, raw_before);
    Ok(())
}

#[test]
fn projected_cache_reuses_appends_rebuilds_and_preserves_valid_cache_on_failure() -> Result<()> {
    let raw = text_transcript(5);
    let mut session = Session::create_with_id(
        "session_projected_cache".to_string(),
        None,
        Some("projected cache".to_string()),
    );
    append_all(&mut session, &raw);
    session.context_view = applied_summary_state(&session.messages, 0, 1, "cached summary");

    let first = session.projected_provider_messages()?.to_vec();
    assert_eq!(first.len(), 4);
    let first_pointer = session.projected_provider_messages()?.as_ptr();
    let second_pointer = session.projected_provider_messages()?.as_ptr();
    assert_eq!(
        first_pointer, second_pointer,
        "unchanged cache must be reused"
    );
    assert_eq!(session.projected_provider_messages_cache_revision, 1);
    assert_eq!(session.projected_provider_messages_cache_len, 5);

    let prefix_before = session.projected_provider_message_prefix_hashes()?.to_vec();
    session.append_stored_message(text("message-5", Role::Assistant, "appended"));
    assert_eq!(session.projected_provider_messages_cache_len, 5);
    let appended = session.projected_provider_messages()?.to_vec();
    assert_eq!(appended.len(), 5);
    assert_eq!(
        &session.projected_provider_message_prefix_hashes()?[..prefix_before.len()],
        prefix_before.as_slice(),
        "append fast path must preserve every existing projected prefix hash"
    );
    assert_eq!(session.projected_provider_messages_cache_len, 6);

    let valid_cache = session.projected_provider_messages_cache.clone();
    let valid_hashes = session
        .projected_provider_message_prefix_hashes_cache
        .clone();
    let valid_revision = session.projected_provider_messages_cache_revision;
    let mut invalid = session.context_view.clone();
    invalid.revision = 2;
    let mut duplicate = invalid.transactions[0].clone();
    duplicate.base_revision = 1;
    duplicate.status_events = vec![StoredContextStatusEvent {
        revision: 2,
        timestamp: timestamp(),
        kind: StoredContextTransactionStatusKind::Applied,
        reason: None,
    }];
    invalid.transactions.push(duplicate);
    session.context_view = invalid;

    assert!(session.projected_provider_messages().is_err());
    assert_serialized_eq(&session.projected_provider_messages_cache, &valid_cache)?;
    assert_eq!(
        session.projected_provider_message_prefix_hashes_cache,
        valid_hashes
    );
    assert_eq!(
        session.projected_provider_messages_cache_revision,
        valid_revision
    );

    session.context_view = reverted_state(applied_summary_state(
        &session.messages,
        0,
        1,
        "cached summary",
    ));
    let reverted = session.projected_provider_messages()?.to_vec();
    assert_serialized_eq(&reverted, &session.raw_messages_for_provider_uncached())?;
    assert_eq!(session.projected_provider_messages_cache_revision, 2);
    assert_eq!(
        session
            .memory_profile_snapshot()
            .projected_provider_cache_message_count,
        reverted.len(),
        "a revision rebuild must replace, not accumulate, cache accounting"
    );

    let replacement = vec![text("replacement", Role::User, "replacement transcript")];
    session.replace_messages(replacement);
    session.context_view = StoredContextViewState::default();
    let rebuilt = session.projected_provider_messages()?.to_vec();
    assert_serialized_eq(&rebuilt, &session.raw_messages_for_provider_uncached())?;
    assert_eq!(rebuilt.len(), 1);
    Ok(())
}

#[test]
fn context_state_round_trips_through_snapshot_journal_stub_and_remote_startup() -> Result<()> {
    let home = IsolatedHome::new("jcode-context-persistence-")?;
    let id = "session_context_persistence";
    let mut session = Session::create_with_id(
        id.to_string(),
        None,
        Some("context persistence".to_string()),
    );
    append_all(&mut session, &text_transcript(4));
    session.context_view = applied_summary_state(
        &session.messages,
        0,
        1,
        "Unicode summary: İstanbul, 日本語, 🧪",
    );
    session.save()?;

    let snapshot_path = session_path(id)?;
    let journal_path = session_journal_path(id)?;
    assert!(snapshot_path.exists());
    assert!(!journal_path.exists());

    session.append_stored_message(text("message-4", Role::User, "journal append"));
    session.save()?;
    assert!(journal_path.exists());

    let mut loaded = Session::load(id)?;
    assert_eq!(loaded.context_view, session.context_view);
    assert_serialized_eq(&loaded.messages, &session.messages)?;
    assert_projected_contains_summary(&mut loaded, "İstanbul, 日本語, 🧪", 4)?;

    loaded.context_view = reverted_state(loaded.context_view.clone());
    loaded.save()?;
    assert!(snapshot_path.exists());
    assert!(
        !journal_path.exists(),
        "context revision changes must checkpoint and remove a stale journal"
    );

    let startup = Session::load_startup_stub(id)?;
    assert_eq!(startup.context_view, loaded.context_view);
    assert!(startup.messages.is_empty());

    let remote = Session::load_for_remote_startup(id)?;
    assert_eq!(remote.context_view, loaded.context_view);
    assert_serialized_eq(&remote.messages, &loaded.messages)?;

    let mut old_meta = serde_json::to_value(loaded.journal_meta())?;
    old_meta
        .as_object_mut()
        .expect("journal metadata object")
        .remove("context_view");
    let decoded: super::super::journal::SessionJournalMeta = serde_json::from_value(old_meta)?;
    assert!(decoded.context_view.is_default());

    assert!(home.temp.path().join("sessions").exists());
    Ok(())
}

#[test]
fn valid_legacy_text_migration_is_exact_deterministic_idempotent_and_checkpointed() -> Result<()> {
    let _home = IsolatedHome::new("jcode-context-legacy-text-")?;
    let id = "session_legacy_text_migration";
    let raw = text_transcript(8);
    let summary =
        "Previous recursive summary preserved exactly.\nDecision: do not lose this text.\n🧭";
    let compaction = legacy_compaction(summary, 5, Some("opaque-native-state"));

    let mut session = Session::create_with_id(
        id.to_string(),
        None,
        Some("legacy text migration".to_string()),
    );
    append_all(&mut session, &raw);
    session.save()?;
    session.compaction = Some(compaction.clone());
    session.provider_session_id = Some("stale-legacy-provider-session".to_string());
    session.save()?;

    let journal_path = session_journal_path(id)?;
    assert!(
        journal_path.exists(),
        "legacy compaction metadata newer than the snapshot must be replayed"
    );
    let raw_before = serde_json::to_vec(&raw)?;

    let mut loaded = Session::load(id)?;
    assert_eq!(serde_json::to_vec(&loaded.messages)?, raw_before);
    assert!(loaded.compaction.is_none());
    assert!(loaded.provider_session_id.is_none());
    assert_eq!(loaded.context_view.revision, 1);
    assert_eq!(loaded.context_view.transactions.len(), 1);
    assert!(
        !journal_path.exists(),
        "migration must checkpoint atomically"
    );

    let transaction = &loaded.context_view.transactions[0];
    let transaction_id = transaction.id.clone();
    assert!(matches!(
        transaction.authorization,
        StoredContextAuthorization::LegacyMigration {
            source: StoredLegacyContextSource::JcodeTextCompaction
        }
    ));
    let StoredContextOperation::RangeSummary(imported) = &transaction.operations[0] else {
        return Err(anyhow!("expected imported range summary"));
    };
    assert_eq!(imported.summary_text, summary);
    assert_eq!(
        imported.legacy_coverage,
        Some(StoredLegacyCompactionCoverage {
            covers_up_to_turn: 5,
            original_turn_count: 7,
            compacted_count: 5,
        })
    );
    assert_eq!(imported.source_range.start_index_hint, 0);
    assert_eq!(imported.source_range.end_index_hint, 4);
    assert_eq!(imported.source_range.message_count, 5);
    assert!(imported.replacement_token_estimate > 0);
    assert_projected_contains_summary(&mut loaded, summary, 4)?;

    let startup = Session::load_startup_stub(id)?;
    assert_eq!(startup.context_view, loaded.context_view);

    let loaded_again = Session::load(id)?;
    assert_eq!(loaded_again.context_view, loaded.context_view);
    assert_eq!(loaded_again.context_view.transactions.len(), 1);
    assert_eq!(loaded_again.context_view.transactions[0].id, transaction_id);
    assert!(loaded_again.compaction.is_none());
    assert!(loaded_again.provider_session_id.is_none());

    let mut first = Session::create_with_id(id.to_string(), None, None);
    first.updated_at = timestamp();
    append_all(&mut first, &raw);
    first.compaction = Some(compaction.clone());
    let first_outcome = first.migrate_legacy_compaction_state();
    let mut second = Session::create_with_id(id.to_string(), None, None);
    second.updated_at = timestamp();
    append_all(&mut second, &raw);
    second.compaction = Some(compaction);
    let second_outcome = second.migrate_legacy_compaction_state();
    assert_eq!(first_outcome, second_outcome);
    assert_eq!(first.context_view, second.context_view);
    assert!(first.compaction.is_none());
    assert!(second.compaction.is_none());
    Ok(())
}

#[test]
fn already_migrated_context_retires_duplicated_legacy_state_and_provider_session() -> Result<()> {
    let raw = text_transcript(5);
    let mut session = Session::create_with_id("retire-legacy-duplicate".to_string(), None, None);
    append_all(&mut session, &raw);
    session.context_view = applied_summary_state(&session.messages, 0, 1, "migrated summary");
    session.context_view.transactions[0].authorization =
        StoredContextAuthorization::LegacyMigration {
            source: StoredLegacyContextSource::JcodeTextCompaction,
        };
    session.compaction = Some(legacy_compaction("migrated summary", 2, None));
    session.provider_session_id = Some("stale-provider-session".to_string());
    let raw_before = serde_json::to_vec(&session.messages)?;
    let context_before = session.context_view.clone();

    let outcome = session.migrate_legacy_compaction_state();

    assert_eq!(
        outcome,
        LegacyContextMigrationOutcome::RetiredMigratedLegacyState
    );
    assert!(outcome.changed_state());
    assert!(session.compaction.is_none());
    assert!(session.provider_session_id.is_none());
    assert_eq!(session.context_view, context_before);
    assert_eq!(serde_json::to_vec(&session.messages)?, raw_before);
    Ok(())
}

#[test]
fn remote_startup_migrates_in_memory_without_becoming_an_unexpected_writer() -> Result<()> {
    let _home = IsolatedHome::new("jcode-context-remote-migration-")?;
    let id = "session_remote_legacy_migration";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    append_all(&mut session, &text_transcript(6));
    session.compaction = Some(legacy_compaction("remote legacy summary", 3, None));
    session.save()?;

    let remote = Session::load_for_remote_startup(id)?;
    assert_eq!(remote.context_view.revision, 1);
    assert_eq!(remote.context_view.transactions.len(), 1);
    assert!(remote.compaction.is_none());

    let before_authoritative_load = Session::load_startup_stub(id)?;
    assert!(before_authoritative_load.context_view.is_default());
    assert!(before_authoritative_load.compaction.is_some());

    let authoritative = Session::load(id)?;
    assert_eq!(authoritative.context_view, remote.context_view);
    let durable_stub = Session::load_startup_stub(id)?;
    assert_eq!(durable_stub.context_view, authoritative.context_view);
    Ok(())
}

#[test]
fn invalid_legacy_states_restore_raw_without_inventing_or_guessing() -> Result<()> {
    let raw = text_transcript(3);
    let cases = vec![
        (
            legacy_compaction("", 2, Some("opaque")),
            LegacyContextMigrationIssue::EncryptedOnly,
        ),
        (
            legacy_compaction("", 0, None),
            LegacyContextMigrationIssue::EmptyState,
        ),
        (
            legacy_compaction("invalid count", 9, None),
            LegacyContextMigrationIssue::InvalidCompactedCount {
                compacted_count: 9,
                message_count: 3,
            },
        ),
        (
            legacy_compaction("zero count", 0, None),
            LegacyContextMigrationIssue::InvalidCompactedCount {
                compacted_count: 0,
                message_count: 3,
            },
        ),
    ];

    for (index, (compaction, expected_issue)) in cases.into_iter().enumerate() {
        let mut session = Session::create_with_id(format!("invalid-legacy-{index}"), None, None);
        append_all(&mut session, &raw);
        session.compaction = Some(compaction);
        let raw_before = serde_json::to_vec(&session.messages)?;
        let outcome = session.migrate_legacy_compaction_state();
        assert_eq!(
            outcome,
            LegacyContextMigrationOutcome::RestoredRaw {
                issue: expected_issue
            }
        );
        assert!(outcome.changed_state());
        assert!(session.compaction.is_none());
        assert!(session.context_view.is_default());
        assert!(session.context_view.transactions.is_empty());
        assert_eq!(serde_json::to_vec(&session.messages)?, raw_before);
        assert_serialized_eq(
            &session.projected_provider_messages()?.to_vec(),
            &session.raw_messages_for_provider_uncached(),
        )?;
    }

    let mut split_pair = Session::create_with_id("split-tool-pair".to_string(), None, None);
    split_pair.append_stored_message(stored(
        "call-message",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool-call".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command":"true"}),
            thought_signature: Some("thought-signature".to_string()),
        }],
    ));
    split_pair.append_stored_message(stored(
        "result-message",
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool-call".to_string(),
            content: "result".to_string(),
            is_error: None,
        }],
    ));
    split_pair.compaction = Some(legacy_compaction("must not guess", 1, None));
    let outcome = split_pair.migrate_legacy_compaction_state();
    assert_eq!(
        outcome,
        LegacyContextMigrationOutcome::RestoredRaw {
            issue: LegacyContextMigrationIssue::RangeNotStructurallyClosed {
                stored_end: 0,
                required_start: 0,
                required_end: 1,
            }
        }
    );
    assert!(split_pair.compaction.is_none());
    assert!(split_pair.context_view.transactions.is_empty());
    Ok(())
}

#[test]
fn invalid_legacy_states_checkpoint_once_and_stay_stable_after_reload() -> Result<()> {
    let _home = IsolatedHome::new("jcode-context-invalid-legacy-persistence-")?;
    let fixtures = [
        (
            "encrypted-only",
            legacy_compaction("", 2, Some("opaque-native-state")),
        ),
        ("invalid-count", legacy_compaction("invalid", 20, None)),
        ("empty", legacy_compaction("", 0, None)),
    ];

    for (name, compaction) in fixtures {
        let id = format!("session_invalid_legacy_{name}");
        let mut session = Session::create_with_id(id.clone(), None, None);
        append_all(&mut session, &text_transcript(4));
        session.compaction = Some(compaction);
        session.save()?;

        let first = Session::load(&id)?;
        assert!(first.compaction.is_none());
        assert!(first.context_view.transactions.is_empty());
        let snapshot_after_first = std::fs::read(session_path(&id)?)?;

        let second = Session::load(&id)?;
        assert!(second.compaction.is_none());
        assert_eq!(second.context_view, first.context_view);
        assert_eq!(std::fs::read(session_path(&id)?)?, snapshot_after_first);
    }
    Ok(())
}

#[test]
fn blocked_legacy_migration_never_claims_or_checkpoints_a_state_change() -> Result<()> {
    let raw = text_transcript(4);
    let compaction = legacy_compaction("blocked migration", 2, None);

    let mut invalid_context = Session::create_with_id("invalid-context".to_string(), None, None);
    append_all(&mut invalid_context, &raw);
    invalid_context.context_view = applied_summary_state(&raw, 0, 0, "existing");
    invalid_context
        .context_view
        .transactions
        .push(invalid_context.context_view.transactions[0].clone());
    invalid_context.compaction = Some(compaction.clone());
    let before = serde_json::to_vec(&invalid_context)?;
    let outcome = invalid_context.migrate_legacy_compaction_state();
    assert!(matches!(
        outcome,
        LegacyContextMigrationOutcome::Blocked {
            issue: LegacyContextMigrationIssue::InvalidContextState(_)
        }
    ));
    assert!(!outcome.changed_state());
    assert_eq!(serde_json::to_vec(&invalid_context)?, before);

    let mut exhausted = Session::create_with_id("revision-exhausted".to_string(), None, None);
    append_all(&mut exhausted, &raw);
    exhausted.context_view.revision = u64::MAX;
    exhausted.compaction = Some(compaction);
    let before = serde_json::to_vec(&exhausted)?;
    let outcome = exhausted.migrate_legacy_compaction_state();
    assert_eq!(
        outcome,
        LegacyContextMigrationOutcome::Blocked {
            issue: LegacyContextMigrationIssue::RevisionExhausted
        }
    );
    assert!(!outcome.changed_state());
    assert_eq!(serde_json::to_vec(&exhausted)?, before);
    Ok(())
}

#[test]
fn context_export_redaction_is_exhaustive_and_does_not_mutate_or_persist_redactions() -> Result<()>
{
    const SECRET: &str = "OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789";
    let _home = IsolatedHome::new("jcode-context-export-redaction-")?;
    let id = "session_context_export_redaction";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    session.append_stored_message(text("source", Role::User, "source message"));
    session.append_stored_message(stored(
        "reasoning",
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "replay reasoning".to_string(),
            },
            ContentBlock::Text {
                text: "answer".to_string(),
                cache_control: None,
            },
        ],
    ));
    session.append_stored_message(stored(
        "tool-call",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command":"true"}),
            thought_signature: None,
        }],
    ));
    session.append_stored_message(stored(
        "tool-result",
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call".to_string(),
            content: "original tool result".repeat(100),
            is_error: Some(false),
        }],
    ));

    let reasoning_target = build_content_target(&session.messages, 1, 0)?;
    let distillation_target = build_content_target(&session.messages, 3, 0)?;
    let mut summary = range_summary(&session.messages, 0, 0, SECRET);
    summary.file_change_digest = SECRET.to_string();
    summary.changed_files = vec![SECRET.to_string()];
    summary.warnings = vec![SECRET.to_string()];
    let suppression = StoredReasoningSuppression {
        selection: StoredReasoningSelection::MessageRanges { ranges: Vec::new() },
        targets: vec![reasoning_target],
        assistant_turns_affected: 1,
        replay_block_kinds: vec![jcode_session_types::StoredContextBlockKind::Reasoning],
        original_token_estimate: 25,
        validation_evidence_version: 1,
        validation: vec![StoredProviderValidationEvidence {
            provider: "provider".to_string(),
            model: "model".to_string(),
            request_builder: "builder".to_string(),
            checked_at: timestamp(),
            outcome: StoredProviderValidationOutcome::Passed,
            warnings: vec![SECRET.to_string()],
        }],
    };
    let distillation = StoredToolResultDistillation {
        target: distillation_target,
        tool_name: "bash".to_string(),
        tool_call_id: "call".to_string(),
        replacement_content: SECRET.to_string(),
        original_token_estimate: 1_000,
        replacement_token_estimate: 100,
        replacement_ratio_millionths: 100_000,
        preservation_rationale: SECRET.to_string(),
        uncertainties: vec![SECRET.to_string()],
        generator: generator(),
        created_at: timestamp(),
    };
    let source_digest = summary.source_range.source_digest;
    session.context_view = StoredContextViewState {
        revision: 1,
        transactions: vec![StoredContextTransaction {
            id: "redaction-transaction".to_string(),
            base_revision: 0,
            created_at: timestamp(),
            authorization: StoredContextAuthorization::UnattendedEmergency {
                authorization_source: SECRET.to_string(),
                trigger: Some(SECRET.to_string()),
                scheduled_item_id: Some("sched-public-id".to_string()),
            },
            operations: vec![
                StoredContextOperation::RangeSummary(summary),
                StoredContextOperation::ReasoningSuppression(suppression),
                StoredContextOperation::ToolResultDistillation(distillation),
            ],
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: timestamp(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: Some(SECRET.to_string()),
            }],
            application: Some(StoredContextApplication {
                provider: "provider".to_string(),
                model: "model".to_string(),
                route: "route".to_string(),
                context_window: Some(372_000),
            }),
            economics: Some(StoredContextEconomics {
                projected_tokens_before: 10_000,
                projected_tokens_after: 2_000,
                estimated_total_request_tokens_before: Some(28_000),
                estimated_total_request_tokens_after: Some(20_000),
                unchanged_prefix_items: 0,
                earliest_changed_provider_item: Some(0),
                old_affected_suffix_tokens: 10_000,
                new_affected_suffix_tokens: 2_000,
                deleted_input_tokens: 8_000,
                context_window: Some(372_000),
                safe_input_budget: Some(370_000),
                pricing: Some(StoredContextPricingSnapshot {
                    billing_mode: StoredContextBillingMode::Metered,
                    input_usd_per_million: Some(5.0),
                    output_usd_per_million: Some(30.0),
                    cache_read_usd_per_million: Some(0.5),
                    cache_write_usd_per_million: Some(6.25),
                    input_price_tiers: Vec::new(),
                    cache_warmth: StoredContextCacheWarmth::Warm,
                }),
                first_request_delta_usd: Some(0.01),
                recurring_savings_per_turn_usd: Some(0.004),
                break_even_turns: Some(3),
                assumptions: vec![SECRET.to_string()],
            }),
            curator_usage: Vec::new(),
            emergency_audit: Some(StoredContextEmergencyAudit {
                authorization_source: SECRET.to_string(),
                scheduled_item_id: Some("sched-public-id".to_string()),
                policy: StoredContextEmergencyPolicy::Authorized {
                    protected_recent_assistant_turns: 5,
                    target_headroom_percent: 10,
                    allow_reasoning_suppression: true,
                    allow_tool_distillation: true,
                    allow_oldest_range_summary: true,
                    authorization_source: SECRET.to_string(),
                },
                trigger_kind: StoredContextEmergencyTriggerKind::ProviderContextLimit,
                provider_error: Some(SECRET.to_string()),
                context_window: 372_000,
                safe_input_budget: 367_904,
                projected_input_tokens: 370_000,
                required_reduction_to_fit_tokens: 2_096,
                required_reduction_to_target_tokens: 75_683,
                achieved_reduction_tokens: 80_000,
                protected_recent_assistant_turns: 5,
                protected_message_count: 8,
                operation_order: vec![
                    StoredContextEmergencyOperationKind::ReasoningSuppression,
                    StoredContextEmergencyOperationKind::ToolResultDistillation,
                    StoredContextEmergencyOperationKind::OldestRangeSummary,
                ],
                retry_outcome: StoredContextEmergencyRetryOutcome::Failed {
                    detail: SECRET.to_string(),
                },
            }),
        }],
        emergency_policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 10,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: SECRET.to_string(),
        },
        ..StoredContextViewState::default()
    };

    let original = session.clone();
    session.save()?;
    let redacted = session.redacted_for_export();
    let redacted_json = serde_json::to_string(&redacted)?;
    assert!(!redacted_json.contains(SECRET));
    assert!(redacted_json.contains("[REDACTED_SECRET]"));
    assert!(redacted_json.contains("redaction-transaction"));
    assert_eq!(
        redacted.context_view.transactions[0].operations.len(),
        original.context_view.transactions[0].operations.len()
    );
    let StoredContextOperation::RangeSummary(redacted_summary) =
        &redacted.context_view.transactions[0].operations[0]
    else {
        return Err(anyhow!("expected redacted summary"));
    };
    assert_eq!(redacted_summary.source_range.source_digest, source_digest);
    let StoredContextOperation::ToolResultDistillation(redacted_distillation) =
        &redacted.context_view.transactions[0].operations[2]
    else {
        return Err(anyhow!("expected redacted distillation"));
    };
    assert_eq!(redacted_distillation.replacement_ratio_millionths, 100_000);

    assert_eq!(session.context_view, original.context_view);
    let loaded = Session::load(id)?;
    assert_eq!(loaded.context_view, original.context_view);
    assert!(serde_json::to_string(&loaded)?.contains(SECRET));
    Ok(())
}

#[test]
fn memory_profile_accounts_for_context_state_projected_cache_and_release() -> Result<()> {
    let mut session = Session::create_with_id("session_context_memory".to_string(), None, None);
    session.append_stored_message(text("summary-source", Role::User, "historical source"));
    session.append_stored_message(stored(
        "tool-call",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "large-call".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command":"large-output"}),
            thought_signature: None,
        }],
    ));
    session.append_stored_message(stored(
        "tool-result",
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "large-call".to_string(),
            content: "X".repeat(20 * 1024),
            is_error: None,
        }],
    ));
    session.context_view = applied_summary_state(&session.messages, 0, 0, "memory summary");

    let before_projection = session.memory_profile_snapshot();
    assert!(before_projection.context_view_json_bytes > 0);
    assert_eq!(before_projection.projected_provider_cache_message_count, 0);

    let projected_count = session.projected_provider_messages()?.len();
    let after_projection = session.memory_profile_snapshot();
    assert_eq!(
        after_projection.projected_provider_cache_message_count,
        projected_count
    );
    assert!(after_projection.projected_provider_cache_json_bytes > 0);
    assert!(after_projection.projected_provider_cache_tool_result_bytes >= 20 * 1024);
    assert!(after_projection.projected_provider_cache_large_blob_bytes >= 20 * 1024);

    session.release_provider_messages_cache();
    let after_release = session.memory_profile_snapshot();
    assert_eq!(after_release.projected_provider_cache_message_count, 0);
    assert_eq!(after_release.projected_provider_cache_json_bytes, 0);
    assert_eq!(after_release.projected_provider_cache_tool_result_bytes, 0);
    assert_eq!(after_release.projected_provider_cache_large_blob_bytes, 0);
    Ok(())
}

#[test]
fn remote_client_stripping_clears_transcript_legacy_context_and_both_caches() -> Result<()> {
    let mut session = Session::create_with_id("session_remote_strip".to_string(), None, None);
    append_all(&mut session, &text_transcript(4));
    session.compaction = Some(legacy_compaction("legacy", 2, None));
    session.context_view = applied_summary_state(&session.messages, 0, 1, "projected");
    let _ = session.provider_messages();
    let _ = session.projected_provider_messages()?;
    assert!(!session.provider_messages_cache.is_empty());
    assert!(!session.projected_provider_messages_cache.is_empty());

    session.strip_transcript_for_remote_client();
    assert!(session.messages.is_empty());
    assert!(session.compaction.is_none());
    assert!(session.context_view.is_default());
    assert!(session.provider_messages_cache.is_empty());
    assert!(session.provider_message_prefix_hashes_cache.is_empty());
    assert!(session.projected_provider_messages_cache.is_empty());
    assert!(
        session
            .projected_provider_message_prefix_hashes_cache
            .is_empty()
    );
    let profile = session.memory_profile_snapshot();
    assert_eq!(profile.message_count, 0);
    assert_eq!(profile.provider_cache_message_count, 0);
    assert_eq!(profile.projected_provider_cache_message_count, 0);
    assert_eq!(
        profile.context_view_json_bytes,
        serde_json::to_vec(&StoredContextViewState::default())?.len()
    );
    Ok(())
}
