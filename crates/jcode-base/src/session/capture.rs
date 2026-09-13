//! Coherent read-only capture at the Session persistence boundary.
use super::{Session, session_journal_path_from_snapshot};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PersistenceIdentity {
    #[serde(default)]
    pub epoch: String,
    /// Exact journal already incorporated in this checkpoint. A crash after
    /// snapshot publication but before journal unlink must not replay it twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_journal_sha256: Option<String>,
}

impl PersistenceIdentity {
    pub fn read(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Header {
            #[serde(default)]
            persistence_identity: PersistenceIdentity,
        }
        Ok(
            serde_json::from_reader::<_, Header>(BufReader::new(File::open(path)?))?
                .persistence_identity,
        )
    }

    pub fn journal_is_retired(&self, path: &Path) -> Result<bool> {
        match &self.retired_journal_sha256 {
            Some(expected) => Ok(file_digest(path)?.as_ref() == Some(expected)),
            None => Ok(false),
        }
    }
}

pub(super) fn file_digest(path: &Path) -> Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    Ok(Some(format!("{:x}", digest.finalize())))
}

/// Short cross-process storage ownership, never held during Agent execution or
/// rendering. Keeping the lock file avoids inode replacement between waiters.
pub(super) fn persistence_lease(path: &Path) -> Result<File> {
    let parent = path.parent().context("Session snapshot has no parent")?;
    crate::storage::ensure_dir(parent)?;
    let parent = parent.canonicalize()?;
    super::persistence_writer::register(&parent)?;
    let leaf = path
        .file_name()
        .context("Session snapshot has no filename")?;
    let lock = parent.join(leaf).with_extension("persistence.lock");
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&lock)?;
    ensure!(
        file.metadata()?.is_file(),
        "Session persistence lease is not a regular file"
    );
    file.lock().context("Acquire Session persistence lease")?;
    Ok(file)
}

/// Immutable captured source. This operation neither activates a profile nor
/// migrates, salvages, saves, rewinds, or edits the inspected conversation.
pub struct CapturedSession {
    session: Session,
}
impl CapturedSession {
    pub fn session(&self) -> &Session {
        &self.session
    }
}

impl Session {
    /// Read relationship metadata without loading prompt/transcript bodies or
    /// invoking the activation/migration loader.
    pub fn inspection_parent(state_root: &Path, session_id: &str) -> Result<Option<String>> {
        ensure!(
            !session_id.is_empty()
                && session_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "Invalid inspection session identity"
        );
        let path = state_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "Inspection relationship source changed type"
        );
        let _lease = persistence_lease(&path)?;
        #[derive(Deserialize)]
        struct Header {
            id: String,
            parent_id: Option<String>,
        }
        let header: Header = serde_json::from_reader(BufReader::new(File::open(&path)?))?;
        ensure!(
            header.id == session_id,
            "Inspection relationship identity mismatch"
        );
        Ok(header.parent_id)
    }

    pub fn capture_readonly(state_root: &Path, session_id: &str) -> Result<CapturedSession> {
        ensure!(
            !session_id.is_empty()
                && session_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "Invalid inspection target identity"
        );
        Self::capture_readonly_from_path(
            &state_root
                .join("sessions")
                .join(format!("{session_id}.json")),
            session_id,
        )
    }

    pub fn capture_readonly_from_path(path: &Path, session_id: &str) -> Result<CapturedSession> {
        Self::capture_readonly_with_metadata(path, session_id, |_| Ok(()))
            .map(|(session, ())| session)
    }

    pub(crate) fn capture_readonly_with_metadata<T>(
        path: &Path,
        session_id: &str,
        capture_metadata: impl FnOnce(&Session) -> Result<T>,
    ) -> Result<(CapturedSession, T)> {
        // Reject absence before allocating any lock metadata.
        ensure!(
            std::fs::symlink_metadata(path)?.is_file(),
            "Inspection source is not a regular Session snapshot"
        );
        let _lease = persistence_lease(path)?;
        super::persistence_writer::verify_active(path, session_id)?;
        let journal = session_journal_path_from_snapshot(path);
        let before = source_stamps(path, &journal)?;
        let mut session: Session = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        ensure!(
            session.id == session_id,
            "Inspection target and stored Session identity disagree"
        );
        // Legacy active writers cannot participate in the new storage lease.
        if session.persistence_identity.epoch.is_empty() {
            let active = path
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join("active_pids").join(session_id));
            ensure!(
                !active.as_ref().is_some_and(|path| path.exists()),
                "Active legacy Session writer does not support coherent inspection; upgrade/resume the writer first"
            );
        }
        super::persistence::replay_inspection_journal(&mut session, &journal)?;
        ensure!(
            before == source_stamps(path, &journal)?,
            "Session source changed outside the cooperative persistence owner; retry inspection after upgrading its writer"
        );
        ensure!(
            session.compaction.is_none(),
            "Legacy compaction requires explicit Session restoration before projected inspection; inspection never migrates source"
        );
        if session.system_prompt.is_some() {
            session.validate_active_agent_profile()?;
        }
        session.reset_persist_state(true);
        session.reset_provider_messages_cache();
        let metadata = capture_metadata(&session)?;
        super::persistence_writer::verify_active(path, session_id)?;
        ensure!(
            before == source_stamps(path, &journal)?,
            "Session source changed outside persistence ownership during inspection metadata capture"
        );
        Ok((CapturedSession { session }, metadata))
    }
}

#[derive(PartialEq, Eq)]
struct SourceStamp {
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
fn stamp(path: &Path) -> Result<Option<SourceStamp>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        meta.is_file(),
        "Session source changed type: {}",
        path.display()
    );
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Some(SourceStamp {
        size: meta.len(),
        modified: meta.modified()?,
        #[cfg(unix)]
        device: meta.dev(),
        #[cfg(unix)]
        inode: meta.ino(),
    }))
}
fn source_stamps(
    snapshot: &Path,
    journal: &Path,
) -> Result<(Option<SourceStamp>, Option<SourceStamp>)> {
    Ok((stamp(snapshot)?, stamp(journal)?))
}
