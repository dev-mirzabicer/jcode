use super::*;
use crate::message::{ContentBlock, Role};
use crate::session::Session;

struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Drop for Restore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value)
            } else {
                crate::env::remove_var(key)
            }
        }
        crate::config::invalidate_config_cache();
    }
}
fn isolate(path: &std::path::Path) -> Restore {
    let restore = Restore(
        ["JCODE_HOME", "JCODE_RUNTIME_DIR"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    crate::env::set_var("JCODE_HOME", path);
    crate::env::set_var("JCODE_RUNTIME_DIR", path.join("runtime"));
    crate::config::invalidate_config_cache();
    restore
}
#[test]
fn request_lifetime_correlation_and_atomic_acknowledgement_preserve_received_data() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    for correlated in [false, true] {
        let home = tempfile::tempdir()?;
        let _restore = isolate(home.path());
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(async {
                let mut session = Session::create(None, None);
                session.add_message(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: "original task".into(),
                        cache_control: None,
                    }],
                );
                session.save()?;
                let mut ingress = ProviderIngress::default();
                let registry = crate::tool::Registry::empty();
                let output = ingress
                    .receive(
                        &session.id,
                        "sdk-id",
                        &ToolOutput::new("complete acquired body"),
                    )
                    .await?;
                let reference = output.provider_receipt.as_ref().unwrap().clone();
                let before = serde_json::to_vec(&session)?;
                assert!(
                    super::super::history::prepare(&session, &registry)
                        .await
                        .is_err()
                );
                assert_eq!(serde_json::to_vec(&session)?, before);
                let store = ExecutionStore::open(home.path())?;
                let mut wrong = reference.clone();
                wrong.namespace = "different-store".into();
                assert!(
                    store
                        .correlate_provider_receipt(&wrong, &session.id, "sdk-id", "message")
                        .is_err()
                );
                assert!(
                    store
                        .correlate_provider_receipt(
                            &reference,
                            "other-session",
                            "sdk-id",
                            "message"
                        )
                        .is_err()
                );
                if correlated {
                    let message = session.add_message(
                        Role::Assistant,
                        vec![ContentBlock::ToolUse {
                            id: "sdk-id".into(),
                            name: "external".into(),
                            input: serde_json::json!({}),
                            thought_signature: None,
                        }],
                    );
                    session.save()?;
                    let context = ToolContext {
                        session_id: session.id.clone(),
                        message_id: message,
                        tool_call_id: "sdk-id".into(),
                        working_dir: None,
                        stdin_request_tx: None,
                        graceful_shutdown_signal: None,
                        execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
                        invocation: Default::default(),
                    };
                    let result = registry
                        .retain_provider_result("external", serde_json::json!({}), context, output)
                        .await?;
                    session.add_message(
                        Role::User,
                        crate::execution::tool_result_blocks("sdk-id".into(), result),
                    );
                    session.save()?;
                }
                let old_len = session.messages.len();
                if !correlated {
                    session.provider_receipt_namespace = "another-store".into();
                    session.provider_receipt_watermark = i64::MAX;
                    session.save()?;
                }
                drop(ingress);
                let prepared = super::super::history::prepare(&session, &registry).await?;
                let outcome = prepared.commit(&mut session)?;
                assert_eq!(
                    outcome.recovered_provider_receipts,
                    usize::from(!correlated)
                );
                assert_eq!(session.messages.len(), old_len + usize::from(!correlated));
                let mut restored = Session::load(&session.id)?;
                assert_eq!(restored.provider_receipt_watermark, reference.sequence);
                assert_eq!(restored.provider_receipt_namespace, reference.namespace);
                let stub = Session::load_startup_stub(&session.id)?;
                assert!(stub.messages.is_empty());
                assert_eq!(stub.provider_receipt_watermark, reference.sequence);
                assert_eq!(stub.provider_receipt_namespace, reference.namespace);
                let remote = Session::load_for_remote_startup(&session.id)?;
                assert_eq!(remote.provider_receipt_watermark, reference.sequence);
                assert_eq!(remote.provider_receipt_namespace, reference.namespace);
                let mut split = Session::create(None, None);
                split.inherit_continuation_state_from(&restored);
                assert_eq!(split.provider_receipt_watermark, 0);
                assert!(split.provider_receipt_namespace.is_empty());
                let prepared = super::super::history::prepare(&split, &registry).await?;
                assert_eq!(prepared.commit(&mut split)?.recovered_provider_receipts, 0);
                let mut regressed = restored.clone();
                regressed.provider_receipt_watermark = reference.sequence + 1;
                let unchanged = serde_json::to_vec(&regressed)?;
                assert!(
                    super::super::history::prepare(&regressed, &registry)
                        .await
                        .is_err()
                );
                assert_eq!(serde_json::to_vec(&regressed)?, unchanged);
                assert_eq!(
                    store.invocation_input(&reference.run_id)?.tool,
                    "provider_result_receipt"
                );
                let record = store.inspect(&reference.run_id)?.unwrap();
                assert_eq!(
                    std::fs::read_to_string(record.output_path.unwrap())?,
                    "complete acquired body"
                );
                restored.truncate_messages(1);
                restored.save()?;
                let prepared = super::super::history::prepare(&restored, &registry).await?;
                assert_eq!(
                    prepared.commit(&mut restored)?.recovered_provider_receipts,
                    0,
                    "Explicit history removal must not replay acknowledged receipts"
                );
                Ok::<_, anyhow::Error>(())
            })?;
    }
    Ok(())
}

#[test]
fn process_exit_fixture() -> Result<()> {
    let Some(root) = std::env::var_os("JCODE_PROVIDER_INGRESS_EXIT") else {
        return Ok(());
    };
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let root = PathBuf::from(root);
            let mut session = Session::create(None, None);
            session.save()?;
            let mut ingress = ProviderIngress::default();
            let output = ingress
                .receive(
                    &session.id,
                    "unmatched-sdk-id",
                    &ToolOutput::new("received before process loss"),
                )
                .await?;
            crate::storage::write_json_secret(
                &root.join("receipt.json"),
                &serde_json::json!({"session":session.id,"receipt":output.provider_receipt}),
            )?;
            std::process::exit(0)
        })
}

#[test]
fn process_loss_before_final_history_retains_receipt_without_inventing_tool_input() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _restore = isolate(home.path());
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "execution::provider_ingress::tests::process_exit_fixture",
            "--nocapture",
        ])
        .env("JCODE_PROVIDER_INGRESS_EXIT", home.path())
        .status()?;
    assert!(status.success());
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let receipt: serde_json::Value =
                crate::storage::read_json(&home.path().join("receipt.json"))?;
            let mut session = Session::load(receipt["session"].as_str().unwrap())?;
            let prepared =
                super::super::history::prepare(&session, &crate::tool::Registry::empty()).await?;
            assert_eq!(
                prepared.commit(&mut session)?.recovered_provider_receipts,
                1
            );
            assert!(
                session
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .all(|block| !matches!(
                        block,
                        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                    )),
                "An uncorrelated acquisition receipt is not a fabricated tool invocation"
            );
            let store = ExecutionStore::open(home.path())?;
            let record = store
                .inspect(receipt["receipt"]["run_id"].as_str().unwrap())?
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(record.output_path.unwrap())?,
                "received before process loss"
            );
            Ok(())
        })
}
