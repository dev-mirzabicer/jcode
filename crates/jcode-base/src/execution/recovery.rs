//! Recovery publishes evidence, never replays a producer or guesses a PID target.
#[cfg(unix)]
use super::storage::OutputLease;
use super::{ExecutionStore, RunRecord, RunState};
use anyhow::{Context, Result, ensure};
#[cfg(unix)]
use jcode_tool_types::StopCause;

enum Prepared {
    Done(RunRecord),
    Unproven,
    #[cfg(unix)]
    Lost {
        record: RunRecord,
        lease: OutputLease,
    },
}

impl ExecutionStore {
    pub(super) fn interrupted_result(
        &self,
        record: &RunRecord,
        target: std::num::NonZeroUsize,
    ) -> Result<jcode_tool_types::ToolOutput> {
        use jcode_tool_types::{OutputReference, OutputSource, ToolOutput, UnavailableReference};
        ensure!(
            record.state == RunState::Interrupted && record.result_path.is_none(),
            "Interruption receipt requires a missing terminal result"
        );
        let partial = record.output_path.as_ref().map(|path| OutputReference {
            invocation_id: record.id.clone(),
            path: path.clone(),
            bytes: record.output_bytes,
            complete: false,
            continuation: None,
            manifest_path: None,
        });
        let mut body = format!(
            "Execution {} was interrupted after its owner was lost. No sealed result proves the operation's final outcome. Completed effects are unknown and were not rolled back. The operation was not repeated.\n",
            record.id
        );
        if let Some(partial) = &partial {
            body.push_str(&format!(
                "Available captured prefix: {} ({} committed bytes).\n",
                partial.path.display(),
                partial.bytes
            ));
        }
        let directory = self.root().join("receipts");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Recovery receipt directory changed type"
        );
        let path = directory.join(format!("{}.interrupted.txt", record.id));
        if path.try_exists()? {
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.len() == body.len() as u64,
                "Interruption receipt changed"
            );
            ensure!(
                std::fs::read_to_string(&path)? == body,
                "Interruption receipt identity changed"
            );
        } else {
            crate::storage::write_text_secret(&path, &body)?;
        }
        let mut output=ToolOutput::new(body).with_error(true).with_metadata(serde_json::json!({"run_id":record.id,"state":record.state,"effects_unknown":true,"partial_output":partial}));
        output.source = OutputSource::Unavailable(UnavailableReference {
            invocation_id: record.id.clone(),
            receipt_path: path,
            partial_output: partial,
        });
        Ok(super::present(output, target))
    }
    /// Recover a proven terminal witness, or record interruption only after the
    /// old runtime image and its recorded command group have ceased execution.
    /// None means there is insufficient evidence to retire a nonterminal owner.
    pub async fn recover_lost_owner(&self, id: &str) -> Result<Option<RunRecord>> {
        let store = self.clone();
        let id = id.to_string();
        let prepared = tokio::task::spawn_blocking(move || -> Result<Prepared> {
            let record = store.inspect(&id)?.context("Unknown invocation")?;
            if record.state.terminal() {
                return Ok(Prepared::Done(record));
            }
            if store.terminal_witness(&id)?.is_some() {
                return match super::storage::try_output_lease(&store, &id)? {
                    Some(lease) => Ok(Prepared::Done(
                        store.recover_terminal_under_lease(&id, &lease)?,
                    )),
                    None => Ok(Prepared::Unproven),
                };
            }
            #[cfg(unix)]
            {
                let owner = store.runtime_endpoint(&record.owner)?.context(
                    "Legacy invocation has no verified runtime identity; no recovery was inferred",
                )?;
                if !owner.image_is_gone(&store)? {
                    return Ok(Prepared::Unproven);
                }
                let lease = super::storage::output_lease(&store, &id)?;
                store.recover_owned_storage(&id, &lease)?;
                let record = store
                    .inspect(&id)?
                    .context("Invocation disappeared during storage recovery")?;
                // Offline storage must not look like absence of a terminal witness.
                if record.output_path.is_some() {
                    let _ = store.open_output_file(&id)?;
                }
                Ok(Prepared::Lost { record, lease })
            }
            #[cfg(not(unix))]
            {
                Ok(Prepared::Unproven)
            }
        })
        .await??;
        match prepared {
            Prepared::Done(record) => Ok(Some(record)),
            Prepared::Unproven => Ok(None),
            #[cfg(unix)]
            Prepared::Lost { record, lease } => self.finish_lost_owner(record, lease).await,
        }
    }

    #[cfg(unix)]
    async fn finish_lost_owner(
        &self,
        record: RunRecord,
        lease: OutputLease,
    ) -> Result<Option<RunRecord>> {
        self.verify_lost_native_processes(&record.id).await?;
        #[cfg(unix)]
        {
            let source = self.clone();
            let id = record.id.clone();
            let process =
                tokio::task::spawn_blocking(move || source.command_process(&id)).await??;
            if let Some(process) = process {
                ensure!(
                    !crate::platform::process_group_has_live_members(process.group).await?,
                    "Execution {} lost its runtime but its recorded process group still has live members. No cancellation or completion was claimed, and no PID was signalled.",
                    record.id
                );
            }
        }
        #[cfg(unix)]
        {
            let store = self.clone();
            tokio::task::spawn_blocking(move || {
                lease.validate(&store, &record.id)?;
                let mut current = store
                    .inspect(&record.id)?
                    .context("Invocation disappeared during recovery")?;
                if current.state.terminal() {
                    return Ok(Some(current));
                }
                ensure!(
                    current.owner == record.owner,
                    "Execution ownership changed during recovery; inspect the new owner"
                );
                let owner = store
                    .runtime_endpoint(&current.owner)?
                    .context("Runtime identity disappeared")?;
                ensure!(
                    owner.image_is_gone(&store)?,
                    "Runtime became active during recovery"
                );
                if store.terminal_witness(&record.id)?.is_some() {
                    return store
                        .recover_terminal_under_lease(&record.id, &lease)
                        .map(Some);
                }
                ensure!(
                    store.request_stop(&current.id, &current.owner, StopCause::OwnerCrash)?,
                    "Owner-loss receipt lost invocation ownership"
                );
                current.stop_cause.get_or_insert(StopCause::OwnerCrash);
                current.state = RunState::Interrupted;
                current.complete = false;
                store.finish(&current)?;
                Ok(Some(current))
            })
            .await?
        }
    }
}

#[cfg(all(test, unix))]
#[path = "recovery_tests.rs"]
mod tests;
