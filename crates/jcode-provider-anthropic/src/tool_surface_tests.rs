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
    let formatted = format_tools(&registry);

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
fn formatted_tools_carry_no_cache_marker_of_their_own() {
    // Breakpoints are placed on the whole request (`place_cache_breakpoints`);
    // the static-prefix marker sits on the last system block, which covers the
    // tools rendered before it (INT-01 DESIGN §6).
    let formatted = format_tools(&formerly_remapped_registry());
    assert!(formatted.iter().all(|tool| tool.cache_control.is_none()));
    assert!(format_tools(&[]).is_empty());
}

#[test]
fn tool_choice_disables_parallel_calls_whenever_tools_are_sent() {
    let formatted = format_tools(&formerly_remapped_registry());
    let choice = ApiToolChoice::for_tools(&formatted).expect("tools present");
    assert_eq!(
        serde_json::to_value(choice).expect("serialize"),
        json!({"type": "auto", "disable_parallel_tool_use": true})
    );
    assert_eq!(ApiToolChoice::for_tools(&[]), None);
}

#[test]
fn request_serializes_tool_choice_beside_tools_and_omits_it_without_tools() {
    let tools = format_tools(&formerly_remapped_registry());
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

#[test]
fn format_tools_removes_top_level_combinators_for_anthropic_api() {
    let tool = ToolDefinition {
        name: "custom".to_string(),
        description: "schema compatibility regression".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "nested_union": {
                    "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]
                }
            },
            "required": ["action"],
            "oneOf": [
                {"type": "object", "properties": {"label": {"type": "string"}}},
                {"type": "object", "properties": {"task_id": {"type": "string"}}}
            ],
            "allOf": [
                {"type": "object", "properties": {"intent": {"type": "string"}}, "required": ["intent"]}
            ]
        }),
    };

    let formatted = format_tools(&[tool]);
    let schema = &formatted[0].input_schema;
    for keyword in ["oneOf", "anyOf", "allOf"] {
        assert!(
            schema.get(keyword).is_none(),
            "Anthropic rejects top-level {keyword}: {schema}"
        );
    }
    for property in ["action", "nested_union", "label", "task_id", "intent"] {
        assert!(
            schema["properties"].get(property).is_some(),
            "missing merged property {property}: {schema}"
        );
    }
    assert!(
        schema["properties"]["nested_union"].get("anyOf").is_some(),
        "nested combinators remain supported and should not be flattened"
    );
    assert_eq!(schema["required"], json!(["action", "intent"]));
}
