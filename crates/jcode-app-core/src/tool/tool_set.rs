//! A session's tool set across its whole life (INT-01/WP-06, D15).
//!
//! Tools render first in every provider prefix, and Claude binds each signed
//! thinking block to the tool set as well as to the messages before it. The
//! set a session's first request advertised is therefore frozen and persisted
//! with the session (`Session::tool_set`), so a reload, a restart or a remote
//! reattach sends the same bytes.
//!
//! Before every request the record is compared with the live registry. A
//! difference (a tool added, removed or redefined; an MCP server that
//! connected or went away; a binary whose tool text changed) is recorded and
//! announced once, as an appended operator delivery that carries the changes
//! structurally. What the provider's `tools` array holds follows from the
//! record and the provider:
//!
//! - a provider that applies tool changes inside a message
//!   (`Provider::renders_tool_changes`) keeps the frozen array; the notice
//!   carries every change to it;
//! - every other provider gets `StoredToolSet::array`: an addition or a schema
//!   change joins the array, which is a recorded tool-set transition; a
//!   description change keeps the first-sent bytes and the notice carries
//!   the new text; a removed tool keeps its definition and a call to it
//!   reports that it is not available.
//!
//! A provider that takes changes inside a message sees only the changes whose
//! notices its projected history still holds. A context summary or a rewind
//! that covers a notice would undo its change for the model, so the plan
//! announces the difference from the state in view ([`tool_changes_in_view`]):
//! such a change is announced again, appended.
//!
//! An MCP tool missing while its servers are still connecting after a start
//! is kept, and a call to it reports that it is reconnecting. Clear and a new
//! session start without a record and freeze a new set at their first
//! request.

use crate::message::{ContentBlock, Message, Role, ToolDefinition};
use jcode_message_types::ToolSetChange;
use jcode_session_types::{
    ContextDeliveryChannel, StoredMessage, StoredToolSet, StoredToolSetChange, ToolAvailability,
};
use std::collections::HashMap;

use super::UnavailableTool;

/// The label of a tool-set transition in the cache-invalidation journal and
/// as the cause of reasoning it invalidates.
pub const TOOL_SET_TRANSITION: &str = "tool set change";

/// What the next request carries, from the session's record and the live
/// registry.
#[derive(Debug)]
pub struct ToolSetPlan {
    /// The record after this request: frozen now if there was none, with
    /// any new changes appended.
    pub record: StoredToolSet,
    /// The `tools` the request carries.
    pub tools: Vec<ToolDefinition>,
    /// Changes to announce before the request, in order.
    pub announced: Vec<StoredToolSetChange>,
    /// The provider's `tools` array differs from the previous request's.
    pub array_changed: bool,
    /// Tools the model was offered that cannot run now, and why.
    pub unavailable: HashMap<String, UnavailableTool>,
}

/// Plan the next request's tool set.
///
/// `inline` says whether the provider takes tool changes inside a message;
/// `in_view` is then the changes its projected history still carries
/// ([`tool_changes_in_view`]). `mcp_connecting` holds back the removal of MCP
/// tools whose servers have not registered yet.
pub fn plan_tool_set(
    record: Option<&StoredToolSet>,
    live: Vec<ToolDefinition>,
    inline: Option<&[ToolSetChange]>,
    mcp_connecting: bool,
) -> ToolSetPlan {
    let held = |name: &str| mcp_connecting && name.starts_with("mcp__");
    let sent = |record: &StoredToolSet| recorded_tools(record, inline.is_some());
    let (record, announced, array_changed) = match record {
        None => (StoredToolSet::new(live.clone()), Vec::new(), false),
        Some(record) => {
            let changed = record.diff(&live, held);
            let announced = match inline {
                // What the model sees comes from the notices in its history.
                Some(in_view) => StoredToolSet {
                    advertised: record.advertised.clone(),
                    changes: in_view
                        .iter()
                        .map(|change| StoredToolSetChange {
                            change: change.clone(),
                            schema_changed: false,
                        })
                        .collect(),
                }
                .diff(&live, |name| {
                    held(name) && record.availability(name) == ToolAvailability::Offered
                }),
                None => changed.clone(),
            };
            let mut next = record.clone();
            next.changes.extend(changed);
            let array_changed = sent(record) != sent(&next);
            (next, announced, array_changed)
        }
    };
    let mut unavailable = HashMap::new();
    for tool in sent(&record) {
        if live.iter().any(|candidate| candidate.name == tool.name) {
            continue;
        }
        let state = match record.availability(&tool.name) {
            ToolAvailability::Offered if held(&tool.name) => UnavailableTool::Reconnecting,
            _ => UnavailableTool::Removed,
        };
        unavailable.insert(tool.name, state);
    }
    ToolSetPlan {
        tools: sent(&record),
        record,
        announced,
        array_changed,
        unavailable,
    }
}

/// The tools a request carries under a session's record, without comparing
/// it with the registry.
///
/// A removed tool keeps its definition, except a globally unavailable one
/// (Swarm while it is disabled), which is never advertised (Phase 4).
pub fn recorded_tools(record: &StoredToolSet, inline: bool) -> Vec<ToolDefinition> {
    let mut tools = if inline {
        record.advertised.clone()
    } else {
        record.array()
    };
    tools.retain(|tool| {
        super::tool_is_globally_available(&tool.name)
            || record.availability(&tool.name) != ToolAvailability::Removed
    });
    tools
}

/// The announced changes a provider's projected history still carries, in
/// order: those of the session's tool-set deliveries whose exact text is
/// still a message of `projected`. Each notice's text is unique in its
/// session ([`tool_set_notice`]).
pub fn tool_changes_in_view(stored: &[StoredMessage], projected: &[Message]) -> Vec<ToolSetChange> {
    let deliveries: HashMap<&str, &[ToolSetChange]> = stored
        .iter()
        .filter_map(StoredMessage::operator_delivery)
        .filter(|delivery| !delivery.tool_changes.is_empty())
        .map(|delivery| (delivery.text, delivery.tool_changes))
        .collect();
    if deliveries.is_empty() {
        return Vec::new();
    }
    projected
        .iter()
        .filter(|message| message.role == Role::User)
        .filter_map(|message| match message.content.as_slice() {
            [ContentBlock::Text { text, .. }] => deliveries.get(text.as_str()),
            _ => None,
        })
        .flat_map(|changes| changes.iter().cloned())
        .collect()
}

/// How many tool-set notices a session's history holds.
pub fn tool_set_notice_count(stored: &[StoredMessage]) -> usize {
    stored
        .iter()
        .filter(|message| {
            message
                .context_delivery()
                .is_some_and(|(channel, _)| channel == ContextDeliveryChannel::ToolSet)
        })
        .count()
}

/// The text of a tool-set notice. It states the facts for every provider: a
/// provider that applies changes inside a message also receives them
/// structurally, and every other provider has additions and schema changes in
/// its tool array. `sequence` numbers the session's notices from 1, which
/// also keeps each notice's text unique in its session.
pub fn tool_set_notice(sequence: usize, changes: &[StoredToolSetChange]) -> String {
    let mut lines = vec![
        format!("# Tool set changed (update {sequence})"),
        String::new(),
    ];
    for StoredToolSetChange {
        change,
        schema_changed,
    } in changes
    {
        lines.push(match change {
            ToolSetChange::Added { definition } => {
                format!("- Added `{}`.", definition.name)
            }
            ToolSetChange::Redefined { definition } => format!(
                "- Changed `{}`{}. Its description is now:\n\n{}\n",
                definition.name,
                if *schema_changed {
                    " (its input changed too)"
                } else {
                    ""
                },
                definition.description.trim()
            ),
            ToolSetChange::Removed { name } => {
                format!("- Removed `{name}`. It is no longer available.")
            }
        });
    }
    lines.join("\n").trim_end().to_string()
}

#[cfg(test)]
#[path = "tool_set_tests.rs"]
mod tests;
