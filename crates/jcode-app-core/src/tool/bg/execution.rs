use super::*;
use crate::execution::{ControlOperation, ControlReply, ExecutionStore, RunRecord};
use anyhow::{Context, ensure};
use jcode_tool_core::OutputStream;
use jcode_tool_types::{OutputSource, StopCause};
use std::io::{Read, Seek, SeekFrom};

async fn snapshot(id: &str) -> Result<RunRecord> {
    let root = crate::storage::jcode_dir()?;
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        ExecutionStore::open(&root)?
            .inspect(&id)?
            .context("Unknown execution run")
    })
    .await?
}

pub(super) async fn execute(params: BgInput, ctx: ToolContext) -> Result<ToolOutput> {
    let id = params.task_id.as_deref().context("Missing run identity")?;
    ensure!(
        params.task_ids.as_ref().is_none_or(Vec::is_empty),
        "Use one durable run ID per control call, or batch independent controls"
    );
    let action = params
        .action
        .as_deref()
        .context("Durable run control requires an explicit action")?;
    let mut ancestry = crate::execution::invocation(&ctx, "bg", Value::Null);
    while !ancestry.call_path.is_empty() {
        ensure!(
            ancestry.id() != id,
            "A tool cannot wait on or control its own invocation or an enclosing batch"
        );
        ancestry.call_path.pop();
    }
    let record = snapshot(id).await?;
    if matches!(action, "cancel" | "watch" | "delivery" | "subscribe") {
        crate::tool::child_policy::authorize_task_control(&ctx, id, &record.session_id)?;
    }
    if params.session_only == Some(true) {
        ensure!(
            record.session_id == ctx.session_id,
            "Run belongs to another session"
        );
    }
    match action {
        "watch" | "delivery" | "subscribe" => {
            let status = background::global()
                .update_delivery(
                    id,
                    params.notify.unwrap_or(true),
                    params.wake.unwrap_or(true),
                )
                .await?
                .context("Unknown background delivery")?;
            Ok(ToolOutput::new(serde_json::to_string_pretty(&status)?)
                .with_metadata(serde_json::to_value(status)?))
        }
        "status" => Ok(ToolOutput::new(serde_json::to_string_pretty(&record)?)
            .with_metadata(serde_json::to_value(&record)?)),
        "cancel" => {
            ensure!(
                params.graceful_timeout_ms.is_none(),
                "Custom process grace periods require the command's background-task control ID"
            );
            let reply = crate::execution::control(
                id,
                ControlOperation::Stop {
                    cause: StopCause::HumanCancellation,
                },
            )
            .await?;
            if let ControlReply::Unavailable { message } = reply {
                anyhow::bail!("{message}");
            }
            Ok(ToolOutput::new(serde_json::to_string_pretty(&reply)?)
                .with_title("Stop request submitted; inspect status for actual completion"))
        }
        "wait" => {
            let (record, reason) = wait_snapshot(
                id,
                record,
                params.max_wait_seconds,
                params.return_on_progress.unwrap_or(true),
            )
            .await?;
            let preview = params.include_output_preview.unwrap_or(
                record.state.terminal() && record.state != crate::execution::RunState::Completed,
            );
            let result = json!({"reason":reason,"run":record});
            let prefix = serde_json::to_string_pretty(&result)?;
            if preview && record.output_path.is_some() {
                read_output(
                    record,
                    Some(
                        params
                            .tail_lines
                            .or(params.lines)
                            .unwrap_or(DEFAULT_WAIT_PREVIEW_LINES),
                    ),
                    ctx,
                    format!("{prefix}\n\nOutput preview:\n"),
                    result,
                )
                .await
            } else {
                Ok(ToolOutput::new(prefix).with_metadata(result))
            }
        }
        "output" | "tail" => {
            let lines = params
                .tail_lines
                .or(params.lines)
                .or((action == "tail").then_some(DEFAULT_TAIL_LINES));
            let metadata = json!({"source_run":id,"source_path":record.output_path,"end_byte":record.output_bytes,"complete":record.complete});
            read_output(record, lines, ctx, String::new(), metadata).await
        }
        _ => anyhow::bail!(
            "Action '{action}' requires a background delivery-task ID; durable run IDs support status, wait, cancel, output and tail"
        ),
    }
}

async fn read_output(
    record: RunRecord,
    lines: Option<usize>,
    ctx: ToolContext,
    prefix: String,
    metadata: Value,
) -> Result<ToolOutput> {
    ensure!(
        record.output_path.is_some(),
        "Run has no retained output yet"
    );
    let root = crate::storage::jcode_dir()?;
    let stop = ctx.graceful_shutdown_signal;
    let capture = ctx.invocation.capture;
    let limit = record.output_bytes;
    tokio::task::spawn_blocking(move || {
        let store = ExecutionStore::open(&root)?;
        let mut file = store.open_retained_output(&record.id, stop.as_ref())?;
        let start = if let Some(lines) = lines {
            tail_start(&mut file, limit, lines)?
        } else {
            0
        };
        file.seek(SeekFrom::Start(start))?;
        let mut selected = file.take(limit - start);
        let mut output = ToolOutput::new(prefix).with_metadata(metadata);
        if let Some(capture) = capture {
            capture.write(OutputStream::Text, output.output.as_bytes())?;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let n = selected.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                capture.write(OutputStream::Text, &buffer[..n])?;
            }
            output.output.clear();
            output.source = OutputSource::Retained(capture.reference()?);
        } else {
            selected.read_to_string(&mut output.output)?;
        }
        Ok(output)
    })
    .await?
}

fn tail_start(file: &mut (impl Read + Seek), end: u64, lines: usize) -> Result<u64> {
    if lines == 0 {
        return Ok(end);
    }
    let mut position = end;
    let mut found = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    while position > 0 {
        let start = position.saturating_sub(buffer.len() as u64);
        let count = (position - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        for i in (0..count).rev() {
            let offset = start + i as u64;
            if buffer[i] == b'\n' && offset + 1 < end {
                found += 1;
                if found == lines {
                    return Ok(offset + 1);
                }
            }
        }
        position = start;
    }
    Ok(0)
}

async fn wait_snapshot(
    id: &str,
    initial: RunRecord,
    seconds: Option<u64>,
    progress: bool,
) -> Result<(RunRecord, &'static str)> {
    if initial.state.terminal() {
        return Ok((initial, "terminal"));
    }
    if seconds == Some(0) {
        return Ok((initial, "timeout"));
    }
    let sequence = initial
        .progress
        .as_ref()
        .map(|value| value.sequence)
        .unwrap_or(0);
    let completion = crate::execution::control(id, ControlOperation::Wait);
    tokio::pin!(completion);
    let deadline = seconds
        .map(|value| {
            tokio::time::Instant::now()
                .checked_add(Duration::from_secs(value))
                .context("Wait deadline exceeds the platform clock")
        })
        .transpose()?;
    let mut poll = tokio::time::interval(Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            result=&mut completion=>return match result? {
                ControlReply::Snapshot{record}=>Ok((*record,"terminal")),
                ControlReply::Unavailable{message}=>Err(anyhow::anyhow!(message)),
                _=>Err(anyhow::anyhow!("Execution owner did not return a terminal snapshot")),
            },
            _=async{if let Some(deadline)=deadline{tokio::time::sleep_until(deadline).await}else{std::future::pending::<()>().await}}=>{
                let record=snapshot(id).await?;let reason=if record.state.terminal(){"terminal"}else{"timeout"};return Ok((record,reason));
            },
            _=poll.tick(),if progress=>{
                let record=snapshot(id).await?;
                if record.state.terminal(){return Ok((record,"terminal"));}
                if let Some(value)=&record.progress && value.sequence>sequence {
                    let reason=if value.checkpoint{"checkpoint"}else{"progress"};return Ok((record,reason));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_output_rejects_changed_retained_bytes() -> Result<()> {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
        let invocation = crate::execution::Invocation {
            session_id: "reader".into(),
            message_id: "message".into(),
            call_path: vec!["source".into()],
            tool: "fixture".into(),
            input: json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let crate::execution::PreparedInvocation::New(record) =
            store.prepare(&invocation, "fixture")?
        else {
            panic!("new source")
        };
        store.start(&record.id, &record.owner)?;
        store.retain(
            record.clone(),
            ToolOutput::new("first\r\noriginal\n"),
            crate::execution::RunState::Completed,
        )?;
        let record = store.inspect(&record.id)?.unwrap();
        let context = || ToolContext {
            session_id: "reader".into(),
            message_id: "message".into(),
            tool_call_id: "read".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        assert_eq!(
            read_output(record.clone(), Some(1), context(), String::new(), json!({}))
                .await?
                .output,
            "original\n"
        );
        // Same length: checking only metadata/length cannot establish integrity.
        std::fs::write(record.output_path.as_ref().unwrap(), "first\r\nmodified\n")?;
        for lines in [None, Some(1)] {
            assert!(
                read_output(record.clone(), lines, context(), String::new(), json!({}))
                    .await
                    .is_err(),
                "bg must not label modified bytes as retained output"
            );
        }
        Ok(())
    }

    #[test]
    fn tail_offsets_preserve_crlf_and_arbitrarily_long_lines() -> Result<()> {
        for text in [
            String::new(),
            "a\r\nb\r\n".into(),
            "a\nb".into(),
            format!("first\n{}\nlast", "🙂".repeat(100_000)),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("source");
            std::fs::write(&path, &text)?;
            for lines in [0, 1, 2, 20] {
                let mut file = std::fs::File::open(&path)?;
                let start = tail_start(&mut file, text.len() as u64, lines)?;
                let parts: Vec<_> = text.split_inclusive('\n').collect();
                let expected = parts[parts.len().saturating_sub(lines)..].concat();
                assert_eq!(&text[start as usize..], expected);
            }
        }
        Ok(())
    }
}
