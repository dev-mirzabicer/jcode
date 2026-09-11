//! Adapter for the pinned search library. Native helper ownership remains with
//! the same command driver used by Bash, not an independent process supervisor.
use super::*;
use agentgrep::execution::{ExecutionControl, ExecutionHost};
#[cfg(unix)]
use jcode_tool_core::{CapturedPart, OutputCapture, OutputStream};
#[cfg(unix)]
use std::io::Read;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn control(ctx: &ToolContext) -> ExecutionControl {
    #[cfg(unix)]
    if let (Some(capture), Some(stop)) = (&ctx.invocation.capture, &ctx.graceful_shutdown_signal) {
        return ExecutionControl::new(Arc::new(SearchHost {
            capture: capture.clone(),
            stop: stop.clone(),
            runtime: tokio::runtime::Handle::current(),
            next: AtomicUsize::new(0),
        }));
    }
    #[cfg(not(unix))]
    if let Some(stop) = &ctx.graceful_shutdown_signal {
        return ExecutionControl::new(Arc::new(NativeSearchOnly { stop: stop.clone() }));
    }
    ExecutionControl::default()
}

#[cfg(not(unix))]
struct NativeSearchOnly {
    stop: jcode_agent_runtime::InterruptSignal,
}
#[cfg(not(unix))]
impl ExecutionHost for NativeSearchOnly {
    fn cancelled(&self) -> bool {
        self.stop.is_set()
    }
    fn command(&self, _command: std::process::Command) -> std::io::Result<std::process::Output> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Use the library's cancellable native search on this platform",
        ))
    }
}

#[cfg(unix)]
struct SearchHost {
    capture: Arc<dyn OutputCapture>,
    stop: jcode_agent_runtime::InterruptSignal,
    runtime: tokio::runtime::Handle,
    next: AtomicUsize,
}
#[cfg(unix)]
impl ExecutionHost for SearchHost {
    fn cancelled(&self) -> bool {
        self.stop.is_set()
    }
    fn command(&self, command: std::process::Command) -> std::io::Result<std::process::Output> {
        let run = self.next.fetch_add(1, Ordering::SeqCst);
        let parts = Arc::new(SearchParts {
            capture: self.capture.clone(),
            prefix: format!("search-{run}"),
        });
        parts
            .capture
            .append_part(&format!("{}-stdout", parts.prefix), &[])
            .map_err(std::io::Error::other)?;
        parts
            .capture
            .append_part(&format!("{}-stderr", parts.prefix), &[])
            .map_err(std::io::Error::other)?;
        let cwd = command
            .get_current_dir()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                std::io::Error::other("Search helper requires an explicit working directory")
            })?;
        let outcome = self
            .runtime
            .block_on(crate::execution::command::run_program(
                command.into(),
                cwd,
                parts.clone(),
                self.stop.clone(),
            ))
            .map_err(|error| {
                let kind = error
                    .downcast_ref::<std::io::Error>()
                    .map(|error| error.kind())
                    .unwrap_or(std::io::ErrorKind::Other);
                std::io::Error::new(kind, error)
            })?;
        let read = |stream: &str| -> std::io::Result<Vec<u8>> {
            let mut part = parts
                .capture
                .read_part(&format!("{}-{stream}", parts.prefix))
                .map_err(std::io::Error::other)?;
            let mut bytes = Vec::new();
            part.reader.read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        if self.stop.is_set() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Search stopped; acquired helper bytes remain retained",
            ));
        }
        Ok(std::process::Output {
            status: outcome.status,
            stdout: read("stdout")?,
            stderr: read("stderr")?,
        })
    }
}
#[cfg(unix)]
struct SearchParts {
    capture: Arc<dyn OutputCapture>,
    prefix: String,
}
#[cfg(unix)]
impl OutputCapture for SearchParts {
    fn write(&self, stream: OutputStream, bytes: &[u8]) -> Result<()> {
        let stream = match stream {
            OutputStream::Stdout => "stdout",
            OutputStream::Stderr => "stderr",
            OutputStream::Text => "control",
        };
        self.capture
            .append_part(&format!("{}-{stream}", self.prefix), bytes)
    }
    fn reference(&self) -> Result<jcode_tool_types::OutputReference> {
        self.capture.reference()
    }
    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.capture
            .append_part(&format!("{}-{name}", self.prefix), bytes)
    }
    fn read_part(&self, name: &str) -> Result<CapturedPart> {
        self.capture.read_part(&format!("{}-{name}", self.prefix))
    }
    fn report_progress(
        &self,
        _progress: crate::bus::BackgroundTaskProgress,
        _checkpoint: bool,
    ) -> Result<()> {
        // Search results are source material, not the helper's progress protocol.
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn search_helper_stop_reaps_actual_process_and_retains_acquired_streams() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = crate::execution::ExecutionStore::open(directory.path())?;
        let ctx = ToolContext {
            session_id: "search-stop".into(),
            message_id: "message".into(),
            tool_call_id: "call".into(),
            working_dir: Some(directory.path().into()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let invocation =
            crate::execution::invocation(&ctx, "agentgrep", json!({"query":"fixture"}));
        let crate::execution::PreparedInvocation::New(record) =
            store.prepare(&invocation, "owner")?
        else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let capture = Arc::new(crate::execution::Capture::create(
            store.clone(),
            record.clone(),
            crate::execution::StorageConfig::default(),
        )?);
        let stop = jcode_agent_runtime::InterruptSignal::new();
        let host = SearchHost {
            capture: capture.clone(),
            stop: stop.clone(),
            runtime: tokio::runtime::Handle::current(),
            next: AtomicUsize::new(0),
        };
        let mut command = std::process::Command::new("bash");
        command.current_dir(directory.path()).args([
            "-c",
            "printf captured; printf captured-error >&2; sleep 30; printf escaped > escaped",
        ]);
        let task = tokio::task::spawn_blocking(move || host.command(command));
        let raw = capture
            .reference()?
            .path
            .with_file_name("part-search-0-stdout.bin");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !std::fs::read(&raw).is_ok_and(|bytes| bytes == b"captured") {
            if tokio::time::Instant::now() > deadline {
                stop.fire();
                let _ = task.await;
                anyhow::bail!("Helper output was not captured");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        stop.fire();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task).await??;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        capture.seal(
            ToolOutput::new("Search stopped"),
            crate::execution::RunState::Cancelled,
        )?;
        assert_eq!(std::fs::read(raw)?, b"captured");
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }
}
