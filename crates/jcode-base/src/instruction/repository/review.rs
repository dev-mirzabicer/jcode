//! Non-mutating review of the exact tree an instruction Save would publish.
use super::git::{GitRepository, validate_operation_id};
use super::mutation::{atomic_write, fingerprint, safe_target, sha256, validate_request_paths};
use super::*;
use std::io::Read;
use std::path::Path;

impl InstructionRepositoryService {
    /// A persisted Save attempt may have written some targets before a crash.
    /// Only original or exact intended states can be resumed. Divergent newer
    /// working edits are never adopted by recovery.
    pub(super) fn review_interrupted_commit(
        &self,
        repository: &InstructionRepositoryRef,
        request: &InstructionCommitRequest,
    ) -> InstructionRepositoryResult<InstructionCommitReview> {
        let expected = request
            .expected_files
            .iter()
            .map(|file| (file.relative_path.clone(), file.fingerprint.clone()))
            .collect();
        let mut observed = request.clone();
        for file in &mut observed.expected_files {
            let actual = fingerprint(repository, &file.relative_path)?;
            if actual != *file
                && !super::mutation::path_matches_final_state(
                    &file.relative_path,
                    &actual.fingerprint,
                    &expected,
                    &request.mutations,
                )
            {
                return Err(stale(
                    repository,
                    "Interrupted Save found newer working intent. Compare and recover explicitly.",
                ));
            }
            *file = actual;
        }
        observed.mutations = request
            .mutations
            .iter()
            .map(|mutation| match mutation {
                InstructionFileMutation::Rename { from, to } => {
                    let content = read_bytes(repository, from)?
                        .or(read_bytes(repository, to)?)
                        .ok_or_else(|| {
                            stale(repository, "Interrupted rename has no recoverable source")
                        })?;
                    Ok(vec![
                        InstructionFileMutation::Delete {
                            relative_path: from.clone(),
                        },
                        InstructionFileMutation::Write {
                            relative_path: to.clone(),
                            content,
                        },
                    ])
                }
                other => Ok(vec![other.clone()]),
            })
            .collect::<InstructionRepositoryResult<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        self.review_commit(repository, &observed)
    }
    /// Capture complete working/committed/proposed files and validate the
    /// prospective Git tree without touching source, HEAD, or either index.
    /// Uncommitted dependencies cannot make an invalid commit appear valid.
    pub fn review_commit(
        &self,
        repository: &InstructionRepositoryRef,
        request: &InstructionCommitRequest,
    ) -> InstructionRepositoryResult<InstructionCommitReview> {
        self.review_commit_with(repository, request, |_| Ok(()))
            .map(|(review, ())| review)
    }

    /// The manager can render affected previews against this same private
    /// candidate, while the repository owner retains its lifetime and validity.
    pub(crate) fn review_commit_with<T>(
        &self,
        repository: &InstructionRepositoryRef,
        request: &InstructionCommitRequest,
        inspect: impl FnOnce(crate::instruction::InstructionSources) -> InstructionRepositoryResult<T>,
    ) -> InstructionRepositoryResult<(InstructionCommitReview, T)> {
        validate_operation_id(&request.operation_id)?;
        validate_request_paths(request)?;
        let git = GitRepository::new(&repository.root);
        let branch = git.branch()?;
        self.check_review_base(repository, request)?;
        let mut files = Vec::new();
        for base in &request.expected_files {
            let working = read_bytes(repository, &base.relative_path)?;
            if captured_fingerprint(working.as_deref()) != base.fingerprint {
                return Err(stale(repository, "Target changed while capturing review"));
            }
            files.push(InstructionReviewedFile {
                relative_path: base.relative_path.clone(),
                committed: git.show_file(&request.expected_head, &base.relative_path)?,
                proposed: working.clone(),
                working,
            });
        }
        for mutation in &request.mutations {
            match mutation {
                InstructionFileMutation::Write {
                    relative_path,
                    content,
                } => {
                    reviewed_file(&mut files, relative_path)?.proposed = Some(content.clone());
                }
                InstructionFileMutation::Delete { relative_path } => {
                    reviewed_file(&mut files, relative_path)?.proposed = None;
                }
                InstructionFileMutation::Rename { from, to } => {
                    if reviewed_file(&mut files, to)?.working.is_some() {
                        return Err(InstructionRepositoryError::new(
                            InstructionRepositoryErrorKind::Conflict,
                            "review rename",
                            "Rename destination already exists",
                        )
                        .repository(repository)
                        .path(to));
                    }
                    let bytes = reviewed_file(&mut files, from)?
                        .working
                        .clone()
                        .ok_or_else(|| stale(repository, "Rename source is missing"))?;
                    reviewed_file(&mut files, from)?.proposed = None;
                    reviewed_file(&mut files, to)?.proposed = Some(bytes);
                }
            }
        }
        let (snapshot, path_issues) = self.committed_validation_snapshot(repository)?;
        let before =
            self.validation_issue_set_for_root(repository, snapshot.path(), path_issues.clone())?;
        let before_default = self.review_default_issue(repository, snapshot.path())?;
        let mut candidate = repository.clone();
        candidate.root = snapshot.path().to_path_buf();
        candidate.owner_only = true;
        let mut remaining_path_issues = path_issues;
        for file in &files {
            match &file.proposed {
                Some(content) => atomic_write(&candidate, &file.relative_path, content)?,
                None => {
                    let path = safe_target(&candidate, &file.relative_path, false)?;
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(InstructionRepositoryError::new(
                                InstructionRepositoryErrorKind::Io,
                                "review deletion",
                                error.to_string(),
                            )
                            .repository(repository)
                            .path(&file.relative_path));
                        }
                    }
                }
            }
            remaining_path_issues.retain(|issue| {
                !issue.starts_with(&format!("path:{}:", file.relative_path.display()))
            });
        }
        let after =
            self.validation_issue_set_for_root(repository, snapshot.path(), remaining_path_issues)?;
        let mut errors = after.difference(&before).cloned().collect::<Vec<_>>();
        let after_default = self.review_default_issue(repository, snapshot.path())?;
        if after_default != before_default
            && let Some(error) = after_default
        {
            errors.push(error);
        }
        let inspected = inspect(self.validation_sources_for_root(repository, snapshot.path())?)?;
        self.check_review_base(repository, request)?;
        if git.branch()? != branch {
            return Err(stale(repository, "Branch changed while reviewing the edit"));
        }
        Ok((
            InstructionCommitReview {
                head: request.expected_head.clone(),
                branch,
                files,
                errors,
            },
            inspected,
        ))
    }

    fn review_default_issue(
        &self,
        repository: &InstructionRepositoryRef,
        root: &Path,
    ) -> InstructionRepositoryResult<Option<String>> {
        let mut candidate = repository.clone();
        candidate.root = root.to_path_buf();
        let Ok(manifest) = self.load_manifest(&candidate) else {
            return Ok(None);
        };
        let Some(default_agent) = manifest.default_agent else {
            return Ok(None);
        };
        let runtime = crate::instruction::InstructionRuntime::discover(
            self.validation_sources_for_root(repository, root)?,
        );
        let result = crate::instruction::InstructionSelector::parse(
            crate::instruction::InstructionKind::Agent,
            &default_agent,
        )
        .and_then(|selector| runtime.resolve(&selector));
        Ok(match result {
            Ok(document)
                if document.metadata.agent.as_ref().is_some_and(|agent| {
                    agent.availability != crate::instruction::AgentAvailability::Isolated
                }) =>
            {
                None
            }
            Ok(_) => Some(format!(
                "Default agent {default_agent} is not available for primary sessions"
            )),
            Err(error) => Some(format!("Invalid default agent {default_agent}: {error}")),
        })
    }

    fn check_review_base(
        &self,
        repository: &InstructionRepositoryRef,
        request: &InstructionCommitRequest,
    ) -> InstructionRepositoryResult<()> {
        if GitRepository::new(&repository.root).head()?.as_deref() != Some(&request.expected_head) {
            return Err(stale(
                repository,
                "Repository HEAD changed. Compare the draft with current source before retrying.",
            ));
        }
        for expected in &request.expected_files {
            if fingerprint(repository, &expected.relative_path)? != *expected {
                return Err(stale(
                    repository,
                    "Working source changed. The draft was preserved; compare and edit on top explicitly.",
                ));
            }
        }
        Ok(())
    }
}

fn reviewed_file<'a>(
    files: &'a mut [InstructionReviewedFile],
    path: &Path,
) -> InstructionRepositoryResult<&'a mut InstructionReviewedFile> {
    files
        .iter_mut()
        .find(|file| file.relative_path == path)
        .ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "review instruction edit",
                "Mutation has no captured file state",
            )
            .path(path)
        })
}

pub(super) fn read_bytes(
    repository: &InstructionRepositoryRef,
    path: &Path,
) -> InstructionRepositoryResult<Option<Vec<u8>>> {
    let target = safe_target(repository, path, false)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = options.open(&target)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Instruction source must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    match read() {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Io,
            "capture instruction file",
            error.to_string(),
        )
        .repository(repository)
        .path(path)),
    }
}

pub(super) fn captured_fingerprint(bytes: Option<&[u8]>) -> InstructionTargetFingerprint {
    match bytes {
        None => InstructionTargetFingerprint::Missing,
        Some(bytes) => InstructionTargetFingerprint::File {
            sha256: sha256(bytes),
            bytes: bytes.len() as u64,
        },
    }
}

fn stale(repository: &InstructionRepositoryRef, detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::StaleDraft,
        "review instruction edit",
        detail,
    )
    .repository(repository)
}
