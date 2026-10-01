//! The tool-set planner (INT-01/WP-06, D15).

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
fn the_first_request_freezes_the_set_and_an_unchanged_registry_keeps_it() {
    let live = vec![tool("bash", "Run", "c"), tool("read", "Read", "p")];
    for inline in [Some(&[][..]), None] {
        let first = plan_tool_set(None, live.clone(), inline, false);
        assert_eq!(first.tools, live);
        assert!(first.announced.is_empty() && !first.array_changed);
        let again = plan_tool_set(Some(&first.record), live.clone(), inline, false);
        assert_eq!(again.tools, live);
        assert!(again.announced.is_empty() && !again.array_changed);
        assert_eq!(again.record, first.record);
    }
}

#[test]
fn each_change_is_announced_once_and_the_array_follows_the_provider() {
    let frozen = plan_tool_set(
        None,
        vec![tool("bash", "Run", "c"), tool("read", "Read", "p")],
        Some(&[]),
        false,
    )
    .record;
    let live = vec![
        tool("bash", "Run (revised)", "c"),
        tool("probe", "Probe", "x"),
    ];

    // In-message changes: the array never changes.
    let inline = plan_tool_set(Some(&frozen), live.clone(), Some(&[]), false);
    assert_eq!(inline.announced.len(), 3);
    assert_eq!(inline.tools, frozen.advertised);
    assert!(!inline.array_changed);
    assert_eq!(
        inline.unavailable.get("read"),
        Some(&UnavailableTool::Removed)
    );

    // Array changes: the addition joins; the description keeps its bytes.
    let array = plan_tool_set(Some(&frozen), live.clone(), None, false);
    assert_eq!(names(&array.tools), vec!["bash", "read", "probe"]);
    assert_eq!(array.tools[0].description, "Run");
    assert!(array.array_changed);

    // Recorded once.
    let after = plan_tool_set(Some(&array.record), live, None, false);
    assert!(after.announced.is_empty() && !after.array_changed);

    // The notice names every change, and its text is unique per update.
    let notice = tool_set_notice(1, &inline.announced);
    for name in ["bash", "read", "probe"] {
        assert!(notice.contains(&format!("`{name}`")), "{notice}");
    }
    assert_ne!(notice, tool_set_notice(2, &inline.announced));
}

#[test]
fn a_description_change_alone_leaves_every_array_unchanged() {
    let frozen = StoredToolSet::new(vec![tool("bash", "Run", "c")]);
    for inline in [Some(&[][..]), None] {
        let plan = plan_tool_set(
            Some(&frozen),
            vec![tool("bash", "Run!", "c")],
            inline,
            false,
        );
        assert_eq!(plan.announced.len(), 1);
        assert!(!plan.array_changed);
        assert_eq!(plan.tools, frozen.advertised);
    }
    // A schema change moves the array where tools are not changed in-message.
    let plan = plan_tool_set(Some(&frozen), vec![tool("bash", "Run", "x")], None, false);
    assert!(plan.array_changed);
    assert!(plan.announced[0].schema_changed);
}

#[test]
fn mcp_tools_wait_for_their_servers_after_a_start() {
    let frozen = StoredToolSet::new(vec![tool("bash", "Run", "c"), tool("mcp__s__q", "Q", "x")]);
    let starting = vec![tool("bash", "Run", "c")];
    let plan = plan_tool_set(Some(&frozen), starting.clone(), Some(&[]), true);
    assert!(plan.announced.is_empty());
    assert_eq!(plan.tools, frozen.advertised);
    assert_eq!(
        plan.unavailable.get("mcp__s__q"),
        Some(&UnavailableTool::Reconnecting)
    );
    // Reconnected: nothing to announce, the same bytes, nothing unavailable.
    let plan = plan_tool_set(Some(&frozen), frozen.advertised.clone(), Some(&[]), false);
    assert!(plan.announced.is_empty() && plan.unavailable.is_empty());
    // Settled without it: a removal.
    let plan = plan_tool_set(Some(&frozen), starting, Some(&[]), false);
    assert!(
        matches!(&plan.announced[0].change, ToolSetChange::Removed { name } if name == "mcp__s__q")
    );
}

fn changes(plan: &ToolSetPlan) -> Vec<ToolSetChange> {
    plan.announced
        .iter()
        .map(|change| change.change.clone())
        .collect()
}

/// A provider that takes changes inside a message sees only the notices its
/// history still holds. A change whose notice a summary or a rewind hid is
/// announced again; the record and the array do not change.
#[test]
fn a_change_whose_notice_left_the_history_is_announced_again() {
    let frozen = StoredToolSet::new(vec![tool("bash", "Run", "c"), tool("read", "Read", "p")]);
    let live = vec![tool("bash", "Run", "c"), tool("probe", "Probe", "x")];
    let first = plan_tool_set(Some(&frozen), live.clone(), Some(&[]), false);
    let in_view = changes(&first);
    assert_eq!(in_view.len(), 2);

    // The notice is in view: nothing more to say.
    let seen = plan_tool_set(Some(&first.record), live.clone(), Some(&in_view), false);
    assert!(seen.announced.is_empty());
    assert_eq!(seen.record, first.record);

    // The notice is hidden: the same changes again, the record unchanged.
    let hidden = plan_tool_set(Some(&first.record), live.clone(), Some(&[]), false);
    assert_eq!(changes(&hidden), in_view);
    assert_eq!(hidden.record, first.record);
    assert!(!hidden.array_changed);
    assert_eq!(hidden.tools, frozen.advertised);

    // Both the original and the repeat in view (the edit was reverted).
    let both: Vec<ToolSetChange> = in_view.iter().chain(&in_view).cloned().collect();
    let reverted = plan_tool_set(Some(&first.record), live.clone(), Some(&both), false);
    assert!(reverted.announced.is_empty());

    // A provider whose array carries the changes has nothing to repeat.
    let array = plan_tool_set(Some(&first.record), live, None, false);
    assert!(array.announced.is_empty());
}

#[test]
fn the_changes_in_view_are_those_of_notices_still_projected() {
    use chrono::Utc;
    let change = |name: &str| ToolSetChange::Removed {
        name: name.to_string(),
    };
    let notice = |sequence: usize, name: &str| {
        jcode_session_types::tool_set_delivery_message(
            format!("message_{sequence}"),
            &tool_set_notice(
                sequence,
                &[StoredToolSetChange {
                    change: change(name),
                    schema_changed: false,
                }],
            ),
            vec![change(name)],
            Utc::now(),
        )
        .expect("delivery")
    };
    let stored = vec![notice(1, "read"), notice(2, "bash")];
    assert_eq!(tool_set_notice_count(&stored), 2);
    let all: Vec<Message> = stored.iter().map(StoredMessage::to_message).collect();
    assert_eq!(
        tool_changes_in_view(&stored, &all),
        vec![change("read"), change("bash")]
    );
    // A summary stands where the first notice was.
    let summarized = vec![Message::user("summary of earlier work"), all[1].clone()];
    assert_eq!(
        tool_changes_in_view(&stored, &summarized),
        vec![change("bash")]
    );
}
