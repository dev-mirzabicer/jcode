mod agentgrep;
pub mod ambient;
mod apply_patch;
mod background_notice;
mod bash;
pub(crate) use bash::parse_command_progress;
#[cfg(unix)]
pub(crate) use bash::tool_scratch_dir;
mod batch;
mod bg;
mod browser;
mod communicate;
#[cfg(target_os = "macos")]
mod computer;
mod config_edit_notice;
mod conversation_search;
mod debug_socket;
mod discover;
mod discover_secrets;
mod edit;
mod gmail;
mod goal;
pub mod inflight;
pub mod instruction_guidance;
mod invalid;
mod jcode_docs;
mod ls;
pub mod mcp;
mod memory;
mod multiedit;
mod mutation_diff;
mod mutation_output;
mod open;
mod patch;
mod read;
pub mod selfdev;
pub(crate) mod serde_coerce;
mod session_search;
pub(crate) mod session_search_index;
mod side_panel;
mod skill;
mod todo;
mod webfetch;
mod websearch;
mod write;

use crate::context_budget::ContextBudgetTracker;
use crate::provider::Provider;
use crate::skill::SkillRegistry;
use anyhow::Result;
use jcode_message_types::ToolDefinition;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn tool_name_is_allowed(allowed: &HashSet<String>, name: &str) -> bool {
    allowed.contains(name) || (allowed.contains("mcp") && name.starts_with("mcp__"))
}

pub(crate) fn tool_name_is_disabled(disabled: &HashSet<String>, name: &str) -> bool {
    disabled.contains(name) || (disabled.contains("mcp") && name.starts_with("mcp__"))
}
use std::sync::{LazyLock, RwLock as StdRwLock};
use tokio::sync::RwLock;

pub(crate) use jcode_tool_core::intent_schema_property;
pub use jcode_tool_core::{StdinInputRequest, Tool, ToolContext, ToolExecutionMode};
pub use jcode_tool_types::{ToolImage, ToolOutput};
pub(crate) use session_search::spawn_recent_index_warmup;

pub(crate) fn parsed_patch_file_paths(tool_name: &str, patch_text: &str) -> Result<Vec<String>> {
    match tool_name {
        "patch" => patch::parsed_file_paths(patch_text),
        "apply_patch" => apply_patch::parsed_file_paths(patch_text),
        _ => Err(anyhow::anyhow!("unsupported patch tool: {tool_name}")),
    }
}

#[derive(Clone, Debug, Default)]
struct SessionToolPolicy {
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
}

static SESSION_TOOL_POLICIES: LazyLock<StdRwLock<HashMap<String, SessionToolPolicy>>> =
    LazyLock::new(|| StdRwLock::new(HashMap::new()));

pub(crate) fn set_session_tool_policy(
    session_id: &str,
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.insert(
        session_id.to_string(),
        SessionToolPolicy {
            allowed_tools,
            disabled_tools,
        },
    );
}

pub(crate) fn clear_session_tool_policy(session_id: &str) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.remove(session_id);
}

fn session_tool_policy(session_id: &str) -> Option<SessionToolPolicy> {
    SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
}

/// Global availability overrides both session policy and previously cached registries.
pub fn tool_is_globally_available(name: &str) -> bool {
    jcode_tool_types::resolve_tool_name(name) != "swarm" || crate::config::config().features.swarm
}

/// Registry of available tools (Arc-wrapped for sharing)
///
/// Clone creates fresh context accounting so each subagent gets independent
/// provider-history tracking. Tools and skills are shared via Arc.
pub struct Registry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    skills: Arc<RwLock<SkillRegistry>>,
    context_budget: Arc<RwLock<ContextBudgetTracker>>,
    bindings: Arc<StdRwLock<HashMap<String, BoundTool>>>,
}

#[derive(Clone)]
struct BoundTool {
    tool: Arc<dyn Tool>,
    definition: ToolDefinition,
    input: jcode_tool_core::input::InputBinding,
}

impl Clone for Registry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            // Each clone gets fresh session-local accounting so parallel
            // subagents cannot corrupt one another.
            context_budget: Arc::new(RwLock::new(ContextBudgetTracker::new())),
            bindings: Default::default(),
        }
    }
}

impl Registry {
    pub(crate) async fn retained_history_result(
        &self,
        name: &str,
        ctx: ToolContext,
        input: Value,
    ) -> Result<Option<ToolOutput>> {
        let id = crate::execution::invocation_id(&ctx);
        let root = crate::storage::jcode_dir()?;
        let name = Self::resolve_tool_name(name).to_string();
        let target = crate::config::config().output.target(&name, None);
        let lookup_name = name.clone();
        let found=tokio::task::spawn_blocking(move||->Result<_> {
            if !root.join("execution/index.sqlite").try_exists()?{return Ok(None);}
            let store=crate::execution::ExecutionStore::open(&root)?;
            let Some(record)=store.inspect(&id)? else{return Ok(None);};
            anyhow::ensure!(record.session_id==ctx.session_id && record.message_id==ctx.message_id && Self::resolve_tool_name(&record.tool)==lookup_name,"Retained execution does not match this historical tool use");
            anyhow::ensure!(store.invocation_input(&id)?.input==input,"Retained execution input differs from the historical tool use; no result was substituted");
            let accepted=store.acceptance_result(&id)?;
            Ok(Some((store,record,accepted)))
        }).await??;
        let Some((store, mut record, accepted)) = found else {
            return Ok(None);
        };
        let output = if let Some(accepted) = accepted {
            accepted
        } else {
            if !record.state.terminal() {
                record=store.recover_lost_owner(&record.id).await?.ok_or_else(||anyhow::anyhow!("Execution {} remains active or unverified. No replacement result or repeated operation was created.",record.id))?;
            }
            tokio::task::spawn_blocking(move || store.result(&record, target)).await??
        };
        Ok(Some(self.guard_context_overflow(&name, output).await))
    }
    /// The provider has already performed the operation. This boundary only
    /// retains and presents received output; it never invokes a native tool.
    pub async fn retain_provider_result(
        &self,
        name: &str,
        input: Value,
        mut ctx: ToolContext,
        output: ToolOutput,
    ) -> Result<ToolOutput> {
        if let Some(receipt) = output.provider_receipt.clone() {
            let root = crate::storage::jcode_dir()?;
            let session = ctx.session_id.clone();
            let tool_id = ctx.tool_call_id.clone();
            let message = ctx.message_id.clone();
            tokio::task::spawn_blocking(move || {
                crate::execution::ExecutionStore::open(&root)?
                    .correlate_provider_receipt(&receipt, &session, &tool_id, &message)
            })
            .await??;
        }
        let target = crate::config::config()
            .output
            .target(Self::resolve_tool_name(name), None);
        ctx.invocation.policy = Default::default();
        ctx.invocation.output_target = Some(target);
        ctx.graceful_shutdown_signal = None;
        let mut invocation = crate::execution::invocation(&ctx, name, input);
        invocation.received_result_digest = Some(
            crate::execution::output_digest(&output)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
        let result = crate::execution::execute(
            invocation,
            ctx,
            target,
            Box::new(move |_| Box::pin(async move { Ok(output) })),
        )
        .await;
        match result {
            Ok(output) => Ok(self.guard_context_overflow(name, output).await),
            Err(error) => {
                if let Some(captured) = error.downcast_ref::<crate::execution::CapturedToolError>()
                    && matches!(&captured.output.source,jcode_tool_types::OutputSource::Retained(reference) if reference.complete)
                {
                    Ok(self
                        .guard_context_overflow(name, captured.output.clone().with_error(true))
                        .await)
                } else {
                    Err(error)
                }
            }
        }
    }
    /// Clone this registry for work that remains part of the same session.
    ///
    /// Ordinary [`Clone`] deliberately creates fresh context accounting and
    /// for a new agent/session. Tool execution tasks, batch subcalls, and
    /// same-session control operations must instead share the current runtime
    /// state or the large-output guard will silently fall back to an empty
    /// default tracker.
    pub fn clone_with_shared_context_runtime(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            context_budget: self.context_budget.clone(),
            bindings: self.bindings.clone(),
        }
    }

    fn shared_skills_registry() -> Arc<RwLock<SkillRegistry>> {
        SkillRegistry::shared_registry()
    }

    fn bound_tool(&self, name: &str, tool: Arc<dyn Tool>) -> BoundTool {
        let mut bindings = self.bindings.write().unwrap_or_else(|p| p.into_inner());
        bindings
            .entry(name.to_string())
            .or_insert_with(|| {
                let input = tool.input_binding();
                let mut definition = tool.to_definition();
                definition.name = name.to_string();
                BoundTool {
                    tool,
                    definition,
                    input,
                }
            })
            .clone()
    }

    pub(super) async fn input_binding_for(
        &self,
        name: &str,
    ) -> Option<jcode_tool_core::input::InputBinding> {
        if !tool_is_globally_available(name) {
            return None;
        }
        let name = Self::resolve_tool_name(name);
        let tool = self.tools.read().await.get(name).cloned()?;
        Some(self.bound_tool(name, tool).input)
    }

    fn insert_tool<T>(tools: &mut HashMap<String, Arc<dyn Tool>>, name: &str, tool: T)
    where
        T: Tool + 'static,
    {
        tools.insert(name.into(), Arc::new(tool) as Arc<dyn Tool>);
    }

    fn insert_tool_timed<T>(
        tools: &mut HashMap<String, Arc<dyn Tool>>,
        timings: &mut Vec<(String, u128)>,
        name: &str,
        make_tool: impl FnOnce() -> T,
    ) where
        T: Tool + 'static,
    {
        let start = std::time::Instant::now();
        Self::insert_tool(tools, name, make_tool());
        timings.push((name.to_string(), start.elapsed().as_millis()));
    }

    /// Create a lightweight empty registry (no tools, no skill loading).
    /// Used by remote-mode clients that don't execute tools locally.
    pub fn empty() -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            skills: Arc::new(RwLock::new(SkillRegistry::default())),
            context_budget: Arc::new(RwLock::new(ContextBudgetTracker::new())),
            bindings: Default::default(),
        }
    }

    /// Base tools that are stateless and can be shared across sessions.
    /// Created once and cached in a OnceLock, then cloned (cheap Arc bumps) per session.
    fn base_tools(skills: &Arc<RwLock<SkillRegistry>>) -> HashMap<String, Arc<dyn Tool>> {
        use std::sync::OnceLock;
        static BASE: OnceLock<HashMap<String, Arc<dyn Tool>>> = OnceLock::new();
        let base = BASE.get_or_init(|| {
            let init_start = std::time::Instant::now();
            let mut timings = Vec::new();
            let mut m = HashMap::new();
            Self::insert_tool_timed(&mut m, &mut timings, "read", read::ReadTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "write", write::WriteTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "agentgrep",
                agentgrep::AgentGrepTool::new,
            );
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "side_panel",
                side_panel::SidePanelTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "edit", edit::EditTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "multiedit",
                multiedit::MultiEditTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "patch", patch::PatchTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "apply_patch",
                apply_patch::ApplyPatchTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "ls", ls::LsTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "bash", bash::BashTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "browser", browser::BrowserTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "open", open::OpenTool::new);
            #[cfg(target_os = "macos")]
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "macos_computer_use",
                computer::ComputerTool::new,
            );
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "webfetch",
                webfetch::WebFetchTool::new,
            );
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "websearch",
                websearch::WebSearchTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "invalid", invalid::InvalidTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "jcode_docs",
                jcode_docs::JcodeDocsTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "todo", todo::TodoTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "bg", bg::BgTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "swarm",
                communicate::CommunicateTool::new,
            );
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "session_search",
                session_search::SessionSearchTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "memory", memory::MemoryTool::new);
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "initiative",
                goal::InitiativeTool::new,
            );
            Self::insert_tool_timed(&mut m, &mut timings, "gmail", gmail::GmailTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "schedule", ambient::ScheduleTool::new);
            Self::insert_tool_timed(&mut m, &mut timings, "selfdev", selfdev::SelfDevTool::new);
            let nonzero: Vec<String> = timings
                .iter()
                .filter(|(_, ms)| *ms > 0)
                .map(|(name, ms)| format!("{name}={ms}ms"))
                .collect();
            crate::logging::info(&format!(
                "[TIMING] registry_base_tools_init: total={}ms, nonzero=[{}]",
                init_start.elapsed().as_millis(),
                nonzero.join(", ")
            ));
            m
        });
        // Clone the Arc entries (cheap refcount bumps, not deep copies)
        let mut tools = base.clone();
        if !crate::config::config().features.memory {
            tools.remove("memory");
        }
        if !crate::config::config().features.swarm {
            tools.remove("swarm");
        }
        // SkillTool needs the skills registry reference (shared across sessions)
        Self::insert_tool(
            &mut tools,
            "skill_manage",
            skill::SkillTool::new(skills.clone()),
        );
        tools
    }

    pub async fn new(_provider: Arc<dyn Provider>) -> Self {
        let start = std::time::Instant::now();
        let skills_start = std::time::Instant::now();
        let skills = Self::shared_skills_registry();
        let skills_ms = skills_start.elapsed().as_millis();
        let context_runtime_start = std::time::Instant::now();
        let context_budget = Arc::new(RwLock::new(ContextBudgetTracker::new()));
        let context_runtime_ms = context_runtime_start.elapsed().as_millis();
        let registry_struct_start = std::time::Instant::now();
        let registry = Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            skills: skills.clone(),
            context_budget: context_budget.clone(),
            bindings: Default::default(),
        };
        let registry_struct_ms = registry_struct_start.elapsed().as_millis();

        let base_start = std::time::Instant::now();
        let mut tools_map = Self::base_tools(&skills);
        let base_ms = base_start.elapsed().as_millis();

        // Per-session tools that need provider/registry references
        let session_tools_start = std::time::Instant::now();
        Self::insert_tool(
            &mut tools_map,
            "batch",
            batch::BatchTool::new(registry.clone_with_shared_context_runtime()),
        );
        Self::insert_tool(
            &mut tools_map,
            "conversation_search",
            conversation_search::ConversationSearchTool::new(context_budget),
        );
        // Integration discovery is on by default (opt-out); when disabled the
        // tool is never registered and no discovery endpoint is ever
        // contacted.
        if crate::config::config().sponsors.enabled {
            Self::insert_tool(
                &mut tools_map,
                "integration_tools",
                discover::DiscoverToolsTool::new(),
            );
        }
        let session_tools_ms = session_tools_start.elapsed().as_millis();

        let write_start = std::time::Instant::now();
        *registry.tools.write().await = tools_map;
        let write_ms = write_start.elapsed().as_millis();
        crate::logging::info(&format!(
            "[TIMING] registry_new: skills={}ms, context_runtime={}ms, registry_struct={}ms, base_tools={}ms, session_tools={}ms, write={}ms, total={}ms",
            skills_ms,
            context_runtime_ms,
            registry_struct_ms,
            base_ms,
            session_tools_ms,
            write_ms,
            start.elapsed().as_millis()
        ));
        registry
    }

    /// Get all tool definitions for the API
    pub async fn definitions(
        &self,
        allowed_tools: Option<&HashSet<String>>,
    ) -> Vec<ToolDefinition> {
        let tools = self.tools.read().await;
        let mut defs: Vec<ToolDefinition> = tools
            .iter()
            .filter(|(name, _)| tool_is_globally_available(name))
            .filter(|(name, _)| {
                allowed_tools
                    .map(|set| tool_name_is_allowed(set, name))
                    .unwrap_or(true)
            })
            .map(|(name, tool)| self.bound_tool(name, Arc::clone(tool)).definition)
            .collect();

        // Sort by name for deterministic ordering - critical for prompt cache hits
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Read the current deterministic tool definitions without waiting. Idle
    /// control commands use this to preflight a prompt replacement without
    /// introducing an async command path or mutating the tool registry.
    pub fn try_definitions(
        &self,
        allowed_tools: Option<&HashSet<String>>,
    ) -> Result<Vec<ToolDefinition>, &'static str> {
        let tools = self
            .tools
            .try_read()
            .map_err(|_| "tool definitions are currently being updated")?;
        let mut definitions = tools
            .iter()
            .filter(|(name, _)| tool_is_globally_available(name))
            .filter(|(name, _)| {
                allowed_tools
                    .map(|set| tool_name_is_allowed(set, name))
                    .unwrap_or(true)
            })
            .map(|(name, tool)| self.bound_tool(name, Arc::clone(tool)).definition)
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(definitions)
    }

    pub async fn tool_names(&self) -> Vec<String> {
        let tools = self.tools.read().await;
        tools
            .keys()
            .filter(|name| tool_is_globally_available(name))
            .cloned()
            .collect()
    }

    /// Enable test mode for memory tools (isolated storage)
    /// Called when session is marked as debug
    pub async fn enable_memory_test_mode(&self) {
        if !crate::config::config().features.memory {
            crate::logging::info(
                "Memory test mode not enabled because memory is globally unavailable",
            );
            return;
        }
        let mut tools = self.tools.write().await;

        // Replace memory tool with test version
        tools.insert(
            "memory".to_string(),
            Arc::new(memory::MemoryTool::new_test()) as Arc<dyn Tool>,
        );

        crate::logging::info("Memory test mode enabled - using isolated storage");
    }

    /// Resolve tool name aliases.
    ///
    /// When using OAuth, the API presents tools with Claude Code names
    /// (e.g. `file_grep`, `shell_exec`). The model uses those names in
    /// sub-tool calls (e.g. inside `batch`), but our registry uses internal
    /// names (`grep`, `bash`). This mapping ensures both forms resolve
    /// correctly.
    ///
    /// The canonical mapping lives in `jcode-tool-types::resolve_tool_name` so
    /// lower-level crates (e.g. config) can normalize tool names without
    /// depending on the tool subsystem; this method delegates to it.
    pub(crate) fn resolve_tool_name(name: &str) -> &str {
        jcode_tool_types::resolve_tool_name(name)
    }

    /// Suggest up to 3 available tool names that look similar to `name`.
    /// Uses cheap, dependency-free heuristics: case-insensitive equality,
    /// prefix/substring containment, then bounded edit distance. Helps the
    /// model recover from hallucinated tool names (#104).
    fn closest_tool_names(name: &str, available: &[&str]) -> Vec<String> {
        let needle = name.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &str)> = available
            .iter()
            .filter_map(|candidate| {
                let hay = candidate.to_ascii_lowercase();
                let score = if hay == needle {
                    0
                } else if hay.starts_with(&needle) || needle.starts_with(&hay) {
                    1
                } else if hay.contains(&needle) || needle.contains(&hay) {
                    2
                } else {
                    let dist = levenshtein(&needle, &hay);
                    // Only suggest near-misses, scaled to the longer name.
                    let threshold = (hay.len().max(needle.len()) / 3).max(2);
                    if dist <= threshold {
                        3 + dist
                    } else {
                        return None;
                    }
                };
                Some((score, *candidate))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(3)
            .map(|(_, name)| name.to_string())
            .collect()
    }

    /// Estimate token count for a string (chars / 4, matching compaction heuristic)
    fn estimate_tokens(s: &str) -> usize {
        crate::util::estimate_tokens(s)
    }

    fn tool_lifecycle_fields(
        phase: &str,
        requested_name: &str,
        resolved_name: &str,
        input: &Value,
        ctx: &ToolContext,
    ) -> Vec<(String, String)> {
        let cwd = ctx
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_string());
        let input_json = serde_json::to_string(input).unwrap_or_default();
        let mut fields = vec![
            ("phase".to_string(), phase.to_string()),
            ("tool_name".to_string(), requested_name.to_string()),
            ("resolved_tool_name".to_string(), resolved_name.to_string()),
            ("session_id".to_string(), ctx.session_id.clone()),
            ("message_id".to_string(), ctx.message_id.clone()),
            ("tool_call_id".to_string(), ctx.tool_call_id.clone()),
            (
                "execution_mode".to_string(),
                format!("{:?}", ctx.execution_mode),
            ),
            ("cwd".to_string(), cwd),
            ("input_json_bytes".to_string(), input_json.len().to_string()),
        ];

        if let Some(object) = input.as_object() {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            fields.push(("input_keys".to_string(), keys.join(",")));

            let path_fields = [
                "file_path",
                "path",
                "target",
                "target_path",
                "old_path",
                "new_path",
            ];
            let mut touched_paths = Vec::new();
            for key in path_fields {
                if let Some(path) = object.get(key).and_then(Value::as_str) {
                    touched_paths.push(format!(
                        "{key}:{}",
                        ctx.resolve_path(std::path::Path::new(path)).display()
                    ));
                }
            }
            if let Some(paths) = object.get("paths").and_then(Value::as_array) {
                for path in paths.iter().filter_map(Value::as_str).take(8) {
                    touched_paths.push(format!(
                        "paths:{}",
                        ctx.resolve_path(std::path::Path::new(path)).display()
                    ));
                }
            }
            if !touched_paths.is_empty() {
                fields.push(("touched_paths".to_string(), touched_paths.join(",")));
                fields.push((
                    "touched_path_count".to_string(),
                    touched_paths.len().to_string(),
                ));
            }

            for text_key in ["command", "prompt", "task", "query", "content"] {
                if let Some(text) = object.get(text_key).and_then(Value::as_str) {
                    fields.push((format!("{text_key}_bytes"), text.len().to_string()));
                    fields.push((
                        format!("{text_key}_chars"),
                        text.chars().count().to_string(),
                    ));
                }
            }
        }

        fields
    }

    /// Maximum fraction of context budget a single tool output may consume.
    /// Outputs that would push total context beyond this are truncated.
    const CONTEXT_GUARD_THRESHOLD: f32 = 0.90;

    /// Fire the `post_tool` observer hook with tool outcome metadata.
    /// No-op (without building the payload) when the hook is not configured.
    fn fire_post_tool_hook(
        resolved_name: &str,
        ctx: &ToolContext,
        result: &Result<ToolOutput>,
        latency_ms: u64,
    ) {
        if !crate::hooks::hook_configured("post_tool") {
            return;
        }
        let mut event = crate::hooks::HookEvent::new("post_tool")
            .session_id(ctx.session_id.clone())
            .field("TOOL_NAME", resolved_name)
            .field(
                "STATUS",
                if result.as_ref().is_ok_and(|output| !output.is_error) {
                    "ok"
                } else {
                    "error"
                },
            )
            .field("DURATION_MS", latency_ms.to_string());
        if let Some(dir) = &ctx.working_dir {
            event = event.cwd(dir.display().to_string());
        }
        match result {
            Ok(output) => {
                event = event.field("OUTPUT_BYTES", output.output.len().to_string());
            }
            Err(error) => {
                const ERROR_LIMIT: usize = 1000;
                let message: String = error.to_string().chars().take(ERROR_LIMIT).collect();
                event = event.field("ERROR", message);
            }
        }
        crate::hooks::dispatch_observer(event);
    }

    /// Maximum fraction of context budget a single tool output may occupy.
    /// Even if we have room, a single output shouldn't dominate the context.
    const SINGLE_OUTPUT_MAX_FRACTION: f32 = 0.30;

    /// Hard ceiling on a single tool output, independent of context budget.
    ///
    /// A fraction alone is not enough. On a model reporting a 1M-token window,
    /// 30% permits a 300k-token single result, so a repo-wide grep sailed
    /// through the guard and cost 233k tokens in one call. No individual tool
    /// result is worth that much of any window: past roughly 50k tokens the
    /// caller is reading a haystack, not an answer, and should narrow the query.
    /// The effective ceiling is the smaller of this and the budget fraction, so
    /// small windows still get proportional protection.
    const SINGLE_OUTPUT_MAX_TOKENS: usize = 50_000;

    /// Execute a tool by name
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        mut ctx: ToolContext,
    ) -> Result<ToolOutput> {
        if !tool_is_globally_available(name) {
            anyhow::bail!(crate::config::SWARM_UNAVAILABLE);
        }
        // Mark this call in-flight for the whole execution so the missing
        // tool-output repair paths do not mistake a slow tool for an
        // interrupted one and inject a duplicate synthetic result. See
        // `tool::inflight`.
        let _in_flight =
            inflight::mark_tool_in_flight(&crate::execution::invocation(&ctx, name, Value::Null));
        let tools = self.tools.read().await;
        let resolved_name = Self::resolve_tool_name(name);
        if let Some(policy) = session_tool_policy(&ctx.session_id) {
            if let Some(allowed) = policy.allowed_tools.as_ref()
                && !tool_name_is_allowed(allowed, resolved_name)
            {
                return Err(anyhow::anyhow!("Tool '{}' is not allowed", resolved_name));
            }
            if tool_name_is_disabled(&policy.disabled_tools, resolved_name) {
                return Err(anyhow::anyhow!("Tool '{}' is disabled", resolved_name));
            }
        }
        let tool = match tools.get(resolved_name) {
            Some(tool) => tool.clone(),
            None => {
                // List available tools so the model can recover instead of
                // spiraling through hallucinated names like "ToolSearch" (#104).
                let mut available: Vec<&str> = tools
                    .keys()
                    .map(|k| k.as_str())
                    .filter(|name| tool_is_globally_available(name))
                    .collect();
                available.sort_unstable();
                let suggestions = Self::closest_tool_names(name, &available);
                let mut msg = format!("Unknown tool: {name}.");
                if !suggestions.is_empty() {
                    msg.push_str(&format!(" Did you mean: {}?", suggestions.join(", ")));
                }
                msg.push_str(&format!(" Available tools: {}.", available.join(", ")));
                return Err(anyhow::anyhow!(msg));
            }
        };

        // Drop the lock before executing
        drop(tools);
        let original_input = input.clone();
        let bound = self.bound_tool(resolved_name, tool);
        let (input, output_size) = bound.input.decode(input)?;
        let tool = bound.tool;
        let target = crate::config::config()
            .output
            .target(resolved_name, output_size);
        ctx.invocation.output_target = Some(target);
        ctx.invocation.policy = tool.execution_policy(&input, &ctx)?;
        if let Some(timeout) = ctx.invocation.policy.foreground_timeout {
            anyhow::ensure!(
                tokio::time::Instant::now().checked_add(timeout).is_some(),
                "Foreground timeout exceeds the platform clock range"
            );
        }

        let invocation = crate::execution::invocation(&ctx, resolved_name, original_input);
        let registry = self.clone_with_shared_context_runtime();
        let requested = name.to_string();
        let canonical = resolved_name.to_string();
        let result = crate::execution::execute(
            invocation,
            ctx.clone(),
            target,
            Box::new(move |ctx| {
                Box::pin(async move {
                    registry
                        .execute_bound(&requested, &canonical, input, tool, ctx)
                        .await
                })
            }),
        )
        .await;
        let output = match result {
            Ok(output) => self.guard_context_overflow(name, output).await,
            Err(error) => {
                if let Some(captured) = error.downcast_ref::<crate::execution::CapturedToolError>()
                {
                    let output = self
                        .guard_context_overflow(name, captured.output.clone())
                        .await;
                    return Err(crate::execution::CapturedToolError { output }.into());
                }
                return Err(error);
            }
        };
        Ok(output)
    }

    async fn execute_bound(
        &self,
        name: &str,
        resolved_name: &str,
        input: Value,
        tool: Arc<dyn Tool>,
        ctx: ToolContext,
    ) -> Result<ToolOutput> {
        // User-configured pre_tool gate: external policy hook that can block
        // this call (exit 2). Skipped entirely when not configured.
        if crate::hooks::hook_configured("pre_tool") {
            let input_json = input.to_string();
            let working_dir = ctx
                .working_dir
                .as_ref()
                .map(|dir| dir.display().to_string());
            let decision = crate::hooks::run_pre_tool_gate(
                &ctx.session_id,
                working_dir.as_deref(),
                resolved_name,
                &input_json,
            )
            .await;
            if let crate::hooks::GateDecision::Block { reason } = decision {
                let mut fields =
                    Self::tool_lifecycle_fields("blocked", name, resolved_name, &input, &ctx);
                fields.push(("block_reason".to_string(), reason.clone()));
                crate::logging::event_warn("TOOL_LIFECYCLE", fields);
                return Err(anyhow::anyhow!(
                    "Tool call blocked by pre_tool hook: {reason}"
                ));
            }
        }

        crate::logging::event_info(
            "TOOL_LIFECYCLE",
            Self::tool_lifecycle_fields("start", name, resolved_name, &input, &ctx),
        );

        let started_at = std::time::Instant::now();
        if !ctx.invocation.policy.manual_ready
            && let Some(ready) = &ctx.invocation.ready
        {
            ready.mark();
        }
        // `batch` and `conversation_search` are registered as shared built-in
        // tools, while context accounting is deliberately session-local. Bind
        // their execution to this Registry rather than the template Registry
        // that originally created the shared tool map.
        let result = if resolved_name == "batch" {
            batch::execute_with_registry(self, input.clone(), ctx.clone()).await
        } else if resolved_name == "conversation_search" {
            conversation_search::execute_with_context_budget(
                &self.context_budget,
                input.clone(),
                ctx.clone(),
            )
            .await
        } else {
            tool.execute(input.clone(), ctx.clone()).await
        };
        let latency_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        crate::telemetry::record_tool_execution(
            resolved_name,
            &input,
            result.as_ref().is_ok_and(|output| !output.is_error),
            latency_ms,
        );
        Self::fire_post_tool_hook(resolved_name, &ctx, &result, latency_ms);

        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let mut fields =
                    Self::tool_lifecycle_fields("error", name, resolved_name, &input, &ctx);
                fields.push(("elapsed_ms".to_string(), latency_ms.to_string()));
                fields.push(("error_class".to_string(), "producer_failure".to_string()));
                crate::logging::event_warn("TOOL_LIFECYCLE", fields);
                return Err(error);
            }
        };

        Ok(output)
    }

    /// Guard only delivery. Complete non-read output has already been retained.
    async fn guard_context_overflow(&self, tool_name: &str, mut output: ToolOutput) -> ToolOutput {
        let context = self.context_budget.read().await;
        let budget = context.token_budget();
        if budget == 0 {
            return output;
        }
        let current = context.effective_token_count();
        let tokens = Self::estimate_tokens(&output.output);
        let threshold = (budget as f32 * Self::CONTEXT_GUARD_THRESHOLD) as usize;
        let single = ((budget as f32 * Self::SINGLE_OUTPUT_MAX_FRACTION) as usize)
            .min(Self::SINGLE_OUTPUT_MAX_TOKENS);
        if current.saturating_add(tokens) <= threshold && tokens <= single {
            return output;
        }
        output.withheld = Some(jcode_tool_types::WithheldDelivery {
            estimated_output_tokens: tokens,
            current_tokens: current,
            budget,
        });
        let reference=match &mut output.source {
            jcode_tool_types::OutputSource::Retained(reference)=>{reference.continuation=None;format!("Complete captured output remains at {}. Read that file; do not repeat the original operation. Run: {}",reference.path.display(),reference.invocation_id)},
            jcode_tool_types::OutputSource::ReadPage(page)=>{
                page.end_byte=page.start_byte;page.end_line=page.start_line;
                page.next_point=Some(page.retry_point.clone());
                format!("This read page was not delivered. Retry file_path=\"{}\" with read_point=\"{}\"; no source position was advanced.",page.path.display(),page.retry_point)
            }
            jcode_tool_types::OutputSource::Inline=>"No retained reference is available for this failure. Do not blindly repeat an operation with uncertain effects.".to_string(),
            jcode_tool_types::OutputSource::Acceptance(reference)=>format!("Background work was accepted. Receipt: {}. Inspect run {}; do not repeat the original operation.",reference.path.display(),reference.invocation_id),
            jcode_tool_types::OutputSource::Unavailable(reference)=>format!("Execution {} was interrupted with an unknown final outcome. Read receipt {}; do not repeat the original operation.",reference.invocation_id,reference.receipt_path.display()),
        };
        let pressure = if current >= threshold {
            "CONTEXT LIMIT REACHED"
        } else {
            "OUTPUT WITHHELD"
        };
        output.output = format!(
            "{pressure}: {tool_name} delivery would exceed the context guard (~{tokens} output tokens, {current}/{budget} already used). Ask the user to /compact when context is full, then retrieve a suitable page. {reference}"
        );
        output.images.clear();
        output.resources.clear();
        if let jcode_tool_types::OutputSource::Retained(reference) = &output.source
            && let Some(path) = &reference.manifest_path
        {
            output
                .output
                .push_str(&format!(" Metadata and resource parts: {}", path.display()));
        }
        output
    }

    /// Register a tool dynamically (for MCP tools, etc.)
    pub async fn register(&self, name: String, tool: Arc<dyn Tool>) {
        let mut tools = self.tools.write().await;
        tools.insert(name, tool);
    }

    /// Register MCP tools (MCP management and server tools)
    /// Connections happen in background to avoid blocking startup.
    /// If `event_tx` is provided, sends an McpStatus event when connections complete.
    /// If `shared_pool` is provided, shared servers reuse processes from the pool.
    pub async fn register_mcp_tools(
        &self,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
        shared_pool: Option<std::sync::Arc<crate::mcp::SharedMcpPool>>,
        session_id: Option<String>,
    ) {
        self.register_mcp_tools_for_dir(event_tx, shared_pool, session_id, None)
            .await
    }

    /// Like [`Self::register_mcp_tools`], but resolves project-local MCP config
    /// (`.mcp.json`, `.jcode/mcp.json`, `.claude/mcp.json`) against
    /// `working_dir` instead of the server process cwd. Remote/client sessions
    /// must pass their session working directory here (issue #420).
    pub async fn register_mcp_tools_for_dir(
        &self,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
        shared_pool: Option<std::sync::Arc<crate::mcp::SharedMcpPool>>,
        session_id: Option<String>,
        working_dir: Option<std::path::PathBuf>,
    ) {
        use crate::mcp::McpManager;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mcp_manager = if let Some(pool) = shared_pool {
            let sid = session_id.unwrap_or_else(|| "unknown".to_string());
            Arc::new(RwLock::new(McpManager::with_shared_pool_for_dir(
                pool,
                sid,
                working_dir,
            )))
        } else {
            Arc::new(RwLock::new(McpManager::new()))
        };

        // Register MCP management tool immediately (with registry for dynamic tool registration)
        let mcp_tool =
            mcp::McpManagementTool::new(Arc::clone(&mcp_manager)).with_registry(self.clone());
        self.register("mcp".to_string(), Arc::new(mcp_tool) as Arc<dyn Tool>)
            .await;

        // Check if we have enabled servers to connect to. Disabled servers stay
        // configured (visible to the mcp management tool, connectable by name)
        // but are not spawned, advertised, or shown as connecting (issue #436).
        let (enabled_count, disabled_count) = {
            let manager = mcp_manager.read().await;
            let enabled = manager
                .config()
                .servers
                .values()
                .filter(|cfg| cfg.is_enabled())
                .count();
            (enabled, manager.config().servers.len() - enabled)
        };

        if disabled_count > 0 {
            crate::logging::info(&format!(
                "MCP: {} disabled server(s) in config (kept, not spawned)",
                disabled_count
            ));
        }

        if enabled_count > 0 {
            crate::logging::info(&format!("MCP: Found {} server(s) in config", enabled_count));

            // Send immediate "connecting" status so the TUI shows loading state
            // Server names with count 0 means "connecting..."
            if let Some(ref tx) = event_tx {
                let server_names: Vec<String> = {
                    let manager = mcp_manager.read().await;
                    manager
                        .config()
                        .servers
                        .iter()
                        .filter(|(_, cfg)| cfg.is_enabled())
                        .map(|(name, _)| format!("{}:0", name))
                        .collect()
                };
                let _ = tx.send(crate::protocol::ServerEvent::McpStatus {
                    servers: server_names,
                });
            }

            // Advertise-early: register proxy tools for each configured server
            // from the on-disk schema cache *before* connections settle, so the
            // first locked tool snapshot already contains MCP tools and we avoid
            // the intentional prompt-cache miss entirely (#206 Phase 2). The
            // proxies connect-on-first-call. Servers with no cached schemas yet
            // (cold start, or reconfigured) fall back to the post-connect
            // registration + one-shot late-register rebuild below.
            let schema_cache = crate::mcp::McpSchemaCache::load();
            let mut advertised_servers: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            {
                let config_servers: Vec<(String, crate::mcp::McpServerConfig)> = {
                    let manager = mcp_manager.read().await;
                    manager
                        .config()
                        .servers
                        .iter()
                        .filter(|(_, cfg)| cfg.is_enabled())
                        .map(|(name, cfg)| (name.clone(), cfg.clone()))
                        .collect()
                };
                let mut advertised_tool_count = 0usize;
                for (server, cfg) in &config_servers {
                    if let Some(cached) = schema_cache.tools_for(server, cfg) {
                        let tools = crate::mcp::create_mcp_tools_from_cached(
                            server,
                            cached,
                            Arc::clone(&mcp_manager),
                        );
                        advertised_tool_count += tools.len();
                        for (name, tool) in tools {
                            self.register(name, tool).await;
                        }
                        advertised_servers.insert(server.clone());
                    }
                }
                if advertised_tool_count > 0 {
                    crate::logging::info(&format!(
                        "MCP: advertised {} cached tool(s) from {} server(s) at spawn \
                         (connect-on-first-call); zero prompt-cache miss expected (#206)",
                        advertised_tool_count,
                        advertised_servers.len()
                    ));
                    // Reflect the advertised tools in the status indicator
                    // immediately so the UI shows them before connections settle.
                    if let Some(ref tx) = event_tx {
                        let mut counts: std::collections::BTreeMap<String, usize> =
                            std::collections::BTreeMap::new();
                        for (server, cfg) in &config_servers {
                            if let Some(cached) = schema_cache.tools_for(server, cfg) {
                                counts.insert(server.clone(), cached.len());
                            }
                        }
                        let servers: Vec<String> = counts
                            .into_iter()
                            .map(|(name, count)| format!("{}:{}", name, count))
                            .collect();
                        let _ = tx.send(crate::protocol::ServerEvent::McpStatus { servers });
                    }
                }
            }

            // Spawn connection and tool registration in background
            let registry = self.clone();
            tokio::spawn(async move {
                let (successes, failures) = {
                    let manager = mcp_manager.write().await;
                    manager.connect_all().await.unwrap_or((0, Vec::new()))
                };

                if successes > 0 {
                    crate::logging::info(&format!("MCP: Connected to {} server(s)", successes));
                }
                if !failures.is_empty() {
                    for (name, error) in &failures {
                        crate::logging::event_rate_limited(
                            crate::logging::LogLevel::Error,
                            &format!("mcp_register_failed:{name}"),
                            std::time::Duration::from_secs(60),
                            "MCP_REGISTER_FAILED",
                            vec![("server", name.to_string()), ("error", error.to_string())],
                        );
                    }
                }

                // Register MCP server tools and collect server info
                let tools = crate::mcp::create_mcp_tools(Arc::clone(&mcp_manager)).await;
                let mut server_counts: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for (name, tool) in &tools {
                    if let Some(rest) = name.strip_prefix("mcp__")
                        && let Some((server, _)) = rest.split_once("__")
                    {
                        *server_counts.entry(server.to_string()).or_default() += 1;
                    }
                    // Idempotent: advertise-early may have already registered an
                    // identical proxy. Re-registering refreshes it with the live
                    // schema, which is correct (handles schema drift).
                    registry.register(name.clone(), tool.clone()).await;
                }

                // Reconcile the on-disk schema cache with the live schemas so the
                // next spawn can advertise the up-to-date tools with zero cache
                // miss. Group live tool defs by server and update each entry
                // under the current config fingerprint; prune servers that are
                // no longer configured. (#206 Phase 2)
                {
                    // Live tool defs grouped by server, plus a snapshot of the
                    // configured servers, captured under one read lock.
                    type LiveToolsByServer =
                        std::collections::BTreeMap<String, Vec<crate::mcp::McpToolDef>>;
                    type ConfigSnapshot = Vec<(String, crate::mcp::McpServerConfig)>;
                    let (live_by_server, config_snapshot): (LiveToolsByServer, ConfigSnapshot) = {
                        let manager = mcp_manager.read().await;
                        let mut grouped: std::collections::BTreeMap<
                            String,
                            Vec<crate::mcp::McpToolDef>,
                        > = std::collections::BTreeMap::new();
                        for (server, def) in manager.all_tools().await {
                            grouped.entry(server).or_default().push(def);
                        }
                        let configs = manager
                            .config()
                            .servers
                            .iter()
                            .map(|(name, cfg)| (name.clone(), cfg.clone()))
                            .collect();
                        (grouped, configs)
                    };

                    let mut cache = crate::mcp::McpSchemaCache::load();
                    let mut dirty = false;
                    for (server, cfg) in &config_snapshot {
                        if let Some(defs) = live_by_server.get(server) {
                            // Only cache servers that actually exposed tools.
                            if cache.update(server, cfg, defs.clone()) {
                                dirty = true;
                            }
                        }
                    }
                    let configured_names: Vec<String> =
                        config_snapshot.iter().map(|(n, _)| n.clone()).collect();
                    if cache.retain_servers(&configured_names) {
                        dirty = true;
                    }
                    if dirty {
                        cache.save();
                        crate::logging::info(
                            "MCP: updated on-disk tool-schema cache from live connection (#206)",
                        );
                    }
                }

                // Notify client of MCP status
                if let Some(tx) = event_tx {
                    let servers: Vec<String> = server_counts
                        .into_iter()
                        .map(|(name, count)| format!("{}:{}", name, count))
                        .collect();
                    let _ = tx.send(crate::protocol::ServerEvent::McpStatus { servers });
                }
            });
        }
    }

    /// Register self-dev tools (only for canary/self-dev sessions)
    pub async fn register_selfdev_tools(&self) {
        // Self-dev management tool
        let selfdev_tool = selfdev::SelfDevTool::new();
        self.register(
            "selfdev".to_string(),
            Arc::new(selfdev_tool) as Arc<dyn Tool>,
        )
        .await;

        // Debug socket tool for direct debug socket access
        let debug_socket_tool = debug_socket::DebugSocketTool::new();
        self.register(
            "debug_socket".to_string(),
            Arc::new(debug_socket_tool) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Register ambient-mode tools (only for ambient sessions)
    pub async fn register_ambient_tools(&self) {
        self.register(
            "end_ambient_cycle".to_string(),
            Arc::new(ambient::EndAmbientCycleTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "schedule_ambient".to_string(),
            Arc::new(ambient::ScheduleAmbientTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "request_permission".to_string(),
            Arc::new(ambient::RequestPermissionTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "send_message".to_string(),
            Arc::new(ambient::SendChannelMessageTool::new()) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Unregister a tool
    pub async fn unregister(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let mut tools = self.tools.write().await;
        tools.remove(name)
    }

    /// Unregister all tools matching a prefix
    pub async fn unregister_prefix(&self, prefix: &str) -> Vec<String> {
        let mut tools = self.tools.write().await;
        let to_remove: Vec<String> = tools
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        for name in &to_remove {
            tools.remove(name);
        }
        to_remove
    }

    /// Get shared access to the skill registry
    pub fn skills(&self) -> Arc<RwLock<SkillRegistry>> {
        self.skills.clone()
    }

    /// Get shared access to policy-free context-budget accounting.
    pub fn context_budget(&self) -> Arc<RwLock<ContextBudgetTracker>> {
        self.context_budget.clone()
    }
}

/// Classic Levenshtein edit distance over Unicode scalar values.
/// Used only for tool-name "did you mean" suggestions, so the simple
/// O(n*m) two-row implementation is more than sufficient.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod mcp_allow_list_tests {
    use super::{tool_name_is_allowed, tool_name_is_disabled};
    use std::collections::HashSet;

    #[test]
    fn allowing_mcp_also_allows_dynamic_server_tools() {
        let allowed = HashSet::from(["mcp".to_string()]);

        assert!(tool_name_is_allowed(&allowed, "mcp"));
        assert!(tool_name_is_allowed(&allowed, "mcp__filesystem__read_file"));
        assert!(!tool_name_is_allowed(&allowed, "mcpish"));
        assert!(!tool_name_is_allowed(&allowed, "bash"));
    }

    #[test]
    fn disabling_mcp_also_disables_dynamic_server_tools() {
        let disabled = HashSet::from(["mcp".to_string()]);

        assert!(tool_name_is_disabled(&disabled, "mcp"));
        assert!(tool_name_is_disabled(
            &disabled,
            "mcp__filesystem__read_file"
        ));
        assert!(!tool_name_is_disabled(&disabled, "mcpish"));
        assert!(!tool_name_is_disabled(&disabled, "bash"));
    }
}

#[cfg(test)]
mod tests;
