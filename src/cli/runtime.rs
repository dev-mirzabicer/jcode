//! Human runtime control over the same native service and durable receipts.
use super::args::{
    RuntimeCommand, RuntimeRecoverCommand, RuntimeRecoverDecision, RuntimeServiceCommand,
    RuntimeStopOptions, RuntimeStrategy, RuntimeTasks,
};
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
    supervision: Option<SupervisionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<crate::runtime_service::ServiceStatus>,
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
    supervision: bool,
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
        let ServerEvent::RuntimeCapabilities {
            id: 1,
            version: Some(1),
            supervision,
        } = read_event(&mut read).await?
        else {
            anyhow::bail!(
                "Connected server does not support runtime_lifecycle_v1; no control was sent"
            );
        };
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
            supervision: supervision == Some(1),
        })
    }
    async fn request(&mut self, request: RuntimeRequest) -> Result<RuntimeResponse> {
        ensure!(
            !request.requires_supervision() || self.supervision,
            "Connected runtime predates runtime_supervision_v1; no control was sent. Upgrade the runtime first"
        );
        let expected = request.clone();
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
        let response = response(read_event(&mut self.read).await?, self.sequence)?;
        ensure!(
            expected.matches_response(&response),
            "Runtime reply kind or logical identity does not match the request; inspect before retrying"
        );
        Ok(response)
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
fn options(args: &RuntimeStopOptions, destination: RuntimeDestination) -> ShutdownOptions {
    ShutdownOptions {
        destination,
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
    let service = service_status(socket);
    match NativeClient::connect(socket).await {
        Ok(mut client) => {
            let supervision = if client.supervision {
                match client.request(RuntimeRequest::Supervision {}).await? {
                    RuntimeResponse::Supervision(supervision) => Some(supervision),
                    _ => None,
                }
            } else {
                None
            };
            Ok(Report {
                socket: socket.into(),
                live_response: true,
                coordinator_owned: Some(true),
                response: Some(RuntimeResponse::Status(client.status)),
                supervision,
                service,
                confirm_request: None,
                detail: None,
            })
        }
        Err(error) => {
            let parent = socket.parent().context("Socket has no parent")?;
            if !parent.try_exists()? {
                return Ok(Report {
                    socket: socket.into(),
                    live_response: false,
                    coordinator_owned: None,
                    response: None,
                    supervision: None,
                    service,
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
                // Recovery items are durable; decisions still need a live runtime.
                supervision: Some(SupervisionStatus {
                    namespace: store.namespace().into(),
                    runtime: String::new(),
                    supervised: false,
                    power: PowerStatus {
                        enabled: crate::config::config().power.prevent_sleep_while_streaming,
                        available: false,
                        active: false,
                        active_work: 0,
                    },
                    recoveries: store.recoveries()?,
                }),
                service,
                confirm_request: None,
                detail: Some(format!(
                    "Live status unavailable; showing durable intent, not proof of live work: {error:#}"
                )),
            })
        }
    }
}

fn service_status(socket: &Path) -> Option<crate::runtime_service::ServiceStatus> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    crate::runtime_service::status(socket).ok()
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
                supervision: None,
                service: None,
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
                supervision: None,
                service: None,
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
        supervision: None,
        service: None,
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
            println!(
                "{} review {} for runtime {}",
                match review.options.destination {
                    RuntimeDestination::Stopped => "Stop",
                    RuntimeDestination::Restart => "Restart",
                },
                review.id,
                review.runtime
            );
            if review.options.destination == RuntimeDestination::Restart {
                println!(
                    "After verified quiescence a new runtime replaces this one. Turns this interrupts continue automatically there; idle sessions stay idle."
                );
            }
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
            println!("Nothing has begun from this review. Confirm the same --socket with:");
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
        Some(RuntimeResponse::Recovery(item)) => print_recovery(item),
        Some(RuntimeResponse::Supervision(_)) => {}
        Some(RuntimeResponse::Error(issue)) => println!("{issue}"),
        None => {}
    }
    if let Some(supervision) = &report.supervision {
        if !supervision.runtime.is_empty() {
            println!(
                "Supervised by login service: {}. Power: enabled {}, available {}, assertion held {}, runtime work {}.",
                supervision.supervised,
                supervision.power.enabled,
                supervision.power.available,
                supervision.power.active,
                supervision.power.active_work
            );
        }
        let unresolved: Vec<_> = supervision
            .recoveries
            .iter()
            .filter(|item| item.resolved.is_none())
            .collect();
        if unresolved.is_empty() {
            println!("No interrupted turns await a recovery decision.");
        }
        for item in unresolved {
            print_recovery(item);
        }
    }
    if let Some(service) = &report.service {
        print_service_status(service);
    }
    Ok(())
}

fn print_recovery(item: &RecoveryItem) {
    println!(
        "Recovery {} revision {}: session {} turn interrupted by {:?} (detected {}).",
        item.id, item.revision, item.session, item.cause, item.detected_at
    );
    for execution in &item.executions {
        println!(
            "  unfinished {} {} ({}){}",
            execution.tool,
            execution.id,
            execution.state,
            if execution.live_owner {
                ", still running under its own owner; do not repeat it"
            } else {
                ""
            }
        );
    }
    match &item.resolved {
        None => println!(
            "  Decide with `jcode runtime recover continue {}` or `jcode runtime recover leave {}`.",
            item.id, item.id
        ),
        Some(resolved) => println!(
            "  Resolved {:?} at {}.",
            resolved.resolution, resolved.resolved_at
        ),
    }
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
                    supervision: None,
                    service: None,
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
                    options: options(&args, RuntimeDestination::Stopped),
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Restart(args) => (
            control(
                &socket,
                RuntimeRequest::Review {
                    options: options(&args, RuntimeDestination::Restart),
                },
            )
            .await?,
            args.json,
        ),
        RuntimeCommand::Change {
            operation,
            revision,
            options: args,
        } => {
            // A change keeps the operation's destination: changing how a
            // restart quiesces never silently turns it into a Stop.
            let destination = match inspect(&socket, operation).await?.response {
                Some(RuntimeResponse::Operation(current)) => current.review.options.destination,
                _ => anyhow::bail!(
                    "Operation {operation} could not be inspected for its destination"
                ),
            };
            (
                control(
                    &socket,
                    RuntimeRequest::ReviewChange {
                        operation,
                        expected_revision: revision,
                        options: options(&args, destination),
                    },
                )
                .await?,
                args.json,
            )
        }
        RuntimeCommand::Recover { action, json } => match action {
            None => (status(&socket).await?, json),
            Some(RuntimeRecoverCommand::Continue(decision)) => {
                let json = decision.json;
                (
                    recover(&socket, decision, RecoveryDecision::Continue).await?,
                    json,
                )
            }
            Some(RuntimeRecoverCommand::Leave(decision)) => {
                let json = decision.json;
                (
                    recover(&socket, decision, RecoveryDecision::LeaveStopped).await?,
                    json,
                )
            }
        },
        RuntimeCommand::Service { action } => return service(&socket, action, provider),
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
                // A verified restart finishes when a new incarnation serves the
                // namespace, not when the old one has merely exited.
                if let Ok(Some(current)) = local_store(&socket).and_then(|store| {
                    store
                        .inspect(operation)
                        .map(|op| Some(op).filter(|op| op.phase == ShutdownPhase::Stopped))
                }) && current.review.options.destination == RuntimeDestination::Restart
                {
                    if let Ok(client) = NativeClient::connect(&socket).await
                        && client.status.runtime.as_deref() != Some(current.review.runtime.as_str())
                    {
                        let report = Report {
                            socket: socket.clone(),
                            live_response: true,
                            coordinator_owned: Some(true),
                            response: Some(RuntimeResponse::Operation(current)),
                            supervision: None,
                            service: None,
                            confirm_request: None,
                            detail: Some(format!(
                                "Restarted: runtime {} now serves this namespace",
                                client.status.runtime.unwrap_or_default()
                            )),
                        };
                        print(&report, json)?;
                        return Ok(());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "Restart quiesced, but no replacement runtime answered before the deadline; inspect `jcode runtime status` and the service log"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
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

async fn recover(
    socket: &Path,
    decision: RuntimeRecoverDecision,
    choice: RecoveryDecision,
) -> Result<Report> {
    let mut client = NativeClient::connect(socket)
        .await
        .context("Recovery decisions need the live runtime; start it with `jcode runtime start`")?;
    let expected_revision = match decision.revision {
        Some(revision) => revision,
        None => {
            let RuntimeResponse::Supervision(supervision) =
                client.request(RuntimeRequest::Supervision {}).await?
            else {
                anyhow::bail!("Runtime did not return supervision status");
            };
            let item = supervision
                .recoveries
                .into_iter()
                .find(|item| item.id == decision.item)
                .context("Unknown recovery item")?;
            ensure!(
                item.resolved.is_none(),
                "Recovery item {} was already resolved",
                item.id
            );
            item.revision
        }
    };
    let request = decision.request.unwrap_or_default();
    let response = client
        .request(RuntimeRequest::Recover {
            item: decision.item,
            expected_revision,
            request,
            decision: choice,
        })
        .await
        .with_context(|| {
            format!(
                "Recovery reply uncertain; retry with --request {request} to replay this decision"
            )
        })?;
    Ok(Report {
        socket: socket.into(),
        live_response: true,
        coordinator_owned: Some(true),
        response: Some(response),
        supervision: None,
        service: None,
        confirm_request: None,
        detail: None,
    })
}

fn service(socket: &Path, action: RuntimeServiceCommand, _provider: &ProviderChoice) -> Result<()> {
    use crate::runtime_service as svc;
    ensure!(
        cfg!(target_os = "macos"),
        "Login-service supervision is supported on macOS only; start the runtime manually elsewhere"
    );
    match action {
        RuntimeServiceCommand::Status { json } => {
            let status = svc::status(socket)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_service_status(&status);
            }
        }
        RuntimeServiceCommand::Install { confirm, json } => {
            let program = crate::build::shared_server_binary_path()?;
            let plan = svc::plan(socket, &program, &std::env::var("PATH").unwrap_or_default())?;
            match confirm {
                None => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        print_service_plan(&plan);
                    }
                }
                Some(digest) => {
                    let status = svc::install(&plan, &digest)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        print_service_status(&status);
                        println!(
                            "Installed. At login the service starts this runtime unless it was intentionally stopped. An already running unmanaged runtime keeps serving; the service waits and takes over when it exits (for example after `jcode runtime restart`)."
                        );
                    }
                }
            }
        }
        RuntimeServiceCommand::Uninstall { json } => {
            let current = svc::status(socket)?;
            ensure!(
                current.pid.is_none(),
                "The supervised runtime is running (pid {}); stop it first with a reviewed `jcode runtime stop`, then uninstall",
                current.pid.unwrap_or_default()
            );
            let status = svc::uninstall(socket)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_service_status(&status);
                println!("Uninstalled. Start the runtime manually with `jcode runtime start`.");
            }
        }
    }
    Ok(())
}

fn print_service_plan(plan: &crate::runtime_service::ServicePlan) {
    println!("Login service plan for {}", plan.socket.display());
    println!("  Label: {}", plan.label);
    println!("  Definition: {}", plan.definition.display());
    println!(
        "  Program: {} {}",
        plan.program.display(),
        plan.arguments.join(" ")
    );
    for (key, value) in &plan.environment {
        println!("  {key}={value}");
    }
    println!("  Log: {}", plan.log.display());
    println!(
        "  Starts at login (unless intentionally stopped); restarts after an unexpected exit, at most every {}s; never after an intentional Stop.",
        plan.throttle_interval_seconds
    );
    println!(
        "  On logout or unload the runtime receives SIGTERM and quiesces for up to {}s (launchd allows {}s before killing it); interrupted turns wait for your recovery decision.",
        crate::server::shutdown::EXTERNAL_SIGNAL_QUIESCENCE_SECONDS,
        plan.exit_timeout_seconds
    );
    println!("Nothing was written. Install exactly this plan with:");
    println!("  jcode runtime service install --confirm {}", plan.digest);
}

fn print_service_status(status: &crate::runtime_service::ServiceStatus) {
    println!(
        "Service {}: installed {}, loaded {}, pid {}, last exit {}.",
        status.label,
        status.installed,
        status.loaded,
        status
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "none".into()),
        status.last_exit.as_deref().unwrap_or("none")
    );
    println!("Definition: {}", status.definition.display());
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
