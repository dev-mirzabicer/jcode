//! Durable reviewed runtime intent. This journal never stops work or infers
//! quiescence. The live coordinator supplies observations from execution owners.
use anyhow::{Context, Result, ensure};
use jcode_workspace_types::{OperationId, RequestId, ReviewId, Revision, runtime::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MARKER: &[u8] = b"jcode-runtime-lifecycle-v1\n";

pub mod admission;

#[derive(Clone)]
pub struct RuntimeStopStore {
    directory: PathBuf,
    namespace: String,
}

/// Kernel ownership is independent of a socket, a PID or a control reply.
pub struct RuntimeStopOwner {
    store: RuntimeStopStore,
    identity: String,
    _lease: File,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    namespace: String,
    revision: Revision,
    desired_stopped: bool,
    current: Option<OperationId>,
    reviews: Vec<ShutdownReview>,
    operations: Vec<ShutdownOperation>,
}

struct Transaction {
    store: RuntimeStopStore,
    journal: Journal,
    lease: File,
    initialized: bool,
}

fn open_file(path: &Path, create: bool, writable: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .create(create)
        .truncate(false);
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
        "Runtime control path is not a regular file"
    );
    Ok(file)
}

impl RuntimeStopStore {
    pub fn new(state_root: &Path, socket: &Path) -> Result<Self> {
        ensure!(
            socket.is_absolute(),
            "Runtime socket must have an absolute namespace"
        );
        let parent = socket
            .parent()
            .context("Runtime socket has no parent")?
            .canonicalize()?;
        let socket = parent.join(socket.file_name().context("Runtime socket has no name")?);
        let namespace = format!(
            "{:x}",
            Sha256::digest(socket.as_os_str().as_encoded_bytes())
        );
        Ok(Self {
            directory: state_root.join("runtime-lifecycle").join(&namespace),
            namespace,
        })
    }

    fn transaction(&self) -> Result<Transaction> {
        // Do not let a symlink turn native state protection into a different store.
        for path in [
            self.directory.parent().context("Missing control parent")?,
            self.directory.as_path(),
        ] {
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "Runtime control directory identity changed"
                );
            }
            crate::storage::ensure_dir(path)?;
        }
        let mut lease = open_file(&self.directory.join("journal.lock"), true, true)?;
        lease.lock().context("Lock runtime lifecycle journal")?;
        let mut marker = Vec::new();
        Read::by_ref(&mut lease)
            .take(128)
            .read_to_end(&mut marker)?;
        ensure!(
            marker.is_empty() || marker == MARKER,
            "Runtime lifecycle initialization record is damaged"
        );
        let initialized = !marker.is_empty();
        let path = self.directory.join("journal.json");
        let journal = match open_file(&path, false, false) {
            Ok(file) => serde_json::from_reader::<_, Journal>(file).context(
                "Runtime lifecycle journal is damaged; no backup or empty fallback was adopted",
            )?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    && !initialized =>
            {
                Journal {
                    schema: 1,
                    namespace: self.namespace.clone(),
                    ..Default::default()
                }
            }
            Err(error) => {
                return Err(error.context(
                    "Runtime lifecycle state is unavailable; automatic startup is blocked",
                ));
            }
        };
        ensure!(
            journal.schema == 1 && journal.namespace == self.namespace,
            "Runtime lifecycle schema or namespace mismatch"
        );
        if let Some(current) = journal.current {
            ensure!(
                journal
                    .operations
                    .iter()
                    .filter(|op| op.id == current)
                    .count()
                    == 1,
                "Runtime lifecycle current operation is inconsistent"
            );
        }
        Ok(Transaction {
            store: self.clone(),
            journal,
            lease,
            initialized,
        })
    }

    pub fn status(&self) -> Result<RuntimeStatus> {
        if !self.directory.try_exists()? {
            return Ok(RuntimeStatus {
                runtime: None,
                desired_stopped: false,
                revision: 0,
                operation: None,
                work: Vec::new(),
            });
        }
        let tx = self.transaction()?;
        Ok(tx.status())
    }

    pub fn require_automatic_start(&self) -> Result<()> {
        ensure!(
            !self.status()?.desired_stopped,
            "Runtime was intentionally stopped; use jcode runtime start explicitly"
        );
        Ok(())
    }

    /// Explicit Start is independent of notification delivery. It does not
    /// authorize replay or continuation of any interrupted primary or command.
    pub fn authorize_start(&self) -> Result<RuntimeStatus> {
        let mut tx = self.transaction()?;
        let lease = open_file(&self.directory.join("owner.lock"), true, true)?;
        lease
            .try_lock()
            .context("Runtime is still owned; Start cannot cancel a live shutdown")?;
        tx.interrupt_unfinished();
        tx.journal.desired_stopped = false;
        tx.commit()?;
        Ok(tx.status())
    }

    pub fn claim(&self) -> Result<RuntimeStopOwner> {
        let mut tx = self.transaction()?;
        let lease = open_file(&self.directory.join("owner.lock"), true, true)?;
        lease
            .try_lock()
            .context("Runtime namespace already has a live coordinator")?;
        ensure!(
            !tx.journal.desired_stopped,
            "Runtime is intentionally stopped; explicit Start is required"
        );
        tx.interrupt_unfinished();
        tx.commit()?;
        Ok(RuntimeStopOwner {
            store: self.clone(),
            identity: uuid::Uuid::new_v4().to_string(),
            _lease: lease,
        })
    }
}

impl Transaction {
    fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            runtime: None,
            desired_stopped: self.journal.desired_stopped,
            revision: self.journal.revision,
            operation: self.journal.current.and_then(|id| {
                self.journal
                    .operations
                    .iter()
                    .find(|op| op.id == id)
                    .cloned()
            }),
            work: Vec::new(),
        }
    }

    fn interrupt_unfinished(&mut self) {
        for op in &mut self.journal.operations {
            if !op.phase.terminal() {
                op.phase = ShutdownPhase::Interrupted;
                op.revision += 1;
                op.issues.push("Previous runtime ended without a verified shutdown outcome; inspect original work before continuing".into());
            }
        }
    }

    fn commit(&mut self) -> Result<()> {
        self.journal.revision = self
            .journal
            .revision
            .checked_add(1)
            .context("Runtime revision exhausted")?;
        // Write the initialization witness first. An interrupted initial write
        // must not later masquerade as a never-used namespace.
        if !self.initialized {
            self.lease.write_all(MARKER)?;
            self.lease.sync_all()?;
            self.initialized = true;
        }
        crate::storage::write_json_secret(
            &self.store.directory.join("journal.json"),
            &self.journal,
        )?;
        File::open(&self.store.directory)?.sync_all()?;
        Ok(())
    }

    fn operation(&mut self, id: OperationId, runtime: &str) -> Result<&mut ShutdownOperation> {
        let op = self
            .journal
            .operations
            .iter_mut()
            .find(|op| op.id == id)
            .context("Unknown runtime operation")?;
        ensure!(
            op.review.runtime == runtime,
            "Operation belongs to another runtime incarnation"
        );
        Ok(op)
    }
}

impl RuntimeStopOwner {
    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn status(&self) -> Result<RuntimeStatus> {
        let mut status = self.store.status()?;
        status.runtime = Some(self.identity.clone());
        Ok(status)
    }

    pub fn inspect(&self, id: OperationId) -> Result<ShutdownOperation> {
        self.store
            .transaction()?
            .journal
            .operations
            .into_iter()
            .find(|op| op.id == id)
            .context("Unknown runtime operation")
    }

    pub fn review(
        &self,
        options: ShutdownOptions,
        work: Vec<RuntimeWork>,
    ) -> Result<ShutdownReview> {
        ensure!(
            options.quiescence_timeout_seconds > 0,
            "Quiescence timeout must be positive"
        );
        validate_work(&work)?;
        let mut tx = self.store.transaction()?;
        ensure!(
            !tx.journal.operations.iter().any(|op| !op.phase.terminal()),
            "Another runtime shutdown is active"
        );
        let review = ShutdownReview {
            id: ReviewId::new(),
            runtime: self.identity.clone(),
            revision: tx.journal.revision,
            options,
            work,
        };
        tx.journal.reviews.push(review.clone());
        tx.commit()?;
        Ok(review)
    }

    /// The caller holds its live admission fence across this transaction and
    /// adoption of the returned phase. Completed work may disappear from review,
    /// but new/rebound owners may not acquire implicit approval.
    pub fn begin(
        &self,
        request: RequestId,
        review: ReviewId,
        work: Vec<RuntimeWork>,
    ) -> Result<ShutdownOperation> {
        validate_work(&work)?;
        let mut tx = self.store.transaction()?;
        if let Some(op) = tx
            .journal
            .operations
            .iter()
            .find(|op| op.request == request)
        {
            ensure!(
                op.review.id == review,
                "Shutdown retry conflicts with its original review"
            );
            return Ok(op.clone());
        }
        ensure!(
            !tx.journal.operations.iter().any(|op| !op.phase.terminal()),
            "Another runtime shutdown is active"
        );
        let review = tx
            .journal
            .reviews
            .iter()
            .find(|item| item.id == review)
            .cloned()
            .context("Unknown shutdown review")?;
        ensure!(
            review.runtime == self.identity,
            "Shutdown review belongs to a previous runtime"
        );
        ensure!(
            !tx.journal
                .operations
                .iter()
                .any(|op| op.review.id == review.id),
            "Shutdown review was already used; review current work again"
        );
        ensure!(
            work.iter().all(|item| review.work.contains(item)),
            "Shutdown review is stale: new work or changed owners require review"
        );
        let phase = match review.options.strategy {
            StopStrategy::FinishCurrent => ShutdownPhase::WaitingForCurrent,
            StopStrategy::Interrupt => ShutdownPhase::Stopping,
        };
        let op = ShutdownOperation {
            id: OperationId::new(),
            request,
            review,
            revision: 1,
            phase,
            force_requested: false,
            remaining: work,
            preserved: Vec::new(),
            issues: Vec::new(),
        };
        tx.journal.current = Some(op.id);
        tx.journal.desired_stopped = true;
        tx.journal.operations.push(op.clone());
        tx.commit()?;
        Ok(op)
    }

    pub fn cancel_wait(&self, id: OperationId, expected: Revision) -> Result<ShutdownOperation> {
        let mut tx = self.store.transaction()?;
        let op = tx.operation(id, &self.identity)?;
        if op.phase == ShutdownPhase::Cancelled && expected.checked_add(1) == Some(op.revision) {
            return Ok(op.clone());
        }
        ensure!(op.revision == expected, "Shutdown revision changed");
        ensure!(
            op.phase == ShutdownPhase::WaitingForCurrent,
            "Shutdown can be cancelled only while waiting"
        );
        op.phase = ShutdownPhase::Cancelled;
        op.revision += 1;
        let result = op.clone();
        tx.journal.desired_stopped = false;
        tx.commit()?;
        Ok(result)
    }

    pub fn enter_stopping(&self, id: OperationId, expected: Revision) -> Result<ShutdownOperation> {
        self.update(id, expected, |op| {
            ensure!(
                op.phase == ShutdownPhase::WaitingForCurrent,
                "Shutdown is no longer waiting"
            );
            op.phase = ShutdownPhase::Stopping;
            Ok(())
        })
    }

    pub fn retry(
        &self,
        id: OperationId,
        expected: Revision,
        force: bool,
    ) -> Result<ShutdownOperation> {
        self.update(id, expected, |op| {
            ensure!(op.phase == ShutdownPhase::Blocked || (force && op.phase == ShutdownPhase::Stopping), "Only a blocked shutdown can retry; Force cannot replace FinishCurrent while waiting");
            op.phase = ShutdownPhase::Stopping;
            op.force_requested |= force;
            op.issues.clear();
            Ok(())
        })
    }

    pub fn observe(
        &self,
        id: OperationId,
        expected: Revision,
        remaining: Vec<RuntimeWork>,
        preserved: Vec<RuntimeWork>,
        issues: Vec<String>,
    ) -> Result<ShutdownOperation> {
        validate_work(&remaining)?;
        validate_work(&preserved)?;
        self.update(id, expected, |op| {
            ensure!(
                !op.phase.terminal(),
                "Cannot change terminal shutdown observations"
            );
            ensure!(
                preserved.is_empty()
                    || op.review.options.independent == IndependentTasks::KeepSupported,
                "Stop-all cannot preserve independent work"
            );
            ensure!(
                preserved.iter().all(|work| work.supported_survivor),
                "Unsupported work cannot be preserved"
            );
            ensure!(
                !remaining
                    .iter()
                    .any(|work| preserved.iter().any(|other| other.id == work.id)),
                "A preserved owner cannot also be pending shutdown"
            );
            op.remaining = remaining;
            op.preserved = preserved;
            op.issues = issues;
            Ok(())
        })
    }

    pub fn block(
        &self,
        id: OperationId,
        expected: Revision,
        issues: Vec<String>,
    ) -> Result<ShutdownOperation> {
        self.update(id, expected, |op| {
            ensure!(
                op.phase == ShutdownPhase::Stopping,
                "Only a stopping operation can be blocked"
            );
            ensure!(
                !issues.is_empty(),
                "A blocked operation must retain its failure"
            );
            op.phase = ShutdownPhase::Blocked;
            op.issues = issues;
            Ok(())
        })
    }

    pub fn complete(&self, id: OperationId, expected: Revision) -> Result<ShutdownOperation> {
        self.update(id, expected, |op| {
            ensure!(
                op.phase == ShutdownPhase::Stopping
                    && op.remaining.is_empty()
                    && op.issues.is_empty(),
                "Shutdown does not have a verified quiescent outcome"
            );
            op.phase = ShutdownPhase::Stopped;
            Ok(())
        })
    }

    fn update(
        &self,
        id: OperationId,
        expected: Revision,
        change: impl FnOnce(&mut ShutdownOperation) -> Result<()>,
    ) -> Result<ShutdownOperation> {
        let mut tx = self.store.transaction()?;
        let op = tx.operation(id, &self.identity)?;
        ensure!(op.revision == expected, "Shutdown revision changed");
        change(op)?;
        op.revision = op
            .revision
            .checked_add(1)
            .context("Shutdown revision exhausted")?;
        let result = op.clone();
        tx.commit()?;
        Ok(result)
    }
}

fn validate_work(work: &[RuntimeWork]) -> Result<()> {
    let mut identities = std::collections::HashSet::new();
    for item in work {
        ensure!(
            !item.id.is_empty() && !item.owner.is_empty() && identities.insert(&item.id),
            "Invalid or duplicate runtime work identity"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
