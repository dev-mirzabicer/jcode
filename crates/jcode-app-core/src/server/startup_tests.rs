#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::runtime::ServerRuntime;
use super::socket::wait_for_existing_server;
use super::{Client, Server, is_server_ready};
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::transport::Listener;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
#[path = "runtime_exit_tests.rs"]
mod forced_exit;

struct TestProvider;

#[test]
#[cfg(unix)]
fn stopped_runtime_and_changed_socket_paths_are_not_silently_recreated_or_removed() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let socket = sandbox.root().join("stopped.sock");
        let debug = sandbox.root().join("stopped-debug.sock");
        let store = crate::runtime_lifecycle::RuntimeStopStore::new(
            &crate::storage::durable_state_dir(),
            &socket,
        )?;
        let owner = store.claim()?;
        use crate::workspace::{RequestId, runtime::*};
        let review = owner.review(
            ShutdownOptions {
                strategy: StopStrategy::Interrupt,
                independent: IndependentTasks::Stop,
                quiescence_timeout_seconds: 1,
            },
            Vec::new(),
        )?;
        let operation = owner.begin(RequestId::new(), review.id, Vec::new())?;
        owner.complete(operation.id, operation.revision)?;
        drop(owner);
        let server = Server::new_with_paths(Arc::new(TestProvider), socket.clone(), debug.clone());
        assert!(server.run().await.is_err());
        assert!(!socket.exists() && !debug.exists());
        let owned = sandbox.root().join("owned.sock");
        let listener = Listener::bind(&owned)?;
        let witness = std::fs::symlink_metadata(&owned)?;
        std::fs::rename(&owned, sandbox.root().join("retained.sock"))?;
        std::fs::write(&owned, "replacement belongs to another operation")?;
        assert!(super::cleanup_bound_sockets(&[(owned.clone(), witness)]).is_err());
        assert_eq!(
            std::fs::read_to_string(&owned)?,
            "replacement belongs to another operation"
        );
        drop(listener);
        Ok(())
    })
}

#[test]
#[cfg(unix)]
fn reviewed_runtime_stop_exits_actual_server_without_a_provisional_session() -> Result<()> {
    use crate::protocol::{Request, ServerEvent};
    use crate::workspace::{RequestId, runtime::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let socket = sandbox.root().join("main.sock");
        let debug = sandbox.root().join("debug.sock");
        let server = Arc::new(Server::new_with_paths(
            Arc::new(TestProvider),
            socket.clone(),
            debug.clone(),
        ));
        let serving = server.clone();
        let task = tokio::spawn(async move { serving.run().await });
        assert!(wait_for_existing_server(&socket, Duration::from_secs(10)).await);
        let client = crate::transport::Stream::connect(&socket).await?;
        let (read, mut write) = client.into_split();
        let mut read = BufReader::new(read);
        async fn exchange(
            read: &mut BufReader<crate::transport::ReadHalf>,
            write: &mut crate::transport::WriteHalf,
            request: Request,
        ) -> Result<ServerEvent> {
            write
                .write_all((serde_json::to_string(&request)? + "\n").as_bytes())
                .await?;
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(15), read.read_line(&mut line)).await??;
            Ok(serde_json::from_str(&line)?)
        }
        assert!(matches!(
            exchange(&mut read, &mut write, Request::RuntimeProbe { id: 1 }).await?,
            ServerEvent::RuntimeCapabilities {
                id: 1,
                version: Some(1)
            }
        ));
        let event = exchange(
            &mut read,
            &mut write,
            Request::RuntimeControl {
                id: 2,
                request: Box::new(RuntimeRequest::Review {
                    options: ShutdownOptions {
                        strategy: StopStrategy::FinishCurrent,
                        independent: IndependentTasks::Stop,
                        quiescence_timeout_seconds: 5,
                    },
                }),
            },
        )
        .await?;
        let ServerEvent::RuntimeResponse { id: 2, response } = event else {
            anyhow::bail!("Unexpected review: {event:?}");
        };
        let RuntimeResponse::Review(review) = *response else {
            anyhow::bail!("Review rejected: {response:?}");
        };
        let request = RequestId::new();
        write
            .write_all(
                (serde_json::to_string(&Request::RuntimeControl {
                    id: 3,
                    request: Box::new(RuntimeRequest::Begin {
                        request,
                        review: review.id,
                    }),
                })? + "\n")
                    .as_bytes(),
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(20), task).await???;
        assert!(server.sessions.read().await.is_empty());
        assert!(!socket.exists() && !debug.exists());
        let store = crate::runtime_lifecycle::RuntimeStopStore::new(
            &crate::storage::durable_state_dir(),
            &socket,
        )?;
        let status = store.status()?;
        assert!(status.desired_stopped);
        let stopped = status.operation.unwrap();
        assert_eq!(stopped.request, request);
        assert_eq!(stopped.phase, ShutdownPhase::Stopped);
        assert!(store.require_automatic_start().is_err());
        drop(server);
        store.authorize_start()?;
        store.require_automatic_start()?;
        Ok(())
    })
}

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "test provider complete should not be called in startup tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

#[tokio::test]
async fn server_run_refuses_to_replace_live_socket() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
    crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());
    let socket_path = temp.path().join("jcode.sock");
    let debug_socket_path = temp.path().join("jcode-debug.sock");
    let _listener = Listener::bind(&socket_path).expect("bind existing live socket");
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let server = Server::new_with_paths(provider, socket_path, debug_socket_path);

    let error = server
        .run()
        .await
        .expect_err("should refuse live socket takeover");
    assert!(
        error
            .to_string()
            .contains("Refusing to replace active server socket"),
        "unexpected error: {error:#}"
    );

    if let Some(prev_runtime) = prev_runtime {
        crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
    } else {
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }
}

#[tokio::test]
async fn is_server_ready_returns_false_immediately_for_missing_socket() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("missing.sock");

    let ready = tokio::time::timeout(Duration::from_millis(50), is_server_ready(&socket_path))
        .await
        .expect("missing socket probe should return quickly");

    assert!(!ready, "missing socket should not report ready");
}

#[tokio::test]
async fn wait_for_existing_server_tolerates_delayed_listener() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("jcode.sock");
    let bind_path = socket_path.clone();

    let bind_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let listener = Listener::bind(&bind_path).expect("bind delayed listener");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(listener);
    });

    let ready = wait_for_existing_server(&socket_path, Duration::from_secs(1)).await;
    assert!(ready, "delayed live listener should be detected");

    bind_task.await.expect("bind task should complete");
}

#[test]
fn server_initializes_schedule_runner_even_when_ambient_disabled() {
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let server = Server::new(provider);

    assert!(
        server.ambient_runner.is_some(),
        "schedule/session tasks need the runner even when ambient is disabled"
    );
}

#[tokio::test]
async fn debug_accept_loop_responds_to_ping_without_affecting_client_count() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("jcode.sock");
    let debug_socket_path = temp.path().join("jcode-debug.sock");
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let server = Server::new_with_paths(provider, socket_path, debug_socket_path.clone());
    let runtime = ServerRuntime::from_server(&server);
    let debug_listener = Listener::bind(&debug_socket_path).expect("bind debug socket");
    let debug_handle = runtime.spawn_debug_accept_loop(debug_listener, std::time::Instant::now());

    let mut client = tokio::time::timeout(
        Duration::from_secs(1),
        Client::connect_debug_with_path(debug_socket_path),
    )
    .await
    .expect("debug connect should complete")
    .expect("debug client should connect");

    assert!(client.ping().await.expect("debug ping should succeed"));
    assert_eq!(*server.client_count.read().await, 0);

    tokio::time::timeout(Duration::from_secs(1), runtime.shutdown())
        .await
        .expect("runtime shutdown should join debug connection tasks");
    tokio::time::timeout(Duration::from_secs(1), debug_handle)
        .await
        .expect("debug accept loop should observe runtime cancellation")
        .expect("debug accept loop should exit cleanly");
}
