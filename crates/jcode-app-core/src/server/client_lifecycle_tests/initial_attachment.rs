use super::*;

#[test]
fn failed_initial_attachment_creates_no_placeholder_or_provider() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    tokio::runtime::Runtime::new()?.block_on(async {
        let root = std::path::PathBuf::from(std::env::var_os("JCODE_HOME").unwrap());
        for mode in ["missing", "corrupt", "owned"] {
            let target = format!("session_attach_{mode}_fixture");
            let foreign = crate::primary::PrimaryHost::default();
            if mode != "missing" {
                let mut session = Session::create_with_id(target.clone(), None, None);
                session.save()?;
                if mode == "corrupt" {
                    std::fs::write(
                        crate::session::session_path(&session.id)?,
                        b"invalid fixture snapshot",
                    )?;
                }
                if mode == "owned" {
                    foreign.own(&target)?;
                }
            }
            let directory = crate::session::session_path(&target)?
                .parent()
                .unwrap()
                .to_path_buf();
            crate::storage::ensure_dir(&directory)?;
            let snapshots = || -> Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
                let mut values = Vec::new();
                for entry in std::fs::read_dir(&directory)? {
                    let path = entry?.path();
                    if path
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        values.push((path.clone(), std::fs::read(path)?));
                    }
                }
                values.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(values)
            };
            let before = snapshots()?;
            let forked = Arc::new(AtomicBool::new(false));
            let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
                forked: forked.clone(),
            });
            let sessions = Arc::new(crate::primary::PrimaryHost::default());
            let (server_stream, client_stream) = crate::transport::stream_pair()?;
            let (debug_response_tx, _) = broadcast::channel(8);
            let (swarm_event_tx, _) = broadcast::channel(8);
            let (global_event_tx, _) = broadcast::channel(8);
            let task = tokio::spawn(handle_client(
                server_stream,
                sessions.clone(),
                global_event_tx,
                provider,
                Arc::new(crate::context::ContextTransactionService::new()),
                crate::server::startup_context::test_coordinator(),
                Arc::new(RwLock::new(false)),
                Arc::new(RwLock::new(String::new())),
                Arc::new(RwLock::new(1)),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                FileTouchService::new(),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(ClientDebugState::default())),
                debug_response_tx,
                Arc::new(RwLock::new(std::collections::VecDeque::new())),
                Arc::new(std::sync::atomic::AtomicU64::new(0)),
                swarm_event_tx,
                "fixture".into(),
                "".into(),
                Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
                Arc::new(RwLock::new(HashMap::new())),
                Arc::new(RwLock::new(HashMap::new())),
                AwaitMembersRuntime::default(),
                SwarmMutationRuntime::default(),
            ));
            let (reader, mut writer) = client_stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut request = subscribe_request(Some(root.to_str().unwrap()));
            if let Request::Subscribe {
                target_session_id,
                startup_context_caller,
                ..
            } = &mut request
            {
                *target_session_id = Some(target);
                *startup_context_caller =
                    Some(crate::protocol::StartupContextPrimaryCaller::HarnessApiAttach);
            }
            writer
                .write_all((serde_json::to_string(&request)? + "\n").as_bytes())
                .await?;
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
            assert!(
                matches!(decode_request_or_event(&line), ServerEvent::Error { .. }),
                "{mode}: {line}"
            );
            drop(writer);
            drop(reader);
            tokio::time::timeout(Duration::from_secs(10), task).await???;
            assert!(!forked.load(Ordering::SeqCst), "{mode}");
            assert!(sessions.read().await.is_empty(), "{mode}");
            assert_eq!(snapshots()?, before, "{mode}");
        }
        Ok(())
    })
}
