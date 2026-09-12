use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use jcode_background_types::BackgroundTaskProgress;
pub use jcode_tool_types::execution::ExecutionProgress;
use rusqlite::{TransactionBehavior, params};

impl ExecutionStore {
    pub(super) fn record_progress(
        &self,
        id: &str,
        owner: &str,
        progress: BackgroundTaskProgress,
        checkpoint: bool,
    ) -> Result<()> {
        let mut progress = progress.normalize();
        // This is a short status projection, not canonical command output. Raw
        // marker bytes remain in the retained stdout/stderr stream unchanged.
        progress.message = progress
            .message
            .map(|text| text.chars().take(512).collect());
        progress.unit = progress.unit.map(|text| text.chars().take(64).collect());
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = transaction
            .query_row(
                "SELECT progress FROM runs WHERE id=?1 AND owner=?2 AND state='running'",
                params![id, owner],
                |row| row.get(0),
            )
            .context("Progress has no running execution owner")?;
        let previous: Option<ExecutionProgress> = previous
            .map(|value| serde_json::from_str(&value))
            .transpose()?;
        if let Some(previous) = &previous {
            if progress.equivalent_to(&previous.value) && checkpoint == previous.checkpoint {
                return Ok(());
            }
            if progress.is_less_informative_than(&previous.value) {
                return Ok(());
            }
        }
        let sequence = previous
            .map(|value| value.sequence)
            .unwrap_or(0)
            .checked_add(1)
            .context("Progress sequence overflow")?;
        let progress = ExecutionProgress {
            value: progress,
            checkpoint,
            sequence,
        };
        let (session_id, tool_name, background): (String, String, bool) = transaction.query_row(
            "SELECT session_id,tool,background FROM runs WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ensure!(transaction.execute("UPDATE runs SET progress=?3,updated=unixepoch() WHERE id=?1 AND owner=?2 AND state='running'",params![id,owner,serde_json::to_string(&progress)?])?==1,"Progress lost execution ownership");
        transaction.commit()?;
        if background {
            crate::bus::Bus::global().publish(crate::bus::BusEvent::BackgroundTaskProgress(
                crate::bus::BackgroundTaskProgressEvent {
                    task_id: id.into(),
                    session_id,
                    tool_name,
                    display_name: None,
                    progress: progress.value,
                },
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};
    use jcode_background_types::{BackgroundTaskProgressKind, BackgroundTaskProgressSource};
    #[test]
    fn progress_uses_owned_monotonic_updates_and_existing_informativeness_rules() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let input = Invocation {
            session_id: "progress".into(),
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
        store.promote(&record.id, "owner")?;
        let mut events = crate::bus::Bus::global().subscribe();
        let progress = BackgroundTaskProgress {
            kind: BackgroundTaskProgressKind::Determinate,
            percent: Some(50.0),
            message: Some("reported".into()),
            current: None,
            total: None,
            unit: None,
            eta_seconds: None,
            updated_at: "first".into(),
            source: BackgroundTaskProgressSource::Reported,
        };
        store.record_progress(&record.id, "owner", progress.clone(), false)?;
        let mut repeated = progress.clone();
        repeated.updated_at = "second".into();
        store.record_progress(&record.id, "owner", repeated, false)?;
        assert_eq!(
            store
                .inspect(&record.id)?
                .unwrap()
                .progress
                .unwrap()
                .sequence,
            1
        );
        let mut less = progress.clone();
        less.percent = None;
        less.kind = BackgroundTaskProgressKind::Indeterminate;
        less.source = BackgroundTaskProgressSource::ParsedOutput;
        store.record_progress(&record.id, "owner", less, false)?;
        assert_eq!(
            store
                .inspect(&record.id)?
                .unwrap()
                .progress
                .unwrap()
                .sequence,
            1
        );
        store.record_progress(&record.id, "owner", progress.clone(), true)?;
        assert_eq!(
            store
                .inspect(&record.id)?
                .unwrap()
                .progress
                .unwrap()
                .sequence,
            2
        );
        assert!(
            store
                .record_progress(&record.id, "wrong-owner", progress, false)
                .is_err()
        );
        let mut count = 0;
        while let Ok(event) = events.try_recv() {
            if let crate::bus::BusEvent::BackgroundTaskProgress(event) = event
                && event.task_id == record.id
            {
                count += 1;
            }
        }
        assert_eq!(count, 2);
        Ok(())
    }
}
