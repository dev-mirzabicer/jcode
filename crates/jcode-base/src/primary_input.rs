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
use std::io::{Read, Write};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancelled_queue: Option<Vec<crate::todo::QueuedMessage>>,
}

pub struct PrimaryInputStore {
    root: PathBuf,
}
pub struct ClientInputCancellation {
    pub input: PrimaryInputEnvelope,
    pub source_digest: String,
    pub queued_messages: Option<Vec<crate::todo::QueuedMessage>>,
}
pub struct InputLease {
    path: PathBuf,
    inbox: Inbox,
    file: File,
    initialized: bool,
}
const INPUT_INITIALIZED: &[u8] = b"jcode-primary-input-v1\n";

pub fn input_digest(input: &PrimaryInputEnvelope) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(input)?)))
}

/// Stable domain correlation for legacy transport scopes and producer-owned
/// execution IDs. Content is validated independently, never part of the ID.
pub fn correlated_input_id(producer: &str, identity: &str) -> RequestId {
    let mut hash = Sha256::new();
    hash.update(producer.as_bytes());
    hash.update([0]);
    hash.update(identity.as_bytes());
    let bytes = hash.finalize();
    uuid::Uuid::from_slice(&bytes[..16])
        .expect("sixteen digest bytes")
        .to_string()
        .parse()
        .expect("UUID input identity")
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

    /// Names only. Callers inspect each independently so a damaged inbox does
    /// not suppress delivery or diagnostics for unrelated primaries.
    pub fn sessions(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut sessions = std::collections::BTreeSet::new();
        for entry in entries {
            let path = entry?.path();
            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("json" | "lock")
            ) && let Some(name) = path.file_stem().and_then(|value| value.to_str())
            {
                sessions.insert(name.to_owned());
            }
        }
        Ok(sessions.into_iter().collect())
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
        let mut file = options.open(self.root.join(format!("{session}.lock")))?;
        ensure!(
            file.metadata()?.is_file(),
            "Primary input lease is not a regular file"
        );
        // This lease spans only synchronous persistence, never a provider or
        // async wait. An inspection racing admission must not strand delivery.
        file.lock().context("Acquire primary input admission")?;
        let mut marker = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(128)
            .read_to_end(&mut marker)?;
        ensure!(
            marker.is_empty() || marker == INPUT_INITIALIZED,
            "Primary input initialization record is damaged"
        );
        let initialized = !marker.is_empty();
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !initialized => Inbox {
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
            file,
            initialized,
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
            cancelled_queue: None,
        });
        lease.save()?;
        Ok(receipt)
    }

    pub fn accept_prepared(
        &self,
        session: &str,
        id: RequestId,
        source_digest: &str,
        prepare: impl FnOnce() -> Result<PrimaryInputEnvelope>,
    ) -> Result<PrimaryInputEnvelope> {
        let mut lease = self.lock(session)?;
        if let Some(record) = lease
            .inbox
            .records
            .iter()
            .find(|record| record.input.id == id)
        {
            ensure!(
                record.input.client_request_digest.as_deref() == Some(source_digest),
                "Primary input ID already has different client intent"
            );
            return Ok(record.input.clone());
        }
        let mut input = prepare()?;
        ensure!(
            input.id == id && input.session == session,
            "Prepared input identity changed"
        );
        input.client_request_digest = Some(source_digest.into());
        let receipt = PrimaryInputReceipt {
            id,
            session: session.into(),
            state: PrimaryInputState::Accepted,
            messages: Vec::new(),
            issue: None,
        };
        lease.inbox.records.push(Record {
            input: input.clone(),
            receipt,
            cancelled_queue: None,
        });
        lease.save()?;
        Ok(input)
    }

    pub fn inspect(&self, session: &str, id: RequestId) -> Result<PrimaryInputReceipt> {
        let mut lease = self.lock(session)?;
        let source = crate::session::Session::load_startup_stub(session)?;
        lease.reconcile(&source)?;
        Ok(lease.record(id)?.receipt.clone())
    }

    pub fn original(&self, session: &str, id: RequestId) -> Result<Option<PrimaryInputEnvelope>> {
        let lease = self.lock(session)?;
        Ok(lease
            .inbox
            .records
            .iter()
            .find(|record| record.input.id == id)
            .map(|record| record.input.clone()))
    }

    pub fn pending(&self, session: &str) -> Result<Vec<PrimaryInputEnvelope>> {
        if !self.root.join(format!("{session}.json")).try_exists()?
            && !self.root.join(format!("{session}.lock")).try_exists()?
        {
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

    pub fn cancel_pending_interrupts(&self, session: &str) -> Result<()> {
        let mut lease = self.lock(session)?;
        lease.reconcile(&crate::session::Session::load_startup_stub(session)?)?;
        for record in &mut lease.inbox.records {
            if record.input.delivery == PrimaryInputDelivery::SafeBoundary
                && record.receipt.state == PrimaryInputState::Accepted
            {
                record.receipt.state = PrimaryInputState::Cancelled;
            }
        }
        lease.save()
    }

    /// A cancellation received before the original transport creates a
    /// tombstone atomically, never a temporarily runnable accepted input.
    pub fn cancel_client_inputs(
        &self,
        session: &str,
        inputs: Vec<ClientInputCancellation>,
    ) -> Result<Vec<PrimaryInputReceipt>> {
        let mut lease = self.lock(session)?;
        lease.reconcile(&crate::session::Session::load_startup_stub(session)?)?;
        for ClientInputCancellation {
            input,
            source_digest: digest,
            ..
        } in &inputs
        {
            ensure!(
                input.session == session && input.delivery != PrimaryInputDelivery::ContextOnly,
                "Cancellation is not scoped to pending conversational input"
            );
            if let Some(record) = lease
                .inbox
                .records
                .iter()
                .find(|record| record.input.id == input.id)
            {
                ensure!(
                    record.input.client_request_digest.as_deref() == Some(digest),
                    "Cancellation refers to different client intent"
                );
            }
        }
        let mut receipts = Vec::new();
        for ClientInputCancellation {
            mut input,
            source_digest: digest,
            queued_messages: queue,
        } in inputs
        {
            if let Some(record) = lease
                .inbox
                .records
                .iter_mut()
                .find(|record| record.input.id == input.id)
            {
                if record.receipt.state == PrimaryInputState::Accepted {
                    record.receipt.state = PrimaryInputState::Cancelled;
                }
                receipts.push(record.receipt.clone());
            } else {
                input.client_request_digest = Some(digest);
                let receipt = PrimaryInputReceipt {
                    id: input.id,
                    session: session.into(),
                    state: PrimaryInputState::Cancelled,
                    messages: Vec::new(),
                    issue: None,
                };
                receipts.push(receipt.clone());
                lease.inbox.records.push(Record {
                    input,
                    receipt,
                    cancelled_queue: queue,
                });
            }
        }
        lease.save()?;
        Ok(receipts)
    }
}

impl InputLease {
    fn save(&mut self) -> Result<()> {
        crate::storage::write_json_secret(&self.path, &self.inbox)?;
        if !self.initialized {
            self.file.write_all(INPUT_INITIALIZED)?;
            self.file.sync_all()?;
            self.initialized = true;
        }
        Ok(())
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
            urgent: false,
            activate_skill: None,
            observe_startup_context: None,
            client_request_digest: None,
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
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.root.join(format!("{}.lock", session.id)))?;
        assert!(contender.try_lock().is_err());
        assert!(lease.cancel(&session, input.id).is_err());
        assert_eq!(
            lease.cancel(&session, pending.id)?.state,
            PrimaryInputState::Cancelled
        );
        drop(lease);
        assert!(store.pending(&session.id)?.is_empty());
        assert!(store.lock("../foreign").is_err());
        let mut prepared_input = input.clone();
        prepared_input.id = RequestId::new();
        let prepared = store.accept_prepared(&session.id, prepared_input.id, "source-a", || {
            Ok(prepared_input.clone())
        })?;
        assert_eq!(
            store.accept_prepared(&session.id, prepared_input.id, "source-a", || panic!(
                "replay must not render again"
            ))?,
            prepared
        );
        assert!(
            store
                .accept_prepared(&session.id, prepared_input.id, "source-b", || panic!(
                    "conflict must not render"
                ))
                .is_err()
        );
        let mut early = input.clone();
        early.id = RequestId::new();
        let receipts = store.cancel_client_inputs(
            &session.id,
            vec![
                ClientInputCancellation {
                    input: prepared_input,
                    source_digest: "source-a".into(),
                    queued_messages: None,
                },
                ClientInputCancellation {
                    input: early.clone(),
                    source_digest: "source-early".into(),
                    queued_messages: Some(vec!["queued before transport".into()]),
                },
            ],
        )?;
        assert!(
            receipts
                .iter()
                .all(|receipt| receipt.state == PrimaryInputState::Cancelled)
        );
        store.accept_prepared(&session.id, early.id, "source-early", || {
            panic!("cancelled transport must not render or run")
        })?;
        assert!(store.pending(&session.id)?.is_empty());
        assert_eq!(
            Session::load(&session.id)?.messages.len(),
            session.messages.len()
        );
        std::fs::write(store.root.join(format!("{}.json", session.id)), "corrupt")?;
        assert!(store.pending(&session.id).is_err());
        Ok(())
    }
}
