//! Trusted workspace controls over the shared execution supervisor. Neither a
//! client connection nor its waiter owns the blocking closeout producer.
use super::*;
use crate::execution::{ExecutionStore, Invocation};
use jcode_tool_core::{
    ExecutionPolicy, InvocationContext, OutputCapture, OutputStream, ToolContext, ToolExecutionMode,
};
use jcode_tool_types::{OutputSource, ToolOutput, execution::ExecutionRequest};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
mod capture;

pub(super) async fn dispatch(request: CloseoutRequest, client: String) -> WorkspaceResponse {
    let session_root = match crate::storage::jcode_dir() {
        Ok(root) => root,
        Err(error) => return WorkspaceResponse::Error(problem(error)),
    };
    dispatch_at(
        crate::storage::durable_state_dir(),
        session_root,
        request,
        client,
    )
    .await
}

async fn dispatch_at(
    state_root: PathBuf,
    session_root: PathBuf,
    request: CloseoutRequest,
    client: String,
) -> WorkspaceResponse {
    let client = match WorkspaceClientAuthority::authenticated(client) {
        Ok(client) => client,
        Err(error) => return WorkspaceResponse::Error(error),
    };
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state_root, session_root, request, client);
        WorkspaceResponse::Error(Issue {
            code: IssueCode::UnsupportedCapability,
            detail:
                "Native checkout closeout requires the supported macOS filesystem/process adapters"
                    .into(),
        })
    }
    #[cfg(target_os = "macos")]
    {
        let service = WorkspaceService::new(&state_root);
        let result = match request {
            CloseoutRequest::Execute { request, spec } => {
                execute(service, session_root, client, request, spec).await
            }
            CloseoutRequest::Execution { request, control } => {
                execution(service, session_root, request, control).await
            }
            request => crate::runtime_lifecycle::admission::spawn_blocking(move || {
                immediate(&service, &client, request)
            })
            .await
            .map_err(problem)
            .and_then(|value| value),
        };
        match result {
            Ok(value) => WorkspaceResponse::Closeout(Box::new(value)),
            Err(error) => WorkspaceResponse::Error(error),
        }
    }
}

#[cfg(target_os = "macos")]
fn immediate(
    service: &WorkspaceService,
    client: &WorkspaceClientAuthority,
    request: CloseoutRequest,
) -> Result<CloseoutResponse> {
    if matches!(&request, CloseoutRequest::Revoke { .. }) {
        return crate::runtime_lifecycle::admission::control(|| {
            immediate_admitted(service, client, request)
        })
        .map_err(problem)?;
    }
    immediate_admitted(service, client, request)
}

#[cfg(target_os = "macos")]
fn immediate_admitted(
    service: &WorkspaceService,
    client: &WorkspaceClientAuthority,
    request: CloseoutRequest,
) -> Result<CloseoutResponse> {
    use CloseoutResponse as Response;
    let _permit = if matches!(&request, CloseoutRequest::Begin { .. }) {
        mutation_permit("closeout-authorization")?
    } else {
        None
    };
    match request {
        CloseoutRequest::Begin {
            request,
            expected_revision,
            spec,
        } => service
            .begin_closeout(client, request, expected_revision, spec)
            .map(|v| Response::Record(Box::new(v))),
        CloseoutRequest::Inspect { operation } => service
            .inspect_closeout(operation)
            .map(|v| Response::Record(Box::new(v))),
        CloseoutRequest::Inventory {
            operation,
            digest,
            after,
            limit,
        } => service
            .closeout_inventory(operation, &digest, after, limit)
            .map(Response::Inventory),
        CloseoutRequest::Revoke {
            request,
            operation,
            expected_revision,
        } => service
            .revoke_closeout(client, request, operation, expected_revision)
            .map(|v| Response::Record(Box::new(v))),
        CloseoutRequest::InspectAction { request } => service
            .inspect_closeout_action(request)
            .map(|v| Response::Action(Box::new(v))),
        CloseoutRequest::Review { operation } => service
            .inspect_closeout_review(operation)
            .map(|v| Response::Review(v.map(Box::new))),
        CloseoutRequest::Recovery { operation } => service
            .pending_closeout_recovery(operation)
            .map(|v| Response::Recovery(v.map(Box::new))),
        CloseoutRequest::RemovalProgress {
            operation,
            expected_revision,
            after,
            limit,
        } => service
            .closeout_removal_progress(operation, expected_revision, after, limit)
            .map(Response::RemovalProgress),
        CloseoutRequest::History { location } => service
            .closed_checkout_history(location)
            .map(|v| Response::History(Box::new(v))),
        CloseoutRequest::Execute { .. } | CloseoutRequest::Execution { .. } => Err(Issue {
            code: IssueCode::UnsupportedCapability,
            detail: "Closeout execution requires its runtime owner".into(),
        }),
    }
}

#[cfg(target_os = "macos")]
async fn execute(
    service: WorkspaceService,
    session_root: PathBuf,
    client: WorkspaceClientAuthority,
    request: RequestId,
    spec: CloseoutActionSpec,
) -> Result<CloseoutResponse> {
    let permit = mutation_permit("closeout-action")?;
    crate::runtime_lifecycle::admission::scope(
        permit,
        execute_admitted(service, session_root, client, request, spec),
    )
    .await
}

#[cfg(target_os = "macos")]
async fn execute_admitted(
    service: WorkspaceService,
    session_root: PathBuf,
    client: WorkspaceClientAuthority,
    request: RequestId,
    spec: CloseoutActionSpec,
) -> Result<CloseoutResponse> {
    let prepare = service.clone();
    let record = crate::runtime_lifecycle::admission::spawn_blocking(move || {
        prepare.admit_closeout_action(&client, request, spec)
    })
    .await
    .map_err(problem)??;
    run_admitted(service, session_root, request, record)
        .await
        .map(|record| CloseoutResponse::Action(Box::new(record)))
}

/// Agent closeout work for the caller's authoritative Session. The base owner
/// restricts the action set and scope; execution reuses the same supervisor.
/// The agent's call waits for its step to settle. Cancelling that wait leaves
/// the step under its execution owner, inspectable by request.
#[cfg(target_os = "macos")]
pub(crate) async fn agent_action(
    session: crate::session::Session,
    request: RequestId,
    spec: CloseoutActionSpec,
) -> Result<CloseoutActionRecord> {
    agent_action_at(
        crate::storage::durable_state_dir(),
        crate::storage::jcode_dir().map_err(problem)?,
        session,
        request,
        spec,
    )
    .await
}

#[cfg(target_os = "macos")]
async fn agent_action_at(
    state_root: PathBuf,
    session_root: PathBuf,
    session: crate::session::Session,
    request: RequestId,
    spec: CloseoutActionSpec,
) -> Result<CloseoutActionRecord> {
    let service = WorkspaceService::new(&state_root);
    let permit = mutation_permit("closeout-agent-action")?;
    let record = crate::runtime_lifecycle::admission::scope(permit, {
        let service = service.clone();
        let session_root = session_root.clone();
        async move {
            let prepare = service.clone();
            let record = crate::runtime_lifecycle::admission::spawn_blocking(move || {
                prepare.admit_agent_closeout_action(&session, request, spec)
            })
            .await
            .map_err(problem)??;
            run_admitted(service, session_root, request, record).await
        }
    })
    .await?;
    if record.result.is_some() || record.issue.is_some() {
        return Ok(record);
    }
    let store = ExecutionStore::open(&session_root).map_err(problem)?;
    crate::execution::control_transport::control_in_store(
        &store,
        &record.run_id,
        crate::execution::ControlOperation::Wait,
    )
    .await
    .map_err(problem)?;
    crate::runtime_lifecycle::admission::spawn_blocking(move || {
        service.inspect_closeout_action(request)
    })
    .await
    .map_err(problem)?
}

#[cfg(not(target_os = "macos"))]
pub(crate) async fn agent_action(
    _session: crate::session::Session,
    _request: RequestId,
    _spec: CloseoutActionSpec,
) -> Result<CloseoutActionRecord> {
    Err(Issue {
        code: IssueCode::UnsupportedCapability,
        detail: "Native checkout closeout requires the supported macOS filesystem/process adapters"
            .into(),
    })
}

#[cfg(target_os = "macos")]
async fn run_admitted(
    service: WorkspaceService,
    session_root: PathBuf,
    request: RequestId,
    record: CloseoutActionRecord,
) -> Result<CloseoutActionRecord> {
    if record.result.is_some() || record.issue.is_some() {
        return Ok(record);
    }
    let invocation = service.closeout_action_invocation(&record)?;
    let ctx = ToolContext {
        session_id: invocation.session_id.clone(),
        message_id: invocation.message_id.clone(),
        tool_call_id: request.to_string(),
        working_dir: invocation.working_dir.clone(),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: InvocationContext {
            policy: ExecutionPolicy {
                background: true,
                notify: false,
                wake: false,
                manual_ready: true,
                cooperative_stop: true,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let producer_service = service.clone();
    let producer_root = session_root.clone();
    let output = crate::execution::execute_at(
        session_root,
        invocation,
        ctx,
        std::num::NonZeroUsize::new(10000).unwrap(),
        Box::new(move |context| {
            Box::pin(async move {
                let handle = tokio::runtime::Handle::current();
                crate::runtime_lifecycle::admission::spawn_blocking(move || -> anyhow::Result<ToolOutput> {
                    let capture = context.invocation.capture.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("Closeout producer has no retained capture")
                    })?;
                    let capture = capture::ControlledCapture {
                        capture: capture.clone(),
                        stop: context.graceful_shutdown_signal.clone().ok_or_else(|| {
                            anyhow::anyhow!("Closeout producer has no owned Stop signal")
                        })?,
                    };
                    let store = ExecutionStore::open(&producer_root)?;
                    let cwd = context
                        .working_dir
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("Closeout control directory is absent"))?;
                    let runtime = CloseoutRuntime::new(&producer_root, &store, cwd, &capture);
                    if let Some(ready) = &context.invocation.ready {
                        ready.mark();
                    }
                    let outcome = handle
                        .block_on(producer_service.execute_closeout_action(request, &runtime));
                    let receipt =
                        producer_service.record_closeout_action_result(request, outcome.clone());
                    let body = serde_json::to_vec(
                        &serde_json::json!({"domain_outcome":outcome, "action_receipt":receipt}),
                    )?;
                    capture.write(OutputStream::Text, &body)?;
                    let mut output = ToolOutput::new("");
                    output.is_error = outcome.is_err() || receipt.is_err();
                    output.source = OutputSource::Retained(capture.reference()?);
                    Ok(output)
                })
                .await
                .map_err(anyhow::Error::from)?
            })
        }),
    )
    .await;
    // A failed/uncertain waiter never starts another action. Domain and sealed
    // execution receipts remain inspectable under these original identities.
    if let Err(error) = output {
        return Err(problem(format!(
            "Action {request}, execution {}: {error}",
            record.run_id
        )));
    }
    crate::runtime_lifecycle::admission::spawn_blocking(move || {
        service.inspect_closeout_action(request)
    })
    .await
    .map_err(problem)?
}

#[cfg(target_os = "macos")]
async fn execution(
    service: WorkspaceService,
    root: PathBuf,
    request: RequestId,
    control: ExecutionRequest,
) -> Result<CloseoutResponse> {
    let id = match &control {
        ExecutionRequest::Inspect {run_id} | ExecutionRequest::Read {run_id, ..} | ExecutionRequest::ReadPart {run_id, ..} | ExecutionRequest::Stop {run_id} => run_id.clone(),
        _ => return Err(Issue {code:IssueCode::UnsupportedCapability, detail:"Closeout supports exact output/status and cooperative Stop, not unscoped listing, force or detached command handoff".into()}),
    };
    let source = root.clone();
    crate::runtime_lifecycle::admission::spawn_blocking(move || -> Result<()> {
        let action = service.inspect_closeout_action(request)?;
        if action.run_id != id {
            return Err(Issue {
                code: IssueCode::InvalidIdentity,
                detail: "Execution belongs to a different closeout action".into(),
            });
        }
        let store = ExecutionStore::open(&source).map_err(problem)?;
        let expected: Invocation = service.closeout_action_invocation(&action)?;
        let actual = store.invocation_input(&id).map_err(problem)?;
        if actual != expected {
            return Err(Issue {
                code: IssueCode::CorruptState,
                detail: "Execution input and closeout action identity disagree".into(),
            });
        }
        Ok(())
    })
    .await
    .map_err(problem)??;
    crate::execution::inspection::inspect(&root, CLOSEOUT_EXECUTION_SESSION, control)
        .await
        .map(CloseoutResponse::Execution)
        .map_err(problem)
}

fn problem(error: impl std::fmt::Display) -> Issue {
    Issue {
        code: IssueCode::RecoveryRequired,
        detail: format!(
            "Closeout execution: {error}. Inspect the original action and execution identities before retrying effects."
        ),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
