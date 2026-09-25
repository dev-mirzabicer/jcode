#![cfg(unix)]
use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use clap::Parser;
use std::sync::Arc;

struct NoInference;
#[async_trait::async_trait]
impl Provider for NoInference {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("Runtime controls must not call a model")
    }
    fn name(&self) -> &str {
        "test"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}
struct SocketEnv(Option<std::ffi::OsString>);
impl Drop for SocketEnv {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => crate::env::set_var("JCODE_SOCKET", value),
            None => crate::env::remove_var("JCODE_SOCKET"),
        }
    }
}

#[test]
fn runtime_parser_requires_exact_confirmation_and_revision_identities() {
    use super::super::args::{Args, Command};
    let review = crate::workspace::ReviewId::new().to_string();
    let request = RequestId::new().to_string();
    let operation = OperationId::new().to_string();
    assert!(Args::try_parse_from(["jcode", "runtime", "confirm", &review]).is_err());
    assert!(Args::try_parse_from(["jcode", "runtime", "force", &operation]).is_err());
    assert!(
        Args::try_parse_from(["jcode", "runtime", "stop", "--quiescence-seconds", "0"]).is_err()
    );
    assert!(matches!(
        Args::try_parse_from([
            "jcode",
            "runtime",
            "confirm",
            &review,
            "--request",
            &request
        ])
        .unwrap()
        .command,
        Some(Command::Runtime {
            action: RuntimeCommand::Confirm { .. }
        })
    ));
    let args = Args::try_parse_from([
        "jcode",
        "runtime",
        "change",
        &operation,
        "--revision",
        "4",
        "--strategy",
        "interrupt",
        "--tasks",
        "keep-supported",
    ])
    .unwrap();
    assert!(matches!(
        args.command,
        Some(Command::Runtime {
            action: RuntimeCommand::Change {
                revision: 4,
                options: RuntimeStopOptions {
                    strategy: RuntimeStrategy::Interrupt,
                    tasks: RuntimeTasks::KeepSupported,
                    ..
                },
                ..
            }
        })
    ));
}

#[test]
fn runtime_cli_controls_real_server_and_recovers_offline_receipts_without_autostart() -> Result<()>
{
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let _socket_env = SocketEnv(std::env::var_os("JCODE_SOCKET"));
    let socket = sandbox.root().join("cli.sock");
    crate::env::set_var("JCODE_SOCKET", &socket);
    tokio::runtime::Runtime::new()?.block_on(async {
        let absent = status(&sandbox.root().join("missing/ipc.sock")).await?;
        assert!(absent.response.is_none() && !sandbox.root().join("missing").exists());
        let server = Arc::new(crate::server::Server::new_with_paths(
            Arc::new(NoInference),
            socket.clone(),
            sandbox.root().join("cli-debug.sock"),
        ));
        let serving = server.clone();
        let task = tokio::spawn(async move { serving.run().await });
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if NativeClient::connect(&socket).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let gate = crate::runtime_lifecycle::admission::current_runtime()?.unwrap();
        let held = gate.independent(
            RuntimeWorkKind::Preparation,
            "cli-owned-preparation".into(),
            None,
        )?;
        let report = control(
            &socket,
            RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::FinishCurrent,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 2,
                },
            },
        )
        .await?;
        assert!(report.confirm_request.is_some());
        let Some(RuntimeResponse::Review(review)) = report.response else {
            anyhow::bail!("No review");
        };
        assert!(!local_store(&socket)?.status()?.desired_stopped);
        let request = report.confirm_request.unwrap();
        let report = control(
            &socket,
            RuntimeRequest::Begin {
                request,
                review: review.id,
            },
        )
        .await?;
        let Some(RuntimeResponse::Operation(first)) = report.response else {
            anyhow::bail!("No operation");
        };
        assert_eq!(first.phase, ShutdownPhase::WaitingForCurrent);
        assert!(
            control(
                &socket,
                RuntimeRequest::Force {
                    operation: first.id,
                    expected_revision: first.revision
                }
            )
            .await
            .is_err()
        );
        let report = control(
            &socket,
            RuntimeRequest::CancelWait {
                operation: first.id,
                expected_revision: first.revision,
            },
        )
        .await?;
        assert!(matches!(
            report.response,
            Some(RuntimeResponse::Operation(ShutdownOperation {
                phase: ShutdownPhase::Cancelled,
                ..
            }))
        ));
        assert!(gate.accepts_input());
        assert!(
            super::super::commands::run_server_stop_command(true, true)
                .await
                .is_err()
        );
        assert!(NativeClient::connect(&socket).await.is_ok());
        drop(held);
        let report = control(
            &socket,
            RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 5,
                },
            },
        )
        .await?;
        let Some(RuntimeResponse::Review(review)) = report.response else {
            anyhow::bail!("No final review");
        };
        let request = RequestId::new();
        let report = control(
            &socket,
            RuntimeRequest::Begin {
                request,
                review: review.id,
            },
        )
        .await?;
        let Some(RuntimeResponse::Operation(final_op)) = report.response else {
            anyhow::bail!("No final operation");
        };
        tokio::time::timeout(Duration::from_secs(15), task).await???;
        drop(server);
        let report = control(
            &socket,
            RuntimeRequest::Begin {
                request,
                review: review.id,
            },
        )
        .await?;
        assert!(!report.live_response);
        assert!(matches!(
            report.response,
            Some(RuntimeResponse::Operation(ShutdownOperation {
                phase: ShutdownPhase::Stopped,
                ..
            }))
        ));
        run(
            RuntimeCommand::Wait {
                operation: final_op.id,
                timeout_seconds: 2,
                json: true,
            },
            &ProviderChoice::Auto,
            None,
            None,
        )
        .await?;
        let before = local_store(&socket)?.status()?;
        assert!(
            super::super::dispatch::spawn_server(&ProviderChoice::Auto, None, None)
                .await
                .is_err()
        );
        assert!(!socket.exists());
        assert_eq!(local_store(&socket)?.status()?, before);
        assert_eq!(
            inspect(&socket, final_op.id).await?.coordinator_owned,
            Some(false)
        );
        assert!(!status(&socket).await?.live_response);
        Ok(())
    })
}

#[test]
fn runtime_client_rejects_wrong_namespace_before_sending_mutation() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let socket = sandbox.root().join("wrong.sock");
        let listener = crate::transport::Listener::bind(&socket)?;
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await?;
            assert!(matches!(
                serde_json::from_str::<Request>(&line)?,
                Request::RuntimeProbe { id: 1 }
            ));
            write
                .write_all(
                    (serde_json::to_string(&ServerEvent::RuntimeCapabilities {
                        id: 1,
                        version: Some(1),
                    })? + "\n")
                        .as_bytes(),
                )
                .await?;
            line.clear();
            read.read_line(&mut line).await?;
            let status = RuntimeStatus {
                namespace: "foreign".into(),
                runtime: Some("other-owner".into()),
                reload_in_progress: false,
                desired_stopped: false,
                revision: 0,
                operation: None,
                work: Vec::new(),
            };
            write
                .write_all(
                    (serde_json::to_string(&ServerEvent::RuntimeResponse {
                        id: 2,
                        response: Box::new(RuntimeResponse::Status(status)),
                    })? + "\n")
                        .as_bytes(),
                )
                .await?;
            line.clear();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), read.read_line(&mut line)).await??,
                0
            );
            Ok::<_, anyhow::Error>(())
        });
        assert!(
            control(
                &socket,
                RuntimeRequest::Review {
                    options: ShutdownOptions {
                        strategy: StopStrategy::Interrupt,
                        independent: IndependentTasks::Stop,
                        quiescence_timeout_seconds: 1
                    }
                }
            )
            .await
            .is_err()
        );
        peer.await??;
        Ok(())
    })
}
