use super::{ToolContext, ToolOutput};
use anyhow::Result;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::sync::Arc;

/// A mutation's successful steps must remain inspectable if a later step fails.
/// Captured callers write receipts incrementally, direct callers keep their full
/// result in memory. This owns no filesystem edits and performs no rollback.
pub(super) struct MutationOutput {
    capture: Option<Arc<dyn OutputCapture>>,
    text: String,
}
impl MutationOutput {
    pub fn new(ctx: &ToolContext) -> Self {
        Self {
            capture: ctx.invocation.capture.clone(),
            text: String::new(),
        }
    }
    pub async fn append(&mut self, text: String) -> Result<()> {
        if let Some(capture) = &self.capture {
            let capture = capture.clone();
            tokio::task::spawn_blocking(move || capture.write(OutputStream::Text, text.as_bytes()))
                .await??;
        } else {
            self.text.push_str(&text);
        }
        Ok(())
    }
    pub fn finish(self, failed: bool) -> Result<ToolOutput> {
        let mut output = ToolOutput::new(self.text).with_error(failed);
        if let Some(capture) = self.capture {
            output.source = jcode_tool_types::OutputSource::Retained(capture.reference()?);
        }
        Ok(output)
    }
}

pub(super) fn check_stop(ctx: &ToolContext) -> Result<()> {
    anyhow::ensure!(
        !ctx.graceful_shutdown_signal
            .as_ref()
            .is_some_and(|signal| signal.is_set()),
        "Mutation stopped between filesystem actions; completed effects were retained"
    );
    Ok(())
}
