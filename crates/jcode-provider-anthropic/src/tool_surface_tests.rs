//! The Anthropic tool surface is the registry's, for both credential routes.
//!
//! These tests replace the curated Claude Code stand-ins that used to be sent
//! over OAuth. Their schemas drifted from the real tools (#706, #722, and the
//! unusable `Agent` stub), so the formatter now has no provider-authored tool
//! content at all. The permanent parity test over the real registry lives in
//! `jcode-app-core` (`tool::provider_parity_tests`).

use super::*;
use jcode_message_types::{ContentBlock, Message, Role, ToolDefinition};
use serde_json::json;

fn registry_tool(name: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("{name}: the registry's own description"),
        input_schema: schema,
    }
}

fn formerly_remapped_registry() -> Vec<ToolDefinition> {
    [
        "bash",
        "edit",
        "read",
        "schedule",
        "skill_manage",
        "subagent",
        "write",
    ]
    .into_iter()
    .map(|name| {
        registry_tool(
            name,
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "intent": {"type": "string"}
                },
                "required": ["task", "intent"],
                "additionalProperties": false
            }),
        )
    })
    .collect()
}

#[test]
fn every_tool_keeps_its_registry_name_description_and_schema_dialect() {
    let registry = formerly_remapped_registry();
    let formatted = format_tools(&registry, false);

    assert_eq!(formatted.len(), registry.len(), "no tool added or dropped");
    for (tool, wire) in registry.iter().zip(&formatted) {
        assert_eq!(wire.name, tool.name, "registry name is sent unchanged");
        assert_eq!(wire.description, tool.description);
        assert_eq!(
            wire.input_schema,
            anthropic_input_schema(&tool.input_schema)
        );
    }
}

#[test]
fn history_tool_uses_replay_registry_names() {
    let blocks = format_content_blocks(&[ContentBlock::ToolUse {
        id: "toolu_1".to_string(),
        name: "subagent".to_string(),
        input: json!({"prompt": "task"}),
        thought_signature: None,
    }]);
    let value = serde_json::to_value(&blocks).expect("serialize");
    assert_eq!(value[0]["name"], "subagent");
}

#[test]
fn only_the_final_tool_carries_the_tools_cache_breakpoint() {
    let formatted = format_tools(&formerly_remapped_registry(), true);
    let marked: Vec<&str> = formatted
        .iter()
        .filter(|tool| tool.cache_control.is_some())
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(marked, vec!["write"]);
    assert!(format_tools(&[], true).is_empty());
}

#[test]
fn tool_choice_disables_parallel_calls_whenever_tools_are_sent() {
    let formatted = format_tools(&formerly_remapped_registry(), false);
    let choice = ApiToolChoice::for_tools(&formatted).expect("tools present");
    assert_eq!(
        serde_json::to_value(choice).expect("serialize"),
        json!({"type": "auto", "disable_parallel_tool_use": true})
    );
    assert_eq!(ApiToolChoice::for_tools(&[]), None);
}

#[test]
fn request_serializes_tool_choice_beside_tools_and_omits_it_without_tools() {
    let tools = format_tools(&formerly_remapped_registry(), false);
    let with_tools = ApiRequest {
        model: "claude-opus-5-5".to_string(),
        max_tokens: 16,
        system: None,
        messages: format_messages(&[Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }]),
        tool_choice: ApiToolChoice::for_tools(&tools),
        tools: Some(tools),
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    };
    let value = serde_json::to_value(&with_tools).expect("serialize");
    assert_eq!(value["tool_choice"]["disable_parallel_tool_use"], true);

    let without_tools = ApiRequest {
        tools: None,
        tool_choice: None,
        ..with_tools
    };
    let value = serde_json::to_value(&without_tools).expect("serialize");
    assert!(value.get("tool_choice").is_none());
}
