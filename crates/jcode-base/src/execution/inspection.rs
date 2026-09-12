//! Human/client inspection over existing metadata, source reader and live control.
//! Callers authorize the same-user connection. No Agent mutex or producer is used.
use super::{
    ExecutionStore, RunRecord,
    control_transport::{ControlOperation, ControlReply, control_in_store},
};
use anyhow::{Context, Result, ensure};
use jcode_tool_types::{
    StopCause,
    execution::{ExecutionContent, ExecutionRequest, ExecutionResponse},
};
use std::path::Path;

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 68
            && id.starts_with("run-")
            && id[4..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid execution identity"
    );
    Ok(())
}

async fn record(store: &ExecutionStore, id: &str) -> Result<RunRecord> {
    validate_id(id)?;
    let store = store.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        store
            .inspect(&id)?
            .context("Execution record is unavailable")
    })
    .await?
}

pub async fn inspect(
    state_root: &Path,
    session: &str,
    request: ExecutionRequest,
) -> Result<ExecutionResponse> {
    ensure!(
        !session.is_empty(),
        "Execution inspection requires an attached session"
    );
    let root = state_root.to_path_buf();
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
    let is_stop = matches!(&request, ExecutionRequest::Stop { .. });
    match request {
        ExecutionRequest::List {
            all_sessions,
            after,
            limit,
        } => {
            let limit = limit.unwrap_or(50);
            ensure!(
                (1..=200).contains(&limit),
                "Execution page limit must be 1 through 200"
            );
            if let Some(after) = &after {
                validate_id(after)?;
            }
            let session = session.to_string();
            let mut runs = tokio::task::spawn_blocking(move || {
                if all_sessions {
                    store.list_all(after.as_deref(), limit + 1)
                } else {
                    store.list(&session, after.as_deref(), limit + 1)
                }
            })
            .await??;
            let next = if runs.len() > limit as usize {
                runs.truncate(limit as usize);
                runs.last().map(|run| run.id.clone())
            } else {
                None
            };
            Ok(ExecutionResponse::List { runs, next })
        }
        ExecutionRequest::Inspect { run_id } => Ok(ExecutionResponse::Status {
            run: Box::new(record(&store, &run_id).await?),
        }),
        ExecutionRequest::Stop { run_id } | ExecutionRequest::Background { run_id } => {
            validate_id(&run_id)?;
            let operation = if is_stop {
                ControlOperation::Stop {
                    cause: StopCause::HumanCancellation,
                }
            } else {
                ControlOperation::Background
            };
            let reply = control_in_store(&store, &run_id, operation).await?;
            let accepted = match reply {
                ControlReply::Accepted { changed } => changed,
                ControlReply::Snapshot { .. } => false,
                ControlReply::Unavailable { message } => anyhow::bail!(message),
                ControlReply::OwnerChanged => {
                    anyhow::bail!("Execution owner changed; retry control, not the original tool")
                }
            };
            let current = record(&store, &run_id).await?;
            Ok(ExecutionResponse::Control {
                run_id,
                accepted,
                state: current.state,
            })
        }
        ExecutionRequest::Read {
            run_id,
            content,
            read_point,
            output_size,
        } => {
            let current = record(&store, &run_id).await?;
            let target = crate::config::config().output.target("read", output_size);
            let root = state_root.to_path_buf();
            let page = tokio::task::spawn_blocking(move || {
                let path = match content {
                    ExecutionContent::Input => {
                        store.invocation_input(&current.id)?;
                        let expected = store
                            .root()
                            .join("inputs")
                            .join(format!("{}.json", current.id));
                        ensure!(
                            current.input_path == expected
                                && std::fs::symlink_metadata(&expected)?.file_type().is_file(),
                            "Execution input path does not match its owned record"
                        );
                        expected
                    }
                    ExecutionContent::Output => match current.output_path.clone() {
                        Some(path) => {
                            ensure!(
                                path == store
                                    .root()
                                    .join("outputs")
                                    .join(&current.id)
                                    .join("output.txt"),
                                "Execution output path does not match its owned record"
                            );
                            path
                        }
                        None => {
                            ensure!(
                                read_point.is_none(),
                                "This execution has no output source for the supplied read point"
                            );
                            return store.result(&current, target);
                        }
                    },
                };
                super::reader::SourceReader::new(&root).read(super::reader::ReadRequest {
                    path,
                    point: read_point,
                    start_line: 1,
                    end_line: None,
                    target,
                    stop: None,
                })
            })
            .await??;
            Ok(ExecutionResponse::Content {
                run_id,
                page: Box::new(page),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};
    use jcode_tool_types::{
        RunState, ToolOutput,
        presentation::{OutputSize, OutputSizeAlias},
    };

    #[tokio::test]
    async fn execution_inspection_pages_metadata_and_reads_owned_content_without_producers()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let mut records = Vec::new();
        for (index, session) in ["s1", "s1", "s1", "s2"].into_iter().enumerate() {
            let invocation = Invocation {
                session_id: session.into(),
                message_id: format!("m-{index}"),
                call_path: vec!["call".into()],
                tool: "fixture".into(),
                input: serde_json::json!({"value":"INPUT_TAIL"}),
                working_dir: None,
                received_result_digest: None,
            };
            let PreparedInvocation::New(record) = store.prepare(&invocation, "fixture")? else {
                panic!()
            };
            store.start(&record.id, "fixture")?;
            store.retain(
                record.clone(),
                ToolOutput::new(format!("{}TAIL", "α".repeat(30000))),
                RunState::Completed,
            )?;
            records.push(store.inspect(&record.id)?.unwrap());
        }
        // Listing/status do not open the backing body, even when unavailable.
        let path = records[0].output_path.as_ref().unwrap();
        let moved = path.with_extension("unavailable");
        std::fs::rename(path, &moved)?;
        let first = inspect(
            root.path(),
            "s1",
            ExecutionRequest::List {
                all_sessions: false,
                after: None,
                limit: Some(2),
            },
        )
        .await?;
        let ExecutionResponse::List { runs, next } = first else {
            panic!()
        };
        assert_eq!(runs.len(), 2);
        assert!(next.is_some());
        let second = inspect(
            root.path(),
            "s1",
            ExecutionRequest::List {
                all_sessions: false,
                after: next,
                limit: Some(2),
            },
        )
        .await?;
        let ExecutionResponse::List { runs: tail, next } = second else {
            panic!()
        };
        assert_eq!(tail.len(), 1);
        assert!(next.is_none());
        assert!(!runs.iter().any(|run| run.id == tail[0].id));
        assert!(matches!(
            inspect(
                root.path(),
                "s1",
                ExecutionRequest::Inspect {
                    run_id: records[0].id.clone()
                }
            )
            .await?,
            ExecutionResponse::Status { .. }
        ));
        std::fs::rename(&moved, path)?;
        let ExecutionResponse::List { runs, .. } = inspect(
            root.path(),
            "s1",
            ExecutionRequest::List {
                all_sessions: true,
                after: None,
                limit: None,
            },
        )
        .await?
        else {
            panic!()
        };
        assert_eq!(runs.len(), 4);
        let id = records[0].id.clone();
        let ExecutionResponse::Content { page, .. } = inspect(
            root.path(),
            "s1",
            ExecutionRequest::Read {
                run_id: id.clone(),
                content: ExecutionContent::Output,
                read_point: None,
                output_size: Some(OutputSize::Alias(OutputSizeAlias::VerySmall)),
            },
        )
        .await?
        else {
            panic!()
        };
        let jcode_tool_types::OutputSource::ReadPage(reference) = page.source else {
            panic!()
        };
        assert!(reference.next_point.is_some());
        assert!(!page.output.contains("TAIL"));
        let ExecutionResponse::Content { page, .. } = inspect(
            root.path(),
            "s1",
            ExecutionRequest::Read {
                run_id: id.clone(),
                content: ExecutionContent::Output,
                read_point: reference.next_point,
                output_size: Some(OutputSize::Alias(OutputSizeAlias::VeryLarge)),
            },
        )
        .await?
        else {
            panic!()
        };
        assert!(page.output.contains("TAIL"));
        let ExecutionResponse::Content { page, .. } = inspect(
            root.path(),
            "s1",
            ExecutionRequest::Read {
                run_id: id.clone(),
                content: ExecutionContent::Input,
                read_point: None,
                output_size: None,
            },
        )
        .await?
        else {
            panic!()
        };
        assert!(page.output.contains("INPUT_TAIL"));
        assert!(matches!(
            inspect(root.path(), "s1", ExecutionRequest::Stop { run_id: id }).await?,
            ExecutionResponse::Control {
                accepted: false,
                state: RunState::Completed,
                ..
            }
        ));
        assert!(
            inspect(
                root.path(),
                "s1",
                ExecutionRequest::Inspect {
                    run_id: "../../unowned".into()
                }
            )
            .await
            .is_err()
        );
        Ok(())
    }
}
