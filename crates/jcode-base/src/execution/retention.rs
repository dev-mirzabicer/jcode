//! Quiet maintenance over the execution store. No scheduling/model ownership.
use super::{ExecutionStore, StorageConfig};
use anyhow::{Context, Result, ensure};
pub use jcode_tool_types::cleanup::{RetentionIssue, RetentionReport};
use std::fs::OpenOptions;

impl ExecutionStore {
    pub fn maintain_retention(&self, config: &StorageConfig, now: i64) -> Result<RetentionReport> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lease = options.open(self.root().join("retention.lock"))?;
        ensure!(
            lease.metadata()?.is_file(),
            "Retention ownership file changed type"
        );
        match lease.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return self
                    .retention_status()
                    .map(|report| report.unwrap_or_default());
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut report = RetentionReport {
            checked_at: now,
            ..Default::default()
        };
        if let Err(error) = self.reconcile_activity_leases(now) {
            report.issues.push(RetentionIssue {
                id: "activity".into(),
                message: format!("{error:#}"),
            });
        }
        match self.resume_confirmed_output_cleanup(now) {
            Ok(outcomes) => {
                report.resumed_cleanups = outcomes.len();
                for outcome in outcomes {
                    for item in outcome.items {
                        if let Some(message) = item.error {
                            report.issues.push(RetentionIssue {
                                id: item.run_id,
                                message,
                            });
                        }
                    }
                }
            }
            Err(error) => report.issues.push(RetentionIssue {
                id: "cleanup-recovery".into(),
                message: format!("{error:#}"),
            }),
        }
        if config.archive.is_some() {
            let connection = self.connection()?;
            let mut query=connection.prepare("SELECT r.id FROM runs r JOIN output_locations l ON l.id=r.id JOIN session_activity a ON a.session_id=r.session_id WHERE r.state NOT IN ('prepared','running') AND a.last_active<=?1 AND a.retention_not_before<=?2 AND (l.archived=0 OR l.cold_archived_at IS NULL OR EXISTS(SELECT 1 FROM relocations m WHERE m.id=r.id AND m.stage<>'complete')) AND NOT EXISTS(SELECT 1 FROM output_deletions d WHERE d.id=r.id) ORDER BY r.id")?;
            let ids = query
                .query_map(
                    rusqlite::params![now.saturating_sub(super::IDLE_SECONDS), now],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let archive_ready = if ids.is_empty() {
                true
            } else {
                match self.verify_retention_archive(config) {
                    Ok(()) => true,
                    Err(error) => {
                        report.issues.push(RetentionIssue {
                            id: "archive".into(),
                            message: format!("Routine archival deferred: {error:#}"),
                        });
                        false
                    }
                }
            };
            for id in ids.into_iter().filter(|_| archive_ready) {
                match self.archive_cold_output(&id, config, now) {
                    Ok(true) => report.archived_outputs += 1,
                    Ok(false) => {}
                    Err(error) => {
                        report.issues.push(RetentionIssue {
                            id,
                            message: format!("{error:#}"),
                        });
                        if let Err(error) = self.verify_retention_archive(config) {
                            report.issues.push(RetentionIssue {
                                id: "archive".into(),
                                message: format!("Remaining routine archival deferred: {error:#}"),
                            });
                            break;
                        }
                    }
                }
            }
        }
        match self.prune_inspection_snapshots(now) {
            Ok(outcome) => {
                report.pruned_snapshots = outcome.pruned;
                report.issues.extend(outcome.errors);
            }
            Err(error) => report.issues.push(RetentionIssue {
                id: "snapshot-pruning".into(),
                message: format!("{error:#}"),
            }),
        }
        crate::storage::write_json_secret(&self.root().join("retention-status.json"), &report)?;
        Ok(report)
    }

    /// Metadata-only human status. This never updates the activity clock.
    pub fn retention_status(&self) -> Result<Option<RetentionReport>> {
        let path = self.root().join("retention-status.json");
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "Retention status changed type"
        );
        Ok(Some(
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path)?))
                .context("Read retention status")?,
        ))
    }
}
