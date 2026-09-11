//! A native worker may claim a command once. Its gated child cannot execute
//! user code until its exact process identity has been durably registered.
use super::process::ProcessIdentity;
use super::{ExecutionStore, RunRecord, RuntimeEndpoint, StorageConfig};
use anyhow::{Context, Result, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub command: String,
    pub working_dir: PathBuf,
    pub timeout_ms: Option<u64>,
    pub background: bool,
    pub notify: bool,
    pub wake: bool,
    pub title: Option<String>,
    pub storage: StorageConfig,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema: u32,
    run_id: String,
    parent_owner: String,
    request: CommandRequest,
}

pub struct ClaimedCommand {
    pub record: RunRecord,
    pub request: CommandRequest,
    pub parent: RuntimeEndpoint,
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 68
            && id.starts_with("run-")
            && id[4..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid command invocation identity"
    );
    Ok(())
}
impl ExecutionStore {
    pub fn is_command_handoff(&self, id: &str, parent: &str, worker: &str) -> Result<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM command_handoffs WHERE run_id=?1 AND parent_owner=?2 AND worker_owner=?3)",params![id,parent,worker],|row|row.get(0))?)
    }
    pub fn prepare_command(&self, record: &RunRecord, request: CommandRequest) -> Result<()> {
        validate_id(&record.id)?;
        let payload = Payload {
            schema: 1,
            run_id: record.id.clone(),
            parent_owner: record.owner.clone(),
            request,
        };
        let bytes = serde_json::to_vec(&payload)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let directory = self.root().join("commands");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Command payload directory changed type"
        );
        let path = directory.join(format!("{}.json", record.id));
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running' AND stop_cause IS NULL)",params![record.id,record.owner],|row|row.get(0))?;
        ensure!(
            owned,
            "Command was stopped or its owner changed before preparation"
        );
        ensure!(
            !path.exists(),
            "Command payload already exists; recover its receipt instead of launching it again"
        );
        crate::storage::write_json_secret(&path, &payload)?;
        transaction.execute("INSERT INTO command_handoffs (run_id,parent_owner,payload_digest,state) VALUES (?1,?2,?3,'prepared')",params![record.id,record.owner,digest])?;
        transaction.commit()?;
        Ok(())
    }

    fn command_payload(&self, id: &str) -> Result<Payload> {
        validate_id(id)?;
        let path = self.root().join("commands").join(format!("{id}.json"));
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "Command payload is not a regular file"
        );
        let payload: Payload = crate::storage::read_json(&path)?;
        ensure!(
            payload.schema == 1 && payload.run_id == id,
            "Command payload identity mismatch"
        );
        let expected: String = self.connection()?.query_row(
            "SELECT payload_digest FROM command_handoffs WHERE run_id=?1",
            [id],
            |row| row.get(0),
        )?;
        ensure!(
            expected == format!("{:x}", Sha256::digest(serde_json::to_vec(&payload)?)),
            "Command payload changed after preparation"
        );
        Ok(payload)
    }

    pub fn claim_command(&self, id: &str, worker: &RuntimeEndpoint) -> Result<ClaimedCommand> {
        let payload = self.command_payload(id)?;
        let worker = self
            .runtime_endpoint(&worker.id)?
            .context("Command worker is not registered")?;
        ensure!(
            worker.process_id == std::process::id(),
            "Only the registered worker process can claim a command"
        );
        ensure!(
            worker.has_live_lease()?,
            "Command worker is not registered as live"
        );
        let parent = self
            .runtime_endpoint(&payload.parent_owner)?
            .context("Command has no verified parent runtime")?;
        if !payload.request.background {
            ensure!(
                parent.has_live_lease()?,
                "Foreground command parent is unavailable; command was not started"
            );
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(transaction.execute("UPDATE command_handoffs SET worker_owner=?2,state='claimed' WHERE run_id=?1 AND parent_owner=?3 AND state='prepared'",params![id,worker.id,payload.parent_owner])?==1,"Command has already been claimed; it cannot be executed again");
        ensure!(transaction.execute("UPDATE runs SET owner=?2,background=?4,updated=unixepoch() WHERE id=?1 AND owner=?3 AND state='running' AND stop_cause IS NULL",params![id,worker.id,payload.parent_owner,payload.request.background])?==1,"Command was stopped before worker activation");
        transaction.commit()?;
        let record = self.inspect(id)?.context("Claimed command disappeared")?;
        Ok(ClaimedCommand {
            record,
            request: payload.request,
            parent,
        })
    }

    pub fn register_command_process(
        &self,
        id: &str,
        worker: &str,
        process: &ProcessIdentity,
    ) -> Result<()> {
        validate_id(id)?;
        let endpoint = self
            .runtime_endpoint(worker)?
            .context("Missing command worker")?;
        ensure!(
            endpoint.process_id == std::process::id() && process.pid != endpoint.process_id,
            "Command process must be a child, not the Jcode runtime"
        );
        ensure!(
            process.pid == process.group && process.matches_live()?,
            "Command child has no verified private process group"
        );
        ensure!(self.connection()?.execute("UPDATE command_handoffs SET process_identity=?3,state='registered' WHERE run_id=?1 AND worker_owner=?2 AND state='claimed' AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running')",params![id,worker,serde_json::to_string(process)?])?==1,"Command process registration lost ownership");
        Ok(())
    }

    /// Called only by the gated child before exec. The actual process identity
    /// and committed state, rather than a filename or PID alone, authorize work.
    pub fn authorize_command_exec(
        &self,
        id: &str,
        current: &ProcessIdentity,
    ) -> Result<CommandRequest> {
        ensure!(
            current.pid == std::process::id(),
            "Only the registered child may authorize its own exec"
        );
        let payload = self.command_payload(id)?;
        let record = self.inspect(id)?.context("Command invocation is missing")?;
        let endpoint = self
            .runtime_endpoint(&record.owner)?
            .context("Command worker identity is unavailable")?;
        ensure!(
            endpoint.has_live_lease()?,
            "Command worker is unavailable; user command was not started"
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (worker,identity):(String,String)=transaction.query_row("SELECT worker_owner,process_identity FROM command_handoffs WHERE run_id=?1 AND state='registered'",[id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        let identity: ProcessIdentity = serde_json::from_str(&identity)?;
        ensure!(worker == endpoint.id, "Command worker changed before exec");
        ensure!(
            identity == *current && current.matches_live()?,
            "Command gate belongs to a different process"
        );
        let allowed:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running' AND stop_cause IS NULL)",params![id,worker],|row|row.get(0))?;
        ensure!(allowed, "Command was stopped before exec");
        ensure!(transaction.execute("UPDATE command_handoffs SET state='executing',exec_authorized=1 WHERE run_id=?1 AND state='registered'",[id])?==1,"Command exec was already authorized");
        transaction.commit()?;
        Ok(payload.request)
    }

    pub fn command_process(&self, id: &str) -> Result<Option<ProcessIdentity>> {
        use rusqlite::OptionalExtension;
        let stored: Option<Option<String>> = self
            .connection()?
            .query_row(
                "SELECT process_identity FROM command_handoffs WHERE run_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        stored
            .flatten()
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn command_was_started(&self, id: &str) -> Result<bool> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_handoffs WHERE run_id=?1 AND exec_authorized=1)",
            [id],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};
    fn runtime(store: &ExecutionStore) -> Result<(RuntimeEndpoint, std::fs::File)> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = store.root().join(format!("{id}.lease"));
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        lease.try_lock()?;
        let endpoint =
            RuntimeEndpoint::new(id, store.root().join("fixture.sock"), path, "0".repeat(64));
        store.register_runtime(&endpoint)?;
        Ok((endpoint, lease))
    }
    fn prepared(store: &ExecutionStore, parent: &RuntimeEndpoint) -> Result<RunRecord> {
        let input = Invocation {
            session_id: "handoff".into(),
            message_id: "message".into(),
            call_path: vec![uuid::Uuid::new_v4().to_string()],
            tool: "command".into(),
            input: serde_json::json!({}),
            working_dir: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, &parent.id)? else {
            panic!()
        };
        store.start(&record.id, &parent.id)?;
        store.prepare_command(
            &record,
            CommandRequest {
                command: "synthetic command".into(),
                working_dir: store.root().to_path_buf(),
                timeout_ms: None,
                background: false,
                notify: true,
                wake: false,
                title: None,
                storage: StorageConfig::default(),
            },
        )?;
        Ok(record)
    }
    #[test]
    fn one_worker_claims_the_same_identity_once() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let (parent, _parent) = runtime(&store)?;
        let (worker, _worker) = runtime(&store)?;
        let record = prepared(&store, &parent)?;
        let claimed = store.claim_command(&record.id, &worker)?;
        assert_eq!(claimed.record.id, record.id);
        assert_eq!(claimed.record.owner, worker.id);
        assert!(store.claim_command(&record.id, &worker).is_err());
        assert!(!store.command_was_started(&record.id)?);
        assert!(
            store
                .register_command_process(
                    &record.id,
                    &worker.id,
                    &ProcessIdentity::capture(std::process::id())?
                )
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn changed_payload_or_stopped_parent_cannot_transfer_ownership() -> Result<()> {
        for tamper in [false, true] {
            let directory = tempfile::tempdir()?;
            let store = ExecutionStore::open(directory.path())?;
            let (parent, _parent) = runtime(&store)?;
            let (worker, _worker) = runtime(&store)?;
            let record = prepared(&store, &parent)?;
            if tamper {
                let path = store
                    .root()
                    .join("commands")
                    .join(format!("{}.json", record.id));
                let mut payload: Payload = crate::storage::read_json(&path)?;
                payload.request.command = "changed".into();
                crate::storage::write_json_secret(&path, &payload)?;
            } else {
                store.request_stop(
                    &record.id,
                    &parent.id,
                    jcode_tool_types::StopCause::HumanCancellation,
                )?;
            }
            assert!(store.claim_command(&record.id, &worker).is_err());
            assert_eq!(store.inspect(&record.id)?.unwrap().owner, parent.id);
            assert!(!store.command_was_started(&record.id)?);
        }
        Ok(())
    }
}
