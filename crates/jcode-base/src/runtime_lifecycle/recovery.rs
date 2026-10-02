//! Durable recovery items for primary turns interrupted without a verified
//! outcome. The live coordinator presents them; a trusted client decides each
//! once. Items never authorize inference by themselves.
use super::turns::TurnRecord;
use anyhow::{Context, Result, ensure};
use jcode_workspace_types::runtime::*;
use jcode_workspace_types::{RequestId, Revision};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SCHEMA: u32 = 1;
/// Resolved history kept for inspection; unresolved items are never pruned.
const RESOLVED_RETENTION: usize = 200;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    items: Vec<RecoveryItem>,
}

#[derive(Clone, Debug)]
pub struct RecoveryStore {
    directory: PathBuf,
}

struct Transaction {
    store: RecoveryStore,
    journal: Journal,
    _lease: std::fs::File,
}

impl RecoveryStore {
    pub(super) fn new(namespace_directory: &Path) -> Self {
        Self {
            directory: namespace_directory.to_path_buf(),
        }
    }

    fn transaction(&self) -> Result<Transaction> {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.directory) {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Runtime recovery directory identity changed"
            );
        }
        crate::storage::ensure_dir(&self.directory)?;
        let lease = super::open_file(&self.directory.join("recovery.lock"), true, true)?;
        lease.lock().context("Lock runtime recovery journal")?;
        let journal = match super::open_file(&self.directory.join("recovery.json"), false, false) {
            Ok(file) => serde_json::from_reader::<_, Journal>(file)
                .context("Runtime recovery journal is damaged; no empty fallback was adopted")?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Journal {
                    schema: SCHEMA,
                    items: Vec::new(),
                }
            }
            Err(error) => return Err(error.context("Runtime recovery journal is unavailable")),
        };
        ensure!(
            journal.schema == SCHEMA,
            "Unknown runtime recovery journal schema"
        );
        Ok(Transaction {
            store: self.clone(),
            journal,
            _lease: lease,
        })
    }

    /// Unresolved items first (oldest first), then resolved history.
    pub fn list(&self) -> Result<Vec<RecoveryItem>> {
        if !self.directory.join("recovery.json").exists() {
            return Ok(Vec::new());
        }
        let mut items = self.transaction()?.journal.items;
        items.sort_by(|a, b| {
            (a.resolved.is_some(), &a.detected_at).cmp(&(b.resolved.is_some(), &b.detected_at))
        });
        Ok(items)
    }

    pub fn unresolved(&self, session: &str) -> Result<Vec<RecoveryItem>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|item| item.session == session && item.resolved.is_none())
            .collect())
    }

    pub fn inspect(&self, id: RecoveryId) -> Result<RecoveryItem> {
        self.transaction()?
            .journal
            .items
            .into_iter()
            .find(|item| item.id == id)
            .context("Unknown runtime recovery item")
    }

    /// Record interrupted turns once each, keyed by their durable turn identity.
    pub fn adopt(
        &self,
        records: &[TurnRecord],
        cause: impl Fn(&TurnRecord) -> RecoveryCause,
    ) -> Result<Vec<RecoveryItem>> {
        let mut tx = self.transaction()?;
        let mut adopted = Vec::new();
        for record in records {
            if let Some(existing) = tx
                .journal
                .items
                .iter()
                .find(|item| item.turn == record.turn)
            {
                adopted.push(existing.clone());
                continue;
            }
            let item = RecoveryItem {
                id: RecoveryId::new(),
                session: record.session.clone(),
                turn: record.turn.clone(),
                runtime: record.runtime.clone(),
                cause: cause(record),
                detected_at: chrono::Utc::now().to_rfc3339(),
                revision: 1,
                resolved: None,
                executions: Vec::new(),
            };
            tx.journal.items.push(item.clone());
            adopted.push(item);
        }
        tx.commit()?;
        Ok(adopted)
    }

    /// A trusted-client decision. The same request replays its original
    /// outcome; any other change to a resolved or newer item is a conflict.
    pub fn resolve(
        &self,
        id: RecoveryId,
        expected: Revision,
        request: RequestId,
        resolution: RecoveryResolution,
    ) -> Result<RecoveryItem> {
        let mut tx = self.transaction()?;
        let item = tx
            .journal
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .context("Unknown runtime recovery item")?;
        if let Some(resolved) = &item.resolved {
            ensure!(
                resolved.request == Some(request),
                "Recovery item was already resolved by another decision"
            );
            return Ok(item.clone());
        }
        ensure!(
            item.revision == expected,
            "Recovery item revision changed; inspect it again"
        );
        item.resolved = Some(RecoveryResolved {
            resolution,
            request: Some(request),
            resolved_at: chrono::Utc::now().to_rfc3339(),
        });
        item.revision += 1;
        let result = item.clone();
        tx.commit()?;
        Ok(result)
    }

    /// A human message to the session resolves every unresolved interruption,
    /// so a later decision cannot inject a stale continuation.
    pub fn supersede(&self, session: &str, input: RequestId) -> Result<Vec<RecoveryItem>> {
        let mut tx = self.transaction()?;
        let mut changed = Vec::new();
        for item in tx
            .journal
            .items
            .iter_mut()
            .filter(|item| item.session == session && item.resolved.is_none())
        {
            item.resolved = Some(RecoveryResolved {
                resolution: RecoveryResolution::SupersededByInput { input },
                request: None,
                resolved_at: chrono::Utc::now().to_rfc3339(),
            });
            item.revision += 1;
            changed.push(item.clone());
        }
        if !changed.is_empty() {
            tx.commit()?;
        }
        Ok(changed)
    }

    /// Continue decisions whose continuation input may not yet be accepted.
    /// Input acceptance is idempotent by ID, so startup can repeat delivery.
    pub fn continued(&self) -> Result<Vec<(RecoveryItem, RequestId)>> {
        Ok(self
            .list()?
            .into_iter()
            .filter_map(|item| match item.resolved.as_ref().map(|r| &r.resolution) {
                Some(RecoveryResolution::Continued { input }) => {
                    let input = *input;
                    Some((item, input))
                }
                _ => None,
            })
            .collect())
    }
}

impl Transaction {
    fn commit(&mut self) -> Result<()> {
        let resolved = self
            .journal
            .items
            .iter()
            .filter(|item| item.resolved.is_some())
            .count();
        if resolved > RESOLVED_RETENTION {
            let mut excess = resolved - RESOLVED_RETENTION;
            self.journal
                .items
                .sort_by(|a, b| a.detected_at.cmp(&b.detected_at));
            self.journal.items.retain(|item| {
                if excess > 0 && item.resolved.is_some() {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
        }
        crate::storage::write_json_secret(
            &self.store.directory.join("recovery.json"),
            &self.journal,
        )?;
        std::fs::File::open(&self.store.directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(session: &str, turn: &str) -> TurnRecord {
        TurnRecord {
            schema: 1,
            session: session.into(),
            turn: turn.into(),
            runtime: "old".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn adoption_is_idempotent_and_decisions_resolve_exactly_once() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = RecoveryStore::new(root.path());
        let records = [record("session_a", "t1")];
        let first = store.adopt(&records, |_| RecoveryCause::UnexpectedExit)?;
        let again = store.adopt(&records, |_| RecoveryCause::UnexpectedExit)?;
        assert_eq!(first, again, "re-running reconciliation adds nothing");
        let item = &first[0];
        let request = RequestId::new();
        let input = RequestId::new();
        let resolved = store.resolve(
            item.id,
            item.revision,
            request,
            RecoveryResolution::Continued { input },
        )?;
        assert_eq!(
            store.resolve(
                item.id,
                item.revision,
                request,
                RecoveryResolution::Continued { input }
            )?,
            resolved,
            "same request replays"
        );
        assert!(
            store
                .resolve(
                    item.id,
                    item.revision,
                    RequestId::new(),
                    RecoveryResolution::LeftStopped {}
                )
                .is_err(),
            "a second client cannot resolve it again"
        );
        assert_eq!(store.continued()?.len(), 1);
        assert!(store.unresolved("session_a")?.is_empty());
        Ok(())
    }

    #[test]
    fn human_input_supersedes_and_stale_revision_conflicts() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = RecoveryStore::new(root.path());
        let item =
            store.adopt(&[record("session_b", "t2")], |_| RecoveryCause::ForcedExit)?[0].clone();
        let input = RequestId::new();
        let changed = store.supersede("session_b", input)?;
        assert_eq!(changed.len(), 1);
        assert!(matches!(
            changed[0].resolved.as_ref().unwrap().resolution,
            RecoveryResolution::SupersededByInput { .. }
        ));
        assert!(
            store
                .resolve(
                    item.id,
                    item.revision,
                    RequestId::new(),
                    RecoveryResolution::LeftStopped {}
                )
                .is_err()
        );
        std::fs::write(root.path().join("recovery.json"), b"{")?;
        assert!(store.list().is_err(), "damage never becomes an empty list");
        Ok(())
    }
}
