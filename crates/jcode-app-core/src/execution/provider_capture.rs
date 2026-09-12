//! Explicit request-local capture shared by a provider adapter and its consumer.
//! No mutable provider singleton or task-local context is used.
use super::*;
use jcode_provider_core::{ProviderRequestContext, ProviderResultCapture};
use jcode_tool_types::ProviderReceiptReference;

#[derive(Clone)]
pub struct ProviderCaptureScope {
    session: String,
    ingress: Arc<tokio::sync::Mutex<ProviderIngress>>,
    received: Arc<AtomicBool>,
}
impl ProviderCaptureScope {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            ingress: Arc::new(tokio::sync::Mutex::new(ProviderIngress::default())),
            received: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn context(&self) -> ProviderRequestContext {
        ProviderRequestContext {
            result_capture: Some(Arc::new(self.clone())),
        }
    }
    pub fn has_received_data(&self) -> bool {
        self.received.load(Ordering::SeqCst)
    }
    pub async fn receive(
        &self,
        key: &str,
        mut output: ToolOutput,
        receipt: Option<ProviderReceiptReference>,
        calls: &[crate::message::ToolCall],
    ) -> Result<ToolOutput> {
        self.received.store(true, Ordering::SeqCst);
        let mut ingress = self.ingress.lock().await;
        if let Some(receipt) = receipt {
            ingress
                .validate_captured(&self.session, key, &output, &receipt, calls)
                .await?;
            output.provider_receipt = Some(receipt);
            Ok(output)
        } else {
            ingress
                .receive_with_calls(&self.session, key, &output, calls)
                .await
        }
    }
}
#[async_trait::async_trait]
impl ProviderResultCapture for ProviderCaptureScope {
    fn has_received_data(&self) -> bool {
        ProviderCaptureScope::has_received_data(self)
    }
    async fn capture_with_calls(
        &self,
        key: &str,
        output: &ToolOutput,
        calls: &[crate::message::ToolCall],
    ) -> Result<ProviderReceiptReference> {
        self.received.store(true, Ordering::SeqCst);
        self.ingress
            .lock()
            .await
            .receive_with_calls(&self.session, key, output, calls)
            .await?
            .provider_receipt
            .context("Provider acquisition returned no receipt")
    }
    async fn capture(&self, key: &str, output: &ToolOutput) -> Result<ProviderReceiptReference> {
        self.received.store(true, Ordering::SeqCst);
        let output = self
            .ingress
            .lock()
            .await
            .receive(&self.session, key, output)
            .await?;
        output
            .provider_receipt
            .context("Provider acquisition returned no receipt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adapter_capture_is_retained_once_and_references_cannot_cross_requests() -> Result<()> {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir()?;
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(value) = self.0.take() {
                    crate::env::set_var("JCODE_HOME", value)
                } else {
                    crate::env::remove_var("JCODE_HOME")
                }
            }
        }
        let _restore = Restore(std::env::var_os("JCODE_HOME"));
        crate::env::set_var("JCODE_HOME", home.path());
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(async {
                let session = crate::id::new_id("capture-session");
                let scope = ProviderCaptureScope::new(&session);
                let output = ToolOutput::new("complete adapter result");
                let context = scope.context();
                let capture = context.result_capture.as_ref().unwrap();
                let observed = crate::message::ToolCall {
                    id: "sdk".into(),
                    name: "external".into(),
                    input: serde_json::json!({"value":"observed"}),
                    ..Default::default()
                };
                let reference = capture
                    .capture_with_calls("sdk", &output, std::slice::from_ref(&observed))
                    .await?;
                let received = scope
                    .receive("sdk", output.clone(), Some(reference.clone()), &[])
                    .await?;
                assert_eq!(received.provider_receipt, Some(reference.clone()));
                let mut changed = observed.clone();
                changed.input = serde_json::json!({"value":"changed"});
                assert!(
                    scope
                        .receive("sdk", output.clone(), Some(reference.clone()), &[changed])
                        .await
                        .is_err()
                );
                let store = ExecutionStore::open(home.path())?;
                assert_eq!(
                    store.invocation_input(&reference.run_id)?.input["observed_tool_calls"],
                    serde_json::json!([observed])
                );
                assert_eq!(store.provider_receipts_after(&session, 0)?.len(), 1);
                assert!(
                    scope
                        .receive(
                            "sdk",
                            ToolOutput::new("changed"),
                            Some(reference.clone()),
                            &[]
                        )
                        .await
                        .is_err()
                );
                assert!(
                    scope
                        .receive("other", output.clone(), Some(reference.clone()), &[])
                        .await
                        .is_err()
                );
                assert!(
                    ProviderCaptureScope::new(&session)
                        .receive("sdk", output, Some(reference), &[])
                        .await
                        .is_err()
                );
                assert_eq!(store.provider_receipts_after(&session, 0)?.len(), 1);
                Ok(())
            })
    }
}
