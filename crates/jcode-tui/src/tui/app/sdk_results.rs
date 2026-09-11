use super::*;
use crate::tool::ToolContext;
use std::path::PathBuf;

impl App {
    /// Retain before publishing UI/provider views. Local Session is authoritative;
    /// never persist the former empty placeholder for an SDK-provided result.
    pub(super) async fn record_local_sdk_result(
        &mut self,
        tool: &ToolCall,
        message_id: &str,
        received: crate::tool::ToolOutput,
    ) -> Result<crate::tool::ToolOutput> {
        let context = ToolContext {
            session_id: self.session.id.clone(),
            message_id: message_id.into(),
            tool_call_id: tool.id.clone(),
            working_dir: self.session.working_dir.as_deref().map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
            invocation: Default::default(),
        };
        let result = self
            .registry
            .retain_provider_result(&tool.name, tool.input.clone(), context, received.clone())
            .await;
        let (output,failure)=match result {
            Ok(output)=>(output,None),
            Err(error)=>(crate::tool::ToolOutput::new(format!("[SDK output retention failed; original received body follows. Do not repeat the operation.]\n{}",crate::execution::sdk_failure_body(&received))).with_error(true),Some(error)),
        };
        let blocks = crate::execution::tool_result_blocks(tool.id.clone(), output.clone());
        let mut candidate = self.session.clone();
        candidate.add_message(Role::User, blocks.clone());
        candidate.save()?;
        self.session = candidate;
        self.add_provider_message(Message {
            role: Role::User,
            content: blocks,
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        });
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn local_sdk_results_and_checkpoints_persist_full_receipts_instead_of_empty_placeholders()
    -> Result<()> {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir()?;
        let _restore = Restore(
            ["JCODE_HOME", "JCODE_RUNTIME_DIR"]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        );
        crate::env::set_var("JCODE_HOME", home.path());
        crate::env::set_var("JCODE_RUNTIME_DIR", home.path().join("runtime"));
        crate::config::invalidate_config_cache();
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?.block_on(async {
            for checkpoint in [false,true] {
                let mut app=tokio::task::block_in_place(crate::tui::app::tests::create_test_app);
                let tool=ToolCall{id:"local-sdk".into(),name:"external_result".into(),input:serde_json::json!({}),intent:None,thought_signature:None};
                let body=format!("{}LOCAL_TAIL","x".repeat(60_000));
                let original=format!("  {}  ",serde_json::json!({"type":"tool_result","tool_use_id":tool.id,"extra":"keep","content":[{"type":"image","source":{"media_type":"image/png","data":"eA=="}}]}));
                let received=crate::execution::received_sdk_result(&tool.id,body.clone(),false,Some(original.clone()));
                if checkpoint {
                    app.checkpoint_partial_local_provider_output("partial","","",&[],std::slice::from_ref(&tool),&std::collections::HashMap::from([(tool.id.clone(),received)]),&[],false).await?;
                    assert!(app.partial_output_persistence_error.is_none());
                } else {
                    let message=app.session.add_message(Role::Assistant,vec![ContentBlock::ToolUse{id:tool.id.clone(),name:tool.name.clone(),input:tool.input.clone(),thought_signature:None}]);
                    app.record_local_sdk_result(&tool,&message,received).await?;
                }
                let stored=crate::session::Session::load(&app.session.id)?;
                let result=stored.messages.iter().flat_map(|message|&message.content).find_map(|block|match block{ContentBlock::ToolResult{content,..}=>Some(content),_=>None}).expect("Saved SDK result");
                assert!(!result.is_empty());assert!(!result.contains("LOCAL_TAIL"));
                let store=crate::execution::ExecutionStore::open(home.path())?;let records=store.list(&app.session.id,None,100)?;let record=records.iter().find(|record|record.tool==tool.name).unwrap();
                assert_eq!(std::fs::read_to_string(record.output_path.as_ref().unwrap())?,body);
                let result=store.result(record,std::num::NonZeroUsize::new(100).unwrap())?;
                assert_eq!(result.metadata.unwrap()["provider_original"],original);assert_eq!(result.images[0].data,"eA==");
            }
            Ok(())
        })
    }
}
