use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context, Result};
use async_trait::async_trait;
use jcode_tool_types::delegation::SubagentRequest;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct DelegationTool {
    catalog: bool,
    host: Option<Arc<crate::delegation::Host>>,
}
impl DelegationTool {
    pub fn proxies() -> [Self; 2] {
        [
            Self {
                catalog: true,
                host: None,
            },
            Self {
                catalog: false,
                host: None,
            },
        ]
    }
    pub fn hosted(host: Arc<crate::delegation::Host>) -> [Self; 2] {
        [
            Self {
                catalog: true,
                host: Some(host.clone()),
            },
            Self {
                catalog: false,
                host: Some(host),
            },
        ]
    }
}
#[async_trait]
impl Tool for DelegationTool {
    fn requires_shared_host(&self) -> bool {
        self.host.is_none()
    }
    fn name(&self) -> &str {
        if self.catalog {
            "get_catalog"
        } else {
            "subagent"
        }
    }
    fn description(&self) -> &str {
        if self.catalog {
            "List available isolated agent profiles, model aliases and task presets with usable selectors and short descriptions. Does not launch a child."
        } else {
            "Create or follow up with an isolated child conversation. Creation requires explicit agent, model_alias and permission. Children do not automatically receive your conversation. Read-only children automatically exclude MCP servers not classified read-only. blocked_mcps only adds exclusions. Ordinary calls wait for the reply; background is explicit. Reuse child_id for follow-ups, not creation settings."
        }
    }
    fn parameters_schema(&self) -> Value {
        if self.catalog {
            return json!({"type":"object","properties":{"working_dir":{"type":"string","description":"Optional project directory, defaulting to the caller's."}},"additionalProperties":false});
        }
        json!({"type":"object","properties":{
            "child_id":{"type":"string","description":"Existing child for a follow-up. Omit for creation."},
            "prompt":{"type":"string","description":"Complete self-contained task or follow-up, preserved exactly."},
            "agent":{"type":"string","description":"Required at creation. Isolated-capable profile selector from get_catalog."},
            "model_alias":{"type":"string","description":"Required at creation. Model-policy alias, not a concrete model name."},
            "permission":{"type":"string","enum":["read_only","read_write"],"description":"Required at creation. Follow-up omission retains current permission at turn start."},
            "effort":{"type":"string","description":"Creation only. Omission uses alias/provider default, not literal none."},
            "preset":{"type":"string","description":"Task preset, default general on creation. Follow-up omission or same identity does not reapply its source."},
            "working_dir":{"type":"string","description":"Creation only. Defaults to caller's working directory."},
            "startup_files":{"type":"array","items":{"type":"string"},"description":"Creation only. Ordered custom Startup Context selection replaces the current project default for this child. Empty means none."},
            "disable_startup_context":{"type":"boolean","description":"Creation only. Captures nothing. Conflicts with any startup_files list."},
            "blocked_mcps":{"type":"array","items":{"type":"string"},"uniqueItems":true,"description":"Creation only. Optional extra MCP-server exclusions retained by this child. Do not list mutating servers merely because the child is read-only; those are excluded automatically."},
            "queue_if_busy":{"type":"boolean","description":"Follow-up only. Explicitly accept FIFO queueing instead of rejecting a busy child. Defaults false."},
            "run_in_background":{"type":"boolean","description":"Return an acceptance receipt instead of waiting. Defaults false; slow foreground work does not auto-detach."},
            "notify":{"type":"boolean","description":"Background notification, default true."},
            "wake":{"type":"boolean","description":"Background wake of the original parent, default false."}
        },"required":["prompt"],"additionalProperties":false})
    }
    fn execution_policy(
        &self,
        input: &Value,
        _: &ToolContext,
    ) -> Result<jcode_tool_core::ExecutionPolicy> {
        if self.catalog {
            return Ok(jcode_tool_core::ExecutionPolicy {
                cooperative_stop: true,
                ..Default::default()
            });
        }
        let request =
            SubagentRequest::from_tool_input(input.clone()).map_err(anyhow::Error::msg)?;
        Ok(jcode_tool_core::ExecutionPolicy {
            background: request.delivery.background,
            notify: request.delivery.notify,
            wake: request.delivery.wake,
            manual_ready: true,
            cooperative_stop: true,
            ..Default::default()
        })
    }
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let host = self
            .host
            .as_ref()
            .context("Delegation requires the compatible shared execution host")?;
        if self.catalog {
            host.catalog(input, ctx).await
        } else {
            host.submit(
                SubagentRequest::from_tool_input(input).map_err(anyhow::Error::msg)?,
                ctx,
            )
            .await
        }
    }
}
