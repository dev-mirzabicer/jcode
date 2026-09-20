//! Client send intent is durable before transport. Each input has its own file,
//! so two attached clients never overwrite each other's pending payloads.
use crate::primary_input::{PrimaryInputReceipt, PrimaryInputState};
use crate::protocol::PrimaryClientInput;
use crate::workspace::RequestId;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    owner: String,
    sequence: u64,
    request: PrimaryClientInput,
    receipt: Option<PrimaryInputReceipt>,
    terminal: bool,
    rejection: Option<String>,
    #[serde(default)]
    cancel_requested: bool,
}

pub(super) struct ClientInputs {
    root: PathBuf,
    owner: String,
    _owner: File,
    pub active: Option<RequestId>,
    pub last_submission: Option<RequestId>,
    pub requests: std::collections::HashMap<u64, (String, RequestId)>,
    retry: Option<PrimaryClientInput>,
    pub cancelled_replies: std::collections::HashSet<u64>,
}

fn private_dir(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "Invalid client input directory"
        );
    }
    crate::storage::ensure_dir(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Invalid client input directory"
    );
    Ok(())
}
fn open_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Invalid client input coordination file"
    );
    Ok(file)
}
impl ClientInputs {
    pub fn new(endpoint: &Path, owner: Option<&str>) -> Result<Self> {
        let endpoint = endpoint.to_str().context("Unsupported non-UTF8 endpoint")?;
        let root = crate::storage::jcode_dir()?.join("client-primary-inputs");
        private_dir(&root)?;
        let root = root.join(format!("{:x}", Sha256::digest(endpoint.as_bytes())));
        private_dir(&root)?;
        let owner = owner
            .map(str::to_owned)
            .unwrap_or_else(|| crate::id::new_id("client"));
        ensure!(
            owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid client identity"
        );
        let owner_file = open_file(&root.join(format!("owner-{owner}.lock")))?;
        owner_file
            .try_lock()
            .context("Client input identity already has a live owner")?;
        Ok(Self {
            root,
            owner,
            _owner: owner_file,
            active: None,
            last_submission: None,
            requests: Default::default(),
            retry: None,
            cancelled_replies: Default::default(),
        })
    }
    fn directory(&self, session: &str) -> Result<PathBuf> {
        ensure!(
            !session.is_empty()
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid input Session"
        );
        let directory = self.root.join(session);
        private_dir(&directory)?;
        Ok(directory)
    }
    fn lease(&self, session: &str) -> Result<(PathBuf, File)> {
        let directory = self.directory(session)?;
        let file = open_file(&directory.join(".lock"))?;
        file.lock()?;
        Ok((directory, file))
    }
    fn read(path: &Path) -> Result<Record> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Client input is not a regular file"
        );
        let record: Record = serde_json::from_reader(file)?;
        ensure!(
            record.version == 1,
            "Unsupported client input journal version"
        );
        ensure!(
            path.file_stem().and_then(|p| p.to_str())
                == Some(record.request.input.id.to_string().as_str()),
            "Client input identity mismatch"
        );
        ensure!(
            !record.owner.is_empty()
                && record
                    .owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid client input owner"
        );
        ensure!(record.sequence > 0, "Invalid client input sequence");
        Ok(record)
    }
    fn save(path: &Path, record: &Record) -> Result<()> {
        crate::storage::write_json_secret(path, record)
    }
    pub fn stage(
        &mut self,
        mut request: PrimaryClientInput,
        transport: u64,
    ) -> Result<PrimaryClientInput> {
        if let Some(mut retry) = self.retry.take() {
            ensure!(
                retry.input.session == request.input.session,
                "Retry belongs to another Session"
            );
            retry.retry_of = Some(retry.input.id);
            retry.input.id = request.input.id;
            retry.input.observe_startup_context = Some(false);
            retry.retry_attempts = retry.retry_attempts.saturating_add(1);
            retry.auto_retry = true;
            request = retry;
        }
        let (directory, _sequence) = self.lease(&request.input.session)?;
        let path = directory.join(format!("{}.json", request.input.id));
        ensure!(!path.try_exists()?, "Client input identity already exists");
        let sequence_path = directory.join(".next");
        let current = if sequence_path.try_exists()? {
            ensure!(
                !std::fs::symlink_metadata(&sequence_path)?
                    .file_type()
                    .is_symlink(),
                "Invalid client input sequence"
            );
            serde_json::from_slice::<u64>(&std::fs::read(&sequence_path)?)
                .context("Corrupt client input sequence")?
        } else {
            ensure!(
                !std::fs::read_dir(&directory)?.any(|entry| entry.is_ok_and(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json"))),
                "Client input sequence disappeared; retain existing records for repair"
            );
            0
        };
        let next = current
            .checked_add(1)
            .context("Client input sequence exhausted")?;
        crate::storage::write_json_secret(&sequence_path, &next)?;
        Self::save(
            &path,
            &Record {
                version: 1,
                owner: self.owner.clone(),
                sequence: next,
                request: request.clone(),
                receipt: None,
                terminal: false,
                rejection: None,
                cancel_requested: false,
            },
        )?;
        self.last_submission = Some(request.input.id);
        self.requests
            .insert(transport, (request.input.session.clone(), request.input.id));
        Ok(request)
    }
    pub fn recover(&self, session: &str) -> Result<Vec<PrimaryClientInput>> {
        let (directory, _lease) = self.lease(session)?;
        let mut pending = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let mut record = Self::read(&path)?;
            ensure!(
                record.request.input.session == session,
                "Client input belongs to another Session"
            );
            if record.terminal || record.rejection.is_some() {
                continue;
            }
            if record.owner != self.owner {
                let owner = open_file(&self.root.join(format!("owner-{}.lock", record.owner)))?;
                match owner.try_lock() {
                    Ok(()) => {}
                    Err(std::fs::TryLockError::WouldBlock) => continue,
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
                record.owner = self.owner.clone();
                Self::save(&path, &record)?;
            }
            if record.receipt.is_none() && !record.cancel_requested {
                pending.push((record.sequence, record.request));
            }
        }
        pending.sort_by_key(|(sequence, _)| *sequence);
        Ok(pending.into_iter().map(|(_, request)| request).collect())
    }
    pub fn pending_inspections(&self, session: &str) -> Result<Vec<RequestId>> {
        let (directory, _lease) = self.lease(session)?;
        let mut inputs = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let record = Self::read(&path)?;
            if record.owner == self.owner
                && !record.terminal
                && !record.cancel_requested
                && record.receipt.is_some()
            {
                inputs.push(record.request.input.id);
            }
        }
        Ok(inputs)
    }

    pub fn cancellations(&self, session: &str, begin: bool) -> Result<Vec<PrimaryClientInput>> {
        let (directory, _lease) = self.lease(session)?;
        let mut requests = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let mut record = Self::read(&path)?;
            if record.owner != self.owner
                || record.terminal
                || record.request.input.delivery
                    == crate::primary_input::PrimaryInputDelivery::ContextOnly
                || self.active == Some(record.request.input.id)
            {
                continue;
            }
            if begin {
                record.cancel_requested = true;
                Self::save(&path, &record)?;
            }
            if record.cancel_requested {
                requests.push(record.request);
            }
        }
        Ok(requests)
    }

    pub fn request(&self, session: &str, id: RequestId) -> Result<Option<PrimaryClientInput>> {
        let (directory, _lease) = self.lease(session)?;
        let path = directory.join(format!("{id}.json"));
        if !path.try_exists()? {
            return Ok(None);
        }
        let record = Self::read(&path)?;
        ensure!(
            record.request.input.session == session,
            "Client input target mismatch"
        );
        Ok((record.owner == self.owner).then_some(record.request))
    }
    pub fn receipt(&self, session: &str, id: RequestId) -> Result<Option<PrimaryInputReceipt>> {
        let (directory, _lease) = self.lease(session)?;
        let path = directory.join(format!("{id}.json"));
        if !path.try_exists()? {
            return Ok(None);
        }
        let record = Self::read(&path)?;
        ensure!(
            record.request.input.session == session,
            "Receipt target mismatch"
        );
        Ok(if record.owner == self.owner {
            record.receipt
        } else {
            None
        })
    }
    pub fn observe(&mut self, receipt: &PrimaryInputReceipt, terminal: bool) -> Result<()> {
        self.observe_receipt(receipt, terminal, false)
    }
    pub fn observe_cancellation(&mut self, receipt: &PrimaryInputReceipt) -> Result<()> {
        self.observe_receipt(receipt, false, true)
    }
    fn observe_receipt(
        &mut self,
        receipt: &PrimaryInputReceipt,
        terminal: bool,
        cancellation: bool,
    ) -> Result<()> {
        let (directory, _lease) = self.lease(&receipt.session)?;
        let path = directory.join(format!("{}.json", receipt.id));
        if !path.try_exists()? {
            return Ok(());
        }
        let mut record = Self::read(&path)?;
        ensure!(
            record.request.input.session == receipt.session,
            "Receipt target mismatch"
        );
        if record.owner != self.owner {
            return Ok(());
        }
        let update = terminal || !record.terminal;
        if cancellation {
            record.cancel_requested = false;
        }
        if update {
            if !matches!(
                record.receipt.as_ref().map(|r| r.state),
                Some(PrimaryInputState::Committed)
            ) || receipt.state != PrimaryInputState::Accepted
            {
                record.receipt = Some(receipt.clone());
            }
            record.terminal |= terminal
                || matches!(
                    receipt.state,
                    PrimaryInputState::Failed | PrimaryInputState::Cancelled
                );
        }
        if update || cancellation {
            Self::save(&path, &record)?;
        }
        Ok(())
    }
    pub fn reject(&self, session: &str, id: RequestId, issue: &str) -> Result<()> {
        let (directory, _lease) = self.lease(session)?;
        let path = directory.join(format!("{id}.json"));
        if !path.try_exists()? {
            return Ok(());
        }
        let mut record = Self::read(&path)?;
        if record.owner == self.owner {
            record.rejection = Some(issue.into());
            Self::save(&path, &record)?;
        }
        Ok(())
    }
    pub fn prepare_retry(&mut self, session: &str) -> Result<()> {
        let id = self
            .active
            .context("No correlated primary input to retry")?;
        let (directory, _lease) = self.lease(session)?;
        let path = directory.join(format!("{id}.json"));
        let mut record = Self::read(&path)?;
        if record.owner != self.owner {
            let owner = open_file(&self.root.join(format!("owner-{}.lock", record.owner)))?;
            owner
                .try_lock()
                .context("The original client still owns this retry")?;
            record.owner = self.owner.clone();
            Self::save(&path, &record)?;
        }
        ensure!(
            record.owner == self.owner
                && record.terminal
                && record
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.issue.is_some()),
            "Input outcome is uncertain; reconnect instead of creating a new attempt"
        );
        let mut request = record.request;
        if record
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.state == PrimaryInputState::Committed)
        {
            request.input.activate_skill = None;
        }
        self.retry = Some(request);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primary_input::{PrimaryInputDelivery, PrimaryInputEnvelope};
    fn input(session: &str, text: &str) -> PrimaryClientInput {
        let mut input =
            PrimaryInputEnvelope::new(session.into(), text.into(), PrimaryInputDelivery::NextTurn);
        input.images = vec![("image/png".into(), "ZmFrZQ==".into())];
        input.activate_skill = Some("synthetic-skill".into());
        input.observe_startup_context = Some(true);
        PrimaryClientInput {
            retry_of: None,
            input,
            queued_messages: None,
            is_system: false,
            retry_attempts: 0,
            auto_retry: false,
        }
    }
    #[test]
    fn durable_client_journal_recovers_complete_identity_and_only_retries_known_failure()
    -> Result<()> {
        let _environment = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        let endpoint = std::env::temp_dir().join("synthetic-endpoint");
        let mut first = ClientInputs::new(&endpoint, Some("first"))?;
        let original = first.stage(input("session_fixture", "original Ω"), 7)?;
        let mut next = input("session_fixture", "");
        next.queued_messages = Some(vec!["queued Ω".into()]);
        let next = first.stage(next, 8)?;
        assert_eq!(
            first.recover("session_fixture")?,
            vec![original.clone(), next.clone()]
        );
        let mut peer = ClientInputs::new(&endpoint, Some("peer"))?;
        assert!(peer.recover("session_fixture")?.is_empty());
        first.active = Some(original.input.id);
        assert!(first.prepare_retry("session_fixture").is_err());
        drop(first);
        assert_eq!(
            peer.recover("session_fixture")?,
            vec![original.clone(), next.clone()]
        );
        let accepted = PrimaryInputReceipt {
            id: original.input.id,
            session: "session_fixture".into(),
            state: PrimaryInputState::Accepted,
            messages: vec![],
            issue: None,
        };
        peer.observe(&accepted, false)?;
        assert_eq!(peer.recover("session_fixture")?, vec![next]);
        let terminal = PrimaryInputReceipt {
            state: PrimaryInputState::Committed,
            messages: vec!["message".into()],
            issue: Some("synthetic provider failure".into()),
            ..accepted.clone()
        };
        peer.active = Some(original.input.id);
        let committed_only = PrimaryInputReceipt {
            issue: None,
            ..terminal.clone()
        };
        peer.observe_cancellation(&committed_only)?;
        assert!(peer.prepare_retry("session_fixture").is_err());
        assert!(
            peer.pending_inspections("session_fixture")?
                .contains(&original.input.id)
        );
        peer.observe(&terminal, true)?;
        peer.observe_cancellation(&committed_only)?;
        assert_eq!(
            peer.receipt("session_fixture", original.input.id)?
                .unwrap()
                .issue,
            terminal.issue
        );
        peer.observe(&accepted, false)?; // a late acceptance cannot erase terminal truth
        peer.active = Some(original.input.id);
        peer.prepare_retry("session_fixture")?;
        let retry = peer.stage(input("session_fixture", "unused retry placeholder"), 9)?;
        assert_ne!(retry.input.id, original.input.id);
        assert_eq!(retry.retry_of, Some(original.input.id));
        assert_eq!(retry.input.content, original.input.content);
        assert_eq!(retry.input.images, original.input.images);
        assert_eq!(retry.input.activate_skill, None); // the previous committed turn activated it
        assert_eq!(retry.input.observe_startup_context, Some(false));
        assert_eq!(retry.retry_attempts, 1);
        assert!(retry.auto_retry && !retry.is_system);
        assert_eq!(
            peer.request("session_fixture", original.input.id)?,
            Some(original)
        );
        Ok(())
    }
    #[test]
    fn durable_client_journal_fails_closed_on_lost_sequence_and_corrupt_owner() -> Result<()> {
        let _environment = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        let mut journal = ClientInputs::new(
            &std::env::temp_dir().join("synthetic-endpoint"),
            Some("owner"),
        )?;
        let original = journal.stage(input("session_fixture", "retain"), 1)?;
        let directory = journal.directory("session_fixture")?;
        std::fs::remove_file(directory.join(".next"))?;
        assert!(
            journal
                .stage(input("session_fixture", "must not replace"), 2)
                .is_err()
        );
        let path = directory.join(format!("{}.json", original.input.id));
        assert_eq!(ClientInputs::read(&path)?.request, original);
        let mut damaged = ClientInputs::read(&path)?;
        damaged.owner = "../../foreign".into();
        ClientInputs::save(&path, &damaged)?;
        assert!(journal.recover("session_fixture").is_err());
        assert!(journal.directory("../escape").is_err());
        Ok(())
    }
}
