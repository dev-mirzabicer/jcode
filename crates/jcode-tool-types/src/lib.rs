pub mod delegation;
pub mod execution;
pub mod presentation;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ProviderReceiptReference {
    #[serde(default)]
    pub namespace: String,
    pub run_id: String,
    pub sequence: i64,
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Prepared,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Prepared | Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    ChildPredecessorFailure,
    HumanCancellation,
    ParentForegroundCancellation,
    ReloadQuiescence,
    OwnerCrash,
}

impl StopCause {
    pub fn description(self) -> &'static str {
        match self {
            Self::ChildPredecessorFailure => {
                "Cancelled after preceding child work failed or stopped"
            }
            Self::HumanCancellation => "Cancelled by user",
            Self::ParentForegroundCancellation => "Cancelled with parent foreground work",
            Self::ReloadQuiescence => {
                "Interrupted by server reload: owned work did not survive quiescence"
            }
            Self::OwnerCrash => "Interrupted by owner crash",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_receipt: Option<ProviderReceiptReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_exit: Option<ProcessExit>,
    pub output: String,
    pub title: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub images: Vec<ToolImage>,
    #[serde(default)]
    pub resources: Vec<ToolResource>,
    #[serde(default)]
    pub source: OutputSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld: Option<WithheldDelivery>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
}
impl ProcessExit {
    pub fn shell_code(&self) -> Option<i32> {
        if self.timed_out { Some(124) } else { self.code }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WithheldDelivery {
    pub estimated_output_tokens: usize,
    pub current_tokens: usize,
    pub budget: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSource {
    #[default]
    Inline,
    Retained(OutputReference),
    ReadPage(ReadPageReference),
    Acceptance(AcceptanceReference),
    Unavailable(UnavailableReference),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnavailableReference {
    pub invocation_id: String,
    pub receipt_path: std::path::PathBuf,
    pub partial_output: Option<OutputReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceReference {
    pub invocation_id: String,
    pub path: std::path::PathBuf,
    pub live_output: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputReference {
    pub invocation_id: String,
    pub path: std::path::PathBuf,
    pub bytes: u64,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
    #[serde(default)]
    pub manifest_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadPageReference {
    pub path: std::path::PathBuf,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u64,
    pub end_line: u64,
    pub retry_point: String,
    pub next_point: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolImage {
    pub media_type: String,
    pub data: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResource {
    pub uri: String,
    pub media_type: Option<String>,
    pub data: String,
}

impl ToolOutput {
    pub fn new(output: impl Into<String>) -> Self {
        Self {
            superseded: false,
            provider_receipt: None,
            output: output.into(),
            process_exit: None,
            title: None,
            metadata: None,
            images: Vec::new(),
            resources: Vec::new(),
            source: OutputSource::Inline,
            withheld: None,
            is_error: false,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_image(mut self, media_type: impl Into<String>, data: impl Into<String>) -> Self {
        self.images.push(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
            label: None,
        });
        self
    }

    pub fn with_labeled_image(
        mut self,
        media_type: impl Into<String>,
        data: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        self.images.push(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
            label: Some(label.into()),
        });
        self
    }
}

/// Resolve tool name aliases to their canonical internal names.
///
/// Providers can present tools with Claude Code aliases (e.g. `file_grep`,
/// `shell_exec`) or API namespace prefixes (e.g. `functions.bash`). Models can
/// repeat those names in sub-tool calls such as `batch`, while our registry
/// uses canonical internal names (`agentgrep`, `bash`). This mapping ensures
/// all of those forms resolve correctly.
///
/// This lives in `jcode-tool-types` (rather than the tool `Registry`) so that
/// low-level crates such as config can normalize tool names without depending
/// on the full tool subsystem.
pub fn resolve_tool_name(name: &str) -> &str {
    // Some function-calling APIs expose a recipient such as `functions.bash`.
    // Models occasionally preserve that transport namespace when constructing
    // a nested tool call, especially inside `batch`.
    let name = name.strip_prefix("functions.").unwrap_or(name);

    match name {
        "communicate" => "swarm",
        "task" | "task_runner" => "subagent",
        "launch" => "open",
        "shell" => "bash",
        "shell_exec" => "bash",
        "read_file" => "read",
        "file_read" => "read",
        "write_file" => "write",
        "file_write" => "write",
        "edit_file" => "edit",
        "file_edit" => "edit",
        // The native grep tool was removed in favor of agentgrep, but models
        // still frequently call `grep` (and OAuth's `file_grep`). agentgrep's
        // grep mode accepts `pattern` as an alias for `query`, so these calls
        // work as-is.
        "grep" | "file_grep" => "agentgrep",
        "skill" | "Skill" => "skill_manage",
        // The integration catalog tool was renamed from `discover_tools`;
        // models trained on or resuming from the old vocabulary still emit it.
        "discover_tools" => "integration_tools",
        "todoread" | "todowrite" | "todo_read" | "todo_write" | "todos" => "todo",
        // The Anthropic OAuth surface advertises PascalCase tool names and
        // reverse-maps them provider-side for top-level calls, but nested
        // `batch` subcall names bypass that mapping and resolve here (issue
        // #486). Keep these in sync with anthropic_map_tool_name_from_oauth.
        "Bash" => "bash",
        "Read" => "read",
        "Write" => "write",
        "Edit" => "edit",
        "Grep" => "agentgrep",
        "Agent" => "subagent",
        "ScheduleWakeup" => "schedule",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_name;

    #[test]
    fn resolve_tool_name_strips_function_namespace_before_alias_resolution() {
        assert_eq!(resolve_tool_name("functions.bash"), "bash");
        assert_eq!(resolve_tool_name("functions.shell_exec"), "bash");
        assert_eq!(resolve_tool_name("functions.file_grep"), "agentgrep");
    }

    #[test]
    fn resolve_tool_name_does_not_strip_unrecognized_namespaces() {
        assert_eq!(
            resolve_tool_name("mcp.functions.bash"),
            "mcp.functions.bash"
        );
    }

    #[test]
    fn resolve_tool_name_maps_pascalcase_oauth_aliases() {
        // Anthropic OAuth advertises PascalCase names; batch subcalls resolve
        // through here rather than the provider-side reverse map (issue #486).
        assert_eq!(resolve_tool_name("Read"), "read");
        assert_eq!(resolve_tool_name("Bash"), "bash");
        assert_eq!(resolve_tool_name("Write"), "write");
        assert_eq!(resolve_tool_name("Edit"), "edit");
        assert_eq!(resolve_tool_name("Grep"), "agentgrep");
        assert_eq!(resolve_tool_name("Agent"), "subagent");
        assert_eq!(resolve_tool_name("ScheduleWakeup"), "schedule");
        assert_eq!(resolve_tool_name("Skill"), "skill_manage");
        assert_eq!(resolve_tool_name("functions.Read"), "read");
    }
}
pub mod cleanup;
pub mod inspection;
