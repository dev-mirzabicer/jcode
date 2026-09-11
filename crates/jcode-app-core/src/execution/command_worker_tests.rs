use super::*;
use crate::execution::command_handoff::CommandRequest;
use std::process::Stdio;

#[test]
fn gated_child_fixture() -> Result<()> {
    let Some(id) = std::env::var_os("JCODE_GATED_CHILD_FIXTURE") else {
        return Ok(());
    };
    child_main(id.to_str().context("Invalid fixture identity")?)
}

#[tokio::test]
async fn worker_fixture() -> Result<()> {
    let Some(id) = std::env::var_os("JCODE_WORKER_FIXTURE") else {
        return Ok(());
    };
    let id = id.to_str().context("Invalid fixture identity")?;
    let mut child = tokio::process::Command::new(std::env::current_exe()?);
    child
        .args([
            "--exact",
            "execution::command_worker::tests::gated_child_fixture",
            "--nocapture",
        ])
        .env("JCODE_GATED_CHILD_FIXTURE", id);
    worker_with_child(id, child).await
}

struct WorkerChild {
    child: std::process::Child,
    store: ExecutionStore,
    id: String,
}
impl Drop for WorkerChild {
    fn drop(&mut self) {
        if let Ok(Some(identity)) = self.store.command_process(&self.id) {
            let _ = identity.signal_group(libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn spawn_worker(store: &ExecutionStore, id: &str) -> Result<WorkerChild> {
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "execution::command_worker::tests::worker_fixture",
            "--nocapture",
        ])
        .env("JCODE_WORKER_FIXTURE", id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    Ok(WorkerChild {
        child,
        store: store.clone(),
        id: id.into(),
    })
}

async fn prepare(
    command: String,
    working_dir: PathBuf,
    background: bool,
) -> Result<(ExecutionStore, Arc<runtime::RuntimeHandle>, RunRecord)> {
    let store = ExecutionStore::open(&crate::storage::jcode_dir()?)?;
    let parent = runtime::ensure_running(&store).await?;
    let input = Invocation {
        session_id: uuid::Uuid::new_v4().to_string(),
        message_id: "message".into(),
        call_path: vec!["command".into()],
        tool: "bash".into(),
        input: serde_json::json!({"command":command}),
        working_dir: Some(working_dir.clone()),
        received_result_digest: None,
    };
    let PreparedInvocation::New(record) = store.prepare(&input, &parent.endpoint.id)? else {
        panic!()
    };
    store.start(&record.id, &record.owner)?;
    store.prepare_command(
        &record,
        CommandRequest {
            command,
            working_dir,
            timeout_ms: None,
            background,
            notify: false,
            wake: false,
            title: None,
            storage: StorageConfig::default(),
        },
    )?;
    Ok((store, parent, record))
}
async fn ready(path: &std::path::Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Worker command did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[tokio::test]
async fn background_command_survives_original_runtime_shutdown_without_reexecution() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (store, parent, record) = prepare(
        "printf x >> effects; printf before; printf ready > ready; sleep 1; printf after".into(),
        directory.path().into(),
        true,
    )
    .await?;
    let mut worker = spawn_worker(&store, &record.id)?;
    ready(&directory.path().join("ready")).await?;
    let endpoint = parent.endpoint.clone();
    runtime::end_test_listener(&parent).await;
    drop(parent);
    assert!(!endpoint.has_live_lease()?);
    let ControlReply::Snapshot { record: completed } = tokio::time::timeout(
        Duration::from_secs(10),
        runtime::control_in_store(&store, &record.id, ControlOperation::Wait),
    )
    .await??
    else {
        panic!()
    };
    assert_eq!(completed.state, RunState::Completed);
    let output = std::fs::read_to_string(completed.output_path.as_ref().unwrap())?;
    assert!(output.contains("before") && output.contains("after"));
    assert_eq!(std::fs::read(directory.path().join("effects"))?, b"x");
    assert!(worker.child.wait()?.success());
    let mut duplicate = spawn_worker(&store, &record.id)?;
    assert!(!duplicate.child.wait()?.success());
    assert_eq!(std::fs::read(directory.path().join("effects"))?, b"x");
    Ok(())
}

#[tokio::test]
async fn foreground_command_loses_parent_and_stops_with_partial_output() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (store, parent, record) = prepare(
        "printf partial; printf ready > ready; sleep 20; printf escaped > escaped".into(),
        directory.path().into(),
        false,
    )
    .await?;
    let mut worker = spawn_worker(&store, &record.id)?;
    ready(&directory.path().join("ready")).await?;
    runtime::end_test_listener(&parent).await;
    drop(parent);
    let ControlReply::Snapshot { record: completed } = tokio::time::timeout(
        Duration::from_secs(10),
        runtime::control_in_store(&store, &record.id, ControlOperation::Wait),
    )
    .await??
    else {
        panic!()
    };
    assert_eq!(completed.state, RunState::Interrupted);
    assert!(std::fs::read_to_string(completed.output_path.unwrap())?.contains("partial"));
    assert!(!directory.path().join("escaped").exists());
    assert!(worker.child.wait()?.success());
    Ok(())
}
