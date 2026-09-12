//! Asynchronous inspection for /fix. Only the local TUI's authoritative Session
//! owner may apply a completed, source-bound repair at an idle event-loop point.
use super::*;
use crate::execution::history::PreparedRepair;
use anyhow::Context;

pub(super) struct PendingRepair {
    session_id: String,
    last_error: Option<String>,
    receiver: tokio::sync::oneshot::Receiver<anyhow::Result<PreparedRepair>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for PendingRepair {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl App {
    pub(super) fn start_history_repair(&mut self) {
        if self.is_remote {
            self.push_display_message(DisplayMessage::error("History repair belongs to the attached server; this client did not save or replace its partial Session."));
            return;
        }
        if self.is_processing || self.history_repair.is_some() {
            self.set_status_notice("Wait for current work before repairing history");
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.set_status_notice("History repair requires the active local runtime");
            return;
        };
        let session = self.session.clone();
        let registry = self.registry.clone_with_shared_context_runtime();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = runtime.spawn(async move {
            let result = crate::execution::history::prepare(&session, &registry).await;
            let _ = sender.send(result);
        });
        self.history_repair = Some(PendingRepair {
            session_id: self.session.id.clone(),
            last_error: self.last_stream_error.clone(),
            receiver,
            task,
        });
        self.set_status_notice("Checking retained tool results · no model request");
    }

    fn finish_fix(&mut self, repaired: usize) -> anyhow::Result<()> {
        let reset =
            self.provider_session_id.is_some() || self.session.provider_session_id.is_some();
        if reset {
            let mut candidate = self.session.clone();
            candidate.provider_session_id = None;
            candidate.save().context(
                "History repair finished, but provider resume reset could not be persisted",
            )?;
            self.session = candidate;
            self.provider_session_id = None;
            self.after_local_provider_context_changed(
                "explicit fix",
                "reset provider resume state without changing history",
            )
            .map_err(anyhow::Error::msg)?;
        }
        self.last_stream_error = None;
        self.set_status_notice("Fix complete · history preserved");
        self.push_display_message(DisplayMessage::system(format!("Fix Results:\nRecovered {repaired} missing tool result(s).{}\nHistory was not converted to text or copied into a replacement session. Context reduction remains an explicit Context Editor operation.",if reset{" Provider resume state was reset."}else{""})));
        Ok(())
    }
}

pub(super) fn drain(app: &mut App) -> bool {
    let Some(pending) = app.history_repair.as_mut() else {
        return false;
    };
    if pending.session_id != app.session.id || app.is_remote {
        app.history_repair = None;
        return false;
    }
    if app.is_processing {
        return false;
    }
    let result = match pending.receiver.try_recv() {
        Ok(result) => result,
        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(anyhow::anyhow!(
            "History inspection ended without a repair result"
        )),
    };
    let pending = app.history_repair.take().unwrap();
    let result = if pending.last_error != app.last_stream_error {
        Err(anyhow::anyhow!(
            "A newer error superseded this repair request; current state was preserved"
        ))
    } else {
        result
            .and_then(|prepared| app.apply_history_repair(prepared))
            .and_then(|repaired| app.finish_fix(repaired))
    };
    if let Err(error) = result {
        app.set_status_notice("History repair blocked · no replacement session");
        app.push_display_message(DisplayMessage::error(format!(
            "History repair could not finish: {error:#}"
        )));
    }
    true
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
    fn fix_restores_retained_results_and_preserves_live_stale_remote_and_switched_sessions()
    -> anyhow::Result<()> {
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
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(async {
                for mode in ["complete", "live", "stale", "switch", "remote"] {
                    let mut app =
                        tokio::task::block_in_place(crate::tui::app::tests::create_test_app);
                    let id = app.session.id.clone();
                    let tool = ToolCall {
                        id: "repair-fixture".into(),
                        name: "fixture".into(),
                        input: serde_json::json!({}),
                        intent: None,
                        thought_signature: None,
                    };
                    let message = app.session.add_message(
                        Role::Assistant,
                        vec![ContentBlock::ToolUse {
                            id: tool.id.clone(),
                            name: tool.name.clone(),
                            input: tool.input.clone(),
                            thought_signature: None,
                        }],
                    );
                    app.session.save()?;
                    let context = crate::tool::ToolContext {
                        session_id: id.clone(),
                        message_id: message,
                        tool_call_id: tool.id.clone(),
                        working_dir: None,
                        stdin_request_tx: None,
                        graceful_shutdown_signal: None,
                        execution_mode: crate::tool::ToolExecutionMode::Direct,
                        invocation: Default::default(),
                    };
                    let live = if mode == "live" {
                        Some(
                            crate::tool::inflight::mark_tool_in_flight(
                                &crate::execution::invocation(
                                    &context,
                                    &tool.name,
                                    tool.input.clone(),
                                ),
                            )
                            .unwrap(),
                        )
                    } else {
                        app.registry
                            .retain_provider_result(
                                &tool.name,
                                tool.input.clone(),
                                context,
                                crate::tool::ToolOutput::new("complete retained result"),
                            )
                            .await?;
                        None
                    };
                    let original = serde_json::to_vec(&app.session)?;
                    if mode == "remote" {
                        app.is_remote = true;
                    }
                    app.input = "/fix".into();
                    app.submit_input();
                    app.input = "keep this unsent user draft".into();
                    if mode == "stale" {
                        app.session.title = Some("newer title".into());
                    }
                    if mode == "switch" {
                        app.session = Session::create(None, None);
                    }
                    let current = serde_json::to_vec(&app.session)?;
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    while app.history_repair.is_some() {
                        super::super::local::handle_tick(&mut app);
                        anyhow::ensure!(
                            tokio::time::Instant::now() < deadline,
                            "/fix did not terminate: {mode}"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    assert_eq!(app.input, "keep this unsent user draft");
                    if mode == "complete" {
                        assert_eq!(app.session.id, id);
                        let saved = Session::load(&id)?;
                        let result = saved
                            .messages
                            .iter()
                            .flat_map(|message| &message.content)
                            .find_map(|block| match block {
                                ContentBlock::ToolResult { content, .. } => Some(content),
                                _ => None,
                            })
                            .unwrap();
                        assert!(result.contains("complete retained result"));
                        assert!(!result.contains("Tool output missing"));
                    } else {
                        assert_eq!(
                            serde_json::to_vec(&app.session)?,
                            current,
                            "Repair changed current state: {mode}"
                        );
                        assert_eq!(
                            serde_json::to_vec(&Session::load(&id)?)?,
                            original,
                            "Repair changed original stored Session: {mode}"
                        );
                    }
                    drop(live);
                }
                Ok(())
            })
    }
}
