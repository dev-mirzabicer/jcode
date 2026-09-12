//! Shared Session-owned tool-result repair. Inspection may run asynchronously;
//! publication validates its exact source and never saves a client projection.
use crate::message::{ContentBlock, Role, TOOL_OUTPUT_MISSING_TEXT, ToolCall};
use crate::session::{Session, StoredMessage};
use crate::tool::{Registry, ToolContext, ToolExecutionMode, ToolOutput};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug)]
pub struct HistoryScan {
    pub calls: HashSet<String>,
    pub results: HashSet<String>,
    pending: Vec<(usize, usize, ToolCall)>,
}
impl HistoryScan {
    pub fn missing_ids(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|(_, _, call)| call.id.clone())
            .collect()
    }
}

pub fn scan<'a>(
    messages: impl IntoIterator<Item = (&'a Role, &'a [ContentBlock])>,
) -> Result<HistoryScan> {
    let mut pending = HashMap::new();
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for (index, (role, content)) in messages.into_iter().enumerate() {
        for (ordinal, block) in content.iter().enumerate() {
            match (role, block) {
                (
                    Role::Assistant,
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        thought_signature,
                    },
                ) => {
                    ensure!(
                        !pending.contains_key(id),
                        "Ambiguous historical tool ID {id}: multiple unresolved uses. No replacement result was invented."
                    );
                    pending.insert(
                        id.clone(),
                        (
                            index,
                            ordinal,
                            ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                                intent: ToolCall::intent_from_input(input),
                                thought_signature: thought_signature.clone(),
                            },
                        ),
                    );
                    calls.insert(id.clone());
                }
                (Role::User, ContentBlock::ToolResult { tool_use_id, .. }) => {
                    pending.remove(tool_use_id);
                    results.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }
    let mut pending = pending.into_values().collect::<Vec<_>>();
    pending.sort_by_key(|(index, ordinal, _)| (*index, *ordinal));
    Ok(HistoryScan {
        calls,
        results,
        pending,
    })
}

pub struct PreparedRepair {
    source: [u8; 32],
    candidate: Option<Session>,
    outcome: RepairOutcome,
}
#[derive(Debug)]
pub struct RepairOutcome {
    pub repaired: usize,
    pub calls: HashSet<String>,
    pub results: HashSet<String>,
}

impl PreparedRepair {
    /// Only the authority that owns this full Session may publish it. Drafts,
    /// history changes and session switches cannot be overwritten by a late reply.
    pub fn commit(mut self, current: &mut Session) -> Result<RepairOutcome> {
        ensure!(
            fingerprint(current)? == self.source,
            "History repair is stale; current Session and user intent were not changed"
        );
        if let Some(candidate) = self.candidate.as_mut() {
            candidate.save().context(
                "History repair could not be persisted; current Session was not replaced",
            )?;
            *current = self.candidate.take().unwrap();
        }
        Ok(self.outcome)
    }
}

pub async fn prepare(session: &Session, registry: &Registry) -> Result<PreparedRepair> {
    let source = fingerprint(session)?;
    let scan = scan(
        session
            .messages
            .iter()
            .map(|message| (&message.role, message.content.as_slice())),
    )?;
    let mut outcome = RepairOutcome {
        repaired: 0,
        calls: scan.calls,
        results: scan.results,
    };
    let mut repairs = Vec::new();
    for (index, ordinal, tool) in scan.pending {
        let ctx = ToolContext {
            session_id: session.id.clone(),
            message_id: session.messages[index].id.clone(),
            tool_call_id: tool.id.clone(),
            working_dir: session.working_dir.as_deref().map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let invocation = crate::execution::invocation(&ctx, &tool.name, serde_json::Value::Null);
        let output = if let Some(output) = registry
            .retained_history_result(&tool.name, ctx, tool.input.clone())
            .await?
        {
            output
        } else {
            ensure!(
                !crate::tool::inflight::is_tool_in_flight(&invocation),
                "Execution {} is still in flight. Its result was not replaced or repeated.",
                invocation.id()
            );
            ToolOutput::new(TOOL_OUTPUT_MISSING_TEXT).with_error(true)
        };
        repairs.push((index, ordinal, tool.id, output));
    }
    let candidate = if repairs.is_empty() {
        None
    } else {
        let mut candidate = session.clone();
        for (inserted, (index, _, id, output)) in repairs.into_iter().enumerate() {
            candidate.insert_message(
                index + 1 + inserted,
                StoredMessage {
                    origin: None,
                    id: crate::id::new_id("message"),
                    role: Role::User,
                    content: crate::execution::tool_result_blocks(id.clone(), output),
                    display_role: None,
                    timestamp: Some(chrono::Utc::now()),
                    tool_duration_ms: None,
                    token_usage: None,
                },
            );
            outcome.results.insert(id);
            outcome.repaired += 1;
        }
        let reconciliation = jcode_context_core::reconcile_context_after_transcript_edit(
            &candidate.messages,
            &candidate.context_view,
            chrono::Utc::now(),
            "historical tool-output repair inserted exact provider structure",
        )?;
        candidate.context_view = reconciliation.state;
        candidate.provider_session_id = None;
        Some(candidate)
    };
    Ok(PreparedRepair {
        source,
        candidate,
        outcome,
    })
}

fn fingerprint(session: &Session) -> Result<[u8; 32]> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = HashWriter(Sha256::new());
    serde_json::to_writer(&mut hash, session)?;
    Ok(hash.0.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_repair_cannot_overwrite_newer_session_or_context_state() -> Result<()> {
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
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut session = Session::create(None, None);
                session.add_message(
                    Role::Assistant,
                    vec![ContentBlock::ToolUse {
                        id: "missing".into(),
                        name: "fixture".into(),
                        input: serde_json::json!({}),
                        thought_signature: None,
                    }],
                );
                let registry = Registry::empty();
                let prepared = prepare(&session, &registry).await?;
                session.title = Some("newer user intent".into());
                let before = serde_json::to_vec(&session)?;
                assert!(prepared.commit(&mut session).is_err());
                assert_eq!(serde_json::to_vec(&session)?, before);
                Ok(())
            })
    }
    #[test]
    fn sequential_equal_ids_are_paired_by_occurrence_not_a_global_set() -> Result<()> {
        let mut session = Session::create(None, None);
        let call = || ContentBlock::ToolUse {
            id: "equal".into(),
            name: "fixture".into(),
            input: serde_json::json!({}),
            thought_signature: None,
        };
        session.add_message(Role::Assistant, vec![call()]);
        session.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "equal".into(),
                content: "first".into(),
                is_error: None,
            }],
        );
        session.add_message(Role::Assistant, vec![call()]);
        assert_eq!(
            scan(
                session
                    .messages
                    .iter()
                    .map(|message| (&message.role, message.content.as_slice()))
            )?
            .missing_ids(),
            vec!["equal"]
        );
        session.add_message(Role::Assistant, vec![call()]);
        assert!(
            scan(
                session
                    .messages
                    .iter()
                    .map(|message| (&message.role, message.content.as_slice()))
            )
            .is_err()
        );
        Ok(())
    }
}
