//! The tool-set lifetime owner (INT-01 WP-05).

use super::*;
use crate::tool::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Named(&'static str);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "fixture"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn decode_input(&self, _: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
    async fn execute(&self, _: serde_json::Value, _: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

async fn register(registry: &Registry, name: &'static str) {
    registry
        .register(name.to_string(), Arc::new(Named(name)))
        .await;
}

/// Resolve with a builder that reads the registry and counts its calls.
async fn resolve(
    lock: &mut ToolSetLock,
    registry: &Registry,
    builds: &AtomicUsize,
) -> ResolvedToolSet {
    lock.resolve(registry, &ToolSetFilters::none(), || async {
        builds.fetch_add(1, Ordering::SeqCst);
        Ok(registry.definitions(None).await)
    })
    .await
    .expect("resolve")
}

fn names(tools: &[ToolDefinition]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

#[tokio::test]
async fn the_first_request_locks_the_set_and_later_ones_reuse_it() {
    let registry = Registry::empty();
    register(&registry, "read").await;
    let builds = AtomicUsize::new(0);
    let mut lock = ToolSetLock::default();

    let first = resolve(&mut lock, &registry, &builds).await;
    register(&registry, "added_later").await;
    let second = resolve(&mut lock, &registry, &builds).await;

    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(names(&second.tools), vec!["read"]);
    assert!(first.transitions.is_empty() && second.transitions.is_empty());
    assert!(lock.locked().is_some());
}

#[tokio::test]
async fn late_mcp_tools_join_once_as_a_named_transition() {
    let registry = Registry::empty();
    register(&registry, "read").await;
    let builds = AtomicUsize::new(0);
    let mut lock = ToolSetLock::default();
    resolve(&mut lock, &registry, &builds).await;

    register(&registry, "mcp__server__first").await;
    let joined = resolve(&mut lock, &registry, &builds).await;
    assert_eq!(names(&joined.tools), vec!["mcp__server__first", "read"]);
    assert_eq!(
        joined.transitions,
        vec![ToolSetTransition::LateMcpRegistration]
    );

    // A later wave waits for an explicit `mcp` release (#206 follow-up).
    register(&registry, "mcp__server__second").await;
    let stable = resolve(&mut lock, &registry, &builds).await;
    assert!(stable.transitions.is_empty());
    assert_eq!(names(&stable.tools), names(&joined.tools));
    assert_eq!(builds.load(Ordering::SeqCst), 2);

    assert!(lock.release_after_mcp_management());
    let reloaded = resolve(&mut lock, &registry, &builds).await;
    assert!(names(&reloaded.tools).contains(&"mcp__server__second"));
    assert!(
        reloaded.transitions.is_empty(),
        "the release itself is the recorded transition"
    );
}

#[tokio::test]
async fn mcp_tools_the_session_excludes_do_not_rebuild_the_set() {
    let registry = Registry::empty();
    register(&registry, "read").await;
    let builds = AtomicUsize::new(0);
    let mut lock = ToolSetLock::default();
    resolve(&mut lock, &registry, &builds).await;
    register(&registry, "mcp__server__blocked").await;

    let disabled: HashSet<String> = ["mcp__server__blocked".to_string()].into();
    let filters = ToolSetFilters {
        allowed: None,
        disabled: &disabled,
    };
    let resolved = lock
        .resolve(&registry, &filters, || async {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(registry.definitions(None).await)
        })
        .await
        .expect("resolve");
    assert!(resolved.transitions.is_empty());
    assert_eq!(names(&resolved.tools), vec!["read"]);
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_reset_locks_a_new_set_at_the_next_request() {
    let registry = Registry::empty();
    register(&registry, "read").await;
    let builds = AtomicUsize::new(0);
    let mut lock = ToolSetLock::default();
    resolve(&mut lock, &registry, &builds).await;
    register(&registry, "write").await;

    lock.reset();
    assert!(lock.locked().is_none());
    let renewed = resolve(&mut lock, &registry, &builds).await;
    assert_eq!(names(&renewed.tools), vec!["read", "write"]);
    assert!(
        renewed.transitions.is_empty(),
        "a new history is not a transition"
    );
}

#[test]
fn a_failed_build_leaves_no_lock() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let registry = Registry::empty();
        let mut lock = ToolSetLock::default();
        let error = lock
            .resolve(&registry, &ToolSetFilters::none(), || async {
                anyhow::bail!("guidance failed")
            })
            .await;
        assert!(error.is_err());
        assert!(lock.locked().is_none());
    });
}

#[test]
fn transitions_have_stable_labels() {
    assert_eq!(
        ToolSetTransition::LateMcpRegistration.label(),
        "late MCP tool registration"
    );
    assert_eq!(
        ToolSetTransition::McpManagement.label(),
        "MCP tool set reload"
    );
    assert_eq!(
        ToolSetTransition::ToolUnavailable.label(),
        "Swarm globally disabled"
    );
}
