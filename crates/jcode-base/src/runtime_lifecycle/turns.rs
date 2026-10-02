//! Durable admission records for primary turns. A record exists exactly while
//! one runtime incarnation owns an unsettled turn. A record that survives into
//! a later incarnation is the evidence of an interrupted turn; nothing is
//! inferred from session status, message shape or client presence.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRecord {
    pub schema: u32,
    pub session: String,
    pub turn: String,
    /// Runtime incarnation that admitted the turn.
    pub runtime: String,
    pub started_at: String,
}

#[derive(Clone, Debug)]
pub struct TurnJournal {
    directory: PathBuf,
    runtime: String,
}

fn validate_session(session: &str) -> Result<()> {
    ensure!(
        !session.is_empty()
            && session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "Invalid primary Session identity for turn journal"
    );
    Ok(())
}

impl TurnJournal {
    pub(super) fn new(namespace_directory: &Path, runtime: &str) -> Self {
        Self {
            directory: namespace_directory.join("turns"),
            runtime: runtime.into(),
        }
    }

    fn ensure_directory(&self) -> Result<()> {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.directory) {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Turn journal directory identity changed"
            );
        }
        crate::storage::ensure_dir(&self.directory)
    }

    fn path(&self, session: &str) -> PathBuf {
        self.directory.join(format!("{session}.json"))
    }

    /// Persist before the turn body can reach a provider. A failure here must
    /// reject admission: an unrecorded turn could not be recovered honestly.
    pub fn begin(&self, session: &str) -> Result<TurnRecord> {
        validate_session(session)?;
        self.ensure_directory()?;
        let path = self.path(session);
        if let Some(existing) = read(&path)? {
            // A prior settle in this same incarnation may have failed to remove
            // its record. Another incarnation's record must already have been
            // reconciled into a recovery item before admission opened.
            ensure!(
                existing.runtime == self.runtime,
                "Session {session} has an unreconciled interrupted turn"
            );
        }
        let record = TurnRecord {
            schema: SCHEMA,
            session: session.into(),
            turn: uuid::Uuid::new_v4().to_string(),
            runtime: self.runtime.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        crate::storage::write_json_secret(&path, &record)
            .with_context(|| format!("Record primary turn admission for {session}"))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(record)
    }

    /// Remove only the exact settled turn. A newer record is never touched.
    pub fn remove(&self, record: &TurnRecord) -> Result<()> {
        validate_session(&record.session)?;
        let path = self.path(&record.session);
        match read(&path)? {
            Some(current) if current.turn == record.turn => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("Settle turn record for {}", record.session))?;
                std::fs::File::open(&self.directory)?.sync_all()?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Records this incarnation retained for interrupted turns. Used when a
    /// planned replacement fails and this incarnation continues them itself.
    pub fn own_records(&self) -> Result<Vec<TurnRecord>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|record| record.runtime == self.runtime)
            .collect())
    }

    /// Records admitted by other incarnations. Unreadable records fail closed:
    /// startup must not silently discard interruption evidence.
    pub fn leftovers(&self) -> Result<Vec<TurnRecord>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|record| record.runtime != self.runtime)
            .collect())
    }

    fn all(&self) -> Result<Vec<TurnRecord>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("Inspect interrupted turn records"),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let record = read(&path)?.context("Turn record vanished during inspection")?;
            ensure!(
                record.schema == SCHEMA
                    && path.file_stem().and_then(|stem| stem.to_str()) == Some(&record.session),
                "Turn record {} is inconsistent",
                path.display()
            );
            records.push(record);
        }
        records.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        Ok(records)
    }
}

fn read(path: &Path) -> Result<Option<TurnRecord>> {
    match super::open_file(path, false, false) {
        Ok(file) => Ok(Some(serde_json::from_reader(file).with_context(|| {
            format!("Turn record {} is damaged", path.display())
        })?)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_survive_only_until_their_exact_turn_settles() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = TurnJournal::new(root.path(), "runtime-a");
        let record = first.begin("session_a")?;
        assert!(
            first.leftovers()?.is_empty(),
            "own records are not leftovers"
        );
        let next = TurnJournal::new(root.path(), "runtime-b");
        assert_eq!(next.leftovers()?, vec![record.clone()]);
        assert!(
            next.begin("session_a").is_err(),
            "an unreconciled turn blocks a new admission"
        );
        let mut stale = record.clone();
        stale.turn = "older".into();
        first.remove(&stale)?;
        assert_eq!(next.leftovers()?.len(), 1, "only the exact turn is settled");
        first.remove(&record)?;
        assert!(next.leftovers()?.is_empty());
        Ok(())
    }

    #[test]
    fn damaged_or_foreign_records_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let journal = TurnJournal::new(root.path(), "runtime-a");
        journal.begin("session_a")?;
        std::fs::write(root.path().join("turns/session_b.json"), b"{not json")?;
        assert!(
            TurnJournal::new(root.path(), "runtime-b")
                .leftovers()
                .is_err()
        );
        assert!(journal.begin("../escape").is_err());
        Ok(())
    }
}
