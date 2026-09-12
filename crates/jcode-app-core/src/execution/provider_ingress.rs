//! Acquisition-time SDK result storage. This owns immutable received records,
//! not a second conversation and not execution of the SDK's tool.
use super::*;
use std::fs::{File, OpenOptions};

#[derive(Default)]
pub struct ProviderIngress {
    state: Option<IngressState>,
    ordinal: u64,
}
struct IngressState {
    store: ExecutionStore,
    session: String,
    request: String,
    lease_path: PathBuf,
    _lease: Arc<File>,
    owner: Arc<runtime::RuntimeHandle>,
}
impl ProviderIngress {
    /// When some received IDs lack a unique tool use, return only the calls
    /// safe to checkpoint. Original ambiguous inputs stay in acquisition data.
    pub fn correlation_failure(
        calls: &[crate::message::ToolCall],
        results: &HashMap<String, ToolOutput>,
    ) -> Option<Vec<crate::message::ToolCall>> {
        let mut counts = HashMap::new();
        for call in calls {
            *counts.entry(call.id.as_str()).or_insert(0usize) += 1;
        }
        if results.keys().all(|id| counts.get(id.as_str()) == Some(&1)) {
            return None;
        }
        Some(
            calls
                .iter()
                .filter(|call| counts.get(call.id.as_str()) == Some(&1))
                .cloned()
                .collect(),
        )
    }
    pub async fn receive(
        &mut self,
        session: &str,
        tool_use_id: &str,
        output: &ToolOutput,
    ) -> Result<ToolOutput> {
        self.receive_with_calls(session, tool_use_id, output, &[])
            .await
    }
    pub async fn receive_with_calls(
        &mut self,
        session: &str,
        tool_use_id: &str,
        output: &ToolOutput,
        calls: &[crate::message::ToolCall],
    ) -> Result<ToolOutput> {
        ensure!(
            output.provider_receipt.is_none() && matches!(output.source, OutputSource::Inline),
            "Provider result already has a capture identity"
        );
        let observed_calls = calls
            .iter()
            .filter(|call| call.id == tool_use_id)
            .cloned()
            .collect::<Vec<_>>();
        if self.state.is_none() {
            let root = crate::storage::jcode_dir()?;
            let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
            let owner = runtime::ensure_running(&store).await?;
            let request = uuid::Uuid::new_v4().simple().to_string();
            let directory = store.root().join("provider-requests");
            let lease_path = directory.join(format!("{request}.lease"));
            let path = lease_path.clone();
            let lease = tokio::task::spawn_blocking(move || -> Result<File> {
                crate::storage::ensure_dir(&directory)?;
                ensure!(
                    std::fs::symlink_metadata(&directory)?.is_dir(),
                    "Provider request directory changed type"
                );
                let mut options = OpenOptions::new();
                options.create_new(true).read(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                let file = options.open(path)?;
                file.lock()?;
                file.sync_all()?;
                #[cfg(unix)]
                File::open(directory)?.sync_all()?;
                Ok(file)
            })
            .await??;
            self.state = Some(IngressState {
                store,
                session: session.into(),
                request,
                lease_path,
                _lease: Arc::new(lease),
                owner,
            });
        }
        let state = self.state.as_ref().unwrap();
        ensure!(
            state.session == session,
            "Provider ingress cannot change sessions"
        );
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .context("Provider result sequence exhausted")?;
        let output = output.clone();
        let store = state.store.clone();
        let request = state.request.clone();
        let lease = state.lease_path.clone();
        let owner = state.owner.endpoint.id.clone();
        let tool_use_id = tool_use_id.to_string();
        let session = session.to_string();
        let ordinal = self.ordinal;
        let config = crate::config::config().output.storage.clone();
        let request_lease = state._lease.clone();
        tokio::task::spawn_blocking(move||{
            let _request_lease=request_lease;
            let digest=crate::execution::output_digest(&output)?;
            let input=serde_json::json!({"source_kind":"provider_result_receipt","provider_tool_use_id":tool_use_id,"payload_digest":digest,"observed_tool_calls":observed_calls});
            let invocation=Invocation{session_id:session,message_id:format!("provider-request-{request}"),call_path:vec![format!("receipt-{ordinal}")],tool:"provider_result_receipt".into(),input,working_dir:None,received_result_digest:None};
            let PreparedInvocation::New(record)=store.prepare(&invocation,&owner)? else{anyhow::bail!("Provider result acquisition identity already exists; no capture was repeated");};
            let id=record.id.clone();
            let result=(||->Result<ToolOutput>{
                let receipt=store.register_provider_receipt(&record,&request,&lease,&tool_use_id)?;
                store.start(&record.id,&owner)?;
                let namespace=store.provider_receipt_namespace()?;
                let capture=Capture::create(store.clone(),record,config)?;
                let mut output=capture.seal(output,RunState::Completed)?;
                output.source=OutputSource::Inline;
                output.provider_receipt=Some(jcode_tool_types::ProviderReceiptReference{namespace,run_id:receipt.run_id,sequence:receipt.sequence});
                Ok(output)
            })();
            if result.is_err() {
                let mut current=store.inspect(&id)?.context("Provider acquisition record disappeared")?;
                if !current.state.terminal() {
                    let witness=store.root().join("receipts").join(format!("{id}.json")).try_exists()? || store.root().join("outputs").join(&id).join("manifest.json").try_exists()?;
                    if witness {store.recover_terminal_output(&id)?;} else {
                        current.state=RunState::Failed;current.complete=false;store.finish(&current)?;
                    }
                }
            }
            result
        }).await?
    }
}

/// Acknowledgement is part of the Session history transaction, not an unrelated
/// SQL flag. Rewind therefore cannot accidentally reannounce old received data.
pub(super) async fn reconcile(session: &mut crate::session::Session) -> Result<usize> {
    let root = crate::storage::jcode_dir()?;
    let session_id = session.id.clone();
    let watermark = session.provider_receipt_watermark;
    let previous_namespace = session.provider_receipt_namespace.clone();
    let found = tokio::task::spawn_blocking(move || -> Result<_> {
        if !root.join("execution/index.sqlite").try_exists()? {
            return Ok(None);
        }
        let store = ExecutionStore::open(&root)?;
        let namespace = store.provider_receipt_namespace()?;
        if namespace == previous_namespace {
            store.validate_provider_receipt_watermark(&session_id, watermark)?;
        }
        let receipts = store.provider_receipts_after(
            &session_id,
            if namespace == previous_namespace {
                watermark
            } else {
                0
            },
        )?;
        Ok(Some((store, receipts, namespace)))
    })
    .await??;
    let Some((store, receipts, namespace)) = found else {
        return Ok(0);
    };
    let mut recovered = 0;
    for receipt in receipts {
        let check = receipt.clone();
        let lease_store = store.clone();
        ensure!(
            !tokio::task::spawn_blocking(move || check.request_is_live(&lease_store)).await??,
            "A provider request still owns unacknowledged received results; no history repair or replay was performed"
        );
        let paired=receipt.message_id.as_ref().and_then(|id|session.messages.iter().position(|message|{
            message.id==*id && message.role==crate::message::Role::Assistant && message.content.iter().any(|block|matches!(block,crate::message::ContentBlock::ToolUse{id,..} if id==&receipt.tool_use_id))
        })).is_some_and(|index|{
            for message in session.messages.iter().skip(index+1) {
                if message.role==crate::message::Role::Assistant && message.content.iter().any(|block|matches!(block,crate::message::ContentBlock::ToolUse{id,..} if id==&receipt.tool_use_id)){return false;}
                if message.role==crate::message::Role::User && message.content.iter().any(|block|matches!(block,crate::message::ContentBlock::ToolResult{tool_use_id,..} if tool_use_id==&receipt.tool_use_id)){return true;}
            }
            false
        });
        if !paired {
            let source = store.clone();
            let id = receipt.run_id.clone();
            let record = tokio::task::spawn_blocking(move || {
                source
                    .inspect(&id)?
                    .context("Provider acquisition record is unavailable")
            })
            .await??;
            let location = record
                .output_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no output path was durably published".into());
            let text = format!(
                "[Recovered provider-result receipt]\nAn SDK result for tool ID {:?} was received but was not confirmed in this conversation. Its acquisition record is {}. Captured output: {} ({} committed bytes). The SDK may have performed effects. No operation was repeated, and missing tool input or completion was not invented.",
                receipt.tool_use_id, record.id, location, record.output_bytes
            );
            session.add_message(
                crate::message::Role::User,
                vec![crate::message::ContentBlock::Text {
                    text,
                    cache_control: None,
                }],
            );
            recovered += 1;
        }
        session.provider_receipt_watermark = receipt.sequence;
        session.provider_receipt_namespace = namespace.clone();
    }
    Ok(recovered)
}

#[cfg(test)]
#[path = "provider_ingress_tests.rs"]
mod tests;
