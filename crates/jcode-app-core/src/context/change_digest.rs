use crate::message::ContentBlock;
use crate::tool::parsed_patch_file_paths;
use jcode_context_core::{ContextTargetIndex, MessageRangeResolutionError};
use jcode_session_types::{StoredMessage, StoredMessageRange};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const MAX_BATCH_NESTING: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextChangeEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn extract_context_change_evidence(
    messages: &[StoredMessage],
    range: &StoredMessageRange,
) -> Result<ContextChangeEvidence, MessageRangeResolutionError> {
    let (start, end) = ContextTargetIndex::new(messages).resolve_message_range(range)?;
    let mut accumulator = EvidenceAccumulator {
        complete: true,
        ..EvidenceAccumulator::default()
    };
    for message in &messages[start..=end] {
        for block in &message.content {
            if let ContentBlock::ToolUse { name, input, .. } = block {
                inspect_tool_call(name, input, 0, &mut accumulator);
            }
        }
    }
    Ok(ContextChangeEvidence {
        changed_files: accumulator.paths.into_iter().collect(),
        complete: accumulator.complete,
        warnings: accumulator.warnings.into_iter().collect(),
    })
}

#[derive(Default)]
struct EvidenceAccumulator {
    paths: BTreeSet<String>,
    warnings: BTreeSet<String>,
    complete: bool,
}

fn inspect_tool_call(
    tool_name: &str,
    input: &Value,
    depth: usize,
    accumulator: &mut EvidenceAccumulator,
) {
    let normalized_name = tool_name.trim().to_ascii_lowercase();
    match normalized_name.as_str() {
        "write" | "edit" | "multiedit" => {
            extract_string_path(input, "file_path", accumulator);
        }
        "patch" | "apply_patch" => match input.get("patch_text").and_then(Value::as_str) {
            Some(patch_text) => match parsed_patch_file_paths(&normalized_name, patch_text) {
                Ok(paths) if !paths.is_empty() => {
                    for path in paths {
                        insert_path(&path, accumulator);
                    }
                }
                Ok(_) => mark_incomplete(
                    accumulator,
                    format!("{normalized_name} contained no parseable file paths"),
                ),
                Err(error) => mark_incomplete(
                    accumulator,
                    format!("{normalized_name} path extraction failed: {error}"),
                ),
            },
            None => mark_incomplete(
                accumulator,
                format!("{normalized_name} did not contain a string patch_text field"),
            ),
        },
        "batch" => inspect_batch(input, depth, accumulator),
        "bash" => mark_incomplete(
            accumulator,
            "shell commands occurred in the selected range; changed-file evidence may be incomplete"
                .to_string(),
        ),
        name if known_non_file_mutating_tool(name) => {}
        name => mark_incomplete(
            accumulator,
            format!(
                "tool {name} has no auditable source-file mutation contract; changed-file evidence may be incomplete"
            ),
        ),
    }
}

fn inspect_batch(input: &Value, depth: usize, accumulator: &mut EvidenceAccumulator) {
    if depth >= MAX_BATCH_NESTING {
        mark_incomplete(
            accumulator,
            "batch nesting exceeded the changed-file evidence safety bound".to_string(),
        );
        return;
    }
    let Some(calls) = input.get("tool_calls").and_then(Value::as_array) else {
        mark_incomplete(
            accumulator,
            "batch input did not contain a tool_calls array".to_string(),
        );
        return;
    };
    for call in calls {
        let Some(object) = call.as_object() else {
            mark_incomplete(
                accumulator,
                "batch contained a non-object tool call".to_string(),
            );
            continue;
        };
        let Some(name) = object
            .get("tool")
            .or_else(|| object.get("name"))
            .and_then(Value::as_str)
        else {
            mark_incomplete(
                accumulator,
                "batch contained a tool call without a tool name".to_string(),
            );
            continue;
        };
        let parameters = nested_batch_parameters(object);
        inspect_tool_call(name, &parameters, depth + 1, accumulator);
    }
}

fn nested_batch_parameters(object: &Map<String, Value>) -> Value {
    for key in ["parameters", "arguments", "args", "input"] {
        if let Some(value) = object.get(key)
            && value.is_object()
        {
            return value.clone();
        }
    }
    let mut flattened = object.clone();
    for key in ["tool", "name", "intent"] {
        flattened.remove(key);
    }
    Value::Object(flattened)
}

fn extract_string_path(input: &Value, key: &str, accumulator: &mut EvidenceAccumulator) {
    match input.get(key).and_then(Value::as_str) {
        Some(path) => insert_path(path, accumulator),
        None => mark_incomplete(
            accumulator,
            format!("mutating tool input did not contain a string {key} field"),
        ),
    }
}

fn insert_path(raw: &str, accumulator: &mut EvidenceAccumulator) {
    match normalize_path(raw) {
        Some(path) => {
            accumulator.paths.insert(path);
        }
        None => mark_incomplete(
            accumulator,
            "mutating tool contained an empty or unusable file path".to_string(),
        ),
    }
}

fn normalize_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let replaced = raw.replace('\\', "/");
    enum Prefix<'a> {
        Relative,
        UnixRoot,
        UncRoot,
        Drive { value: &'a str, rooted: bool },
    }
    let bytes = replaced.as_bytes();
    let (prefix, rest, rooted, protected_components) =
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let value = &replaced[..2];
            let rest = &replaced[2..];
            let rooted = rest.starts_with('/');
            (
                Prefix::Drive { value, rooted },
                rest.trim_start_matches('/'),
                rooted,
                0,
            )
        } else if replaced.starts_with("//") {
            (Prefix::UncRoot, replaced.trim_start_matches('/'), true, 2)
        } else if replaced.starts_with('/') {
            (Prefix::UnixRoot, replaced.trim_start_matches('/'), true, 0)
        } else {
            (Prefix::Relative, replaced.as_str(), false, 0)
        };
    let mut components = Vec::<String>::new();
    for component in rest.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.len() > protected_components
                    && components.last().is_some_and(|component| component != "..")
                {
                    components.pop();
                } else if !rooted {
                    components.push("..".to_string());
                }
            }
            component => components.push(component.to_string()),
        }
    }
    let joined = components.join("/");
    match prefix {
        Prefix::Relative => (!joined.is_empty()).then_some(joined),
        Prefix::UnixRoot => Some(if joined.is_empty() {
            "/".to_string()
        } else {
            format!("/{joined}")
        }),
        Prefix::UncRoot => Some(if joined.is_empty() {
            "//".to_string()
        } else {
            format!("//{joined}")
        }),
        Prefix::Drive {
            value,
            rooted: true,
        } => Some(if joined.is_empty() {
            format!("{value}/")
        } else {
            format!("{value}/{joined}")
        }),
        Prefix::Drive {
            value,
            rooted: false,
        } => Some(format!("{value}{joined}")),
    }
}

fn mark_incomplete(accumulator: &mut EvidenceAccumulator, warning: String) {
    accumulator.complete = false;
    accumulator.warnings.insert(warning);
}

fn known_non_file_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "agentgrep"
            | "bg"
            | "conversation_search"
            | "debug_socket"
            | "discover"
            | "gmail"
            | "initiative"
            | "integration_tools"
            | "invalid"
            | "jcode_docs"
            | "ls"
            | "mcp"
            | "memory"
            | "open"
            | "read"
            | "schedule"
            | "session_search"
            | "side_panel"
            | "skill_manage"
            | "todo"
            | "webfetch"
            | "websearch"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use jcode_context_core::build_message_range;

    fn tool_message(id: &str, name: &str, input: Value) -> StoredMessage {
        StoredMessage {
            id: id.to_string(),
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: format!("call-{id}"),
                name: name.to_string(),
                input,
                thought_signature: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    #[test]
    fn exact_range_extracts_every_structured_mutation_and_deduplicates_paths() {
        let messages = vec![
            tool_message(
                "write",
                "write",
                serde_json::json!({"file_path": "./src/lib.rs", "content": "x"}),
            ),
            tool_message(
                "edit",
                "edit",
                serde_json::json!({"file_path": "src/../src/lib.rs"}),
            ),
            tool_message(
                "multi",
                "multiedit",
                serde_json::json!({"file_path": "src/main.rs", "edits": []}),
            ),
            tool_message(
                "patch",
                "patch",
                serde_json::json!({
                    "patch_text": "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n"
                }),
            ),
            tool_message(
                "apply",
                "apply_patch",
                serde_json::json!({
                    "patch_text": "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n*** End Patch"
                }),
            ),
        ];
        let range = build_message_range(&messages, 0, messages.len() - 1).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert!(evidence.complete);
        assert_eq!(
            evidence.changed_files,
            vec![
                "src/a.rs",
                "src/lib.rs",
                "src/main.rs",
                "src/new.rs",
                "src/old.rs"
            ]
        );
    }

    #[test]
    fn batch_mutations_are_recursive_and_shell_is_explicitly_incomplete() {
        let messages = vec![tool_message(
            "batch",
            "batch",
            serde_json::json!({
                "tool_calls": [
                    {"tool": "write", "file_path": "src/flat.rs", "content": "x"},
                    {"tool": "edit", "parameters": {"file_path": "src/nested.rs"}},
                    {"tool": "bash", "command": "printf x > src/shell.rs"}
                ]
            }),
        )];
        let range = build_message_range(&messages, 0, 0).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert_eq!(evidence.changed_files, vec!["src/flat.rs", "src/nested.rs"]);
        assert!(!evidence.complete);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("shell commands"))
        );
    }

    #[test]
    fn evidence_never_reads_outside_the_selected_range() {
        let messages = vec![
            tool_message(
                "outside",
                "write",
                serde_json::json!({"file_path": "outside.rs"}),
            ),
            tool_message(
                "inside",
                "write",
                serde_json::json!({"file_path": "inside.rs"}),
            ),
        ];
        let range = build_message_range(&messages, 1, 1).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert_eq!(evidence.changed_files, vec!["inside.rs"]);
    }

    #[test]
    fn unified_and_codex_patches_cover_add_update_delete_and_move_paths() {
        let messages = vec![
            tool_message(
                "unified",
                "patch",
                serde_json::json!({
                    "patch_text": concat!(
                        "--- /dev/null\n+++ b/src/added.rs\n@@ -0,0 +1 @@\n+added\n",
                        "--- a/src/updated.rs\n+++ b/src/updated.rs\n@@ -1 +1 @@\n-old\n+new\n",
                        "--- a/src/deleted.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n"
                    )
                }),
            ),
            tool_message(
                "codex",
                "apply_patch",
                serde_json::json!({
                    "patch_text": concat!(
                        "*** Begin Patch\n",
                        "*** Add File: src/codex-added.rs\n+added\n",
                        "*** Update File: src/codex-old.rs\n",
                        "*** Move to: src/codex-new.rs\n",
                        "@@\n-old\n+new\n",
                        "*** Delete File: src/codex-deleted.rs\n",
                        "*** End Patch"
                    )
                }),
            ),
        ];
        let range = build_message_range(&messages, 0, 1).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert!(evidence.complete);
        assert_eq!(
            evidence.changed_files,
            vec![
                "src/added.rs",
                "src/codex-added.rs",
                "src/codex-deleted.rs",
                "src/codex-new.rs",
                "src/codex-old.rs",
                "src/deleted.rs",
                "src/updated.rs",
            ]
        );
    }

    #[test]
    fn malformed_empty_and_unknown_mutation_contracts_are_explicitly_incomplete() {
        let messages = vec![
            tool_message(
                "malformed-unified",
                "patch",
                serde_json::json!({"patch_text": "--- a/src/lib.rs\nmissing plus header"}),
            ),
            tool_message(
                "empty-codex",
                "apply_patch",
                serde_json::json!({"patch_text": "*** Begin Patch\n*** End Patch"}),
            ),
            tool_message(
                "missing-patch",
                "patch",
                serde_json::json!({"patch_text": 7}),
            ),
            tool_message(
                "unknown",
                "custom_mutator",
                serde_json::json!({"file_path": "unknown.rs"}),
            ),
            tool_message(
                "delegated",
                "swarm",
                serde_json::json!({"action": "spawn", "prompt": "Implement the change"}),
            ),
            tool_message(
                "selfdev-command",
                "selfdev",
                serde_json::json!({"action": "test", "command": "./scripts/check.sh"}),
            ),
        ];
        let range = build_message_range(&messages, 0, messages.len() - 1).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert!(!evidence.complete);
        assert!(evidence.changed_files.is_empty());
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("patch contained no parseable file paths"))
        );
        assert!(evidence.warnings.iter().any(|warning| {
            warning.contains("apply_patch")
                && (warning.contains("no parseable file paths")
                    || warning.contains("path extraction failed"))
        }));
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("string patch_text"))
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("custom_mutator"))
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("swarm")),
            "delegated work can mutate the repository outside the selected root transcript"
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("selfdev")),
            "selfdev test commands can execute shell-mediated changes"
        );
        let mut sorted = evidence.warnings.clone();
        sorted.sort();
        assert_eq!(
            evidence.warnings, sorted,
            "warning order must be deterministic"
        );
    }

    #[test]
    fn every_batch_parameter_shape_is_supported_and_malformed_nesting_is_bounded() {
        let mut excessive = serde_json::json!({
            "tool_calls": [{
                "tool": "write",
                "parameters": {"file_path": "src/too-deep.rs"}
            }]
        });
        for _ in 0..=MAX_BATCH_NESTING {
            excessive = serde_json::json!({
                "tool_calls": [{"tool": "batch", "input": excessive}]
            });
        }
        let messages = vec![tool_message(
            "batch-shapes",
            "batch",
            serde_json::json!({
                "tool_calls": [
                    {"tool": "write", "file_path": "src/flat.rs"},
                    {"tool": "write", "parameters": {"file_path": "src/parameters.rs"}},
                    {"name": "edit", "arguments": {"file_path": "src/arguments.rs"}},
                    {"tool": "multiedit", "args": {"file_path": "src/args.rs"}},
                    {"tool": "write", "input": {"file_path": "src/input.rs"}},
                    7,
                    {"parameters": {"file_path": "src/missing-name.rs"}},
                    {"tool": "batch", "parameters": excessive}
                ]
            }),
        )];
        let range = build_message_range(&messages, 0, 0).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert_eq!(
            evidence.changed_files,
            vec![
                "src/args.rs",
                "src/arguments.rs",
                "src/flat.rs",
                "src/input.rs",
                "src/parameters.rs",
            ]
        );
        assert!(!evidence.complete);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("non-object"))
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("without a tool name"))
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("nesting exceeded"))
        );
    }

    #[test]
    fn path_normalization_preserves_absolute_and_leading_parent_semantics() {
        let messages = vec![
            tool_message(
                "absolute",
                "write",
                serde_json::json!({"file_path": "/Users/test/../project/./src/lib.rs"}),
            ),
            tool_message(
                "parent",
                "edit",
                serde_json::json!({"file_path": "../../src/../lib.rs"}),
            ),
            tool_message("empty", "write", serde_json::json!({"file_path": "   "})),
            tool_message(
                "windows-drive",
                "write",
                serde_json::json!({
                    "file_path": r"C:\Users\test\..\project\src\lib.rs"
                }),
            ),
            tool_message(
                "windows-unc",
                "write",
                serde_json::json!({
                    "file_path": r"\\server\share\directory\..\file.rs"
                }),
            ),
        ];
        let range = build_message_range(&messages, 0, messages.len() - 1).expect("range");
        let evidence = extract_context_change_evidence(&messages, &range).expect("evidence");
        assert_eq!(
            evidence.changed_files,
            vec![
                "../../lib.rs",
                "//server/share/file.rs",
                "/Users/project/src/lib.rs",
                "C:/Users/project/src/lib.rs",
            ]
        );
        assert!(!evidence.complete);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("empty or unusable"))
        );
    }
}
