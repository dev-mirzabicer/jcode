//! A session's advertised tool set (INT-01/WP-06, D15).
//!
//! The definitions the first request advertised are frozen and persisted
//! with the session, so a reload, a restart or a remote reattach sends the
//! same tool bytes. Every later change (a tool added, removed or redefined;
//! an MCP server connecting or going away; a binary whose tool text
//! differs) is recorded here and announced to the model once, as an appended
//! notice. What a provider's `tools` array holds follows from this record:
//!
//! - a provider that takes tool changes inside a message keeps the frozen
//!   array and receives every change in the notice;
//! - every other provider gets [`StoredToolSet::array`]: additions and schema
//!   changes join the array (a recorded tool-set transition), a description
//!   change keeps the first-sent bytes (the notice carries the new text), and
//!   a removed tool's definition stays (calling it reports that it is not
//!   available).

use jcode_message_types::{ToolDefinition, ToolSetChange};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The frozen tool set of a session and the changes announced since.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredToolSet {
    /// The definitions the first request advertised, in that order.
    pub advertised: Vec<ToolDefinition>,
    /// Every change announced since, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<StoredToolSetChange>,
}

/// One announced change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredToolSetChange {
    pub change: ToolSetChange,
    /// For a redefinition: whether the input schema changed, and not only the
    /// description.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub schema_changed: bool,
}

/// Where a tool name stands in a session's set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAvailability {
    /// Offered to the model now.
    Offered,
    /// Announced as removed.
    Removed,
    /// Never part of the set.
    Unknown,
}

impl StoredToolSet {
    pub fn new(advertised: Vec<ToolDefinition>) -> Self {
        Self {
            advertised,
            changes: Vec::new(),
        }
    }

    /// Each name's current definition, or `None` once it was removed, with
    /// names in the order they were first advertised or added.
    fn current(&self) -> Vec<(String, Option<ToolDefinition>)> {
        let mut order: Vec<String> = self.advertised.iter().map(|t| t.name.clone()).collect();
        let mut state: BTreeMap<String, Option<ToolDefinition>> = self
            .advertised
            .iter()
            .map(|tool| (tool.name.clone(), Some(tool.clone())))
            .collect();
        for StoredToolSetChange { change, .. } in &self.changes {
            let name = change.name().to_string();
            if !state.contains_key(&name) {
                order.push(name.clone());
            }
            state.insert(
                name,
                match change {
                    ToolSetChange::Added { definition }
                    | ToolSetChange::Redefined { definition } => Some(definition.clone()),
                    ToolSetChange::Removed { .. } => None,
                },
            );
        }
        order
            .into_iter()
            .map(|name| {
                let definition = state.remove(&name).flatten();
                (name, definition)
            })
            .collect()
    }

    /// The tools the model is offered now.
    pub fn effective(&self) -> Vec<ToolDefinition> {
        self.current()
            .into_iter()
            .filter_map(|(_, definition)| definition)
            .collect()
    }

    /// Where `name` stands.
    pub fn availability(&self, name: &str) -> ToolAvailability {
        match self.current().into_iter().find(|(known, _)| known == name) {
            Some((_, Some(_))) => ToolAvailability::Offered,
            Some((_, None)) => ToolAvailability::Removed,
            None => ToolAvailability::Unknown,
        }
    }

    /// The `tools` array for a provider that cannot take tool changes inside
    /// a message: the frozen array, with additions and schema changes
    /// applied in place. Descriptions keep their first-sent bytes and removed
    /// tools keep their definitions.
    pub fn array(&self) -> Vec<ToolDefinition> {
        let mut array = self.advertised.clone();
        for StoredToolSetChange {
            change,
            schema_changed,
        } in &self.changes
        {
            match change {
                ToolSetChange::Added { definition } => {
                    match array.iter_mut().find(|tool| tool.name == definition.name) {
                        Some(existing) => *existing = definition.clone(),
                        None => array.push(definition.clone()),
                    }
                }
                ToolSetChange::Redefined { definition } if *schema_changed => {
                    if let Some(existing) =
                        array.iter_mut().find(|tool| tool.name == definition.name)
                    {
                        *existing = definition.clone();
                    }
                }
                ToolSetChange::Redefined { .. } | ToolSetChange::Removed { .. } => {}
            }
        }
        array
    }

    /// How `live` differs from the tools offered now, as changes to announce.
    /// A tool missing from `live` for which `held` is true (an MCP tool whose
    /// server is still reconnecting) is not a removal.
    pub fn diff(
        &self,
        live: &[ToolDefinition],
        held: impl Fn(&str) -> bool,
    ) -> Vec<StoredToolSetChange> {
        let offered = self.effective();
        let mut changes = Vec::new();
        for tool in &offered {
            match live.iter().find(|candidate| candidate.name == tool.name) {
                None if held(&tool.name) => {}
                None => changes.push(StoredToolSetChange {
                    change: ToolSetChange::Removed {
                        name: tool.name.clone(),
                    },
                    schema_changed: false,
                }),
                Some(candidate) if candidate != tool => changes.push(StoredToolSetChange {
                    change: ToolSetChange::Redefined {
                        definition: candidate.clone(),
                    },
                    schema_changed: candidate.input_schema != tool.input_schema,
                }),
                Some(_) => {}
            }
        }
        for candidate in live {
            if !offered.iter().any(|tool| tool.name == candidate.name) {
                changes.push(StoredToolSetChange {
                    change: ToolSetChange::Added {
                        definition: candidate.clone(),
                    },
                    schema_changed: false,
                });
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, description: &str, property: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: json!({"type": "object", "properties": {property: {"type": "string"}}}),
        }
    }

    fn names(tools: &[ToolDefinition]) -> Vec<&str> {
        tools.iter().map(|tool| tool.name.as_str()).collect()
    }

    #[test]
    fn an_unchanged_registry_changes_nothing() {
        let set = StoredToolSet::new(vec![tool("bash", "Run", "c"), tool("read", "Read", "p")]);
        assert!(set.diff(&set.advertised.clone(), |_| false).is_empty());
        assert_eq!(set.array(), set.advertised);
        assert_eq!(set.effective(), set.advertised);
    }

    #[test]
    fn changes_are_classified_and_each_provider_array_follows_its_rule() {
        let mut set = StoredToolSet::new(vec![
            tool("bash", "Run", "c"),
            tool("read", "Read", "p"),
            tool("edit", "Edit", "p"),
        ]);
        let live = vec![
            tool("bash", "Run (revised)", "c"),
            tool("edit", "Edit", "path"),
            tool("probe", "Probe", "x"),
        ];
        let changes = set.diff(&live, |_| false);
        assert_eq!(changes.len(), 4);
        assert!(
            matches!(&changes[0].change, ToolSetChange::Redefined { definition } if definition.name == "bash")
        );
        assert!(!changes[0].schema_changed);
        assert!(matches!(&changes[1].change, ToolSetChange::Removed { name } if name == "read"));
        assert!(
            matches!(&changes[2].change, ToolSetChange::Redefined { definition } if definition.name == "edit")
        );
        assert!(changes[2].schema_changed);
        assert!(
            matches!(&changes[3].change, ToolSetChange::Added { definition } if definition.name == "probe")
        );
        set.changes.extend(changes);

        assert_eq!(set.effective(), live);
        // The provider array: first-sent description, new schema, the removed
        // definition kept, the addition appended.
        let array = set.array();
        assert_eq!(names(&array), vec!["bash", "read", "edit", "probe"]);
        assert_eq!(array[0].description, "Run");
        assert_eq!(array[2], live[1]);
        assert_eq!(set.availability("read"), ToolAvailability::Removed);
        assert_eq!(set.availability("probe"), ToolAvailability::Offered);
        assert_eq!(set.availability("other"), ToolAvailability::Unknown);
        // Recorded once: the same registry again changes nothing.
        assert!(set.diff(&live, |_| false).is_empty());
    }

    #[test]
    fn a_held_tool_is_not_removed_and_a_returning_tool_is_added_back() {
        let mut set =
            StoredToolSet::new(vec![tool("bash", "Run", "c"), tool("mcp__s__q", "Q", "x")]);
        let without = vec![tool("bash", "Run", "c")];
        assert!(
            set.diff(&without, |name| name.starts_with("mcp__"))
                .is_empty()
        );
        let removal = set.diff(&without, |_| false);
        set.changes.extend(removal);
        assert_eq!(names(&set.effective()), vec!["bash"]);
        let back = set.diff(
            &[tool("bash", "Run", "c"), tool("mcp__s__q", "Q2", "x")],
            |_| false,
        );
        assert!(matches!(&back[0].change, ToolSetChange::Added { .. }));
        set.changes.extend(back);
        assert_eq!(set.array()[1].description, "Q2");
        assert_eq!(set.availability("mcp__s__q"), ToolAvailability::Offered);
    }

    #[test]
    fn the_record_round_trips() {
        let mut set = StoredToolSet::new(vec![tool("bash", "Run", "c")]);
        set.changes
            .extend(set.diff(&[tool("bash", "Run", "x")], |_| false));
        let decoded: StoredToolSet =
            serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(decoded, set);
    }
}
