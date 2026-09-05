use crate::message::ContentBlock;
use crate::tool::{Registry, parsed_patch_file_paths};
use jcode_context_core::{ContextTargetIndex, MessageRangeResolutionError};
use jcode_session_types::{
    STORED_CONTEXT_EVIDENCE_MAX_PATH_CHARS, STORED_CONTEXT_EVIDENCE_MAX_PATHS_PER_CATEGORY,
    STORED_CONTEXT_EVIDENCE_MAX_WARNING_CHARS, STORED_CONTEXT_EVIDENCE_MAX_WARNINGS_PER_CATEGORY,
    StoredContextFileEvidence, StoredContextPathEvidence, StoredMessage, StoredMessageRange,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BATCH_NESTING: usize = 8;

/// Extract auditable file evidence from exactly one authoritative stored-message range.
///
/// Evidence is based on structured tool inputs and matching tool-result outcomes. The
/// extractor never reads the live filesystem, never upgrades a search into a read, and
/// never claims that a failed or result-less operation succeeded.
pub fn extract_context_file_evidence(
    messages: &[StoredMessage],
    range: &StoredMessageRange,
) -> Result<StoredContextFileEvidence, MessageRangeResolutionError> {
    let (start, end) = ContextTargetIndex::new(messages).resolve_message_range(range)?;
    let range_messages = &messages[start..=end];
    let results = tool_results_by_id(range_messages);
    let mut accumulator = FileEvidenceAccumulator::complete();

    for message in range_messages {
        for block in &message.content {
            let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            else {
                continue;
            };
            let outcome = top_level_tool_outcome(id, &results);
            inspect_tool_call(name, input, outcome, 0, &mut accumulator);
        }
    }

    Ok(accumulator.finish())
}

#[derive(Clone, Copy)]
struct ToolResultView<'a> {
    content: &'a str,
    is_error: bool,
}

type ToolResultsById<'a> = BTreeMap<&'a str, Vec<ToolResultView<'a>>>;

#[derive(Clone, Copy)]
enum ToolOutcome<'a> {
    Succeeded { content: &'a str },
    Failed,
    Missing,
    Ambiguous,
}

fn tool_results_by_id(messages: &[StoredMessage]) -> ToolResultsById<'_> {
    let mut results = BTreeMap::<&str, Vec<ToolResultView<'_>>>::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                results
                    .entry(tool_use_id.as_str())
                    .or_default()
                    .push(ToolResultView {
                        content,
                        is_error: is_error.unwrap_or(false),
                    });
            }
        }
    }
    results
}

fn top_level_tool_outcome<'a>(
    tool_use_id: &str,
    results: &'a ToolResultsById<'a>,
) -> ToolOutcome<'a> {
    match results.get(tool_use_id) {
        None => ToolOutcome::Missing,
        Some(items) if items.len() != 1 => ToolOutcome::Ambiguous,
        Some(items) if items[0].is_error => ToolOutcome::Failed,
        Some(items) => ToolOutcome::Succeeded {
            content: items[0].content,
        },
    }
}

#[derive(Default)]
struct FileEvidenceAccumulator {
    changed: CategoryAccumulator,
    read_or_inspected: CategoryAccumulator,
    searched_or_browsed: CategoryAccumulator,
}

impl FileEvidenceAccumulator {
    fn complete() -> Self {
        Self {
            changed: CategoryAccumulator::complete(),
            read_or_inspected: CategoryAccumulator::complete(),
            searched_or_browsed: CategoryAccumulator::complete(),
        }
    }

    fn finish(self) -> StoredContextFileEvidence {
        StoredContextFileEvidence {
            changed: self.changed.finish(),
            read_or_inspected: self.read_or_inspected.finish(),
            searched_or_browsed: self.searched_or_browsed.finish(),
        }
    }

    fn mark_all_incomplete(&mut self, warning: &str) {
        self.changed.mark_incomplete(warning);
        self.read_or_inspected.mark_incomplete(warning);
        self.searched_or_browsed.mark_incomplete(warning);
    }
}

#[derive(Default)]
struct CategoryAccumulator {
    paths: BTreeSet<String>,
    warnings: BTreeSet<String>,
    complete: bool,
    omitted_path_count: usize,
    omitted_warning_count: usize,
}

impl CategoryAccumulator {
    fn complete() -> Self {
        Self {
            complete: true,
            ..Self::default()
        }
    }

    fn insert_path(&mut self, raw: &str, kind: &str) {
        match normalize_path(raw) {
            Some(path) => self.insert_normalized(path, kind),
            None => self.mark_incomplete(&format!(
                "{kind} contained an empty or unusable path; evidence may be incomplete"
            )),
        }
    }

    fn insert_locator(&mut self, raw: &str, kind: &str) {
        let trimmed = raw.trim();
        if trimmed.contains("://") {
            if trimmed.is_empty() {
                self.mark_incomplete(&format!(
                    "{kind} contained an empty or unusable locator; evidence may be incomplete"
                ));
            } else {
                self.insert_normalized(trimmed.to_string(), kind);
            }
        } else {
            self.insert_path(trimmed, kind);
        }
    }

    fn insert_normalized(&mut self, value: String, kind: &str) {
        if value.chars().count() > STORED_CONTEXT_EVIDENCE_MAX_PATH_CHARS {
            self.mark_incomplete(&format!(
                "{kind} contained a path or locator longer than the persisted evidence limit; it was omitted"
            ));
            return;
        }
        if self.paths.contains(&value) {
            return;
        }
        if self.paths.len() < STORED_CONTEXT_EVIDENCE_MAX_PATHS_PER_CATEGORY {
            self.paths.insert(value);
        } else {
            self.complete = false;
            self.omitted_path_count = self.omitted_path_count.saturating_add(1);
        }
    }

    fn mark_incomplete(&mut self, warning: &str) {
        self.complete = false;
        let warning = truncate_chars(warning, STORED_CONTEXT_EVIDENCE_MAX_WARNING_CHARS);
        if self.warnings.contains(&warning) {
            return;
        }
        if self.warnings.len() < STORED_CONTEXT_EVIDENCE_MAX_WARNINGS_PER_CATEGORY.saturating_sub(2)
        {
            self.warnings.insert(warning);
        } else {
            self.omitted_warning_count = self.omitted_warning_count.saturating_add(1);
        }
    }

    fn finish(mut self) -> StoredContextPathEvidence {
        if self.omitted_path_count > 0 {
            self.warnings.insert(format!(
                "{} additional evidence path(s) were omitted from persisted provenance",
                self.omitted_path_count
            ));
        }
        if self.omitted_warning_count > 0 {
            self.warnings.insert(format!(
                "{} additional evidence warning(s) were omitted from persisted provenance",
                self.omitted_warning_count
            ));
        }
        StoredContextPathEvidence {
            paths: self.paths.into_iter().collect(),
            complete: self.complete,
            warnings: self.warnings.into_iter().collect(),
        }
    }
}

fn inspect_tool_call<'a>(
    tool_name: &str,
    input: &Value,
    outcome: ToolOutcome<'a>,
    depth: usize,
    accumulator: &mut FileEvidenceAccumulator,
) {
    let normalized_name = Registry::resolve_tool_name(tool_name.trim()).to_ascii_lowercase();
    if !outcome_succeeded(outcome) {
        mark_unsuccessful_tool(&normalized_name, input, outcome, accumulator);
        return;
    }
    let ToolOutcome::Succeeded { content } = outcome else {
        unreachable!("successful outcome checked above");
    };

    match normalized_name.as_str() {
        "write" | "edit" | "multiedit" => extract_string_path(
            input,
            "file_path",
            &normalized_name,
            &mut accumulator.changed,
        ),
        "patch" | "apply_patch" => {
            inspect_patch(&normalized_name, input, &mut accumulator.changed)
        }
        "read" => extract_string_path(
            input,
            "file_path",
            "read",
            &mut accumulator.read_or_inspected,
        ),
        "agentgrep" => inspect_agentgrep(input, accumulator),
        "ls" => {
            let path = input.get("path").and_then(Value::as_str).unwrap_or(".");
            accumulator
                .searched_or_browsed
                .insert_path(path, "ls directory browse");
        }
        "browser" => inspect_browser(input, accumulator),
        "open" => inspect_open(input, accumulator),
        "webfetch" => inspect_webfetch(input, accumulator),
        "batch" => inspect_batch(input, content, depth, accumulator),
        "bash" => accumulator.mark_all_incomplete(
            "shell commands occurred in the selected range; changed, read, and searched path evidence may be incomplete",
        ),
        name if known_non_file_operating_tool(name) => {}
        name => accumulator.mark_all_incomplete(&format!(
            "tool {name} has no auditable file-access contract; changed, read, and searched path evidence may be incomplete"
        )),
    }
}

fn outcome_succeeded(outcome: ToolOutcome<'_>) -> bool {
    matches!(outcome, ToolOutcome::Succeeded { .. })
}

fn mark_unsuccessful_tool(
    tool_name: &str,
    input: &Value,
    outcome: ToolOutcome<'_>,
    accumulator: &mut FileEvidenceAccumulator,
) {
    if known_non_file_operating_tool(tool_name) || browser_action_is_non_browsing(tool_name, input)
    {
        return;
    }
    let outcome_label = match outcome {
        ToolOutcome::Failed => "returned an error",
        ToolOutcome::Missing => "has no matching result in the selected range",
        ToolOutcome::Ambiguous => "has multiple matching results in the selected range",
        ToolOutcome::Succeeded { .. } => return,
    };
    let warning = format!("{tool_name} {outcome_label}; evidence does not claim that it succeeded");
    match tool_name {
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" => {
            accumulator.changed.mark_incomplete(&format!(
                "{warning}; partial mutation effects are possible, so no changed path is claimed"
            ))
        }
        "read" => accumulator.read_or_inspected.mark_incomplete(&warning),
        "agentgrep" => {
            accumulator.searched_or_browsed.mark_incomplete(&warning);
            if agentgrep_is_outline(input) {
                accumulator.read_or_inspected.mark_incomplete(&warning);
            }
        }
        "ls" | "browser" | "open" | "webfetch" => {
            accumulator.searched_or_browsed.mark_incomplete(&warning)
        }
        "batch" | "bash" => accumulator.mark_all_incomplete(&warning),
        _ => accumulator.mark_all_incomplete(&warning),
    }
}

fn inspect_patch(tool_name: &str, input: &Value, accumulator: &mut CategoryAccumulator) {
    match input.get("patch_text").and_then(Value::as_str) {
        Some(patch_text) => match parsed_patch_file_paths(tool_name, patch_text) {
            Ok(paths) if !paths.is_empty() => {
                for path in paths {
                    accumulator.insert_path(&path, tool_name);
                }
            }
            Ok(_) => accumulator.mark_incomplete(&format!(
                "{tool_name} contained no parseable file paths; changed-file evidence may be incomplete"
            )),
            Err(error) => accumulator.mark_incomplete(&format!(
                "{tool_name} path extraction failed: {error}; changed-file evidence may be incomplete"
            )),
        },
        None => accumulator.mark_incomplete(&format!(
            "{tool_name} did not contain a string patch_text field; changed-file evidence may be incomplete"
        )),
    }
}

fn inspect_agentgrep(input: &Value, accumulator: &mut FileEvidenceAccumulator) {
    let mode = input
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("grep")
        .trim()
        .to_ascii_lowercase();
    let file = input
        .get("file")
        .or_else(|| input.get("file_path"))
        .and_then(Value::as_str);
    let path = input.get("path").and_then(Value::as_str);
    let search_scope = path.or(file).unwrap_or(".");
    accumulator
        .searched_or_browsed
        .insert_path(search_scope, "agentgrep search scope");

    if mode == "outline" {
        match file {
            Some(file) => accumulator
                .read_or_inspected
                .insert_path(file, "agentgrep outline inspection"),
            None => accumulator.read_or_inspected.mark_incomplete(
                "agentgrep outline did not identify a file; inspected-file evidence may be incomplete",
            ),
        }
    }
}

fn agentgrep_is_outline(input: &Value) -> bool {
    input
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("outline"))
}

fn inspect_browser(input: &Value, accumulator: &mut FileEvidenceAccumulator) {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if browser_action_is_non_browsing("browser", input) {
        return;
    }
    if let Some(url) = input.get("url").and_then(Value::as_str) {
        accumulator
            .searched_or_browsed
            .insert_locator(url, "browser URL");
    } else if action == "upload" {
        match input.get("path").and_then(Value::as_str) {
            Some(path) => accumulator
                .searched_or_browsed
                .insert_path(path, "browser upload path"),
            None => accumulator.searched_or_browsed.mark_incomplete(
                "browser upload did not identify a path; browsed-path evidence may be incomplete",
            ),
        }
    } else {
        accumulator.searched_or_browsed.mark_incomplete(&format!(
            "browser {action} used the current page without an explicit URL; browsed-path evidence may be incomplete"
        ));
    }
}

fn browser_action_is_non_browsing(tool_name: &str, input: &Value) -> bool {
    if tool_name != "browser" {
        return false;
    }
    matches!(
        input.get("action").and_then(Value::as_str),
        Some(
            "status"
                | "setup"
                | "list_tabs"
                | "select_tab"
                | "get_active_tab"
                | "list_frames"
                | "wait"
                | "press"
        )
    )
}

fn inspect_open(input: &Value, accumulator: &mut FileEvidenceAccumulator) {
    match input.get("target").and_then(Value::as_str) {
        Some(target) => accumulator
            .searched_or_browsed
            .insert_locator(target, "open target"),
        None => accumulator.searched_or_browsed.mark_incomplete(
            "open did not identify a target; browsed-path evidence may be incomplete",
        ),
    }
}

fn inspect_webfetch(input: &Value, accumulator: &mut FileEvidenceAccumulator) {
    match input.get("url").and_then(Value::as_str) {
        Some(url) => accumulator
            .searched_or_browsed
            .insert_locator(url, "webfetch URL"),
        None => accumulator.searched_or_browsed.mark_incomplete(
            "webfetch did not identify a URL; browsed-path evidence may be incomplete",
        ),
    }
}

fn inspect_batch(
    input: &Value,
    output: &str,
    depth: usize,
    accumulator: &mut FileEvidenceAccumulator,
) {
    if depth >= MAX_BATCH_NESTING {
        accumulator.mark_all_incomplete(
            "batch nesting exceeded the file-evidence safety bound; evidence may be incomplete",
        );
        return;
    }
    let Some(calls) = input.get("tool_calls").and_then(Value::as_array) else {
        accumulator.mark_all_incomplete(
            "batch input did not contain a tool_calls array; evidence may be incomplete",
        );
        return;
    };
    for (index, call) in calls.iter().enumerate() {
        let Some(object) = call.as_object() else {
            accumulator.mark_all_incomplete(
                "batch contained a non-object tool call; evidence may be incomplete",
            );
            continue;
        };
        let Some(name) = object
            .get("tool")
            .or_else(|| object.get("name"))
            .and_then(Value::as_str)
        else {
            accumulator.mark_all_incomplete(
                "batch contained a tool call without a tool name; evidence may be incomplete",
            );
            continue;
        };
        let canonical_name = Registry::resolve_tool_name(name).to_ascii_lowercase();
        let parameters = nested_batch_parameters(object);
        let outcome = batch_subcall_outcome(output, index, name, &canonical_name);
        inspect_tool_call(
            &canonical_name,
            &parameters,
            outcome,
            depth + 1,
            accumulator,
        );
    }
}

fn batch_subcall_outcome<'a>(
    output: &'a str,
    zero_based_index: usize,
    original_name: &str,
    canonical_name: &str,
) -> ToolOutcome<'a> {
    let header_prefix = format!("--- [{}] ", zero_based_index + 1);
    let header_candidates = if original_name.eq_ignore_ascii_case(canonical_name) {
        vec![format!("{header_prefix}{canonical_name} ---")]
    } else {
        vec![
            format!("{header_prefix}{original_name} ---"),
            format!("{header_prefix}{canonical_name} ---"),
        ]
    };
    let mut matching_body_starts = Vec::new();
    let mut offset = 0usize;
    for line_with_ending in output.split_inclusive('\n') {
        let line = line_with_ending.trim_end_matches(['\r', '\n']);
        if header_candidates.iter().any(|header| line == header) {
            matching_body_starts.push(offset.saturating_add(line_with_ending.len()));
        }
        offset = offset.saturating_add(line_with_ending.len());
    }
    if matching_body_starts.is_empty() {
        return ToolOutcome::Missing;
    }
    if matching_body_starts.len() != 1 {
        return ToolOutcome::Ambiguous;
    }
    let body = output[matching_body_starts[0]..].trim_start_matches(['\r', '\n']);
    let end = body
        .find("\n--- [")
        .or_else(|| body.find("\nCompleted:"))
        .unwrap_or(body.len());
    let body = body[..end].trim();
    if body.starts_with("Error: ") {
        ToolOutcome::Failed
    } else {
        ToolOutcome::Succeeded { content: body }
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

fn extract_string_path(
    input: &Value,
    key: &str,
    tool_name: &str,
    accumulator: &mut CategoryAccumulator,
) {
    match input.get(key).and_then(Value::as_str) {
        Some(path) => accumulator.insert_path(path, tool_name),
        None => accumulator.mark_incomplete(&format!(
            "{tool_name} input did not contain a string {key} field; evidence may be incomplete"
        )),
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

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn known_non_file_operating_tool(name: &str) -> bool {
    matches!(
        name,
        "bg" | "conversation_search"
            | "debug_socket"
            | "discover"
            | "gmail"
            | "initiative"
            | "integration_tools"
            | "invalid"
            | "jcode_docs"
            | "mcp"
            | "memory"
            | "schedule"
            | "session_search"
            | "side_panel"
            | "skill_manage"
            | "todo"
            | "websearch"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use jcode_context_core::build_message_range;

    fn tool_pair(
        id: &str,
        name: &str,
        input: Value,
        output: &str,
        is_error: Option<bool>,
    ) -> Vec<StoredMessage> {
        vec![
            StoredMessage {
                origin: None,
                id: format!("use-{id}"),
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
            },
            StoredMessage {
                origin: None,
                id: format!("result-{id}"),
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("call-{id}"),
                    content: output.to_string(),
                    is_error,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            },
        ]
    }

    fn tool_use_only(id: &str, name: &str, input: Value) -> StoredMessage {
        StoredMessage {
            origin: None,
            id: format!("use-{id}"),
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

    fn evidence(messages: &[StoredMessage]) -> StoredContextFileEvidence {
        let range = build_message_range(messages, 0, messages.len() - 1).expect("range");
        extract_context_file_evidence(messages, &range).expect("evidence")
    }

    #[test]
    fn successful_structured_tools_keep_changed_read_and_searched_categories_distinct() {
        let mut messages = Vec::new();
        messages.extend(tool_pair(
            "write",
            "write",
            serde_json::json!({"file_path": "./src/lib.rs", "content": "x"}),
            "wrote file",
            None,
        ));
        messages.extend(tool_pair(
            "read",
            "read",
            serde_json::json!({"file_path": "src/../src/lib.rs"}),
            "file contents",
            None,
        ));
        messages.extend(tool_pair(
            "grep",
            "agentgrep",
            serde_json::json!({"mode": "grep", "path": "src", "query": "Parser"}),
            "matches",
            None,
        ));
        messages.extend(tool_pair(
            "outline",
            "agentgrep",
            serde_json::json!({"mode": "outline", "file": "src/parser.rs"}),
            "outline",
            None,
        ));
        messages.extend(tool_pair(
            "ls",
            "ls",
            serde_json::json!({"path": "tests"}),
            "entries",
            None,
        ));
        messages.extend(tool_pair(
            "browser",
            "browser",
            serde_json::json!({"action": "open", "url": "https://example.test/docs"}),
            "opened",
            None,
        ));
        messages.extend(tool_pair(
            "browser-upload",
            "browser",
            serde_json::json!({"action": "upload", "path": "fixtures/input.png"}),
            "uploaded",
            None,
        ));
        messages.extend(tool_pair(
            "open",
            "open",
            serde_json::json!({"action": "open", "target": "docs/README.md"}),
            "opened",
            None,
        ));
        messages.extend(tool_pair(
            "webfetch",
            "webfetch",
            serde_json::json!({"url": "https://example.test/api"}),
            "fetched",
            None,
        ));

        let evidence = evidence(&messages);
        assert_eq!(evidence.changed.paths, ["src/lib.rs"]);
        assert_eq!(
            evidence.read_or_inspected.paths,
            ["src/lib.rs", "src/parser.rs"]
        );
        assert_eq!(
            evidence.searched_or_browsed.paths,
            [
                "docs/README.md",
                "fixtures/input.png",
                "https://example.test/api",
                "https://example.test/docs",
                "src",
                "src/parser.rs",
                "tests",
            ]
        );
        assert!(evidence.changed.complete);
        assert!(evidence.read_or_inspected.complete);
        assert!(evidence.searched_or_browsed.complete);
    }

    #[test]
    fn search_never_implies_read_and_outline_is_explicit_inspection() {
        let mut messages = tool_pair(
            "grep",
            "grep",
            serde_json::json!({"path": "crates", "query": "needle"}),
            "matches",
            None,
        );
        messages.extend(tool_pair(
            "outline",
            "agentgrep",
            serde_json::json!({"mode": "outline", "file_path": "src/lib.rs"}),
            "symbols",
            None,
        ));

        let evidence = evidence(&messages);
        assert_eq!(evidence.read_or_inspected.paths, ["src/lib.rs"]);
        assert_eq!(evidence.searched_or_browsed.paths, ["crates", "src/lib.rs"]);
        assert!(
            !evidence
                .read_or_inspected
                .paths
                .contains(&"crates".to_string())
        );
    }

    #[test]
    fn failed_missing_and_duplicate_results_never_create_false_success_evidence() {
        let mut messages = tool_pair(
            "failed-write",
            "write",
            serde_json::json!({"file_path": "failed.rs"}),
            "permission denied",
            Some(true),
        );
        messages.push(tool_use_only(
            "missing-read",
            "read",
            serde_json::json!({"file_path": "missing.rs"}),
        ));
        messages.push(tool_use_only(
            "duplicate-ls",
            "ls",
            serde_json::json!({"path": "duplicate"}),
        ));
        for suffix in ["a", "b"] {
            messages.push(StoredMessage {
                origin: None,
                id: format!("duplicate-result-{suffix}"),
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call-duplicate-ls".to_string(),
                    content: "entries".to_string(),
                    is_error: None,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            });
        }

        let evidence = evidence(&messages);
        assert!(evidence.changed.paths.is_empty());
        assert!(evidence.read_or_inspected.paths.is_empty());
        assert!(evidence.searched_or_browsed.paths.is_empty());
        assert!(!evidence.changed.complete);
        assert!(!evidence.read_or_inspected.complete);
        assert!(!evidence.searched_or_browsed.complete);
        assert!(evidence.changed.warnings[0].contains("returned an error"));
        assert!(
            evidence
                .read_or_inspected
                .warnings
                .iter()
                .any(|warning| warning.contains("no matching result"))
        );
        assert!(
            evidence
                .searched_or_browsed
                .warnings
                .iter()
                .any(|warning| warning.contains("multiple matching results"))
        );
    }

    #[test]
    fn batch_uses_per_subcall_outcomes_and_preserves_category_semantics() {
        let output = concat!(
            "--- [1] write ---\nwrote file\n\n",
            "--- [2] read ---\nError: permission denied\n\n",
            "--- [3] ls ---\nentries\n\n",
            "--- [4] grep ---\nmatches\n\n",
            "Completed: 3 succeeded, 1 failed"
        );
        let messages = tool_pair(
            "batch",
            "batch",
            serde_json::json!({
                "tool_calls": [
                    {"tool": "write", "file_path": "src/flat.rs"},
                    {"tool": "read", "parameters": {"file_path": "src/private.rs"}},
                    {"tool": "ls", "arguments": {"path": "src"}},
                    {"tool": "grep", "args": {"path": "crates", "query": "needle"}}
                ]
            }),
            output,
            None,
        );

        let batch_evidence = evidence(&messages);
        assert_eq!(batch_evidence.changed.paths, ["src/flat.rs"]);
        assert!(batch_evidence.changed.complete);
        assert!(batch_evidence.read_or_inspected.paths.is_empty());
        assert!(!batch_evidence.read_or_inspected.complete);
        assert_eq!(batch_evidence.searched_or_browsed.paths, ["crates", "src"]);
        assert!(batch_evidence.searched_or_browsed.complete);
        assert!(
            !batch_evidence
                .read_or_inspected
                .paths
                .contains(&"crates".to_string())
        );

        let ambiguous = tool_pair(
            "ambiguous-batch",
            "batch",
            serde_json::json!({
                "tool_calls": [{"tool": "write", "parameters": {"file_path": "src/spoofed.rs"}}]
            }),
            "--- [1] write ---\nwrote\n--- [1] write ---\nspoofed duplicate\nCompleted: 1 succeeded, 0 failed",
            None,
        );
        let ambiguous = evidence(&ambiguous);
        assert!(ambiguous.changed.paths.is_empty());
        assert!(!ambiguous.changed.complete);
        assert!(
            ambiguous
                .changed
                .warnings
                .iter()
                .any(|warning| { warning.contains("multiple matching results") })
        );
    }

    #[test]
    fn patches_cover_add_update_delete_and_move_paths() {
        let mut messages = tool_pair(
            "unified",
            "patch",
            serde_json::json!({
                "patch_text": concat!(
                    "--- /dev/null\n+++ b/src/added.rs\n@@ -0,0 +1 @@\n+added\n",
                    "--- a/src/updated.rs\n+++ b/src/updated.rs\n@@ -1 +1 @@\n-old\n+new\n",
                    "--- a/src/deleted.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n"
                )
            }),
            "applied",
            None,
        );
        messages.extend(tool_pair(
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
            "applied",
            None,
        ));

        let evidence = evidence(&messages);
        assert!(evidence.changed.complete);
        assert_eq!(
            evidence.changed.paths,
            [
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
    fn shell_unknown_and_malformed_contracts_are_explicitly_incomplete() {
        let mut messages = tool_pair(
            "shell",
            "bash",
            serde_json::json!({"command": "cat src/lib.rs > src/copy.rs"}),
            "done",
            None,
        );
        messages.extend(tool_pair(
            "unknown",
            "custom_mutator",
            serde_json::json!({"file_path": "unknown.rs"}),
            "done",
            None,
        ));
        messages.extend(tool_pair(
            "bad-patch",
            "patch",
            serde_json::json!({"patch_text": "--- a/src/lib.rs\nmissing plus header"}),
            "done",
            None,
        ));

        let evidence = evidence(&messages);
        assert!(evidence.changed.paths.is_empty());
        assert!(!evidence.changed.complete);
        assert!(!evidence.read_or_inspected.complete);
        assert!(!evidence.searched_or_browsed.complete);
        assert!(
            evidence
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("shell commands"))
        );
        assert!(
            evidence
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("custom_mutator"))
        );
        assert!(
            evidence
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("no parseable file paths"))
        );
    }

    #[test]
    fn evidence_never_uses_tool_calls_or_results_outside_the_selected_range() {
        let mut messages = tool_pair(
            "outside",
            "write",
            serde_json::json!({"file_path": "outside.rs"}),
            "wrote",
            None,
        );
        let inside_start = messages.len();
        messages.extend(tool_pair(
            "inside",
            "read",
            serde_json::json!({"file_path": "inside.rs"}),
            "contents",
            None,
        ));
        let inside_end = messages.len() - 1;
        let range = build_message_range(&messages, inside_start, inside_end).expect("range");
        let evidence = extract_context_file_evidence(&messages, &range).expect("evidence");
        assert!(evidence.changed.paths.is_empty());
        assert_eq!(evidence.read_or_inspected.paths, ["inside.rs"]);
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
        let output = concat!(
            "--- [1] write ---\nok\n\n",
            "--- [2] write ---\nok\n\n",
            "--- [3] edit ---\nok\n\n",
            "--- [4] multiedit ---\nok\n\n",
            "--- [5] write ---\nok\n\n",
            "--- [6] batch ---\nmissing nested execution detail\n\n",
            "Completed: 6 succeeded, 0 failed"
        );
        let messages = tool_pair(
            "batch-shapes",
            "batch",
            serde_json::json!({
                "tool_calls": [
                    {"tool": "write", "file_path": "src/flat.rs"},
                    {"tool": "write", "parameters": {"file_path": "src/parameters.rs"}},
                    {"name": "edit", "arguments": {"file_path": "src/arguments.rs"}},
                    {"tool": "multiedit", "args": {"file_path": "src/args.rs"}},
                    {"tool": "write", "input": {"file_path": "src/input.rs"}},
                    {"tool": "batch", "parameters": excessive},
                    7,
                    {"parameters": {"file_path": "src/missing-name.rs"}}
                ]
            }),
            output,
            None,
        );

        let evidence = evidence(&messages);
        assert_eq!(
            evidence.changed.paths,
            [
                "src/args.rs",
                "src/arguments.rs",
                "src/flat.rs",
                "src/input.rs",
                "src/parameters.rs",
            ]
        );
        assert!(!evidence.changed.complete);
        assert!(
            evidence
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("non-object"))
        );
        assert!(
            evidence
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("without a tool name"))
        );
        let mut bounded = FileEvidenceAccumulator::complete();
        inspect_batch(&excessive, "", MAX_BATCH_NESTING, &mut bounded);
        let bounded = bounded.finish();
        assert!(
            bounded
                .changed
                .warnings
                .iter()
                .any(|warning| warning.contains("nesting exceeded"))
        );
    }

    #[test]
    fn path_normalization_preserves_absolute_parent_windows_and_url_semantics() {
        let mut messages = tool_pair(
            "absolute",
            "write",
            serde_json::json!({"file_path": "/Users/test/../project/./src/lib.rs"}),
            "wrote",
            None,
        );
        messages.extend(tool_pair(
            "parent",
            "edit",
            serde_json::json!({"file_path": "../../src/../lib.rs"}),
            "edited",
            None,
        ));
        messages.extend(tool_pair(
            "windows-drive",
            "write",
            serde_json::json!({"file_path": r"C:\Users\test\..\project\src\lib.rs"}),
            "wrote",
            None,
        ));
        messages.extend(tool_pair(
            "windows-unc",
            "write",
            serde_json::json!({"file_path": r"\\server\share\directory\..\file.rs"}),
            "wrote",
            None,
        ));
        messages.extend(tool_pair(
            "url",
            "webfetch",
            serde_json::json!({"url": "https://example.test/a/../b"}),
            "fetched",
            None,
        ));
        messages.extend(tool_pair(
            "empty",
            "read",
            serde_json::json!({"file_path": "   "}),
            "contents",
            None,
        ));

        let evidence = evidence(&messages);
        assert_eq!(
            evidence.changed.paths,
            [
                "../../lib.rs",
                "//server/share/file.rs",
                "/Users/project/src/lib.rs",
                "C:/Users/project/src/lib.rs",
            ]
        );
        assert_eq!(
            evidence.searched_or_browsed.paths,
            ["https://example.test/a/../b"]
        );
        assert!(!evidence.read_or_inspected.complete);
        assert!(
            evidence
                .read_or_inspected
                .warnings
                .iter()
                .any(|warning| warning.contains("empty or unusable"))
        );
    }

    #[test]
    fn persisted_path_and_warning_provenance_is_bounded_without_false_completeness() {
        let mut category = CategoryAccumulator::complete();
        for index in 0..STORED_CONTEXT_EVIDENCE_MAX_PATHS_PER_CATEGORY + 3 {
            category.insert_path(&format!("src/generated/{index}.rs"), "write");
        }
        category.insert_locator(
            &format!(
                "https://example.test/{}",
                "x".repeat(STORED_CONTEXT_EVIDENCE_MAX_PATH_CHARS + 1)
            ),
            "browser",
        );
        for index in 0..STORED_CONTEXT_EVIDENCE_MAX_WARNINGS_PER_CATEGORY + 5 {
            category.mark_incomplete(&format!("warning {index}"));
        }
        let evidence = category.finish();
        assert_eq!(
            evidence.paths.len(),
            STORED_CONTEXT_EVIDENCE_MAX_PATHS_PER_CATEGORY
        );
        assert!(!evidence.complete);
        assert!(evidence.warnings.len() <= STORED_CONTEXT_EVIDENCE_MAX_WARNINGS_PER_CATEGORY);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| { warning.contains("3 additional evidence path(s) were omitted") })
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| { warning.contains("additional evidence warning(s) were omitted") })
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| { warning.contains("longer than the persisted evidence limit") })
        );
    }
}
