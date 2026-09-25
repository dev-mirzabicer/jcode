//! Human runtime control over the same native service and durable receipts.
use super::args::{RuntimeCommand, RuntimeStopOptions, RuntimeStrategy, RuntimeTasks};
use super::provider_init::ProviderChoice;
use crate::protocol::{Request, ServerEvent};
use crate::runtime_lifecycle::RuntimeStopStore;
use crate::workspace::{OperationId, RequestId, runtime::*};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Serialize)]
struct Report {
    socket: PathBuf,
    live_response: bool,
    coordinator_owned: Option<bool>,
    response: Option<RuntimeResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirm_request: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

struct NativeClient {
    read: BufReader<crate::transport::ReadHalf>,
    write: crate::transport::WriteHalf,
    sequence: u64,
    status: RuntimeStatus,
}
impl NativeClient {
    async fn connect(socket: &Path) -> Result<Self> {
        let stream = tokio::time::timeout(
            Duration::from_secs(10),
            crate::transport::Stream::connect(socket),
        )
        .await??;
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write_request(&mut write, Request::RuntimeProbe { id: 1 }).await?;
        ensure!(
            matches!(
                read_event(&mut read).await?,
                ServerEvent::RuntimeCapabilities {
                    id: 1,
                    version: Some(1)
                }
            ),
            "Connected server does not support runtime_lifecycle_v1; no control was sent"
        );
        write_request(
            &mut write,
            Request::RuntimeControl {
                id: 2,
                request: Box::new(RuntimeRequest::Status {}),
            },
        )
        .await?;
        let RuntimeResponse::Status(status) = response(read_event(&mut read).await?, 2)? else {
            anyhow::bail!("Runtime did not return status");
        };
        let store = local_store(socket)?;
        ensure!(
            status.namespace == store.namespace() && status.runtime.is_some(),
            "Server runtime namespace does not match the selected socket; no control was sent"
        );
        Ok(Self {
            read,
            write,
            sequence: 2,
            status,
        })
    }
    async fn request(&mut self, request: RuntimeRequest) -> Result<RuntimeResponse> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Runtime request sequence exhausted")?;
        write_request(
            &mut self.write,
            Request::RuntimeControl {
                id: self.sequence,
                request: Box::new(request),
            },
        )
        .await?;
        response(read_event(&mut self.read).await?, self.sequence)
    }
}
async fn write_request(write: &mut crate::transport::WriteHalf, request: Request) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10),
        write.write_all((serde_json::to_string(&request)? + "\n").as_bytes()),
    )
    .await??;
    Ok(())
}
async fn read_event(read: &mut BufReader<crate::transport::ReadHalf>) -> Result<ServerEvent> {
    let mut line = String::new();
    ensure!(
        tokio::time::timeout(Duration::from_secs(30), read.read_line(&mut line)).await?? > 0,
        "Runtime connection closed before its reply; inspect retained intent before retrying"
    );
    serde_json::from_str(&line).context("Decode runtime reply")
}
fn response(event: ServerEvent, expected: u64) -> Result<RuntimeResponse> {
    match event {
        ServerEvent::RuntimeResponse { id, response } if id == expected => match *response {
            RuntimeResponse::Error(issue) => Err(anyhow::anyhow!(issue)),
            response => Ok(response),
        },
        ServerEvent::Error { id, message, .. } if id == expected => {
            anyhow::bail!("Runtime rejected request: {message}")
        }
        other => anyhow::bail!("Uncorrelated runtime reply: {other:?}"),
    }
}
fn local_store(socket: &Path) -> Result<RuntimeStopStore> {
    RuntimeStopStore::new(&crate::storage::durable_state_dir(), socket)
}
fn options(args: &RuntimeStopOptions) -> ShutdownOptions {
    ShutdownOptions {
        strategy: match args.strategy {
            RuntimeStrategy::FinishCurrent => StopStrategy::FinishCurrent,
            RuntimeStrategy::Interrupt => StopStrategy::Interrupt,
        },
        independent: match args.tasks {
            RuntimeTasks::Stop => IndependentTasks::Stop,
            RuntimeTasks::KeepSupported => IndependentTasks::KeepSupported,
        },
        quiescence_timeout_seconds: args.quiescence_seconds,
    }
}
async fn status(socket: &Path) -> Result<Report> {
    match NativeClient::connect(socket).await {
        Ok(client) => Ok(Report {
            socket: socket.into(),
            live_response: true,
            coordinator_owned: Some(true),
            response: Some(RuntimeResponse::Status(client.status)),
            confirm_request: None,
            detail: None,
        }),
        Err(error) => {
            let parent = socket.parent().context("Socket has no parent")?;
            if !parent.try_exists()? {
                return Ok(Report {
                    socket: socket.into(),
                    live_response: false,
                    coordinator_owned: None,
                    response: None,
                    confirm_request: None,
                    detail: Some(format!(
                        "Runtime socket directory is unavailable. No process started and no directory created. {error:#}"
                    )),
                });
            }
            let store = local_store(socket)?;
            Ok(Report {
                socket: socket.into(),
                live_response: false,
                coordinator_owned: Some(store.owner_is_live()?),
                response: Some(RuntimeResponse::Status(store.status()?)),
                confirm_request: None,
                detail: Some(format!(
                    "Live status unavailable; showing durable intent, not proof of live work: {error:#}"
                )),
            })
        }
    }
}
async fn inspect(socket: &Path, operation: OperationId) -> Result<Report> {
    match NativeClient::connect(socket).await {
        Ok(mut client) => {
            let response = client
                .request(RuntimeRequest::Inspect { operation })
                .await?;
            Ok(Report {
                socket: socket.into(),
                live_response: true,
                coordinator_owned: Some(true),
                response: Some(response),
                confirm_request: None,
                detail: None,
            })
        }
        Err(error) => {
            let store = local_store(socket)?;
            Ok(Report {
                socket: socket.into(),
                live_response: false,
                coordinator_owned: Some(store.owner_is_live()?),
                response: Some(RuntimeResponse::Operation(store.inspect(operation)?)),
                confirm_request: None,
                detail: Some(format!(
                    "Durable operation, live reply unavailable: {error:#}"
                )),
            })
        }
    }
}
async fn control(socket: &Path, request: RuntimeRequest) -> Result<Report> {
    let original = request.clone();
    let result = async { NativeClient::connect(socket).await?.request(request).await }.await;
    let (response, live, detail) = match result {
        Ok(response) => (response, true, None),
        Err(error) => {
            if let RuntimeRequest::Begin { request, review } = original
                && let Some(operation) = local_store(socket)?.accepted_request(request, review)?
            {
                (
                    RuntimeResponse::Operation(operation),
                    false,
                    Some(format!(
                        "Recovered original accepted request from its durable receipt; no control replay: {error:#}"
                    )),
                )
            } else {
                return Err(error);
            }
        }
    };
    let confirm_request = matches!(response, RuntimeResponse::Review(_)).then(RequestId::new);
    Ok(Report {
        socket: socket.into(),
        live_response: live,
        coordinator_owned: Some(local_store(socket)?.owner_is_live()?),
        response: Some(response),
        confirm_request,
        detail,
    })
}
fn print(report: &Report, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("Runtime socket: {}", report.socket.display());
    println!(
        "Live reply: {}. Coordinator owned: {}.",
        report.live_response,
        match report.coordinator_owned {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    );
    if let Some(detail) = &report.detail {
        println!("{detail}");
    }
    match &report.response {
        Some(RuntimeResponse::Review(review)) => {
            println!("Review {} for runtime {}", review.id, review.runtime);
            println!(
                "Strategy: {:?}. Independent tasks: {:?}. Quiescence deadline: {}s.",
                review.options.strategy,
                review.options.independent,
                review.options.quiescence_timeout_seconds
            );
            if let Some(prior) = &review.replaces {
                println!(
                    "Replaces operation {} revision {}. Prior effects are not rolled back.",
                    prior.operation, prior.revision
                );
            }
            for work in &review.work {
                println!(
                    "  {:?} {} (owner {}, session {:?}, native-survival capability {})",
                    work.kind, work.id, work.owner, work.session, work.supported_survivor
                );
            }
            println!("No shutdown has begun from this review. Confirm the same --socket with:");
            println!(
                "  jcode --socket '{}' runtime confirm {} --request {}",
                report
                    .socket
                    .to_str()
                    .context("Socket path cannot be rendered as a shell argument")?
                    .replace('\'', "'\\''"),
                review.id,
                report
                    .confirm_request
                    .context("Review request identity missing")?
            );
        }
        Some(RuntimeResponse::Operation(operation)) => print_operation(operation),
        Some(RuntimeResponse::Status(status)) => {
            println!(
                "Namespace: {}. Runtime: {:?}. Desired stopped: {}. Reloading: {}.",
                status.namespace, status.runtime, status.desired_stopped, status.reload_in_progress
            );
            if let Some(operation) = &status.operation {
                print_operation(operation);
            }
            for work in &status.work {
                println!("  {:?} {} (owner {})", work.kind, work.id, work.owner);
            }
        }
        Some(RuntimeResponse::Error(issue)) => println!("{issue}"),
        None => {}
    }
    Ok(())
}
fn print_operation(operation: &ShutdownOperation) {
    println!(
        "Operation {} revision {}: {:?}",
        operation.id, operation.revision, operation.phase
    );
    println!(
        "Remaining: {}. Preserved: {}. Cancellation closed: {}.",
        operation.remaining.len(),
        operation.preserved.len(),
        operation.cancellation_closed
    );
    for work in &operation.remaining {
        println!(
            "  remaining {:?} {} owner {}",
            work.kind, work.id, work.owner
        );
    }
    for work in &operation.preserved {
        println!("  preserved {} owner {}", work.id, work.owner);
    }
    for issue in &operation.issues {
        println!("  issue: {issue}");
    }
    if !operation.phase.terminal() {
        println!(
            "Acceptance is not completion. Use runtime inspect/wait; Force requires an explicit operation revision."
        );
    }
}

pub(crate) async fn run(
    action: RuntimeCommand,
    provider: &ProviderChoice,
    model: Option<&str>,
    profile: Option<&str>,
) -> Result<()> {
    ensure!(
        cfg!(unix),
        "Managed runtime controls are unsupported on this platform"
    );
    let socket = crate::server::socket_path();
    let (report, json) = match action {
        RuntimeCommand::Status { json } => (status(&socket).await?, json),
        RuntimeCommand::Start { json } => {
            let status = start(provider, model, profile).await?;
            (
                Report {
                    socket,
                    live_response: true,
                    coordinator_owned: Some(true),
                    response: Some(RuntimeResponse::Status(status)),
                    confirm_request: None,
                    detail: None,
                },
                json,
            )
        }
        RuntimeCommand::Stop(args) => (
            control(
                &socket,
                RuntimeRequest::Review {
                    options: options(&args),
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Change {
            operation,
            revision,
            options: args,
        } => (
            control(
                &socket,
                RuntimeRequest::ReviewChange {
                    operation,
                    expected_revision: revision,
                    options: options(&args),
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Confirm {
            review,
            request,
            json,
        } => (
            control(&socket, RuntimeRequest::Begin { request, review }).await?,
            json,
        ),
        RuntimeCommand::Inspect { operation, json } => (inspect(&socket, operation).await?, json),
        RuntimeCommand::Cancel(args) => (
            control(
                &socket,
                RuntimeRequest::CancelWait {
                    operation: args.operation,
                    expected_revision: args.revision,
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Retry(args) => (
            control(
                &socket,
                RuntimeRequest::Retry {
                    operation: args.operation,
                    expected_revision: args.revision,
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Force(args) => (
            control(
                &socket,
                RuntimeRequest::Force {
                    operation: args.operation,
                    expected_revision: args.revision,
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Wait {
            operation,
            timeout_seconds,
            json,
        } => {
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(timeout_seconds.into());
            loop {
                let mut report = tokio::time::timeout_at(deadline, inspect(&socket, operation))
                    .await
                    .context(
                        "Runtime wait deadline elapsed; no cancellation or force was performed",
                    )??;
                if let Some(RuntimeResponse::Operation(current)) = &report.response {
                    if current.phase.terminal()
                        && (!matches!(
                            current.phase,
                            ShutdownPhase::Stopped | ShutdownPhase::Forced
                        ) || !local_store(&socket)?.owner_is_live()?)
                    {
                        if matches!(
                            current.phase,
                            ShutdownPhase::Stopped | ShutdownPhase::Forced
                        ) {
                            report.coordinator_owned = Some(false);
                        }
                        print(&report, json)?;
                        ensure!(
                            current.phase != ShutdownPhase::Forced,
                            "Runtime exited by explicit Force with uncertain outcomes; inspect the retained receipt"
                        );
                        return Ok(());
                    }
                    if current.phase == ShutdownPhase::Blocked {
                        print(&report, json)?;
                        anyhow::bail!(
                            "Runtime remains blocked; inspect before explicit Retry, Change or Force"
                        );
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    print(&report, json)?;
                    anyhow::bail!(
                        "Runtime wait deadline elapsed; work was not cancelled or forced"
                    );
                }
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
                )
                .await;
            }
        }
    };
    print(&report, json)
}

pub(crate) async fn start(
    provider: &ProviderChoice,
    model: Option<&str>,
    profile: Option<&str>,
) -> Result<RuntimeStatus> {
    ensure!(
        cfg!(unix),
        "Managed runtime controls are unsupported on this platform"
    );
    super::dispatch::start_server_explicit(provider, model, profile).await?;
    let client = NativeClient::connect(&crate::server::socket_path())
        .await
        .context("Verify explicitly started runtime namespace and capability")?;
    ensure!(
        !client.status.desired_stopped,
        "Runtime is still stopping; Start did not cancel its operation"
    );
    Ok(client.status)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
