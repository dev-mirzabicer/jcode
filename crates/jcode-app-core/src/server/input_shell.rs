//! Human input commands share the existing native execution/capture owner.
use super::client_actions::combine_input_shell_output;
use crate::{agent::Agent, execution, protocol::ServerEvent};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use jcode_tool_core::{
    CaptureMode, ExecutionPolicy, InvocationContext, ToolContext, ToolExecutionMode,
};
use jcode_tool_types::OutputSource;
use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::Mutex;

pub(super) fn handle(
    id: u64,
    command: String,
    agent: &Arc<Mutex<Agent>>,
    events: &crate::client_delivery::ClientEventSender,
) {
    use crate::runtime_lifecycle::admission;
    let permit = match admission::preparation("input-shell", None) {
        Ok(permit) => permit,
        Err(error) => {
            let _ = events.send(ServerEvent::InputShellResult {
                result: crate::message::InputShellResult {
                    command,
                    cwd: None,
                    output: format!("Command was not started: {error:#}"),
                    exit_code: None,
                    duration_ms: 0,
                    truncated: false,
                    failed_to_start: true,
                },
            });
            let _ = events.send(ServerEvent::Done { id });
            return;
        }
    };
    let agent = agent.clone();
    let events = events.clone();
    tokio::spawn(admission::scope(permit.clone(), async move {
        let started = Instant::now();
        let mut result = crate::message::InputShellResult {
            command: command.clone(),
            cwd: None,
            output: String::new(),
            exit_code: None,
            duration_ms: 0,
            truncated: false,
            failed_to_start: true,
        };
        let prepared = admission::prepare(permit, async {
            let agent = agent.lock().await;
            Ok((
                agent.session_id().to_owned(),
                agent.working_dir().map(str::to_owned),
            ))
        })
        .await;
        match prepared {
            Err(error) => result.output = format!("Command was not started: {error:#}"),
            Ok((session, cwd)) => {
                result.cwd = cwd.clone();
                // Once execution owns the command, only its real Stop/handoff
                // controls may end it. Disconnect does not drop that waiter.
                result.failed_to_start = false;
                match execute(session, command, cwd.map(PathBuf::from)).await {
                    Ok(delivery) => {
                        result.output = delivery.output;
                        result.exit_code = delivery.code;
                        result.truncated = delivery.truncated;
                        result.failed_to_start = delivery.failed_to_start;
                    }
                    Err(error) => {
                        result.output = format!(
                            "Command outcome needs inspection: {error:#}. Do not repeat uncertain effects."
                        )
                    }
                }
            }
        }
        result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let _ = events.send(ServerEvent::InputShellResult { result });
        let _ = events.send(ServerEvent::Done { id });
    }));
}

struct Delivery {
    output: String,
    code: Option<i32>,
    truncated: bool,
    failed_to_start: bool,
}

async fn execute(session: String, command: String, cwd: Option<PathBuf>) -> Result<Delivery> {
    let cwd = cwd.map(Ok).unwrap_or_else(std::env::current_dir)?;
    let ctx = ToolContext {
        session_id: session,
        message_id: crate::id::new_id("input-shell"),
        tool_call_id: "input-shell".into(),
        working_dir: Some(cwd.clone()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: InvocationContext {
            policy: ExecutionPolicy {
                capture: CaptureMode::NativeCommand,
                manual_ready: true,
                cooperative_stop: true,
                notify: false,
                wake: false,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let invocation =
        execution::invocation(&ctx, "input_shell", serde_json::json!({"command":command}));
    let run = invocation.id();
    let request = execution::command_handoff::CommandRequest {
        command,
        working_dir: cwd,
        timeout_ms: None,
        background: false,
        notify: false,
        wake: false,
        title: Some("Input shell".into()),
        storage: crate::config::config().output.storage.clone(),
    };
    let output = execution::execute(
        invocation,
        ctx,
        NonZeroUsize::new(40_000).unwrap(),
        Box::new(move |ctx| Box::pin(execution::command_worker::launch(request, ctx))),
    )
    .await;
    let root = crate::storage::jcode_dir()?;
    crate::runtime_lifecycle::admission::spawn_blocking(move || -> Result<Delivery> {
        let store = execution::ExecutionStore::open(&root)?;
        let record = store
            .inspect(&run)?
            .with_context(|| format!("No execution receipt for {run}"))?;
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                ensure!(
                    record.state.terminal(),
                    "Execution {run} is not terminal: {error:#}"
                );
                store.result(&record, NonZeroUsize::new(40_000).unwrap())?
            }
        };
        if matches!(output.source, OutputSource::Acceptance(_)) {
            return Ok(Delivery {
                output: format!(
                    "Command continues as retained execution {run}. Inspect it through Tasks.\n{}",
                    output.output
                ),
                code: None,
                truncated: false,
                failed_to_start: false,
            });
        }
        let failed_to_start = store
            .command_ownership(&run)?
            .is_none_or(|owner| owner.process.is_none());
        if failed_to_start {
            return Ok(Delivery {
                output: output.output,
                code: None,
                truncated: false,
                failed_to_start: true,
            });
        }
        let info = store.read_part_page(&record, "streams.json", 0, 1024 * 1024, None)?;
        ensure!(
            info.next_offset.is_none(),
            "Stream metadata is incomplete for {run}"
        );
        let info: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD.decode(info.data_base64)?,
        )?;
        let read = |name: &str| -> Result<Vec<u8>> {
            let bytes = info[name]["bytes"]
                .as_u64()
                .context("Missing retained stream length")?;
            if bytes == 0 {
                return Ok(Vec::new());
            }
            let part = store.read_part_page(&record, &format!("{name}.bin"), 0, 32_768, None)?;
            ensure!(
                part.total_bytes == bytes,
                "Retained stream length changed for {run}"
            );
            Ok(base64::engine::general_purpose::STANDARD.decode(part.data_base64)?)
        };
        let (mut text, truncated) = combine_input_shell_output(&read("stdout")?, &read("stderr")?);
        if record.stop_cause.is_some() {
            text.push_str(&format!("\nCommand stopped. Retained execution: {run}"));
        }
        let code = output
            .metadata
            .as_ref()
            .and_then(|value| value["exit_code"].as_i64())
            .and_then(|code| i32::try_from(code).ok());
        Ok(Delivery {
            output: text,
            code,
            truncated,
            failed_to_start: false,
        })
    })
    .await?
}

#[cfg(test)]
#[path = "input_shell_tests.rs"]
mod tests;
