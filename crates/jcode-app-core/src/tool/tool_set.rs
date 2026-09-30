//! The lifetime of the tool set a session's provider requests carry.
//!
//! Tools render first in every provider prefix, and Claude binds each signed
//! thinking block to the tool set as well as to the messages before it. A
//! changed set therefore breaks the whole prompt cache on every provider and
//! invalidates all earlier Claude thinking. The set is locked at a session's
//! first request and changes only at a named [`ToolSetTransition`] (INT-01
//! INV-1, GROUNDING H1).
//!
//! What keeps the set: changes to history (context-control transitions,
//! rewind and its undo, tool-output repair, legacy migration) and provider or
//! model switches. The tool surface depends on neither. What renews it: a new
//! provider history, that is a session change or clear ([`ToolSetLock::reset`]),
//! and the transitions below. Registry definitions are bound once per tool
//! name, so rebuilding over unchanged membership reproduces the same bytes;
//! membership changes only through MCP registration and the `mcp` tool.

use std::collections::HashSet;

use super::{Registry, tool_is_globally_available, tool_name_is_allowed, tool_name_is_disabled};
use crate::message::ToolDefinition;

/// An intentional change to a session's tool set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSetTransition {
    /// MCP server tools registered after the set was locked. Servers connect
    /// in the background so the first turn is never blocked (#206); their
    /// tools join the set once, when they appear.
    LateMcpRegistration,
    /// The `mcp` tool connected, disconnected or reloaded servers.
    McpManagement,
    /// A tool in the set became globally unavailable (Swarm disabled).
    ToolUnavailable,
}

impl ToolSetTransition {
    /// The label recorded in the cache-invalidation journal and as the cause
    /// of any replayed reasoning the change invalidates.
    pub fn label(self) -> &'static str {
        match self {
            Self::LateMcpRegistration => "late MCP tool registration",
            Self::McpManagement => "MCP tool set reload",
            Self::ToolUnavailable => "Swarm globally disabled",
        }
    }
}

/// The session filters a late MCP registration is checked against.
pub struct ToolSetFilters<'a> {
    pub allowed: Option<&'a HashSet<String>>,
    pub disabled: &'a HashSet<String>,
}

impl ToolSetFilters<'_> {
    pub fn none() -> ToolSetFilters<'static> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        ToolSetFilters {
            allowed: None,
            disabled: &EMPTY,
        }
    }

    fn admits(&self, name: &str) -> bool {
        self.allowed
            .is_none_or(|allowed| tool_name_is_allowed(allowed, name))
            && !tool_name_is_disabled(self.disabled, name)
    }
}

/// The tools the next request carries, with the transitions that changed
/// them since the previous request.
pub struct ResolvedToolSet {
    pub tools: Vec<ToolDefinition>,
    pub transitions: Vec<ToolSetTransition>,
}

/// The only owner of a session's locked tool set.
#[derive(Clone, Debug, Default)]
pub struct ToolSetLock {
    locked: Option<Vec<ToolDefinition>>,
    /// Whether late MCP registration can no longer change the locked set:
    /// its tools already joined, or the `mcp` tool has not re-armed it since.
    late_mcp_settled: bool,
}

impl ToolSetLock {
    /// The locked set, when a request has locked one.
    pub fn locked(&self) -> Option<&[ToolDefinition]> {
        self.locked.as_deref()
    }

    /// The set the next request carries: the locked set, after any recorded
    /// transition, or a new set from `build` when none is locked.
    pub async fn resolve<F, Fut>(
        &mut self,
        registry: &Registry,
        filters: &ToolSetFilters<'_>,
        build: F,
    ) -> anyhow::Result<ResolvedToolSet>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Vec<ToolDefinition>>>,
    {
        let mut transitions = Vec::new();
        if let Some(locked) = self.locked.as_mut() {
            let before = locked.len();
            locked.retain(|tool| tool_is_globally_available(&tool.name));
            if locked.len() != before {
                transitions.push(ToolSetTransition::ToolUnavailable);
            }
        }
        if let Some(locked) = &self.locked {
            if self.late_mcp_settled || !has_new_mcp_tools(registry, locked, filters).await {
                return Ok(ResolvedToolSet {
                    tools: locked.clone(),
                    transitions,
                });
            }
            crate::logging::info(
                "MCP tools registered after the tool set was locked; rebuilding it once to \
                 expose them (one intentional prompt-cache transition, #206)",
            );
            self.late_mcp_settled = true;
            self.locked = None;
            transitions.push(ToolSetTransition::LateMcpRegistration);
        }
        let tools = build().await?;
        crate::logging::info(&format!(
            "Locking tool list at {} tools for cache stability",
            tools.len()
        ));
        self.locked = Some(tools.clone());
        Ok(ResolvedToolSet { tools, transitions })
    }

    /// The `mcp` tool may have changed registry membership: the next request
    /// rebuilds the set, and a later MCP registration may join it again.
    /// Returns whether a locked set was released.
    pub fn release_after_mcp_management(&mut self) -> bool {
        self.late_mcp_settled = false;
        self.locked.take().is_some()
    }

    /// A new provider history (session change or clear) locks a new set at
    /// its first request.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A lock in a given state, for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn from_parts(locked: Option<Vec<ToolDefinition>>, late_mcp_settled: bool) -> Self {
        Self {
            locked,
            late_mcp_settled,
        }
    }

    #[cfg(test)]
    pub(crate) fn late_mcp_settled(&self) -> bool {
        self.late_mcp_settled
    }
}

/// Whether the registry holds MCP tools the session admits that `locked`
/// lacks.
async fn has_new_mcp_tools(
    registry: &Registry,
    locked: &[ToolDefinition],
    filters: &ToolSetFilters<'_>,
) -> bool {
    registry.tool_names().await.iter().any(|name| {
        name.starts_with("mcp__")
            && filters.admits(name)
            && !locked.iter().any(|tool| &tool.name == name)
    })
}

#[cfg(test)]
#[path = "tool_set_tests.rs"]
mod tests;
