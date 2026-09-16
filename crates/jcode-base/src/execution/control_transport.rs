//! Shared local control client and wire contract. No Agent, provider or TUI state.
use super::{ExecutionStore, RunRecord, RuntimeEndpoint};
use anyhow::{Context, Result, ensure};
use jcode_tool_types::StopCause;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
pub const VERSION: u32 = 2;
pub const MAX_REQUEST: u64 = 16 * 1024;
const MAX_REPLY: u64 = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlOperation {
    Inspect,
    Wait,
    Stop { cause: StopCause },
    ForceStop,
    Background,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ControlReply {
    Snapshot { record: Box<RunRecord> },
    Accepted { changed: bool },
    OwnerChanged,
    Unavailable { message: String },
}

// Never derive Debug for the credential-bearing envelope.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub instance: String,
    pub key: String,
    pub run_id: String,
    pub action: ControlOperation,
}
#[derive(Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub reply: ControlReply,
}

async fn inspect(store: &ExecutionStore, id: &str) -> Result<RunRecord> {
    let store = store.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || store.inspect(&id)?.context("Unknown invocation")).await?
}
pub async fn exchange(
    endpoint: &RuntimeEndpoint,
    id: &str,
    action: ControlOperation,
) -> Result<ControlReply> {
    ensure!(
        (1..=VERSION).contains(&endpoint.protocol_version),
        "Execution owner does not support this control protocol"
    );
    ensure!(
        !matches!(&action, ControlOperation::ForceStop) || endpoint.protocol_version >= 2,
        "This execution owner predates force-stop support. Ordinary Stop remains available."
    );
    let check = endpoint.clone();
    ensure!(
        tokio::task::spawn_blocking(move || check.has_live_lease()).await??,
        "Execution control endpoint is no longer owned; no PID was signalled"
    );
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::transport::Stream::connect(&endpoint.endpoint),
    )
    .await??;
    #[cfg(unix)]
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "Execution control server is not this user"
    );
    let (read, mut write) = stream.into_split();
    let request = Request {
        version: endpoint.protocol_version,
        instance: endpoint.id.clone(),
        key: endpoint.transport_key().into(),
        run_id: id.into(),
        action,
    };
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    tokio::time::timeout(std::time::Duration::from_secs(5), write.write_all(&bytes)).await??;
    let mut reader = BufReader::new(read.take(MAX_REPLY + 1));
    let mut bytes = Vec::new();
    if matches!(request.action, ControlOperation::Wait) {
        reader.read_until(b'\n', &mut bytes).await?;
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_until(b'\n', &mut bytes),
        )
        .await??;
    }
    ensure!(
        bytes.len() as u64 <= MAX_REPLY && bytes.last() == Some(&b'\n'),
        "Incomplete or oversized execution control reply"
    );
    let response: Response = serde_json::from_slice(&bytes)?;
    ensure!(
        response.version == endpoint.protocol_version,
        "Execution control protocol changed"
    );
    Ok(response.reply)
}

/// Route only to a verified runtime identity. Never signal a PID inferred from
/// stale metadata. Control and status transfer no output bodies or credentials.
pub async fn control(id: &str, action: ControlOperation) -> Result<ControlReply> {
    let root = crate::storage::jcode_dir()?;
    let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
    control_in_store(&store, id, action).await
}
pub async fn control_in_store(
    store: &ExecutionStore,
    id: &str,
    action: ControlOperation,
) -> Result<ControlReply> {
    for _ in 0..2 {
        let record = inspect(store, id).await?;
        if record.state.terminal() {
            return Ok(match action {
                ControlOperation::Inspect | ControlOperation::Wait => ControlReply::Snapshot {
                    record: Box::new(record),
                },
                _ => ControlReply::Accepted { changed: false },
            });
        }
        let source = store.clone();
        let owner = record.owner.clone();
        let endpoint = tokio::task::spawn_blocking(move || {
            source
                .runtime_endpoint(&owner)?
                .context("Invocation has no verified runtime control endpoint")
        })
        .await??;
        match exchange(&endpoint, id, action.clone()).await {
            Ok(ControlReply::OwnerChanged) => continue,
            Ok(reply @ ControlReply::Unavailable { .. }) => {
                // A native worker may seal its outcome before shutting down
                // the task serving this request. Treat that reply like a lost
                // transport: durable truth wins, but unproven work stays unavailable.
                let current = inspect(store, id).await?;
                if current.state.terminal() {
                    return Ok(ControlReply::Snapshot {
                        record: Box::new(current),
                    });
                }
                if current.owner != endpoint.id {
                    continue;
                }
                return Ok(reply);
            }
            Ok(reply) => return Ok(reply),
            Err(error) => {
                let current = inspect(store, id).await?;
                if current.state.terminal() {
                    return Ok(ControlReply::Snapshot {
                        record: Box::new(current),
                    });
                }
                if current.owner != endpoint.id {
                    continue;
                }
                return Err(error);
            }
        }
    }
    anyhow::bail!(
        "Execution owner changed during control; retry the control request, not the operation"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_wait_reply_preserves_the_durable_terminal_outcome() -> Result<()> {
        use super::super::{Invocation, PreparedInvocation, RunState};
        for state in [
            RunState::Completed,
            RunState::Failed,
            RunState::Cancelled,
            RunState::Interrupted,
            RunState::Running,
        ] {
            let root = tempfile::tempdir()?;
            let store = ExecutionStore::open(root.path())?;
            let lease_path = root.path().join("owner.lease");
            let lease = std::fs::File::create(&lease_path)?;
            lease.lock()?;
            let endpoint = RuntimeEndpoint::new(
                "1".repeat(32),
                root.path().join("control.sock"),
                lease_path,
                "2".repeat(64),
            );
            let listener = tokio::net::UnixListener::bind(&endpoint.endpoint)?;
            store.register_runtime(&endpoint)?;
            let invocation = Invocation {
                session_id: "waiter".into(),
                message_id: "message".into(),
                call_path: vec!["call".into()],
                tool: "fixture".into(),
                input: serde_json::json!({}),
                working_dir: None,
                received_result_digest: None,
            };
            let PreparedInvocation::New(record) = store.prepare(&invocation, &endpoint.id)? else {
                panic!("new fixture invocation")
            };
            store.start(&record.id, &record.owner)?;
            let producer_store = store.clone();
            let id = record.id.clone();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await?;
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await?;
                let request: Request = serde_json::from_str(&line)?;
                assert!(matches!(request.action, ControlOperation::Wait));
                assert_eq!(request.run_id, record.id);
                // A native worker seals its result, then its runtime shutdown
                // can cancel the control handler's final metadata read.
                if state.terminal() {
                    producer_store.retain(
                        record,
                        jcode_tool_types::ToolOutput::new("result"),
                        state,
                    )?;
                }
                let response = Response {
                    version: VERSION,
                    reply: ControlReply::Unavailable {
                        message: "synthetic owner shutdown".into(),
                    },
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                    .await?;
                Ok::<_, anyhow::Error>(())
            });
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                control_in_store(&store, &id, ControlOperation::Wait),
            )
            .await??;
            server.await??;
            if state.terminal() {
                let ControlReply::Snapshot { record } = outcome else {
                    panic!(
                        "Persisted {state:?} outcome was hidden by the unavailable reply: {outcome:?}"
                    )
                };
                assert_eq!(record.state, state);
                assert_eq!(record.id, id);
            } else {
                assert!(matches!(outcome, ControlReply::Unavailable { .. }));
                assert_eq!(store.inspect(&id)?.unwrap().state, RunState::Running);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn legacy_owner_force_rejects_before_transport_but_ordinary_stop_keeps_legacy_route() {
        let root = tempfile::tempdir().unwrap();
        let mut endpoint = RuntimeEndpoint::new(
            "legacy".into(),
            root.path().join("absent.sock"),
            root.path().join("absent.lease"),
            "private".into(),
        );
        endpoint.protocol_version = 1;
        let error = exchange(&endpoint, "run-fixture", ControlOperation::ForceStop)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("predates"));
        let error = exchange(
            &endpoint,
            "run-fixture",
            ControlOperation::Stop {
                cause: StopCause::HumanCancellation,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("lease"),
            "Ordinary version-one control was rejected as incompatible: {error}"
        );
    }
}
