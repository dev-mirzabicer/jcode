//! A kernel-lifetime projection of existing Session activity, shared by same-user
//! runtime namespaces. Session checkpoints remain location authority.
use super::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    token: String,
    namespace: String,
    session: String,
    session_root: PathBuf,
    execution_root: PathBuf,
}

pub(super) struct Projection {
    path: PathBuf,
    _file: File,
}
impl Drop for Projection {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            crate::logging::warn(&format!("Activity projection cleanup failed: {error}"));
        }
    }
}

pub struct ActiveSessionLocation {
    pub session: String,
    pub session_root: PathBuf,
    pub working_directory: Option<PathBuf>,
}

#[cfg(unix)]
pub fn activity_projection_directory() -> PathBuf {
    // SAFETY: geteuid has no preconditions or side effects.
    PathBuf::from(format!("/tmp/jcode-session-activity-{}", unsafe {
        libc::geteuid()
    }))
}

#[cfg(unix)]
fn validate_directory(directory: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(directory)?;
    // SAFETY: geteuid has no preconditions or side effects.
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Shared activity directory has unexpected ownership or protection"
    );
    Ok(())
}

#[cfg(unix)]
fn open(path: &Path, create: bool) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions or side effects.
    ensure!(
        metadata.is_file()
            && (1..=2).contains(&metadata.nlink())
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Activity projection is not a private owned file"
    );
    Ok(file)
}

impl SessionActivityGuard {
    #[cfg(unix)]
    pub(crate) fn publish_location(&mut self, session_root: &Path) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        ensure!(
            self.projection.is_none(),
            "Activity location was already published"
        );
        let directory = activity_projection_directory();
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        validate_directory(&directory)?;
        let record = Record {
            token: self.token.clone(),
            namespace: self.namespace.clone(),
            session: self.session.clone(),
            session_root: session_root.canonicalize()?,
            execution_root: self.store.root().canonicalize()?,
        };
        let stage = directory.join(format!(".{}.pending", self.token));
        let final_path = directory.join(format!("{}.json", self.token));
        let mut file = open(&stage, true)?;
        file.lock()?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        // A reader sees complete metadata only. The caller retains filesystem
        // admission until this publication, before inference can begin.
        std::fs::hard_link(&stage, &final_path)?;
        std::fs::remove_file(&stage)?;
        File::open(&directory)?.sync_all()?;
        self.projection = Some(Projection {
            path: final_path,
            _file: file,
        });
        Ok(())
    }
}

#[cfg(unix)]
pub fn active_session_locations() -> Result<Vec<ActiveSessionLocation>> {
    let directory = activity_projection_directory();
    match std::fs::symlink_metadata(&directory) {
        Ok(_) => validate_directory(&directory)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let mut result = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let mut file = match open(&entry.path(), false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        match file.try_lock() {
            Ok(()) => continue, // Kernel proves the projected activity has ended.
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: Record = serde_json::from_slice(&bytes)?;
        ensure!(
            entry.file_name().to_str() == Some(&format!("{}.json", record.token)),
            "Activity projection identity mismatch"
        );
        let database = record.execution_root.join("index.sqlite");
        // Inspection never initializes or migrates a foreign execution store.
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let namespace: String = connection.query_row(
            "SELECT namespace FROM provider_receipt_namespace WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            namespace == record.namespace,
            "Live activity execution namespace was replaced"
        );
        let registered: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_activity_leases WHERE token=?1 AND session_id=?2)",
            params![record.token, record.session],
            |row| row.get(0),
        )?;
        if !registered {
            continue;
        } // Completion can precede projection cleanup.
        let session =
            crate::session::Session::load_startup_stub_in(&record.session_root, &record.session)?;
        result.push(ActiveSessionLocation {
            session: record.session,
            session_root: record.session_root,
            working_directory: session.working_dir.map(PathBuf::from),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_rejects_replaced_execution_namespace_without_initializing_it() -> Result<()> {
        let _environment = crate::storage::lock_test_env();
        let session_root = crate::storage::jcode_dir()?;
        let mut session = crate::session::Session::create(None, None);
        session.save()?;
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let mut activity = store.begin_session_activity(&session.id)?;
        activity.publish_location(&session_root)?;
        let old = directory.path().join("old-execution");
        std::fs::rename(store.root(), &old)?;
        assert!(active_session_locations().is_err());
        assert!(!store.root().exists());
        let replacement = ExecutionStore::open(directory.path())?;
        assert!(active_session_locations().is_err());
        drop(activity);
        assert!(replacement.last_activity(&session.id)?.is_none());
        assert!(
            !active_session_locations()?
                .iter()
                .any(|entry| entry.session == session.id)
        );
        Ok(())
    }

    #[test]
    fn projection_process_helper() -> Result<()> {
        let Some(root) = std::env::var_os("JCODE_ACTIVITY_PROJECTION_FIXTURE_ROOT") else {
            return Ok(());
        };
        let root = PathBuf::from(root);
        let session = std::env::var("JCODE_ACTIVITY_PROJECTION_FIXTURE_SESSION")?;
        let ready = PathBuf::from(
            std::env::var_os("JCODE_ACTIVITY_PROJECTION_FIXTURE_READY")
                .context("Missing fixture rendezvous")?,
        );
        let store = ExecutionStore::open(&root)?;
        let mut guard = store.begin_session_activity(&session)?;
        guard.publish_location(&root)?;
        std::fs::write(ready, &guard.token)?;
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte)?;
        Ok(())
    }

    #[test]
    fn projection_process_death_releases_kernel_activity_without_pid_guessing() -> Result<()> {
        let _environment = crate::storage::lock_test_env();
        let root = crate::storage::jcode_dir()?;
        let mut session = crate::session::Session::create(None, None);
        let scratch = tempfile::tempdir()?;
        session.working_dir = Some(scratch.path().canonicalize()?.display().to_string());
        session.save()?;
        let ready = scratch.path().join("ready");
        struct Owned(std::process::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Owned(
            std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "execution::activity::projection::tests::projection_process_helper",
                    "--nocapture",
                ])
                .env("JCODE_ACTIVITY_PROJECTION_FIXTURE_ROOT", &root)
                .env("JCODE_ACTIVITY_PROJECTION_FIXTURE_SESSION", &session.id)
                .env("JCODE_ACTIVITY_PROJECTION_FIXTURE_READY", &ready)
                .stdin(std::process::Stdio::piped())
                .spawn()?,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !ready.exists() {
            ensure!(
                child.0.try_wait()?.is_none(),
                "Fixture child exited before publishing activity"
            );
            ensure!(
                std::time::Instant::now() < deadline,
                "Fixture activity publication timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        ensure!(
            active_session_locations()?
                .iter()
                .any(|p| p.session == session.id),
            "Live child activity was invisible"
        );
        child.0.kill()?;
        child.0.wait()?;
        ensure!(
            !active_session_locations()?
                .iter()
                .any(|p| p.session == session.id),
            "Dead process retained live activity"
        );
        let token = std::fs::read_to_string(ready)?;
        ensure!(
            token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid fixture token"
        );
        let path = activity_projection_directory().join(format!("{token}.json"));
        let mut file = open(&path, false)?;
        file.try_lock()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: Record = serde_json::from_slice(&bytes)?;
        ensure!(
            record.token == token && record.session == session.id,
            "Fixture cleanup identity mismatch"
        );
        std::fs::remove_file(path)?;
        Ok(())
    }
}
