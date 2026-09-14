//! Read-only monitor projections. Lists never open input, output or Session bodies.
use super::{ExecutionStore, managed_read::ManagedRead, store::query_record};
use anyhow::{Context, Result, ensure};
use jcode_tool_types::{
    execution::ExecutionContent,
    task_monitor::{
        TaskCursor, TaskMonitorRequest, TaskMonitorResponse, TaskRow, TaskTextPage, TaskView,
    },
};
use rusqlite::{Connection, params};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub async fn inspect(
    root: &Path,
    session: &str,
    request: TaskMonitorRequest,
) -> Result<TaskMonitorResponse> {
    ensure!(
        !session.is_empty(),
        "Task monitor requires an originating session"
    );
    let root = root.to_path_buf();
    let session = session.to_owned();
    tokio::task::spawn_blocking(move || {
        let store = ExecutionStore::open(&root)?;
        match request {
            TaskMonitorRequest::List {
                view,
                all_sessions,
                parent_run,
                before,
                limit,
            } => store.task_page(
                &session,
                view,
                all_sessions,
                parent_run.as_deref(),
                before,
                limit.unwrap_or(100),
            ),
            TaskMonitorRequest::Inspect { run_id } => Ok(TaskMonitorResponse::Status {
                row: Box::new(task_row(&store.connection()?, &run_id)?),
            }),
            TaskMonitorRequest::Read {
                run_id,
                content,
                offset,
                limit,
            } => {
                let page =
                    store.task_text_page(&run_id, content, offset, limit.unwrap_or(32 * 1024))?;
                Ok(TaskMonitorResponse::Text {
                    run_id,
                    content,
                    page,
                })
            }
        }
    })
    .await?
}

fn task_row(connection: &Connection, id: &str) -> Result<TaskRow> {
    let run = query_record(connection, id)?.context("Execution record is unavailable")?;
    let (created, updated, child_id, expandable, force_stop_available) = connection.query_row(
        "SELECT r.created,r.updated,c.child_id,
         EXISTS(SELECT 1 FROM runs nested WHERE nested.parent_id=r.id)
         OR c.child_id IS NOT NULL,
         EXISTS(SELECT 1 FROM native_processes n JOIN runtimes owner ON owner.id=n.owner WHERE n.run_id=r.id AND n.owner=r.owner AND n.finished=0 AND n.identity IS NOT NULL AND owner.protocol_version>=2) AND r.state='running'
         FROM runs r LEFT JOIN child_turns c ON c.run_id=r.id WHERE r.id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    Ok(TaskRow {
        run,
        created,
        updated,
        child_id,
        expandable,
        force_stop_available,
    })
}

impl ExecutionStore {
    fn task_page(
        &self,
        session: &str,
        view: TaskView,
        all_sessions: bool,
        parent_run: Option<&str>,
        before: Option<TaskCursor>,
        limit: u32,
    ) -> Result<TaskMonitorResponse> {
        ensure!(
            (1..=200).contains(&limit),
            "Task page limit must be 1 through 200"
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        // A child expansion deliberately means that conversation's execution
        // history, not an invented attribution of every tool to one child turn.
        let parent_child = parent_run
            .map(|id| task_row(&transaction, id))
            .transpose()?
            .and_then(|row| row.child_id);
        let states = match view {
            TaskView::Active => "'prepared','queued','running'",
            TaskView::Completed => "'completed','failed','cancelled','interrupted'",
        };
        let sql = format!(
            "SELECT r.id FROM runs r WHERE r.state IN ({states})
            AND (?1 OR r.session_id=?2 OR r.session_id IN (
                SELECT c.child_id FROM child_turns c JOIN runs origin ON origin.id=c.run_id
                WHERE c.initial=1 AND origin.session_id=?2))
            AND (?3 IS NULL OR r.parent_id=?3 OR (r.session_id=?4 AND r.parent_id IS NULL))
            AND (r.created<?5 OR (r.created=?5 AND r.id<?6))
            ORDER BY r.created DESC,r.id DESC LIMIT ?7"
        );
        let mut statement = transaction.prepare(&sql)?;
        let ids = statement
            .query_map(
                params![
                    all_sessions,
                    session,
                    parent_run,
                    parent_child,
                    before.as_ref().map_or(i64::MAX, |cursor| cursor.created),
                    before.as_ref().map_or("", |cursor| cursor.run_id.as_str()),
                    limit + 1
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = ids.len() > limit as usize;
        let rows = ids
            .iter()
            .take(limit as usize)
            .map(|id| task_row(&transaction, id))
            .collect::<Result<Vec<_>>>()?;
        let next = has_more
            .then(|| {
                rows.last().map(|row| TaskCursor {
                    created: row.created,
                    run_id: row.run.id.clone(),
                })
            })
            .flatten();
        Ok(TaskMonitorResponse::List { rows, next })
    }

    fn task_text_page(
        &self,
        id: &str,
        content: ExecutionContent,
        offset: Option<u64>,
        limit: u32,
    ) -> Result<TaskTextPage> {
        ensure!(
            (4..=64 * 1024).contains(&limit),
            "Task text window must be 4 through 65536 bytes"
        );
        let record = self
            .inspect(id)?
            .context("Execution record is unavailable")?;
        match content {
            ExecutionContent::Output => {
                let Some(path) = record.output_path.clone() else {
                    // Plain reads deliberately retain a source receipt rather
                    // than archiving the entire source. Show that existing
                    // receipt without rereading or rerunning anything.
                    let receipt = self.result(
                        &record,
                        std::num::NonZeroUsize::new(usize::try_from(limit)?).unwrap(),
                    )?;
                    let bytes = receipt.output.into_bytes();
                    let total = u64::try_from(bytes.len())?;
                    return text_window(
                        &mut std::io::Cursor::new(bytes),
                        total,
                        offset.or(Some(0)),
                        limit,
                    );
                };
                let mut source = ManagedRead::open(
                    self.root()
                        .parent()
                        .context("Execution namespace missing")?,
                    &path,
                )?
                .context("Output is not a managed execution source")?;
                let total = source.length;
                text_window(&mut source, total, offset, limit)
            }
            ExecutionContent::Input => {
                // Existing receipt owner verifies the complete immutable input.
                self.invocation_input(id)?;
                let expected = self.root().join("inputs").join(format!("{id}.json"));
                ensure!(
                    record.input_path == expected
                        && std::fs::symlink_metadata(&expected)?.is_file(),
                    "Execution input path changed ownership"
                );
                let mut source = std::fs::File::open(expected)?;
                let total = source.metadata()?.len();
                text_window(&mut source, total, offset.or(Some(0)), limit)
            }
        }
    }
}

fn text_window(
    source: &mut (impl Read + Seek),
    total: u64,
    offset: Option<u64>,
    limit: u32,
) -> Result<TaskTextPage> {
    let mut start = offset.unwrap_or_else(|| total.saturating_sub(u64::from(limit)));
    ensure!(
        start <= total,
        "Task output position exceeds its committed prefix"
    );
    source.seek(SeekFrom::Start(start))?;
    let size = (total - start).min(u64::from(limit));
    let mut bytes = vec![0; usize::try_from(size)?];
    source.read_exact(&mut bytes)?;
    let leading = bytes
        .iter()
        .take_while(|byte| **byte & 0xc0 == 0x80)
        .count();
    start += u64::try_from(leading)?;
    let bytes = &bytes[leading..];
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(TaskTextPage {
        start,
        end: start + u64::try_from(text.len())?,
        total,
        text: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation, RunState};
    use jcode_tool_types::ToolOutput;

    fn run(store: &ExecutionStore, session: &str, name: &str, state: RunState) -> String {
        let PreparedInvocation::New(record) = store
            .prepare(
                &Invocation {
                    session_id: session.into(),
                    message_id: name.into(),
                    call_path: vec![name.into()],
                    tool: "fixture".into(),
                    input: serde_json::json!({"text":"input"}),
                    working_dir: None,
                    received_result_digest: None,
                },
                "owner",
            )
            .unwrap()
        else {
            panic!()
        };
        store.start(&record.id, "owner").unwrap();
        if state.terminal() {
            store
                .retain(record.clone(), ToolOutput::new("é漢字\n".repeat(20)), state)
                .unwrap();
        }
        record.id
    }

    #[test]
    fn task_pages_are_metadata_only_scoped_and_stable() {
        let root = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(root.path()).unwrap();
        let first = run(&store, "parent", "one", RunState::Completed);
        let second = run(&store, "parent", "two", RunState::Failed);
        run(&store, "parent", "live", RunState::Running);
        run(&store, "other", "other", RunState::Completed);
        std::fs::remove_file(store.inspect(&first).unwrap().unwrap().output_path.unwrap()).unwrap();
        let TaskMonitorResponse::List { rows, next } = store
            .task_page("parent", TaskView::Completed, false, None, None, 1)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        let TaskMonitorResponse::List { rows: tail, next } = store
            .task_page("parent", TaskView::Completed, false, None, next, 1)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(tail.len(), 1);
        assert_ne!(rows[0].run.id, tail[0].run.id);
        assert!(next.is_none());
        assert!([first, second].contains(&tail[0].run.id));
        let TaskMonitorResponse::List { rows, .. } = store
            .task_page("parent", TaskView::Active, false, None, None, 20)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run.state, RunState::Running);
    }

    #[test]
    fn task_windows_reassemble_unicode_and_tail_without_unbounded_buffers() {
        let root = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(root.path()).unwrap();
        let id = run(&store, "parent", "text", RunState::Completed);
        let mut offset = 0;
        let mut text = String::new();
        loop {
            let page = store
                .task_text_page(&id, ExecutionContent::Output, Some(offset), 7)
                .unwrap();
            assert_eq!(page.start, offset);
            text.push_str(&page.text);
            if page.end == page.total {
                break;
            }
            assert!(page.end > offset);
            offset = page.end;
        }
        assert_eq!(text, "é漢字\n".repeat(20));
        let tail = store
            .task_text_page(&id, ExecutionContent::Output, None, 7)
            .unwrap();
        assert!(text.ends_with(&tail.text));
        assert_eq!(tail.end, tail.total);
        assert!(
            store
                .task_text_page(&id, ExecutionContent::Output, Some(u64::MAX), 7)
                .is_err()
        );
    }
    /// Action-capable fixture setup only. The public TUI performs review/deletion.
    #[test]
    #[ignore = "requires an explicit private native monitor fixture manifest"]
    fn native_monitor_archive_fixture() -> Result<()> {
        use crate::execution::{ArchiveConfig, IDLE_SECONDS, StorageConfig};
        #[derive(serde::Deserialize)]
        struct Setup {
            root: std::path::PathBuf,
            archive: ArchiveConfig,
            result: std::path::PathBuf,
        }
        let manifest = std::env::var_os("JCODE_TASK_MONITOR_FIXTURE")
            .context("Missing native fixture manifest")?;
        let setup: Setup = crate::storage::read_json(std::path::Path::new(&manifest))?;
        ensure!(
            std::fs::read_to_string(setup.root.join("WP05_NATIVE_FIXTURE"))?
                == "native monitor fixture",
            "Not an explicitly owned native fixture"
        );
        ensure!(
            setup.archive.directory.components().count() == 1
                && setup
                    .archive
                    .directory
                    .to_string_lossy()
                    .starts_with("jcode-execution-fixture-wp05-"),
            "Archive target is not a private fixture"
        );
        ensure!(
            !setup.archive.mount.join(&setup.archive.directory).exists(),
            "Archive fixture already exists"
        );
        let store = ExecutionStore::open(&setup.root)?;
        let id = run(
            &store,
            "native-monitor-archive",
            "archive",
            RunState::Completed,
        );
        let mut session =
            crate::session::Session::create_with_id("native-monitor-archive".into(), None, None);
        session.add_message(
            crate::message::Role::Assistant,
            vec![crate::message::ContentBlock::ToolUse {
                id: "archive".into(),
                name: "fixture".into(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
        );
        // Match the fixture invocation's authoritative message identity.
        session.messages[0].id = "archive".into();
        crate::storage::write_json_secret(
            &setup.root.join("sessions/native-monitor-archive.json"),
            &session,
        )?;
        let snapshot = store.create_inspection_snapshot(
            "native-monitor-reader",
            "native-monitor-archive",
            chrono::Utc::now().timestamp(),
        )?;
        let now = store.last_activity("native-monitor-archive")?.unwrap() + IDLE_SECONDS + 1;
        ensure!(
            store.archive_cold_output(
                &id,
                &StorageConfig {
                    archive: Some(setup.archive),
                    local_reserve_bytes: 0,
                    archive_reserve_bytes: 1024 * 1024
                },
                now
            )?,
            "Fixture output did not archive"
        );
        let record = store.inspect(&id)?.unwrap();
        crate::storage::write_json_secret(
            &setup.result,
            &serde_json::json!({"run_id":id,"snapshot_id":snapshot,"output_path":record.output_path,"bytes":record.output_bytes}),
        )?;
        Ok(())
    }
    #[test]
    fn large_monitor_history_pages_without_opening_any_payload() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let mut connection = store.connection()?;
        let transaction = connection.transaction()?;
        for index in 0..20000 {
            transaction.execute("INSERT INTO runs(id,session_id,message_id,tool,input_digest,state,owner,input_path,created,updated) VALUES(?1,?2,'m','fixture','digest','completed','offline','missing-payload',?3,?3)",params![format!("run-{index:064x}"),if index%2==0{"parent"}else{"other"},index])?;
        }
        transaction.commit()?;
        let start = std::time::Instant::now();
        let TaskMonitorResponse::List { rows, next } =
            store.task_page("parent", TaskView::Completed, false, None, None, 100)?
        else {
            panic!()
        };
        assert_eq!(rows.len(), 100);
        assert!(rows.iter().all(|row| row.run.session_id == "parent"));
        let TaskMonitorResponse::List { rows: older, .. } =
            store.task_page("parent", TaskView::Completed, false, None, next, 100)?
        else {
            panic!()
        };
        assert_eq!(older.len(), 100);
        assert!(
            older
                .iter()
                .all(|row| !rows.iter().any(|first| first.run.id == row.run.id))
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "metadata page unexpectedly slow: {:?}",
            start.elapsed()
        );
        assert!(
            store
                .task_text_page(&rows[0].run.id, ExecutionContent::Input, None, 100)
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn monitor_scope_and_expansion_use_structural_child_and_batch_relationships() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let PreparedInvocation::New(parent) = store.prepare(
            &Invocation {
                session_id: "parent".into(),
                message_id: "child".into(),
                call_path: vec!["child".into()],
                tool: "subagent".into(),
                input: serde_json::json!({}),
                working_dir: None,
                received_result_digest: None,
            },
            "owner",
        )?
        else {
            panic!()
        };
        store.admit_child_turn(&parent.id, "owner", "child", true, false, 15)?;
        store.retain(
            parent.clone(),
            ToolOutput::new("child reply"),
            RunState::Completed,
        )?;
        let child_tool = run(&store, "child", "tool", RunState::Completed);
        run(&store, "unrelated", "other", RunState::Completed);
        let TaskMonitorResponse::List { rows, .. } =
            store.task_page("parent", TaskView::Completed, false, None, None, 100)?
        else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.run.id == child_tool));
        let TaskMonitorResponse::List { rows, .. } = store.task_page(
            "parent",
            TaskView::Completed,
            false,
            Some(&parent.id),
            None,
            100,
        )?
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run.id, child_tool);
        let batch = run(&store, "parent", "batch", RunState::Completed);
        let nested = run(&store, "parent", "nested", RunState::Completed);
        store.connection()?.execute(
            "UPDATE runs SET parent_id=?1 WHERE id=?2",
            params![batch, nested],
        )?;
        let TaskMonitorResponse::List { rows, .. } = store.task_page(
            "parent",
            TaskView::Completed,
            false,
            Some(&batch),
            None,
            100,
        )?
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run.id, nested);
        Ok(())
    }
    #[test]
    fn read_page_monitor_shows_retained_receipt_without_reopening_changed_source() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let path = root.path().join("source.txt");
        std::fs::write(&path, "ORIGINAL SOURCE")?;
        let output = crate::execution::reader::SourceReader::new(root.path()).read(
            crate::execution::reader::ReadRequest {
                path: path.clone(),
                point: None,
                start_line: 1,
                end_line: None,
                target: std::num::NonZeroUsize::new(1000).unwrap(),
                stop: None,
            },
        )?;
        let PreparedInvocation::New(record) = store.prepare(
            &Invocation {
                session_id: "reader".into(),
                message_id: "read".into(),
                call_path: vec!["read".into()],
                tool: "read".into(),
                input: serde_json::json!({"file_path":path}),
                working_dir: None,
                received_result_digest: None,
            },
            "owner",
        )?
        else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        store.retain(record.clone(), output, RunState::Completed)?;
        std::fs::write(&path, "CHANGED SOURCE")?;
        let page = store.task_text_page(&record.id, ExecutionContent::Output, None, 1000)?;
        assert!(page.text.contains("receipt"));
        assert!(!page.text.contains("CHANGED SOURCE"));
        assert!(store.inspect(&record.id)?.unwrap().output_path.is_none());
        Ok(())
    }
}
