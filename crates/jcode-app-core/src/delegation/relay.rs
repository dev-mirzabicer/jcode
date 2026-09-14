use super::*;
use crate::protocol::{Request, ServerEvent};
use crate::transport::{ReadHalf, WriteHalf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const HOST_PROTOCOL: u32 = 1;

pub(crate) async fn forward(tool: &str, input: Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = crate::server::socket_path();
    let stream = match crate::server::connect_socket(&path).await {
        Ok(stream) => stream,
        Err(first) => {
            crate::server_spawn::spawn_delegation_server()
                .await
                .with_context(|| {
                    format!("Delegation host unavailable at {}: {first}", path.display())
                })?;
            crate::server::connect_socket(&path)
                .await
                .context("Connect compatible delegation host after startup")?
        }
    };
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&Request::DelegationProbe { id: 1 })?
            )
            .as_bytes(),
        )
        .await?;
    let probe = tokio::time::timeout(std::time::Duration::from_secs(10), next_event(&mut reader))
        .await
        .context("Delegation host capability negotiation timed out before execution")??;
    let ServerEvent::DelegationCapabilities {
        id: 1,
        version: HOST_PROTOCOL,
        namespace,
    } = probe
    else {
        anyhow::bail!(
            "Shared host does not advertise compatible isolated delegation. Upgrade the host before retrying; no child was requested."
        );
    };
    ensure!(
        Path::new(&namespace) == crate::storage::jcode_dir()?.canonicalize()?,
        "Delegation host belongs to a different Jcode namespace"
    );
    check_stop(ctx)?;
    let mut call_path = ctx.invocation.ancestors.clone();
    call_path.push(ctx.tool_call_id.clone());
    let request = Request::DelegationExecute {
        id: 2,
        invocation: Box::new(HostedDelegationInvocation {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            call_path,
            working_dir: ctx.working_dir.clone(),
            tool: tool.into(),
            input,
        }),
    };
    writer
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;
    let response = async {
        match next_event(&mut reader).await? {
            ServerEvent::DelegationResult { id: 2, output } => Ok(output),
            ServerEvent::Error { message, .. } => Err(anyhow::anyhow!(message)),
            _ => anyhow::bail!(
                "Unexpected delegation response. Retrieve the original scoped run instead of repeating uncertain work."
            ),
        }
    };
    if let Some(stop) = &ctx.graceful_shutdown_signal {
        tokio::select! {
            result = response => result,
            _ = stop.notified() => anyhow::bail!("Delegation wait interrupted. The host retains the original invocation and its result."),
        }
    } else {
        response.await
    }
}

async fn next_event(reader: &mut BufReader<ReadHalf>) -> Result<ServerEvent> {
    let mut line = String::new();
    ensure!(
        reader.read_line(&mut line).await? != 0,
        "Delegation host disconnected; original invocation may remain inspectable"
    );
    serde_json::from_str(&line).context("Decode delegation host response")
}

pub(crate) async fn serve_request(
    reader: &mut BufReader<ReadHalf>,
    writer: Arc<Mutex<WriteHalf>>,
    id: u64,
    invocation: HostedDelegationInvocation,
    host: Arc<Host>,
) -> Result<()> {
    ensure!(
        matches!(invocation.tool.as_str(), "subagent" | "get_catalog"),
        "Unsupported hosted delegation operation"
    );
    let mut call_path = invocation.call_path;
    let tool_call_id = call_path
        .pop()
        .context("Hosted invocation has no complete call identity")?;
    let registry = Registry::new(host.provider.clone()).await;
    for tool in crate::tool::subagent::DelegationTool::hosted(host) {
        registry.register(tool.name().into(), Arc::new(tool)).await;
    }
    let ctx = ToolContext {
        session_id: invocation.session_id,
        message_id: invocation.message_id,
        tool_call_id,
        working_dir: invocation.working_dir,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::AgentTurn,
        invocation: jcode_tool_core::InvocationContext {
            ancestors: call_path,
            ..Default::default()
        },
    };
    let operation = registry.execute(&invocation.tool, invocation.input, ctx);
    tokio::pin!(operation);
    let mut disconnect = String::new();
    let result = tokio::select! {
        result = &mut operation => result,
        _ = reader.read_line(&mut disconnect) => return Ok(()),
    };
    let output = match result {
        Ok(output) => output,
        Err(error) => {
            if let Some(captured) = error.downcast_ref::<crate::execution::CapturedToolError>() {
                captured.output.clone().with_error(true)
            } else {
                ToolOutput::new(format!("Delegation failed: {error:#}")).with_error(true)
            }
        }
    };
    let bytes = format!(
        "{}\n",
        serde_json::to_string(&ServerEvent::DelegationResult { id, output })?
    );
    writer.lock().await.write_all(bytes.as_bytes()).await?;
    Ok(())
}
