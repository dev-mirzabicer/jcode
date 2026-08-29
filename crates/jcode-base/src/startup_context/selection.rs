use super::types::{
    ActiveProject, ExternalTargetApproval, SelectedStartupPath, StartupFileIssue,
    StartupFileIssueKind, StartupFileSpec, StartupFileSpecId, StartupPathClassification,
    StartupSelectionEntry, StartupSelectionInput, StartupSelectionPreview, StartupTargetType,
    StartupUnsupportedContent, ValidatedStartupSelection, reject_parent_components,
    validate_relative_selected_path,
};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct ResolvedStartupTarget {
    pub(super) spec: StartupFileSpec,
    pub(super) logical_path: PathBuf,
    pub(super) resolved_path: PathBuf,
    pub(super) classification: StartupPathClassification,
    pub(super) metadata: Metadata,
}

pub(super) fn preview_selection(
    project: &ActiveProject,
    inputs: Vec<StartupSelectionInput>,
    max_entries: usize,
    max_batch_bytes: u64,
) -> StartupSelectionPreview {
    if inputs.len() > max_entries {
        return StartupSelectionPreview::new(
            project.key().clone(),
            Vec::new(),
            vec![StartupFileIssue::batch(
                StartupFileIssueKind::TooManyEntries {
                    count: inputs.len(),
                    limit: max_entries,
                },
            )],
        );
    }

    let mut entries = Vec::with_capacity(inputs.len());
    let mut first_resolved_index = HashMap::<PathBuf, usize>::new();
    let mut total_bytes = 0u64;

    for (input_index, input) in inputs.into_iter().enumerate() {
        let input_path = input.path().to_path_buf();
        match normalize_input(project, input_index, input, max_batch_bytes) {
            Ok(target) => {
                if let Some(first_input_index) =
                    first_resolved_index.insert(target.resolved_path.clone(), input_index)
                {
                    entries.push(StartupSelectionEntry::Issue(
                        StartupFileIssue::for_input(
                            input_index,
                            input_path,
                            StartupFileIssueKind::DuplicateSelection { first_input_index },
                        )
                        .with_spec_id(target.spec.id().clone()),
                    ));
                    continue;
                }
                total_bytes = total_bytes.saturating_add(target.metadata.len());
                entries.push(StartupSelectionEntry::Selected(
                    ValidatedStartupSelection::new(
                        input_index,
                        target.spec,
                        target.resolved_path,
                        target.classification,
                        target.metadata.len(),
                    ),
                ));
            }
            Err(issue) => entries.push(StartupSelectionEntry::Issue(issue)),
        }
    }

    let batch_issues = if total_bytes > max_batch_bytes {
        vec![StartupFileIssue::batch(
            StartupFileIssueKind::BatchTooLarge {
                bytes: total_bytes,
                limit: max_batch_bytes,
            },
        )]
    } else {
        Vec::new()
    };
    StartupSelectionPreview::new(project.key().clone(), entries, batch_issues)
}

pub(super) fn expand_directory(
    project: &ActiveProject,
    directory: &Path,
    max_entries: usize,
    max_batch_bytes: u64,
) -> StartupSelectionPreview {
    let directory_path = match clean_selected_input_path(project, directory) {
        Ok(path) => path,
        Err(kind) => {
            return StartupSelectionPreview::new(
                project.key().clone(),
                Vec::new(),
                vec![StartupFileIssue::batch(kind)],
            );
        }
    };
    let resolved = match canonicalize_selected_path(&directory_path) {
        Ok(path) => path,
        Err(kind) => {
            return StartupSelectionPreview::new(
                project.key().clone(),
                Vec::new(),
                vec![StartupFileIssue::batch(kind)],
            );
        }
    };
    if !resolved.starts_with(project.active_root()) {
        return StartupSelectionPreview::new(
            project.key().clone(),
            Vec::new(),
            vec![StartupFileIssue::batch(
                StartupFileIssueKind::DirectoryOutsideProject,
            )],
        );
    }
    if !resolved.is_dir() {
        return StartupSelectionPreview::new(
            project.key().clone(),
            Vec::new(),
            vec![StartupFileIssue::batch(
                StartupFileIssueKind::UnsupportedTarget {
                    target_type: StartupTargetType::DeviceOrSpecial,
                },
            )],
        );
    }

    let read_dir = match std::fs::read_dir(&resolved) {
        Ok(entries) => entries,
        Err(error) => {
            return StartupSelectionPreview::new(
                project.key().clone(),
                Vec::new(),
                vec![StartupFileIssue::batch(
                    StartupFileIssueKind::DirectoryReadFailed {
                        detail: error.to_string(),
                    },
                )],
            );
        }
    };
    let mut children = Vec::new();
    for child in read_dir {
        match child {
            Ok(child) => children.push(child),
            Err(error) => {
                return StartupSelectionPreview::new(
                    project.key().clone(),
                    Vec::new(),
                    vec![StartupFileIssue::batch(
                        StartupFileIssueKind::DirectoryReadFailed {
                            detail: error.to_string(),
                        },
                    )],
                );
            }
        }
    }
    children.sort_by(|left, right| natural_os_cmp(&left.file_name(), &right.file_name()));

    let inputs = children
        .into_iter()
        .filter_map(|child| match child.file_type() {
            Ok(file_type) if file_type.is_dir() => None,
            Ok(_) => Some(StartupSelectionInput::new(child.path())),
            Err(_) => Some(StartupSelectionInput::new(child.path())),
        })
        .collect();
    preview_selection(project, inputs, max_entries, max_batch_bytes)
}

pub(super) fn resolve_existing_spec(
    project: &ActiveProject,
    spec: &StartupFileSpec,
    max_batch_bytes: u64,
) -> Result<ResolvedStartupTarget, StartupFileIssue> {
    let logical_absolute = match spec.path() {
        SelectedStartupPath::ProjectRelative(path) => project.active_root().join(path),
        SelectedStartupPath::ExternalAbsolute(path) => path.clone(),
    };
    resolve_target(
        project,
        spec.clone(),
        logical_absolute,
        spec.external_approval()
            .map(|approval| approval.approved_resolved_target()),
        max_batch_bytes,
        None,
    )
}

fn normalize_input(
    project: &ActiveProject,
    input_index: usize,
    input: StartupSelectionInput,
    max_batch_bytes: u64,
) -> Result<ResolvedStartupTarget, StartupFileIssue> {
    let input_path = input.path().to_path_buf();
    let logical_absolute = clean_selected_input_path(project, input.path())
        .map_err(|kind| StartupFileIssue::for_input(input_index, input_path.clone(), kind))?;
    let resolved_path = canonicalize_selected_path(&logical_absolute)
        .map_err(|kind| StartupFileIssue::for_input(input_index, input_path.clone(), kind))?;

    let input_inside_project = logical_absolute.strip_prefix(project.active_root()).ok();
    let resolved_inside_project = resolved_path.strip_prefix(project.active_root()).ok();
    let selected_path = if let Some(relative) = input_inside_project {
        SelectedStartupPath::ProjectRelative(relative.to_path_buf())
    } else if let Some(relative) = resolved_inside_project {
        SelectedStartupPath::ProjectRelative(relative.to_path_buf())
    } else {
        SelectedStartupPath::ExternalAbsolute(logical_absolute.clone())
    };
    let classification = if resolved_inside_project.is_some() {
        StartupPathClassification::Project
    } else {
        StartupPathClassification::External
    };
    let external_approval = normalize_external_approval(
        input.approved_external_target(),
        &resolved_path,
        classification,
    )
    .map_err(|kind| StartupFileIssue::for_input(input_index, input_path.clone(), kind))?;
    let id = input
        .existing_id()
        .cloned()
        .unwrap_or_else(|| spec_id(project, &selected_path));
    let spec = StartupFileSpec::new(id, selected_path, external_approval);

    resolve_target(
        project,
        spec,
        logical_absolute,
        input.approved_external_target(),
        max_batch_bytes,
        Some(input_index),
    )
}

fn resolve_target(
    project: &ActiveProject,
    spec: StartupFileSpec,
    logical_absolute: PathBuf,
    approved_external_target: Option<&Path>,
    max_batch_bytes: u64,
    input_index: Option<usize>,
) -> Result<ResolvedStartupTarget, StartupFileIssue> {
    let resolved_path = canonicalize_selected_path(&logical_absolute)
        .map_err(|kind| issue_for(&spec, input_index, kind))?;
    if resolved_path.to_str().is_none() {
        return Err(issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::InvalidPathEncoding,
        ));
    }
    let classification = if resolved_path.starts_with(project.active_root()) {
        StartupPathClassification::Project
    } else {
        StartupPathClassification::External
    };
    if classification == StartupPathClassification::External {
        let approved = approved_external_target
            .or_else(|| {
                spec.external_approval()
                    .map(|approval| approval.approved_resolved_target())
            })
            .ok_or_else(|| {
                issue_for(
                    &spec,
                    input_index,
                    StartupFileIssueKind::ExternalApprovalRequired {
                        resolved_target: resolved_path.clone(),
                    },
                )
            })?;
        let approved = canonicalize_approval(approved).map_err(|detail| {
            issue_for(
                &spec,
                input_index,
                StartupFileIssueKind::InvalidExternalApproval { detail },
            )
        })?;
        if approved != resolved_path {
            return Err(issue_for(
                &spec,
                input_index,
                StartupFileIssueKind::ExternalTargetChanged {
                    approved_target: approved,
                    resolved_target: resolved_path,
                },
            ));
        }
    }

    if let Some(content) = unsupported_extension(&logical_absolute) {
        return Err(issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::UnsupportedContent { content },
        ));
    }

    let symlink = std::fs::symlink_metadata(&logical_absolute)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink());
    let file = File::open(&logical_absolute).map_err(|error| {
        issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::Unreadable {
                detail: error.to_string(),
            },
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::Unreadable {
                detail: error.to_string(),
            },
        )
    })?;
    if !metadata.is_file() {
        let target_type = if metadata.is_dir() {
            if symlink {
                StartupTargetType::SymlinkToDirectory
            } else {
                StartupTargetType::Directory
            }
        } else {
            StartupTargetType::DeviceOrSpecial
        };
        return Err(issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::UnsupportedTarget { target_type },
        ));
    }
    if metadata.len() > max_batch_bytes {
        return Err(issue_for(
            &spec,
            input_index,
            StartupFileIssueKind::FileTooLarge {
                bytes: metadata.len(),
                limit: max_batch_bytes,
            },
        ));
    }

    Ok(ResolvedStartupTarget {
        spec,
        logical_path: logical_absolute,
        resolved_path,
        classification,
        metadata,
    })
}

fn clean_selected_input_path(
    project: &ActiveProject,
    path: &Path,
) -> Result<PathBuf, StartupFileIssueKind> {
    if path.as_os_str().is_empty() {
        return Err(StartupFileIssueKind::EmptyPath);
    }
    if path.to_str().is_none() {
        return Err(StartupFileIssueKind::InvalidPathEncoding);
    }
    reject_parent_components(path).map_err(|_| StartupFileIssueKind::PathTraversal)?;
    if path.is_absolute() {
        clean_components(path)
    } else {
        validate_relative_selected_path(path).map_err(|detail| {
            if detail.contains("UTF-8") {
                StartupFileIssueKind::InvalidPathEncoding
            } else if detail.contains("parent") {
                StartupFileIssueKind::PathTraversal
            } else {
                StartupFileIssueKind::EmptyPath
            }
        })?;
        clean_components(&project.active_root().join(path))
    }
}

fn clean_components(path: &Path) -> Result<PathBuf, StartupFileIssueKind> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return Err(StartupFileIssueKind::PathTraversal),
            other => clean.push(other.as_os_str()),
        }
    }
    Ok(clean)
}

fn canonicalize_selected_path(path: &Path) -> Result<PathBuf, StartupFileIssueKind> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
            {
                Err(StartupFileIssueKind::BrokenSymlink)
            } else {
                Err(StartupFileIssueKind::Missing)
            }
        }
        Err(error) => Err(StartupFileIssueKind::Unreadable {
            detail: error.to_string(),
        }),
    }
}

fn normalize_external_approval(
    approval: Option<&Path>,
    resolved_path: &Path,
    classification: StartupPathClassification,
) -> Result<Option<ExternalTargetApproval>, StartupFileIssueKind> {
    if classification == StartupPathClassification::Project {
        return Ok(None);
    }
    let approval = approval.ok_or_else(|| StartupFileIssueKind::ExternalApprovalRequired {
        resolved_target: resolved_path.to_path_buf(),
    })?;
    let approval = canonicalize_approval(approval)
        .map_err(|detail| StartupFileIssueKind::InvalidExternalApproval { detail })?;
    if approval != resolved_path {
        return Err(StartupFileIssueKind::ExternalTargetChanged {
            approved_target: approval,
            resolved_target: resolved_path.to_path_buf(),
        });
    }
    Ok(Some(ExternalTargetApproval::new(approval)))
}

fn canonicalize_approval(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("approved external target is not absolute".to_string());
    }
    if path.to_str().is_none() {
        return Err("approved external target is not valid UTF-8".to_string());
    }
    reject_parent_components(path)?;
    std::fs::canonicalize(path)
        .map_err(|error| format!("could not resolve approved external target: {error}"))
}

fn spec_id(project: &ActiveProject, path: &SelectedStartupPath) -> StartupFileSpecId {
    let mut hasher = Sha256::new();
    hasher.update(b"jcode-startup-file-spec-v1\0");
    hasher.update(project.key().stable_bytes());
    hasher.update(b"\0");
    match path {
        SelectedStartupPath::ProjectRelative(path) => {
            hasher.update(b"project\0");
            hasher.update(path.to_string_lossy().as_bytes());
        }
        SelectedStartupPath::ExternalAbsolute(path) => {
            hasher.update(b"external\0");
            hasher.update(path.to_string_lossy().as_bytes());
        }
    }
    StartupFileSpecId::from_digest(format!("{:x}", hasher.finalize()))
}

fn issue_for(
    spec: &StartupFileSpec,
    input_index: Option<usize>,
    kind: StartupFileIssueKind,
) -> StartupFileIssue {
    match input_index {
        Some(input_index) => StartupFileIssue::for_input(input_index, spec.path().as_path(), kind)
            .with_spec_id(spec.id().clone()),
        None => StartupFileIssue::for_spec(spec, kind),
    }
}

fn unsupported_extension(path: &Path) -> Option<StartupUnsupportedContent> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if extension == "pdf" {
        return Some(StartupUnsupportedContent::Pdf);
    }
    if matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "webp"
            | "tiff"
            | "tif"
            | "avif"
            | "heic"
            | "heif"
    ) {
        return Some(StartupUnsupportedContent::Image);
    }
    None
}

fn natural_os_cmp(left: &OsStr, right: &OsStr) -> Ordering {
    natural_bytes_cmp(left.as_encoded_bytes(), right.as_encoded_bytes())
}

fn natural_bytes_cmp(left: &[u8], right: &[u8]) -> Ordering {
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = digit_run_end(left, left_index);
            let right_end = digit_run_end(right, right_index);
            let left_digits = &left[left_index..left_end];
            let right_digits = &right[right_index..right_end];
            let left_significant = trim_numeric_zeroes(left_digits);
            let right_significant = trim_numeric_zeroes(right_digits);
            let ordering = left_significant
                .len()
                .cmp(&right_significant.len())
                .then_with(|| left_significant.cmp(right_significant))
                .then_with(|| left_digits.len().cmp(&right_digits.len()));
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index = left_end;
            right_index = right_end;
            continue;
        }
        let left_folded = left[left_index].to_ascii_lowercase();
        let right_folded = right[right_index].to_ascii_lowercase();
        let ordering = left_folded
            .cmp(&right_folded)
            .then_with(|| left[left_index].cmp(&right[right_index]));
        if ordering != Ordering::Equal {
            return ordering;
        }
        left_index += 1;
        right_index += 1;
    }
    left.len().cmp(&right.len())
}

fn digit_run_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn trim_numeric_zeroes(bytes: &[u8]) -> &[u8] {
    let first_nonzero = bytes
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(bytes.len().saturating_sub(1));
    &bytes[first_nonzero..]
}

#[cfg(test)]
pub(super) fn natural_name_cmp(left: &str, right: &str) -> Ordering {
    natural_bytes_cmp(left.as_bytes(), right.as_bytes())
}
