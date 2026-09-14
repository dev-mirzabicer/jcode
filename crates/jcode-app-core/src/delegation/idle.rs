//! Bounded idle runtime cache. Session remains the durable conversation owner.
//! This never publishes a second transcript or expires a child conversation.
use super::*;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

struct IdleRuntime {
    agent: Agent,
    used: Instant,
}
static IDLE: LazyLock<Mutex<HashMap<(PathBuf, String), IdleRuntime>>> =
    LazyLock::new(Default::default);

pub(super) fn take(root: &Path, id: &str) -> Option<Agent> {
    IDLE.lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&(root.into(), id.into()))
        .map(|entry| entry.agent)
}

pub(super) fn put(root: &Path, agent: Agent) {
    let mut idle = IDLE.lock().unwrap_or_else(|p| p.into_inner());
    let key = (root.to_path_buf(), agent.session_id().to_string());
    idle.insert(
        key,
        IdleRuntime {
            agent,
            used: Instant::now(),
        },
    );
    // This is a memory/process-cache bound, not admission or a conversation TTL.
    // Active/FIFO execution remains owned by ExecutionStore and is never queued
    // for cache space. Eviction restores exact Session state on the next call.
    let limit = crate::config::config()
        .delegation
        .max_running_children
        .get();
    let mut candidates = idle
        .iter()
        .filter(|((namespace, _), _)| namespace == root)
        .map(|(key, value)| (value.used, key.clone()))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(used, _)| *used);
    let excess = candidates.len().saturating_sub(limit);
    for (_, key) in candidates.into_iter().take(excess) {
        idle.remove(&key);
        crate::tool::clear_session_tool_policy(&key.1);
    }
}

pub(super) fn clear(root: &Path) {
    let mut idle = IDLE.lock().unwrap_or_else(|p| p.into_inner());
    idle.retain(|(namespace, id), _| {
        if namespace == root {
            crate::tool::clear_session_tool_policy(id);
            false
        } else {
            true
        }
    });
}
