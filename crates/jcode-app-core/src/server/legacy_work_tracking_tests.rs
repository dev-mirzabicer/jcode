use super::swarm_retirement::{retirement_connection, retirement_terminal};
use super::*;

#[tokio::test]
async fn legacy_work_tracking_protocol_rejects_retired_workflows_before_sessions_or_models() {
    use crate::protocol::Request;
    use crate::workflow::{
        CommandWorkflow as C, WorkflowLoopMode as M, WorkflowPromptRequest as W,
    };
    use tokio::io::AsyncWriteExt;
    let _lock = crate::storage::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _env = configure_test_env(&root);
    let _off = crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(false);
    let provider = Arc::new(StreamingMockProvider::default());
    let server = Server::new(provider.clone());
    let retired = [
        C::Commit,
        C::CommitPush,
        C::ReleaseFast,
        C::ReleaseMacos,
        C::ReleaseRemote,
        C::Test { claim: "x".into() },
        C::Plan {
            goal: Some("x".into()),
        },
        C::Improve {
            plan_only: true,
            focus: None,
        },
        C::ImproveStop,
        C::ImproveResume {
            mode: M::ImprovePlan,
            todos: vec![],
        },
    ];
    for command in retired {
        let workflow = W::Command { command };
        for request in [
            Request::RenderWorkflowPrompt {
                id: 1,
                workflow: workflow.clone(),
            },
            Request::SplitWithWorkflow {
                id: 1,
                workflow: workflow.clone(),
            },
        ] {
            let (client, task) = retirement_connection(&server).await;
            let (reader, mut writer) = client.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            writer
                .write_all((serde_json::to_string(&request).unwrap() + "\n").as_bytes())
                .await
                .unwrap();
            match (request, retirement_terminal(&mut reader, 1).await) {
                (
                    Request::SplitWithWorkflow { .. },
                    ServerEvent::WorkflowSplitFailed { message, .. },
                )
                | (Request::RenderWorkflowPrompt { .. }, ServerEvent::Error { message, .. }) => {
                    assert_eq!(message, crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE)
                }
                other => panic!("unexpected workflow result: {other:?}"),
            }
            drop(writer);
            task.await.unwrap().unwrap();
        }
    }
    assert!(server.sessions.read().await.is_empty());
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(!root.path().join("home/goals").exists());
}
