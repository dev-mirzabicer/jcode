//! Native subprocess ownership only. These entry points never create an Agent,
//! choose a model, load instructions or delegate implementation work.
use super::command::{CommandGate, CommandSpec};
use super::process::ProcessIdentity;
use super::*;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::time::Duration;

pub const WORKER_ARGUMENT: &str = "__jcode-command-worker";
pub const CHILD_ARGUMENT: &str = "__jcode-command-child";

static EXECUTABLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn register_current_executable() -> Result<()> {
    let executable = std::env::current_exe()?;
    if let Some(existing) = EXECUTABLE.get() {
        ensure!(
            *existing == executable,
            "Native command executable changed within a runtime"
        );
    } else {
        let _ = EXECUTABLE.set(executable);
    }
    Ok(())
}

fn worker_program(id: &str) -> Result<std::process::Command> {
    #[cfg(test)]
    {
        let mut program = std::process::Command::new(std::env::current_exe()?);
        program
            .args([
                "--exact",
                "execution::command_worker::tests::worker_fixture",
                "--nocapture",
            ])
            .env("JCODE_WORKER_FIXTURE", id);
        Ok(program)
    }
    #[cfg(not(test))]
    {
        let executable = EXECUTABLE
            .get()
            .context("This host has not registered native command-worker support")?;
        let mut program = std::process::Command::new(executable);
        program.arg(WORKER_ARGUMENT).arg(id);
        Ok(program)
    }
}

pub(crate) async fn launch(
    request: command_handoff::CommandRequest,
    ctx: ToolContext,
) -> Result<ToolOutput> {
    let identity = ctx
        .invocation
        .identity
        .as_ref()
        .context("Native command requires an owned invocation")?;
    let id = identity.id.clone();
    let owner = identity.owner.clone();
    let mut program = worker_program(&id)?;
    let root = crate::storage::jcode_dir()?;
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
    let record = store
        .inspect(&id)?
        .context("Command invocation is unavailable")?;
    let prepared = {
        let store = store.clone();
        let record = record.clone();
        tokio::task::spawn_blocking(move || store.prepare_command(&record, request)).await?
    };
    prepared?;
    let log_path = store
        .root()
        .join("commands")
        .join(format!("{id}.worker.log"));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    options.mode(0o600);
    let log = options.open(&log_path)?;
    program
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    let mut child = crate::platform::spawn_detached(&mut program)?;
    let mut stopped = false;
    let mut ready = false;
    let mut last_progress = 0;
    loop {
        let current = {
            let store = store.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                store
                    .inspect(&id)?
                    .context("Command invocation disappeared")
            })
            .await??
        };
        if current.background
            && let Some(progress) = &current.progress
            && progress.sequence > last_progress
        {
            last_progress = progress.sequence;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::BackgroundTaskProgress(
                crate::bus::BackgroundTaskProgressEvent {
                    task_id: id.clone(),
                    tool_name: current.tool.clone(),
                    display_name: None,
                    session_id: current.session_id.clone(),
                    progress: progress.value.clone(),
                },
            ));
        }
        if current.state.terminal() {
            if current.output_path.is_some()
                && let Some(ready) = &ctx.invocation.ready
            {
                ready.mark();
            }
            let store = store.clone();
            let target = ctx
                .invocation
                .output_target
                .context("Missing command presentation target")?;
            return tokio::task::spawn_blocking(move || store.result(&current, target)).await?;
        }
        if current.owner != owner
            && current
                .output_path
                .as_ref()
                .is_some_and(|path| path.is_file())
        {
            if !ready {
                if let Some(signal) = &ctx.invocation.ready {
                    signal.mark();
                }
                ready = true;
            }
            if !stopped
                && let Some(cause) = ctx
                    .graceful_shutdown_signal
                    .as_ref()
                    .and_then(InterruptSignal::stop_cause)
            {
                if cause == StopCause::ReloadQuiescence {
                    let reply =
                        runtime::control_in_store(&store, &id, ControlOperation::Background)
                            .await?;
                    ensure!(
                        matches!(
                            reply,
                            ControlReply::Accepted { changed: true }
                                | ControlReply::Snapshot { .. }
                        ),
                        "Command could not be preserved for reload"
                    );
                    return super::background_handoff(&ctx).await;
                }
                let reply =
                    runtime::control_in_store(&store, &id, ControlOperation::Stop { cause })
                        .await?;
                if let ControlReply::Unavailable { message } = reply {
                    anyhow::bail!("{message}");
                }
                stopped = true;
            }
        }
        if let Some(status) = child.try_wait()? {
            let current = store
                .inspect(&id)?
                .context("Command invocation disappeared")?;
            if current.state.terminal() {
                continue;
            }
            anyhow::bail!(
                "Native command owner exited ({status}) without a terminal receipt. Startup log: {}. Do not repeat uncertain effects.",
                log_path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn child_main(id: &str) -> Result<()> {
    let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    let identity = ProcessIdentity::capture(std::process::id())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while store.command_process(id)?.is_none() {
        let record = store
            .inspect(id)?
            .context("Command invocation disappeared before exec")?;
        ensure!(
            !record.state.terminal() && record.stop_cause.is_none(),
            "Command was stopped before exec"
        );
        ensure!(
            store
                .runtime_endpoint(&record.owner)?
                .context("Missing command worker")?
                .has_live_lease()?,
            "Command worker ended before process registration"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "Command process was not registered before the startup deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let request = store.authorize_command_exec(id, &identity)?;
    let error = std::process::Command::new("bash")
        .arg("-c")
        .arg(request.command)
        .current_dir(request.working_dir)
        .exec();
    Err(error).context("Exec the authorized command")
}

#[cfg(test)]
#[path = "command_worker_tests.rs"]
mod tests;

pub async fn worker_main(id: &str) -> Result<()> {
    let program = std::env::current_exe()?;
    let mut child = tokio::process::Command::new(program);
    child.arg(CHILD_ARGUMENT).arg(id);
    worker_with_child(id, child).await
}

async fn worker_with_child(id: &str, child: tokio::process::Command) -> Result<()> {
    let root = crate::storage::jcode_dir()?;
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
    let runtime = runtime::ensure_running(&store).await?;
    let claim = {
        let store = store.clone();
        let id = id.to_string();
        let endpoint = runtime.endpoint.clone();
        tokio::task::spawn_blocking(move || store.claim_command(&id, &endpoint)).await??
    };
    let record = claim.record;
    let setup = {
        let store = store.clone();
        let record = record.clone();
        let config = claim.request.storage.clone();
        tokio::task::spawn_blocking(move || {
            let input = store.invocation_input(&record.id)?;
            let capture = Arc::new(Capture::create(store, record, config)?);
            Ok::<_, anyhow::Error>((input, capture))
        })
        .await?
    };
    let (input, capture) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            let store = store.clone();
            let mut record = record.clone();
            record.state = RunState::Failed;
            tokio::task::spawn_blocking(move || store.finish(&record)).await??;
            return Err(error.context("Command worker preparation failed before user code started"));
        }
    };
    let (commands, mut requests) = mpsc::unbounded_channel();
    let (result, result_rx) = watch::channel(None);
    let run = Arc::new(LiveRun {
        owns_execution: AtomicBool::new(true),
        store: store.clone(),
        runtime,
        invocation: input,
        stop: InterruptSignal::new(),
        background: AtomicBool::new(record.background),
        ready: Default::default(),
        delivery: Default::default(),
        commands,
        result: result_rx,
    });
    let key = (store.root().to_path_buf(), record.id.clone());
    {
        let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            !live.contains_key(&key),
            "Command is already live in this worker"
        );
        live.insert(key.clone(), run.clone());
    }
    let _registration = LiveRegistration(key);
    let current = {
        let store = store.clone();
        let id = record.id.clone();
        tokio::task::spawn_blocking(move || {
            store
                .inspect(&id)?
                .context("Command disappeared during setup")
        })
        .await??
    };
    if let Some(cause) = current.stop_cause {
        run.stop.fire_with_cause(cause);
    }
    let spec = CommandSpec {
        command: claim.request.command,
        working_dir: claim.request.working_dir,
        timeout: claim.request.timeout_ms.map(Duration::from_millis),
    };
    let gate = CommandGate {
        program: child,
        store: store.clone(),
        run_id: record.id.clone(),
        owner: record.owner.clone(),
    };
    let operation = command::run_gated(spec, capture.clone(), run.stop.clone(), gate);
    tokio::pin!(operation);
    let mut parent_tick = tokio::time::interval(Duration::from_millis(250));
    parent_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut recorded_stop = false;
    let outcome = loop {
        tokio::select! {
            biased;
            outcome=&mut operation=>break outcome,
            _=run.stop.notified(),if !recorded_stop=>{
                let store=store.clone();let id=record.id.clone();let owner=record.owner.clone();let cause=run.stop.stop_cause().unwrap_or(StopCause::HumanCancellation);
                let persisted=tokio::task::spawn_blocking(move||store.request_stop(&id,&owner,cause)).await;
                if !matches!(persisted,Ok(Ok(true))) {crate::logging::warn("Command stop request could not be persisted; finalization will report storage state");}
                recorded_stop=true;
            }
            Some(Command::Background(reply))=requests.recv()=>{
                let answer=if run.stop.is_set(){Err("Command is stopping".to_string())}else{
                    let store=store.clone();let id=record.id.clone();let owner=record.owner.clone();
                    match tokio::task::spawn_blocking(move||store.promote(&id,&owner)).await {
                        Ok(Ok(true))=>{run.background.store(true,Ordering::SeqCst);Ok(true)},
                        Ok(Ok(false))=>Ok(false),Ok(Err(error))=>Err(error.to_string()),Err(error)=>Err(error.to_string()),
                    }
                };let _=reply.send(answer);
            }
            _=parent_tick.tick(),if !run.background.load(Ordering::SeqCst) && !run.stop.is_set()=>{
                let parent=claim.parent.clone();
                if !matches!(tokio::task::spawn_blocking(move||parent.has_live_lease()).await,Ok(Ok(true))) {
                    run.stop.fire_with_cause(StopCause::OwnerCrash);
                }
            }
            _=termination.recv(),if !run.stop.is_set()=>{run.stop.fire_with_cause(StopCause::HumanCancellation);}
            _=interrupt.recv(),if !run.stop.is_set()=>{run.stop.fire_with_cause(StopCause::HumanCancellation);}
        }
    };
    let store_for_finish = store.clone();
    let id = record.id.clone();
    let finished = tokio::task::spawn_blocking(move || {
        let (mut output, state) = match outcome {
            Ok(outcome) => {
                let state = match outcome.stop_cause {
                    Some(
                        StopCause::HumanCancellation | StopCause::ParentForegroundCancellation,
                    ) => RunState::Cancelled,
                    Some(StopCause::ReloadQuiescence | StopCause::OwnerCrash) => {
                        RunState::Interrupted
                    }
                    None if outcome.output.is_error => RunState::Failed,
                    None => RunState::Completed,
                };
                (outcome.output, state)
            }
            Err(error) => {
                let _ = capture.write(
                    OutputStream::Text,
                    format!("\n[Command execution failed]\n{error:#}\n").as_bytes(),
                );
                let mut output = ToolOutput::new("").with_error(true);
                output.source = OutputSource::Retained(capture.reference()?);
                (output, RunState::Failed)
            }
        };
        output.title = claim.request.title;
        let state = if !store_for_finish.command_was_started(&id)? {
            match store_for_finish
                .inspect(&id)?
                .and_then(|record| record.stop_cause)
            {
                Some(StopCause::HumanCancellation | StopCause::ParentForegroundCancellation) => {
                    RunState::Cancelled
                }
                Some(StopCause::ReloadQuiescence | StopCause::OwnerCrash) => RunState::Interrupted,
                None => RunState::Failed,
            }
        } else {
            state
        };
        capture.seal(output, state)?;
        Ok::<_, anyhow::Error>(state)
    })
    .await?;
    match finished {
        Ok(state) => {
            result.send_replace(Some(Arc::new(Completion {
                output: ToolOutput::new(""),
                failed: state != RunState::Completed,
            })));
            Ok(())
        }
        Err(error) => {
            result.send_replace(Some(Arc::new(Completion {
                output: ToolOutput::new(
                    "Command finalization failed; retained files remain inspectable",
                ),
                failed: true,
            })));
            Err(error)
        }
    }
}
