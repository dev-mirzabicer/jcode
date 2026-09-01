use super::capture::capture_stable_file;
use super::selection::resolve_existing_spec;
use super::types::{ActiveProject, StartupFileIssueKind, StartupFileSpec, StartupObservedState};
use jcode_session_types::{
    StoredStartupExternalApproval, StoredStartupFileReceipt, StoredStartupFileSpec,
    StoredStartupPathClassification, StoredStartupSelectedPath,
};
use std::path::PathBuf;

pub(super) fn observe_receipt_file(
    project: &ActiveProject,
    file: &StoredStartupFileReceipt,
    max_bytes: u64,
    max_capture_attempts: usize,
) -> StartupObservedState {
    let logical_path = PathBuf::from(&file.logical_path);
    let selected_path = if logical_path.is_absolute() {
        StoredStartupSelectedPath::ExternalAbsolute {
            path: file.logical_path.clone(),
        }
    } else {
        StoredStartupSelectedPath::ProjectRelative {
            path: file.logical_path.clone(),
        }
    };
    let external_approval = matches!(
        file.classification,
        StoredStartupPathClassification::External
    )
    .then(|| StoredStartupExternalApproval {
        approved_resolved_target: file.resolved_path.clone(),
    });
    let spec = match StartupFileSpec::from_stored(StoredStartupFileSpec {
        id: file.spec_id.clone(),
        path: selected_path,
        external_approval,
    }) {
        Ok(spec) => spec,
        Err(_) => return StartupObservedState::Unsupported,
    };
    let target = match resolve_existing_spec(project, &spec, max_bytes) {
        Ok(target) => target,
        Err(issue) => return issue_state(issue.kind()),
    };
    match capture_stable_file(project, target, max_bytes, max_capture_attempts, None) {
        Ok(captured) if captured.sha256() == file.sha256 => StartupObservedState::Current,
        Ok(captured) => StartupObservedState::Changed {
            sha256: captured.sha256().to_string(),
            bytes: captured.bytes(),
        },
        Err(issue) => issue_state(issue.kind()),
    }
}

fn issue_state(kind: &StartupFileIssueKind) -> StartupObservedState {
    match kind {
        StartupFileIssueKind::Missing | StartupFileIssueKind::BrokenSymlink => {
            StartupObservedState::Missing
        }
        StartupFileIssueKind::Unreadable { .. } => StartupObservedState::Unreadable,
        StartupFileIssueKind::EmptyPath
        | StartupFileIssueKind::InvalidPathEncoding
        | StartupFileIssueKind::PathTraversal
        | StartupFileIssueKind::UnsupportedTarget { .. }
        | StartupFileIssueKind::UnsupportedContent { .. }
        | StartupFileIssueKind::NonUtf8
        | StartupFileIssueKind::ExternalApprovalRequired { .. }
        | StartupFileIssueKind::ExternalTargetChanged { .. }
        | StartupFileIssueKind::InvalidExternalApproval { .. }
        | StartupFileIssueKind::DuplicateSelection { .. }
        | StartupFileIssueKind::TooManyEntries { .. }
        | StartupFileIssueKind::FileTooLarge { .. }
        | StartupFileIssueKind::BatchTooLarge { .. }
        | StartupFileIssueKind::ChangedDuringCapture
        | StartupFileIssueKind::DirectoryOutsideProject
        | StartupFileIssueKind::DirectoryReadFailed { .. } => StartupObservedState::Unsupported,
    }
}
