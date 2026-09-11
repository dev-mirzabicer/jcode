//! Native command execution for the durable command owner. The owner retains
//! this future through quiescence; callers stop via the signal, not task abort.
use anyhow::{Context, Result, ensure};
use jcode_agent_runtime::InterruptSignal;
use jcode_tool_core::{OutputCapture, OutputStream};
use jcode_tool_types::{OutputSource, StopCause, ToolOutput};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub struct CommandSpec {
    pub command: String,
    pub working_dir: PathBuf,
    pub timeout: Option<Duration>,
}
pub struct CommandOutcome {
    pub output: ToolOutput,
    pub stop_cause: Option<StopCause>,
}

pub(crate) struct CommandGate {
    pub program: tokio::process::Command,
    pub store: crate::execution::ExecutionStore,
    pub run_id: String,
    pub owner: String,
}

pub(crate) async fn run_gated(
    spec: CommandSpec,
    capture: Arc<dyn OutputCapture>,
    stop: InterruptSignal,
    gate: CommandGate,
) -> Result<CommandOutcome> {
    run_with_control(spec, capture, stop, &NativeControl, Some(gate), None).await
}

pub struct CommandInput {
    pub requests: tokio::sync::mpsc::UnboundedSender<jcode_tool_core::StdinInputRequest>,
    pub call_id: String,
}

pub async fn run_with_input(
    spec: CommandSpec,
    capture: Arc<dyn OutputCapture>,
    stop: InterruptSignal,
    input: CommandInput,
) -> Result<CommandOutcome> {
    run_with_control(spec, capture, stop, &NativeControl, None, Some(input)).await
}

async fn serve_input(
    pid: u32,
    mut pipe: tokio::process::ChildStdin,
    input: CommandInput,
) -> Result<()> {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut ordinal = 0u64;
    loop {
        let state = tokio::task::spawn_blocking(move || {
            #[cfg(target_os = "linux")]
            {
                crate::stdin_detect::linux::check_process_tree(pid)
            }
            #[cfg(not(target_os = "linux"))]
            {
                crate::stdin_detect::is_waiting_for_stdin(pid)
            }
        })
        .await?;
        if state == crate::stdin_detect::StdinState::Reading {
            ordinal = ordinal
                .checked_add(1)
                .context("Stdin request identity exhausted")?;
            let (response_tx, response) = tokio::sync::oneshot::channel();
            input
                .requests
                .send(jcode_tool_core::StdinInputRequest {
                    request_id: format!("stdin-{}-{ordinal}", input.call_id),
                    prompt: String::new(),
                    is_password: false,
                    response_tx,
                })
                .map_err(|_| anyhow::anyhow!("Stdin response channel closed"))?;
            let mut text = response.await?;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            pipe.write_all(text.as_bytes()).await?;
            pipe.flush().await?;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct GroupGuard(Option<u32>);
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = crate::platform::signal_detached_process_group(pid, libc::SIGKILL);
        }
    }
}
struct DrainGuard(Vec<tokio::task::AbortHandle>);
impl Drop for DrainGuard {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

async fn drain(
    mut input: impl AsyncRead + Unpin,
    capture: Arc<dyn OutputCapture>,
    stream: OutputStream,
) -> Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut pending = Vec::new();
    let mut oversized = false;
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            if !oversized && !pending.is_empty() {
                publish_progress_lines(capture.clone(), vec![pending]).await?;
            }
            return Ok(());
        }
        let bytes = buffer[..count].to_vec();
        let writer = capture.clone();
        tokio::task::spawn_blocking(move || writer.write(stream, &bytes)).await??;
        // Bound only the status parser's scratch space. Every original byte,
        // including oversized or invalid markers, has already been captured.
        let mut lines = Vec::new();
        for part in buffer[..count].split_inclusive(|byte| *byte == b'\n') {
            if !oversized {
                if pending.len() + part.len() > 64 * 1024 {
                    pending.clear();
                    oversized = true;
                } else {
                    pending.extend_from_slice(part);
                }
            }
            if part.last() == Some(&b'\n') {
                if !oversized {
                    lines.push(std::mem::take(&mut pending));
                }
                oversized = false;
            }
        }
        if !lines.is_empty() {
            publish_progress_lines(capture.clone(), lines).await?;
        }
    }
}

async fn publish_progress_lines(
    capture: Arc<dyn OutputCapture>,
    lines: Vec<Vec<u8>>,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        for line in lines {
            if let Ok(line) = std::str::from_utf8(&line)
                && let Some((progress, checkpoint)) = crate::tool::parse_command_progress(line)?
            {
                capture.report_progress(progress, checkpoint)?;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
async fn signal_group(pid: u32, signal: i32) -> Result<()> {
    if let Err(error) = crate::platform::signal_detached_process_group(pid, signal)
        && error.raw_os_error() != Some(libc::ESRCH)
        && (error.raw_os_error() != Some(libc::EPERM)
            || crate::platform::process_group_has_live_members(pid).await?)
    {
        return Err(error.into());
    }
    Ok(())
}

pub async fn run(
    spec: CommandSpec,
    capture: Arc<dyn OutputCapture>,
    stop: InterruptSignal,
) -> Result<CommandOutcome> {
    run_with_control(spec, capture, stop, &NativeControl, None, None).await
}

#[async_trait::async_trait]
trait ProcessControl: Send + Sync {
    async fn signal(&self, pid: u32, signal: i32) -> Result<()>;
    async fn live(&self, pid: u32) -> Result<bool>;
}
struct NativeControl;
#[async_trait::async_trait]
impl ProcessControl for NativeControl {
    async fn signal(&self, pid: u32, signal: i32) -> Result<()> {
        signal_group(pid, signal).await
    }
    async fn live(&self, pid: u32) -> Result<bool> {
        Ok(crate::platform::process_group_has_live_members(pid).await?)
    }
}

async fn run_with_control(
    spec: CommandSpec,
    capture: Arc<dyn OutputCapture>,
    stop: InterruptSignal,
    control: &dyn ProcessControl,
    gate: Option<CommandGate>,
    input: Option<CommandInput>,
) -> Result<CommandOutcome> {
    ensure!(!stop.is_set(), "Command was stopped before launch");
    let (mut command, registration) = if let Some(gate) = gate {
        (gate.program, Some((gate.store, gate.run_id, gate.owner)))
    } else {
        let mut command = tokio::process::Command::new("bash");
        command.arg("-c").arg(&spec.command);
        (command, None)
    };
    command
        .current_dir(&spec.working_dir)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn().context("Start owned command")?;
    let pid = child
        .id()
        .context("Owned command has no process identity")?;
    let mut group = GroupGuard(Some(pid));
    let stdin_task = if let Some(input) = input {
        let pipe = child.stdin.take().context("Missing command stdin")?;
        Some(tokio::spawn(serve_input(pid, pipe, input)))
    } else {
        None
    };
    let mut stdout = tokio::spawn(drain(
        child.stdout.take().context("Missing stdout")?,
        capture.clone(),
        OutputStream::Stdout,
    ));
    let mut stderr = tokio::spawn(drain(
        child.stderr.take().context("Missing stderr")?,
        capture.clone(),
        OutputStream::Stderr,
    ));
    let mut handles = vec![stdout.abort_handle(), stderr.abort_handle()];
    if let Some(input) = &stdin_task {
        handles.push(input.abort_handle());
    }
    let _drains = DrainGuard(handles);
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut failure = if let Some((store, id, owner)) = registration {
        tokio::task::spawn_blocking(move || {
            let identity = crate::execution::process::ProcessIdentity::capture(pid)?;
            store.register_command_process(&id, &owner, &identity)
        })
        .await?
        .err()
    } else {
        None
    };
    let mut cause = None;
    let mut control_error: Option<String> = None;
    let mut timed_out = false;
    let mut killing = false;
    let deadline = spec
        .timeout
        .map(|duration| tokio::time::Instant::now() + duration);
    let mut force_at = None;
    let status = loop {
        if (cause.is_some() || timed_out || failure.is_some()) && force_at.is_none() && !killing {
            if let Err(error) = control.signal(pid, libc::SIGTERM).await {
                control_error.get_or_insert(error.to_string());
            }
            force_at = Some(tokio::time::Instant::now() + Duration::from_millis(400));
        }
        tokio::select! {
            biased;
            result=&mut stdout,if !stdout_done=>{
                stdout_done=true;
                if let Err(error)=result.unwrap_or_else(|error|Err(error.into())) {failure.get_or_insert(error);}
            }
            result=&mut stderr,if !stderr_done=>{
                stderr_done=true;
                if let Err(error)=result.unwrap_or_else(|error|Err(error.into())) {failure.get_or_insert(error);}
            }
            status=child.wait(),if stdout_done && stderr_done && (cause.is_none() && !timed_out && failure.is_none() || killing)=>{
                let status=status?;
                // No await may intervene between reaping and disarming: after
                // reaping, an empty group's numeric PID can eventually be reused.
                group.0=None;
                break status;
            }
            _=stop.notified(),if cause.is_none() && !timed_out && failure.is_none()=>{
                cause=Some(stop.stop_cause().unwrap_or(StopCause::HumanCancellation));
            }
            _=async {if let Some(deadline)=deadline {tokio::time::sleep_until(deadline).await}else{std::future::pending::<()>().await}},if !timed_out && cause.is_none() && failure.is_none()=>{
                timed_out=true;
            }
            _=async {if let Some(at)=force_at {tokio::time::sleep_until(at).await}else{std::future::pending::<()>().await}}=>{
                if let Err(error)=control.signal(pid,libc::SIGKILL).await {control_error.get_or_insert(error.to_string());}
                // Keep the leader unreaped until the whole owned group has
                // ceased work. An uninterruptible descendant remains quiescing.
                loop {
                    match control.live(pid).await {
                        Ok(false)=>break,
                        Ok(true)=>{},
                        Err(error)=>{control_error.get_or_insert(error.to_string());},
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                killing=true;force_at=None;
            }
        }
    };
    if let Some(error) = failure {
        return Err(error.context("Command output capture failed; owned command was stopped"));
    }
    use std::os::unix::process::ExitStatusExt;
    let exit_code = status.code();
    let exit_signal = status.signal();
    let note = if let Some(error) = &control_error {
        format!(
            "\n[Stop encountered an error: {error}. Ownership was retained until the process group ceased work.]\n"
        )
    } else if timed_out {
        "\n[Command stopped at its explicit deadline]\n".to_string()
    } else if let Some(cause) = cause {
        format!("\n[{}]\n", cause.description())
    } else if let Some(code) = exit_code.filter(|code| *code != 0) {
        format!("\n\nExit code: {code}")
    } else if let Some(signal) = exit_signal {
        format!("\n[Command ended from signal {signal}]\n")
    } else {
        String::new()
    };
    if !note.is_empty() {
        let writer = capture.clone();
        tokio::task::spawn_blocking(move || writer.write(OutputStream::Text, note.as_bytes()))
            .await??;
    }
    let mut output=ToolOutput::new("").with_metadata(serde_json::json!({"exit_code":exit_code,"exit_signal":exit_signal,"timed_out":timed_out,"control_error":control_error,"requested_stop":cause}))
        .with_error(!status.success() || timed_out || cause.is_some() || exit_signal.is_some() || control_error.is_some());
    output.source = OutputSource::Retained(capture.reference()?);
    Ok(CommandOutcome {
        output,
        stop_cause: if control_error.is_none() { cause } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, RunRecord, RunState, StorageConfig,
    };

    struct DeniedControl;
    #[async_trait::async_trait]
    impl ProcessControl for DeniedControl {
        async fn signal(&self, _: u32, _: i32) -> Result<()> {
            Err(std::io::Error::from_raw_os_error(libc::EPERM).into())
        }
        async fn live(&self, pid: u32) -> Result<bool> {
            NativeControl.live(pid).await
        }
    }

    #[tokio::test]
    async fn denied_stop_does_not_release_live_work_or_claim_cancellation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (store, record, capture) = capture(directory.path())?;
        let ready = directory.path().join("ready");
        let spec = CommandSpec {
            command: "printf ready > ready; sleep 1; printf done > finished".into(),
            working_dir: directory.path().into(),
            timeout: None,
        };
        let stop = InterruptSignal::new();
        let signal = stop.clone();
        let writer = capture.clone();
        let task = tokio::spawn(async move {
            run_with_control(spec, writer, signal, &DeniedControl, None, None).await
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if tokio::time::Instant::now() >= deadline {
                task.abort();
                anyhow::bail!("Fixture did not start");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.fire();
        let outcome = tokio::time::timeout(Duration::from_secs(5), task).await???;
        assert_eq!(std::fs::read(directory.path().join("finished"))?, b"done");
        assert!(outcome.output.metadata.as_ref().unwrap()["control_error"].is_string());
        assert!(outcome.stop_cause.is_none());
        capture.seal(outcome.output, RunState::Failed)?;
        assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Failed);
        Ok(())
    }
    fn capture(root: &std::path::Path) -> Result<(ExecutionStore, RunRecord, Arc<Capture>)> {
        let store = ExecutionStore::open(root)?;
        let input = Invocation {
            session_id: "command".into(),
            message_id: "message".into(),
            call_path: vec![uuid::Uuid::new_v4().to_string()],
            tool: "command-fixture".into(),
            input: serde_json::json!({}),
            working_dir: Some(root.to_path_buf()),
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let capture = Arc::new(Capture::create(
            store.clone(),
            record.clone(),
            StorageConfig::default(),
        )?);
        Ok((store, record, capture))
    }
    #[tokio::test]
    async fn full_streams_and_non_utf8_bytes_survive_the_actual_command() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (store, record, capture) = capture(directory.path())?;
        let spec=CommandSpec{command:"python3 -c 'import os,threading; a=threading.Thread(target=lambda:os.write(1,b\"x\"*300000+b\"\\xffSTDOUT_TAIL\")); a.start(); os.write(2,b\"y\"*300000+b\"STDERR_TAIL\"); a.join()'".into(),working_dir:directory.path().into(),timeout:None};
        let outcome = run(spec, capture.clone(), InterruptSignal::new()).await?;
        assert_eq!(outcome.output.metadata.as_ref().unwrap()["exit_code"], 0);
        capture.seal(outcome.output, RunState::Completed)?;
        let path = store.inspect(&record.id)?.unwrap().output_path.unwrap();
        let stdout = std::fs::read(path.with_file_name("stdout.bin"))?;
        assert_eq!(stdout.len(), 300012);
        assert!(stdout.ends_with(b"\xffSTDOUT_TAIL"));
        assert!(std::fs::read(path.with_file_name("stderr.bin"))?.ends_with(b"STDERR_TAIL"));
        let text = std::fs::read_to_string(&path)?;
        assert!(text.contains("\u{fffd}STDOUT_TAIL") && text.contains("STDERR_TAIL"));
        Ok(())
    }
    #[tokio::test]
    async fn stop_forces_term_resistant_descendants_and_preserves_effects() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (_, _, capture) = capture(directory.path())?;
        let ready = directory.path().join("ready");
        let spec=CommandSpec{command:"printf committed > effect; (trap '' TERM; printf ready > ready; sleep 20; printf escaped > escaped) & wait".into(),working_dir:directory.path().into(),timeout:None};
        let stop = InterruptSignal::new();
        let signal = stop.clone();
        let writer = capture.clone();
        let task = tokio::spawn(async move { run(spec, writer, signal).await });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if tokio::time::Instant::now() >= deadline {
                task.abort();
                anyhow::bail!("Command did not start");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.fire_with_cause(StopCause::HumanCancellation);
        let outcome = tokio::time::timeout(Duration::from_secs(5), task).await???;
        assert_eq!(outcome.stop_cause, Some(StopCause::HumanCancellation));
        capture.seal(outcome.output, RunState::Cancelled)?;
        assert_eq!(
            std::fs::read(directory.path().join("effect"))?,
            b"committed"
        );
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }
    #[tokio::test]
    async fn explicit_deadline_is_distinct_from_human_cancellation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (_, _, capture) = capture(directory.path())?;
        let spec = CommandSpec {
            command: "printf partial; sleep 20".into(),
            working_dir: directory.path().into(),
            timeout: Some(Duration::from_millis(100)),
        };
        let outcome = run(spec, capture.clone(), InterruptSignal::new()).await?;
        assert_eq!(outcome.stop_cause, None);
        assert_eq!(outcome.output.metadata.as_ref().unwrap()["timed_out"], true);
        assert!(outcome.output.is_error);
        capture.seal(outcome.output, RunState::Failed)?;
        Ok(())
    }
}
