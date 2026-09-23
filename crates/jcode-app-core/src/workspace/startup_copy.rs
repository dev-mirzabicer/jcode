use super::*;
use crate::protocol::{StartupContextFailure, StartupContextFailureKind, StartupContextOperation};
use std::sync::Arc;
#[cfg(test)]
mod tests;

pub(super) async fn apply(
    request: RequestId,
    review: ReviewId,
    client: String,
    coordinator: Arc<crate::server::startup_context::StartupContextCoordinator>,
) -> WorkspaceResponse {
    let root = crate::storage::durable_state_dir();
    let result = tokio::task::spawn_blocking(move || -> Result<StartupCopyRecord> {
        let _trusted = WorkspaceClientAuthority::authenticated(client.clone())?;
        let service = WorkspaceService::new(&root);
        let result = commit(&service, request, review, &client, &coordinator);
        if let Err(problem) = &result
            && problem.code != IssueCode::Busy
            && let Ok(current) = service.inspect_startup_copy(request)
            && current.review.id == review
        {
            // The plan may already be committed even if its catalog receipt
            // failed. Retain the original issue and exact request for retry.
            let _ = service.record_startup_copy_issue(request, problem.clone());
        }
        result
    })
    .await;
    match result {
        Ok(Ok(record)) => WorkspaceResponse::StartupCopy(record),
        Ok(Err(error)) => WorkspaceResponse::Error(error),
        Err(error) => WorkspaceResponse::Error(Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!(
                "Startup Context copy task ended without a reply; inspect request {request}: {error}"
            ),
        }),
    }
}

fn commit(
    service: &WorkspaceService,
    request: RequestId,
    review: ReviewId,
    client: &str,
    coordinator: &crate::server::startup_context::StartupContextCoordinator,
) -> Result<StartupCopyRecord> {
    let intent = service.reserve_startup_copy(request, review)?;
    let _root = service.acquire_root(intent.record.review.target)?;
    let intent = service.validate_startup_copy_intent(request)?;
    if intent.record.state == StartupCopyState::Complete {
        return service.finish_startup_copy(request);
    }
    coordinator
        .commit_workspace_copy(
            &intent.target_path,
            &intent.transition,
            request,
            client,
            || {
                let current = service
                    .validate_startup_copy_intent(request)
                    .map_err(as_startup_failure)?;
                if !intent.transition.same_change(&current.transition) {
                    return Err(as_startup_failure(Issue {
                        code: IssueCode::Conflict,
                        detail: "Startup Context copy transition changed after review".into(),
                    }));
                }
                Ok(())
            },
        )
        .map_err(as_workspace_issue)?;
    service.finish_startup_copy(request)
}

fn as_startup_failure(issue: Issue) -> StartupContextFailure {
    StartupContextFailure {
        operation: StartupContextOperation::ApplySelection,
        kind: if issue.code == IssueCode::Busy {
            StartupContextFailureKind::LeaseBusy
        } else if issue.code == IssueCode::Conflict {
            StartupContextFailureKind::OperationConflict
        } else {
            StartupContextFailureKind::Recovery
        },
        message: issue.detail,
        retryable: true,
        issues: vec![],
    }
}

fn as_workspace_issue(failure: StartupContextFailure) -> Issue {
    let code = match failure.kind {
        StartupContextFailureKind::LeaseBusy => IssueCode::Busy,
        StartupContextFailureKind::StalePlanRevision
        | StartupContextFailureKind::OperationConflict => IssueCode::Conflict,
        StartupContextFailureKind::InvalidPath | StartupContextFailureKind::InvalidRequest => {
            IssueCode::InvalidInput
        }
        _ => IssueCode::RecoveryRequired,
    };
    Issue {
        code,
        detail: format!("Startup Context plan copy: {}", failure.message),
    }
}
