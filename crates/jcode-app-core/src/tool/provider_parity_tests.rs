//! Claude and GPT receive the same tool surface (INT-01 R01, R02; D3, D12).
//!
//! Every built-in tool is formatted through the production Anthropic and
//! OpenAI request builders. Names and descriptions must be identical, and each
//! schema must equal the registry schema passed through that provider's one
//! named dialect normalizer. The normalizers must preserve every property,
//! `required` entry, enum and nested object, apart from recorded top-level
//! combinator widenings. Finally, the minimal input each advertised schema
//! allows must decode through the tool's real decoder, which is the check the
//! old hand-written Claude `Agent` stand-in would have failed.
//!
//! The check functions take wire JSON so they can also be pointed at a
//! captured payload from another build.

use super::*;
use jcode_provider_core::openai_schema::{
    openai_compatible_schema, schema_supports_strict, strict_normalize_schema,
};
use jcode_provider_core::{ANTHROPIC_TOOL_NAME_POLICY, anthropic_tool_name_is_valid};
use serde_json::{Value, json};

/// Tools whose registry schema has a top-level `oneOf`/`anyOf`/`allOf` that a
/// production dialect flattens, as `(tool, dialect id, keyword)`. The flat
/// schema loses only the per-branch requirement; the tool's decoder remains
/// the authority on which combination is valid (D12).
const RECORDED_TOP_LEVEL_WIDENINGS: &[(&str, &str, &str)] = &[];

async fn surface() -> (crate::auth::test_sandbox::AuthTestSandbox, Registry) {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("sandbox");
    let registry = Registry::builtin_tool_surface(Arc::new(super::MockProvider)).await;
    (sandbox, registry)
}

fn openai_parameters(schema: &Value) -> (Value, bool) {
    let compatible = openai_compatible_schema(schema);
    let strict = schema_supports_strict(&compatible);
    if strict {
        (strict_normalize_schema(&compatible), true)
    } else {
        (compatible, false)
    }
}

/// R01: names, descriptions and schema provenance, per tool, in order.
fn surface_parity_failures(
    definitions: &[ToolDefinition],
    anthropic: &Value,
    openai: &Value,
) -> Vec<String> {
    let mut failures = Vec::new();
    let names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    if let Err(errors) = ANTHROPIC_TOOL_NAME_POLICY.validate(&names) {
        failures.extend(errors);
    }
    let (Some(anthropic), Some(openai)) = (anthropic.as_array(), openai.as_array()) else {
        return vec!["wire tool lists must be arrays".to_string()];
    };
    if anthropic.len() != definitions.len() || openai.len() != definitions.len() {
        failures.push(format!(
            "tool counts differ: registry {}, anthropic {}, openai {}",
            definitions.len(),
            anthropic.len(),
            openai.len()
        ));
    }
    for (definition, (claude, gpt)) in definitions.iter().zip(anthropic.iter().zip(openai)) {
        let name = definition.name.as_str();
        let wire_name = ANTHROPIC_TOOL_NAME_POLICY.wire_name(name);
        if claude["name"] != wire_name {
            failures.push(format!("{name}: Claude sees name {}", claude["name"]));
        }
        if !anthropic_tool_name_is_valid(wire_name) {
            failures.push(format!("{name}: not a valid Anthropic tool name"));
        }
        if gpt["name"] != name {
            failures.push(format!("{name}: GPT sees name {}", gpt["name"]));
        }
        for (provider, wire) in [("Claude", claude), ("GPT", gpt)] {
            if wire["description"] != definition.description.as_str() {
                failures.push(format!(
                    "{name}: {provider} description differs from registry"
                ));
            }
        }
        let anthropic_schema =
            jcode_provider_anthropic::anthropic_input_schema(&definition.input_schema);
        if claude["input_schema"] != anthropic_schema {
            failures.push(format!(
                "{name}: Claude schema is not the Anthropic dialect of the registry schema"
            ));
        }
        let (openai_schema, strict) = openai_parameters(&definition.input_schema);
        if gpt["parameters"] != openai_schema || gpt["strict"] != strict {
            failures.push(format!(
                "{name}: GPT schema is not the OpenAI dialect of the registry schema"
            ));
        }
    }
    failures
}

/// Minimal instance of `schema`: required object members only, the first
/// non-null alternative, the first enum value, the lower array bound.
fn minimal_instance(schema: &Value) -> Value {
    let Some(map) = schema.as_object() else {
        return Value::Null;
    };
    if let Some(value) = map.get("const") {
        return value.clone();
    }
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .find(|value| !value.is_null())
            .cloned()
            .unwrap_or(Value::Null);
    }
    for combiner in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = map.get(combiner).and_then(Value::as_array) {
            let branch = branches
                .iter()
                .find(|branch| branch.get("type") != Some(&json!("null")))
                .or(branches.first());
            if let Some(branch) = branch {
                return minimal_instance(branch);
            }
        }
    }
    let kind = match map.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null")
            .unwrap_or("null"),
        _ if map.contains_key("properties") => "object",
        _ => "string",
    };
    match kind {
        "object" => {
            let properties = map.get("properties").and_then(Value::as_object);
            let mut object = serde_json::Map::new();
            for name in map
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let property = properties.and_then(|p| p.get(name)).unwrap_or(&Value::Null);
                object.insert(name.to_string(), minimal_instance(property));
            }
            Value::Object(object)
        }
        "array" => {
            let count = map.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            let item = minimal_instance(map.get("items").unwrap_or(&Value::Null));
            Value::Array((0..count).map(|_| item.clone()).collect())
        }
        "integer" | "number" => map.get("minimum").cloned().unwrap_or(json!(1)),
        "boolean" => json!(false),
        "null" => Value::Null,
        _ => {
            let length = map.get("minLength").and_then(Value::as_u64).unwrap_or(1);
            Value::String("x".repeat(length.max(1) as usize))
        }
    }
}

/// R02: the minimal input of each advertised schema decodes through the
/// tool's real decoder, reached by the name the model would call.
async fn decode_failures(registry: &Registry, wire_tools: &Value, schema_key: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for tool in wire_tools.as_array().into_iter().flatten() {
        let wire_name = tool["name"].as_str().unwrap_or_default();
        let registry_name = ANTHROPIC_TOOL_NAME_POLICY.registry_name(wire_name);
        let input = minimal_instance(&tool[schema_key]);
        if let Err(error) = registry.decode_input(registry_name, input.clone()).await {
            failures.push(format!("{wire_name} rejected {input}: {error:#}"));
        }
    }
    failures
}

/// D12: what a dialect normalizer may and may not change.
fn preservation_failures(
    tool: &str,
    dialect: &str,
    original: &Value,
    normalized: &Value,
    widenings: &mut Vec<(String, String, String)>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = original.get(keyword).and_then(Value::as_array)
            && normalized.get(keyword).is_none()
            && !(keyword == "oneOf" && normalized.get("anyOf").is_some())
        {
            widenings.push((tool.to_string(), dialect.to_string(), keyword.to_string()));
            for branch in branches {
                for name in branch
                    .get("properties")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|p| p.keys())
                {
                    if normalized["properties"].get(name).is_none() {
                        failures.push(format!(
                            "[{dialect}] {tool}: widened branch property `{name}` disappeared"
                        ));
                    }
                }
            }
        }
    }
    compare_preserved(
        original,
        normalized,
        &format!("[{dialect}] {tool} $"),
        &mut failures,
    );
    failures
}

/// The branch of an OpenAI strict `anyOf: [original, {type: null}]` wrapper
/// that corresponds to the original, or the node itself.
fn unwrap_nullable<'a>(original: &Value, normalized: &'a Value) -> &'a Value {
    if original.get("anyOf").is_none()
        && let Some(branches) = normalized.get("anyOf").and_then(Value::as_array)
        && branches
            .iter()
            .any(|b| b.get("type") == Some(&json!("null")))
        && let Some(branch) = branches
            .iter()
            .find(|b| b.get("type") != Some(&json!("null")))
    {
        return branch;
    }
    normalized
}

fn compare_preserved(original: &Value, normalized: &Value, path: &str, failures: &mut Vec<String>) {
    let normalized = unwrap_nullable(original, normalized);
    let (Some(original), Some(normalized)) = (original.as_object(), normalized.as_object()) else {
        return;
    };
    if let Some(description) = original.get("description")
        && normalized.get("description") != Some(description)
    {
        failures.push(format!("{path}: description changed"));
    }
    if let Some(values) = original.get("enum").and_then(Value::as_array) {
        let kept = normalized.get("enum").and_then(Value::as_array);
        if !kept.is_some_and(|kept| values.iter().all(|value| kept.contains(value))) {
            failures.push(format!("{path}: enum values lost"));
        }
    }
    if let Some(Value::String(kind)) = original.get("type") {
        let kept = match normalized.get("type") {
            Some(Value::String(k)) => k == kind,
            Some(Value::Array(ks)) => ks.iter().any(|k| k == kind),
            _ => false,
        };
        if !kept {
            failures.push(format!("{path}: type `{kind}` lost"));
        }
    }
    for name in original
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let kept = normalized
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| required.iter().any(|r| r == name));
        if !kept {
            failures.push(format!("{path}: required `{name}` lost"));
        }
    }
    if let Some(properties) = original.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            match normalized.get("properties").and_then(|p| p.get(name)) {
                Some(kept) => compare_preserved(
                    property,
                    kept,
                    &format!("{path}.properties.{name}"),
                    failures,
                ),
                None => failures.push(format!("{path}: property `{name}` lost")),
            }
        }
    }
    if let Some(items) = original.get("items") {
        match normalized.get("items") {
            Some(kept) => compare_preserved(items, kept, &format!("{path}.items"), failures),
            None => failures.push(format!("{path}: items lost")),
        }
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        let Some(branches) = original.get(keyword).and_then(Value::as_array) else {
            continue;
        };
        // OpenAI spells `oneOf` as `anyOf`; a flattened top level is checked
        // by the caller as a recorded widening.
        let kept = normalized.get(keyword).or_else(|| {
            (keyword == "oneOf")
                .then(|| normalized.get("anyOf"))
                .flatten()
        });
        if let Some(kept) = kept.and_then(Value::as_array) {
            for (index, branch) in branches.iter().enumerate() {
                match kept.get(index) {
                    Some(kept) => compare_preserved(
                        branch,
                        kept,
                        &format!("{path}.{keyword}[{index}]"),
                        failures,
                    ),
                    None => failures.push(format!("{path}.{keyword}[{index}] lost")),
                }
            }
        }
    }
}

#[tokio::test]
async fn claude_and_gpt_receive_the_same_tool_surface() {
    let (_sandbox, registry) = surface().await;
    let definitions = registry.definitions(None).await;
    for expected in [
        "subagent",
        "selfdev",
        "debug_socket",
        "end_ambient_cycle",
        "mcp",
    ] {
        assert!(
            definitions.iter().any(|d| d.name == expected),
            "the surface must include `{expected}`"
        );
    }
    let anthropic = serde_json::to_value(jcode_provider_anthropic::format_tools(&definitions))
        .expect("serialize");
    let openai = Value::Array(crate::provider::openai_request::build_tools(&definitions));

    let failures = surface_parity_failures(&definitions, &anthropic, &openai);
    assert!(
        failures.is_empty(),
        "tool surfaces differ:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn dialect_normalizers_preserve_every_tool_schema() {
    let (_sandbox, registry) = surface().await;
    let definitions = registry.definitions(None).await;
    let mut failures = Vec::new();
    let mut widenings = Vec::new();
    for definition in &definitions {
        let original = &definition.input_schema;
        let anthropic = jcode_provider_anthropic::anthropic_input_schema(original);
        failures.extend(preservation_failures(
            &definition.name,
            "anthropic",
            original,
            &anthropic,
            &mut widenings,
        ));
        let (openai, _) = openai_parameters(original);
        failures.extend(preservation_failures(
            &definition.name,
            "openai",
            original,
            &openai,
            &mut widenings,
        ));
    }
    let recorded: Vec<(String, String, String)> = RECORDED_TOP_LEVEL_WIDENINGS
        .iter()
        .map(|(tool, dialect, keyword)| {
            (tool.to_string(), dialect.to_string(), keyword.to_string())
        })
        .collect();
    assert_eq!(
        widenings, recorded,
        "top-level combinator widenings changed"
    );
    assert!(
        failures.is_empty(),
        "normalization lost meaning:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn every_advertised_minimal_input_decodes_through_the_real_tool() {
    let (_sandbox, registry) = surface().await;
    let definitions = registry.definitions(None).await;
    let registry_tools = Value::Array(
        definitions
            .iter()
            .map(|d| json!({"name": d.name, "input_schema": d.input_schema}))
            .collect(),
    );
    let anthropic = serde_json::to_value(jcode_provider_anthropic::format_tools(&definitions))
        .expect("serialize");

    let mut failures = decode_failures(&registry, &registry_tools, "input_schema").await;
    failures.extend(decode_failures(&registry, &anthropic, "input_schema").await);
    assert!(
        failures.is_empty(),
        "advertised inputs rejected:\n{}",
        failures.join("\n")
    );
}

/// The generator must produce what a schema demands, or the decoder test
/// above passes vacuously.
#[test]
fn minimal_instances_follow_the_schema() {
    let schema = json!({
        "type": "object",
        "properties": {
            "command": {"type": "string"},
            "mode": {"type": "string", "enum": ["a", "b"]},
            "count": {"type": ["integer", "null"], "minimum": 2},
            "items": {"type": "array", "minItems": 1, "items": {"type": "boolean"}},
            "nested": {"anyOf": [{"type": "null"}, {"type": "object", "properties": {"x": {"type": "number"}}, "required": ["x"]}]},
            "optional": {"type": "string"}
        },
        "required": ["command", "mode", "count", "items", "nested"]
    });
    assert_eq!(
        minimal_instance(&schema),
        json!({"command": "x", "mode": "a", "count": 2, "items": [false], "nested": {"x": 1}})
    );
}

/// The decoder check must reject a stand-in whose fields the tool does not
/// accept, as the old OAuth `Agent` stub was.
#[tokio::test]
async fn a_drifted_stand_in_schema_is_rejected() {
    let (_sandbox, registry) = surface().await;
    let stand_in = json!([{
        "name": "subagent",
        "input_schema": {
            "type": "object",
            "properties": {"description": {"type": "string"}, "prompt": {"type": "string"}},
            "required": ["description", "prompt"]
        }
    }]);
    let failures = decode_failures(&registry, &stand_in, "input_schema").await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("description"), "{failures:?}");
}
