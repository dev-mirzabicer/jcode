use super::AmbientRunnerHandle;
use crate::ambient::{Priority, ScheduleTarget, ScheduledItem};
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::session::Session;
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use jcode_session_types::StoredContextEmergencyPolicy;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

struct TestProvider;

#[derive(Clone, Default)]
struct StreamingTestProvider {
    responses: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
}

impl StreamingTestProvider {
    fn queue_response(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }
}

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "TestProvider should not be used for streaming completions in ambient runner tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

#[async_trait]
impl Provider for StreamingTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let stream = stream! {
            for event in events {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn runner_stays_alive_to_service_schedules_when_ambient_disabled() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    let task = tokio::spawn(runner.clone().run_loop(provider));

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        runner.is_running().await,
        "runner should remain active for scheduled tasks even with ambient disabled"
    );

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn spawn_target_creates_one_child_session_and_runs_task() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());

    let provider = StreamingTestProvider::default();
    provider.queue_response(vec![
        StreamEvent::TextDelta("Spawned session handled task.".to_string()),
        StreamEvent::MessageEnd { stop_reason: None },
    ]);
    let provider: Arc<dyn Provider> = Arc::new(provider);

    let mut parent = Session::create_with_id(
        "session_parent_spawn_test".to_string(),
        None,
        Some("Parent".to_string()),
    );
    parent.working_dir = Some(temp.path().display().to_string());
    parent.add_message(
        Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "historical scheduled-task context".to_string(),
            cache_control: None,
        }],
    );
    parent.context_view.emergency_policy = StoredContextEmergencyPolicy::Authorized {
        protected_recent_assistant_turns: 9,
        target_headroom_percent: 17,
        allow_reasoning_suppression: true,
        allow_tool_distillation: false,
        allow_oldest_range_summary: true,
        authorization_source: "parent-session-policy-must-not-transfer".to_string(),
    };
    parent.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "scheduled-task context summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });
    parent.save().expect("save parent session");
    let migrated_parent = Session::load(&parent.id).expect("migrate parent context");
    assert!(migrated_parent.compaction.is_none());
    assert_eq!(migrated_parent.context_view.active_transaction_count(), 1);

    let item = ScheduledItem {
        id: "sched_spawn_test".to_string(),
        scheduled_for: chrono::Utc::now(),
        context: "Follow up later".to_string(),
        priority: Priority::Normal,
        target: ScheduleTarget::Spawn {
            parent_session_id: parent.id.clone(),
        },
        created_by_session: parent.id.clone(),
        created_at: chrono::Utc::now(),
        working_dir: parent.working_dir.clone(),
        task_description: Some("Follow up later".to_string()),
        relevant_files: vec!["src/lib.rs".to_string()],
        git_branch: None,
        additional_context: Some("Background: spawned schedule test".to_string()),
        context_emergency_policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 4,
            target_headroom_percent: 12,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: "scheduled-item-test-policy".to_string(),
        },
    };

    let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    let notice_source = temp
        .path()
        .join("instructions/notifications/scheduled-task-due.md");
    let write_notice = |body: &str| {
        std::fs::write(
            &notice_source,
            format!(
                "---\nid: scheduled-task-due\nkind: notification\ntemplate: handlebars\n---\n{body}"
            ),
        )
        .unwrap()
    };
    write_notice("{{invalid}}");
    assert!(
        runner
            .spawn_session_for_scheduled_item(&provider, &item, &parent.id)
            .await
            .is_err()
    );
    for entry in std::fs::read_dir(temp.path().join("sessions")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_ne!(stored["parent_id"].as_str(), Some(parent.id.as_str()));
        }
    }
    write_notice("SYNTHETIC-DUE");
    let child_session_id = runner
        .spawn_session_for_scheduled_item(&provider, &item, &parent.id)
        .await
        .expect("spawned scheduled task should succeed");

    assert_ne!(child_session_id, parent.id);

    let child = Session::load(&child_session_id).expect("load spawned child session");
    assert!(
        child
            .messages
            .iter()
            .any(|message| message.content_preview().contains("SYNTHETIC-DUE"))
    );
    write_notice("LATER-DUE");
    assert!(
        crate::ambient::format_scheduled_session_message(&item)
            .unwrap()
            .contains("LATER-DUE")
    );
    assert!(
        child
            .messages
            .iter()
            .any(|message| message.content_preview().contains("SYNTHETIC-DUE"))
    );
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.working_dir, parent.working_dir);
    assert!(child.compaction.is_none());
    assert_eq!(
        child.context_view.transactions,
        migrated_parent.context_view.transactions
    );
    assert_eq!(
        child.context_view.emergency_policy,
        StoredContextEmergencyPolicy::Block
    );
    assert_eq!(
        migrated_parent.context_view.emergency_policy,
        parent.context_view.emergency_policy
    );
    assert!(child.messages.iter().any(|message| {
        message.role == Role::User
            && message.content_preview().contains("[Scheduled task]")
            && message.content_preview().contains("Follow up later")
    }));
    assert!(child.messages.iter().any(|message| {
        message.role == Role::Assistant
            && message
                .content_preview()
                .contains("Spawned session handled task.")
    }));
}

#[test]
#[cfg(unix)]
fn scheduled_live_delivery_uses_notify_without_a_provisional_subscription() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir()?;
    let endpoint = root.path().join("notify.sock");
    let _socket = EnvVarGuard::set_path("JCODE_SOCKET", &endpoint);
    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::UnixListener::bind(&endpoint)?;
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let crate::protocol::Request::PrimaryControlProbe { id } =
                crate::protocol::decode_request(&line).unwrap()
            else {
                panic!("delivery must negotiate its capability");
            };
            write
                .write_all(
                    crate::protocol::encode_event(
                        &crate::protocol::ServerEvent::PrimaryControlCapabilities {
                            id,
                            input_version: 1,
                            location_version: 1,
                            location_enabled: false,
                            legacy_adoption_version: None,
                            context_scope_version: None,
                            session_inspection_version: None,
                            session_placement_version: None,
                        },
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let request = crate::protocol::decode_request(&line).unwrap();
            let crate::protocol::Request::PrimaryInput { id, input } = request else {
                panic!("scheduled delivery must not construct a notifier Session")
            };
            assert_eq!(input.session, "detached-synthetic");
            assert_eq!(input.content, "scheduled adapter fixture");
            assert!(input.unattended_context.is_none());
            assert_eq!(
                input.id,
                crate::primary_input::correlated_input_id("scheduled-item", "fixture-schedule")
            );
            write
                .write_all(
                    crate::protocol::encode_event(
                        &crate::protocol::ServerEvent::PrimaryInputReceipt {
                            id,
                            receipt: jcode_session_types::PrimaryInputReceipt {
                                id: input.id,
                                session: input.session,
                                state: jcode_session_types::PrimaryInputState::Accepted,
                                messages: Vec::new(),
                                issue: None,
                            },
                        },
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        AmbientRunnerHandle::notify_live_session(
            "fixture-schedule",
            "detached-synthetic",
            "scheduled adapter fixture",
            None,
        )
        .await?;
        peer.await?;
        Ok(())
    })
}

/// After the managed rollout a spawned scheduled session must be able to
/// work: it takes its parent's placement and cwd like a Split, never carries
/// the parent's direct grants, and an unplaced or missing parent fails before
/// any child Session is created.
#[tokio::test]
async fn spawned_schedule_takes_parent_placement_without_direct_grants() -> Result<()> {
    use crate::workspace::{
        Audience, EntityId, GrantChange, OperationId, OrganizationChange, Placement, Registration,
        RequestId, WorkspaceClientAuthority, WorkspaceService, WriteTarget,
    };
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    std::fs::write(
        crate::config::Config::path().unwrap(),
        "[features]\nmanaged_primary_launch = true\n",
    )?;
    crate::config::Config::invalidate_cache();
    let result = async {
        let work = tempfile::tempdir()?;
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new())?;
        let mut roots = Vec::new();
        for name in ["a", "b"] {
            let path = work.path().join(name);
            std::fs::create_dir(&path)?;
            let path = path.canonicalize()?;
            let review = service.review_organization_change(
                service.status()?.revision,
                OrganizationChange::RegisterLocation {
                    name: name.into(),
                    path: path.clone(),
                    registration: Registration::Standalone,
                },
            )?;
            let EntityId::Location(id) = service
                .apply_organization_change(RequestId::new(), review.id)?
                .targets[0]
            else {
                panic!()
            };
            roots.push((id, path));
        }
        let mut parent = Session::create(None, None);
        let prepared = service.prepare_primary_location(
            Placement::Standalone(roots[0].0),
            Some(&roots[0].1),
            OperationId::new(),
        )?;
        parent.location = Some(prepared.location.clone());
        parent.working_dir = Some(roots[0].1.to_string_lossy().into());
        drop(prepared);
        parent.save()?;
        let review = service.review_grant_change(
            service.status()?.revision,
            GrantChange::Issue {
                audience: Audience::Session(parent.id.clone()),
                target: WriteTarget::Root(roots[1].0),
                proposal: None,
            },
        )?;
        service.apply_grant_change(
            &WorkspaceClientAuthority::authenticated("test-human")?,
            RequestId::new(),
            review.id,
        )?;

        let item = |parent_id: &str| ScheduledItem {
            id: format!("sched_placed_{parent_id}"),
            scheduled_for: chrono::Utc::now(),
            context: "SYNTHETIC SPAWN".to_string(),
            priority: Priority::Normal,
            target: ScheduleTarget::Spawn {
                parent_session_id: parent_id.to_string(),
            },
            created_by_session: parent_id.to_string(),
            created_at: chrono::Utc::now(),
            // A cwd recorded at scheduling time never overrides the staged
            // location of a placed child.
            working_dir: Some(roots[1].1.to_string_lossy().into()),
            task_description: Some("SYNTHETIC SPAWN".to_string()),
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
            context_emergency_policy: StoredContextEmergencyPolicy::Block,
        };
        let provider = StreamingTestProvider::default();
        provider.queue_response(vec![
            StreamEvent::TextDelta("Spawned placed child ran.".to_string()),
            StreamEvent::MessageEnd { stop_reason: None },
        ]);
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let runner = AmbientRunnerHandle::new(Arc::new(crate::safety::SafetySystem::new()));
        let child_id = runner
            .spawn_session_for_scheduled_item(&provider, &item(&parent.id), &parent.id)
            .await?;
        let child = Session::load(&child_id)?;
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
        let location = child.location.as_ref().expect("spawned child is placed");
        assert_eq!(location.placement, Placement::Standalone(roots[0].0));
        assert_eq!(location.cwd.observed_path(), roots[0].1.as_path());
        assert_eq!(child.working_dir, parent.working_dir);
        assert!(child.scope_copy.is_some_and(|copy| copy.ready));
        assert!(child.messages.iter().any(|message| {
            message.role == Role::Assistant
                && message
                    .content_preview()
                    .contains("Spawned placed child ran.")
        }));
        let scope = service.session_write_scope(&child)?;
        assert!(
            scope.grants.is_empty(),
            "an unattended spawn must not carry direct grants: {:?}",
            scope.grants
        );

        let children_of = |id: &str| -> Result<usize> {
            let mut count = 0;
            for entry in std::fs::read_dir(crate::storage::jcode_dir()?.join("sessions"))? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
                    count += usize::from(stored["parent_id"].as_str() == Some(id));
                }
            }
            Ok(count)
        };
        let mut unplaced = Session::create(None, None);
        unplaced.working_dir = Some(roots[0].1.to_string_lossy().into());
        unplaced.save()?;
        let refused = runner
            .spawn_session_for_scheduled_item(&provider, &item(&unplaced.id), &unplaced.id)
            .await
            .expect_err("an unplaced parent cannot spawn after rollout");
        assert!(format!("{refused:#}").contains("needs a placed parent session"));
        assert_eq!(children_of(&unplaced.id)?, 0);
        let missing = "session_missing_spawn_parent";
        assert!(
            runner
                .spawn_session_for_scheduled_item(&provider, &item(missing), missing)
                .await
                .is_err()
        );
        assert_eq!(children_of(missing)?, 0);
        Ok(())
    }
    .await;
    std::fs::remove_file(crate::config::Config::path().unwrap()).ok();
    crate::config::Config::invalidate_cache();
    result
}
