use super::*;
use jcode_tool_types::{RunState, ToolOutput, cleanup::CleanupSelection};

#[test]
fn one_failed_deletion_does_not_hide_or_skip_other_confirmed_outputs() -> Result<()> {
    let (fixture, _) = sealed_fixture()?;
    let invocation = Invocation {
        session_id: "storage".into(),
        message_id: "second".into(),
        call_path: vec!["call".into()],
        tool: "fixture".into(),
        input: serde_json::json!({}),
        working_dir: None,
        received_result_digest: None,
    };
    let PreparedInvocation::New(second) = fixture.store.prepare(&invocation, "owner")? else {
        panic!();
    };
    fixture.store.start(&second.id, "owner")?;
    let capture = crate::execution::Capture::create(
        fixture.store.clone(),
        second.clone(),
        Default::default(),
    )?;
    capture.seal(ToolOutput::new("second output"), RunState::Completed)?;
    drop(capture);
    let now = fixture.store.last_activity("storage")?.unwrap() + crate::execution::IDLE_SECONDS + 1;
    for id in [&fixture.record.id, &second.id] {
        fixture.store.archive_cold_output_with_environment(
            id,
            &fixture.config,
            now,
            fixture.environment.clone(),
        )?;
    }
    let review = fixture.store.review_output_cleanup_with_environment(
        "human",
        CleanupSelection::Outputs {
            run_ids: vec![fixture.record.id.clone(), second.id.clone()],
        },
        now,
        fixture.environment.as_ref(),
    )?;
    *fixture.environment.fail_stage.lock().unwrap() = Some("cleanup_after_part");
    let outcome = fixture.store.confirm_output_cleanup_with_environment(
        "human",
        &review.review_id,
        &review.confirmation_id,
        now,
        fixture.environment.as_ref(),
    )?;
    assert_eq!(outcome.items.len(), 2);
    assert_eq!(outcome.items.iter().filter(|item| item.deleted).count(), 1);
    assert_eq!(
        outcome
            .items
            .iter()
            .filter(|item| item.error.is_some())
            .count(),
        1
    );
    assert!(fixture.record.input_path.is_file() && second.input_path.is_file());
    let retried = fixture.store.confirm_output_cleanup_with_environment(
        "human",
        &review.review_id,
        &review.confirmation_id,
        now,
        fixture.environment.as_ref(),
    )?;
    assert!(retried.items.iter().all(|item| item.deleted));
    Ok(())
}

#[test]
#[ignore = "requires explicit tiny owned fixture on the verified archive volume"]
fn native_cold_retention_snapshot_and_cleanup_journey() -> Result<()> {
    let config_path = std::env::var_os("JCODE_EXECUTION_ARCHIVE_FIXTURE")
        .context("Missing explicit native fixture configuration")?;
    let archive: ArchiveConfig = crate::storage::read_json(Path::new(&config_path))?;
    ensure!(
        archive.directory.components().count() == 1
            && archive
                .directory
                .to_string_lossy()
                .starts_with("jcode-execution-fixture-"),
        "Native retention test requires an explicitly owned fixture directory"
    );
    ensure!(
        !archive.mount.join(&archive.directory).exists(),
        "Fixture directory already exists; refusing to overwrite"
    );
    let binding = verified_archive(&archive)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        use crate::message::{ContentBlock, Role};
        use jcode_tool_types::OutputSource;
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let mut session =
            crate::session::Session::create_with_id("native_target".into(), None, None);
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call".into(),
                name: "fixture".into(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
        );
        let session_path = root.path().join("sessions/native_target.json");
        crate::storage::write_json_secret(&session_path, &session)?;
        let source_before = std::fs::read(&session_path)?;
        let invocation = Invocation {
            session_id: session.id.clone(),
            message_id: session.messages[0].id.clone(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&invocation, "native-fixture")? else {
            bail!("Duplicate native fixture");
        };
        store.start(&record.id, "native-fixture")?;
        let capture =
            crate::execution::Capture::create(store.clone(), record.clone(), Default::default())?;
        let original = format!("{}\nNATIVE_TAIL\n", "αβγ".repeat(500));
        capture.seal(ToolOutput::new(&original), RunState::Completed)?;
        drop(capture);
        let snapshot = store.create_inspection_snapshot("reader", &session.id, 100)?;
        let delivered = store
            .read_inspection_snapshot("reader", &snapshot, 101)?
            .expand_tool("tool-1-1")?;
        let local = store.output_location(&record.id)?.unwrap().0;
        let local_identity = DirectoryBinding::open(&local)?.identity()?;
        let alias = store.inspect(&record.id)?.unwrap().output_path.unwrap();
        let reader = crate::execution::reader::SourceReader::new(root.path());
        let request = |point| crate::execution::reader::ReadRequest {
            path: alias.clone(),
            point,
            start_line: 1,
            end_line: None,
            target: std::num::NonZeroUsize::new(100).unwrap(),
            stop: None,
        };
        let first = reader.read(request(None))?;
        let OutputSource::ReadPage(first) = first.source else {
            bail!("Missing native continuation");
        };
        let now = store.last_activity(&session.id)?.unwrap() + crate::execution::IDLE_SECONDS + 1;
        let storage = StorageConfig {
            archive: Some(archive.clone()),
            local_reserve_bytes: 0,
            archive_reserve_bytes: 1024 * 1024,
        };
        assert!(store.archive_cold_output(&record.id, &storage, now)?);
        let physical = store.output_location(&record.id)?.unwrap().0;
        assert!(physical.starts_with(&binding.path));
        #[cfg(unix)]
        assert_ne!(
            local_identity.device,
            DirectoryBinding::open(&physical)?.identity()?.device,
            "Fixture must exercise real cross-volume copy"
        );
        assert!(!local.exists());
        assert_eq!(std::fs::read_to_string(&alias)?, original);
        let mut next = request(first.next_point.clone());
        next.target = std::num::NonZeroUsize::new(10_000).unwrap();
        let continued = reader.read(next)?;
        assert!(continued.output.contains("NATIVE_TAIL"));
        assert_eq!(
            store
                .read_inspection_snapshot("reader", &snapshot, 102)?
                .expand_tool("tool-1-1")?,
            delivered
        );
        for spec in [
            ArchiveConfig {
                mount: root.path().join("not-mounted"),
                ..archive.clone()
            },
            ArchiveConfig {
                volume_uuid: "00000000-0000-0000-0000-000000000000".into(),
                ..archive.clone()
            },
        ] {
            store.connection()?.execute(
                "UPDATE output_locations SET archive_spec=?2 WHERE id=?1",
                params![record.id, serde_json::to_string(&spec)?],
            )?;
            assert!(reader.read(request(first.next_point.clone())).is_err());
            assert!(!root.path().join("not-mounted").exists());
        }
        store.connection()?.execute(
            "UPDATE output_locations SET archive_spec=?2 WHERE id=?1",
            params![record.id, serde_json::to_string(&archive)?],
        )?;
        let review = store.review_output_cleanup(
            "human",
            CleanupSelection::Outputs {
                run_ids: vec![record.id.clone()],
            },
            now,
        )?;
        assert_eq!(
            review.candidates[0].affected_snapshot_ids,
            vec![snapshot.clone()]
        );
        assert!(review.selected_bytes >= original.len() as u64);
        assert!(
            store
                .confirm_output_cleanup("human", &review.review_id, "wrong", now)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&alias)?, original);
        let outcome = store.confirm_output_cleanup(
            "human",
            &review.review_id,
            &review.confirmation_id,
            now,
        )?;
        assert!(outcome.items[0].deleted);
        assert_eq!(
            store.confirm_output_cleanup(
                "human",
                &review.review_id,
                &review.confirmation_id,
                now
            )?,
            outcome
        );
        assert!(!physical.exists());
        assert!(
            format!("{:#}", reader.read(request(first.next_point)).unwrap_err())
                .contains("deliberately")
        );
        assert!(
            store
                .read_inspection_snapshot("reader", &snapshot, 103)?
                .expand_tool("tool-1-1")
                .is_err()
        );
        assert_eq!(std::fs::read(&session_path)?, source_before);
        assert!(delivered.contains("NATIVE_TAIL"));
        Ok(())
    }));
    binding.verify()?;
    std::fs::remove_dir_all(&binding.path)
        .context("Owned native retention fixture cleanup failed")?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn sealed_fixture() -> Result<(Fixture, i64)> {
    let fixture = Fixture::new()?;
    let capture = crate::execution::Capture::create(
        fixture.store.clone(),
        fixture.record.clone(),
        Default::default(),
    )?;
    capture.seal(
        ToolOutput::new("retained fixture output\nTAIL"),
        RunState::Completed,
    )?;
    drop(capture);
    let now = fixture.store.last_activity("storage")?.unwrap() + crate::execution::IDLE_SECONDS + 1;
    Ok((fixture, now))
}

#[test]
fn cold_archive_recovers_every_move_boundary_without_move_back_or_generation_churn() -> Result<()> {
    for stage in [
        "before_copy",
        "after_copy",
        "after_publish",
        "after_alias",
        "after_delete",
    ] {
        let (fixture, now) = sealed_fixture()?;
        let local = fixture.store.root().join("data").join(&fixture.record.id);
        let before = std::fs::read(local.join("output.txt"))?;
        *fixture.environment.fail_stage.lock().unwrap() = Some(stage);
        assert!(
            fixture
                .store
                .archive_cold_output_with_environment(
                    &fixture.record.id,
                    &fixture.config,
                    now,
                    fixture.environment.clone()
                )
                .is_err(),
            "{stage}"
        );
        fixture.store.archive_cold_output_with_environment(
            &fixture.record.id,
            &fixture.config,
            now,
            fixture.environment.clone(),
        )?;
        assert!(!fixture.store.archive_cold_output_with_environment(
            &fixture.record.id,
            &fixture.config,
            now,
            fixture.environment.clone()
        )?);
        let (archived, true) = fixture.store.output_location(&fixture.record.id)?.unwrap() else {
            panic!()
        };
        assert!(!local.exists());
        assert_eq!(std::fs::read(archived.join("output.txt"))?, before);
        assert_eq!(
            std::fs::read(
                fixture
                    .store
                    .root()
                    .join("outputs")
                    .join(&fixture.record.id)
                    .join("output.txt")
            )?,
            before
        );
        let generation: i64 = fixture.store.connection()?.query_row(
            "SELECT generation FROM output_locations WHERE id=?1",
            [&fixture.record.id],
            |row| row.get(0),
        )?;
        assert_eq!(generation, 1);
        fixture.store.touch_activity("storage", now)?;
        assert!(!fixture.store.archive_cold_output_with_environment(
            &fixture.record.id,
            &fixture.config,
            now,
            fixture.environment.clone()
        )?);
        assert_eq!(
            fixture
                .store
                .output_location(&fixture.record.id)?
                .unwrap()
                .0,
            archived
        );
    }
    Ok(())
}

#[test]
fn cold_archive_excludes_live_and_recent_sessions_and_defers_offline_without_local_substitute()
-> Result<()> {
    let (fixture, now) = sealed_fixture()?;
    assert!(!fixture.store.archive_cold_output_with_environment(
        &fixture.record.id,
        &fixture.config,
        now - crate::execution::IDLE_SECONDS,
        fixture.environment.clone()
    )?);
    let guard = fixture.store.begin_session_activity("storage")?;
    assert!(!fixture.store.archive_cold_output_with_environment(
        &fixture.record.id,
        &fixture.config,
        now,
        fixture.environment.clone()
    )?);
    drop(guard);
    std::fs::remove_dir(&fixture.environment.archive)?;
    let local = fixture.store.root().join("data").join(&fixture.record.id);
    let before = std::fs::read(local.join("output.txt"))?;
    assert!(
        fixture
            .store
            .archive_cold_output_with_environment(
                &fixture.record.id,
                &fixture.config,
                now,
                fixture.environment.clone()
            )
            .is_err()
    );
    assert!(!fixture.environment.archive.exists());
    assert_eq!(std::fs::read(local.join("output.txt"))?, before);
    Ok(())
}

#[test]
fn cleanup_preview_requires_exact_confirmation_and_reports_whole_output_overshoot() -> Result<()> {
    let (fixture, now) = sealed_fixture()?;
    fixture.store.archive_cold_output_with_environment(
        &fixture.record.id,
        &fixture.config,
        now,
        fixture.environment.clone(),
    )?;
    let physical = fixture
        .store
        .output_location(&fixture.record.id)?
        .unwrap()
        .0;
    let before = std::fs::read(physical.join("output.txt"))?;
    let review = fixture.store.review_output_cleanup_with_environment(
        "human",
        CleanupSelection::OldestBytes { bytes: 1 },
        now,
        fixture.environment.as_ref(),
    )?;
    assert_eq!(review.candidates.len(), 1);
    assert_eq!(review.overshoot_bytes, review.selected_bytes - 1);
    assert_eq!(std::fs::read(physical.join("output.txt"))?, before);
    assert!(
        fixture
            .store
            .confirm_output_cleanup_with_environment(
                "human",
                &review.review_id,
                "wrong",
                now,
                fixture.environment.as_ref()
            )
            .is_err()
    );
    assert!(
        fixture
            .store
            .confirm_output_cleanup_with_environment(
                "another-human",
                &review.review_id,
                &review.confirmation_id,
                now,
                fixture.environment.as_ref()
            )
            .is_err()
    );
    assert!(physical.exists());
    std::fs::write(physical.join("output.txt"), b"changed fixture bytes")?;
    assert!(
        fixture
            .store
            .confirm_output_cleanup_with_environment(
                "human",
                &review.review_id,
                &review.confirmation_id,
                now,
                fixture.environment.as_ref()
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(physical.join("output.txt"))?,
        b"changed fixture bytes"
    );
    std::fs::write(physical.join("output.txt"), &before)?;
    fixture.store.touch_activity("storage", now)?;
    let result = fixture.store.confirm_output_cleanup_with_environment(
        "human",
        &review.review_id,
        &review.confirmation_id,
        now,
        fixture.environment.as_ref(),
    )?;
    assert!(
        result.items[0].deleted,
        "Resuming or inspecting a session must not hide its already cold archived outputs"
    );
    Ok(())
}

#[test]
fn cleanup_failure_is_targeted_and_retry_converges_without_deleting_input_or_history() -> Result<()>
{
    for stage in [
        "cleanup_before_delete",
        "cleanup_after_part",
        "cleanup_after_files",
        "cleanup_after_alias",
    ] {
        let (fixture, now) = sealed_fixture()?;
        fixture.store.archive_cold_output_with_environment(
            &fixture.record.id,
            &fixture.config,
            now,
            fixture.environment.clone(),
        )?;
        let record = fixture.store.inspect(&fixture.record.id)?.unwrap();
        let input_before = std::fs::read(&record.input_path)?;
        let unrelated = fixture._directory.path().join("research-document.md");
        std::fs::write(&unrelated, "preserved")?;
        let review = fixture.store.review_output_cleanup_with_environment(
            "human",
            CleanupSelection::Outputs {
                run_ids: vec![record.id.clone()],
            },
            now,
            fixture.environment.as_ref(),
        )?;
        *fixture.environment.fail_stage.lock().unwrap() = Some(stage);
        let first = fixture.store.confirm_output_cleanup_with_environment(
            "human",
            &review.review_id,
            &review.confirmation_id,
            now,
            fixture.environment.as_ref(),
        )?;
        assert!(!first.items[0].deleted, "{stage}");
        assert!(first.items[0].error.is_some());
        assert!(
            fixture
                .store
                .open_output_file(&record.id)
                .unwrap_err()
                .to_string()
                .contains("deliberately")
        );
        let second = fixture.store.confirm_output_cleanup_with_environment(
            "human",
            &review.review_id,
            &review.confirmation_id,
            now,
            fixture.environment.as_ref(),
        )?;
        assert!(second.items[0].deleted);
        let third = fixture.store.confirm_output_cleanup_with_environment(
            "human",
            &review.review_id,
            &review.confirmation_id,
            now,
            fixture.environment.as_ref(),
        )?;
        assert_eq!(second, third);
        assert_eq!(std::fs::read(&record.input_path)?, input_before);
        assert_eq!(std::fs::read_to_string(&unrelated)?, "preserved");
        assert!(fixture.store.inspect(&record.id)?.unwrap().state.terminal());
    }
    Ok(())
}
