//! Adapter for the pinned search library. Native helper ownership remains with
//! the same command driver used by Bash, not an independent process supervisor.
use super::*;
use agentgrep::execution::{ExecutionControl, ExecutionHost};
#[cfg(all(test, unix))]
use jcode_tool_core::OutputCapture;
use std::sync::Arc;

pub(super) fn control(ctx: &ToolContext) -> Result<ExecutionControl> {
    #[cfg(unix)]
    if ctx.invocation.capture.is_some() && ctx.graceful_shutdown_signal.is_some() {
        return Ok(ExecutionControl::new(Arc::new(SearchHost {
            host: crate::execution::helper::HelperHost::new(ctx, "search")?,
        })));
    }
    #[cfg(not(unix))]
    if let Some(stop) = &ctx.graceful_shutdown_signal {
        return Ok(ExecutionControl::new(Arc::new(NativeSearchOnly {
            stop: stop.clone(),
        })));
    }
    Ok(ExecutionControl::default())
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
    host: crate::execution::helper::HelperHost,
}
#[cfg(unix)]
impl ExecutionHost for SearchHost {
    fn cancelled(&self) -> bool {
        self.host.cancelled()
    }
    fn command(&self, command: std::process::Command) -> std::io::Result<std::process::Output> {
        self.host
            .program(command.into(), None, None)
            .map_err(|error| {
                let kind = if self.host.cancelled() {
                    std::io::ErrorKind::Interrupted
                } else {
                    error
                        .downcast_ref::<std::io::Error>()
                        .map(|error| error.kind())
                        .unwrap_or(std::io::ErrorKind::Other)
                };
                std::io::Error::new(kind, error)
            })
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
        let mut ctx = ctx;
        ctx.invocation.capture = Some(capture.clone());
        ctx.graceful_shutdown_signal = Some(stop.clone());
        let host = SearchHost {
            host: crate::execution::helper::HelperHost::new(&ctx, "search")?,
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
