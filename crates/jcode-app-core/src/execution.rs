//! Live execution ownership. Persistence and output bytes stay with base.
use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use jcode_agent_runtime::InterruptSignal;
pub use jcode_base::execution::*;
use jcode_tool_core::{OutputCapture, OutputStream, ToolContext};
use jcode_tool_types::{OutputSource, StopCause, ToolOutput};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot, watch};
#[cfg(unix)]
pub mod command;
#[cfg(unix)]
pub mod command_worker;
mod runtime;
pub use runtime::{ControlOperation, ControlReply, control};

type Producer = Box<dyn FnOnce(ToolContext) -> BoxFuture<'static, Result<ToolOutput>> + Send>;
type Key = (PathBuf, String);
static LIVE: LazyLock<Mutex<HashMap<Key, Arc<LiveRun>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct LiveRegistration(Key);
impl Drop for LiveRegistration {
    fn drop(&mut self) {
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}
struct AbortProducer(tokio::task::AbortHandle);
impl Drop for AbortProducer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct LiveRun {
    owns_execution: AtomicBool,
    store: ExecutionStore,
    runtime: Arc<runtime::RuntimeHandle>,
    invocation: Invocation,
    stop: InterruptSignal,
    background: AtomicBool,
    ready: jcode_tool_core::ExecutionReady,
    delivery: tokio::sync::Mutex<()>,
    commands: mpsc::UnboundedSender<Command>,
    result: watch::Receiver<Option<Arc<Completion>>>,
}
enum Command {
    Background(oneshot::Sender<Result<bool, String>>),
}
struct Completion {
    output: ToolOutput,
    failed: bool,
}

#[derive(Debug)]
pub struct CapturedToolError {
    pub output: ToolOutput,
}
impl std::fmt::Display for CapturedToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.output.output)
    }
}
impl std::error::Error for CapturedToolError {}

struct ForegroundWait {
    run: Arc<LiveRun>,
    armed: bool,
}
impl Drop for ForegroundWait {
    fn drop(&mut self) {
        if self.armed && !self.run.background.load(Ordering::SeqCst) {
            self.run
                .stop
                .fire_with_cause(StopCause::ParentForegroundCancellation);
        }
    }
}

pub fn invocation_id(ctx: &ToolContext) -> String {
    invocation(ctx, "", serde_json::Value::Null).id()
}
pub fn invocation(ctx: &ToolContext, tool: &str, input: serde_json::Value) -> Invocation {
    let mut path = ctx.invocation.ancestors.clone();
    path.push(ctx.tool_call_id.clone());
    Invocation {
        session_id: ctx.session_id.clone(),
        message_id: ctx.message_id.clone(),
        call_path: path,
        tool: tool.to_string(),
        input,
        working_dir: ctx.working_dir.clone(),
        received_result_digest: None,
    }
}

/// The original execution owns work. Replayed transport waiters attach without
/// gaining cancellation ownership or invoking their supplied producer again.
pub(crate) async fn execute(
    invocation: Invocation,
    ctx: ToolContext,
    target: NonZeroUsize,
    producer: Producer,
) -> Result<ToolOutput> {
    let root = crate::storage::jcode_dir()?;
    let policy = ctx.invocation.policy.clone();
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
    let runtime = runtime::ensure_running(&store).await?;
    let key = (store.root().to_path_buf(), invocation.id());
    let (run, created) = {
        let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(run) = live.get(&key) {
            ensure!(
                run.invocation == invocation,
                "Invocation replay conflicts with its original input"
            );
            (Arc::clone(run), false)
        } else {
            let (commands, receiver) = mpsc::unbounded_channel();
            let (result, subscription) = watch::channel(None);
            let run = Arc::new(LiveRun {
                owns_execution: AtomicBool::new(false),
                store: store.clone(),
                runtime,
                invocation,
                stop: InterruptSignal::new(),
                background: AtomicBool::new(policy.background),
                ready: Default::default(),
                delivery: Default::default(),
                commands,
                result: subscription,
            });
            live.insert(key.clone(), Arc::clone(&run));
            let owned = Arc::clone(&run);
            let registration = LiveRegistration(key.clone());
            tokio::spawn(async move {
                let _registration = registration;
                let completion =
                    supervise(store, Arc::clone(&owned), receiver, ctx, target, producer).await;
                let completion = match completion {
                    Ok(completion) => completion,
                    Err(error) => Completion {
                        output: ToolOutput::new(format!(
                            "Execution storage/control failed for {}: {error:#}. Do not repeat uncertain effects.",
                            key.1
                        )),
                        failed: true,
                    },
                };
                // Metadata and captured files have their own durable truth even
                // when the original caller has stopped waiting.
                result.send_replace(Some(Arc::new(completion)));
            });
            (run, true)
        }
    };
    let mut wait = ForegroundWait {
        run: Arc::clone(&run),
        armed: created,
    };
    let mut receiver = run.result.clone();
    let mut deadline = policy.background.then(tokio::time::Instant::now);
    loop {
        if deadline.is_none() && run.ready.is_ready() {
            deadline = policy
                .foreground_timeout
                .map(|duration| tokio::time::Instant::now() + duration);
        }
        let completed = receiver.borrow_and_update().clone();
        if let Some(completed) = completed {
            wait.armed = false;
            if policy.background && run.ready.is_ready() && !completed.failed {
                return background_receipt(run.clone(), &policy, target).await;
            }
            if completed.failed {
                return Err(CapturedToolError {
                    output: completed.output.clone(),
                }
                .into());
            }
            return Ok(completed.output.clone());
        }
        tokio::select! {
            _=run.ready.wait(),if !run.ready.is_ready()=>{}
            changed=receiver.changed()=>{changed.context("Execution owner closed before a result was available")?;}
            _=async {if let Some(at)=deadline{tokio::time::sleep_until(at).await}else{std::future::pending::<()>().await}}=>{
                while !run.ready.is_ready() {
                    if receiver.borrow_and_update().is_some(){break;}
                    tokio::select!{_=run.ready.wait()=>{},changed=receiver.changed()=>{changed?;}}
                }
                if run.ready.is_ready(){
                    let receipt=background_receipt(run.clone(),&policy,target).await?;
                    wait.armed=false;
                    return Ok(receipt);
                }
            }
        }
    }
}

async fn background_receipt(
    run: Arc<LiveRun>,
    policy: &jcode_tool_core::ExecutionPolicy,
    target: NonZeroUsize,
) -> Result<ToolOutput> {
    let _delivery = run.delivery.lock().await;
    let id = run.invocation.id();
    let existing = {
        let store = run.store.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || store.acceptance_result(&id)).await??
    };
    if let Some(output) = existing {
        return Ok(output);
    }
    if !run.background.load(Ordering::SeqCst) && !promote(&id).await? {
        let store = run.store.clone();
        let id = id.clone();
        let output = tokio::task::spawn_blocking(move || {
            store.result(
                &store
                    .inspect(&id)?
                    .context("Invocation disappeared during promotion")?,
                target,
            )
        })
        .await??;
        if output.is_error {
            return Err(CapturedToolError { output }.into());
        }
        return Ok(output);
    }
    let store = run.store.clone();
    let requester = run.runtime.endpoint.id.clone();
    let receipt_id = id.clone();
    let control = Arc::new(BackgroundControl { run: run.clone() });
    let completion = run.clone();
    let handle = tokio::spawn(async move {
        let mut receiver = completion.result.clone();
        loop {
            if let Some(completed) = receiver.borrow_and_update().clone() {
                if completed.failed {
                    return Err(CapturedToolError {
                        output: completed.output.clone(),
                    }
                    .into());
                }
                return Ok(completed.output.clone());
            }
            receiver.changed().await?;
        }
    });
    crate::background::global()
        .adopt_controlled_with_delivery(
            &run.invocation.tool,
            &run.invocation.session_id,
            handle,
            control,
            (policy.notify, policy.wake),
        )
        .await?;
    let output =
        tokio::task::spawn_blocking(move || store.background_acceptance(&receipt_id, &requester))
            .await??;
    Ok(output)
}

#[cfg(unix)]
pub(crate) async fn background_handoff(ctx: &ToolContext) -> Result<ToolOutput> {
    let root = crate::storage::jcode_dir()?.join("execution");
    let run = LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(root, invocation_id(ctx)))
        .cloned()
        .context("Background handoff lost its live invocation")?;
    background_receipt(
        run,
        &ctx.invocation.policy,
        ctx.invocation
            .output_target
            .context("Missing presentation target")?,
    )
    .await
}

async fn supervise(
    store: ExecutionStore,
    run: Arc<LiveRun>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut ctx: ToolContext,
    target: NonZeroUsize,
    producer: Producer,
) -> Result<Completion> {
    let _in_flight = crate::tool::inflight::mark_tool_in_flight(&run.invocation);
    let owner = run.runtime.endpoint.id.clone();
    let preparation = {
        let store = store.clone();
        let input = run.invocation.clone();
        let owner = owner.clone();
        tokio::task::spawn_blocking(move || store.prepare(&input, &owner)).await??
    };
    let record = match preparation {
        PreparedInvocation::New(record) => {
            run.owns_execution.store(true, Ordering::SeqCst);
            record
        }
        PreparedInvocation::Existing(mut record) => {
            let source = store.clone();
            let prior = record.clone();
            let accepted = tokio::task::spawn_blocking(move || -> Result<Option<ToolOutput>> {
                if let Some(output) = source.acceptance_result(&prior.id)? {
                    return Ok(Some(output));
                }
                if source.background_delivery(&prior.id)?.is_some() {
                    return source
                        .background_acceptance(&prior.id, &prior.owner)
                        .map(Some);
                }
                Ok(None)
            })
            .await??;
            if let Some(output) = accepted {
                return Ok(Completion {
                    output,
                    failed: false,
                });
            }
            if !record.state.terminal() {
                if let Some(recovered) = store.recover_lost_owner(&record.id).await? {
                    record = recovered;
                } else {
                    // This caller is attaching to earlier work, not creating it.
                    // Stop cancels only this wait and must not signal that owner.
                    let waiting = crate::execution::runtime::control_in_store(
                        &store,
                        &record.id,
                        ControlOperation::Wait,
                    );
                    let reply = tokio::select! {
                        reply=waiting=>reply?,
                        _=run.stop.notified()=>anyhow::bail!("Stopped waiting for existing execution {}; its work was not repeated or cancelled",record.id),
                        _=async{if let Some(parent)=&ctx.graceful_shutdown_signal{parent.notified().await}else{std::future::pending::<()>().await}}=>anyhow::bail!("Stopped waiting for existing execution {}; its work was not repeated or cancelled",record.id),
                    };
                    record = match reply {
                        ControlReply::Snapshot { record } if record.state.terminal() => *record,
                        ControlReply::Unavailable { message } => anyhow::bail!("{message}"),
                        _ => anyhow::bail!(
                            "Existing execution has no proven terminal receipt; inspect its owner rather than repeating effects"
                        ),
                    };
                }
            }
            return tokio::task::spawn_blocking(move || {
                let failed = record.state != RunState::Completed;
                Ok(Completion {
                    output: store.result(&record, target)?,
                    failed,
                })
            })
            .await?;
        }
    };
    let capture_result = {
        let store = store.clone();
        let record = record.clone();
        let config = crate::config::config().output.storage.clone();
        let mode = ctx.invocation.policy.capture;
        let background = ctx.invocation.policy.background;
        tokio::task::spawn_blocking(move || {
            store.start(&record.id, &record.owner)?;
            if background {
                store.promote(&record.id, &record.owner)?;
            }
            if mode != jcode_tool_core::CaptureMode::Complete {
                Ok(None)
            } else {
                Capture::create(store, record, config).map(|capture| Some(Arc::new(capture)))
            }
        })
        .await?
    };
    let capture = match capture_result {
        Ok(capture) => capture,
        Err(error) => {
            let store = store.clone();
            let id = record.id.clone();
            tokio::task::spawn_blocking(move || {
                let mut record = store
                    .inspect(&id)?
                    .context("Missing failed allocation invocation")?;
                record.state = RunState::Failed;
                record.complete = false;
                store.finish(&record)
            })
            .await??;
            return Err(
                error.context("Output capture could not be prepared; the producer was not started")
            );
        }
    };
    let parent = ctx.graceful_shutdown_signal.clone();
    let native_command =
        ctx.invocation.policy.capture == jcode_tool_core::CaptureMode::NativeCommand;
    let cooperative_stop = ctx.invocation.policy.cooperative_stop;
    ctx.invocation.identity = Some(jcode_tool_core::InvocationIdentity {
        id: record.id.clone(),
        owner: record.owner.clone(),
    });
    ctx.invocation.ready = Some(run.ready.clone());
    ctx.graceful_shutdown_signal = Some(run.stop.clone());
    ctx.invocation.capture = capture.clone().map(|value| value as Arc<dyn OutputCapture>);
    let stop_before_start = run.stop.stop_cause().or_else(|| {
        if run.background.load(Ordering::SeqCst) {
            None
        } else {
            parent.as_ref().and_then(InterruptSignal::stop_cause)
        }
    });
    let (result, stopping) = if let Some(cause) = stop_before_start {
        (
            Err(anyhow::anyhow!(
                "{} before producer start",
                cause.description()
            )),
            Some(cause),
        )
    } else {
        let mut task = tokio::spawn(producer(ctx));
        let _owned_producer = AbortProducer(task.abort_handle());
        let mut stopping = None;
        let mut abort_at = None;
        loop {
            tokio::select! {
                biased;
                result=&mut task=>break (result.unwrap_or_else(|error|Err(anyhow::anyhow!("Owned tool task ended: {error}"))),stopping),
                _=run.stop.notified(),if stopping.is_none()=>{
                    let cause=run.stop.stop_cause().unwrap_or(StopCause::HumanCancellation);
                    stopping=Some(cause);
                    let store=store.clone();let id=record.id.clone();let owner=owner.clone();
                    let persisted=tokio::task::spawn_blocking(move||if native_command {Ok(true)}else{store.request_stop(&id,&owner,cause)}).await;
                    if !matches!(persisted,Ok(Ok(true))) {crate::logging::warn("Could not persist an owned tool stop request; terminal publication will recheck storage");}
                    if !cooperative_stop {abort_at=Some(tokio::time::Instant::now()+std::time::Duration::from_millis(750));}
                }
                _=async {if let Some(parent)=&parent {parent.notified().await}else{std::future::pending::<()>().await}},if stopping.is_none() && !run.background.load(Ordering::SeqCst)=>{
                    run.stop.fire_with_cause(parent.as_ref().and_then(InterruptSignal::stop_cause).unwrap_or(StopCause::ParentForegroundCancellation));
                }
                _=async {if let Some(at)=abort_at {tokio::time::sleep_until(at).await}else{std::future::pending::<()>().await}}=>{
                    task.abort();abort_at=None;
                    // Retain the join and live entry until actual quiescence.
                    // Blocking producers are not falsely marked cancelled.
                }
                Some(Command::Background(reply))=commands.recv()=>{
                    let result=if stopping.is_some(){Err("Invocation is already stopping".to_string())}else{
                        let store=store.clone();let id=record.id.clone();let owner=owner.clone();
                        match tokio::task::spawn_blocking(move||store.promote(&id,&owner)).await {
                            Ok(Ok(true))=>{run.background.store(true,Ordering::SeqCst);Ok(true)},
                            Ok(Ok(false))=>Ok(false),
                            Ok(Err(error))=>Err(error.to_string()),Err(error)=>Err(error.to_string()),
                        }
                    };
                    let _=reply.send(result);
                }
            }
        }
    };
    let state = match stopping {
        Some(StopCause::HumanCancellation | StopCause::ParentForegroundCancellation) => {
            RunState::Cancelled
        }
        Some(StopCause::ReloadQuiescence | StopCause::OwnerCrash) => RunState::Interrupted,
        None if !result.as_ref().is_ok_and(|output| !output.is_error) => RunState::Failed,
        None => RunState::Completed,
    };
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        if native_command {
            let actual=store.inspect(&record.id)?.context("Native command invocation disappeared")?;
            if actual.owner!=record.owner {
                ensure!(store.is_command_handoff(&record.id,&record.owner,&actual.owner)?,"Native command result belongs to an unverified owner");
                if let Ok(output)=&result && matches!(output.source,OutputSource::Acceptance(_)) {
                    let output=store.acceptance_result(&record.id)?.context("Native command acceptance was not persisted")?;
                    return Ok(Completion{output,failed:false});
                }
                ensure!(actual.state.terminal(),"Native command remains owned and active; inspect run {} rather than repeating its effects",record.id);
                let output=store.result(&actual,target)?;
                return Ok(Completion{output,failed:actual.state!=RunState::Completed});
            }
        }
        if let Some(cause) = stopping {
            ensure!(
                store.request_stop(&record.id, &record.owner, cause)?,
                "Stop outcome lost invocation ownership"
            );
        }
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let text = format!("\n[Execution error]\n{error:#}");
                if let Some(capture) = &capture {
                    // A failed capture keeps its earlier prefix. Sealing reports
                    // that failure rather than converting it to success.
                    let _ = capture.write(OutputStream::Text, text.as_bytes());
                    let mut output = ToolOutput::new("");
                    output.source = OutputSource::Retained(capture.reference()?);
                    output
                } else {
                    ToolOutput::new(text)
                }
            }
        };
        let output = if let Some(capture) = capture {
            capture.seal(output, state)?
        } else {
            store.retain(record.clone(), output, state)?
        };
        let output = if matches!(output.source, OutputSource::Retained(_)) {
            store.result(
                &store
                    .inspect(&record.id)?
                    .context("Terminal invocation disappeared")?,
                target,
            )?
        } else {
            output
        };
        Ok(Completion {
            output,
            failed: state != RunState::Completed,
        })
    })
    .await?
}

pub async fn promote(id: &str) -> Result<bool> {
    let root = crate::storage::jcode_dir()?.join("execution");
    let run = LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(root, id.to_string()))
        .cloned();
    let Some(run) = run else {
        return Ok(false);
    };
    let store = run.store.clone();
    let run_id = id.to_string();
    let current = tokio::task::spawn_blocking(move || {
        store
            .inspect(&run_id)?
            .context("Missing invocation during promotion")
    })
    .await??;
    if current.owner != run.runtime.endpoint.id || !run.owns_execution.load(Ordering::SeqCst) {
        let result =
            runtime::control_in_store(&run.store, id, ControlOperation::Background).await?;
        if matches!(result, ControlReply::Accepted { changed: true }) {
            run.background.store(true, Ordering::SeqCst);
            return Ok(true);
        }
        if let ControlReply::Unavailable { message } = result {
            anyhow::bail!("{message}");
        }
        return Ok(false);
    }
    ensure!(!run.stop.is_set(), "Invocation is already stopping");
    let (reply, response) = oneshot::channel();
    run.commands
        .send(Command::Background(reply))
        .context("Invocation owner is unavailable")?;
    response
        .await
        .context("Invocation completed before promotion")?
        .map_err(anyhow::Error::msg)
}

/// Acceptance of a stop request is not a terminal-completion claim.
pub fn request_stop(id: &str, cause: StopCause) -> Result<bool> {
    let root = crate::storage::jcode_dir()?.join("execution");
    let run = LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(root, id.to_string()))
        .cloned();
    if let Some(run) = run {
        run.stop.fire_with_cause(cause);
        Ok(true)
    } else {
        Ok(false)
    }
}

struct BackgroundControl {
    run: Arc<LiveRun>,
}
#[async_trait::async_trait]
impl jcode_tool_core::OwnedExecutionControl for BackgroundControl {
    async fn request_stop(&self, cause: StopCause) -> Result<bool> {
        match runtime::control_in_store(
            &self.run.store,
            &self.run.invocation.id(),
            ControlOperation::Stop { cause },
        )
        .await?
        {
            ControlReply::Accepted { changed } => Ok(changed),
            ControlReply::Snapshot { record } => Ok(!record.state.terminal()),
            ControlReply::Unavailable { message } => Err(anyhow::anyhow!(message)),
            ControlReply::OwnerChanged => Err(anyhow::anyhow!(
                "Execution owner changed during cancellation"
            )),
        }
    }
    fn execution_id(&self) -> Option<String> {
        Some(self.run.invocation.id())
    }
    async fn survives_reload(&self) -> Result<bool> {
        let store = self.run.store.clone();
        let id = self.run.invocation.id();
        let original = self.run.runtime.endpoint.id.clone();
        tokio::task::spawn_blocking(move || {
            let record = store
                .inspect(&id)?
                .context("Missing background invocation")?;
            if record.owner == original || record.state.terminal() {
                return Ok(false);
            }
            ensure!(
                record.background,
                "A foreign foreground execution cannot be silently preserved as background"
            );
            store
                .runtime_endpoint(&record.owner)?
                .context("Background runtime identity is unavailable")?
                .has_live_lease()
        })
        .await?
    }
    async fn wait(&self) -> Result<RunState> {
        let mut receiver = self.run.result.clone();
        loop {
            if receiver.borrow_and_update().is_some() {
                break;
            }
            receiver
                .changed()
                .await
                .context("Execution owner ended without a completion receipt")?;
        }
        match runtime::control_in_store(
            &self.run.store,
            &self.run.invocation.id(),
            ControlOperation::Wait,
        )
        .await?
        {
            ControlReply::Snapshot { record } => Ok(record.state),
            ControlReply::Unavailable { message } => Err(anyhow::anyhow!(message)),
            _ => Err(anyhow::anyhow!(
                "Execution stopped without a durable terminal receipt"
            )),
        }
    }
}

pub fn background_control(
    id: &str,
) -> Result<Option<Arc<dyn jcode_tool_core::OwnedExecutionControl>>> {
    let root = crate::storage::jcode_dir()?.join("execution");
    Ok(LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(root, id.to_string()))
        .cloned()
        .map(|run| {
            Arc::new(BackgroundControl { run }) as Arc<dyn jcode_tool_core::OwnedExecutionControl>
        }))
}

/// Waiting for existing work never acquires ownership of that work. Dropping
/// this wait, including a wait on explicitly backgrounded work, does not stop it.
pub async fn wait_for(id: &str) -> Result<RunRecord> {
    let root = crate::storage::jcode_dir()?.join("execution");
    let run = LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(root, id.to_string()))
        .cloned();
    if let Some(run) = run {
        let mut result = run.result.clone();
        while result.borrow_and_update().is_none() {
            result
                .changed()
                .await
                .context("Execution owner ended without a completion receipt")?;
        }
    }
    let root = crate::storage::jcode_dir()?;
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let record = ExecutionStore::open(&root)?
            .inspect(&id)?
            .context("Unknown invocation")?;
        ensure!(
            record.state.terminal(),
            "Invocation has not published a terminal outcome"
        );
        Ok(record)
    })
    .await?
}
