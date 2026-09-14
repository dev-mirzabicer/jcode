use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use jcode_tool_types::{AcceptanceReference, OutputSource, ToolOutput};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

impl ExecutionStore {
    pub fn background_acceptance(&self, id: &str, requesting_owner: &str) -> Result<ToolOutput> {
        self.background_acceptance_with_child(id, requesting_owner, None)
    }

    pub fn background_acceptance_with_child(
        &self,
        id: &str,
        requesting_owner: &str,
        child: Option<&jcode_tool_types::delegation::ChildExecutionReceipt>,
    ) -> Result<ToolOutput> {
        ensure!(
            id.len() == 68
                && id.starts_with("run-")
                && id[4..].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid acceptance identity"
        );
        let record = self.inspect(id)?.context("Unknown background invocation")?;
        ensure!(
            record.background,
            "Execution has not been explicitly backgrounded"
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let allowed:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND (owner=?2 OR EXISTS(SELECT 1 FROM command_handoffs WHERE run_id=?1 AND parent_owner=?2)))",params![id,requesting_owner],|row|row.get(0))?;
        ensure!(
            allowed,
            "Background acceptance belongs to a different execution owner"
        );
        let existing: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM acceptance_receipts WHERE run_id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if existing {
            drop(transaction);
            return self
                .acceptance_result(id)?
                .context("Background receipt disappeared");
        }
        let directory = self.root().join("receipts");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Receipt directory changed type"
        );
        let path = directory.join(format!("{id}.accepted.json"));
        let body = format!(
            "Background execution accepted. Run: {id}\nUse bg status, wait, output or cancel with task_id=\"{id}\". Completion is a separate outcome.\nReceipt: {}",
            path.display()
        );
        let mut output=ToolOutput::new(body).with_metadata(serde_json::json!({"background":true,"task_id":id,"run_id":id,"output_file":record.output_path}));
        if let Some(child) = child {
            let registered: Option<String> = transaction
                .query_row(
                    "SELECT child_id FROM child_turns WHERE run_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(
                child.turn_id == id && registered.as_deref() == Some(child.child_id.as_str()),
                "Child acceptance does not match its admitted invocation"
            );
            output.output.push_str(&format!(
                "\nChild: {}\nArtifact directory: {}\nQueued: {}",
                child.child_id.as_str(),
                child.artifact_dir.display(),
                child.queued
            ));
            output
                .metadata
                .as_mut()
                .context("Missing acceptance metadata")?["child"] = serde_json::to_value(child)?;
        }
        output.source = OutputSource::Acceptance(AcceptanceReference {
            invocation_id: id.into(),
            path: path.clone(),
            live_output: record.output_path,
        });
        let bytes = serde_json::to_vec(&output)?;
        crate::storage::write_json_secret(&path, &output)?;
        transaction.execute(
            "INSERT INTO acceptance_receipts (run_id,receipt_path,digest) VALUES (?1,?2,?3)",
            params![
                id,
                path.to_str().context("Invalid receipt path")?,
                format!("{:x}", Sha256::digest(&bytes))
            ],
        )?;
        transaction.commit()?;
        Ok(output)
    }

    pub fn acceptance_result(&self, id: &str) -> Result<Option<ToolOutput>> {
        let row: Option<(String, String)> = self
            .connection()?
            .query_row(
                "SELECT receipt_path,digest FROM acceptance_receipts WHERE run_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((path, digest)) = row else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        ensure!(
            path.parent() == Some(self.root().join("receipts").as_path())
                && std::fs::symlink_metadata(&path)?.is_file(),
            "Background receipt is unavailable or changed type"
        );
        let output: ToolOutput = crate::storage::read_json(&path)?;
        ensure!(
            digest == format!("{:x}", Sha256::digest(serde_json::to_vec(&output)?)),
            "Background receipt changed after publication"
        );
        ensure!(
            matches!(&output.source,OutputSource::Acceptance(reference) if reference.invocation_id==id && reference.path==path),
            "Background receipt identity mismatch"
        );
        Ok(Some(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};
    #[test]
    fn acceptance_is_immutable_and_does_not_finalize_running_work() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let input = Invocation {
            session_id: "receipt".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        assert!(store.background_acceptance(&record.id, "owner").is_err());
        store.promote(&record.id, "owner")?;
        let first = store.background_acceptance(&record.id, "owner")?;
        let second = store.background_acceptance(&record.id, "owner")?;
        assert_eq!(serde_json::to_vec(&first)?, serde_json::to_vec(&second)?);
        assert_eq!(
            store.inspect(&record.id)?.unwrap().state,
            crate::execution::RunState::Running
        );
        assert!(
            store
                .background_acceptance(&record.id, "different-owner")
                .is_err()
        );
        let OutputSource::Acceptance(reference) = first.source else {
            panic!()
        };
        let mut changed: ToolOutput = crate::storage::read_json(&reference.path)?;
        changed.output.push_str("modified");
        crate::storage::write_json_secret(&reference.path, &changed)?;
        assert!(store.acceptance_result(&record.id).is_err());
        Ok(())
    }
}
