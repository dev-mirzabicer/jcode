use super::*;
use crate::execution::{
    DeliveryChannel, DeliveryState, ExecutionStore, Invocation, PreparedInvocation, RunState,
    RuntimeEndpoint,
};

#[test]
fn runtime_namespace_preserves_completion_for_original_host() -> anyhow::Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let store = ExecutionStore::open(sandbox.root())?;
        let runtimes = store.root().join("runtimes");
        crate::storage::ensure_dir(&runtimes)?;
        let lease_path = runtimes.join("namespace-fixture.lease");
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease_path)?;
        lease.try_lock()?;
        let owner = uuid::Uuid::new_v4().simple().to_string();
        let endpoint = RuntimeEndpoint::new(
            owner.clone(),
            runtimes.join("fixture.sock"),
            lease_path,
            "c".repeat(64),
        );
        store.register_runtime(&endpoint)?;
        store.bind_runtime_namespace(&owner, &"a".repeat(64))?;
        let invocation = Invocation {
            session_id: "namespace-recipient".into(),
            message_id: "fixture".into(),
            call_path: vec!["completion".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(mut record) = store.prepare(&invocation, &owner)? else {
            anyhow::bail!("Fixture replayed");
        };
        store.start(&record.id, &owner)?;
        store.promote(&record.id, &owner)?;
        store.register_background_delivery(&record.id, true, true)?;
        record.state = RunState::Completed;
        store.finish(&record)?;
        let event = crate::bus::BackgroundTaskCompleted {
            task_id: record.id.clone(),
            tool_name: "fixture".into(),
            display_name: None,
            session_id: invocation.session_id,
            status: crate::bus::BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            output_preview: "retained".into(),
            output_file: sandbox.root().join("unused-output"),
            duration_secs: 0.0,
            notify: true,
            wake: true,
        };
        let foreign = crate::runtime_lifecycle::admission::RuntimeAdmission::register_namespace(
            sandbox.root(),
            "foreign",
            Some("b".repeat(64)),
        )?;
        assert!(matches!(
            completion_permit(&event, DeliveryChannel::Wake).await,
            CompletionPermit::Skip
        ));
        assert_eq!(
            store.background_delivery(&record.id)?.unwrap().wake_state,
            DeliveryState::Pending
        );
        drop(foreign);
        let _original = crate::runtime_lifecycle::admission::RuntimeAdmission::register_namespace(
            sandbox.root(),
            "original",
            Some("a".repeat(64)),
        )?;
        let permit = completion_permit(&event, DeliveryChannel::Wake).await;
        assert!(matches!(permit, CompletionPermit::Tracked(_)));
        finish_completion(permit, true).await;
        assert_eq!(
            store.background_delivery(&record.id)?.unwrap().wake_state,
            DeliveryState::Delivered
        );
        Ok(())
    })
}
