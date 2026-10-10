//! The agent-editable files of a session: `workflow.md`, `summary.md` and the
//! read-only `history/` revisions, under `<jcode home>/session-work/<session>/`.
//!
//! The store is the authority. A workflow write is validated, committed as a
//! revision, then written to the file, and history is refreshed. A file that
//! differs from the store is restored at the next safe point.
use super::store::{CommitReceipt, SessionWorkStore};
use super::{SessionWorkError, store::private_dir};
use chrono::{DateTime, Utc};
use jcode_session_work_types::RevisionSource;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const WORKFLOW_FILE: &str = "workflow.md";
pub const SUMMARY_FILE: &str = "summary.md";
pub const HISTORY_DIR: &str = "history";
/// Revisions kept as files in `history/`. The store keeps all of them.
pub const HISTORY_FILES: usize = 40;

/// A path inside the session-work surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfacePath {
    pub session: String,
    pub file: SurfaceFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurfaceFile {
    /// The surface root or a session directory itself.
    Directory,
    Workflow,
    Summary,
    /// Anything under `history/`.
    History,
    /// Any other name.
    Other,
}

/// What a safe-point reconciliation found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reconciled {
    /// No workflow exists yet.
    NoWorkflow,
    Unchanged,
    /// The host finished writing a committed revision after an interruption.
    Synced {
        revision: u32,
    },
    /// The file was changed outside the native file tools and was restored.
    Restored {
        revision: u32,
    },
}

enum FileState {
    Missing,
    Regular(Vec<u8>),
    /// A symlink, directory or other entry where a file belongs.
    NotRegular,
}

impl FileState {
    fn holds(&self, bytes: &[u8]) -> bool {
        matches!(self, Self::Regular(current) if current == bytes)
    }
}

#[derive(Clone, Debug)]
pub struct SessionWorkSurface {
    root: PathBuf,
    store: SessionWorkStore,
}

impl SessionWorkSurface {
    /// The surface under the Jcode home, over the default store.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self::at(
            crate::storage::jcode_dir()?.join("session-work"),
            SessionWorkStore::new(),
        ))
    }

    pub fn at(root: impl Into<PathBuf>, store: SessionWorkStore) -> Self {
        Self {
            root: root.into(),
            store,
        }
    }

    pub fn store(&self) -> &SessionWorkStore {
        &self.store
    }

    pub fn session_dir(&self, session: &str) -> Result<PathBuf, SessionWorkError> {
        validate_session_id(session)?;
        Ok(self.root.join(session))
    }

    pub fn workflow_path(&self, session: &str) -> Result<PathBuf, SessionWorkError> {
        Ok(self.session_dir(session)?.join(WORKFLOW_FILE))
    }

    pub fn summary_path(&self, session: &str) -> Result<PathBuf, SessionWorkError> {
        Ok(self.session_dir(session)?.join(SUMMARY_FILE))
    }

    pub fn history_dir(&self, session: &str) -> Result<PathBuf, SessionWorkError> {
        Ok(self.session_dir(session)?.join(HISTORY_DIR))
    }

    /// The surface root with existing ancestors resolved, for comparing with
    /// resolved native-tool destinations.
    pub fn resolved_root(&self) -> PathBuf {
        if let Ok(root) = self.root.canonicalize() {
            return root;
        }
        match (self.root.parent(), self.root.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .map(|parent| parent.join(name))
                .unwrap_or_else(|_| self.root.clone()),
            _ => self.root.clone(),
        }
    }

    /// Classify a resolved absolute path. `None` when it is outside the surface.
    pub fn classify(&self, resolved: &Path) -> Option<SurfacePath> {
        let root = self.resolved_root();
        let relative = resolved.strip_prefix(&root).ok()?;
        let parts: Vec<String> = relative
            .components()
            .map(|component| match component {
                Component::Normal(part) => part.to_string_lossy().into_owned(),
                _ => String::from(".."),
            })
            .collect();
        let Some(session) = parts.first() else {
            return Some(SurfacePath {
                session: String::new(),
                file: SurfaceFile::Directory,
            });
        };
        let file = match parts.get(1..).unwrap_or_default() {
            [] => SurfaceFile::Directory,
            [name] if name == WORKFLOW_FILE => SurfaceFile::Workflow,
            [name] if name == SUMMARY_FILE => SurfaceFile::Summary,
            [name, ..] if name == HISTORY_DIR => SurfaceFile::History,
            _ => SurfaceFile::Other,
        };
        Some(SurfacePath {
            session: session.clone(),
            file,
        })
    }

    /// The current workflow text from the store, which is the authority.
    pub fn current_workflow(&self, session: &str) -> Result<Option<String>, SessionWorkError> {
        Ok(self
            .store
            .workflow_head(session)?
            .map(|head| head.revision.text))
    }

    /// Validate and commit `text` as the agent's next revision, then write the
    /// file and refresh history. Nothing changes when validation fails.
    pub fn write_workflow(
        &self,
        session: &str,
        request: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> Result<CommitReceipt, SessionWorkError> {
        let receipt =
            self.store
                .commit_workflow(session, request, text, &RevisionSource::Agent {}, now)?;
        self.sync_file(session)?;
        self.refresh_history(session)?;
        Ok(receipt)
    }

    pub fn read_summary(&self, session: &str) -> Result<Option<Vec<u8>>, SessionWorkError> {
        Ok(match read_file(&self.summary_path(session)?)? {
            FileState::Regular(bytes) => Some(bytes),
            FileState::Missing | FileState::NotRegular => None,
        })
    }

    /// `summary.md` is a plain draft owned by the host until closeout freezes it.
    pub fn write_summary(&self, session: &str, bytes: &[u8]) -> Result<(), SessionWorkError> {
        let dir = self.session_dir(session)?;
        private_dir(&dir)?;
        write_atomic(&dir, SUMMARY_FILE, bytes, 0o600)
    }

    /// Create the session directory and bring the files up to the store.
    pub fn materialize(&self, session: &str) -> Result<(), SessionWorkError> {
        private_dir(&self.session_dir(session)?)?;
        if self.store.workflow_head(session)?.is_some() {
            self.sync_file(session)?;
            self.refresh_history(session)?;
        }
        Ok(())
    }

    /// Bring `workflow.md` and `history/` back to the store at a safe point.
    pub fn reconcile(&self, session: &str) -> Result<Reconciled, SessionWorkError> {
        let Some(head) = self.store.workflow_head(session)? else {
            return Ok(Reconciled::NoWorkflow);
        };
        let revision = head.revision.revision;
        let outcome = if head.synced < revision {
            self.sync_file(session)?;
            Reconciled::Synced { revision }
        } else if read_file(&self.workflow_path(session)?)?.holds(head.revision.text.as_bytes()) {
            Reconciled::Unchanged
        } else {
            self.sync_file(session)?;
            Reconciled::Restored { revision }
        };
        self.refresh_history(session)?;
        Ok(outcome)
    }

    /// Remove everything of a session that was never published.
    pub fn remove_unpublished(&self, session: &str) -> Result<(), SessionWorkError> {
        // An identity that cannot name a session directory never had one.
        let Ok(dir) = self.session_dir(session) else {
            return Ok(());
        };
        let materialized = std::fs::symlink_metadata(&dir).is_ok();
        if let Err(error) = self.store.remove_unpublished(session) {
            // Activation creates the directory right after its store rows. A
            // session without one never reached session work, so an unrelated
            // store problem must not block its cleanup.
            if materialized {
                return Err(error);
            }
            return Ok(());
        }
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&dir),
            Ok(_) => std::fs::remove_file(&dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
        .map_err(SessionWorkError::io)
    }

    /// Write the head revision to the file and record it as written.
    fn sync_file(&self, session: &str) -> Result<(), SessionWorkError> {
        let head = self
            .store
            .workflow_head(session)?
            .ok_or_else(|| SessionWorkError::Corrupt("workflow head disappeared".into()))?;
        let dir = self.session_dir(session)?;
        private_dir(&dir)?;
        write_atomic(&dir, WORKFLOW_FILE, head.revision.text.as_bytes(), 0o600)?;
        self.store.mark_synced(session, head.revision.revision)
    }

    /// Keep the newest revisions as read-only `history/rNN.md` files.
    fn refresh_history(&self, session: &str) -> Result<(), SessionWorkError> {
        let dir = self.history_dir(session)?;
        private_dir(&dir)?;
        let revisions = self.store.recent_revisions(session, HISTORY_FILES)?;
        let kept: Vec<String> = revisions
            .iter()
            .map(|revision| history_file_name(revision.revision))
            .collect();
        for (revision, name) in revisions.iter().zip(&kept) {
            if !read_file(&dir.join(name))?.holds(revision.text.as_bytes()) {
                write_atomic(&dir, name, revision.text.as_bytes(), 0o400)?;
            }
        }
        for entry in std::fs::read_dir(&dir).map_err(SessionWorkError::io)? {
            let entry = entry.map_err(SessionWorkError::io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_history_file_name(&name) && !kept.contains(&name) {
                std::fs::remove_file(entry.path()).map_err(SessionWorkError::io)?;
            }
        }
        Ok(())
    }
}

pub fn history_file_name(revision: u32) -> String {
    format!("r{revision:02}.md")
}

fn is_history_file_name(name: &str) -> bool {
    name.strip_prefix('r')
        .and_then(|rest| rest.strip_suffix(".md"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn validate_session_id(session: &str) -> Result<(), SessionWorkError> {
    let valid = !session.is_empty()
        && session.len() <= 200
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(SessionWorkError::Invalid(format!(
            "`{session}` is not a session ID"
        )))
    }
}

fn read_file(path: &Path) -> Result<FileState, SessionWorkError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => std::fs::read(path)
            .map(FileState::Regular)
            .map_err(SessionWorkError::io),
        Ok(_) => Ok(FileState::NotRegular),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileState::Missing),
        Err(error) => Err(SessionWorkError::io(error)),
    }
}

/// Write a complete file by rename, so readers never see a partial text. A
/// directory or symlink in the file's place is replaced, never followed.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> Result<(), SessionWorkError> {
    let temp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let target = dir.join(name);
        if let Ok(metadata) = std::fs::symlink_metadata(&target)
            && metadata.is_dir()
        {
            // Never delete a directory someone put here; its contents are theirs.
            return Err(std::io::Error::other(format!(
                "{} is a directory; move it away so the host can write the file",
                target.display()
            )));
        }
        std::fs::rename(&temp, &target)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(SessionWorkError::io)
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
