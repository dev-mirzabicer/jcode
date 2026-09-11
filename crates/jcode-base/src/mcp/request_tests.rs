use super::*;

fn handle_fixture() -> (McpHandle, mpsc::Receiver<String>) {
    let (writer_tx, receiver) = mpsc::channel(32);
    (
        McpHandle {
            name: "fixture".into(),
            request_id: Arc::new(AtomicU64::new(1)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            writer_tx,
            server_info: Default::default(),
            capabilities: Default::default(),
            tools: Default::default(),
            closed: Arc::new(AtomicBool::new(false)),
        },
        receiver,
    )
}

#[tokio::test]
async fn failed_send_and_protocol_timeout_leave_no_pending_entries() -> Result<()> {
    let (handle, messages) = handle_fixture();
    drop(messages);
    assert!(handle.request("initialize", None).await.is_err());
    assert!(handle.pending.lock().unwrap().is_empty());
    let (handle, mut messages) = handle_fixture();
    assert!(
        handle
            .request_with_timeout(
                "initialize",
                None,
                Some(std::time::Duration::from_millis(5))
            )
            .await
            .is_err()
    );
    assert!(handle.pending.lock().unwrap().is_empty());
    let request: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    let cancelled: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    assert_eq!(request["id"], cancelled["params"]["requestId"]);
    handle.closed.store(true, Ordering::SeqCst);
    assert!(handle.request("tools/list", None).await.is_err());
    assert!(messages.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn dropping_request_removes_only_its_pending_entry_and_sends_cancel() -> Result<()> {
    let (handle, mut messages) = handle_fixture();
    let caller = handle.clone();
    let task = tokio::spawn(async move {
        caller
            .request("tools/call", Some(serde_json::json!({"name":"fixture"})))
            .await
    });
    let request: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    task.abort();
    let _ = task.await;
    assert!(
        handle.pending.lock().unwrap().is_empty(),
        "a dropped consumer must not leak a pending request"
    );
    let notice: Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), messages.recv())
            .await?
            .unwrap(),
    )?;
    assert_eq!(notice["method"], "notifications/cancelled");
    assert_eq!(notice["params"]["requestId"], request["id"]);
    Ok(())
}

#[tokio::test]
async fn cancelled_request_cannot_consume_another_requests_reply() -> Result<()> {
    let (handle, mut messages) = handle_fixture();
    let first = handle.clone();
    let first = tokio::spawn(async move { first.request("tools/call", None).await });
    let request: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    let first_id = request["id"].as_u64().unwrap();
    let second = handle.clone();
    let second = tokio::spawn(async move { second.request("tools/call", None).await });
    let request: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    let second_id = request["id"].as_u64().unwrap();
    first.abort();
    let _ = first.await;
    {
        let mut pending = handle.pending.lock().unwrap();
        assert!(!pending.contains_key(&first_id));
        let reply = pending.remove(&second_id).unwrap();
        reply
            .send(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(second_id),
                result: Some(serde_json::json!({"ok":true})),
                error: None,
                extra: Default::default(),
            })
            .unwrap();
    }
    assert_eq!(second.await??.result.unwrap()["ok"], true);
    assert!(handle.pending.lock().unwrap().is_empty());
    Ok(())
}

fn server_config() -> McpServerConfig {
    let script = r#"
import base64, json, os, struct, sys, zlib
def chunk(kind,data):
    return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,6,0,0,0))+chunk(b'IDAT',zlib.compress(b'\x00\xff\x00\x00\xff'))+chunk(b'IEND',b'')
def send(value):
    print(json.dumps(value),flush=True)
for line in sys.stdin:
    request=json.loads(line); method=request.get('method'); ident=request.get('id')
    if method=='shutdown': break
    if method=='notifications/cancelled':
        cancelled=request['params']['requestId']
        marker=os.environ.get('FIXTURE_MARKER')
        if marker:
            with open(marker+'.cancelled','w') as f: f.write(str(cancelled))
        send({'jsonrpc':'2.0','id':cancelled,'result':{'content':[{'type':'text','text':'late response'}]}})
        continue
    if ident is None: continue
    if method=='initialize':
        result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'fixture','version':'1'}}
    elif method=='tools/list':
        result={'tools':[{'name':'inspect','description':'Synthetic transport fixture','inputSchema':{'type':'object','properties':{'mode':{'type':'string'}},'additionalProperties':False}}]}
    else:
        mode=request.get('params',{}).get('arguments',{}).get('mode','echo')
        if mode=='hold':
            marker=os.environ.get('FIXTURE_MARKER')
            if marker:
                with open(marker,'w') as f: f.write(str(ident))
            continue
        if mode=='missing_result':
            send({'jsonrpc':'2.0','id':ident,'vendor':'MISSING_RESULT'})
            continue
        if mode=='rpc_error':
            send({'jsonrpc':'2.0','id':ident,'error':{'code':-32001,'message':'synthetic failure','data':{'sentinel':'RPC_DATA'},'vendor':'ERROR_EXTRA'},'vendor':'RESPONSE_EXTRA'})
            continue
        if mode=='malformed':
            result={'content':[{'type':'future-content','payload':'DECODE_SENTINEL'}],'structuredContent':{'preserve':True}}
        elif mode in ('rich','rich_error'):
            result={'content':[{'type':'text','text':'\u03b1'*25000+'TAIL'},{'type':'image','mimeType':'image/png','data':base64.b64encode(png).decode()},{'type':'resource','resource':{'uri':'fixture://binary','mimeType':'application/octet-stream','blob':base64.b64encode(b'RAW_RESOURCE').decode()}}],'structuredContent':{'sentinel':'STRUCTURED'},'isError':mode=='rich_error','vendor':'RESULT_EXTRA'}
        else: result={'content':[{'type':'text','text':'still alive'}]}
    send({'jsonrpc':'2.0','id':ident,'result':result})
"#;
    serde_json::from_value(
        serde_json::json!({"command":"python3","args":["-u","-c",script],"shared":false}),
    )
    .unwrap()
}

#[tokio::test]
async fn tool_call_does_not_inherit_the_thirty_second_handshake_deadline() -> Result<()> {
    let (handle, mut messages) = handle_fixture();
    let caller = handle.clone();
    let task =
        tokio::spawn(async move { caller.call_tool("fixture", serde_json::json!({})).await });
    let request: Value = serde_json::from_str(&messages.recv().await.unwrap())?;
    let id = request["id"].as_u64().unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(31)).await;
    assert!(
        !task.is_finished(),
        "a tool-owned deadline must not be replaced by the protocol handshake timeout"
    );
    handle
        .pending
        .lock()
        .unwrap()
        .remove(&id)
        .unwrap()
        .send(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: Some(serde_json::json!({"content":[{"type":"text","text":"complete"}]})),
            error: None,
            extra: Default::default(),
        })
        .unwrap();
    assert_eq!(task.await??.raw["content"][0]["text"], "complete");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn real_stdio_cancellation_preserves_shared_server_and_ignores_late_reply() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("held");
    let mut config = server_config();
    config
        .env
        .insert("FIXTURE_MARKER".into(), marker.display().to_string());
    let mut client = McpClient::connect("owned-cancel-fixture".into(), &config).await?;
    let handle = client.handle();
    let request = tokio::spawn(async move {
        handle
            .call_tool("inspect", serde_json::json!({"mode":"hold"}))
            .await
    });
    let observed = async {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() {
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "fixture did not receive request"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(std::fs::read_to_string(&marker)?)
    }
    .await;
    request.abort();
    let _ = request.await;
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.call_tool("inspect", serde_json::json!({"mode":"echo"})),
    )
    .await;
    let running = client.is_running();
    client.shutdown().await;
    let ident = observed?;
    let reply = reply??;
    assert!(
        running,
        "one cancelled request must not kill the shared server"
    );
    assert_eq!(reply.raw["content"][0]["text"], "still alive");
    assert_eq!(
        std::fs::read_to_string(marker.with_file_name("held.cancelled"))?,
        ident
    );
    assert!(client.handle.pending.lock().unwrap().is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn real_proxy_preserves_rich_resources_structured_errors_and_decode_failures() -> Result<()> {
    use crate::execution::{ExecutionStore, Invocation, PreparedInvocation, RunState};
    use jcode_tool_core::{ToolContext, ToolExecutionMode};
    let config = super::super::McpConfig {
        servers: HashMap::from([("fixture".into(), server_config())]),
    };
    let manager = Arc::new(tokio::sync::RwLock::new(
        super::super::McpManager::with_config(config),
    ));
    manager.read().await.connect_all().await?;
    let mut tools = super::super::create_mcp_tools(manager.clone()).await;
    let (_, tool) = tools.pop().context("Missing real MCP proxy")?;
    let results = async {
        let mut results = Vec::new();
        for mode in [
            "rich",
            "rich_error",
            "rpc_error",
            "malformed",
            "missing_result",
        ] {
            let ctx = ToolContext {
                session_id: "mcp-retention".into(),
                message_id: "message".into(),
                tool_call_id: mode.into(),
                working_dir: None,
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: ToolExecutionMode::Direct,
                invocation: Default::default(),
            };
            results.push((
                mode,
                tool.execute(serde_json::json!({"mode":mode}), ctx).await?,
            ));
        }
        Ok::<_, anyhow::Error>(results)
    }
    .await;
    manager.write().await.disconnect_all().await;
    let dir = tempfile::tempdir()?;
    let store = ExecutionStore::open(dir.path())?;
    for (mode, output) in results? {
        let input = Invocation {
            session_id: "mcp-retention".into(),
            message_id: "message".into(),
            call_path: vec![mode.into()],
            tool: "mcp__fixture__inspect".into(),
            input: serde_json::json!({"mode":mode}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let state = if output.is_error {
            RunState::Failed
        } else {
            RunState::Completed
        };
        let expected_metadata = output.metadata.clone();
        store.retain(record.clone(), output, state)?;
        let saved = store.inspect(&record.id)?.unwrap();
        let restored = store.result(&saved, std::num::NonZeroUsize::new(100).unwrap())?;
        assert_eq!(restored.metadata, expected_metadata);
        assert_eq!(restored.is_error, mode != "rich");
        if mode.starts_with("rich") {
            let text = std::fs::read_to_string(saved.output_path.as_ref().unwrap())?;
            assert!(text.contains(&format!("{}TAIL", "α".repeat(25000))));
            assert_eq!(restored.images.len(), 1);
            assert_eq!(restored.resources.len(), 1);
            assert_eq!(
                std::fs::read(saved.output_path.unwrap().with_file_name("resource-0.bin"))?,
                b"RAW_RESOURCE"
            );
            assert_eq!(
                restored.metadata.unwrap()["mcp_result"]["structuredContent"]["sentinel"],
                "STRUCTURED"
            );
        } else {
            let raw = &restored.metadata.as_ref().unwrap()["mcp_response"];
            if mode == "rpc_error" {
                assert_eq!(raw["error"]["data"]["sentinel"], "RPC_DATA");
                assert_eq!(raw["vendor"], "RESPONSE_EXTRA");
            } else if mode == "missing_result" {
                assert_eq!(raw["vendor"], "MISSING_RESULT");
            } else {
                assert_eq!(raw["content"][0]["payload"], "DECODE_SENTINEL");
            }
        }
    }
    Ok(())
}
