//! Human-only target routing. Child conversation ownership never moves to a client.
use crate::{
    agent::Agent,
    context::ContextTransactionService,
    protocol::{Request, ServerEvent},
    session::Session,
};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub async fn handle(
    child_id: String,
    request: Request,
    service: Arc<ContextTransactionService>,
    repositories: crate::instruction::InstructionRepositoryService,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    jcode_tool_types::delegation::ChildSessionId::parse(child_id.clone())
        .map_err(anyhow::Error::msg)?;
    ensure!(
        matches!(
            &request,
            Request::GetContextEditorSnapshot { .. }
                | Request::GetContextMessageDetail { .. }
                | Request::PreviewContextRanges { .. }
                | Request::PreviewContextCuratorPlan { .. }
                | Request::SaveContextCuratorDefault { .. }
                | Request::PrepareContextDraft { .. }
                | Request::CancelContextDraft { .. }
                | Request::GetContextDraftStatus { .. }
                | Request::PreviewContextDraftSelection { .. }
                | Request::ApplyContextDraft { .. }
                | Request::ListContextTransactions { .. }
                | Request::GetContextTransactionDetail { .. }
                | Request::RevertContextTransaction { .. }
                | Request::ReapplyContextTransaction { .. }
        ),
        "Only human Context Editor operations may target a child. Chat, takeover and automatic restart are unavailable."
    );
    match &request {
        Request::CancelContextDraft { id, draft_id } => {
            super::context_control::handle_cancel_context_draft(
                *id,
                draft_id.clone(),
                &child_id,
                &service,
                &event_tx,
            );
            return Ok(());
        }
        Request::GetContextDraftStatus { id, draft_id } => {
            super::context_control::handle_get_context_draft_status(
                *id,
                draft_id.clone(),
                &child_id,
                &service,
                &event_tx,
            );
            return Ok(());
        }
        _ => {}
    }
    ensure!(
        !super::server_reload_starting(),
        "Child context host is reloading"
    );
    let root = crate::storage::jcode_dir()?;
    let source = root.clone();
    let target = child_id.clone();
    let (lease, session) = tokio::task::spawn_blocking(move || -> Result<_> {
        let store = crate::execution::ExecutionStore::open(&source)?;
        let lease = store.idle_child_control(&target)?;
        let session = Session::capture_readonly(&source, &target)?
            .session()
            .clone();
        ensure!(
            session.isolated_child.is_some(),
            "Context target is not an isolated child"
        );
        session.validate_active_agent_profile()?;
        store.touch_activity(&target, chrono::Utc::now().timestamp())?;
        Ok((lease, session))
    })
    .await??;
    let provider = session
        .isolated_child
        .as_ref()
        .context("Missing child identity")?
        .identity
        .resolution
        .restore_provider()
        .map_err(|error| anyhow::anyhow!("Stored child route is unavailable: {error:?}"))?;
    let registry = crate::tool::Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(Agent::from_isolated_session(
        provider,
        registry,
        session,
        repositories,
    )?));
    // No parent Agent mutex is acquired. The existing service captures immutable
    // curator input and checks revision/protected directives at the later commit.
    // Each request reloads current authority, so a parent follow-up cannot leave
    // this client operating on a cached pre-follow-up transcript.
    let handled = super::context_control::dispatch_editor_request(
        request, &child_id, &agent, &service, false, &event_tx,
    );
    drop(lease);
    ensure!(handled, "Unsupported child context operation");
    Ok(())
}

/// Local TUI child controls still execute on the compatible shared child host.
/// No primary session is attached, resumed, created or handed to the client.
pub async fn forward(
    child_id: String,
    request: Request,
    output: mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let id = request.id();
    let stream = super::connect_socket(&super::socket_path())
        .await
        .context("Child host is unavailable; reconnect before editing")?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    async fn exchange(
        reader: &mut BufReader<crate::transport::ReadHalf>,
        write: &mut crate::transport::WriteHalf,
        request: Request,
    ) -> Result<ServerEvent> {
        write
            .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
            .await?;
        let mut line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await??;
        ensure!(n != 0, "Child context host disconnected during negotiation");
        Ok(serde_json::from_str(&line)?)
    }
    let probe = exchange(&mut reader, &mut write, Request::DelegationProbe { id }).await?;
    let ServerEvent::DelegationCapabilities {
        version: 1,
        namespace,
        ..
    } = probe
    else {
        anyhow::bail!("Host does not support isolated children")
    };
    ensure!(
        std::path::Path::new(&namespace) == crate::storage::jcode_dir()?.canonicalize()?,
        "Child context host is in a different namespace"
    );
    ensure!(
        matches!(
            exchange(&mut reader, &mut write, Request::TaskMonitorProbe { id }).await?,
            ServerEvent::TaskMonitorCapabilities {
                version: 1,
                child_context: true,
                ..
            }
        ),
        "Host does not support targeted child context editing"
    );
    write
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&Request::ChildContext {
                    id,
                    child_id: child_id.clone(),
                    request: Box::new(request)
                })?
            )
            .as_bytes(),
        )
        .await?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let event: ServerEvent = serde_json::from_str(&line)?;
        ensure!(
            matches!(&event,ServerEvent::ChildContextResponse{id:received,child_id:target,..} if *received==id && *target==child_id),
            "Child context reply changed target or request identity"
        );
        if output.send(event).is_err() {
            break;
        }
    }
    Ok(())
}
