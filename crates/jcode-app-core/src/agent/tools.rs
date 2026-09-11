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
        results: &mut std::collections::HashMap<String, (String, bool)>,
        message_id: &str,
        events: Option<&tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
    ) -> anyhow::Result<()> {
        for tool in calls {
            if self.provider_leaves_tool_to_host(&tool.name) {
                continue;
            }
            let (content,is_error)=results.get(&tool.id).cloned().ok_or_else(||anyhow::anyhow!("SDK-managed tool {} ({}) ended without a result. Its input remains in history; no local operation was executed to recreate unknown effects.",tool.name,tool.id))?;
            let output = match self
                .retain_sdk_result(tool, message_id, content.clone(), is_error)
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    self.add_message(crate::message::Role::User,vec![ContentBlock::ToolResult{
                        tool_use_id:tool.id.clone(),is_error:Some(true),
                        content:format!("[SDK output retention failed; original received body follows. Do not repeat the operation.]\n{content}"),
                    }]);
                    self.session.save()?;
                    return Err(error.context("Managed SDK result could not be archived; original body was preserved in history without repeating its effects"));
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
        Ok(())
    }

    pub(super) async fn retain_sdk_result(
        &self,
        tool: &ToolCall,
        message_id: &str,
        content: String,
        is_error: bool,
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
            .retain_provider_result(
                &tool.name,
                tool.input.clone(),
                ctx,
                ToolOutput::new(content)
                    .with_error(is_error)
                    .with_metadata(serde_json::json!({"provider_supplied":true})),
            )
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
    let mut blocks = vec![ContentBlock::ToolResult {
        tool_use_id,
        content: output.output,
        is_error: output.is_error.then_some(true),
    }];
    for img in output.images {
        blocks.push(ContentBlock::Image {
            media_type: img.media_type,
            data: img.data,
        });
        if let Some(label) = img.label.filter(|label| !label.trim().is_empty()) {
            blocks.push(ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {}]",
                    label
                ),
                cache_control: None,
            });
        }
    }
    blocks
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
