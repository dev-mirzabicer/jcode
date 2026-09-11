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

type Producer = Box<dyn FnOnce(ToolContext) -> BoxFuture<'static, Result<ToolOutput>> + Send>;
type Key = (PathBuf, String);
static LIVE: LazyLock<Mutex<HashMap<Key, Arc<LiveRun>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct LiveRun {
    store: ExecutionStore,
    invocation: Invocation,
    stop: InterruptSignal,
    background: AtomicBool,
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
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
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
                store: store.clone(),
                invocation,
                stop: InterruptSignal::new(),
                background: AtomicBool::new(false),
                commands,
                result: subscription,
            });
            live.insert(key.clone(), Arc::clone(&run));
            let owned = Arc::clone(&run);
            tokio::spawn(async move {
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
                LIVE.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
            });
            (run, true)
        }
    };
    let mut wait = ForegroundWait {
        run: Arc::clone(&run),
        armed: created,
    };
    let mut receiver = run.result.clone();
    loop {
        let completed = receiver.borrow_and_update().clone();
        if let Some(completed) = completed {
            wait.armed = false;
            if completed.failed {
                return Err(CapturedToolError {
                    output: completed.output.clone(),
                }
                .into());
            }
            return Ok(completed.output.clone());
        }
        receiver
            .changed()
            .await
            .context("Execution owner closed before a result was available")?;
    }
}

async fn supervise(
    store: ExecutionStore,
    run: Arc<LiveRun>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut ctx: ToolContext,
    target: NonZeroUsize,
    producer: Producer,
) -> Result<Completion> {
    let owner = crate::background::runtime_instance_id().to_string();
    let preparation = {
        let store = store.clone();
        let input = run.invocation.clone();
        let owner = owner.clone();
        tokio::task::spawn_blocking(move || store.prepare(&input, &owner)).await??
    };
    let record = match preparation {
        PreparedInvocation::New(record) => record,
        PreparedInvocation::Existing(record) => {
            let store = store.clone();
            return tokio::task::spawn_blocking(move||{
                let record=if record.state.terminal(){record}else{store.recover_terminal_output(&record.id)
                    .context("Invocation already exists without a proven completed result; inspect its owner instead of reexecuting")?};
                let failed=record.state!=RunState::Completed;
                Ok(Completion {output:store.result(&record,target)?,failed})
            }).await?;
        }
    };
    let capture_result = {
        let store = store.clone();
        let record = record.clone();
        let config = crate::config::config().output.storage.clone();
        tokio::task::spawn_blocking(move || {
            store.start(&record.id, &record.owner)?;
            if record.tool == "read" {
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
    ctx.graceful_shutdown_signal = Some(run.stop.clone());
    ctx.invocation.capture = capture.clone().map(|value| value as Arc<dyn OutputCapture>);
    let stop_before_start = run
        .stop
        .stop_cause()
        .or_else(|| parent.as_ref().and_then(InterruptSignal::stop_cause));
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
                    let persisted=tokio::task::spawn_blocking(move||store.request_stop(&id,&owner,cause)).await;
                    if !matches!(persisted,Ok(Ok(true))) {crate::logging::warn("Could not persist an owned tool stop request; terminal publication will recheck storage");}
                    abort_at=Some(tokio::time::Instant::now()+std::time::Duration::from_millis(750));
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
        None if result.is_err() => RunState::Failed,
        None => RunState::Completed,
    };
    tokio::task::spawn_blocking(move || {
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
    fn request_stop(&self, cause: StopCause) -> Result<bool> {
        if self.run.result.borrow().is_some() {
            return Ok(false);
        }
        self.run.stop.fire_with_cause(cause);
        Ok(true)
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
        let store = self.run.store.clone();
        let id = self.run.invocation.id();
        tokio::task::spawn_blocking(move || {
            let record = store
                .inspect(&id)?
                .context("Missing invocation after execution")?;
            ensure!(
                record.state.terminal(),
                "Execution stopped without a durable terminal receipt"
            );
            Ok(record.state)
        })
        .await?
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
