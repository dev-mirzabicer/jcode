//! Durable primary input admission. Original accepted content is retained until
//! explicit lifecycle cleanup. Session receipts, not queue removal or transport
//! acknowledgements, establish model-visible transcript commitment.
use anyhow::{Context, Result, ensure};
pub use jcode_session_types::{
    PrimaryInputDelivery, PrimaryInputEnvelope, PrimaryInputReceipt, PrimaryInputState,
    StoredPrimaryInputReceipt,
};
use jcode_workspace_types::RequestId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inbox {
    version: u32,
    session: String,
    records: Vec<Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    input: PrimaryInputEnvelope,
    receipt: PrimaryInputReceipt,
}

pub struct PrimaryInputStore {
    root: PathBuf,
}
pub struct InputLease {
    path: PathBuf,
    inbox: Inbox,
    _file: File,
}

pub fn input_digest(input: &PrimaryInputEnvelope) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(input)?)))
}

impl PrimaryInputStore {
    pub fn new(state_root: &Path) -> Self {
        Self {
            root: state_root.join("primary-inputs"),
        }
    }
    pub fn current() -> Self {
        Self::new(&crate::storage::durable_state_dir())
    }

    pub fn lock(&self, session: &str) -> Result<InputLease> {
        ensure!(
            !session.is_empty()
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid primary input Session identity"
        );
        crate::storage::ensure_dir(&self.root)?;
        let metadata = std::fs::symlink_metadata(&self.root)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Primary input store is not a private directory"
        );
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(self.root.join(format!("{session}.lock")))?;
        ensure!(
            file.metadata()?.is_file(),
            "Primary input lease is not a regular file"
        );
        file.try_lock().context("Primary input admission is busy")?;
        let path = self.root.join(format!("{session}.json"));
        let inbox = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Primary input journal is not a regular private file"
                );
                // Generic read_json may recover an older .bak. That could
                // resurrect cancelled input or forget accepted identities.
                serde_json::from_slice::<Inbox>(&std::fs::read(&path)?)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Inbox {
                version: 1,
                session: session.into(),
                records: Vec::new(),
            },
            Err(error) => return Err(error.into()),
        };
        ensure!(
            inbox.version == 1 && inbox.session == session,
            "Primary input journal identity/schema mismatch"
        );
        let mut seen = std::collections::HashSet::new();
        for record in &inbox.records {
            ensure!(
                record.input.session == session
                    && record.receipt.session == session
                    && record.receipt.id == record.input.id
                    && seen.insert(record.input.id),
                "Primary input journal contains inconsistent identities"
            );
        }
        Ok(InputLease {
            path,
            inbox,
            _file: file,
        })
    }

    pub fn accept(&self, input: PrimaryInputEnvelope) -> Result<PrimaryInputReceipt> {
        let mut lease = self.lock(&input.session)?;
        if let Some(record) = lease.inbox.records.iter().find(|r| r.input.id == input.id) {
            ensure!(
                record.input == input,
                "Primary input ID already has different original content or delivery policy"
            );
            return Ok(record.receipt.clone());
        }
        let receipt = PrimaryInputReceipt {
            id: input.id,
            session: input.session.clone(),
            state: PrimaryInputState::Accepted,
            messages: Vec::new(),
            issue: None,
        };
        lease.inbox.records.push(Record {
            input,
            receipt: receipt.clone(),
        });
        lease.save()?;
        Ok(receipt)
    }

    pub fn inspect(&self, session: &str, id: RequestId) -> Result<PrimaryInputReceipt> {
        let mut lease = self.lock(session)?;
        let source = crate::session::Session::load_startup_stub(session)?;
        lease.reconcile(&source)?;
        Ok(lease.record(id)?.receipt.clone())
    }

    pub fn pending(&self, session: &str) -> Result<Vec<PrimaryInputEnvelope>> {
        if !self.root.join(format!("{session}.json")).try_exists()? {
            return Ok(Vec::new());
        }
        let mut lease = self.lock(session)?;
        let source = crate::session::Session::load_startup_stub(session)?;
        lease.reconcile(&source)?;
        Ok(lease
            .inbox
            .records
            .iter()
            .filter(|r| r.receipt.state == PrimaryInputState::Accepted)
            .map(|r| r.input.clone())
            .collect())
    }

    pub fn fail(&self, session: &str, id: RequestId, detail: String) -> Result<()> {
        let mut lease = self.lock(session)?;
        lease.reconcile(&crate::session::Session::load_startup_stub(session)?)?;
        let record = lease.record_mut(id)?;
        // A later dispatch failure does not make a committed input uncommitted.
        if record.receipt.state == PrimaryInputState::Accepted {
            record.receipt.state = PrimaryInputState::Failed;
        }
        record.receipt.issue = Some(detail);
        lease.save()
    }
}

impl InputLease {
    fn save(&self) -> Result<()> {
        crate::storage::write_json_secret(&self.path, &self.inbox)
    }
    fn record(&self, id: RequestId) -> Result<&Record> {
        self.inbox
            .records
            .iter()
            .find(|r| r.input.id == id)
            .context("Unknown primary input")
    }
    fn record_mut(&mut self, id: RequestId) -> Result<&mut Record> {
        self.inbox
            .records
            .iter_mut()
            .find(|r| r.input.id == id)
            .context("Unknown primary input")
    }
    pub fn reconcile(&mut self, session: &crate::session::Session) -> Result<()> {
        ensure!(
            session.id == self.inbox.session,
            "Input reconciliation belongs to another Session"
        );
        let mut changed = false;
        for record in &mut self.inbox.records {
            if let Some(committed) = session
                .primary_inputs
                .iter()
                .find(|r| r.id == record.input.id)
            {
                ensure!(
                    committed.digest == input_digest(&record.input)?,
                    "Committed input digest differs from accepted input"
                );
                if record.receipt.state != PrimaryInputState::Committed
                    || record.receipt.messages != committed.messages
                {
                    record.receipt.state = PrimaryInputState::Committed;
                    record.receipt.messages = committed.messages.clone();
                    changed = true;
                }
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }
    pub fn cancel(
        &mut self,
        session: &crate::session::Session,
        id: RequestId,
    ) -> Result<PrimaryInputReceipt> {
        self.reconcile(session)?;
        let record = self.record_mut(id)?;
        ensure!(
            matches!(
                record.receipt.state,
                PrimaryInputState::Accepted | PrimaryInputState::Cancelled
            ),
            "Input is no longer pending"
        );
        record.receipt.state = PrimaryInputState::Cancelled;
        let receipt = record.receipt.clone();
        self.save()?;
        Ok(receipt)
    }
    pub fn require_pending(&self, input: &PrimaryInputEnvelope) -> Result<()> {
        let record = self.record(input.id)?;
        ensure!(
            record.input == *input && record.receipt.state == PrimaryInputState::Accepted,
            "Primary input is not pending with these exact contents"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    #[test]
    fn primary_input_reconciles_committed_session_without_reappending_or_losing_original()
    -> Result<()> {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::tempdir()?;
        struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Env {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => crate::env::set_var(key, value),
                        None => crate::env::remove_var(key),
                    }
                }
            }
        }
        let _env = Env(vec![
            ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
            ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
        ]);
        crate::env::set_var("JCODE_HOME", temp.path().join("home"));
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path().join("runtime"));
        let mut session = Session::create(None, None);
        session.save()?;
        let store = PrimaryInputStore::current();
        let input = PrimaryInputEnvelope {
            id: RequestId::new(),
            session: session.id.clone(),
            delivery: PrimaryInputDelivery::SafeBoundary,
            content: "original unicode 🪴 input".into(),
            images: vec![("image/png".into(), "synthetic".into())],
            display_role: None,
            origin: Some(jcode_session_types::StoredMessageOrigin::Human),
            system_reminder: None,
            unattended_context: None,
        };
        let accepted = store.accept(input.clone())?;
        assert_eq!(accepted.state, PrimaryInputState::Accepted);
        assert_eq!(store.accept(input.clone())?, accepted);
        let mut conflict = input.clone();
        conflict.content.push('!');
        assert!(store.accept(conflict).is_err());
        assert_eq!(store.pending(&session.id)?, vec![input.clone()]);
        // Crash before Session checkpoint leaves the complete original pending.
        let start = session.messages.len();
        session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: input.content.clone(),
                cache_control: None,
            }],
        );
        session.record_primary_input(&input, start)?;
        assert_eq!(
            store.inspect(&session.id, input.id)?.state,
            PrimaryInputState::Accepted
        );
        session.save()?;
        // Crash after Session checkpoint and before inbox acknowledgement is
        // repaired from its structural receipt, not a string search.
        let committed = store.inspect(&session.id, input.id)?;
        assert_eq!(committed.state, PrimaryInputState::Committed);
        assert_eq!(committed.messages, vec![session.messages[start].id.clone()]);
        assert!(store.pending(&session.id)?.is_empty());
        assert_eq!(store.accept(input.clone())?, committed);
        assert_eq!(
            Session::load_startup_stub(&session.id)?.primary_inputs,
            session.primary_inputs
        );
        let mut split = Session::create(Some(session.id.clone()), None);
        split.inherit_continuation_state_from(&session);
        assert!(split.primary_inputs.is_empty());
        assert_eq!(split.messages.len(), session.messages.len());
        let mut pending = input.clone();
        pending.id = RequestId::new();
        store.accept(pending.clone())?;
        let mut lease = store.lock(&session.id)?;
        assert!(store.lock(&session.id).is_err());
        assert!(lease.cancel(&session, input.id).is_err());
        assert_eq!(
            lease.cancel(&session, pending.id)?.state,
            PrimaryInputState::Cancelled
        );
        drop(lease);
        assert!(store.pending(&session.id)?.is_empty());
        assert!(store.lock("../foreign").is_err());
        std::fs::write(store.root.join(format!("{}.json", session.id)), "corrupt")?;
        assert!(store.pending(&session.id).is_err());
        Ok(())
    }
}
