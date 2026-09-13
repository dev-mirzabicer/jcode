//! Versioned cooperation proof for an active Session writer. This is a storage
//! compatibility handshake, not attestation of human intent or a process sandbox.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Serialize, Deserialize)]
struct Capability {
    schema: u32,
    pid: u32,
}

fn open(path: &Path, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
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
        "Session writer capability changed type"
    );
    Ok(file)
}

pub(super) fn register(session_directory: &Path) -> Result<()> {
    static WRITERS: OnceLock<Mutex<HashMap<PathBuf, File>>> = OnceLock::new();
    let directory = session_directory.join(".writers");
    crate::storage::ensure_dir(&directory)?;
    ensure!(
        std::fs::symlink_metadata(&directory)?.is_dir(),
        "Session writer capability directory changed type"
    );
    jcode_core::fs::set_directory_permissions_owner_only(&directory)?;
    let path = directory.join(format!("{}.lock", std::process::id()));
    let mut writers = WRITERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    writers.retain(|path, _| path.is_file());
    if writers.contains_key(&path) {
        return Ok(());
    }
    let mut file = open(&path, true)?;
    file.try_lock()
        .context("Session writer compatibility ownership is already held")?;
    file.set_len(0)?;
    file.rewind()?;
    serde_json::to_writer(
        &mut file,
        &Capability {
            schema: 1,
            pid: std::process::id(),
        },
    )?;
    file.flush()?;
    file.sync_all()?;
    writers.insert(path, file);
    Ok(())
}

pub(super) fn verify_active(snapshot: &Path, session_id: &str) -> Result<()> {
    let sessions = snapshot.parent().context("Missing Session directory")?;
    let root = sessions.parent().context("Missing Session namespace")?;
    let marker = root.join("active_pids").join(session_id);
    let pid = match std::fs::read_to_string(&marker) {
        Ok(value) => value
            .trim()
            .parse::<u32>()
            .context("Invalid active Session writer identity")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !crate::platform::is_process_running(pid) {
        return Ok(());
    }
    let directory = sessions.join(".writers");
    ensure!(
        std::fs::symlink_metadata(&directory).is_ok_and(|meta| meta.is_dir()),
        "Active Session writer does not advertise coherent capture support; upgrade its runtime"
    );
    let mut file = open(&directory.join(format!("{pid}.lock")), false)
        .context("Active Session writer lacks coherent capture capability")?;
    match file.try_lock_shared() {
        Err(std::fs::TryLockError::WouldBlock) => {}
        Ok(()) => anyhow::bail!(
            "Active Session writer capability is not owned by a live compatible runtime"
        ),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let mut bytes = Vec::new();
    (&mut file).take(4096).read_to_end(&mut bytes)?;
    let capability: Capability = serde_json::from_slice(&bytes)?;
    ensure!(
        capability.schema == 1 && capability.pid == pid,
        "Unsupported active Session writer capability"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn modern_snapshot_does_not_imply_its_active_writer_supports_capture() -> Result<()> {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::tempdir()?;
        let sessions = root.path().join("sessions");
        std::fs::create_dir(&sessions)?;
        let mut session = crate::session::Session::create_with_id("target".into(), None, None);
        session.persistence_identity.epoch = "synthetic-modern-epoch".into();
        let snapshot = sessions.join("target.json");
        crate::storage::write_json_secret(&snapshot, &session)?;
        let child = Child(std::process::Command::new("/bin/sleep").arg("30").spawn()?);
        let markers = root.path().join("active_pids");
        std::fs::create_dir(&markers)?;
        std::fs::write(markers.join("target"), child.0.id().to_string())?;
        assert!(crate::session::Session::capture_readonly(root.path(), "target").is_err());
        let capability = sessions
            .join(".writers")
            .join(format!("{}.lock", child.0.id()));
        crate::storage::write_json_secret(
            &capability,
            &Capability {
                schema: 1,
                pid: child.0.id(),
            },
        )?;
        assert!(
            crate::session::Session::capture_readonly(root.path(), "target").is_err(),
            "An unowned declaration is not live writer support"
        );
        std::fs::write(markers.join("target"), std::process::id().to_string())?;
        assert!(crate::session::Session::capture_readonly(root.path(), "target").is_ok());
        Ok(())
    }
}
