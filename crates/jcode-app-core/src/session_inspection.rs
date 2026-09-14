//! Authorized, streaming Session inspection over the ordinary execution owner.
use crate::execution::ExecutionStore;
use crate::session::Session;
use anyhow::{Context, Result, ensure};
use jcode_agent_runtime::InterruptSignal;
use jcode_tool_core::{
    ExecutionPolicy, InvocationContext, OutputCapture, OutputStream, ToolContext,
};
use jcode_tool_types::{
    OutputSource, ToolOutput,
    cleanup::{CleanupRequest, CleanupResponse},
    inspection::{InspectionRequest, InspectionResponse},
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy)]
enum Actor {
    Agent,
    Human,
}

fn parent(root: &Path, id: &str, stop: Option<&InterruptSignal>) -> Result<Option<String>> {
    let (continuation, original) = Session::inspection_relationships(root, id, stop)?;
    Ok(original.or(continuation))
}
fn ancestor(
    root: &Path,
    ancestor: &str,
    descendant: &str,
    stop: Option<&InterruptSignal>,
) -> Result<bool> {
    Session::inspection_has_ancestor(root, ancestor, descendant, stop)
}
fn authorize(
    root: &Path,
    reader: &str,
    target: &str,
    actor: Actor,
    stop: Option<&InterruptSignal>,
) -> Result<()> {
    if matches!(actor, Actor::Human) || reader == target {
        return Ok(());
    }
    let (_, reader_parent) = Session::inspection_relationships(root, reader, stop)?;
    if reader_parent.as_deref() == Some(target) {
        return Ok(());
    }
    let (_, target_parent) = Session::inspection_relationships(root, target, stop)?;
    if let Some(original) = target_parent
        && (original == reader || ancestor(root, &original, reader, stop)?)
    {
        return Ok(());
    }
    // Continuation ancestry conveys reading, never child follow-up/control ownership.
    ensure!(
        ancestor(root, target, reader, stop)? || ancestor(root, reader, target, stop)?,
        "Target Session is outside this agent's readable conversation relationships"
    );
    Ok(())
}

struct InspectionSink {
    capture: Option<Arc<dyn OutputCapture>>,
    stop: Option<InterruptSignal>,
    inline: Vec<u8>,
    pending: Vec<u8>,
    capture_failed: bool,
}
impl InspectionSink {
    fn flush_captured(&mut self) -> std::io::Result<()> {
        if self.capture_failed {
            return Err(std::io::Error::other(
                "Inspection capture previously failed; retained prefix is unchanged",
            ));
        }
        if self.pending.is_empty() {
            return Ok(());
        }
        if let Some(capture) = &self.capture {
            let result = capture
                .write(OutputStream::Text, &self.pending)
                .map_err(std::io::Error::other);
            self.pending.clear();
            if result.is_err() {
                self.capture_failed = true;
            }
            result
        } else {
            Ok(())
        }
    }
}
impl Write for InspectionSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for chunk in bytes.chunks(64 * 1024) {
            if self.stop.as_ref().is_some_and(|signal| signal.is_set()) {
                return Err(std::io::Error::other("Session inspection cancelled"));
            }
            if self.capture.is_some() {
                if self.pending.len() + chunk.len() > 64 * 1024 {
                    self.flush_captured()?;
                }
                self.pending.extend_from_slice(chunk);
                if self.pending.len() == 64 * 1024 {
                    self.flush_captured()?;
                }
            } else {
                self.inline.extend_from_slice(chunk);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_captured()
    }
}

fn render(
    root: &Path,
    reader: &str,
    request: &InspectionRequest,
    actor: Actor,
    sink: &mut InspectionSink,
) -> Result<String> {
    ensure!(
        !sink.stop.as_ref().is_some_and(|signal| signal.is_set()),
        "Session inspection cancelled"
    );
    let store = ExecutionStore::open(root)?;
    let now = chrono::Utc::now().timestamp();
    let opening = matches!(request, InspectionRequest::Outline { .. });
    let id = match request {
        InspectionRequest::Outline { target, .. } => {
            let target = match target.as_str() {
                "self" => reader.to_string(),
                "parent" => parent(root, reader, sink.stop.as_ref())?
                    .context("This session has no readable parent")?,
                _ => target.clone(),
            };
            authorize(root, reader, &target, actor, sink.stop.as_ref())?;
            let id = store.create_inspection_snapshot_with_stop(
                reader,
                &target,
                now,
                sink.stop.as_ref(),
            )?;
            // Publish the bounded identity receipt even if Stop arrived just
            // after snapshot commit. The common failure manifest retains it.
            if let Some(capture) = &sink.capture {
                capture.append_part("inspection-snapshot", id.as_bytes())?;
            }
            id
        }
        InspectionRequest::Transcript { snapshot_id, .. }
        | InspectionRequest::ExpandTool { snapshot_id, .. } => snapshot_id.clone(),
    };
    let snapshot = if matches!(actor, Actor::Human) {
        store.read_inspection_snapshot_for_client(reader, &id, now, sink.stop.as_ref())?
    } else {
        store.read_inspection_snapshot_with_stop(reader, &id, now, sink.stop.as_ref())?
    };
    if !opening && let Some(capture) = &sink.capture {
        capture.append_part("inspection-snapshot", id.as_bytes())?;
    }
    if matches!(actor, Actor::Human) {
        store.touch_activity(snapshot.target(), now)?;
    }
    match request {
        InspectionRequest::Outline { .. } => snapshot.write_outline(&mut *sink)?,
        InspectionRequest::Transcript { range, raw, .. } => {
            snapshot.write_transcript(&mut *sink, range.as_ref(), *raw)?
        }
        InspectionRequest::ExpandTool { tool_use_id, .. } => {
            snapshot.write_tool_expansion(&mut *sink, tool_use_id)?
        }
    }
    Ok(id)
}

async fn produce(
    root: PathBuf,
    reader: String,
    request: InspectionRequest,
    actor: Actor,
    capture: Option<Arc<dyn OutputCapture>>,
    stop: Option<InterruptSignal>,
) -> Result<ToolOutput> {
    tokio::task::spawn_blocking(move || {
        let mut sink = InspectionSink {
            capture,
            stop,
            inline: Vec::new(),
            pending: Vec::with_capacity(64 * 1024),
            capture_failed: false,
        };
        let rendered = render(&root, &reader, &request, actor, &mut sink);
        // Already formatted bytes remain available even when Stop interrupted
        // the next step. Never let a buffered writer's Drop retry partial I/O.
        sink.flush_captured()?;
        let id = rendered?;
        ensure!(
            !sink.stop.as_ref().is_some_and(|signal| signal.is_set()),
            "Session inspection cancelled after retaining its output"
        );
        let mut output = if let Some(capture) = &sink.capture {
            let mut output = ToolOutput::new("");
            output.source = OutputSource::Retained(capture.reference()?);
            output
        } else {
            ToolOutput::new(String::from_utf8(sink.inline)?)
        };
        output.metadata = Some(serde_json::json!({"inspection_snapshot_id":id}));
        Ok(output)
    })
    .await?
}

pub(crate) async fn agent_inspection_with_context(
    root: PathBuf,
    reader: String,
    request: InspectionRequest,
    capture: Option<Arc<dyn OutputCapture>>,
    stop: Option<InterruptSignal>,
) -> Result<ToolOutput> {
    produce(root, reader, request, Actor::Agent, capture, stop).await
}

#[cfg(test)]
async fn agent_inspection(
    root: PathBuf,
    reader: String,
    request: InspectionRequest,
) -> Result<ToolOutput> {
    agent_inspection_with_context(root, reader, request, None, None).await
}

/// Trusted client operations use the same supervision, capture, Stop and
/// retained-result presentation as model tools. No provider/producer is repeated.
pub(crate) async fn human_inspection(
    root: PathBuf,
    reader: String,
    request: InspectionRequest,
) -> Result<InspectionResponse> {
    let target = match &request {
        InspectionRequest::Outline { output_size, .. }
        | InspectionRequest::Transcript { output_size, .. }
        | InspectionRequest::ExpandTool { output_size, .. } => crate::config::config()
            .output
            .target("session_outline", *output_size),
    };
    let ctx = ToolContext {
        session_id: reader.clone(),
        message_id: format!("inspection-{}", uuid::Uuid::new_v4().simple()),
        tool_call_id: "client-inspection".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
        invocation: InvocationContext {
            policy: ExecutionPolicy {
                cooperative_stop: true,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let invocation =
        crate::execution::invocation(&ctx, "session_inspection", serde_json::to_value(&request)?);
    let origin = root.clone();
    let output = crate::execution::execute_at(
        root,
        invocation,
        ctx,
        target,
        Box::new(move |ctx| {
            Box::pin(async move {
                produce(
                    origin,
                    reader,
                    request,
                    Actor::Human,
                    ctx.invocation.capture,
                    ctx.graceful_shutdown_signal,
                )
                .await
            })
        }),
    )
    .await?;
    let snapshot_id = output
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("inspection_snapshot_id"))
        .and_then(serde_json::Value::as_str)
        .context("Retained inspection lost its snapshot correlation")?
        .to_string();
    Ok(InspectionResponse {
        snapshot_id,
        content: Box::new(output),
    })
}

/// Privileged client review/confirmation under D-29's trusted-client boundary.
/// This is not physical-human attestation and is not a Registry model tool.
pub(crate) async fn human_cleanup(
    root: PathBuf,
    reader: String,
    request: CleanupRequest,
) -> Result<CleanupResponse> {
    tokio::task::spawn_blocking(move || {
        let store = ExecutionStore::open(&root)?;
        let now = chrono::Utc::now().timestamp();
        match request {
            CleanupRequest::Status => Ok(CleanupResponse::Status {
                status: store.retention_status()?,
            }),
            CleanupRequest::Review { selection } => Ok(CleanupResponse::Review {
                review: store.review_output_cleanup(&reader, selection, now)?,
            }),
            CleanupRequest::Confirm {
                review_id,
                confirmation_id,
            } => Ok(CleanupResponse::Outcome {
                outcome: store.confirm_output_cleanup(
                    &reader,
                    &review_id,
                    &confirmation_id,
                    now,
                )?,
            }),
        }
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_inspection_flushes_its_available_prefix_without_accepting_new_bytes() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let invocation = crate::execution::Invocation {
            session_id: "reader".into(),
            message_id: "message".into(),
            call_path: vec!["inspection".into()],
            tool: "session_inspection".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let crate::execution::PreparedInvocation::New(record) =
            store.prepare(&invocation, "fixture")?
        else {
            panic!();
        };
        store.start(&record.id, "fixture")?;
        let capture = Arc::new(crate::execution::Capture::create(
            store.clone(),
            record.clone(),
            Default::default(),
        )?);
        let stop = InterruptSignal::new();
        let mut sink = InspectionSink {
            capture: Some(capture.clone()),
            stop: Some(stop.clone()),
            inline: Vec::new(),
            pending: Vec::with_capacity(64 * 1024),
            capture_failed: false,
        };
        sink.write_all(b"AVAILABLE_PREFIX")?;
        assert_eq!(capture.reference()?.bytes, 0);
        stop.fire_with_cause(jcode_tool_types::StopCause::HumanCancellation);
        assert!(sink.write_all(b"UNACCEPTED_TAIL").is_err());
        sink.flush_captured()?;
        assert_eq!(capture.reference()?.bytes, 16);
        store.request_stop(
            &record.id,
            "fixture",
            jcode_tool_types::StopCause::HumanCancellation,
        )?;
        let mut output = ToolOutput::new("");
        output.source = OutputSource::Retained(capture.reference()?);
        capture.seal(output, crate::execution::RunState::Cancelled)?;
        let output = store.result(
            &store.inspect(&record.id)?.unwrap(),
            std::num::NonZeroUsize::new(1000).unwrap(),
        )?;
        assert!(
            output.is_error
                && output.output.contains("AVAILABLE_PREFIX")
                && !output.output.contains("UNACCEPTED_TAIL")
        );
        Ok(())
    }
    fn seed(root: &Path, id: &str, parent: Option<&str>, text: &str) {
        let mut session = Session::create_with_id(id.into(), parent.map(str::to_string), None);
        session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
        );
        crate::storage::write_json_secret(
            &root.join("sessions").join(format!("{id}.json")),
            &session,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn seeded_ancestry_is_readable_without_granting_unrelated_access_or_mutating_source()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        seed(root.path(), "parent", None, "PARENT_SOURCE");
        seed(root.path(), "child", Some("parent"), "CHILD_SOURCE");
        seed(root.path(), "other", None, "UNRELATED_SOURCE");
        let before = std::fs::read(root.path().join("sessions/parent.json"))?;
        let outline = agent_inspection(
            root.path().into(),
            "child".into(),
            InspectionRequest::Outline {
                target: "parent".into(),
                output_size: None,
            },
        )
        .await?;
        assert!(outline.output.contains("PARENT_SOURCE"));
        let id = serde_json::from_str::<serde_json::Value>(&outline.output)?["snapshot_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            agent_inspection(
                root.path().into(),
                "child".into(),
                InspectionRequest::Outline {
                    target: "other".into(),
                    output_size: None
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("sessions/parent.json"))?,
            before
        );
        seed(root.path(), "parent", None, "LATER_SOURCE");
        let old = agent_inspection(
            root.path().into(),
            "child".into(),
            InspectionRequest::Transcript {
                snapshot_id: id,
                range: None,
                raw: true,
                output_size: None,
            },
        )
        .await?;
        assert!(old.output.contains("PARENT_SOURCE"));
        assert!(!old.output.contains("LATER_SOURCE"));
        let store = ExecutionStore::open(root.path())?;
        assert!(store.last_activity("child")?.is_some());
        assert!(
            store.last_activity("parent")?.is_none(),
            "An agent inspecting its target does not resume that target"
        );
        Ok(())
    }

    #[tokio::test]
    async fn human_inspection_retains_clipped_complete_output_and_counts_explicit_target_use()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        seed(root.path(), "reader", None, "READER");
        seed(
            root.path(),
            "target",
            None,
            &format!("{}TAIL_SENTINEL", "z".repeat(50_000)),
        );
        let result = human_inspection(
            root.path().into(),
            "reader".into(),
            InspectionRequest::Outline {
                target: "target".into(),
                output_size: Some(jcode_tool_types::presentation::OutputSize::Alias(
                    jcode_tool_types::presentation::OutputSizeAlias::VerySmall,
                )),
            },
        )
        .await?;
        assert!(!result.content.output.contains("TAIL_SENTINEL"));
        let jcode_tool_types::OutputSource::Retained(reference) = &result.content.source else {
            panic!("Expected retained complete outline");
        };
        assert!(std::fs::read_to_string(&reference.path)?.contains("TAIL_SENTINEL"));
        assert!(
            ExecutionStore::open(root.path())?
                .last_activity("target")?
                .is_some()
        );
        Ok(())
    }
}
