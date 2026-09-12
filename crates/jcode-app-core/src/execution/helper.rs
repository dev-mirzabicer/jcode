//! Native helper I/O through the existing process-group/capture owner.
//! Blocking native adapters pass this context explicitly; there is no thread-local policy.
use anyhow::{Context, Result, ensure};
use jcode_tool_core::{CapturedPart, OutputCapture, OutputStream, ToolContext};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct HelperHost {
    capture: Arc<dyn OutputCapture>,
    stop: jcode_agent_runtime::InterruptSignal,
    runtime: tokio::runtime::Handle,
    cwd: PathBuf,
    prefix: String,
    next: Arc<AtomicU64>,
}
impl HelperHost {
    pub fn new(context: &ToolContext, prefix: &str) -> Result<Self> {
        ensure!(
            !prefix.is_empty()
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid native helper namespace"
        );
        Ok(Self {
            capture: context
                .invocation
                .capture
                .clone()
                .context("Native helper requires its invocation capture")?,
            stop: context
                .graceful_shutdown_signal
                .clone()
                .context("Native helper requires its invocation stop owner")?,
            runtime: tokio::runtime::Handle::try_current()?,
            cwd: context
                .working_dir
                .clone()
                .map(Ok)
                .unwrap_or_else(std::env::current_dir)?,
            prefix: prefix.into(),
            next: Arc::new(AtomicU64::new(0)),
        })
    }
    pub fn cancelled(&self) -> bool {
        self.stop.is_set()
    }
    pub fn check_stop(&self) -> Result<()> {
        ensure!(
            !self.stop.is_set(),
            "Native helper stopped; completed effects were not rolled back"
        );
        Ok(())
    }
    pub fn command(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<std::process::Output> {
        self.check_stop()?;
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        self.program(command, None, timeout)
    }
    pub fn program(
        &self,
        command: tokio::process::Command,
        stdin: Option<&[u8]>,
        timeout: Option<Duration>,
    ) -> Result<std::process::Output> {
        self.runtime.block_on(self.execute(command, timeout, stdin))
    }
    async fn execute(
        &self,
        mut command: tokio::process::Command,
        timeout: Option<Duration>,
        stdin: Option<&[u8]>,
    ) -> Result<std::process::Output> {
        self.check_stop()?;
        let ordinal = self
            .next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| anyhow::anyhow!("Native helper identity exhausted"))?;
        let parts = Arc::new(HelperParts {
            capture: self.capture.clone(),
            prefix: format!("{}-{ordinal}", self.prefix),
        });
        for stream in ["stdout", "stderr"] {
            parts
                .capture
                .append_part(&format!("{}-{stream}", parts.prefix), &[])?;
        }
        let stdin = if let Some(bytes) = stdin {
            let name = format!("{}-stdin", parts.prefix);
            parts.capture.append_part(&name, &[])?;
            for chunk in bytes.chunks(64 * 1024) {
                self.check_stop()?;
                parts.capture.append_part(&name, chunk)?;
            }
            Some(parts.capture.read_part(&name)?.reader)
        } else {
            None
        };
        let cwd = command
            .as_std()
            .get_current_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.cwd.clone());
        command.current_dir(&cwd);
        let result = super::command::run_program(
            command,
            cwd,
            parts.clone(),
            self.stop.clone(),
            timeout,
            stdin,
        )
        .await?;
        self.check_stop()?;
        ensure!(
            !result
                .output
                .process_exit
                .as_ref()
                .is_some_and(|exit| exit.timed_out),
            "Native helper command timed out; acquired stdout/stderr remain retained in {}",
            self.capture.reference()?.path.display()
        );
        let read = |stream: &str| -> Result<Vec<u8>> {
            let mut part = parts
                .capture
                .read_part(&format!("{}-{stream}", parts.prefix))?;
            let mut bytes = Vec::new();
            part.reader.read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        Ok(std::process::Output {
            status: result.status,
            stdout: read("stdout")?,
            stderr: read("stderr")?,
        })
    }
}
struct HelperParts {
    capture: Arc<dyn OutputCapture>,
    prefix: String,
}
impl OutputCapture for HelperParts {
    fn begin_process(&self) -> Result<String> {
        self.capture.begin_process()
    }
    fn register_process(&self, ticket: &str, pid: u32) -> Result<()> {
        self.capture.register_process(ticket, pid)
    }
    fn finish_process(&self, ticket: &str) -> Result<()> {
        self.capture.finish_process(ticket)
    }
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
    fn report_progress(&self, _: crate::bus::BackgroundTaskProgress, _: bool) -> Result<()> {
        Ok(())
    }
}
