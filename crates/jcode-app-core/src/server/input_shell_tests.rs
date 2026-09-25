use super::*;
use crate::workspace::{RequestId, runtime::*};
use std::time::Duration;

struct NoInference;
#[async_trait::async_trait]
impl crate::provider::Provider for NoInference {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        anyhow::bail!("InputShell must not infer")
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn crate::provider::Provider> {
        Arc::new(Self)
    }
}
async fn agent(work: &std::path::Path) -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(NoInference);
    let mut agent = Agent::new(provider.clone(), crate::tool::Registry::new(provider).await);
    agent.set_working_dir(work.to_str().unwrap());
    Arc::new(Mutex::new(agent))
}
async fn cleanup(root: &std::path::Path) -> Result<()> {
    let store = execution::ExecutionStore::open(root)?;
    let mut reports = Vec::new();
    for record in store.list_all(None, 100)? {
        let result = settle(&store, &record).await;
        reports.push(serde_json::json!({"run":record.id,"error":result.err().map(|error|format!("{error:#}"))}));
    }
    println!(
        "WP08_INPUT_SHELL_CLEANUP={}",
        serde_json::to_string(&reports)?
    );
    ensure!(
        reports.iter().all(|row| row["error"].is_null()),
        "Fixture cleanup incomplete: {reports:?}"
    );
    Ok(())
}
async fn settle(store: &execution::ExecutionStore, record: &execution::RunRecord) -> Result<()> {
    if !record.state.terminal() {
        execution::control_transport::control_in_store(
            store,
            &record.id,
            execution::control_transport::ControlOperation::Stop {
                cause: jcode_tool_types::StopCause::HumanCancellation,
            },
        )
        .await?;
        execution::control_transport::control_in_store(
            store,
            &record.id,
            execution::control_transport::ControlOperation::Wait,
        )
        .await?;
    }
    ensure!(
        store
            .inspect(&record.id)?
            .is_some_and(|row| row.state.terminal()),
        "Missing terminal fixture receipt"
    );
    if let Some(owner) = store
        .command_ownership(&record.id)?
        .and_then(|owner| owner.worker)
    {
        let endpoint = store
            .runtime_endpoint(&owner)?
            .context("missing fixture worker")?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while endpoint.has_live_lease()? {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
    }
    Ok(())
}
fn with_cleanup(result: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => anyhow::bail!("{error:#}; cleanup also failed: {cleanup:#}"),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}
#[test]
fn native_input_shell_preserves_grouped_display_and_complete_raw_output() -> Result<()> {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let work = tempfile::tempdir()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let result = async {
            for (command, expected, code) in [
                (
                    "printf out; printf err >&2; exit 7",
                    "out\n[stderr]\nerr",
                    7,
                ),
                ("true", "", 0),
                ("printf err >&2", "[stderr]\nerr", 0),
            ] {
                let result = execute(
                    "input-shell-fixture".into(),
                    command.into(),
                    Some(work.path().into()),
                )
                .await?;
                ensure!(
                    // The test-only gated child is a libtest subprocess. Its header
                    // is acquired too; production must never strip arbitrary output.
                    result.output == format!("\nrunning 1 test\n{expected}"),
                    "Grouped display differs: {:?}",
                    result.output
                );
                ensure!(
                    result.code == Some(code),
                    "Exit code differs: {:?}",
                    result.code
                );
                ensure!(
                    !result.failed_to_start && !result.truncated,
                    "Command failed or display truncated"
                );
            }
            let result = execute(
                "input-shell-fixture".into(),
                "printf x >> effects; head -c 50000 /dev/zero | tr '\\0' a; printf RAW_TAIL".into(),
                Some(work.path().into()),
            )
            .await?;
            ensure!(result.truncated, "Large display was not bounded");
            ensure!(
                !result.output.contains("RAW_TAIL"),
                "Display limit did not apply"
            );
            ensure!(
                std::fs::read(work.path().join("effects"))? == b"x",
                "Effect repeated"
            );
            let store = execution::ExecutionStore::open(home.root())?;
            let rows = store.list_all(None, 100)?;
            ensure!(rows.len() == 4, "Wrong execution count");
            let mut complete = false;
            for row in rows {
                ensure!(row.state.terminal(), "Command is not terminal");
                if let Ok(part) = store.read_part_page(&row, "stdout.bin", 0, 100_000, None) {
                    complete |= base64::engine::general_purpose::STANDARD
                        .decode(part.data_base64)?
                        .ends_with(b"RAW_TAIL");
                }
            }
            ensure!(complete, "Complete raw output missing");
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let settled = cleanup(home.root()).await;
        with_cleanup(result, settled)
    })
}
#[test]
fn input_shell_runtime_stop_reaches_actual_native_work() -> Result<()> {
    lifecycle_case(IndependentTasks::Stop)
}
#[test]
fn input_shell_keep_returns_same_run_without_repeating_effects() -> Result<()> {
    lifecycle_case(IndependentTasks::KeepSupported)
}
fn lifecycle_case(independent: IndependentTasks) -> Result<()> {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let work = tempfile::tempdir()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let source = agent(work.path()).await;
        let session = source.lock().await.session_id().to_owned();
        let host = Arc::new(crate::primary::PrimaryHost::new(std::collections::HashMap::from([(session, source.clone())])));
        let lifecycle = super::super::shutdown::RuntimeLifecycle::new(home.root(), &home.root().join("input-shell.sock"), host, crate::background::global().clone()).await?;
        let result = async {
            let (events, mut replies) = tokio::sync::mpsc::unbounded_channel();
            handle(17, "printf x >> effects; printf before; printf ready > ready; for i in $(seq 1 600); do [ -f release ] && break; sleep .05; done; printf after".into(), &source, &events.into());
            tokio::time::timeout(Duration::from_secs(15), async {
                while !work.path().join("ready").exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
            }).await?;
            let RuntimeResponse::Review(review) = lifecycle.request(RuntimeRequest::Review { options: ShutdownOptions { strategy: StopStrategy::Interrupt, independent, quiescence_timeout_seconds: 10 } }).await? else { anyhow::bail!("Unexpected runtime reply") };
            let mut stopped = lifecycle.stopped();
            lifecycle.request(RuntimeRequest::Begin { request: RequestId::new(), review: review.id }).await?;
            let settled = tokio::time::timeout(Duration::from_secs(15), async {
                while stopped.borrow_and_update().is_none() { stopped.changed().await?; }
                Ok::<_, anyhow::Error>(())
            }).await;
            if let Err(error) = settled { anyhow::bail!("Stopped signal: {error}; state: {:?}", lifecycle.request(RuntimeRequest::Status {}).await?); }
            settled??;
            let Some(ServerEvent::InputShellResult { result }) = replies.recv().await else { anyhow::bail!("Unexpected runtime reply") };
            ensure!(!result.failed_to_start, "Command was not started: {}", result.output);
            ensure!(matches!(replies.recv().await, Some(ServerEvent::Done { id: 17 })), "Missing completion");
            let store = execution::ExecutionStore::open(home.root())?;
            let row = store.list_all(None, 100)?.into_iter().find(|row| row.tool == "input_shell").context("missing human command")?;
            if independent == IndependentTasks::KeepSupported {
                ensure!(!row.state.terminal(), "Preserved command was stopped");
                ensure!(row.background, "Missing durable promotion");
                ensure!(result.exit_code.is_none(), "False terminal status");
                ensure!(result.output.contains(&row.id), "Missing run identity");
                std::fs::write(work.path().join("release"), b"go")?;
                execution::control_transport::control_in_store(&store, &row.id, execution::control_transport::ControlOperation::Wait).await?;
                let row = store.inspect(&row.id)?.unwrap();
                let part = store.read_part_page(&row, "stdout.bin", 0, 1024, None)?;
                ensure!(base64::engine::general_purpose::STANDARD.decode(part.data_base64)? == b"\nrunning 1 test\nbeforeafter", "Survivor output differs");
            } else {
                ensure!(row.state.terminal(), "Command is not terminal");
                ensure!(row.stop_cause.is_some(), "Missing cancellation cause");
                ensure!(result.output.contains("before"), "Partial output missing: {}", result.output);
            }
            ensure!(std::fs::read(work.path().join("effects"))? == b"x", "Effect repeated");
            // New input commands are rejected before any effect after Stop.
            let (events, mut denied) = tokio::sync::mpsc::unbounded_channel();
            handle(18, "printf denied > should-not-exist".into(), &source, &events.into());
            ensure!(matches!(denied.recv().await, Some(ServerEvent::InputShellResult { result }) if result.failed_to_start), "New command was not denied");
            ensure!(!work.path().join("should-not-exist").exists(), "Denied effect occurred");
            Ok::<_, anyhow::Error>(())
        }.await;
        let settled = cleanup(home.root()).await;
        with_cleanup(result, settled)
    })
}
