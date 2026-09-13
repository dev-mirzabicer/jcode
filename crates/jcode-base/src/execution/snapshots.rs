//! Durable as-of inspection artifacts. Session remains the only mutable history.
use super::{ExecutionStore, Invocation, RunRecord};
use crate::message::{ContentBlock, Message};
use crate::session::{Session, StoredMessage};
use anyhow::{Context, Result, ensure};
use jcode_context_core::ProjectedMessageSource;
use jcode_tool_types::inspection::TranscriptRange;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::Read;

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
struct SourceMessage {
    id: String,
    digest: String,
}
#[derive(Serialize, Deserialize)]
struct ProjectedMessage {
    digest: String,
    start: usize,
    end: usize,
    summary: Option<SummaryOrigin>,
}
#[derive(Serialize, Deserialize)]
struct SummaryOrigin {
    transaction_id: String,
    operation_index: usize,
}
#[derive(Serialize, Deserialize)]
struct SnapshotTool {
    reference: String,
    message_id: String,
    provider_id: String,
    name: String,
    input_digest: String,
    run: Option<RunRecord>,
    /// Captured stored results, not reconstructed producer output.
    legacy_results: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    id: String,
    reader: String,
    target: String,
    captured_at: i64,
    context_revision: u64,
    messages: Vec<SourceMessage>,
    projected: Vec<ProjectedMessage>,
    context_digest: String,
    instructions_digest: String,
    tools: Vec<SnapshotTool>,
}

/// A read holds shared kernel ownership until all requested backing reads finish.
/// Pruning never invalidates a reader halfway through its operation.
pub struct SnapshotRead {
    store: ExecutionStore,
    manifest: Manifest,
    _lease: File,
    stop: Option<jcode_agent_runtime::InterruptSignal>,
}

#[derive(Debug, Default)]
pub struct SnapshotPruneOutcome {
    pub pruned: usize,
    pub reclaimed_blobs: usize,
    pub deferred_for_readers: bool,
    pub errors: Vec<super::RetentionIssue>,
}

impl ExecutionStore {
    pub fn create_inspection_snapshot(
        &self,
        reader: &str,
        target: &str,
        now: i64,
    ) -> Result<String> {
        self.create_inspection_snapshot_with_stop(reader, target, now, None)
    }

    pub fn create_inspection_snapshot_with_stop(
        &self,
        reader: &str,
        target: &str,
        now: i64,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<String> {
        ensure!(
            !stop.is_some_and(|signal| signal.is_set()),
            "Session inspection cancelled"
        );
        ensure!(
            !reader.is_empty(),
            "Inspection requires an originating reader session"
        );
        ensure!(
            !target.is_empty()
                && target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "Invalid inspection target identity"
        );
        let root = self
            .root()
            .parent()
            .context("Missing execution namespace")?;
        let path = root.join("sessions").join(format!("{target}.json"));
        let (source, runs) =
            Session::capture_readonly_with_metadata(&path, target, stop, |session| {
                let mut connection = self.connection()?;
                let tx = connection.transaction()?;
                let ids = {
                    let mut query =
                        tx.prepare("SELECT id FROM runs WHERE session_id=?1 ORDER BY id")?;
                    query
                        .query_map([&session.id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                };
                ids.into_iter()
                    .map(|id| {
                        super::store::query_record(&tx, &id)?
                            .map(|record| (id, record))
                            .context("Captured run disappeared")
                    })
                    .collect::<Result<HashMap<_, _>>>()
            })?;
        // The short Session lease and SQLite read transaction are gone before
        // projection, blob publication, or any large output acquisition.
        let session = source.session();
        let projection =
            jcode_context_core::project_context(&session.messages, &session.context_view)?;
        let _lease = self
            .snapshot_lease_with_stop(false, false, stop)?
            .context("Snapshot publication lease unavailable")?;
        let mut refs = BTreeSet::new();
        let mut put = |value: &serde_json::Value| -> Result<String> {
            ensure!(
                !stop.is_some_and(|signal| signal.is_set()),
                "Session inspection cancelled before blob publication"
            );
            let digest = self.put_inspection_blob(value)?;
            refs.insert(digest.clone());
            Ok(digest)
        };
        let messages = session
            .messages
            .iter()
            .map(|message| {
                Ok(SourceMessage {
                    id: message.id.clone(),
                    digest: put(&serde_json::to_value(message)?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let positions: HashMap<_, _> = session
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.as_str(), index + 1))
            .collect();
        ensure!(
            positions.len() == messages.len(),
            "Session has ambiguous duplicate message identities"
        );
        let projected = projection
            .messages
            .iter()
            .zip(&projection.sources)
            .map(|(message, source)| {
                let (start, end, summary) = match source {
                    ProjectedMessageSource::RawMessage { stored_index, .. } => {
                        (*stored_index + 1, *stored_index + 1, None)
                    }
                    ProjectedMessageSource::RangeSummary {
                        operation,
                        source_range,
                    } => (
                        *positions
                            .get(source_range.start_message_id.as_str())
                            .context("Summary start disappeared")?,
                        *positions
                            .get(source_range.end_message_id.as_str())
                            .context("Summary end disappeared")?,
                        Some(SummaryOrigin {
                            transaction_id: operation.transaction_id.clone(),
                            operation_index: operation.operation_index,
                        }),
                    ),
                };
                Ok(ProjectedMessage {
                    digest: put(&serde_json::to_value(message)?)?,
                    start,
                    end,
                    summary,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let context_digest = put(&serde_json::to_value(&session.context_view)?)?;
        let instructions_digest = put(&serde_json::json!({
            "system_prompt": session.system_prompt,
            "active_skill": session.active_skill,
            "agent_profile_message_ids": session.agent_profile_message_ids,
            "startup_context": session.startup_context,
        }))?;
        let mut tools = Vec::new();
        for (message_index, message) in session.messages.iter().enumerate() {
            for (block_index, block) in message.content.iter().enumerate() {
                let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                else {
                    continue;
                };
                let scope = Invocation {
                    session_id: session.id.clone(),
                    message_id: message.id.clone(),
                    call_path: vec![id.clone()],
                    tool: name.clone(),
                    input: input.clone(),
                    working_dir: None,
                    received_result_digest: None,
                };
                let run = runs.get(&scope.id()).cloned();
                let mut legacy_results = Vec::new();
                for later in &session.messages[message_index + 1..] {
                    if later.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { id: later_id, .. } if later_id == id)) { break; }
                    for block in &later.content {
                        if matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)
                        {
                            legacy_results.push(put(&serde_json::to_value(block)?)?);
                        }
                    }
                }
                tools.push(SnapshotTool {
                    reference: format!("tool-{}-{}", message_index + 1, block_index + 1),
                    message_id: message.id.clone(),
                    provider_id: id.clone(),
                    name: name.clone(),
                    input_digest: put(input)?,
                    run,
                    legacy_results,
                });
            }
        }
        // Nested executions use scoped durable invocation IDs, never repeating
        // batch ordinal/provider IDs. Freeze their own input and prefix/status.
        let mut nested: Vec<_> = runs.into_values().collect();
        nested.sort_by(|a, b| a.id.cmp(&b.id));
        for run in nested {
            if run.parent_id.is_none() || !positions.contains_key(run.message_id.as_str()) {
                continue;
            }
            let invocation = self.invocation_input(&run.id)?;
            tools.push(SnapshotTool {
                reference: run.id.clone(),
                message_id: run.message_id.clone(),
                provider_id: invocation.call_path.last().cloned().unwrap_or_default(),
                name: run.tool.clone(),
                input_digest: put(&invocation.input)?,
                run: Some(run),
                legacy_results: vec![],
            });
        }
        let id = format!("snapshot-{}", uuid::Uuid::new_v4().simple());
        let manifest = Manifest {
            schema: 1,
            id: id.clone(),
            reader: reader.into(),
            target: session.id.clone(),
            captured_at: now,
            context_revision: session.context_view.revision,
            messages,
            projected,
            context_digest,
            instructions_digest,
            tools,
        };
        let manifest_digest = put(&serde_json::to_value(&manifest)?)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO inspection_snapshots(id,reader,target,created,manifest_digest,state) VALUES (?1,?2,?3,?4,?5,'retained')", params![id, reader, session.id, now, manifest_digest])?;
        for digest in refs {
            tx.execute(
                "INSERT INTO inspection_blob_refs(snapshot_id,digest) VALUES (?1,?2)",
                params![id, digest],
            )?;
        }
        for tool in &manifest.tools {
            if let Some(run) = &tool.run {
                tx.execute("INSERT OR IGNORE INTO inspection_output_refs(snapshot_id,run_id) VALUES (?1,?2)", params![id,run.id])?;
            }
        }
        Self::touch_activity_in(&tx, reader, now)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn read_inspection_snapshot(
        &self,
        reader: &str,
        id: &str,
        now: i64,
    ) -> Result<SnapshotRead> {
        self.read_inspection_snapshot_with_stop(reader, id, now, None)
    }

    pub fn read_inspection_snapshot_with_stop(
        &self,
        reader: &str,
        id: &str,
        now: i64,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<SnapshotRead> {
        self.read_snapshot_authorized(reader, id, now, stop, false)
    }

    /// Privileged same-user client access. It does not change snapshot ownership.
    pub fn read_inspection_snapshot_for_client(
        &self,
        reader: &str,
        id: &str,
        now: i64,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<SnapshotRead> {
        self.read_snapshot_authorized(reader, id, now, stop, true)
    }

    fn read_snapshot_authorized(
        &self,
        reader: &str,
        id: &str,
        now: i64,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
        trusted_client: bool,
    ) -> Result<SnapshotRead> {
        validate_snapshot_id(id)?;
        let lease = self
            .snapshot_lease_with_stop(true, false, stop)?
            .context("Snapshot read ownership unavailable")?;
        let row: Option<(String, String, String)> = self
            .connection()?
            .query_row(
                "SELECT reader,manifest_digest,state FROM inspection_snapshots WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (owner, digest, state) =
            row.context("Unknown inspection snapshot; open a new outline")?;
        let root = self
            .root()
            .parent()
            .context("Missing inspection namespace")?;
        ensure!(
            trusted_client
                || owner == reader
                || Session::inspection_has_ancestor(root, &owner, reader, stop)?,
            "Inspection snapshot belongs to another reader session"
        );
        ensure!(
            state == "retained",
            "Inspection snapshot was deliberately pruned; previously received transcript content is unchanged"
        );
        let manifest: Manifest = self.inspection_blob_with_stop(&digest, stop)?;
        ensure!(
            manifest.schema == 1 && manifest.id == id && manifest.reader == owner,
            "Inspection manifest identity mismatch"
        );
        self.touch_activity(reader, now)?;
        Ok(SnapshotRead {
            store: self.clone(),
            manifest,
            _lease: lease,
            stop: stop.cloned(),
        })
    }

    pub(super) fn snapshot_lease(&self, shared: bool, nonblocking: bool) -> Result<Option<File>> {
        self.snapshot_lease_with_stop(shared, nonblocking, None)
    }

    fn snapshot_lease_with_stop(
        &self,
        shared: bool,
        nonblocking: bool,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<Option<File>> {
        ensure!(
            !stop.is_some_and(|signal| signal.is_set()),
            "Snapshot inspection cancelled"
        );
        let directory = self.root().join("snapshots");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Snapshot store changed type"
        );
        jcode_core::fs::set_directory_permissions_owner_only(&directory)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(directory.join("ownership.lock"))?;
        ensure!(
            file.metadata()?.is_file(),
            "Invalid snapshot ownership file"
        );
        if nonblocking {
            let result = if shared {
                file.try_lock_shared()
            } else {
                file.try_lock()
            };
            match result {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        } else if let Some(stop) = stop {
            loop {
                ensure!(
                    !stop.is_set(),
                    "Snapshot inspection cancelled while waiting for storage ownership"
                );
                let result = if shared {
                    file.try_lock_shared()
                } else {
                    file.try_lock()
                };
                match result {
                    Ok(()) => break,
                    Err(std::fs::TryLockError::WouldBlock) => {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
        } else if shared {
            file.lock_shared()?;
        } else {
            file.lock()?;
        }
        Ok(Some(file))
    }

    fn put_inspection_blob(&self, value: &serde_json::Value) -> Result<String> {
        let bytes = serde_json::to_vec(value)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let directory = self.root().join("snapshots/blobs");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Snapshot blob directory changed type"
        );
        jcode_core::fs::set_directory_permissions_owner_only(&directory)?;
        let path = directory.join(&digest);
        // Register ownership before filesystem publication. Interrupted unreferenced
        // blobs can be reclaimed from this index, never guessed by directory age.
        self.connection()?.execute("INSERT INTO inspection_blobs(digest,bytes) VALUES (?1,?2) ON CONFLICT(digest) DO NOTHING", params![digest, i64::try_from(bytes.len())?])?;
        if path.exists() {
            let existing: serde_json::Value = self.inspection_blob(&digest)?;
            ensure!(
                existing == *value,
                "Content-addressed inspection blob conflicts"
            );
        } else {
            crate::storage::write_text_secret(&path, std::str::from_utf8(&bytes)?)?;
        }
        Ok(digest)
    }

    fn inspection_blob<T: DeserializeOwned>(&self, digest: &str) -> Result<T> {
        self.inspection_blob_with_stop(digest, None)
    }

    fn inspection_blob_with_stop<T: DeserializeOwned>(
        &self,
        digest: &str,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<T> {
        ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid inspection blob identity"
        );
        let path = self.root().join("snapshots/blobs").join(digest);
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "Inspection blob changed type"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(path)?;
        ensure!(file.metadata()?.is_file(), "Inspection blob changed type");
        let mut bytes = Vec::new();
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            ensure!(
                !stop.is_some_and(|signal| signal.is_set()),
                "Snapshot blob read cancelled"
            );
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            bytes.extend_from_slice(&buffer[..count]);
        }
        ensure!(
            format!("{:x}", hash.finalize()) == digest,
            "Inspection blob failed integrity validation"
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn prune_inspection_snapshots(&self, now: i64) -> Result<SnapshotPruneOutcome> {
        let Some(_lease) = self.snapshot_lease(false, true)? else {
            return Ok(SnapshotPruneOutcome {
                deferred_for_readers: true,
                ..Default::default()
            });
        };
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut query = tx.prepare("SELECT id FROM (SELECT s.id,s.reader,row_number() OVER(PARTITION BY s.reader,s.target ORDER BY s.created DESC,s.sequence DESC) AS position FROM inspection_snapshots s WHERE s.state='retained') ranked JOIN session_activity a ON a.session_id=ranked.reader WHERE position>2 AND a.last_active<=?1 AND a.retention_not_before<=?2 AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.session_id=ranked.reader AND r.state IN ('prepared','running')) AND NOT EXISTS(SELECT 1 FROM session_activity_leases l WHERE l.session_id=ranked.reader)")?;
            query
                .query_map(
                    rusqlite::params![now.saturating_sub(super::activity::IDLE_SECONDS), now],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in &ids {
            tx.execute(
                "UPDATE inspection_snapshots SET state='pruned' WHERE id=?1",
                [id],
            )?;
            tx.execute(
                "DELETE FROM inspection_blob_refs WHERE snapshot_id=?1",
                [id],
            )?;
            tx.execute(
                "DELETE FROM inspection_output_refs WHERE snapshot_id=?1",
                [id],
            )?;
        }
        tx.commit()?;
        let mut outcome = SnapshotPruneOutcome {
            pruned: ids.len(),
            ..Default::default()
        };
        match self.reclaim_inspection_blobs() {
            Ok((reclaimed, errors)) => {
                outcome.reclaimed_blobs = reclaimed;
                outcome.errors = errors;
            }
            Err(error) => outcome.errors.push(super::RetentionIssue {
                id: "snapshot-blobs".into(),
                message: format!(
                    "Snapshot pruning committed, but blob reclamation failed: {error:#}"
                ),
            }),
        }
        Ok(outcome)
    }

    fn reclaim_inspection_blobs(&self) -> Result<(usize, Vec<super::RetentionIssue>)> {
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT digest FROM inspection_blobs b WHERE NOT EXISTS(SELECT 1 FROM inspection_blob_refs r WHERE r.digest=b.digest)")?;
        let digests = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut reclaimed = 0;
        let mut errors = Vec::new();
        for digest in digests {
            let result = (|| -> Result<()> {
                ensure!(
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "Invalid registered inspection blob identity"
                );
                let path = self.root().join("snapshots/blobs").join(&digest);
                if path.exists() {
                    let _: serde_json::Value = self.inspection_blob(&digest)?;
                    std::fs::remove_file(&path)?;
                    #[cfg(unix)]
                    File::open(path.parent().context("Missing blob parent")?)?.sync_all()?;
                }
                connection.execute("DELETE FROM inspection_blobs WHERE digest=?1 AND NOT EXISTS(SELECT 1 FROM inspection_blob_refs WHERE digest=?1)", [&digest])?;
                Ok(())
            })();
            match result {
                Ok(()) => reclaimed += 1,
                Err(error) => errors.push(super::RetentionIssue {
                    id: digest,
                    message: format!("Blob reclamation remains incomplete: {error:#}"),
                }),
            }
        }
        Ok((reclaimed, errors))
    }
}

fn validate_snapshot_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 41
            && id.starts_with("snapshot-")
            && id[9..].bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid inspection snapshot identity"
    );
    Ok(())
}

#[path = "snapshot_render.rs"]
mod render;
