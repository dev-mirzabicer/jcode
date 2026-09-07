use super::git::GitRepository;
use super::types::*;
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

const LEASE_DURATION_SECONDS: i64 = 300;

#[derive(Debug)]
pub(super) struct RepositoryMutationGuard {
    #[cfg(any(unix, windows))]
    _file: File,
    owner_path: PathBuf,
    operation_id: String,
}

#[derive(Clone, Copy)]
enum LeaseKind<'a> {
    Mutation,
    Setup,
    Draft(&'a str),
    Operation(&'a str),
    GitDirectory(&'a Path),
}

pub(super) fn acquire_mutation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    operation_id: &str,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    acquire_lease(state_root, repository, operation_id, LeaseKind::Mutation)
}
pub(super) fn acquire_setup_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    operation_id: &str,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    acquire_lease(state_root, repository, operation_id, LeaseKind::Setup)
}
pub(super) fn acquire_draft_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    id: &str,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    acquire_lease(state_root, repository, id, LeaseKind::Draft(id))
}
pub(super) fn acquire_operation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    id: &str,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    acquire_lease(state_root, repository, id, LeaseKind::Operation(id))
}
pub(super) fn acquire_recovery_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    id: &str,
    common: &Path,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    acquire_lease(state_root, repository, id, LeaseKind::GitDirectory(common))
}
fn acquire_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    operation_id: &str,
    kind: LeaseKind<'_>,
) -> InstructionRepositoryResult<RepositoryMutationGuard> {
    let paths = lease_paths(state_root, repository, kind)?;
    reject_symlink_components(&paths.owner)?;
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

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&paths.lock)
            .map_err(|error| {
                if error.raw_os_error() == Some(32) {
                    busy_error(repository, read_owner(&paths.owner))
                } else {
                    lease_io_error(repository, "open mutation lock", &paths.lock, error)
                }
            })?;
        crate::platform::set_permissions_owner_only(&paths.lock).map_err(|error| {
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

    #[cfg(not(any(unix, windows)))]
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

pub(super) fn active_operation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    id: &str,
) -> Option<InstructionMutationLeaseInfo> {
    active_lease(state_root, repository, LeaseKind::Operation(id))
}

pub(super) fn active_mutation_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
) -> Option<InstructionMutationLeaseInfo> {
    active_lease(state_root, repository, LeaseKind::Mutation)
}
fn active_lease(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    kind: LeaseKind<'_>,
) -> Option<InstructionMutationLeaseInfo> {
    let paths = lease_paths(state_root, repository, kind).ok()?;
    active_lease_paths(paths)
}

fn active_lease_paths(paths: LeasePaths) -> Option<InstructionMutationLeaseInfo> {
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

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        if !paths.lock.exists() {
            return None;
        }
        match OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&paths.lock)
        {
            Ok(_) => None,
            Err(_) => read_owner(&paths.owner),
        }
    }

    #[cfg(not(any(unix, windows)))]
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

fn lease_paths(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    kind: LeaseKind<'_>,
) -> InstructionRepositoryResult<LeasePaths> {
    let git = GitRepository::new(&repository.root);
    let (directory, name) = match (kind, git.common_directory()) {
        (LeaseKind::GitDirectory(common), _) => {
            (common.join("jcode-instruction-leases"), "mutation".into())
        }
        (LeaseKind::Operation(id), _) => {
            uuid::Uuid::parse_str(id).map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "operation identity",
                    error.to_string(),
                )
            })?;
            (
                canonical_prefix(state_root).join("instruction-repositories/operation-leases"),
                id.to_string(),
            )
        }
        (LeaseKind::Mutation, Some(common)) => (
            common.join("jcode-instruction-leases"),
            "mutation".to_string(),
        ),
        (LeaseKind::Draft(id), Some(common)) => {
            uuid::Uuid::parse_str(id).map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "draft lease identity",
                    error.to_string(),
                )
            })?;
            (
                common.join("jcode-instruction-leases"),
                format!("draft-{id}"),
            )
        }
        (LeaseKind::Draft(_), None) => {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "acquire draft lease",
                "Drafts require their own initialized Git repository",
            )
            .repository(repository));
        }
        (kind, _) => {
            let anchor = if let Some(project) = &repository.project_root {
                let project_git = GitRepository::new(project);
                project_git
                    .common_directory()
                    .map(|common| common.join("jcode-instruction-setups"))
                    .unwrap_or_else(|| canonical_prefix(project).join(".jcode/instruction-setups"))
            } else {
                canonical_prefix(repository.root.parent().unwrap_or(state_root))
                    .join("state/instruction-setups")
            };
            let identity_root = if matches!(kind, LeaseKind::Setup) {
                repository
                    .project_root
                    .as_deref()
                    .unwrap_or(&repository.root)
            } else {
                &repository.root
            };
            let identity = format!(
                "{:x}",
                Sha256::digest(canonical_prefix(identity_root).to_string_lossy().as_bytes())
            );
            (
                anchor,
                format!(
                    "{}-{identity}",
                    if matches!(kind, LeaseKind::Setup) {
                        "setup"
                    } else {
                        "bootstrap"
                    }
                ),
            )
        }
    };
    Ok(LeasePaths {
        lock: directory.join(format!("{name}.lock")),
        owner: directory.join(format!("{name}.owner.json")),
    })
}

pub(super) fn active_drafts(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
) -> InstructionRepositoryResult<Vec<InstructionMutationLeaseInfo>> {
    let paths = lease_paths(state_root, repository, LeaseKind::Mutation)?;
    let Some(directory) = paths.lock.parent() else {
        return Ok(Vec::new());
    };
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(lease_io_error(
                repository,
                "inspect active drafts",
                directory,
                error,
            ));
        }
    };
    let mut owners = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| lease_io_error(repository, "inspect draft lease", directory, error))?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix("draft-"))
            .and_then(|name| name.strip_suffix(".owner.json"))
        else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_ok()
            && let Some(owner) = active_lease(state_root, repository, LeaseKind::Draft(id))
        {
            owners.push(owner);
        }
    }
    Ok(owners)
}

pub(super) fn require_no_drafts_in_git_directory(common: &Path) -> InstructionRepositoryResult<()> {
    let directory = common.join("jcode-instruction-leases");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "inspect recovery leases",
                error.to_string(),
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "inspect recovery lease",
                error.to_string(),
            )
        })?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix("draft-"))
            .and_then(|name| name.strip_suffix(".owner.json"))
        else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_ok()
            && active_lease_paths(LeasePaths {
                lock: directory.join(format!("draft-{id}.lock")),
                owner: entry.path(),
            })
            .is_some()
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::MutationBusy,
                "repair checkout",
                "Close the existing draft before restoring its missing checkout",
            ));
        }
    }
    Ok(())
}

fn canonical_prefix(path: &Path) -> PathBuf {
    let mut cursor = path;
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut canonical) = cursor.canonicalize() {
            for name in suffix.into_iter().rev() {
                canonical.push(name);
            }
            return canonical;
        }
        let Some(parent) = cursor.parent() else {
            return path.to_path_buf();
        };
        if let Some(name) = cursor.file_name() {
            suffix.push(name.to_os_string());
        }
        cursor = parent;
    }
}

fn reject_symlink_components(path: &Path) -> InstructionRepositoryResult<()> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component.as_os_str());
        match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::SymlinkEscape,
                    "open repository lease",
                    "Lease path crosses a symlink",
                )
                .path(&cursor));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "inspect repository lease",
                    error.to_string(),
                )
                .path(&cursor));
            }
        }
    }
    Ok(())
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
                "operation '{}' in process {} owns the mutation lease (metadata deadline {}; kernel lock is authoritative on Unix and Windows)",
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
