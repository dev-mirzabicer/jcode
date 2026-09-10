//! System prompt management

use std::path::{Path, PathBuf};
use std::process::Command;

/// Default system prompt for jcode (embedded at compile time)
pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("prompt/system_prompt.md");

/// Prompt guidance for the optional Mermaid rendering capability.
pub const MERMAID_PROMPT: &str = "# Mermaid\n\nRender fenced `mermaid` blocks inline.";

/// Harness capabilities that conditionally contribute prompt modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCapabilities {
    pub mermaid: bool,
}

impl Default for PromptCapabilities {
    fn default() -> Self {
        Self { mermaid: true }
    }
}

impl PromptCapabilities {
    pub fn current() -> Self {
        Self {
            mermaid: crate::config::config().features.mermaid,
        }
    }
}

/// Reasoning-effort sentinel that means "use the strongest reasoning the model
/// supports, AND actively orchestrate the work with the swarm tool". Providers
/// translate this to their strongest real effort when building API requests,
/// while the UI/session keep the literal `swarm` marker so the agent knows to
/// render the managed swarm-effort directive.
pub const SWARM_EFFORT: &str = "swarm";

/// Reasoning-effort sentinel for the **deep task graph** mode: strongest model
/// reasoning AND the comprehensive DAG-first swarm workflow (decompose into a
/// validated task graph, critique/verify gates, typed artifact handoffs). Sits
/// one rung above [`SWARM_EFFORT`] on the effort ladder: `... xhigh`, `swarm`
/// (light fan-out), `swarm-deep` (deep task graph). Providers translate this to
/// their strongest real effort, while the UI/session keep the literal marker so
/// the agent knows to render the managed swarm-deep-effort directive.
pub const SWARM_DEEP_EFFORT: &str = "swarm-deep";

/// Returns true when `effort` is either swarm sentinel (light or deep),
/// case-insensitive. Used by providers to map to the strongest real effort.
pub fn is_swarm_effort(effort: &str) -> bool {
    let trimmed = effort.trim();
    trimmed.eq_ignore_ascii_case(SWARM_EFFORT) || trimmed.eq_ignore_ascii_case(SWARM_DEEP_EFFORT)
}

/// Returns true when `effort` is specifically the deep task-graph sentinel.
pub fn is_deep_swarm_effort(effort: &str) -> bool {
    effort.trim().eq_ignore_ascii_case(SWARM_DEEP_EFFORT)
}

/// The user-facing "general effort" ladder is one list, but each rung is one of
/// two internal kinds: a plain reasoning level (mapped straight to the provider
/// wire effort) or a swarm orchestration mode (which also pins reasoning to the
/// model's max). [`EffortKind`] is the single classifier all consumers use so the
/// UI, providers, and scheduler never disagree about what a rung means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortKind {
    /// A plain reasoning level (none/low/medium/high/xhigh/max).
    Reasoning,
    /// Light swarm mode: max reasoning + parallel fan-out.
    SwarmLight,
    /// Deep swarm mode: max reasoning + DAG-first task graph.
    SwarmDeep,
}

impl EffortKind {
    /// True when this rung is a swarm orchestration mode rather than a plain
    /// reasoning level. Such rungs must not be treated as per-model effort
    /// variants (e.g. they should not generate `model (effort)` picker rows).
    pub fn is_swarm_mode(self) -> bool {
        matches!(self, EffortKind::SwarmLight | EffortKind::SwarmDeep)
    }
}

/// Classify a general-effort rung string into its [`EffortKind`].
pub fn classify_effort(effort: &str) -> EffortKind {
    let trimmed = effort.trim();
    if trimmed.eq_ignore_ascii_case(SWARM_DEEP_EFFORT) {
        EffortKind::SwarmDeep
    } else if trimmed.eq_ignore_ascii_case(SWARM_EFFORT) {
        EffortKind::SwarmLight
    } else {
        EffortKind::Reasoning
    }
}

/// True when an effort rung is a swarm orchestration mode (light or deep) rather
/// than a plain reasoning level. Convenience wrapper over [`classify_effort`].
pub fn is_swarm_mode_effort(effort: &str) -> bool {
    classify_effort(effort).is_swarm_mode()
}

/// Append the appropriate swarm directive to a split prompt's dynamic part when
/// the active reasoning effort is a swarm sentinel. The deep sentinel injects the
/// DAG-first task-graph directive; the light sentinel injects the fan-out
/// directive. No-op otherwise.
pub fn append_swarm_effort_directive(
    split: &mut SplitSystemPrompt,
    effort: Option<&str>,
    working_dir: Option<&Path>,
) -> Result<(), crate::instruction::SystemPromptActivationError> {
    if !crate::config::config().features.swarm {
        return Ok(());
    }
    use crate::instruction::workflow::Workflow;
    let resource = match effort {
        Some(effort) if is_deep_swarm_effort(effort) => Workflow::SwarmDeepEffort,
        Some(effort) if is_swarm_effort(effort) => Workflow::SwarmEffort,
        _ => return Ok(()),
    };
    let directive = resource.render(working_dir)?;
    if !split.dynamic_part.is_empty() {
        split.dynamic_part.push_str("\n\n");
    }
    split.dynamic_part.push_str(&directive);
    Ok(())
}
const SELFDEV_MODE_PROMPT: &str = include_str!("prompt/selfdev_mode.txt");
const SELFDEV_FOCUS_TUI_PROMPT: &str = include_str!("prompt/selfdev_focus_tui.txt");
const SELFDEV_FOCUS_DESKTOP2_PROMPT: &str = include_str!("prompt/selfdev_focus_desktop2.txt");
/// Split system prompt for efficient caching
/// Static content is cached, dynamic content is not
#[derive(Debug, Clone, Default)]
pub struct SplitSystemPrompt {
    /// Static content that should be cached (instruction files, base prompt, skills)
    pub static_part: String,
    /// Dynamic turn context that changes per request (memory, active skill, reminders)
    pub dynamic_part: String,
}

impl SplitSystemPrompt {
    pub fn chars(&self) -> usize {
        match (self.static_part.is_empty(), self.dynamic_part.is_empty()) {
            (true, true) => 0,
            (false, true) => self.static_part.len(),
            (true, false) => self.dynamic_part.len(),
            (false, false) => self.static_part.len() + 2 + self.dynamic_part.len(),
        }
    }

    pub fn estimated_tokens(&self) -> usize {
        crate::util::estimate_tokens(&if self.static_part.is_empty() {
            self.dynamic_part.clone()
        } else if self.dynamic_part.is_empty() {
            self.static_part.clone()
        } else {
            format!("{}\n\n{}", self.static_part, self.dynamic_part)
        })
    }
}

/// Skill info for system prompt
#[derive(Debug)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// Information about what's loaded in the context window
#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    // === Static (System Prompt) ===
    /// Base system prompt size (chars)
    pub system_prompt_chars: usize,
    /// Immutable session context size (chars), when persisted in transcript history.
    pub session_context_chars: usize,
    /// Whether project AGENTS.md was loaded
    pub has_project_agents_md: bool,
    /// Project AGENTS.md size (chars)
    pub project_agents_md_chars: usize,
    /// Whether global ~/AGENTS.md was loaded
    pub has_global_agents_md: bool,
    /// Global AGENTS.md size (chars)
    pub global_agents_md_chars: usize,
    /// Skills section size (chars)
    pub skills_chars: usize,
    /// Self-dev section size (chars)
    pub selfdev_chars: usize,
    /// Memory section size (chars)
    pub memory_chars: usize,
    /// Prompt overlay section size (chars)
    pub prompt_overlay_chars: usize,
    /// Preferred tools section size (chars)
    pub preferred_tools_chars: usize,
    // === Dynamic (Conversation) ===
    /// Tool definitions sent to API (chars)
    pub tool_defs_chars: usize,
    /// Number of tool definitions
    pub tool_defs_count: usize,
    /// User messages total size (chars)
    pub user_messages_chars: usize,
    /// Number of user messages
    pub user_messages_count: usize,
    /// Assistant messages total size (chars)
    pub assistant_messages_chars: usize,
    /// Number of assistant messages
    pub assistant_messages_count: usize,
    /// Tool calls size (chars)
    pub tool_calls_chars: usize,
    /// Number of tool calls
    pub tool_calls_count: usize,
    /// Tool results size (chars)
    pub tool_results_chars: usize,
    /// Number of tool results
    pub tool_results_count: usize,

    /// Total system prompt size (chars)
    pub total_chars: usize,
}

impl ContextInfo {
    /// Rough estimate of tokens (chars / 4 is a common approximation)
    pub fn estimated_tokens(&self) -> usize {
        self.total_chars / 4
    }

    pub fn prompt_prefix_chars(&self) -> usize {
        self.system_prompt_chars
            + self.session_context_chars
            + self.project_agents_md_chars
            + self.global_agents_md_chars
            + self.skills_chars
            + self.selfdev_chars
            + self.memory_chars
            + self.prompt_overlay_chars
            + self.preferred_tools_chars
            + self.tool_defs_chars
    }

    pub fn prompt_prefix_tokens(&self) -> usize {
        self.prompt_prefix_chars() / 4
    }

    pub fn tool_definition_tokens(&self) -> usize {
        self.tool_defs_chars / 4
    }

    /// Get breakdown as (label, chars, icon) tuples for display
    pub fn breakdown(&self) -> Vec<(&'static str, usize, &'static str)> {
        let mut parts = vec![
            ("sys", self.system_prompt_chars, "⚙"),
            ("session", self.session_context_chars, "🌍"),
        ];
        if self.has_project_agents_md {
            parts.push(("agents", self.project_agents_md_chars, "📋"));
        }
        if self.has_global_agents_md {
            parts.push(("~agents", self.global_agents_md_chars, "📋"));
        }
        if self.skills_chars > 0 {
            parts.push(("skills", self.skills_chars, "🔧"));
        }
        if self.selfdev_chars > 0 {
            parts.push(("dev", self.selfdev_chars, "🛠"));
        }
        if self.memory_chars > 0 {
            parts.push(("mem", self.memory_chars, "🧠"));
        }
        if self.prompt_overlay_chars > 0 {
            parts.push(("overlay", self.prompt_overlay_chars, "🧩"));
        }
        if self.preferred_tools_chars > 0 {
            parts.push(("tools", self.preferred_tools_chars, "🧰"));
        }
        parts
    }
}

/// Build the full system prompt with static context.
pub fn build_system_prompt(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
) -> Result<String, crate::instruction::SystemPromptActivationError> {
    build_system_prompt_with_selfdev(skill_prompt, available_skills, false)
}

/// Build the full system prompt with optional self-dev tools
pub fn build_system_prompt_with_selfdev(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
) -> Result<String, crate::instruction::SystemPromptActivationError> {
    let (prompt, _) = build_system_prompt_with_context(skill_prompt, available_skills, is_selfdev)?;
    Ok(prompt)
}

/// Build the full system prompt and return context info about what was loaded
pub fn build_system_prompt_with_context(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
) -> Result<(String, ContextInfo), crate::instruction::SystemPromptActivationError> {
    build_system_prompt_with_context_and_memory(skill_prompt, available_skills, is_selfdev, None)
}

/// Build the full system prompt with optional memory section and return context info
pub fn build_system_prompt_with_context_and_memory(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
    memory_prompt: Option<&str>,
) -> Result<(String, ContextInfo), crate::instruction::SystemPromptActivationError> {
    build_system_prompt_full(
        skill_prompt,
        available_skills,
        is_selfdev,
        memory_prompt,
        None,
    )
}

/// Build the full system prompt with working directory support for loading context files
pub fn build_system_prompt_full(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
) -> Result<(String, ContextInfo), crate::instruction::SystemPromptActivationError> {
    build_system_prompt_full_with_capabilities(
        skill_prompt,
        available_skills,
        is_selfdev,
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
    )
}

pub fn build_system_prompt_full_with_capabilities(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    capabilities: PromptCapabilities,
) -> Result<(String, ContextInfo), crate::instruction::SystemPromptActivationError> {
    crate::instruction::SystemPromptComposer::new().compatibility_full(
        skill_prompt,
        available_skills,
        is_selfdev,
        memory_prompt,
        working_dir,
        capabilities,
    )
}

/// Build system prompt split into static (cacheable) and dynamic parts
/// This improves cache hit rate by keeping frequently-changing content separate
pub fn build_system_prompt_split(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
) -> Result<(SplitSystemPrompt, ContextInfo), crate::instruction::SystemPromptActivationError> {
    build_system_prompt_split_with_capabilities(
        skill_prompt,
        available_skills,
        is_selfdev,
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
    )
}

pub fn build_system_prompt_split_with_capabilities(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    is_selfdev: bool,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    capabilities: PromptCapabilities,
) -> Result<(SplitSystemPrompt, ContextInfo), crate::instruction::SystemPromptActivationError> {
    crate::instruction::SystemPromptComposer::new().compatibility_split(
        skill_prompt,
        available_skills,
        is_selfdev,
        memory_prompt,
        working_dir,
        capabilities,
    )
}

/// Build self-dev tools prompt section (static version without dynamic socket path)
#[cfg(test)]
fn build_selfdev_prompt_static() -> String {
    build_selfdev_prompt_static_for_context(SelfDevProductContext::Tui)
}

/// Build self-dev tools prompt section
#[cfg(test)]
fn build_selfdev_prompt() -> String {
    build_selfdev_prompt_for_context(SelfDevProductContext::Tui)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfDevProductContext {
    Tui,
    Desktop2,
}

impl SelfDevProductContext {
    fn from_working_dir(working_dir: Option<&Path>) -> Self {
        let Some(working_dir) = working_dir else {
            return Self::Tui;
        };

        let path = working_dir.to_string_lossy().replace('\\', "/");
        if path.contains("/crates/jcode-desktop") || path.ends_with("crates/jcode-desktop2") {
            Self::Desktop2
        } else {
            Self::Tui
        }
    }

    fn prompt_block(self) -> &'static str {
        match self {
            Self::Tui => SELFDEV_FOCUS_TUI_PROMPT,
            Self::Desktop2 => SELFDEV_FOCUS_DESKTOP2_PROMPT,
        }
    }
}

pub(crate) fn build_selfdev_prompt_static_for_working_dir(working_dir: Option<&Path>) -> String {
    build_selfdev_prompt_static_for_context(SelfDevProductContext::from_working_dir(working_dir))
}

pub(crate) fn build_selfdev_prompt_for_working_dir(working_dir: Option<&Path>) -> String {
    build_selfdev_prompt_for_context(SelfDevProductContext::from_working_dir(working_dir))
}

fn build_selfdev_prompt_static_for_context(context: SelfDevProductContext) -> String {
    build_selfdev_prompt_for_context(context).replace("__DEBUG_SOCKET_BLOCK__\n\n", "")
}

fn build_selfdev_prompt_for_context(context: SelfDevProductContext) -> String {
    SELFDEV_MODE_PROMPT.replace("__SELFDEV_PRODUCT_FOCUS__", context.prompt_block())
}

/// Build immutable session context captured once per session.
pub fn build_session_context(working_dir: Option<&Path>) -> String {
    let mut lines = vec!["# Session Context".to_string()];

    let now_utc = chrono::Utc::now();
    lines.push(format!("Date: {}", now_utc.format("%Y-%m-%d")));
    lines.push(format!("Time: {} UTC", now_utc.format("%H:%M:%S")));
    lines.push("Timezone: UTC".to_string());
    lines.push(format!("OS: {}", std::env::consts::OS));
    lines.push(format!("Architecture: {}", std::env::consts::ARCH));
    lines.push(format!(
        "Jcode version: {} ({})",
        jcode_build_meta::version(),
        jcode_build_meta::git_hash()
    ));

    if let Some(hardware) = hardware_context() {
        lines.push(hardware);
    }

    let cwd = working_dir.map(Path::to_path_buf);
    if let Some(cwd) = cwd.as_deref() {
        lines.push(format!("Working directory: {}", cwd.display()));
        if let Some(git_info) = get_git_info(Some(cwd)) {
            lines.push(git_info);
        }
    }

    lines.join("\n")
}

/// Get git branch and status summary
fn get_git_info(working_dir: Option<&Path>) -> Option<String> {
    let mut command = Command::new("git");
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    // Check if we're in a git repo
    let in_repo = command
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !in_repo {
        return None;
    }

    let mut info = vec!["Git:".to_string()];

    // Current branch
    let mut branch_command = Command::new("git");
    if let Some(dir) = working_dir {
        branch_command.current_dir(dir);
    }
    if let Ok(output) = branch_command.args(["branch", "--show-current"]).output()
        && output.status.success()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            info.push(format!("  Branch: {}", branch));
        }
    }

    // Short status (modified files count)
    let mut status_command = Command::new("git");
    if let Some(dir) = working_dir {
        status_command.current_dir(dir);
    }
    if let Ok(output) = status_command.args(["status", "--porcelain"]).output()
        && output.status.success()
    {
        let status = String::from_utf8_lossy(&output.stdout);
        let modified: Vec<&str> = status.lines().take(5).collect();
        if !modified.is_empty() {
            info.push(format!("  Modified: {} files", status.lines().count()));
            for file in modified {
                info.push(format!("    {}", file));
            }
            if status.lines().count() > 5 {
                info.push("    ...".to_string());
            }
        }
    }

    if info.len() > 1 {
        Some(info.join("\n"))
    } else {
        None
    }
}

fn hardware_context() -> Option<String> {
    // Hardware never changes for the life of the process, but this used to be
    // rebuilt for every session create/attach, forking `lspci` each time. On a
    // busy shared server that meant one subprocess per client connection.
    static HARDWARE_CONTEXT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HARDWARE_CONTEXT
        .get_or_init(hardware_context_uncached)
        .clone()
}

fn hardware_context_uncached() -> Option<String> {
    let mut lines = Vec::new();

    if let Some(machine) = machine_model() {
        lines.push(format!("  Machine: {}", machine));
    }
    if let Some(cpu) = cpu_model() {
        lines.push(format!("  CPU: {}", cpu));
    }
    if let Some(gpu) = gpu_summary() {
        lines.push(format!("  GPU: {}", gpu));
    }
    if let Some(memory) = memory_summary() {
        lines.push(format!("  Memory: {}", memory));
    }

    if lines.is_empty() {
        None
    } else {
        let mut out = vec!["Hardware:".to_string()];
        out.extend(lines);
        Some(out.join("\n"))
    }
}

fn read_trimmed_file(path: impl Into<PathBuf>) -> Option<String> {
    std::fs::read_to_string(path.into())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn machine_model() -> Option<String> {
    let vendor = read_trimmed_file("/sys/devices/virtual/dmi/id/sys_vendor");
    let product = read_trimmed_file("/sys/devices/virtual/dmi/id/product_name");
    match (vendor, product) {
        (Some(vendor), Some(product)) if product.contains(&vendor) => Some(product),
        (Some(vendor), Some(product)) => Some(format!("{} {}", vendor, product)),
        (None, Some(product)) => Some(product),
        (Some(vendor), None) => Some(vendor),
        (None, None) => None,
    }
}

fn cpu_model() -> Option<String> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo.lines().find_map(|line| {
        let (_, value) = line.split_once(':')?;
        if line.trim_start().starts_with("model name") {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        } else {
            None
        }
    })
}

fn memory_summary() -> Option<String> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?.trim();
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })?;
    let gib = kb as f64 / 1024.0 / 1024.0;
    Some(format!("{:.1} GiB", gib))
}

fn gpu_summary() -> Option<String> {
    let output = Command::new("lspci").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut gpus: Vec<String> = text
        .lines()
        .filter(|line| {
            line.contains(" VGA compatible controller")
                || line.contains(" 3D controller")
                || line.contains(" Display controller")
        })
        .filter_map(|line| {
            line.split_once(':')
                .map(|(_, rest)| rest.trim().to_string())
        })
        .collect();
    gpus.dedup();
    if gpus.is_empty() {
        None
    } else {
        Some(gpus.join("; "))
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod prompt_tests;
