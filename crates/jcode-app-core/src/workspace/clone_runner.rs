use super::*;
use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, RunState};
use jcode_tool_core::OutputCapture;
use jcode_tool_types::{OutputSource, ToolOutput};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

type Key = (PathBuf, RequestId);
static LIVE: LazyLock<Mutex<HashSet<Key>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct Active(Key);
impl Drop for Active {
    fn drop(&mut self) {
        LIVE.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

pub(super) async fn begin(
    request: RequestId,
    review: ReviewId,
    client: String,
) -> WorkspaceResponse {
    let root = crate::storage::durable_state_dir();
    let service = WorkspaceService::new(&root);
    let record = tokio::task::spawn_blocking(move || {
        let _ = WorkspaceClientAuthority::authenticated(client)?;
        service.begin_clone(request, review)
    })
    .await;
    match record {
        Ok(Ok(record)) => {
            if record.state == CloneState::Pending && record.output_run.is_none() {
                start(root, request);
            }
            WorkspaceResponse::Clone(record)
        }
        Ok(Err(error)) => WorkspaceResponse::Error(error),
        Err(error) => WorkspaceResponse::Error(Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!(
                "Clone intent task stopped without a reply; inspect request {request}: {error}"
            ),
        }),
    }
}

pub(super) async fn resume(request: RequestId, client: String) -> WorkspaceResponse {
    let root = crate::storage::durable_state_dir();
    let service = WorkspaceService::new(&root);
    let record = tokio::task::spawn_blocking(move || {
        let _ = WorkspaceClientAuthority::authenticated(client)?;
        service.inspect_clone(request)
    })
    .await;
    match record {
        Ok(Ok(record)) if !matches!(record.state, CloneState::Ready | CloneState::Cancelled) => {
            if let Some(previous) = &record.output_run {
                let previous = previous.clone();
                let output_root = match crate::storage::jcode_dir() {
                    Ok(path) => path,
                    Err(error) => return WorkspaceResponse::Error(problem(error)),
                };
                let store =
                    match tokio::task::spawn_blocking(move || ExecutionStore::open(&output_root))
                        .await
                    {
                        Ok(Ok(store)) => store,
                        Ok(Err(error)) => return WorkspaceResponse::Error(problem(error)),
                        Err(error) => return WorkspaceResponse::Error(problem(error)),
                    };
                match store.recover_lost_owner(&previous).await {
                    Ok(Some(row)) if row.state.terminal() => {}
                    Ok(_) => {
                        return WorkspaceResponse::Error(Issue {
                            code: IssueCode::Busy,
                            detail: format!(
                                "Previous Git work {previous} is still owned or its effect outcome is unknown. Inspect its retained output; no second acquisition was started."
                            ),
                        });
                    }
                    Err(error) => return WorkspaceResponse::Error(problem(error)),
                }
            }
            start(root, request);
            WorkspaceResponse::Clone(record)
        }
        Ok(Ok(record)) if record.state == CloneState::Ready => {
            let root = crate::storage::durable_state_dir();
            match tokio::task::spawn_blocking(move || {
                WorkspaceService::new(&root).finish_clone_backup(request)
            })
            .await
            {
                Ok(Ok(ready)) => WorkspaceResponse::Clone(ready),
                Ok(Err(error)) => WorkspaceResponse::Error(error),
                Err(error) => WorkspaceResponse::Error(problem(error)),
            }
        }
        Ok(Ok(record)) => WorkspaceResponse::Clone(record),
        Ok(Err(error)) => WorkspaceResponse::Error(error),
        Err(error) => WorkspaceResponse::Error(Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!("Clone recovery inspection stopped without a reply: {error}"),
        }),
    }
}

fn start(root: PathBuf, request: RequestId) {
    let key = (root.clone(), request);
    if !LIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone())
    {
        return;
    }
    tokio::spawn(async move {
        let _active = Active(key);
        let service = WorkspaceService::new(&root);
        if let Err(error) = run(&service, request).await {
            crate::logging::warn(&format!(
                "Checkout clone {request} retained an issue: {error}"
            ));
            let _ = service.record_clone_launch_failure(request, error);
        }
    });
}

async fn run(service: &WorkspaceService, request: RequestId) -> Result<()> {
    let run_root = crate::storage::jcode_dir().map_err(problem)?;
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&run_root))
        .await
        .map_err(problem)?
        .map_err(problem)?;
    let runtime = crate::execution::ensure_execution_runtime(&store)
        .await
        .map_err(problem)?;
    let owner = runtime.endpoint.id.clone();
    let invocation = Invocation {
        session_id: "workspace-operations".into(),
        message_id: request.to_string(),
        call_path: vec!["clone".into(), crate::id::new_id("attempt")],
        tool: "workspace_clone".into(),
        input: serde_json::json!({"request": request}),
        working_dir: None,
        received_result_digest: None,
    };
    let run = match store.prepare(&invocation, &owner).map_err(problem)? {
        PreparedInvocation::New(run) => run,
        PreparedInvocation::Existing(_) => {
            return Err(Issue {
                code: IssueCode::Conflict,
                detail: "Duplicate checkout output identity".into(),
            });
        }
    };
    let capture = Capture::create(
        store.clone(),
        run.clone(),
        crate::config::config().output.storage.clone(),
    )
    .map_err(problem)?;
    service.record_clone_output(request, run.id.clone())?;
    store.start(&run.id, &owner).map_err(problem)?;
    let result = service.execute_clone(request, &capture).await;
    let mut output = ToolOutput::new("");
    output.is_error = result.is_err();
    output.source = OutputSource::Retained(capture.reference().map_err(problem)?);
    output.metadata = Some(
        serde_json::json!({"operation":request,"result":result.as_ref().ok(),"issue":result.as_ref().err()}),
    );
    let outcome = if result.is_ok() {
        RunState::Completed
    } else {
        RunState::Failed
    };
    capture.seal(output, outcome).map_err(problem)?;
    result.map(|_| ())
}

fn problem(error: impl std::fmt::Display) -> Issue {
    Issue {
        code: IssueCode::RecoveryRequired,
        detail: format!("Retained clone execution: {error}"),
    }
}
