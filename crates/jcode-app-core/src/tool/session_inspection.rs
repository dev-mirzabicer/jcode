use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use jcode_tool_types::inspection::InspectionRequest;
use serde_json::{Value, json};

pub(super) enum InspectionTool {
    Outline,
    Transcript,
    ExpandTool,
}
impl InspectionTool {
    pub fn all() -> [Self; 3] {
        [Self::Outline, Self::Transcript, Self::ExpandTool]
    }
}
#[async_trait]
impl Tool for InspectionTool {
    fn name(&self) -> &str {
        match self {
            Self::Outline => "session_outline",
            Self::Transcript => "read_transcript",
            Self::ExpandTool => "expand_tool_use",
        }
    }
    fn description(&self) -> &str {
        match self {
            Self::Outline => {
                "Open a durable as-of outline of a readable session. Returns a snapshot ID required by transcript and tool expansion. Use self, parent, or a readable session ID. Calling again captures a new snapshot."
            }
            Self::Transcript => {
                "Read an immutable inspection snapshot. Uses the captured context projection by default. Explicit raw returns original stored messages. Ranges use one-based source message positions and include an intersecting summary whole with its actual coverage."
            }
            Self::ExpandTool => {
                "Expand a snapshot-bound tool reference from session_outline. Returns full input and retained output bounded to the captured prefix/status. Provider tool IDs alone are not valid references. Does not repeat the producer."
            }
        }
    }
    fn parameters_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        properties.insert("intent".into(), super::intent_schema_property());
        let required = match self {
            Self::Outline => {
                properties.insert("target".into(),json!({"type":"string","description":"self, parent, or an authorized target session ID."}));
                vec!["target"]
            }
            Self::Transcript => {
                properties.insert(
                    "snapshot_id".into(),
                    json!({"type":"string","description":"Exact ID returned by session_outline."}),
                );
                properties.insert("range".into(),json!({"type":"object","properties":{"start":{"type":"integer","minimum":1},"end":{"type":"integer","minimum":1}},"required":["start","end"],"additionalProperties":false,"description":"Inclusive source message positions from the captured outline."}));
                properties.insert("raw".into(),json!({"type":"boolean","description":"Explicitly read original stored messages instead of their context projection."}));
                vec!["snapshot_id"]
            }
            Self::ExpandTool => {
                properties.insert("snapshot_id".into(), json!({"type":"string"}));
                properties.insert("tool_use_id".into(),json!({"type":"string","description":"Snapshot-bound tool_use_id returned by its outline, not the raw provider ID."}));
                vec!["snapshot_id", "tool_use_id"]
            }
        };
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn execution_policy(
        &self,
        _: &Value,
        _: &ToolContext,
    ) -> Result<jcode_tool_core::ExecutionPolicy> {
        Ok(jcode_tool_core::ExecutionPolicy {
            cooperative_stop: true,
            ..Default::default()
        })
    }
    async fn execute(&self, mut input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let object = input
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Inspection input must be an object"))?;
        ensure!(
            !object.contains_key("action"),
            "Inspection tool action is fixed by its tool identity"
        );
        object.remove("intent");
        object.retain(|_, value| !value.is_null());
        object.insert(
            "action".into(),
            Value::String(
                match self {
                    Self::Outline => "outline",
                    Self::Transcript => "transcript",
                    Self::ExpandTool => "expand_tool",
                }
                .into(),
            ),
        );
        let request: InspectionRequest = serde_json::from_value(input)?;
        ensure!(
            !ctx.graceful_shutdown_signal
                .as_ref()
                .is_some_and(|signal| signal.is_set()),
            "Session inspection cancelled"
        );
        let output = crate::session_inspection::agent_inspection_with_context(
            crate::storage::jcode_dir()?,
            ctx.session_id,
            request,
            ctx.invocation.capture.clone(),
            ctx.graceful_shutdown_signal.clone(),
        )
        .await?;
        ensure!(
            !ctx.graceful_shutdown_signal
                .as_ref()
                .is_some_and(|signal| signal.is_set()),
            "Session inspection cancelled after capture; no producer was repeated"
        );
        Ok(output)
    }
}
