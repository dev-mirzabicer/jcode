use super::*;
use crate::instruction::{
    InstructionId, InstructionKind, InstructionResourceRef, InstructionScope,
};
use futures::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
struct RecordingProvider {
    calls: Arc<AtomicUsize>,
    quiet: bool,
}
#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &str {
        "isolated-fixture"
    }
    fn model(&self) -> String {
        "synthetic-model".into()
    }
    fn credential_mode(&self) -> crate::provider::CredentialMode {
        crate::provider::CredentialMode::OAuth
    }
    fn reasoning_effort(&self) -> Option<String> {
        Some("high".into())
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let first = futures::stream::iter(vec![Ok(crate::message::StreamEvent::TextDelta(
            "SYNTHETIC REPLY".into(),
        ))]);
        if self.quiet {
            Ok(Box::pin(first.chain(futures::stream::pending())))
        } else {
            Ok(Box::pin(first.chain(futures::stream::iter(vec![Ok(
                crate::message::StreamEvent::MessageEnd { stop_reason: None },
            )]))))
        }
    }
}

async fn fixture(
    quiet: bool,
) -> (
    crate::auth::test_sandbox::AuthTestSandbox,
    Agent,
    Arc<AtomicUsize>,
    PathBuf,
) {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let artifacts = home.root().join("artifacts/session_child_fixture");
    std::fs::create_dir_all(&artifacts).unwrap();
    let work = home.root().join("project");
    std::fs::create_dir_all(&work).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        calls: calls.clone(),
        quiet,
    });
    let registry = Registry::new(provider.clone()).await;
    let mut session = Session::create_with_id("session_child_fixture".into(), None, None);
    let profile = crate::session::StoredAgentReference {
        scope: InstructionScope::Global,
        id: "fixture".into(),
        display_name: "Fixture".into(),
    };
    session.install_system_prompt(crate::session::StoredSystemPromptState {
        text: "SYNTHETIC SYSTEM".into(),
        active_agent: profile.clone(),
        first_provider_dispatch_at: None,
        active_transition_message_id: None,
    });
    let selection = jcode_provider_core::RouteSelection {
        model: "synthetic-model".into(),
        runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
        api_method: "openai-oauth".into(),
        provider_label: "OpenAI".into(),
        detail: String::new(),
    };
    let resolution = serde_json::from_value(serde_json::json!({"requested_alias":"fixture","selection":selection,"selected_effort":"high","used_model_override":false,"used_effort_override":false})).unwrap();
    session
        .install_isolated_child(
            crate::session::IsolatedChildIdentity {
                blocked_mcps: Default::default(),
                profile,
                original_parent: "session_parent_fixture".into(),
                creation_run: "run-fixture".into(),
                working_dir: work.canonicalize().unwrap(),
                artifact_dir: artifacts.canonicalize().unwrap(),
                resolution,
            },
            Permission::ReadOnly,
            crate::instruction::TaskPresetActivation {
                resource: InstructionResourceRef {
                    scope: InstructionScope::Global,
                    kind: InstructionKind::Notification,
                    id: InstructionId::parse("task-preset.general").unwrap(),
                },
                text: "SYNTHETIC PRESET".into(),
            },
        )
        .unwrap();
    let agent = Agent::from_isolated_session(
        provider,
        registry,
        session,
        crate::instruction::InstructionRepositoryService::new(),
    )
    .unwrap();
    (home, agent, calls, artifacts)
}

#[tokio::test]
async fn child_reply_is_one_ordinary_turn_and_restores_without_model_switch() {
    let (_home, mut agent, calls, _) = fixture(false).await;
    agent
        .prepare_isolated_turn("run-first", "self-contained input", None, None)
        .await
        .unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let reply = agent
        .run_prepared_isolated_turn("run-first", InterruptSignal::new(), tx)
        .await
        .unwrap();
    assert_eq!(reply, "SYNTHETIC REPLY");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let loaded = Session::load(agent.session_id()).unwrap();
    assert_eq!(loaded.isolated_child, agent.session.isolated_child);
    assert!(
        agent
            .prepare_isolated_turn("run-first", "duplicate", None, None)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn child_stop_interrupts_quiet_stream_and_retains_partial_reply() {
    let (_home, mut agent, calls, _) = fixture(true).await;
    agent
        .prepare_isolated_turn("run-stop", "input", None, None)
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stop = InterruptSignal::new();
    let signal = stop.clone();
    let run = agent.run_prepared_isolated_turn("run-stop", stop, tx);
    let stopper = async move {
        while let Some(event) = rx.recv().await {
            if matches!(event, ServerEvent::TextDelta { .. }) {
                signal.fire_with_cause(jcode_tool_types::StopCause::HumanCancellation);
                break;
            }
        }
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(run, stopper)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        agent.last_assistant_text().as_deref(),
        Some("SYNTHETIC REPLY")
    );
    Session::load(agent.session_id())
        .unwrap()
        .validate_active_agent_profile()
        .unwrap();
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn child_registry_enforces_native_writes_and_releases_its_tool_map() {
    let (_home, agent, _, artifacts) = fixture(false).await;
    let context = |call: &str| ToolContext {
        session_id: agent.session_id().into(),
        message_id: "message-fixture".into(),
        tool_call_id: call.into(),
        working_dir: agent.working_dir().map(PathBuf::from),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: Default::default(),
    };
    let file = artifacts.join("report.md");
    agent
        .registry
        .execute(
            "write",
            serde_json::json!({"file_path":file,"content":"artifact"}),
            context("inside"),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "artifact");
    let outside = PathBuf::from(agent.working_dir().unwrap()).join("outside.md");
    assert!(
        agent
            .registry
            .execute(
                "write",
                serde_json::json!({"file_path":outside,"content":"blocked"}),
                context("outside")
            )
            .await
            .is_err()
    );
    assert!(!outside.exists());
    assert!(
        agent
            .registry
            .execute(
                "selfdev",
                serde_json::json!({"action":"status"}),
                context("admin")
            )
            .await
            .is_err()
    );
    crate::tool::clear_session_tool_policy(agent.session_id());
    drop(agent);
}

#[tokio::test]
async fn child_directives_are_locked_before_curator_work_and_survive_rewind_undo() {
    let (_home, mut agent, calls, _) = fixture(false).await;
    agent
        .prepare_isolated_turn("run-first", "first input", None, None)
        .await
        .unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    agent
        .run_prepared_isolated_turn("run-first", InterruptSignal::new(), tx)
        .await
        .unwrap();
    let preset = crate::instruction::TaskPresetActivation {
        resource: InstructionResourceRef {
            scope: InstructionScope::Global,
            kind: InstructionKind::Notification,
            id: InstructionId::parse("task-preset.special").unwrap(),
        },
        text: "SYNTHETIC SPECIAL".into(),
    };
    agent
        .prepare_isolated_turn(
            "run-second",
            "second input",
            Some(Permission::ReadWrite),
            Some(preset),
        )
        .await
        .unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    agent
        .run_prepared_isolated_turn("run-second", InterruptSignal::new(), tx)
        .await
        .unwrap();
    let service = crate::context::ContextTransactionService::new();
    let snapshot = service
        .context_editor_snapshot_for_session(
            &agent.session,
            false,
            agent.provider.as_ref(),
            "fixture",
            None,
        )
        .unwrap();
    let protected = agent.session.active_child_directive_ids();
    for id in &protected {
        assert!(
            snapshot
                .messages
                .iter()
                .any(|message| &message.message_id == id
                    && message.active_child_directive
                    && message.instructions_locked())
        );
        let request = crate::protocol::ContextMessageRangeSelection {
            start_message_id: id.clone(),
            end_message_id: id.clone(),
        };
        assert!(
            service
                .preview_context_ranges_for_session(
                    &agent.session,
                    snapshot.context_revision,
                    snapshot.transcript_digest,
                    &[request]
                )
                .is_err()
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "range rejection must precede any curator request"
    );
    let before = serde_json::to_value(&agent.session.messages).unwrap();
    agent.rewind_to_message(1).unwrap();
    agent.session.validate_active_agent_profile().unwrap();
    assert_eq!(agent.session.active_child_directive_ids(), protected);
    agent.undo_rewind().unwrap();
    assert_eq!(
        serde_json::to_value(&agent.session.messages).unwrap(),
        before
    );
    agent.session.validate_active_agent_profile().unwrap();
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn children_cannot_update_shared_initiatives_or_be_restored_as_primary_chat() {
    let (home, mut agent, calls, _) = fixture(false).await;
    agent
        .prepare_isolated_turn("run-rw", "input", Some(Permission::ReadWrite), None)
        .await
        .unwrap();
    for scope in ["project", "global"] {
        let ctx = ToolContext {
            session_id: agent.session_id().into(),
            message_id: "message-initiative".into(),
            tool_call_id: scope.into(),
            working_dir: agent.working_dir().map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let result = agent
            .registry
            .execute(
                "initiative",
                serde_json::json!({"action":"create","scope":scope,"title":"must not publish"}),
                ctx,
            )
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("initiative updates belong to the parent")
        );
    }
    assert!(!home.root().join("goals").exists());
    let mut primary = Agent::new(agent.provider.clone(), Registry::empty());
    let previous = primary.session_id().to_string();
    assert!(primary.restore_session(agent.session_id()).is_err());
    assert_eq!(primary.session_id(), previous);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::tool::clear_session_tool_policy(primary.session_id());
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn child_mutator_matrix_checks_every_target_before_effects() {
    let (_home, agent, _calls, artifacts) = fixture(false).await;
    let inside = artifacts.join("matrix.txt");
    let outside = PathBuf::from(agent.working_dir().unwrap()).join("outside.txt");
    std::fs::write(&inside, "OLD\n").unwrap();
    std::fs::write(&outside, "OLD\n").unwrap();
    let inputs = |path: &std::path::Path| {
        vec![
            (
                "write",
                serde_json::json!({"file_path":path,"content":"NEW\n"}),
            ),
            (
                "file_write",
                serde_json::json!({"file_path":path,"content":"NEW\n"}),
            ),
            (
                "edit",
                serde_json::json!({"file_path":path,"old_string":"OLD","new_string":"NEW"}),
            ),
            (
                "file_edit",
                serde_json::json!({"file_path":path,"old_string":"OLD","new_string":"NEW"}),
            ),
            (
                "multiedit",
                serde_json::json!({"file_path":path,"edits":[{"old_string":"OLD","new_string":"NEW"}]}),
            ),
            (
                "patch",
                serde_json::json!({"patch_text":format!("--- {}\n+++ {}\n@@ -1 +1 @@\n-OLD\n+NEW\n",path.display(),path.display())}),
            ),
            (
                "apply_patch",
                serde_json::json!({"patch_text":format!("*** Begin Patch\n*** Update File: {}\n@@\n-OLD\n+NEW\n*** End Patch",path.display())}),
            ),
        ]
    };
    for (scope, path) in [("inside", &inside), ("outside", &outside)] {
        for (tool, input) in inputs(path) {
            std::fs::write(&inside, "OLD\n").unwrap();
            std::fs::write(&outside, "OLD\n").unwrap();
            let ctx = ToolContext {
                session_id: agent.session_id().into(),
                message_id: "matrix".into(),
                tool_call_id: format!("{scope}-{tool}"),
                working_dir: agent.working_dir().map(PathBuf::from),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: ToolExecutionMode::Direct,
                invocation: Default::default(),
            };
            let result = agent.registry.execute(tool, input, ctx).await;
            if scope == "inside" {
                assert!(!result.unwrap().is_error, "{tool}");
                assert_eq!(std::fs::read_to_string(&inside).unwrap(), "NEW\n");
            } else {
                assert!(
                    result.unwrap_err().to_string().contains("Read-only child"),
                    "{tool}"
                );
            }
            assert_eq!(std::fs::read_to_string(&outside).unwrap(), "OLD\n");
        }
    }
    for (id, patch) in [
        (
            "mixed",
            format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n-OLD\n+NEW\n*** Update File: {}\n@@\n-OLD\n+NEW\n*** End Patch",
                inside.display(),
                outside.display()
            ),
        ),
        (
            "move",
            format!(
                "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-OLD\n+NEW\n*** End Patch",
                inside.display(),
                outside.display()
            ),
        ),
    ] {
        std::fs::write(&inside, "OLD\n").unwrap();
        let ctx = ToolContext {
            session_id: agent.session_id().into(),
            message_id: "matrix".into(),
            tool_call_id: id.into(),
            working_dir: agent.working_dir().map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        assert!(
            agent
                .registry
                .execute("apply_patch", serde_json::json!({"patch_text":patch}), ctx)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), "OLD\n");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "OLD\n");
    }
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn child_does_not_adopt_primary_guardrail_reconsideration_workflow() {
    struct Fable;
    #[async_trait::async_trait]
    impl Provider for Fable {
        fn name(&self) -> &str {
            "synthetic"
        }
        fn model(&self) -> String {
            "claude-fable-5".into()
        }
        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
        async fn complete(
            &self,
            _: &[crate::message::Message],
            _: &[crate::message::ToolDefinition],
            _: &str,
            _: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            panic!("mechanism check must not call inference")
        }
    }
    let (home, mut agent, _, _) = fixture(false).await;
    agent.provider = Arc::new(Fable);
    let before = agent.session.messages.len();
    let mut attempts = 0;
    assert!(
        !agent
            .maybe_reconsider_fable_guardrail(Some("refusal"), &mut attempts)
            .unwrap()
    );
    assert_eq!(attempts, 0);
    assert_eq!(agent.session.messages.len(), before);
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    let registration = crate::instruction::notification::Notification::FableGuardrailFirst
        .registration()
        .unwrap();
    std::fs::write(
        home.root()
            .join("instructions")
            .join(registration.default_relative_path),
        format!(
            "---\nid: {}\nkind: notification\n---\nSYNTHETIC RECONSIDERATION",
            registration.id
        ),
    )
    .unwrap();
    agent.session.isolated_child = None;
    assert!(
        agent
            .maybe_reconsider_fable_guardrail(Some("refusal"), &mut attempts)
            .unwrap()
    );
    assert_eq!(attempts, 1);
    assert_eq!(agent.session.messages.len(), before + 1);
    crate::tool::clear_session_tool_policy(agent.session_id());
}

#[tokio::test]
async fn background_registry_keeps_original_permission_after_turn_change_and_cleanup() {
    let (home, mut agent, _, _) = fixture(false).await;
    let original = agent.registry.clone_with_shared_context_runtime();
    agent
        .prepare_isolated_turn(
            "permission-transition",
            "next",
            Some(Permission::ReadWrite),
            None,
        )
        .await
        .unwrap();
    crate::tool::clear_session_tool_policy(agent.session_id());
    let target = home.root().join("scratch/permission-check.txt");
    let context = |id: &str| ToolContext {
        session_id: agent.session_id().into(),
        message_id: "permission-snapshot".into(),
        tool_call_id: id.into(),
        working_dir: agent.working_dir().map(PathBuf::from),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: Default::default(),
    };
    assert!(
        original
            .execute(
                "write",
                serde_json::json!({"file_path":target,"content":"OLD TURN"}),
                context("old")
            )
            .await
            .is_err()
    );
    assert!(!target.exists());
    assert!(
        !agent
            .registry
            .execute(
                "write",
                serde_json::json!({"file_path":target,"content":"NEW TURN"}),
                context("new")
            )
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), "NEW TURN");
    assert!(
        original
            .execute(
                "schedule",
                serde_json::json!({"action":"list"}),
                context("admin")
            )
            .await
            .is_err()
    );
}
