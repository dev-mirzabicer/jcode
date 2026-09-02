use super::git::{GitRepository, validate_operation_id};
use super::lease::acquire_mutation_lease;
use super::types::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub(super) fn validate_relative_path(path: &Path) -> InstructionRepositoryResult<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(invalid_path(
            path,
            "path must be a nonempty repository-relative path",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::InvalidUtf8,
                        "validate managed path",
                        "managed instruction paths must be valid UTF-8",
                    )
                    .path(path)
                })?;
                if value == ".git" || value.contains(['\0', '\n', '\r', ':']) {
                    return Err(invalid_path(
                        path,
                        "path contains a reserved or Git-ambiguous component",
                    ));
                }
            }
            _ => return Err(invalid_path(path, "path traversal is not allowed")),
        }
    }
    Ok(())
}

pub(super) fn fingerprint(
    repository: &InstructionRepositoryRef,
    relative_path: &Path,
) -> InstructionRepositoryResult<InstructionFileState> {
    let target = safe_target(repository, relative_path, false)?;
    let fingerprint = match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => InstructionTargetFingerprint::Symlink,
        Ok(metadata) if metadata.is_file() => {
            let bytes = std::fs::read(&target)
                .map_err(|error| io_error(repository, "read target fingerprint", &target, error))?;
            InstructionTargetFingerprint::File {
                sha256: sha256(&bytes),
                bytes: bytes.len() as u64,
            }
        }
        Ok(_) => InstructionTargetFingerprint::Other,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            InstructionTargetFingerprint::Missing
        }
        Err(error) => {
            return Err(io_error(
                repository,
                "inspect target fingerprint",
                &target,
                error,
            ));
        }
    };
    Ok(InstructionFileState {
        relative_path: relative_path.to_path_buf(),
        fingerprint,
    })
}

pub(super) fn read_working_utf8(
    repository: &InstructionRepositoryRef,
    relative_path: &Path,
) -> InstructionRepositoryResult<Option<String>> {
    let target = safe_target(repository, relative_path, false)?;
    match std::fs::read(&target) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidUtf8,
                "read working instruction file",
                error.to_string(),
            )
            .repository(repository)
            .path(&target)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(
            repository,
            "read working instruction file",
            &target,
            error,
        )),
    }
}

pub(super) fn atomic_write(
    repository: &InstructionRepositoryRef,
    relative_path: &Path,
    bytes: &[u8],
) -> InstructionRepositoryResult<()> {
    let target = safe_target(repository, relative_path, true)?;
    atomic_write_path(&target, bytes, repository.owner_only)
        .map_err(|error| io_error(repository, "write managed instruction file", &target, error))
}

pub(super) fn atomic_write_path(
    target: &Path,
    bytes: &[u8],
    owner_only: bool,
) -> std::io::Result<()> {
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    if owner_only {
        crate::platform::set_directory_permissions_owner_only(parent)?;
    }
    let nonce: u64 = rand::random();
    let temporary = parent.join(format!(
        ".{}.jcode-tmp-{}-{nonce}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("instruction"),
        std::process::id()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temporary)?;
        if owner_only {
            crate::platform::set_permissions_owner_only(&temporary)?;
        } else {
            set_managed_file_permissions(&temporary, target)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, target)?;
        if owner_only {
            crate::platform::set_permissions_owner_only(target)?;
        }
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn commit_request<F>(
    state_root: &Path,
    repository: &InstructionRepositoryRef,
    request: &InstructionCommitRequest,
    validate_after_write: F,
) -> InstructionRepositoryResult<InstructionCommitOutcome>
where
    F: FnOnce() -> InstructionRepositoryResult<()>,
{
    validate_operation_id(&request.operation_id)?;
    let git = GitRepository::new(&repository.root);
    if let Some(commit) = git.find_operation_commit(&request.operation_id)? {
        let changed_paths = affected_paths(&request.mutations);
        git.refresh_index_paths(&changed_paths)?;
        return Ok(InstructionCommitOutcome {
            disposition: InstructionCommitDisposition::AlreadyCommitted,
            commit,
            changed_paths,
        });
    }
    let _lease = acquire_mutation_lease(state_root, repository, &request.operation_id)?;
    if let Some(commit) = git.find_operation_commit(&request.operation_id)? {
        let changed_paths = affected_paths(&request.mutations);
        git.refresh_index_paths(&changed_paths)?;
        return Ok(InstructionCommitOutcome {
            disposition: InstructionCommitDisposition::AlreadyCommitted,
            commit,
            changed_paths,
        });
    }
    let branch = git.branch()?;
    if branch.is_none() {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::DetachedHead,
            "commit managed instruction edit",
            "Save is unavailable while the instruction repository is detached",
        )
        .repository(repository));
    }
    let current_head = git.head()?.ok_or_else(|| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::RepositoryDamaged,
            "commit managed instruction edit",
            "instruction repository has no current HEAD",
        )
        .repository(repository)
    })?;
    if current_head != request.expected_head {
        return Err(stale_error(
            repository,
            format!(
                "repository HEAD changed from {} to {current_head}",
                request.expected_head
            ),
        ));
    }
    validate_request_paths(request)?;
    let expected_by_path = request
        .expected_files
        .iter()
        .map(|state| (state.relative_path.clone(), state.fingerprint.clone()))
        .collect::<BTreeMap<_, _>>();
    for expected in &request.expected_files {
        let actual = fingerprint(repository, &expected.relative_path)?;
        if actual != *expected
            && !path_matches_final_state(
                &expected.relative_path,
                &actual.fingerprint,
                &expected_by_path,
                &request.mutations,
            )
        {
            return Err(stale_error(
                repository,
                format!(
                    "target {} changed after the draft was opened",
                    expected.relative_path.display()
                ),
            ));
        }
    }

    apply_mutations(repository, &request.mutations, &expected_by_path).map_err(|error| {
        if error.existing_state_unchanged {
            error.may_have_working_changes()
        } else {
            error
        }
    })?;
    validate_after_write().map_err(InstructionRepositoryError::may_have_working_changes)?;
    let paths = affected_paths(&request.mutations);
    let index_dir = state_root
        .join("instruction-repositories")
        .join("isolated-indexes")
        .join(&repository.id);
    crate::storage::ensure_dir(&index_dir).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Io,
            "create isolated index directory",
            error.to_string(),
        )
        .repository(repository)
        .may_have_working_changes()
    })?;
    let index_path = index_dir.join(format!("{}.index", request.operation_id));
    let outcome = git.commit_paths(
        &index_path,
        &paths,
        &request.message,
        &request.operation_id,
        &request.expected_head,
    );
    let _ = std::fs::remove_file(&index_path);
    let _ = std::fs::remove_file(index_path.with_extension("index.lock"));
    match outcome {
        Ok(Some(commit)) => Ok(InstructionCommitOutcome {
            disposition: InstructionCommitDisposition::Created,
            commit,
            changed_paths: paths,
        }),
        Ok(None) => Ok(InstructionCommitOutcome {
            disposition: InstructionCommitDisposition::NoChange,
            commit: current_head,
            changed_paths: Vec::new(),
        }),
        Err(error) => Err(error.repository(repository).may_have_working_changes()),
    }
}

fn validate_request_paths(request: &InstructionCommitRequest) -> InstructionRepositoryResult<()> {
    if request.mutations.is_empty() {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate instruction commit",
            "commit request has no file mutations",
        ));
    }
    let affected = affected_paths(&request.mutations)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let expected = request
        .expected_files
        .iter()
        .map(|state| state.relative_path.clone())
        .collect::<BTreeSet<_>>();
    if affected != expected {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate instruction commit",
            "expected file states must cover every affected path exactly",
        ));
    }
    let mut writes = BTreeSet::new();
    for mutation in &request.mutations {
        for path in mutation.affected_paths() {
            validate_relative_path(path)?;
            if !writes.insert(path.clone()) {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "validate instruction commit",
                    format!("path {} appears in more than one mutation", path.display()),
                ));
            }
        }
    }
    Ok(())
}

fn apply_mutations(
    repository: &InstructionRepositoryRef,
    mutations: &[InstructionFileMutation],
    expected_by_path: &BTreeMap<PathBuf, InstructionTargetFingerprint>,
) -> InstructionRepositoryResult<()> {
    for mutation in mutations {
        match mutation {
            InstructionFileMutation::Write {
                relative_path,
                content,
            } => atomic_write(repository, relative_path, content)?,
            InstructionFileMutation::Delete { relative_path } => {
                let target = safe_target(repository, relative_path, false)?;
                match std::fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(io_error(
                            repository,
                            "delete managed instruction file",
                            &target,
                            error,
                        ));
                    }
                }
            }
            InstructionFileMutation::Rename { from, to } => {
                let source = safe_target(repository, from, false)?;
                let target = safe_target(repository, to, true)?;
                if !source.exists()
                    && target.exists()
                    && expected_by_path.get(from).is_some_and(|expected| {
                        fingerprint(repository, to)
                            .is_ok_and(|actual| actual.fingerprint == *expected)
                    })
                {
                    continue;
                }
                if target.exists() {
                    return Err(InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Conflict,
                        "rename managed instruction file",
                        "rename target already exists",
                    )
                    .repository(repository)
                    .path(&target));
                }
                std::fs::rename(&source, &target).map_err(|error| {
                    io_error(
                        repository,
                        "rename managed instruction file",
                        &source,
                        error,
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn path_matches_final_state(
    path: &Path,
    actual: &InstructionTargetFingerprint,
    expected_by_path: &BTreeMap<PathBuf, InstructionTargetFingerprint>,
    mutations: &[InstructionFileMutation],
) -> bool {
    mutations.iter().any(|mutation| match mutation {
        InstructionFileMutation::Write {
            relative_path,
            content,
        } if relative_path == path => {
            actual
                == &InstructionTargetFingerprint::File {
                    sha256: sha256(content),
                    bytes: content.len() as u64,
                }
        }
        InstructionFileMutation::Delete { relative_path } if relative_path == path => {
            actual == &InstructionTargetFingerprint::Missing
        }
        InstructionFileMutation::Rename { from, to } if from == path => {
            actual == &InstructionTargetFingerprint::Missing
        }
        InstructionFileMutation::Rename { from, to } if to == path => expected_by_path
            .get(from)
            .is_some_and(|source| source == actual),
        _ => false,
    })
}

pub(super) fn affected_paths(mutations: &[InstructionFileMutation]) -> Vec<PathBuf> {
    let mut paths = BTreeMap::<PathBuf, ()>::new();
    for mutation in mutations {
        for path in mutation.affected_paths() {
            paths.insert(path.clone(), ());
        }
    }
    paths.into_keys().collect()
}

pub(super) fn safe_target(
    repository: &InstructionRepositoryRef,
    relative_path: &Path,
    create_parents: bool,
) -> InstructionRepositoryResult<PathBuf> {
    validate_relative_path(relative_path)?;
    let root_metadata = std::fs::symlink_metadata(&repository.root).map_err(|error| {
        io_error(
            repository,
            "inspect instruction repository root",
            &repository.root,
            error,
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::SymlinkEscape,
            "resolve managed instruction path",
            "repository root must be a real directory, not a symlink",
        )
        .repository(repository)
        .path(&repository.root));
    }
    let mut current = repository.root.clone();
    let components = relative_path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(invalid_path(relative_path, "path traversal is not allowed"));
        };
        current.push(component);
        let is_target = index + 1 == components.len();
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::SymlinkEscape,
                    "resolve managed instruction path",
                    "managed path crosses a symlink",
                )
                .repository(repository)
                .path(&current));
            }
            Ok(metadata) if !is_target && !metadata.is_dir() => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::InvalidPath,
                    "resolve managed instruction path",
                    "managed path crosses a non-directory component",
                )
                .repository(repository)
                .path(&current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create_parents && !is_target {
                    std::fs::create_dir(&current).map_err(|error| {
                        io_error(
                            repository,
                            "create managed instruction directory",
                            &current,
                            error,
                        )
                    })?;
                    if repository.owner_only {
                        crate::platform::set_directory_permissions_owner_only(&current).map_err(
                            |error| {
                                io_error(
                                    repository,
                                    "secure managed instruction directory",
                                    &current,
                                    error,
                                )
                            },
                        )?;
                    }
                }
            }
            Err(error) => {
                return Err(io_error(
                    repository,
                    "inspect managed instruction path",
                    &current,
                    error,
                ));
            }
        }
    }
    Ok(current)
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn stale_error(
    repository: &InstructionRepositoryRef,
    detail: String,
) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::StaleDraft,
        "validate draft base",
        detail,
    )
    .repository(repository)
}

fn invalid_path(path: &Path, detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::InvalidPath,
        "validate managed path",
        detail,
    )
    .path(path)
}

fn io_error(
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
fn set_managed_file_permissions(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::metadata(target)
        .map(|metadata| metadata.permissions())
        .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o644));
    std::fs::set_permissions(temporary, permissions)
}

#[cfg(not(unix))]
fn set_managed_file_permissions(_temporary: &Path, _target: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, target)
}

#[cfg(not(unix))]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() {
        std::fs::remove_file(target)?;
    }
    std::fs::rename(temporary, target)
}
