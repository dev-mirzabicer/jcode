use super::*;
use crate::execution::{Capture, Invocation, PreparedInvocation, RuntimeEndpoint, StorageConfig};
use jcode_tool_core::{OutputCapture, OutputStream};
use jcode_tool_types::{OutputSource, ToolOutput};
use std::path::Path;

fn fixture(root: &Path) -> Result<(ExecutionStore, RuntimeEndpoint, RunRecord)> {
    let store = ExecutionStore::open(root)?;
    let lease = store.root().join("fixture-runtime.lease");
    std::fs::File::create(&lease)?;
    let runtime = RuntimeEndpoint::new(
        uuid::Uuid::new_v4().simple().to_string(),
        root.join("fixture.sock"),
        lease,
        "k".repeat(64),
    );
    store.register_runtime(&runtime)?;
    let invocation = Invocation {
        session_id: "owner-loss".into(),
        message_id: "message".into(),
        call_path: vec!["call".into()],
        tool: "fixture".into(),
        input: serde_json::json!({"original":"input"}),
        working_dir: Some(root.into()),
        received_result_digest: None,
    };
    let PreparedInvocation::New(record) = store.prepare(&invocation, &runtime.id)? else {
        anyhow::bail!("Unexpected existing fixture");
    };
    store.start(&record.id, &runtime.id)?;
    Ok((store, runtime, record))
}
fn replace_image(store: &ExecutionStore, runtime: &RuntimeEndpoint) -> Result<()> {
    store.connection()?.execute(
        "UPDATE runtimes SET process_image='prior-process-image' WHERE id=?1",
        [&runtime.id],
    )?;
    Ok(())
}

#[tokio::test]
async fn unregistered_native_launch_and_legacy_tracking_cannot_claim_quiescence() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (store, runtime, record) = fixture(root.path())?;
    let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
    let ticket = capture.begin_process()?;
    assert!(
        capture
            .seal(ToolOutput::new("not completed"), RunState::Completed)
            .is_err()
    );
    assert!(
        store
            .finish_native_process(&record.id, "another-owner", &ticket)
            .is_err()
    );
    assert!(
        store
            .finish_native_process("another-run", &record.owner, &ticket)
            .is_err()
    );
    replace_image(&store, &runtime)?;
    drop(capture);
    let error = store.recover_lost_owner(&record.id).await.unwrap_err();
    assert!(error.to_string().contains("unproven native launch"));
    assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Running);
    // Explicitly model a failed spawn acknowledgement from its original owner.
    store.finish_native_process(&record.id, &record.owner, &ticket)?;
    store.connection()?.execute(
        "UPDATE runs SET native_tracking=NULL WHERE id=?1",
        [&record.id],
    )?;
    let error = store.recover_lost_owner(&record.id).await.unwrap_err();
    assert!(error.to_string().contains("Legacy execution"));
    assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Running);
    Ok(())
}

#[tokio::test]
async fn unleased_live_image_and_active_capture_are_not_mistaken_for_stopped_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (store, runtime, record) = fixture(root.path())?;
    assert!(store.recover_lost_owner(&record.id).await?.is_none());
    let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
    capture.write(OutputStream::Stdout, b"captured prefix")?;
    replace_image(&store, &runtime)?;
    assert!(store.recover_lost_owner(&record.id).await.is_err());
    assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Running);
    let path = capture.reference()?.path;
    drop(capture);
    let recovered = store.recover_lost_owner(&record.id).await?.unwrap();
    assert_eq!(recovered.state, RunState::Interrupted);
    assert!(!recovered.complete);
    assert_eq!(std::fs::read_to_string(&path)?, "captured prefix");
    let result = store.result(&recovered, std::num::NonZeroUsize::new(20).unwrap())?;
    assert!(result.is_error);
    let OutputSource::Unavailable(reference) = result.source else {
        panic!("Expected an interruption receipt, not invented original output");
    };
    assert_eq!(reference.partial_output.unwrap().path, path);
    let receipt = std::fs::read_to_string(reference.receipt_path)?;
    assert!(receipt.contains("unknown"));
    assert_eq!(
        store.recover_lost_owner(&record.id).await?.unwrap().state,
        RunState::Interrupted
    );
    assert_eq!(store.list("owner-loss", None, 10)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn sealed_witness_restores_its_actual_outcome_without_inventing_interruption() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (store, _runtime, record) = fixture(root.path())?;
    let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
    store.connection()?.execute_batch("CREATE TRIGGER fail_terminal BEFORE UPDATE OF state ON runs WHEN NEW.state='completed' BEGIN SELECT RAISE(FAIL,'injected terminal persistence failure'); END;")?;
    assert!(
        capture
            .seal(
                ToolOutput::new("complete original body"),
                RunState::Completed
            )
            .is_err()
    );
    drop(capture);
    store
        .connection()?
        .execute_batch("DROP TRIGGER fail_terminal;")?;
    let recovered = store.recover_lost_owner(&record.id).await?.unwrap();
    assert_eq!(recovered.state, RunState::Completed);
    assert!(recovered.complete);
    assert_eq!(
        std::fs::read_to_string(recovered.output_path.unwrap())?,
        "complete original body"
    );
    Ok(())
}

#[tokio::test]
async fn recorded_live_command_group_blocks_retirement_without_signalling_it() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (store, runtime, record) = fixture(root.path())?;
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30").process_group(0).kill_on_drop(true);
    let mut child = command.spawn()?;
    let identity = crate::execution::process::ProcessIdentity::capture(child.id().unwrap())?;
    store.connection()?.execute("INSERT INTO command_handoffs(run_id,parent_owner,worker_owner,payload_digest,state,process_identity) VALUES (?1,?2,?2,'fixture','registered',?3)",rusqlite::params![record.id,runtime.id,serde_json::to_string(&identity)?])?;
    replace_image(&store, &runtime)?;
    let result = store.recover_lost_owner(&record.id).await;
    assert!(
        child.try_wait()?.is_none(),
        "Recovery must not signal a guessed process"
    );
    child.kill().await?;
    child.wait().await?;
    assert!(result.unwrap_err().to_string().contains("live members"));
    assert_eq!(store.inspect(&record.id)?.unwrap().state, RunState::Running);
    assert_eq!(
        store.recover_lost_owner(&record.id).await?.unwrap().state,
        RunState::Interrupted
    );
    Ok(())
}

#[test]
fn crash_fixture() -> Result<()> {
    let Some(root) = std::env::var_os("JCODE_OWNER_LOSS_FIXTURE") else {
        return Ok(());
    };
    let root = std::path::PathBuf::from(root);
    let (store, runtime, record) = fixture(&root)?;
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&runtime.lease_path)?;
    lease.lock()?;
    let capture = Capture::create(store, record.clone(), StorageConfig::default())?;
    capture.write(OutputStream::Stdout, b"before process exit")?;
    std::fs::write(root.join("run-id"), &record.id)?;
    // Simulate loss of the process without Rust destructors or terminal sealing.
    std::process::exit(0)
}

#[tokio::test]
async fn actual_owner_process_exit_preserves_prefix_and_never_replays_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let status = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "execution::recovery::tests::crash_fixture",
            "--nocapture",
        ])
        .env("JCODE_OWNER_LOSS_FIXTURE", root.path())
        .status()
        .await?;
    assert!(status.success());
    let id = std::fs::read_to_string(root.path().join("run-id"))?;
    let store = ExecutionStore::open(root.path())?;
    let input = store.invocation_input(&id)?;
    let record = store.recover_lost_owner(&id).await?.unwrap();
    assert_eq!(record.state, RunState::Interrupted);
    assert_eq!(record.stop_cause, Some(StopCause::OwnerCrash));
    assert_eq!(
        std::fs::read_to_string(record.output_path.unwrap())?,
        "before process exit"
    );
    assert_eq!(store.invocation_input(&id)?, input);
    assert_eq!(store.list("owner-loss", None, 10)?.len(), 1);
    Ok(())
}
