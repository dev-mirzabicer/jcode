use super::*;
use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, RunState};
use jcode_tool_core::OutputCapture;
use jcode_tool_types::execution::ExecutionRequest;
use jcode_tool_types::{OutputSource, ToolOutput};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

type Key = (PathBuf, RequestId);
const CLONE_EXECUTION_SESSION: &str = "workspace-operations";
const CLONE_EXECUTION_TOOL: &str = "workspace_clone";
static LIVE: LazyLock<Mutex<HashSet<Key>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct Active(Key);
impl Drop for Active {
    fn drop(&mut self) {
        LIVE.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

pub(super) async fn output(
    clone: RequestId,
    request: ExecutionRequest,
    client: String,
) -> WorkspaceResponse {
    let run_id = match &request {
        ExecutionRequest::Inspect { run_id }
        | ExecutionRequest::Read { run_id, .. }
        | ExecutionRequest::ReadPart { run_id, .. } => run_id.clone(),
        _ => return WorkspaceResponse::Error(Issue {
            code: IssueCode::UnsupportedCapability,
            detail: "Clone output supports inspection and exact reads, not execution control or unscoped listings".into(),
        }),
    };
    let state_root = crate::storage::durable_state_dir();
    let output_root = match crate::storage::jcode_dir() {
        Ok(root) => root,
        Err(error) => return WorkspaceResponse::Error(problem(error)),
    };
    let read_root = output_root.clone();
    let permitted = tokio::task::spawn_blocking(move || -> Result<()> {
        let _trusted = WorkspaceClientAuthority::authenticated(client)?;
        let operation = WorkspaceService::new(&state_root).inspect_clone(clone)?;
        if !operation.output_runs.contains(&run_id) {
            return Err(Issue {
                code: IssueCode::InvalidIdentity,
                detail: "This retained run does not belong to the requested checkout operation"
                    .into(),
            });
        }
        let store = ExecutionStore::open(&read_root).map_err(problem)?;
        let run = store
            .inspect(&run_id)
            .map_err(problem)?
            .ok_or_else(|| Issue {
                code: IssueCode::RecoveryRequired,
                detail: "Checkout output run is missing from its execution store".into(),
            })?;
        if run.session_id != CLONE_EXECUTION_SESSION
            || run.message_id != clone.to_string()
            || run.tool != CLONE_EXECUTION_TOOL
        {
            return Err(Issue {
                code: IssueCode::CorruptState,
                detail: "Checkout operation and retained execution identity disagree".into(),
            });
        }
        Ok(())
    })
    .await;
    match permitted {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return WorkspaceResponse::Error(error),
        Err(error) => return WorkspaceResponse::Error(problem(error)),
    }
    match crate::execution::inspection::inspect(&output_root, CLONE_EXECUTION_SESSION, request)
        .await
    {
        Ok(response) => WorkspaceResponse::CloneOutput(response),
        Err(error) => WorkspaceResponse::Error(problem(error)),
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
            if record.state == CloneState::Pending && record.output_runs.is_empty() {
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
            if let Some(previous) = record.output_runs.last() {
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
        session_id: CLONE_EXECUTION_SESSION.into(),
        message_id: request.to_string(),
        call_path: vec![crate::id::new_id("clone-attempt")],
        tool: CLONE_EXECUTION_TOOL.into(),
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
    if let Err(error) = service.record_clone_output(request, run.id.clone()) {
        return Err(no_effect_failure(&store, &run, error));
    }
    if let Err(error) = store.start(&run.id, &owner) {
        return Err(no_effect_failure(&store, &run, problem(error)));
    }
    let capture = match Capture::create(
        store.clone(),
        run.clone(),
        crate::config::config().output.storage.clone(),
    ) {
        Ok(capture) => capture,
        Err(error) => return Err(no_effect_failure(&store, &run, problem(error))),
    };
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

fn no_effect_failure(
    store: &ExecutionStore,
    prepared: &crate::execution::RunRecord,
    problem: Issue,
) -> Issue {
    let mut failed = prepared.clone();
    failed.state = RunState::Failed;
    if let Err(error) = store.finish(&failed) {
        return Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!(
                "No Git work started: {}; retained execution could not seal its failed run: {error}",
                problem.detail
            ),
        };
    }
    problem
}

fn problem(error: impl std::fmt::Display) -> Issue {
    Issue {
        code: IssueCode::RecoveryRequired,
        detail: format!("Retained clone execution: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_output_start_has_terminal_no_effect_receipt() {
        let state = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(state.path()).unwrap();
        let invocation = Invocation {
            session_id: "fixture-workspace".into(),
            message_id: RequestId::new().to_string(),
            call_path: vec!["attempt".into()],
            tool: "workspace_clone".into(),
            input: serde_json::json!({"fixture":true}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(prepared) =
            store.prepare(&invocation, "fixture-owner").unwrap()
        else {
            panic!()
        };
        store.start(&prepared.id, "fixture-owner").unwrap();
        let error = no_effect_failure(
            &store,
            &prepared,
            Issue {
                code: IssueCode::Io,
                detail: "synthetic output allocation failure".into(),
            },
        );
        assert_eq!(error.code, IssueCode::Io);
        let retained = store.inspect(&prepared.id).unwrap().unwrap();
        assert_eq!(retained.state, RunState::Failed);
        assert!(!retained.complete);
        assert!(retained.output_path.is_none());
        assert_eq!(retained.output_bytes, 0);
    }
}
