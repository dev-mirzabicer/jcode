use super::selection::{ResolvedStartupTarget, resolve_existing_spec};
use super::types::{
    ActiveProject, CapturedStartupFile, PreparedStartupEntry, StartupFailurePolicy,
    StartupFileIssue, StartupFileIssueKind, StartupFileSpec, StartupPreparation,
    StartupPreparationOutcome, StartupProjectPlan, StartupSelectionEntry, StartupSelectionPreview,
    StartupUnsupportedContent,
};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::Path;

type CaptureHook<'a> = Option<&'a dyn Fn(&Path, usize)>;

pub(super) fn prepare_plan(
    project: &ActiveProject,
    plan: &StartupProjectPlan,
    policy: StartupFailurePolicy,
    max_batch_bytes: u64,
    max_capture_attempts: usize,
) -> Result<StartupPreparationOutcome, super::types::StartupContextError> {
    if plan.project_key() != project.key() {
        return Err(super::types::StartupContextError::PlanProjectMismatch);
    }
    let items = plan
        .entries()
        .iter()
        .cloned()
        .map(PreparationInput::Spec)
        .collect();
    Ok(prepare_inputs(
        project,
        plan.revision(),
        items,
        Vec::new(),
        policy,
        max_batch_bytes,
        max_capture_attempts,
        None,
    ))
}

pub(super) fn prepare_preview(
    project: &ActiveProject,
    plan_revision: u64,
    preview: &StartupSelectionPreview,
    policy: StartupFailurePolicy,
    max_batch_bytes: u64,
    max_capture_attempts: usize,
) -> Result<StartupPreparationOutcome, super::types::StartupContextError> {
    if preview.project_key() != project.key() {
        return Err(super::types::StartupContextError::SelectionProjectMismatch);
    }
    let items = preview
        .entries()
        .iter()
        .map(|entry| match entry {
            StartupSelectionEntry::Selected(selected) => {
                PreparationInput::Spec(selected.spec().clone())
            }
            StartupSelectionEntry::Issue(issue) => PreparationInput::Issue(issue.clone()),
        })
        .collect();
    Ok(prepare_inputs(
        project,
        plan_revision,
        items,
        preview.batch_issues().to_vec(),
        policy,
        max_batch_bytes,
        max_capture_attempts,
        None,
    ))
}

#[cfg(test)]
pub(super) fn prepare_preview_with_hook(
    project: &ActiveProject,
    plan_revision: u64,
    preview: &StartupSelectionPreview,
    policy: StartupFailurePolicy,
    max_batch_bytes: u64,
    max_capture_attempts: usize,
    hook: &dyn Fn(&Path, usize),
) -> Result<StartupPreparationOutcome, super::types::StartupContextError> {
    if preview.project_key() != project.key() {
        return Err(super::types::StartupContextError::SelectionProjectMismatch);
    }
    let items = preview
        .entries()
        .iter()
        .map(|entry| match entry {
            StartupSelectionEntry::Selected(selected) => {
                PreparationInput::Spec(selected.spec().clone())
            }
            StartupSelectionEntry::Issue(issue) => PreparationInput::Issue(issue.clone()),
        })
        .collect();
    Ok(prepare_inputs(
        project,
        plan_revision,
        items,
        preview.batch_issues().to_vec(),
        policy,
        max_batch_bytes,
        max_capture_attempts,
        Some(hook),
    ))
}

enum PreparationInput {
    Spec(StartupFileSpec),
    Issue(StartupFileIssue),
}

enum PreparedCandidate {
    Target(ResolvedStartupTarget),
    Issue(StartupFileIssue),
}

#[allow(clippy::too_many_arguments)]
fn prepare_inputs(
    project: &ActiveProject,
    plan_revision: u64,
    inputs: Vec<PreparationInput>,
    mut batch_issues: Vec<StartupFileIssue>,
    policy: StartupFailurePolicy,
    max_batch_bytes: u64,
    max_capture_attempts: usize,
    hook: CaptureHook<'_>,
) -> StartupPreparationOutcome {
    let mut candidates = Vec::with_capacity(inputs.len());
    let mut preflight_bytes = 0u64;
    for input in inputs {
        match input {
            PreparationInput::Issue(issue) => candidates.push(PreparedCandidate::Issue(issue)),
            PreparationInput::Spec(spec) => {
                match resolve_existing_spec(project, &spec, max_batch_bytes) {
                    Ok(target) => {
                        preflight_bytes = preflight_bytes.saturating_add(target.metadata.len());
                        candidates.push(PreparedCandidate::Target(target));
                    }
                    Err(issue) => candidates.push(PreparedCandidate::Issue(issue)),
                }
            }
        }
    }

    if preflight_bytes > max_batch_bytes {
        if !batch_issues
            .iter()
            .any(|issue| matches!(issue.kind(), StartupFileIssueKind::BatchTooLarge { .. }))
        {
            batch_issues.push(StartupFileIssue::batch(
                StartupFileIssueKind::BatchTooLarge {
                    bytes: preflight_bytes,
                    limit: max_batch_bytes,
                },
            ));
        }
        let entries = candidates
            .into_iter()
            .map(|candidate| match candidate {
                PreparedCandidate::Target(target) => {
                    PreparedStartupEntry::Issue(StartupFileIssue::for_spec(
                        &target.spec,
                        StartupFileIssueKind::BatchTooLarge {
                            bytes: preflight_bytes,
                            limit: max_batch_bytes,
                        },
                    ))
                }
                PreparedCandidate::Issue(issue) => PreparedStartupEntry::Issue(issue),
            })
            .collect();
        return StartupPreparationOutcome::from_policy(
            StartupPreparation::new(project.clone(), plan_revision, entries, batch_issues),
            policy,
        );
    }

    let mut captured_bytes = 0u64;
    let mut entries = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match candidate {
            PreparedCandidate::Issue(issue) => entries.push(PreparedStartupEntry::Issue(issue)),
            PreparedCandidate::Target(target) => match capture_stable_file(
                project,
                target,
                max_batch_bytes,
                max_capture_attempts,
                hook,
            ) {
                Ok(file) => {
                    let next_bytes = captured_bytes.saturating_add(file.bytes());
                    if next_bytes > max_batch_bytes {
                        entries.push(PreparedStartupEntry::Issue(
                            StartupFileIssue::for_input(
                                entries.len(),
                                file.logical_path(),
                                StartupFileIssueKind::BatchTooLarge {
                                    bytes: next_bytes,
                                    limit: max_batch_bytes,
                                },
                            )
                            .with_spec_id(file.spec_id().clone()),
                        ));
                    } else {
                        captured_bytes = next_bytes;
                        entries.push(PreparedStartupEntry::Captured(file));
                    }
                }
                Err(issue) => entries.push(PreparedStartupEntry::Issue(issue)),
            },
        }
    }

    StartupPreparationOutcome::from_policy(
        StartupPreparation::new(project.clone(), plan_revision, entries, batch_issues),
        policy,
    )
}

fn capture_stable_file(
    project: &ActiveProject,
    initial_target: ResolvedStartupTarget,
    max_bytes: u64,
    max_capture_attempts: usize,
    hook: CaptureHook<'_>,
) -> Result<CapturedStartupFile, StartupFileIssue> {
    let spec = initial_target.spec.clone();
    let mut target = initial_target;
    let attempts = max_capture_attempts.max(1);
    for attempt in 0..attempts {
        match capture_attempt(&target, max_bytes, attempt, hook) {
            Ok(file) => return Ok(file),
            Err(issue)
                if matches!(issue.kind(), StartupFileIssueKind::ChangedDuringCapture)
                    && attempt + 1 < attempts =>
            {
                target = resolve_existing_spec(project, &spec, max_bytes)?;
            }
            Err(issue) => return Err(issue),
        }
    }
    unreachable!("capture attempt loop always returns")
}

fn capture_attempt(
    target: &ResolvedStartupTarget,
    max_bytes: u64,
    attempt: usize,
    hook: CaptureHook<'_>,
) -> Result<CapturedStartupFile, StartupFileIssue> {
    let mut file = File::open(&target.logical_path).map_err(|error| {
        StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::Unreadable {
                detail: error.to_string(),
            },
        )
    })?;
    let before = file.metadata().map_err(|error| {
        StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::Unreadable {
                detail: error.to_string(),
            },
        )
    })?;
    if !before.is_file() {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::ChangedDuringCapture,
        ));
    }
    if before.len() > max_bytes {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::FileTooLarge {
                bytes: before.len(),
                limit: max_bytes,
            },
        ));
    }

    let bounded_read = max_bytes.saturating_add(1);
    let capacity = usize::try_from(before.len().min(max_bytes)).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(bounded_read)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            StartupFileIssue::for_spec(
                &target.spec,
                StartupFileIssueKind::Unreadable {
                    detail: error.to_string(),
                },
            )
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::FileTooLarge {
                bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                limit: max_bytes,
            },
        ));
    }

    if let Some(hook) = hook {
        hook(&target.logical_path, attempt);
    }

    let after = file.metadata().map_err(|error| {
        StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::Unreadable {
                detail: error.to_string(),
            },
        )
    })?;
    let current_resolved = std::fs::canonicalize(&target.logical_path).map_err(|_| {
        StartupFileIssue::for_spec(&target.spec, StartupFileIssueKind::ChangedDuringCapture)
    })?;
    let current_metadata = std::fs::metadata(&target.logical_path).map_err(|_| {
        StartupFileIssue::for_spec(&target.spec, StartupFileIssueKind::ChangedDuringCapture)
    })?;
    let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if current_resolved != target.resolved_path
        || !same_file_identity(&before, &after)
        || !same_file_identity(&after, &current_metadata)
        || before.len() != after.len()
        || after.len() != current_metadata.len()
        || byte_len != after.len()
        || metadata_changed(&before, &after)
    {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::ChangedDuringCapture,
        ));
    }

    if bytes.starts_with(b"%PDF-") {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::UnsupportedContent {
                content: StartupUnsupportedContent::Pdf,
            },
        ));
    }
    if has_image_magic(&bytes) {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::UnsupportedContent {
                content: StartupUnsupportedContent::Image,
            },
        ));
    }
    if bytes.contains(&0) {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::UnsupportedContent {
                content: StartupUnsupportedContent::Binary,
            },
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| StartupFileIssue::for_spec(&target.spec, StartupFileIssueKind::NonUtf8))?;
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(StartupFileIssue::for_spec(
            &target.spec,
            StartupFileIssueKind::UnsupportedContent {
                content: StartupUnsupportedContent::Binary,
            },
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    let estimated_tokens = u64::try_from(crate::util::estimate_tokens(&text)).unwrap_or(u64::MAX);
    Ok(CapturedStartupFile::new(
        target.spec.id().clone(),
        target.spec.path().as_path().to_path_buf(),
        target.resolved_path.clone(),
        target.classification,
        sha256,
        byte_len,
        estimated_tokens,
        text,
    ))
}

#[cfg(unix)]
fn same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    left.len() == right.len()
        && left.created().ok() == right.created().ok()
        && left.modified().ok() == right.modified().ok()
}

#[cfg(unix)]
fn metadata_changed(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
}

#[cfg(not(unix))]
fn metadata_changed(before: &Metadata, after: &Metadata) -> bool {
    before.modified().ok() != after.modified().ok()
}

fn has_image_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"BM")
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}
