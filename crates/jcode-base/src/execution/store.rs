use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Prepared,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Prepared | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invocation {
    pub session_id: String,
    pub message_id: String,
    /// Full nested path, not a repeating batch ordinal or provider ID alone.
    pub call_path: Vec<String>,
    pub tool: String,
    pub input: serde_json::Value,
}

impl Invocation {
    pub fn id(&self) -> String {
        // Tuple serialization preserves field boundaries and prevents delimiter aliases.
        let scope = serde_json::to_vec(&(&self.session_id, &self.message_id, &self.call_path))
            .expect("string tuples serialize");
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
            version == 0 || version == SCHEMA,
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
        transaction.commit()?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection> {
        let path = self.root.join("index.sqlite");
        // Precreate with owner-only mode. WAL/SHM inherit the database mode and
        // the containing directory is private even during first open.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
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
        transaction.execute("INSERT INTO runs (id,session_id,message_id,tool,input_digest,state,owner,input_path) VALUES (?1,?2,?3,?4,?5,'prepared',?6,?7)",
            params![id, invocation.session_id, invocation.message_id, invocation.tool, digest, owner, path_text(&input_path)?])?;
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
    let row = connection.query_row("SELECT id,session_id,message_id,tool,state,owner,input_path,result_path,output_path,output_bytes,complete FROM runs WHERE id=?1", [id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?, row.get::<_, Option<String>>(8)?, row.get::<_, i64>(9)?, row.get::<_, bool>(10)?))
    }).optional()?;
    let Some((
        id,
        session_id,
        message_id,
        tool,
        state,
        owner,
        input,
        result,
        output,
        bytes,
        complete,
    )) = row
    else {
        return Ok(None);
    };
    let state = match state.as_str() {
        "prepared" => RunState::Prepared,
        "running" => RunState::Running,
        "completed" => RunState::Completed,
        "failed" => RunState::Failed,
        "cancelled" => RunState::Cancelled,
        "interrupted" => RunState::Interrupted,
        _ => bail!("Invalid persisted invocation state"),
    };
    Ok(Some(RunRecord {
        id,
        session_id,
        message_id,
        tool,
        state,
        owner,
        input_path: input.into(),
        result_path: result.map(Into::into),
        output_path: output.map(Into::into),
        output_bytes: u64::try_from(bytes)?,
        complete,
    }))
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
}
