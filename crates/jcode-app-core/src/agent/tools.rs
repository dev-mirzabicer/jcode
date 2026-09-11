use crate::message::{ContentBlock, ToolCall};
use crate::terminal_println as println;
use crate::tool::ToolOutput;

impl super::Agent {
    pub(super) fn provider_leaves_tool_to_host(&self, name: &str) -> bool {
        self.provider.handles_tools_internally()
            && self
                .provider
                .host_managed_tool(crate::tool::Registry::resolve_tool_name(name))
    }

    pub(super) async fn retain_managed_sdk_results(
        &mut self,
        calls: &[ToolCall],
        results: &mut std::collections::HashMap<String, ToolOutput>,
        message_id: &str,
        events: Option<&tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
    ) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        for tool in calls {
            if self.provider_leaves_tool_to_host(&tool.name) {
                continue;
            }
            let Some(received) = results.get(&tool.id).cloned() else {
                failures.push(format!("SDK-managed tool {} ({}) ended without a result; input retained, no local replay",tool.name,tool.id));
                continue;
            };
            let content = crate::execution::sdk_failure_body(&received);
            let output = match self.retain_sdk_output(tool, message_id, received).await {
                Ok(output) => output,
                Err(error) => {
                    self.add_message(crate::message::Role::User,vec![ContentBlock::ToolResult{
                        tool_use_id:tool.id.clone(),is_error:Some(true),
                        content:format!("[SDK output retention failed; original received body follows. Do not repeat the operation.]\n{content}"),
                    }]);
                    self.session.save()?;
                    results.remove(&tool.id);
                    failures.push(format!("Managed SDK result {} could not be archived; original body preserved in history: {error:#}",tool.id));
                    continue;
                }
            };
            let presented = output.output.clone();
            let failed = output.is_error;
            self.add_message(
                crate::message::Role::User,
                tool_output_to_content_blocks(tool.id.clone(), output),
            );
            self.session.save()?;
            results.remove(&tool.id);
            crate::bus::Bus::global().publish(crate::bus::BusEvent::ToolUpdated(
                crate::bus::ToolEvent {
                    session_id: self.session.id.clone(),
                    message_id: message_id.into(),
                    tool_call_id: tool.id.clone(),
                    tool_name: tool.name.clone(),
                    intent: tool.intent.clone(),
                    title: None,
                    status: if failed {
                        crate::bus::ToolStatus::Error
                    } else {
                        crate::bus::ToolStatus::Completed
                    },
                },
            ));
            if let Some(events) = events {
                let _ = events.send(crate::protocol::ServerEvent::ToolDone {
                    id: tool.id.clone(),
                    name: tool.name.clone(),
                    output: presented,
                    error: failed
                        .then(|| "Provider tool failed; retained result is available".into()),
                });
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "SDK receipt processing failed after preserving available results. No operation was repeated. {}",
            failures.join("\n")
        );
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn retain_sdk_result(
        &self,
        tool: &ToolCall,
        message_id: &str,
        content: String,
        is_error: bool,
    ) -> anyhow::Result<ToolOutput> {
        self.retain_sdk_output(
            tool,
            message_id,
            crate::execution::received_sdk_result(&tool.id, content, is_error, None),
        )
        .await
    }
    pub(super) async fn retain_sdk_output(
        &self,
        tool: &ToolCall,
        message_id: &str,
        output: ToolOutput,
    ) -> anyhow::Result<ToolOutput> {
        let ctx = crate::tool::ToolContext {
            session_id: self.session.id.clone(),
            message_id: message_id.into(),
            tool_call_id: tool.id.clone(),
            working_dir: self.working_dir().map(std::path::PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
            invocation: Default::default(),
        };
        self.registry
            .retain_provider_result(&tool.name, tool.input.clone(), ctx, output)
            .await
    }
}

/// Build rendered side-pane images from a tool output's attached images.
///
/// This mirrors how `render_messages_and_images` derives images from persisted
/// session history (source = ToolResult), so live-streamed images match what a
/// later History reload would produce. `tool_name` and `tool_input` provide the
/// label fallback (e.g. the `read` tool's `file_path`); `tool_call_id` anchors
/// the image to its tool message in the transcript.
pub(super) fn tool_output_side_pane_images(
    tool_call_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    output: &ToolOutput,
) -> Vec<jcode_session_types::RenderedImage> {
    if output.images.is_empty() {
        return Vec::new();
    }
    let fallback_label = tool_input
        .get("file_path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    output
        .images
        .iter()
        .map(|img| jcode_session_types::RenderedImage {
            media_type: img.media_type.clone(),
            data: img.data.clone(),
            label: img
                .label
                .as_ref()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .or_else(|| fallback_label.clone()),
            source: jcode_session_types::RenderedImageSource::ToolResult {
                tool_name: tool_name.to_string(),
            },
            anchor: Some(jcode_session_types::RenderedImageAnchor::ToolCall {
                id: tool_call_id.to_string(),
            }),
        })
        .collect()
}

pub(super) fn tool_output_to_content_blocks(
    tool_use_id: String,
    output: ToolOutput,
) -> Vec<ContentBlock> {
    crate::execution::tool_result_blocks(tool_use_id, output)
}

pub(super) fn print_tool_summary(tool: &ToolCall) {
    match tool.name.as_str() {
        "bash" => {
            if let Some(cmd) = tool.input.get("command").and_then(|v| v.as_str()) {
                let short = if cmd.len() > 60 {
                    format!("{}...", crate::util::truncate_str(cmd, 60))
                } else {
                    cmd.to_string()
                };
                println!("$ {}", short);
            }
        }
        "read" | "write" | "edit" => {
            if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                println!("{}", path);
            }
        }
        "glob" | "grep" => {
            if let Some(pattern) = tool.input.get("pattern").and_then(|v| v.as_str()) {
                println!("'{}'", pattern);
            }
        }
        "ls" => {
            let path = tool
                .input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            println!("{}", path);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_conversion_preserves_the_complete_delivered_body_and_error_flag() {
        let text = format!("{}TAIL", "α".repeat(600_000));
        let blocks = tool_output_to_content_blocks(
            "tool-id".into(),
            ToolOutput::new(&text).with_error(true),
        );
        assert!(
            matches!(&blocks[0],ContentBlock::ToolResult{content,is_error:Some(true),..} if content==&text)
        );
    }
}
