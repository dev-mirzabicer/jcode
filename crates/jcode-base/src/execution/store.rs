use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA: i64 = 9;

pub use jcode_tool_types::RunState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub session_id: String,
    pub message_id: String,
    /// Full nested path, not a repeating batch ordinal or provider ID alone.
    pub call_path: Vec<String>,
    pub tool: String,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_result_digest: Option<String>,
}

impl Invocation {
    pub fn id(&self) -> String {
        Self::scope_id(&self.session_id, &self.message_id, &self.call_path)
    }
    pub fn parent_id(&self) -> Option<String> {
        (self.call_path.len() > 1).then(|| {
            Self::scope_id(
                &self.session_id,
                &self.message_id,
                &self.call_path[..self.call_path.len() - 1],
            )
        })
    }
    fn scope_id(session: &str, message: &str, path: &[String]) -> String {
        // Tuple serialization preserves field boundaries and prevents delimiter aliases.
        let scope = serde_json::to_vec(&(session, message, path)).expect("string tuples serialize");
        format!("run-{:x}", Sha256::digest(scope))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub session_id: String,
    pub message_id: String,
    pub tool: String,
    pub state: RunState,
    pub owner: String,
    pub input_path: PathBuf,
    pub result_path: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub output_bytes: u64,
    pub complete: bool,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub stop_cause: Option<jcode_tool_types::StopCause>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

pub enum PreparedInvocation {
    New(RunRecord),
    Existing(RunRecord),
}

/// Metadata transactions only. Large result/input bodies remain in owned files.
/// No connection is held across a tool, provider call, or event-loop await.
#[derive(Clone)]
pub struct ExecutionStore {
    root: PathBuf,
}

impl ExecutionStore {
    pub fn open(state_root: &Path) -> Result<Self> {
        let root = state_root.join("execution");
        crate::storage::ensure_dir(&root)?;
        ensure!(
            !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "Execution root may not be a symlink"
        );
        jcode_core::fs::set_directory_permissions_owner_only(&root)?;
        let store = Self { root };
        let mut connection = store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            (0..=SCHEMA).contains(&version),
            "Unsupported execution database schema {version}"
        );
        if version == 0 {
            transaction.execute_batch(
                "CREATE TABLE runs (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
                    tool TEXT NOT NULL, input_digest TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL, input_path TEXT NOT NULL, result_path TEXT,
                    output_path TEXT, output_bytes INTEGER NOT NULL DEFAULT 0,
                    complete INTEGER NOT NULL DEFAULT 0,
                    created INTEGER NOT NULL DEFAULT (unixepoch()),
                    updated INTEGER NOT NULL DEFAULT (unixepoch()),
                    CHECK (state IN ('prepared','running','completed','failed','cancelled','interrupted'))
                );
                CREATE INDEX runs_session_page ON runs(session_id, id);
                CREATE INDEX runs_state_page ON runs(state, created, id);
                CREATE TABLE relocations (
                    id TEXT PRIMARY KEY REFERENCES runs(id), source TEXT NOT NULL,
                    destination TEXT NOT NULL, stage TEXT NOT NULL,
                    manifest_path TEXT NOT NULL
                );
                PRAGMA user_version=1;"
            )?;
        }
        if version < 2 {
            transaction.execute_batch(
                "CREATE TABLE output_locations (
                id TEXT PRIMARY KEY REFERENCES runs(id), physical TEXT NOT NULL,
                archived INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0,
                capture_error TEXT
            ); PRAGMA user_version=2;",
            )?;
        }
        if version < 3 {
            transaction.execute_batch(
                "CREATE TABLE output_allocations (
                id TEXT PRIMARY KEY REFERENCES runs(id), physical TEXT NOT NULL,
                archived INTEGER NOT NULL, owner TEXT NOT NULL,
                stage TEXT NOT NULL CHECK(stage IN ('prepared','published'))
            );
            INSERT INTO output_allocations (id,physical,archived,owner,stage)
                SELECT l.id,l.physical,l.archived,r.owner,'published'
                FROM output_locations l JOIN runs r ON r.id=l.id;
            PRAGMA user_version=3;",
            )?;
        }
        if version < 4 {
            transaction.execute_batch(
                "ALTER TABLE runs ADD COLUMN background INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE runs ADD COLUMN stop_cause TEXT;
                ALTER TABLE runs ADD COLUMN parent_id TEXT REFERENCES runs(id);
                PRAGMA user_version=4;",
            )?;
        }
        if version < 5 {
            transaction.execute_batch("CREATE TABLE runtimes (
                id TEXT PRIMARY KEY, endpoint TEXT NOT NULL, auth_key TEXT NOT NULL,
                lease_path TEXT NOT NULL, protocol_version INTEGER NOT NULL, process_id INTEGER NOT NULL
            ); PRAGMA user_version=5;")?;
        }
        if version < 6 {
            transaction.execute_batch("CREATE TABLE command_handoffs (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), parent_owner TEXT NOT NULL,
                worker_owner TEXT REFERENCES runtimes(id), payload_digest TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('prepared','claimed','registered','executing','finished')),
                process_identity TEXT, exec_authorized INTEGER NOT NULL DEFAULT 0
            ); PRAGMA user_version=6;")?;
        }
        if version < 7 {
            transaction.execute_batch(
                "CREATE TABLE acceptance_receipts (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), receipt_path TEXT NOT NULL,
                digest TEXT NOT NULL
            ); PRAGMA user_version=7;",
            )?;
        }
        if version < 8 {
            transaction.execute_batch("CREATE TABLE background_deliveries (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), notify INTEGER NOT NULL,
                wake INTEGER NOT NULL, started_at TEXT NOT NULL,
                notify_state TEXT NOT NULL DEFAULT 'pending', wake_state TEXT NOT NULL DEFAULT 'pending',
                notify_attempt TEXT, wake_attempt TEXT,
                CHECK(notify_state IN ('pending','in_flight','delivered','uncertain')),
                CHECK(wake_state IN ('pending','in_flight','delivered','uncertain'))
            ); PRAGMA user_version=8;")?;
        }
        if version < 9 {
            transaction.execute_batch(
                "CREATE TABLE output_chunks (
                run_id TEXT NOT NULL REFERENCES runs(id), start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL, sha256 TEXT NOT NULL,
                PRIMARY KEY(run_id,start_byte), CHECK(end_byte>start_byte)
            );
            ALTER TABLE output_locations ADD COLUMN archive_spec TEXT;
            ALTER TABLE output_allocations ADD COLUMN archive_spec TEXT;
            PRAGMA user_version=9;",
            )?;
        }
        transaction.commit()?;
        Ok(store)
    }

    pub(super) fn connection(&self) -> Result<Connection> {
        let path = self.root.join("index.sqlite");
        // Precreate with owner-only mode. WAL/SHM inherit the database mode and
        // the containing directory is private even during first open.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Execution database may not be a symlink"
        );
        jcode_core::fs::set_permissions_owner_only(&path)?;
        drop(file);
        let connection = Connection::open(&path).context("Open execution metadata")?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn prepare(&self, invocation: &Invocation, owner: &str) -> Result<PreparedInvocation> {
        ensure!(
            !invocation.session_id.is_empty()
                && !invocation.message_id.is_empty()
                && !invocation.call_path.is_empty()
                && invocation.call_path.iter().all(|part| !part.is_empty()),
            "Invocation requires complete session/message/call identity"
        );
        let id = invocation.id();
        let bytes = serde_json::to_vec(invocation)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = transaction
            .query_row("SELECT input_digest FROM runs WHERE id=?1", [&id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        {
            ensure!(
                previous == digest,
                "Invocation identity conflicts with different input; no work executed"
            );
            let record = query_record(&transaction, &id)?.context("Missing existing invocation")?;
            return Ok(PreparedInvocation::Existing(record));
        }
        let directory = self.root.join("inputs");
        crate::storage::ensure_dir(&directory)?;
        let input_path = directory.join(format!("{id}.json"));
        crate::storage::write_json_secret(&input_path, invocation)?;
        transaction.execute("INSERT INTO runs (id,session_id,message_id,tool,input_digest,state,owner,input_path,parent_id) VALUES (?1,?2,?3,?4,?5,'prepared',?6,?7,?8)",
            params![id, invocation.session_id, invocation.message_id, invocation.tool, digest, owner, path_text(&input_path)?,invocation.parent_id()])?;
        let record = query_record(&transaction, &id)?.context("Missing prepared invocation")?;
        transaction.commit()?;
        Ok(PreparedInvocation::New(record))
    }

    pub fn start(&self, id: &str, owner: &str) -> Result<()> {
        let changed = self.connection()?.execute("UPDATE runs SET state='running',updated=unixepoch() WHERE id=?1 AND owner=?2 AND state='prepared'", params![id, owner])?;
        ensure!(
            changed == 1,
            "Invocation cannot start again or under another owner"
        );
        Ok(())
    }

    pub fn inspect(&self, id: &str) -> Result<Option<RunRecord>> {
        query_record(&self.connection()?, id)
    }

    pub fn invocation_input(&self, id: &str) -> Result<Invocation> {
        let record = self.inspect(id)?.context("Unknown invocation")?;
        ensure!(
            record.input_path.parent() == Some(self.root.join("inputs").as_path()),
            "Invocation input is outside its owned store"
        );
        ensure!(
            std::fs::symlink_metadata(&record.input_path)?.is_file(),
            "Invocation input changed type"
        );
        let input: Invocation = crate::storage::read_json(&record.input_path)?;
        let digest: String = self.connection()?.query_row(
            "SELECT input_digest FROM runs WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        ensure!(
            input.id() == id
                && digest == format!("{:x}", Sha256::digest(serde_json::to_vec(&input)?)),
            "Invocation input changed after submission"
        );
        Ok(input)
    }

    pub fn promote(&self, id: &str, owner: &str) -> Result<bool> {
        Ok(self.connection()?.execute("UPDATE runs SET background=1,updated=unixepoch() WHERE id=?1 AND owner=?2 AND state IN ('prepared','running')",params![id,owner])?==1)
    }

    pub fn request_stop(
        &self,
        id: &str,
        owner: &str,
        cause: jcode_tool_types::StopCause,
    ) -> Result<bool> {
        Ok(self.connection()?.execute("UPDATE runs SET stop_cause=COALESCE(stop_cause,?3),updated=unixepoch() WHERE id=?1 AND owner=?2 AND state IN ('prepared','running')",params![id,owner,serde_json::to_string(&cause)?])?==1)
    }

    /// Finalization references only files already flushed by the output owner.
    pub fn finish(&self, record: &RunRecord) -> Result<()> {
        ensure!(
            record.state.terminal(),
            "Finalization requires a terminal state"
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = query_record(&transaction, &record.id)?.context("Unknown invocation")?;
        ensure!(
            previous.owner == record.owner,
            "Invocation belongs to another runtime"
        );
        if previous.state.terminal() {
            ensure!(
                previous.state == record.state
                    && previous.result_path == record.result_path
                    && previous.output_path == record.output_path
                    && previous.output_bytes == record.output_bytes
                    && previous.complete == record.complete,
                "Conflicting terminal invocation receipt"
            );
            return Ok(());
        }
        let bytes = i64::try_from(record.output_bytes)
            .context("Output size exceeds metadata representation")?;
        transaction.execute("UPDATE runs SET state=?2,result_path=?3,output_path=?4,output_bytes=?5,complete=?6,updated=unixepoch() WHERE id=?1", params![
            record.id, record.state.as_str(), record.result_path.as_deref().map(path_text).transpose()?,
            record.output_path.as_deref().map(path_text).transpose()?, bytes, record.complete])?;
        transaction.execute("UPDATE command_handoffs SET state='finished' WHERE run_id=?1 AND COALESCE(worker_owner,parent_owner)=?2",params![record.id,record.owner])?;
        transaction.commit()?;
        Ok(())
    }

    /// Indexed, bounded metadata pagination. This never opens a payload file.
    pub fn list(&self, session: &str, after: Option<&str>, limit: u32) -> Result<Vec<RunRecord>> {
        ensure!(
            (1..=1000).contains(&limit),
            "Metadata page limit must be 1 through 1000"
        );
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT id FROM runs WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?;
        let ids = statement
            .query_map(params![session, after.unwrap_or(""), limit], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                query_record(&connection, &id)?
                    .context("Invocation disappeared during metadata read")
            })
            .collect()
    }
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().context("Execution state path must be UTF-8")
}

fn query_record(connection: &Connection, id: &str) -> Result<Option<RunRecord>> {
    Ok(connection
        .query_row("SELECT * FROM runs WHERE id=?1", [id], |row| {
            let raw_state: String = row.get("state")?;
            let state = match raw_state.as_str() {
                "prepared" => RunState::Prepared,
                "running" => RunState::Running,
                "completed" => RunState::Completed,
                "failed" => RunState::Failed,
                "cancelled" => RunState::Cancelled,
                "interrupted" => RunState::Interrupted,
                _ => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other("Invalid persisted invocation state")),
                    ));
                }
            };
            let stop: Option<String> = row.get("stop_cause")?;
            let stop_cause = stop
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            let bytes: i64 = row.get("output_bytes")?;
            let output_bytes = u64::try_from(bytes).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(RunRecord {
                id: row.get("id")?,
                session_id: row.get("session_id")?,
                message_id: row.get("message_id")?,
                tool: row.get("tool")?,
                state,
                owner: row.get("owner")?,
                input_path: PathBuf::from(row.get::<_, String>("input_path")?),
                result_path: row
                    .get::<_, Option<String>>("result_path")?
                    .map(PathBuf::from),
                output_path: row
                    .get::<_, Option<String>>("output_path")?
                    .map(PathBuf::from),
                output_bytes,
                complete: row.get("complete")?,
                background: row.get("background")?,
                stop_cause,
                parent_id: row.get("parent_id")?,
            })
        })
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation() -> Invocation {
        Invocation {
            session_id: "session".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "synthetic".into(),
            input: serde_json::json!({"value": 1}),
            working_dir: None,
            received_result_digest: None,
        }
    }

    #[test]
    fn scope_replay_and_terminal_transactions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let mut call = invocation();
        let PreparedInvocation::New(mut record) = store.prepare(&call, "owner")? else {
            panic!()
        };
        assert!(matches!(
            store.prepare(&call, "owner")?,
            PreparedInvocation::Existing(_)
        ));
        call.input = serde_json::json!({"value": 2});
        assert!(store.prepare(&call, "owner").is_err());
        call.session_id = "another".into();
        assert!(matches!(
            store.prepare(&call, "owner")?,
            PreparedInvocation::New(_)
        ));
        store.start(&record.id, "owner")?;
        assert!(store.start(&record.id, "owner").is_err());
        record.state = RunState::Cancelled;
        store.finish(&record)?;
        store.finish(&record)?;
        record.state = RunState::Completed;
        assert!(store.finish(&record).is_err());
        assert_eq!(
            ExecutionStore::open(dir.path())?
                .inspect(&record.id)?
                .unwrap()
                .state,
            RunState::Cancelled
        );
        Ok(())
    }

    #[test]
    fn concurrent_preparation_has_exactly_one_owner() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || store.prepare(&invocation(), "owner"))
            })
            .collect();
        let mut new = 0;
        for thread in threads {
            if matches!(thread.join().unwrap()?, PreparedInvocation::New(_)) {
                new += 1;
            }
        }
        assert_eq!(new, 1);
        assert_eq!(store.list("session", None, 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn subprocess_prepare_fixture() -> Result<()> {
        let Some(root) = std::env::var_os("JCODE_EXECUTION_RACE_DIR") else {
            return Ok(());
        };
        let root = PathBuf::from(root);
        let pid = std::process::id();
        std::fs::write(root.join(format!("ready-{pid}")), b"ready")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !root.join("go").exists() {
            ensure!(
                std::time::Instant::now() < deadline,
                "Admission fixture barrier timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let store = ExecutionStore::open(&root)?;
        if matches!(
            store.prepare(&invocation(), "owner")?,
            PreparedInvocation::New(_)
        ) {
            std::fs::write(root.join(format!("winner-{pid}")), b"new")?;
        }
        Ok(())
    }

    #[test]
    fn independent_processes_cannot_both_claim_the_same_invocation() -> Result<()> {
        struct Children(Vec<std::process::Child>);
        impl Drop for Children {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let mut children = Children(Vec::new());
        for _ in 0..2 {
            children.0.push(
                std::process::Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "execution::store::tests::subprocess_prepare_fixture",
                        "--nocapture",
                    ])
                    .env("JCODE_EXECUTION_RACE_DIR", dir.path())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn()?,
            );
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let ready = std::fs::read_dir(dir.path())?
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-"))
                .count();
            if ready == 2 {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Child admission fixtures did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(dir.path().join("go"), b"go")?;
        for child in &mut children.0 {
            assert!(child.wait()?.success());
        }
        let winners = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("winner-"))
            .count();
        assert_eq!(winners, 1);
        assert_eq!(store.list("session", None, 10)?.len(), 1);
        Ok(())
    }
}
