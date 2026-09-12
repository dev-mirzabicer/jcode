use anyhow::Result;
use async_trait::async_trait;
use jcode_agent_runtime::InterruptSignal;
use jcode_message_types::ToolDefinition;
use jcode_tool_types::ToolOutput;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
pub mod input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Text,
    Stdout,
    Stderr,
}

/// Implemented by the output owner. A successful write acknowledges retention,
/// not merely placement in an untracked in-memory preview buffer.
pub trait OutputCapture: Send + Sync {
    /// Reserve before spawning so a crash in the launch/registration gap is
    /// explicitly unproven, not mistaken for an execution without children.
    fn begin_process(&self) -> Result<String>;
    fn register_process(&self, ticket: &str, pid: u32) -> Result<()>;
    /// Only after failed spawn or observed whole-group quiescence and reaping.
    fn finish_process(&self, ticket: &str) -> Result<()>;
    fn report_progress(
        &self,
        progress: jcode_background_types::BackgroundTaskProgress,
        checkpoint: bool,
    ) -> Result<()>;
    fn write(&self, stream: OutputStream, bytes: &[u8]) -> Result<()>;
    fn reference(&self) -> Result<jcode_tool_types::OutputReference>;
    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<()>;
    fn read_part(&self, name: &str) -> Result<CapturedPart>;
}

pub struct CapturedPart {
    pub path: PathBuf,
    pub reader: std::fs::File,
}

/// A delivery wrapper delegates stop to the runtime that owns actual work.
/// Waiting alone does not grant ownership or imply successful cancellation.
#[async_trait]
pub trait OwnedExecutionControl: Send + Sync {
    async fn request_stop(&self, cause: jcode_tool_types::StopCause) -> Result<bool>;
    async fn wait(&self) -> Result<jcode_tool_types::RunState>;
    fn execution_id(&self) -> Option<String> {
        None
    }
    async fn survives_reload(&self) -> Result<bool> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub struct InvocationContext {
    /// Received SDK rejection for a tool structurally excluded by that SDK.
    /// Retained as an auxiliary part before the host starts its own operation.
    pub provider_rejection: Option<String>,
    pub provider_receipt: Option<jcode_tool_types::ProviderReceiptReference>,
    pub ancestors: Vec<String>,
    pub output_target: Option<std::num::NonZeroUsize>,
    pub capture: Option<Arc<dyn OutputCapture>>,
    pub policy: ExecutionPolicy,
    pub identity: Option<InvocationIdentity>,
    pub ready: Option<ExecutionReady>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureMode {
    #[default]
    Complete,
    SourceRead,
    NativeCommand,
}

#[derive(Clone)]
pub struct InvocationIdentity {
    pub id: String,
    pub owner: String,
}

#[derive(Clone)]
pub struct ExecutionPolicy {
    pub capture: CaptureMode,
    pub background: bool,
    pub foreground_timeout: Option<std::time::Duration>,
    pub notify: bool,
    pub wake: bool,
    pub manual_ready: bool,
    pub cooperative_stop: bool,
}
impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            capture: CaptureMode::Complete,
            background: false,
            foreground_timeout: None,
            notify: true,
            wake: false,
            manual_ready: false,
            cooperative_stop: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct ExecutionReady {
    flag: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}
impl ExecutionReady {
    pub fn mark(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    pub fn is_ready(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub async fn wait(&self) {
        let mut ready = std::pin::pin!(self.notify.notified());
        ready.as_mut().enable();
        if !self.is_ready() {
            ready.await;
        }
    }
}

pub const TOOL_INTENT_DESCRIPTION: &str =
    "Required short label shown in the UI: why this call is being made.";

/// Retired framework input retained only for explicit compatibility rejection.
/// A producer-owned field with this name stays inside its published arguments.
pub const ACCEPT_LARGE_OUTPUT_KEY: &str = "accept_large_output";

pub fn intent_schema_property() -> Value {
    serde_json::json!({
        "type": "string",
        "description": TOOL_INTENT_DESCRIPTION,
    })
}

/// Add the common presentation option and required display intent. Producer
/// collisions must first be wrapped through the tool's frozen InputBinding.
pub fn ensure_intent_in_schema(mut schema: Value) -> Value {
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    // Only touch object-shaped parameter schemas.
    let is_object_schema = object
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| t == "object")
        .unwrap_or_else(|| object.contains_key("properties"));
    if !is_object_schema {
        return schema;
    }

    let properties = object
        .entry("properties")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(properties) = properties.as_object_mut() {
        properties
            .entry("intent")
            .or_insert_with(intent_schema_property);
        // Presentation is optional and does not modify producer arguments.
        properties
            .entry("output_size")
            .or_insert_with(jcode_tool_types::presentation::schema);
    } else {
        return schema;
    }

    match object.get_mut("required") {
        Some(Value::Array(required)) => {
            if !required.iter().any(|v| v.as_str() == Some("intent")) {
                required.push(Value::String("intent".to_string()));
            }
        }
        _ => {
            object.insert(
                "required".to_string(),
                Value::Array(vec![Value::String("intent".to_string())]),
            );
        }
    }

    schema
}

/// A request for stdin input from a running command.
pub struct StdinInputRequest {
    pub request_id: String,
    pub prompt: String,
    pub is_password: bool,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub message_id: String,
    pub tool_call_id: String,
    pub working_dir: Option<PathBuf>,
    pub stdin_request_tx: Option<tokio::sync::mpsc::UnboundedSender<StdinInputRequest>>,
    pub graceful_shutdown_signal: Option<InterruptSignal>,
    pub execution_mode: ToolExecutionMode,
    pub invocation: InvocationContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    AgentTurn,
    Direct,
}

impl ToolContext {
    pub fn for_subcall(&self, tool_call_id: String) -> Self {
        let mut invocation = InvocationContext::default();
        invocation.ancestors.clone_from(&self.invocation.ancestors);
        invocation.ancestors.push(self.tool_call_id.clone());
        Self {
            session_id: self.session_id.clone(),
            message_id: self.message_id.clone(),
            tool_call_id,
            working_dir: self.working_dir.clone(),
            stdin_request_tx: self.stdin_request_tx.clone(),
            graceful_shutdown_signal: self.graceful_shutdown_signal.clone(),
            execution_mode: self.execution_mode,
            invocation,
        }
    }

    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(ref base) = self.working_dir {
            base.join(path)
        } else {
            path.to_path_buf()
        }
    }
}

/// A tool that can be executed by the agent.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (must match what's sent to the API).
    fn name(&self) -> &str;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// JSON Schema for the input parameters.
    fn parameters_schema(&self) -> Value;

    fn input_binding(&self) -> input::InputBinding {
        input::InputBinding::for_schema(&self.parameters_schema())
    }

    fn execution_policy(&self, _input: &Value, _ctx: &ToolContext) -> Result<ExecutionPolicy> {
        Ok(ExecutionPolicy::default())
    }

    /// Execute the tool with the given input.
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput>;

    /// Convert to API tool definition.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_binding().schema(self.parameters_schema()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_intent_adds_property_and_required() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string"}
            }
        });
        let out = ensure_intent_in_schema(schema);
        assert!(out["properties"]["intent"].is_object());
        let required: Vec<_> = out["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"command"));
        assert!(required.contains(&"intent"));
    }

    #[test]
    fn ensure_intent_creates_required_array_when_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {}
        });
        let out = ensure_intent_in_schema(schema);
        assert_eq!(out["required"], serde_json::json!(["intent"]));
    }

    #[test]
    fn ensure_intent_preserves_existing_intent_property() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["intent"],
            "properties": {
                "intent": {"type": "string", "description": "custom"}
            }
        });
        let out = ensure_intent_in_schema(schema);
        assert_eq!(out["properties"]["intent"]["description"], "custom");
        assert_eq!(
            out["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.as_str() == Some("intent"))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_intent_skips_non_object_schemas() {
        let schema = serde_json::json!({"type": "string"});
        let out = ensure_intent_in_schema(schema.clone());
        assert_eq!(out, schema);
    }
}

#[cfg(test)]
mod presentation_schema_tests {
    use super::*;

    #[test]
    fn injects_optional_presentation_into_object_schema() {
        // MCP tools are built from remote definitions and never edit their own
        // schemas, so they can only advertise the flag if injection is central.
        // A schema shaped like an MCP proxy's proves the mechanism.
        let mcp_shaped = serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": { "path": { "type": "string" } }
        });
        let out = ensure_intent_in_schema(mcp_shaped);
        assert!(out["properties"]["output_size"].is_object());
        assert!(out["properties"].get(ACCEPT_LARGE_OUTPUT_KEY).is_none());
        // Optional by design: requiring it would make the model answer a token
        // budget question on every call.
        let required: Vec<&str> = out["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"intent"));
        assert!(!required.contains(&ACCEPT_LARGE_OUTPUT_KEY));
    }

    #[test]
    fn never_overwrites_a_schema_that_declares_the_flag_itself() {
        let custom = serde_json::json!({
            "type": "object",
            "properties": {
                ACCEPT_LARGE_OUTPUT_KEY: { "type": "boolean", "description": "custom" }
            }
        });
        let out = ensure_intent_in_schema(custom);
        assert_eq!(
            out["properties"][ACCEPT_LARGE_OUTPUT_KEY]["description"], "custom",
            "a tool's own declaration must survive injection"
        );
    }

    #[test]
    fn the_legacy_boundary_key_remains_explicit() {
        // The registry reads this exact constant off raw tool input. If the two
        // ever diverge, the flag would be advertised but never honored, which is
        // worse than not offering it at all.
        assert_eq!(ACCEPT_LARGE_OUTPUT_KEY, "accept_large_output");
    }
}
