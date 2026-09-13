use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct InvalidTool;

impl InvalidTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct InvalidInput {
    tool: String,
    error: String,
}

#[async_trait]
impl Tool for InvalidTool {
    fn name(&self) -> &str {
        "invalid"
    }

    fn description(&self) -> &str {
        "Report invalid tool usage. Use only when a tool call is malformed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["tool", "error"],
            "properties": {
                "intent": super::intent_schema_property(),
                "tool": {
                    "type": "string",
                    "description": "Tool name."
                },
                "error": {
                    "type": "string",
                    "description": "Validation error."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: InvalidInput = serde_json::from_value(input)?;
        Ok(ToolOutput::new(format!(
            "Invalid tool invocation for '{}': {}",
            params.tool, params.error
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_call_diagnostic_preserves_complete_selected_error() -> Result<()> {
        let error = "α".repeat(30_000) + "DIAGNOSTIC_TAIL";
        let context = ToolContext {
            session_id: "invalid-fixture".into(),
            message_id: "message".into(),
            tool_call_id: "call".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let output = InvalidTool::new()
            .execute(json!({"tool":"synthetic","error":error}), context)
            .await?;
        assert!(output.output.contains(&error));
        Ok(())
    }
}
