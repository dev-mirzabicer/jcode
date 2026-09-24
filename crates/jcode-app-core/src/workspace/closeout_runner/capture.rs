//! Cheap Stop observation over the supervisor's original signal. Retention
//! methods still forward after Stop so partial and terminal evidence is kept.
use jcode_tool_core::{CapturedPart, OutputCapture, OutputStream};
use std::sync::Arc;

pub(super) struct ControlledCapture {
    pub capture: Arc<dyn OutputCapture>,
    pub stop: jcode_agent_runtime::InterruptSignal,
}
impl OutputCapture for ControlledCapture {
    fn check_cancelled(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.stop.is_set(), "Closeout execution was stopped");
        Ok(())
    }
    fn begin_process(&self) -> anyhow::Result<String> {
        self.capture.begin_process()
    }
    fn register_process(&self, ticket: &str, pid: u32) -> anyhow::Result<()> {
        self.capture.register_process(ticket, pid)
    }
    fn finish_process(&self, ticket: &str) -> anyhow::Result<()> {
        self.capture.finish_process(ticket)
    }
    fn report_progress(
        &self,
        progress: jcode_base::bus::BackgroundTaskProgress,
        checkpoint: bool,
    ) -> anyhow::Result<()> {
        self.capture.report_progress(progress, checkpoint)
    }
    fn write(&self, stream: OutputStream, bytes: &[u8]) -> anyhow::Result<()> {
        self.capture.write(stream, bytes)
    }
    fn reference(&self) -> anyhow::Result<jcode_tool_types::OutputReference> {
        self.capture.reference()
    }
    fn append_part(&self, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.capture.append_part(name, bytes)
    }
    fn read_part(&self, name: &str) -> anyhow::Result<CapturedPart> {
        self.capture.read_part(name)
    }
}
