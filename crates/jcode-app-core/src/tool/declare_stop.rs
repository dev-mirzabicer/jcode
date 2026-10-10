//! `declare_stop`: a session with session work declares why it ends its
//! turn, then writes its final message. The rules live in
//! `session_work::stop`; this tool parses arguments, supplies the background
//! work the session owns and renders the approved texts.
use super::{Tool, ToolContext, ToolOutput};
use crate::instruction::notification::Notification;
use crate::session_work::stop::{self, DeclareInput, DeclareOutcome, Refusal};
use anyhow::{Context, Result};
use async_trait::async_trait;
use jcode_session_work_types::{Declaration, ItemKind, SessionWorkRole};
use serde_json::{Value, json};
use std::path::Path;

pub const DECLARE_STOP_TOOL: &str = "declare_stop";

pub struct DeclareStopTool;

impl DeclareStopTool {
    pub fn new() -> Self {
        Self
    }

    /// The child variant: completion only.
    pub fn child_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": crate::tool::intent_schema_property(),
                "action": {"type": "string", "enum": ["complete"]},
                "summary": {"type": "string"},
                "leave_running": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }
}

impl Default for DeclareStopTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep `declare_stop` only for sessions with session work, in the variant
/// of the session's role.
pub fn retain_for_session(
    tools: &mut Vec<crate::message::ToolDefinition>,
    session: &crate::session::Session,
) {
    match &session.session_work {
        None => tools.retain(|tool| tool.name != DECLARE_STOP_TOOL),
        Some(binding) if binding.role == SessionWorkRole::Child => {
            for tool in tools
                .iter_mut()
                .filter(|tool| tool.name == DECLARE_STOP_TOOL)
            {
                tool.input_schema = DeclareStopTool::child_schema();
            }
        }
        Some(_) => {}
    }
}

#[async_trait]
impl Tool for DeclareStopTool {
    fn name(&self) -> &str {
        DECLARE_STOP_TOOL
    }

    /// The approved description is managed guidance, frozen per session.
    fn description(&self) -> &str {
        ""
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": crate::tool::intent_schema_property(),
                "action": {"type": "string", "enum": ["wait", "complete", "withdraw"]},
                "condition": {"type": "string"},
                "timeout": {"type": "string"},
                "summary": {"type": "string"},
                "leave_running": {"type": "array", "items": {"type": "string"}},
                "closeout": {"type": "boolean"},
                "documents": {"type": "array", "items": {"type": "string"}},
                "accepted": {"type": "boolean"}
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn decode_input(&self, input: &Value) -> Result<()> {
        parse_input(input).map(drop)
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let request = super::native_files::session_work::request_base(&ctx);
        let store = crate::session_work::SessionWorkStore::new();
        let activation = store
            .activation(&ctx.session_id)
            .context("Session work is unavailable for this session")?
            .ok_or_else(|| {
                anyhow::anyhow!("declare_stop is available only in sessions with session work")
            })?;
        let input = parse_input(&input)?;
        let work = background_work(&ctx.session_id).await;
        let now = chrono::Utc::now();
        let items = stop::observe_items(&store, &ctx.session_id, &work, now)?;
        let config = crate::config::config();
        let outcome = stop::declare(
            &store,
            &ctx.session_id,
            activation.role,
            &request,
            &input,
            &items,
            &config.session_work.timeouts,
            now,
        )?;
        let working_dir = ctx.working_dir.as_deref();
        Ok(match outcome {
            DeclareOutcome::Declared {
                record,
                also_running,
            } => ToolOutput::new(declared_text(
                &record.declaration,
                &also_running,
                working_dir,
            )?)
            .with_title("stop declared"),
            DeclareOutcome::Withdrawn(_) => {
                ToolOutput::new(Notification::SessionWorkStopWithdrawn.render(working_dir)?)
                    .with_title("declaration withdrawn")
            }
            DeclareOutcome::Refused(refusal) => {
                ToolOutput::new(refusal_text(&refusal, working_dir)?)
                    .with_title("declaration refused")
                    .with_error(true)
            }
        })
    }
}

fn parse_input(input: &Value) -> Result<DeclareInput> {
    let text = |name: &str| input.get(name).and_then(Value::as_str).map(str::to_string);
    let list = |name: &str| -> Result<Vec<String>> {
        match input.get(name) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow::anyhow!("{name} must be a list of strings"))
                })
                .collect(),
            Some(_) => anyhow::bail!("{name} must be a list of strings"),
        }
    };
    let flag = |name: &str| input.get(name).and_then(Value::as_bool).unwrap_or(false);
    Ok(DeclareInput {
        action: text("action").context("action is required: wait, complete or withdraw")?,
        condition: text("condition"),
        timeout: text("timeout"),
        summary: text("summary"),
        leave_running: list("leave_running")?,
        closeout: flag("closeout"),
        documents: list("documents")?,
        accepted: flag("accepted"),
    })
}

/// The background work a session owns, from the background task registry.
pub(crate) async fn background_work(session: &str) -> Vec<stop::BackgroundWork> {
    crate::background::global()
        .list()
        .await
        .into_iter()
        .filter(|task| task.session_id == session)
        .map(|task| stop::BackgroundWork {
            kind: if task.tool_name == "subagent" {
                ItemKind::Child
            } else {
                ItemKind::Task
            },
            label: task
                .display_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| task.tool_name.clone()),
            wake: task.wake,
            running: task.status == crate::bus::BackgroundTaskStatus::Running,
            task_id: task.task_id,
        })
        .collect()
}

fn declared_text(
    declaration: &Declaration,
    also_running: &[jcode_session_work_types::SessionItem],
    working_dir: Option<&Path>,
) -> Result<String> {
    match declaration {
        Declaration::Wait {
            condition,
            timeout,
            timeout_source,
            ..
        } => {
            let mut text = Notification::SessionWorkStopWaitDeclared {
                condition: &condition.to_string(),
                timeout: &timeout.to_string(),
                timeout_source: timeout_source.label(),
            }
            .render(working_dir)?;
            if !also_running.is_empty() {
                text.push(' ');
                text.push_str(
                    &Notification::SessionWorkStopAlsoRunning {
                        items: &stop::item_list_and(also_running),
                    }
                    .render(working_dir)?,
                );
            }
            Ok(text)
        }
        Declaration::Complete { .. } => {
            Ok(Notification::SessionWorkStopCompleteDeclared.render(working_dir)?)
        }
    }
}

/// The approved text for a refused declaration, as a tool result or a
/// correction.
pub(crate) fn refusal_text(refusal: &Refusal, working_dir: Option<&Path>) -> Result<String> {
    Ok(match refusal {
        Refusal::Unfinished(modules) => Notification::SessionWorkStopRefusedWorkflow {
            modules: &stop::module_list(modules),
        }
        .render(working_dir)?,
        Refusal::StillRunning(items) => Notification::SessionWorkStopRefusedBackground {
            items: &stop::item_list(items),
        }
        .render(working_dir)?,
        Refusal::Invalid(reason) => {
            Notification::SessionWorkStopRefused { reason }.render(working_dir)?
        }
    })
}
