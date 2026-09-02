use super::types::*;
use chrono::{Duration, Utc};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

const LEASE_DURATION_SECONDS: i64 = 300;

#[derive(Debug)]
pub(super) struct RepositoryMutationGuard {
    #[cfg(unix)]
    _file: File,
    owner_path: PathBuf,
    operation_id: String,
}

pub(super) fn acquire_mutation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    operation_id: &str,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    let paths = lease_paths(state_root, repository);
    crate::storage::ensure_dir(paths.owner.parent().unwrap_or(state_root)).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Io,
            "create mutation lease directory",
            error.to_string(),
        )
        .repository(repository)
    })?;
    let acquired_at = Utc::now();
    let owner = InstructionMutationLeaseInfo {
        operation_id: operation_id.to_string(),
        pid: std::process::id(),
        acquired_at,
        expires_at: acquired_at + Duration::seconds(LEASE_DURATION_SECONDS),
    };

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&paths.lock)
            .map_err(|error| {
                lease_io_error(repository, "open mutation lock", &paths.lock, error)
            })?;
        crate::platform::set_permissions_owner_only(&paths.lock).map_err(|error| {
            lease_io_error(repository, "secure mutation lock", &paths.lock, error)
        })?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(busy_error(repository, read_owner(&paths.owner)));
        }
        set_close_on_exec(&file).map_err(|error| {
            lease_io_error(repository, "secure mutation lock", &paths.lock, error)
        })?;
        crate::storage::write_json_secret(&paths.owner, &owner).map_err(|error| {
            lease_io_error(
                repository,
                "write mutation owner",
                &paths.owner,
                std::io::Error::other(error.to_string()),
            )
        })?;
        Ok(RepositoryMutationGuard {
            _file: file,
            owner_path: paths.owner,
            operation_id: operation_id.to_string(),
        })
    }

    #[cfg(not(unix))]
    {
        if let Some(existing) = read_owner(&paths.owner) {
            if existing.expires_at > Utc::now() {
                return Err(busy_error(repository, Some(existing)));
            }
            let _ = std::fs::remove_file(&paths.owner);
        } else if paths.owner.exists() {
            let _ = std::fs::remove_file(&paths.owner);
        }
        let bytes = serde_json::to_vec(&owner).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "serialize mutation owner",
                error.to_string(),
            )
            .repository(repository)
        })?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&paths.owner).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                busy_error(repository, read_owner(&paths.owner))
            } else {
                lease_io_error(repository, "create mutation owner", &paths.owner, error)
            }
        })?;
        use std::io::Write;
        file.write_all(&bytes).map_err(|error| {
            lease_io_error(repository, "write mutation owner", &paths.owner, error)
        })?;
        crate::platform::set_permissions_owner_only(&paths.owner).map_err(|error| {
            lease_io_error(repository, "secure mutation owner", &paths.owner, error)
        })?;
        Ok(RepositoryMutationGuard {
            owner_path: paths.owner,
            operation_id: operation_id.to_string(),
        })
    }
}

pub(super) fn active_mutation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
) -> Option<InstructionMutationLeaseInfo> {
    let paths = lease_paths(state_root, repository);

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if !paths.lock.exists() {
            return None;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(false)
            .open(&paths.lock)
            .ok()?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            None
        } else {
            read_owner(&paths.owner)
        }
    }

    #[cfg(not(unix))]
    {
        read_owner(&paths.owner).filter(|owner| owner.expires_at > Utc::now())
    }
}

impl Drop for RepositoryMutationGuard {
    fn drop(&mut self) {
        let matches = read_owner(&self.owner_path)
            .is_some_and(|owner| owner.operation_id == self.operation_id);
        if matches {
            let _ = std::fs::remove_file(&self.owner_path);
        }
    }
}

struct LeasePaths {
    lock: PathBuf,
    owner: PathBuf,
}

fn lease_paths(state_root: &Path, repository: &InstructionRepositoryRef) -> LeasePaths {
    let directory = state_root
        .join("instruction-repositories")
        .join("mutation-leases");
    LeasePaths {
        lock: directory.join(format!("{}.lock", repository.id)),
        owner: directory.join(format!("{}.owner.json", repository.id)),
    }
}

fn read_owner(path: &Path) -> Option<InstructionMutationLeaseInfo> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn busy_error(
    repository: &InstructionRepositoryRef,
    owner: Option<InstructionMutationLeaseInfo>,
) -> InstructionRepositoryError {
    let detail = owner.map_or_else(
        || "another process owns the repository mutation lease".to_string(),
        |owner| {
            format!(
                "operation '{}' in process {} owns the mutation lease until {}",
                owner.operation_id, owner.pid, owner.expires_at
            )
        },
    );
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::MutationBusy,
        "acquire mutation lease",
        detail,
    )
    .repository(repository)
}

fn lease_io_error(
    repository: &InstructionRepositoryRef,
    operation: &str,
    path: &Path,
    error: std::io::Error,
) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Io,
        operation,
        error.to_string(),
    )
    .repository(repository)
    .path(path)
}

#[cfg(unix)]
fn set_close_on_exec(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
