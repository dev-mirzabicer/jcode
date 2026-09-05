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
