use jcode_message_types::{
    ContentBlock, Message as ChatMessage, Role, TOOL_OUTPUT_MISSING_TEXT, ToolDefinition,
    sanitize_tool_id,
};
use jcode_provider_core::ContextRequestBuilderValidation;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub const COPILOT_API_VERSION: &str = "2025-04-01";

/// Default model id. This must be a **Copilot catalog** id (dot-separated,
/// e.g. `claude-sonnet-4.6`), not the Anthropic-native hyphenated form: the
/// Copilot API rejects the latter with HTTP 400 `model_not_supported`
/// (issue #640). Keep this in sync with the head of [`FALLBACK_MODELS`].
pub const DEFAULT_MODEL: &str = "claude-sonnet-4.6";

pub const FALLBACK_MODELS: &[&str] = &[
    "claude-sonnet-4.6",
    "claude-sonnet-4.5",
    "claude-haiku-4.5",
    "claude-opus-4.6",
    "claude-opus-4.6-fast",
    "claude-opus-4.5",
    "claude-sonnet-4",
    "gemini-3-pro-preview",
    "gpt-5.4",
    "gpt-5.4-pro",
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex",
    "gpt-5.1",
    "gpt-5.1-codex-mini",
    "gpt-5-mini",
    "gpt-4.1",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCatalog {
    pub models: Vec<String>,
    pub fetched_at_rfc3339: String,
}

pub fn is_known_display_model(model: &str) -> bool {
    FALLBACK_MODELS.contains(&model)
}

pub fn max_token_parameter_for_model(model: &str) -> &'static str {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.starts_with("gpt-5") {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

pub fn add_max_token_parameter(body: &mut Value, model: &str, max_tokens: u32) {
    body[max_token_parameter_for_model(model)] = json!(max_tokens);
}

/// Build OpenAI-compatible messages array from jcode's message format.
///
/// Properly pairs tool_use blocks (in assistant messages) with their
/// corresponding tool_result blocks (in user messages), handling out-of-order
/// results and missing outputs.
pub fn build_messages(system: &str, messages: &[ChatMessage]) -> Vec<Value> {
    let mut result = Vec::new();
    let missing_output = format!("[Error] {}", TOOL_OUTPUT_MISSING_TEXT);

    if !system.is_empty() {
        result.push(json!({
            "role": "system",
            "content": system,
        }));
    }

    let mut tool_result_last_pos: HashMap<String, usize> = HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        if let Role::User = msg.role {
            for block in &msg.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    tool_result_last_pos.insert(tool_use_id.clone(), idx);
                }
            }
        }
    }

    let mut tool_calls_seen: HashSet<String> = HashSet::new();
    let mut pending_tool_results: HashMap<String, String> = HashMap::new();
    let mut used_tool_results: HashSet<String> = HashSet::new();

    for (idx, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::User => {
                let mut text_parts: Vec<&str> = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            text_parts.push(text.as_str());
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            if used_tool_results.contains(tool_use_id) {
                                continue;
                            }
                            let output = if is_error == &Some(true) {
                                format!("[Error] {}", content)
                            } else if content.is_empty() {
                                TOOL_OUTPUT_MISSING_TEXT.to_string()
                            } else {
                                content.clone()
                            };
                            if tool_calls_seen.contains(tool_use_id) {
                                result.push(json!({
                                    "role": "tool",
                                    "tool_call_id": sanitize_tool_id(tool_use_id),
                                    "content": output,
                                }));
                                used_tool_results.insert(tool_use_id.clone());
                            } else if !pending_tool_results.contains_key(tool_use_id) {
                                pending_tool_results.insert(tool_use_id.clone(), output);
                            }
                        }
                        _ => {}
                    }
                }

                let text = text_parts.join("\n");
                if !text.is_empty() {
                    result.push(json!({
                        "role": "user",
                        "content": text,
                    }));
                }
            }
            Role::Assistant => {
                let mut content_text = String::new();
                let mut tool_calls = Vec::new();
                let mut post_tool_outputs: Vec<(String, String)> = Vec::new();
                let mut missing_tool_outputs: Vec<String> = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            content_text.push_str(text);
                        }
                        ContentBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            let args = if input.is_object() {
                                input.to_string()
                            } else {
                                "{}".to_string()
                            };
                            tool_calls.push(json!({
                                "id": sanitize_tool_id(id),
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": args,
                                }
                            }));
                            tool_calls_seen.insert(id.clone());
                            if let Some(output) = pending_tool_results.remove(id) {
                                post_tool_outputs.push((id.clone(), output));
                                used_tool_results.insert(id.clone());
                            } else {
                                let has_future_output = tool_result_last_pos
                                    .get(id)
                                    .map(|pos| *pos > idx)
                                    .unwrap_or(false);
                                if !has_future_output {
                                    missing_tool_outputs.push(id.clone());
                                    used_tool_results.insert(id.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let mut assistant_msg = json!({
                    "role": "assistant",
                });

                if !content_text.is_empty() {
                    assistant_msg["content"] = json!(content_text);
                }
                if !tool_calls.is_empty() {
                    assistant_msg["tool_calls"] = json!(tool_calls);
                }

                if !content_text.is_empty() || !tool_calls.is_empty() {
                    result.push(assistant_msg);

                    for (tool_call_id, output) in post_tool_outputs {
                        result.push(json!({
                            "role": "tool",
                            "tool_call_id": sanitize_tool_id(&tool_call_id),
                            "content": output,
                        }));
                    }

                    for missing_id in missing_tool_outputs {
                        result.push(json!({
                            "role": "tool",
                            "tool_call_id": sanitize_tool_id(&missing_id),
                            "content": missing_output.clone(),
                        }));
                    }
                }
            }
        }
    }

    result
}

/// Validate projected history through the production GitHub Copilot chat builder.
pub fn validate_projected_messages(
    messages: &[ChatMessage],
) -> Result<ContextRequestBuilderValidation, String> {
    if messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::OpenAICompaction { .. }))
    }) {
        return Err(
            "Projected Copilot history contains OpenAI-native compaction state that the chat-completions builder cannot replay."
                .to_string(),
        );
    }

    let formatted = build_messages("", messages);
    if formatted.is_empty() {
        return Err(
            "Projected history normalizes to no GitHub Copilot chat messages; the request would not contain a valid conversation turn."
                .to_string(),
        );
    }

    let mut tool_call_positions = HashMap::new();
    let mut synthetic_missing_outputs = 0usize;
    let missing_output = format!("[Error] {TOOL_OUTPUT_MISSING_TEXT}");

    for (index, message) in formatted.iter().enumerate() {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if !matches!(role, "user" | "assistant" | "tool") {
            return Err(format!(
                "GitHub Copilot chat message {index} has invalid role '{role}'."
            ));
        }

        match role {
            "user" => {
                if !message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| !content.is_empty())
                {
                    return Err(format!(
                        "GitHub Copilot user message {index} has no text content."
                    ));
                }
            }
            "assistant" => {
                let has_content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| !content.is_empty());
                let tool_calls = message.get("tool_calls").and_then(Value::as_array);
                let has_tool_calls = tool_calls.is_some_and(|calls| !calls.is_empty());
                if !has_content && !has_tool_calls {
                    return Err(format!(
                        "GitHub Copilot assistant message {index} has neither content nor tool_calls."
                    ));
                }

                if let Some(tool_calls) = tool_calls {
                    for (offset, call) in tool_calls.iter().enumerate() {
                        let call_id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| {
                                format!(
                                    "GitHub Copilot assistant message {index} contains a tool call without an id."
                                )
                            })?;
                        if tool_call_positions
                            .insert(call_id.to_string(), index)
                            .is_some()
                        {
                            return Err(format!(
                                "GitHub Copilot projected history contains duplicate normalized tool-call id '{call_id}'."
                            ));
                        }
                        let function = call.get("function").ok_or_else(|| {
                            format!("GitHub Copilot tool call '{call_id}' has no function payload.")
                        })?;
                        if !function
                            .get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|name| !name.is_empty())
                            || function.get("arguments").and_then(Value::as_str).is_none()
                        {
                            return Err(format!(
                                "GitHub Copilot tool call '{call_id}' has an invalid function payload."
                            ));
                        }

                        let output = formatted.get(index + 1 + offset).ok_or_else(|| {
                            format!(
                                "GitHub Copilot tool call '{call_id}' has no immediately following tool output."
                            )
                        })?;
                        if output.get("role").and_then(Value::as_str) != Some("tool")
                            || output.get("tool_call_id").and_then(Value::as_str) != Some(call_id)
                        {
                            return Err(format!(
                                "GitHub Copilot tool call '{call_id}' is not immediately followed by its matching tool output."
                            ));
                        }
                    }
                }
            }
            "tool" => {
                let tool_call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        format!("GitHub Copilot tool message {index} has no tool_call_id.")
                    })?;
                if !tool_call_positions
                    .get(tool_call_id)
                    .is_some_and(|call_index| *call_index < index)
                {
                    return Err(format!(
                        "GitHub Copilot tool message {index} is orphaned from its assistant tool call."
                    ));
                }
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("GitHub Copilot tool message {index} has non-text content.")
                    })?;
                if content == missing_output {
                    synthetic_missing_outputs += 1;
                }
            }
            _ => unreachable!(),
        }
    }

    let normalization_notes = if synthetic_missing_outputs == 0 {
        Vec::new()
    } else {
        vec![format!(
            "The production Copilot formatter inserted {synthetic_missing_outputs} explicit missing tool-output placeholder(s)."
        )]
    };
    Ok(ContextRequestBuilderValidation {
        normalized_item_count: formatted.len(),
        formatter_placeholder_count: 0,
        normalization_notes,
    })
}

/// Build OpenAI-compatible tools array.
pub fn build_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            // Copilot's chat-completions endpoint routes to heterogeneous
            // upstreams and, like OpenRouter, rejects combiners at the tool
            // schema root. Normalize to that conservative dialect before the
            // schema is placed in the request payload (issue #855).
            let parameters = jcode_schema_dialect::normalize(
                &t.input_schema,
                &jcode_schema_dialect::registry::OPENROUTER,
            );
            json!({
                "type": "function",
                "function": {
                    "name": &t.name,
                    // Prompt-visible. Approximate token cost for this field:
                    // t.description_token_estimate().
                    "description": &t.description,
                    "parameters": parameters,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swarm_shaped_tool() -> ToolDefinition {
        ToolDefinition {
            name: "swarm".to_string(),
            description: "Coordinate agents".to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {"type": "string"},
                    "message": {"type": "string"}
                },
                "anyOf": [
                    {
                        "type": "object",
                        "required": ["action", "label"],
                        "properties": {
                            "action": {"type": "string", "enum": ["spawn"]},
                            "label": {"type": "string", "minLength": 1}
                        }
                    },
                    {
                        "type": "object",
                        "required": ["action"],
                        "properties": {
                            "action": {"type": "string", "enum": ["list"]}
                        }
                    }
                ]
            }),
        }
    }

    #[test]
    fn copilot_tool_serialization_flattens_swarm_top_level_combiners() {
        let json = serde_json::to_string(&build_tools(&[swarm_shaped_tool()])).unwrap();
        let serialized: Value = serde_json::from_str(&json).unwrap();
        let parameters = &serialized[0]["function"]["parameters"];

        for combiner in ["anyOf", "oneOf", "allOf"] {
            assert!(
                parameters.get(combiner).is_none(),
                "top-level {combiner} must not reach Copilot: {parameters}"
            );
        }
    }

    #[test]
    fn copilot_tool_payload_preserves_swarm_properties_and_required_fields() {
        let tools = build_tools(&[swarm_shaped_tool()]);
        let function = &tools[0]["function"];
        let parameters = &function["parameters"];

        assert_eq!(function["name"], "swarm");
        assert_eq!(function["description"], "Coordinate agents");
        assert!(parameters["properties"]["action"].is_object());
        assert!(parameters["properties"]["message"].is_object());
        assert!(parameters["properties"]["label"].is_object());
        assert_eq!(parameters["required"], json!(["action"]));
    }

    #[test]
    fn projected_history_validation_accepts_paired_tool_calls() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    input: json!({"path": "README.md"}),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            ChatMessage::tool_result("call-1", "contents", false),
            ChatMessage::user("Continue."),
        ];

        let validation = validate_projected_messages(&messages)
            .expect("paired Copilot chat history must validate");
        assert_eq!(validation.normalized_item_count, 3);
    }

    #[test]
    fn projected_history_validation_rejects_delayed_tool_outputs() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    input: json!({"path": "README.md"}),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            ChatMessage::user("An unrelated user turn."),
            ChatMessage::tool_result("call-1", "contents", false),
        ];

        let error = validate_projected_messages(&messages).unwrap_err();
        assert!(error.contains("not immediately followed"));
    }

    #[test]
    fn projected_history_validation_rejects_normalized_tool_id_collisions() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call.a".to_string(),
                        name: "read".to_string(),
                        input: json!({"file_path": "a"}),
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call_a".to_string(),
                        name: "read".to_string(),
                        input: json!({"file_path": "b"}),
                        thought_signature: None,
                    },
                ],
                timestamp: None,
                tool_duration_ms: None,
            },
            ChatMessage::tool_result("call.a", "a", false),
            ChatMessage::tool_result("call_a", "b", false),
        ];

        let error = validate_projected_messages(&messages).unwrap_err();
        assert!(error.contains("duplicate normalized tool-call id 'call_a'"));
    }
}
