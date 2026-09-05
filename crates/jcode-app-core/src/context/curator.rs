use crate::config::ContextCuratorConfig;
use crate::message::{ContentBlock, Message, Role, StreamEvent};
use crate::protocol::{
    CONTEXT_IDENTIFIER_MAX_CHARS, CONTEXT_PROTOCOL_MAX_EVENT_BYTES, ContextCuratorPlanPreview,
    ContextCuratorRouteOption, ContextCuratorRoutePreview, ContextCuratorSourcePurpose,
    ContextCuratorSourceScope, ContextCuratorTaskPreview,
};
use crate::provider::{ModelRoute, Provider, RouteSelection};
use futures::StreamExt;
use jcode_context_core::{
    ContextTargetIndex, context_token_rates_for_input_tokens, estimate_content_block_tokens,
    estimate_message_tokens,
};
use jcode_session_types::{
    StoredContentTarget, StoredContextArtifactGenerator, StoredContextBillingMode,
    StoredContextCuratorRole, StoredContextCuratorSelectionSource, StoredContextCuratorUsage,
    StoredContextPricingSnapshot, StoredMessage, StoredMessageRange,
};
#[cfg(test)]
use jcode_session_types::{StoredContextFileEvidence, StoredContextPathEvidence};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const CONTEXT_RANGE_SUMMARIZER_PROMPT_VERSION: &str = "context-range-summarizer-v3";
pub const CONTEXT_TOOL_DISTILLER_PROMPT_VERSION: &str = "context-tool-result-distiller-v2";

/// Product-approved verbatim base role instruction. Do not edit without an
/// explicit product decision and prompt-version change.
pub const RANGE_SUMMARIZER_BASE_PROMPT: &str = r#"This is `jcode`, an agentic coding harness. You are a __range summarizer__. A "range" is a (continuous) *slice* from a coding session. You will be given such a slice, and your task is to **losslessly summarize this slice**. A coding session, expectedly, includes user messages, assistant messages, assistant tool calls and their outputs. The purpose of compacting a slice is to reduce clutter by compressing a less-relevant past slice of a session into a smaller one. Less-relevant does not mean *not relevant*, and that's the reason you are "summarizing" it instead of simply removing it from the session history.
The slice will be *replaced* with your summary, and thus the working agent will *forget* the original slice completely, and their memory of that slice will be replaced by your summary. Thus, the agent will _rely_ on your summary. The agent will **continue working** after your summary. The agent does _not_ continue from where your summary ends — the slice you see is not the full session itself.
For example, if the session had 100 messages, your slice can be anything: message 20 to 30, 10 to 90, 50 to 80, and so on.
Keep your summary very detailed, informative, high-quality, well-written, and robust. Tell about what's been done, what interactions happened, what tool calls were made, what actions were taken, and so on. If files were read, explain the contents of these files (since if your slice contains a file read, it means the agent will "forget" that file as well, evidently), and so on. Make your summary *intentional*, and specific, and detailed. Not "files were read", "x file was read", "commits were made", and so on — these give no valuable information to the agent.
Return exactly one JSON object matching the supplied schema. Do not use markdown fences. Be **meticulous**."#;

const RANGE_MANDATORY_INSTRUCTIONS: &str = r#"Mandatory correctness requirements:
- Preserve user intent and preferences, decisions and rejected alternatives, exact constraints and invariants, implementation state at the end of the slice, files and symbols changed, commands and observed results, failures, unresolved issues, next steps, provider and environment facts, and operationally relevant IDs, hashes, paths, versions, values, and error strings.
- Preserve the substantive contents and findings of file reads, not merely the fact that a file was read.
- Never claim unverified work passed, omit unresolved failures, invent changed files, or replace precise technical facts with vague prose.
- Treat the complete `conversation_slice` as the authoritative primary source. Harness-generated file evidence is supporting metadata and never replaces substantive source content.
- Keep files changed, files read or inspected, and paths searched or browsed distinct. A search or directory browse does not prove that a file was read, and a read does not prove that a file changed.
- Use every harness-generated evidence category honestly. When any category is marked incomplete, retain its uncertainty and reasons.
- Return only the required JSON object with non-empty `summary`, `file_change_digest`, and `warnings` fields of the declared types."#;

const TOOL_DISTILLER_BASE_PROMPT: &str = r#"This is `jcode`, an agentic coding harness. You are a __tool-result distiller__. You will receive one complete tool result, its matching tool call, and only the supporting conversation needed to understand what later work relies upon. Your task is to decide whether that one result can be replaced by a substantially smaller representation without losing any meaningful information that could affect continued coding work.

The original tool result will disappear from the provider-facing context if the user accepts your proposal. Preserve exact errors and failing names, paths and line numbers, hashes, IDs, ports, versions, values, test counts, exit status, user-visible output, warnings, uncertainty, negative findings, and every fact relied upon later. A vague statement that a command ran or a file was inspected is not a substitute for its substantive findings. If a safe replacement cannot fit strictly below the supplied token threshold, mark the result ineligible.

Return exactly one JSON object matching the supplied schema. Do not use markdown fences or commentary. Be meticulous."#;

const TOOL_MANDATORY_INSTRUCTIONS: &str = r#"Mandatory correctness requirements:
- Evaluate only the one supplied tool result. Do not summarize unrelated conversation or propose other context operations.
- `eligible: true` requires a non-empty replacement and preservation rationale, no ineligible reason, and a replacement that remains strictly below 20 percent after Jcode re-tokenizes the preserved ToolResult structure.
- `eligible: false` requires a non-empty ineligible reason and must not provide eligible-only fields.
- A completely unnecessary result still needs a concise explicit marker when eligible.
- Preserve uncertainty and mark the result ineligible whenever lossless reduction below the limit is not possible.
- Return only the required JSON object."#;

#[derive(Clone)]
pub struct ContextCuratorRoute {
    pub provider: Arc<dyn Provider>,
    pub provider_name: String,
    pub provider_display_name: String,
    pub model: String,
    pub route: String,
    pub effort: Option<String>,
    pub pricing: StoredContextPricingSnapshot,
}

impl fmt::Debug for ContextCuratorRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextCuratorRoute")
            .field("provider_name", &self.provider_name)
            .field("provider_display_name", &self.provider_display_name)
            .field("model", &self.model)
            .field("route", &self.route)
            .field("effort", &self.effort)
            .field("pricing", &self.pricing)
            .finish_non_exhaustive()
    }
}

impl ContextCuratorRoute {
    pub fn preview(&self) -> ContextCuratorRoutePreview {
        ContextCuratorRoutePreview {
            provider_name: self.provider_name.clone(),
            provider_display_name: self.provider_display_name.clone(),
            model: self.model.clone(),
            route: self.route.clone(),
            effort: self.effort.clone(),
        }
    }

    pub fn generator(
        &self,
        role: StoredContextCuratorRole,
        prompt_version: &str,
        selection_source: StoredContextCuratorSelectionSource,
        transaction_instructions: &str,
        task_instructions: &str,
    ) -> StoredContextArtifactGenerator {
        StoredContextArtifactGenerator {
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            route: self.route.clone(),
            prompt_version: prompt_version.to_string(),
            effort: self.effort.clone(),
            role: Some(role),
            selection_source: Some(selection_source),
            transaction_instructions: nonempty_owned(transaction_instructions),
            task_instructions: nonempty_owned(task_instructions),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ContextCuratorLimits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_plan_preview_bytes: usize,
    pub timeout: Duration,
}

impl Default for ContextCuratorLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 32 * 1024 * 1024,
            max_response_bytes: 2 * 1024 * 1024,
            max_plan_preview_bytes: CONTEXT_PROTOCOL_MAX_EVENT_BYTES.saturating_sub(4 * 1024),
            timeout: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ContextCuratorRangeWork {
    pub request_id: String,
    pub source_range: StoredMessageRange,
    pub file_evidence: jcode_session_types::StoredContextFileEvidence,
    pub additional_instructions: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ContextCuratorToolWork {
    pub request_id: String,
    pub target: StoredContentTarget,
    pub message_index: usize,
    pub tool_name: String,
    pub tool_call_id: String,
    pub tool_input: Value,
    pub is_error: Option<bool>,
    pub original_token_estimate: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCuratorRangeArtifact {
    pub summary: String,
    pub file_change_digest: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCuratorToolArtifact {
    Eligible {
        replacement: String,
        replacement_token_estimate: usize,
        preservation_rationale: String,
        uncertainties: Vec<String>,
    },
    Ineligible {
        reason: String,
        uncertainties: Vec<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextCuratorArtifacts {
    pub range_summaries: BTreeMap<String, ContextCuratorRangeArtifact>,
    pub tool_distillations: BTreeMap<String, ContextCuratorToolArtifact>,
    pub usage: Vec<StoredContextCuratorUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCuratorError {
    Route(String),
    RequestTooLarge {
        bytes: usize,
        limit: usize,
    },
    InputTooLarge {
        estimated_tokens: usize,
        safe_budget: usize,
    },
    ImagesUnsupported {
        count: usize,
        provider: String,
    },
    ResponseTooLarge {
        bytes: usize,
        limit: usize,
    },
    PlanPreviewTooLarge {
        bytes: usize,
        limit: usize,
    },
    Timeout,
    Canceled,
    Provider(String),
    UnexpectedToolUse,
    InvalidResponse(String),
    TaskFailed {
        task_id: String,
        completed: usize,
        total: usize,
        reason: String,
    },
}

impl fmt::Display for ContextCuratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(reason) => write!(formatter, "context curator route is unavailable: {reason}"),
            Self::RequestTooLarge { bytes, limit } => write!(
                formatter,
                "complete atomic curator request is {bytes} bytes, exceeding the {limit}-byte bound; no source was truncated"
            ),
            Self::InputTooLarge {
                estimated_tokens,
                safe_budget,
            } => write!(
                formatter,
                "complete atomic curator input is estimated at {estimated_tokens} tokens, exceeding the selected route's safe {safe_budget}-token input budget; no source was truncated"
            ),
            Self::ImagesUnsupported { count, provider } => write!(
                formatter,
                "complete atomic curator material contains {count} image(s), but route {provider} does not support image input"
            ),
            Self::ResponseTooLarge { bytes, limit } => write!(
                formatter,
                "context curator response reached {bytes} bytes, exceeding the {limit}-byte bound"
            ),
            Self::PlanPreviewTooLarge { bytes, limit } => write!(
                formatter,
                "exact curator plan preview is {bytes} bytes, exceeding the {limit}-byte bound; reduce the number of atomic tasks or ephemeral instructions before generation"
            ),
            Self::Timeout => formatter.write_str("context curator request timed out"),
            Self::Canceled => formatter.write_str("context curator request was canceled"),
            Self::Provider(reason) => write!(formatter, "context curator provider failed: {reason}"),
            Self::UnexpectedToolUse => formatter.write_str(
                "context curator attempted a tool call; artifact generation requires JSON text only",
            ),
            Self::InvalidResponse(reason) => {
                write!(formatter, "context curator returned invalid structured output: {reason}")
            }
            Self::TaskFailed {
                task_id,
                completed,
                total,
                reason,
            } => write!(
                formatter,
                "atomic curator task {task_id} failed after {completed} of {total} task(s) completed: {reason}; no generated artifact was activated or retained as an applicable transaction"
            ),
        }
    }
}

impl Error for ContextCuratorError {}

pub fn resolve_context_curator_route(
    provider_fork: Arc<dyn Provider>,
    model_routes: &[ModelRoute],
    active_route: &str,
    config: &ContextCuratorConfig,
) -> Result<ContextCuratorRoute, ContextCuratorError> {
    for (label, value) in [
        ("provider", config.provider.as_deref()),
        ("route", config.route.as_deref()),
        ("model", config.model.as_deref()),
        ("effort", config.effort.as_deref()),
    ] {
        if let Some(value) = value {
            let chars = value.chars().count();
            if value.trim().is_empty() {
                return Err(ContextCuratorError::Route(format!(
                    "configured curator {label} selector is empty"
                )));
            }
            if chars > CONTEXT_IDENTIFIER_MAX_CHARS {
                return Err(ContextCuratorError::Route(format!(
                    "configured curator {label} selector contains {chars} characters, exceeding the {CONTEXT_IDENTIFIER_MAX_CHARS}-character bound"
                )));
            }
        }
    }
    let mut route = active_route.to_string();
    let requested_model = config
        .model
        .clone()
        .unwrap_or_else(|| provider_fork.model());
    let has_route_selector = config
        .route
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_provider_selector = config
        .provider
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    if has_route_selector || has_provider_selector {
        let matches = model_routes
            .iter()
            .filter(|candidate| candidate.available && candidate.model == requested_model)
            .filter(|candidate| {
                config.provider.as_deref().is_none_or(|selector| {
                    if config.route.is_none() {
                        // Preserve the pre-Phase-15 overloaded provider selector for
                        // existing config files while new UI writes a separate route.
                        route_selector_matches(selector, candidate)
                    } else {
                        candidate.provider.eq_ignore_ascii_case(selector.trim())
                    }
                })
            })
            .filter(|candidate| {
                config
                    .route
                    .as_deref()
                    .is_none_or(|selector| route_selector_matches(selector, candidate))
            })
            .collect::<Vec<_>>();
        let selected = match matches.as_slice() {
            [selected] => *selected,
            [] => {
                return Err(ContextCuratorError::Route(format!(
                    "no available route matches provider {:?}, route {:?}, and model {:?}",
                    config.provider, config.route, requested_model
                )));
            }
            _ => {
                let choices = matches
                    .iter()
                    .map(|candidate| format!("{} ({})", candidate.provider, candidate.api_method))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ContextCuratorError::Route(format!(
                    "provider {:?}, route {:?}, and model {:?} are ambiguous; matching routes: {choices}",
                    config.provider, config.route, requested_model
                )));
            }
        };
        provider_fork
            .set_route_selection(&RouteSelection::from_model_route(selected))
            .map_err(|error| ContextCuratorError::Route(error.to_string()))?;
        route = selected.api_method.clone();
    } else if config.model.is_some() {
        provider_fork
            .set_model(&requested_model)
            .map_err(|error| ContextCuratorError::Route(error.to_string()))?;
    }

    if let Some(effort) = config.effort.as_deref() {
        provider_fork
            .set_reasoning_effort(effort)
            .map_err(|error| ContextCuratorError::Route(error.to_string()))?;
    }

    let provider_name = provider_fork.name().to_string();
    let provider_display_name = provider_fork.display_name();
    let model = provider_fork.model();
    if provider_name.trim().is_empty() || model.trim().is_empty() {
        return Err(ContextCuratorError::Route(
            "the independent provider fork did not expose a stable provider and model identity"
                .to_string(),
        ));
    }
    if route.trim().is_empty() {
        return Err(ContextCuratorError::Route(
            "the independent curator provider did not expose a stable route identity".to_string(),
        ));
    }
    let pricing = crate::provider::pricing::context_pricing_snapshot(
        &model,
        &provider_display_name,
        &route,
        jcode_session_types::StoredContextCacheWarmth::Unknown,
    );
    Ok(ContextCuratorRoute {
        provider: Arc::clone(&provider_fork),
        provider_name,
        provider_display_name,
        model,
        route,
        effort: provider_fork.reasoning_effort(),
        pricing,
    })
}

pub(crate) fn curator_route_options(model_routes: &[ModelRoute]) -> Vec<ContextCuratorRouteOption> {
    let mut seen = BTreeSet::new();
    let mut options = model_routes
        .iter()
        .filter(|route| route.available)
        .filter_map(|route| {
            let key = (
                route.provider.to_ascii_lowercase(),
                route.api_method.to_ascii_lowercase(),
                route.model.to_ascii_lowercase(),
            );
            seen.insert(key).then(|| ContextCuratorRouteOption {
                provider: route.provider.clone(),
                route: route.api_method.clone(),
                model: route.model.clone(),
                detail: route.detail.clone(),
                efforts: jcode_provider_core::inferred_reasoning_efforts(
                    Some(&route.provider),
                    Some(&route.model),
                )
                .into_iter()
                .filter(|effort| !matches!(*effort, "swarm" | "swarm-deep"))
                .map(str::to_string)
                .collect(),
            })
        })
        .collect::<Vec<_>>();
    options.sort_by(|left, right| {
        left.model
            .to_ascii_lowercase()
            .cmp(&right.model.to_ascii_lowercase())
            .then_with(|| {
                left.provider
                    .to_ascii_lowercase()
                    .cmp(&right.provider.to_ascii_lowercase())
            })
            .then_with(|| left.route.cmp(&right.route))
    });
    options
}

fn route_selector_matches(selector: &str, route: &ModelRoute) -> bool {
    let selector = selector.trim().to_ascii_lowercase();
    if selector.is_empty() {
        return false;
    }
    let selection = RouteSelection::from_model_route(route);
    route.provider.to_ascii_lowercase() == selector
        || route.api_method.to_ascii_lowercase() == selector
        || selection.runtime_key.stable_id().to_ascii_lowercase() == selector
}

#[derive(Clone, Debug)]
pub(crate) struct ContextCuratorPlan {
    tasks: Vec<ContextCuratorTask>,
    pub preview: ContextCuratorPlanPreview,
}

#[derive(Clone, Debug)]
struct ContextCuratorTask {
    task_id: String,
    role: StoredContextCuratorRole,
    kind: ContextCuratorTaskKind,
    system_prompt: String,
    response_contract: String,
    scope: Vec<ScopedMessage>,
    active_summary_texts: Vec<String>,
}

#[derive(Clone, Debug)]
enum ContextCuratorTaskKind {
    Range(ContextCuratorRangeWork),
    Tool(ContextCuratorToolWork),
}

#[derive(Clone, Debug)]
struct ScopedMessage {
    purpose: ContextCuratorSourcePurpose,
    message_index: usize,
    block_ordinals: Vec<usize>,
    includes_all_blocks: bool,
}

pub(crate) struct ContextCuratorPlanInput<'a> {
    pub session_id: &'a str,
    pub context_revision: u64,
    pub transcript_digest: u64,
    pub messages: &'a [StoredMessage],
    pub ranges: &'a [ContextCuratorRangeWork],
    pub tools: &'a [ContextCuratorToolWork],
    pub active_summary_texts: &'a [String],
    pub transaction_instructions: &'a str,
    pub selection_source: StoredContextCuratorSelectionSource,
}

pub(crate) fn build_context_curator_plan(
    route: &ContextCuratorRoute,
    input: ContextCuratorPlanInput<'_>,
    limits: ContextCuratorLimits,
) -> Result<ContextCuratorPlan, ContextCuratorError> {
    let target_index = ContextTargetIndex::new(input.messages);
    let selected_ranges = input
        .ranges
        .iter()
        .map(|range| {
            target_index
                .resolve_message_range(&range.source_range)
                .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tool_exclusions = tool_source_exclusions(input.messages, input.tools, &target_index)?;
    let mut tasks = Vec::with_capacity(input.ranges.len().saturating_add(input.tools.len()));

    for range in input.ranges {
        let (start, end) = target_index
            .resolve_message_range(&range.source_range)
            .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
        let scope = (start..=end)
            .map(|message_index| ScopedMessage {
                purpose: ContextCuratorSourcePurpose::PrimaryRange,
                message_index,
                block_ordinals: Vec::new(),
                includes_all_blocks: true,
            })
            .collect::<Vec<_>>();
        tasks.push(ContextCuratorTask {
            task_id: range.request_id.clone(),
            role: StoredContextCuratorRole::RangeSummarizer,
            kind: ContextCuratorTaskKind::Range(range.clone()),
            system_prompt: range_system_prompt(
                input.transaction_instructions,
                &range.additional_instructions,
            ),
            response_contract: pretty_json(&range_response_schema())?,
            scope,
            active_summary_texts: Vec::new(),
        });
    }

    for tool in input.tools {
        let scope = tool_scope(
            input.messages,
            tool,
            input.tools,
            &selected_ranges,
            &tool_exclusions,
            &target_index,
        )?;
        tasks.push(ContextCuratorTask {
            task_id: tool.request_id.clone(),
            role: StoredContextCuratorRole::ToolResultDistiller,
            kind: ContextCuratorTaskKind::Tool(tool.clone()),
            system_prompt: tool_system_prompt(input.transaction_instructions),
            response_contract: pretty_json(&tool_response_schema())?,
            scope,
            active_summary_texts: input.active_summary_texts.to_vec(),
        });
    }

    let mut previews = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let prepared = prepare_task_call(route, input.messages, task, limits)?;
        previews.push(ContextCuratorTaskPreview {
            task_id: task.task_id.clone(),
            role: task.role,
            target_label: task_target_label(task),
            effective_system_prompt: task.system_prompt.clone(),
            response_contract: task.response_contract.clone(),
            estimated_input_tokens: prepared.estimated_input_tokens,
            safe_input_budget: prepared.safe_input_budget,
            request_bytes: prepared.request_bytes,
            request_byte_limit: limits.max_request_bytes,
            image_count: prepared.image_count,
            source_scope: public_scope(input.messages, task),
            file_evidence: match &task.kind {
                ContextCuratorTaskKind::Range(work) => Some(work.file_evidence.clone()),
                ContextCuratorTaskKind::Tool(_) => None,
            },
            user_instructions: crate::protocol::ContextCuratorInstructionDisclosure {
                transaction_wide_chars: input.transaction_instructions.chars().count(),
                task_specific_chars: match &task.kind {
                    ContextCuratorTaskKind::Range(work) => {
                        work.additional_instructions.chars().count()
                    }
                    ContextCuratorTaskKind::Tool(_) => 0,
                },
            },
        });
    }

    let mut preview = ContextCuratorPlanPreview {
        session_id: input.session_id.to_string(),
        context_revision: input.context_revision,
        transcript_digest: input.transcript_digest,
        route: route.preview(),
        using_configured_default: input.selection_source
            == StoredContextCuratorSelectionSource::ConfiguredDefault,
        tasks: previews,
        fingerprint: String::new(),
    };
    let canonical = serde_json::to_vec(&preview)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    let final_preview_bytes = canonical.len().saturating_add(128);
    if final_preview_bytes > limits.max_plan_preview_bytes {
        return Err(ContextCuratorError::PlanPreviewTooLarge {
            bytes: final_preview_bytes,
            limit: limits.max_plan_preview_bytes,
        });
    }
    preview.fingerprint = format!("{:x}", Sha256::digest(canonical));
    Ok(ContextCuratorPlan { tasks, preview })
}

#[cfg(test)]
pub(crate) async fn run_context_curator(
    route: &ContextCuratorRoute,
    messages: &[StoredMessage],
    ranges: &[ContextCuratorRangeWork],
    tools: &[ContextCuratorToolWork],
    active_summary_texts: &[String],
    cancellation: &CancellationToken,
    limits: ContextCuratorLimits,
) -> Result<ContextCuratorArtifacts, ContextCuratorError> {
    let plan = build_context_curator_plan(
        route,
        ContextCuratorPlanInput {
            session_id: "",
            context_revision: 0,
            transcript_digest: 0,
            messages,
            ranges,
            tools,
            active_summary_texts,
            transaction_instructions: "",
            selection_source: StoredContextCuratorSelectionSource::ConfiguredDefault,
        },
        limits,
    )?;
    run_context_curator_plan(route, messages, &plan, cancellation, limits, |_, _| {}).await
}

pub(crate) async fn run_context_curator_plan<F>(
    route: &ContextCuratorRoute,
    messages: &[StoredMessage],
    plan: &ContextCuratorPlan,
    cancellation: &CancellationToken,
    limits: ContextCuratorLimits,
    mut on_completed: F,
) -> Result<ContextCuratorArtifacts, ContextCuratorError>
where
    F: FnMut(usize, Option<&StoredContextCuratorUsage>),
{
    if cancellation.is_cancelled() {
        return Err(ContextCuratorError::Canceled);
    }
    let mut artifacts = ContextCuratorArtifacts::default();
    let total = plan.tasks.len();
    for (index, task) in plan.tasks.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(ContextCuratorError::Canceled);
        }
        let result = run_context_curator_task(route, messages, task, cancellation, limits)
            .await
            .map_err(|error| match error {
                ContextCuratorError::Canceled => ContextCuratorError::Canceled,
                other => ContextCuratorError::TaskFailed {
                    task_id: task.task_id.clone(),
                    completed: index,
                    total,
                    reason: other.to_string(),
                },
            })?;
        match result.artifact {
            ContextCuratorTaskArtifact::Range(artifact) => {
                artifacts
                    .range_summaries
                    .insert(task.task_id.clone(), artifact);
            }
            ContextCuratorTaskArtifact::Tool(artifact) => {
                artifacts
                    .tool_distillations
                    .insert(task.task_id.clone(), artifact);
            }
        }
        if let Some(usage) = result.usage.as_ref() {
            artifacts.usage.push(usage.clone());
        }
        on_completed(index + 1, result.usage.as_ref());
    }
    Ok(artifacts)
}

struct ContextCuratorTaskResult {
    artifact: ContextCuratorTaskArtifact,
    usage: Option<StoredContextCuratorUsage>,
}

enum ContextCuratorTaskArtifact {
    Range(ContextCuratorRangeArtifact),
    Tool(ContextCuratorToolArtifact),
}

async fn run_context_curator_task(
    route: &ContextCuratorRoute,
    messages: &[StoredMessage],
    task: &ContextCuratorTask,
    cancellation: &CancellationToken,
    limits: ContextCuratorLimits,
) -> Result<ContextCuratorTaskResult, ContextCuratorError> {
    let prepared = prepare_task_call(route, messages, task, limits)?;
    let provider = route.provider.fork();
    if provider.name() != route.provider_name
        || provider.model() != route.model
        || provider.reasoning_effort() != route.effort
    {
        return Err(ContextCuratorError::Route(format!(
            "independent task fork changed curator identity from {}/{}/{:?} to {}/{}/{:?}",
            route.provider_name,
            route.model,
            route.effort,
            provider.name(),
            provider.model(),
            provider.reasoning_effort()
        )));
    }
    let call = collect_curator_response(
        provider.as_ref(),
        &prepared.provider_messages,
        &task.system_prompt,
        limits.max_response_bytes,
    );
    let collected = tokio::select! {
        _ = cancellation.cancelled() => return Err(ContextCuratorError::Canceled),
        result = tokio::time::timeout(limits.timeout, call) => {
            match result {
                Ok(result) => result?,
                Err(_) => return Err(ContextCuratorError::Timeout),
            }
        }
    };
    if cancellation.is_cancelled() {
        return Err(ContextCuratorError::Canceled);
    }

    let prompt_version = task_prompt_version(task.role);
    let usage = build_curator_usage(route, task, prompt_version, collected.usage);
    let artifact = match &task.kind {
        ContextCuratorTaskKind::Range(_) => {
            ContextCuratorTaskArtifact::Range(parse_range_response(&collected.text)?)
        }
        ContextCuratorTaskKind::Tool(work) => {
            ContextCuratorTaskArtifact::Tool(parse_tool_response(&collected.text, work)?)
        }
    };
    Ok(ContextCuratorTaskResult { artifact, usage })
}

struct PreparedTaskCall {
    provider_messages: Vec<Message>,
    request_bytes: usize,
    estimated_input_tokens: usize,
    safe_input_budget: usize,
    image_count: usize,
}

fn prepare_task_call(
    route: &ContextCuratorRoute,
    messages: &[StoredMessage],
    task: &ContextCuratorTask,
    limits: ContextCuratorLimits,
) -> Result<PreparedTaskCall, ContextCuratorError> {
    let request = build_task_request(messages, task)?;
    let request_json = serde_json::to_string(&request.payload)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    if !request.images.is_empty() && !route.provider.supports_image_input() {
        return Err(ContextCuratorError::ImagesUnsupported {
            count: request.images.len(),
            provider: route.provider_display_name.clone(),
        });
    }

    let mut content = vec![ContentBlock::Text {
        text: request_json,
        cache_control: None,
    }];
    for image in &request.images {
        content.push(ContentBlock::Text {
            text: format!("Image attachment {}:", image.image_ref),
            cache_control: None,
        });
        content.push(ContentBlock::Image {
            media_type: image.media_type.clone(),
            data: image.data.clone(),
        });
    }
    let provider_messages = vec![Message {
        role: Role::User,
        content,
        timestamp: None,
        tool_duration_ms: None,
    }];
    let request_bytes = serde_json::to_vec(&provider_messages)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?
        .len()
        .saturating_add(task.system_prompt.len());
    if request_bytes > limits.max_request_bytes {
        return Err(ContextCuratorError::RequestTooLarge {
            bytes: request_bytes,
            limit: limits.max_request_bytes,
        });
    }
    let prompt_message = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: task.system_prompt.clone(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let estimated_input_tokens = estimate_message_tokens(&provider_messages[0])
        .saturating_add(estimate_message_tokens(&prompt_message));
    let safe_input_budget = route.provider.context_request_budget().safe_input_budget();
    if estimated_input_tokens > safe_input_budget {
        return Err(ContextCuratorError::InputTooLarge {
            estimated_tokens: estimated_input_tokens,
            safe_budget: safe_input_budget,
        });
    }
    Ok(PreparedTaskCall {
        provider_messages,
        request_bytes,
        estimated_input_tokens,
        safe_input_budget,
        image_count: request.images.len(),
    })
}

#[derive(Default)]
struct CuratorUsageAccumulator {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

struct CollectedCuratorResponse {
    text: String,
    usage: CuratorUsageAccumulator,
}

async fn collect_curator_response(
    provider: &dyn Provider,
    messages: &[Message],
    system_prompt: &str,
    max_response_bytes: usize,
) -> Result<CollectedCuratorResponse, ContextCuratorError> {
    let response = provider
        .complete(messages, &[], system_prompt, None)
        .await
        .map_err(|error| ContextCuratorError::Provider(error.to_string()))?;
    tokio::pin!(response);
    let mut text = String::new();
    let mut usage = CuratorUsageAccumulator::default();
    while let Some(event) = response.next().await {
        match event.map_err(|error| ContextCuratorError::Provider(error.to_string()))? {
            StreamEvent::TextDelta(delta) => {
                let new_len = text.len().saturating_add(delta.len());
                if new_len > max_response_bytes {
                    return Err(ContextCuratorError::ResponseTooLarge {
                        bytes: new_len,
                        limit: max_response_bytes,
                    });
                }
                text.push_str(&delta);
            }
            StreamEvent::TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                if input_tokens.is_some() {
                    usage.input_tokens = input_tokens;
                }
                if output_tokens.is_some() {
                    usage.output_tokens = output_tokens;
                }
                if cache_read_input_tokens.is_some() {
                    usage.cache_read_input_tokens = cache_read_input_tokens;
                }
                if cache_creation_input_tokens.is_some() {
                    usage.cache_creation_input_tokens = cache_creation_input_tokens;
                }
            }
            StreamEvent::RetryRollback { .. } => {
                text.clear();
                usage = CuratorUsageAccumulator::default();
            }
            StreamEvent::ToolUseStart { .. }
            | StreamEvent::ToolInputDelta(_)
            | StreamEvent::ToolUseEnd
            | StreamEvent::ToolUseSignature(_)
            | StreamEvent::ToolResult { .. }
            | StreamEvent::GeneratedImage { .. }
            | StreamEvent::NativeToolCall { .. } => {
                return Err(ContextCuratorError::UnexpectedToolUse);
            }
            StreamEvent::Error { message, .. } => {
                return Err(ContextCuratorError::Provider(message));
            }
            _ => {}
        }
    }
    Ok(CollectedCuratorResponse { text, usage })
}

fn build_curator_usage(
    route: &ContextCuratorRoute,
    task: &ContextCuratorTask,
    prompt_version: &str,
    usage: CuratorUsageAccumulator,
) -> Option<StoredContextCuratorUsage> {
    let input_tokens = usage.input_tokens?;
    let output_tokens = usage.output_tokens?;
    let cache_read_input_tokens = usage.cache_read_input_tokens;
    let cache_creation_input_tokens = usage.cache_creation_input_tokens;
    Some(StoredContextCuratorUsage {
        provider: route.provider_name.clone(),
        model: route.model.clone(),
        route: route.route.clone(),
        effort: route.effort.clone(),
        role: Some(task.role),
        artifact_id: Some(task.task_id.clone()),
        prompt_version: Some(prompt_version.to_string()),
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        cost_usd: exact_usage_cost(
            &route.pricing,
            &route.provider_name,
            input_tokens,
            output_tokens,
            cache_read_input_tokens.unwrap_or_default(),
            cache_creation_input_tokens.unwrap_or_default(),
        ),
    })
}

fn exact_usage_cost(
    pricing: &StoredContextPricingSnapshot,
    provider_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> Option<f64> {
    if pricing.billing_mode != StoredContextBillingMode::Metered {
        return None;
    }
    let has_cache_accounting = cache_read_tokens > 0 || cache_creation_tokens > 0;
    let accounting = if has_cache_accounting {
        Some(cache_usage_accounting(provider_name)?)
    } else {
        None
    };
    let tier_input_tokens = match accounting {
        Some(CacheUsageAccounting::Split) => input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_creation_tokens),
        Some(CacheUsageAccounting::Inclusive) | None => input_tokens,
    };
    let rates = context_token_rates_for_input_tokens(
        pricing,
        usize::try_from(tier_input_tokens).unwrap_or(usize::MAX),
    );
    let input_rate = rates.input_usd_per_million?;
    let output_rate = if output_tokens == 0 {
        0.0
    } else {
        rates.output_usd_per_million?
    };
    let fresh_input_tokens = match accounting {
        Some(CacheUsageAccounting::Split) => input_tokens,
        Some(CacheUsageAccounting::Inclusive) => input_tokens.saturating_sub(
            cache_read_tokens
                .saturating_add(cache_creation_tokens)
                .min(input_tokens),
        ),
        None => input_tokens,
    };
    let cache_read_rate = if cache_read_tokens == 0 {
        0.0
    } else {
        rates.cache_read_usd_per_million?
    };
    let cache_write_rate = if cache_creation_tokens > 0 {
        rates.cache_write_usd_per_million?
    } else {
        0.0
    };
    Some(
        fresh_input_tokens as f64 * input_rate / 1_000_000.0
            + output_tokens as f64 * output_rate / 1_000_000.0
            + cache_read_tokens as f64 * cache_read_rate / 1_000_000.0
            + cache_creation_tokens as f64 * cache_write_rate / 1_000_000.0,
    )
}

#[derive(Clone, Copy)]
enum CacheUsageAccounting {
    Split,
    Inclusive,
}

fn cache_usage_accounting(provider_name: &str) -> Option<CacheUsageAccounting> {
    let provider_name = provider_name.trim().to_ascii_lowercase();
    if provider_name.contains("anthropic") || provider_name.contains("claude") {
        return Some(CacheUsageAccounting::Split);
    }
    if provider_name.contains("openai")
        || provider_name.contains("openrouter")
        || provider_name.contains("chatgpt")
        || provider_name.contains("codex")
    {
        return Some(CacheUsageAccounting::Inclusive);
    }
    None
}

struct BuiltTaskRequest {
    payload: CuratorTaskRequestPayload,
    images: Vec<CuratorImageAttachment>,
}

#[derive(Serialize)]
#[serde(tag = "task", rename_all = "snake_case")]
enum CuratorTaskRequestPayload {
    RangeSummary {
        response_schema: Value,
        harness_file_evidence: CuratorFileEvidencePayload,
        conversation_slice: Vec<CuratorMessagePayload>,
    },
    ToolResultDistillation {
        response_schema: Value,
        tool: CuratorToolPayload,
        matching_tool_call: Vec<CuratorMessagePayload>,
        complete_tool_result: Vec<CuratorMessagePayload>,
        supporting_conversation: Vec<CuratorMessagePayload>,
        active_context_summaries: Vec<String>,
    },
}

#[derive(Serialize)]
struct CuratorFileEvidencePayload {
    changed: CuratorPathEvidencePayload,
    read_or_inspected: CuratorPathEvidencePayload,
    searched_or_browsed: CuratorPathEvidencePayload,
}

#[derive(Serialize)]
struct CuratorPathEvidencePayload {
    paths: Vec<String>,
    complete: bool,
    warnings: Vec<String>,
}

impl From<&jcode_session_types::StoredContextFileEvidence> for CuratorFileEvidencePayload {
    fn from(evidence: &jcode_session_types::StoredContextFileEvidence) -> Self {
        let category = |category: &jcode_session_types::StoredContextPathEvidence| {
            CuratorPathEvidencePayload {
                paths: category.paths.clone(),
                complete: category.complete,
                warnings: category.warnings.clone(),
            }
        };
        Self {
            changed: category(&evidence.changed),
            read_or_inspected: category(&evidence.read_or_inspected),
            searched_or_browsed: category(&evidence.searched_or_browsed),
        }
    }
}

#[derive(Serialize)]
struct CuratorToolPayload {
    name: String,
    input: Value,
    original_token_estimate: usize,
    replacement_must_be_below_tokens: usize,
}

#[derive(Serialize)]
struct CuratorMessagePayload {
    sequence: usize,
    role: Role,
    blocks: Vec<Value>,
}

struct CuratorImageAttachment {
    image_ref: String,
    media_type: String,
    data: String,
}

fn build_task_request(
    messages: &[StoredMessage],
    task: &ContextCuratorTask,
) -> Result<BuiltTaskRequest, ContextCuratorError> {
    let mut images = Vec::new();
    let payload = match &task.kind {
        ContextCuratorTaskKind::Range(work) => CuratorTaskRequestPayload::RangeSummary {
            response_schema: range_response_schema(),
            harness_file_evidence: CuratorFileEvidencePayload::from(&work.file_evidence),
            conversation_slice: scoped_payloads(
                messages,
                task.scope
                    .iter()
                    .filter(|scope| scope.purpose == ContextCuratorSourcePurpose::PrimaryRange),
                &mut images,
            )?,
        },
        ContextCuratorTaskKind::Tool(work) => CuratorTaskRequestPayload::ToolResultDistillation {
            response_schema: tool_response_schema(),
            tool: CuratorToolPayload {
                name: work.tool_name.clone(),
                input: work.tool_input.clone(),
                original_token_estimate: work.original_token_estimate,
                replacement_must_be_below_tokens: work
                    .original_token_estimate
                    .saturating_mul(20)
                    .saturating_sub(1)
                    / 100,
            },
            matching_tool_call: scoped_payloads(
                messages,
                task.scope
                    .iter()
                    .filter(|scope| scope.purpose == ContextCuratorSourcePurpose::PrimaryToolCall),
                &mut images,
            )?,
            complete_tool_result: scoped_payloads(
                messages,
                task.scope.iter().filter(|scope| {
                    scope.purpose == ContextCuratorSourcePurpose::PrimaryToolResult
                }),
                &mut images,
            )?,
            supporting_conversation: scoped_payloads(
                messages,
                task.scope.iter().filter(|scope| {
                    scope.purpose == ContextCuratorSourcePurpose::SupportingConversation
                }),
                &mut images,
            )?,
            active_context_summaries: task.active_summary_texts.clone(),
        },
    };
    Ok(BuiltTaskRequest { payload, images })
}

fn scoped_payloads<'a>(
    messages: &[StoredMessage],
    scopes: impl Iterator<Item = &'a ScopedMessage>,
    images: &mut Vec<CuratorImageAttachment>,
) -> Result<Vec<CuratorMessagePayload>, ContextCuratorError> {
    scopes
        .enumerate()
        .map(|(sequence, scope)| {
            let message = messages.get(scope.message_index).ok_or_else(|| {
                ContextCuratorError::InvalidResponse(format!(
                    "curator scope references missing stored message index {}",
                    scope.message_index
                ))
            })?;
            let ordinals = if scope.includes_all_blocks {
                (0..message.content.len()).collect::<Vec<_>>()
            } else {
                scope.block_ordinals.clone()
            };
            let blocks = ordinals
                .into_iter()
                .map(|ordinal| {
                    let block = message.content.get(ordinal).ok_or_else(|| {
                        ContextCuratorError::InvalidResponse(format!(
                            "curator scope references missing block {ordinal} in stored message index {}",
                            scope.message_index
                        ))
                    })?;
                    curator_block_payload(block, images)
                })
                .collect::<Result<Vec<_>, ContextCuratorError>>()?;
            Ok(CuratorMessagePayload {
                sequence,
                role: message.role.clone(),
                blocks,
            })
        })
        .collect()
}

fn curator_block_payload(
    block: &ContentBlock,
    images: &mut Vec<CuratorImageAttachment>,
) -> Result<Value, ContextCuratorError> {
    let payload = match block {
        ContentBlock::Image { media_type, data } => {
            let image_ref = format!("image-{}", images.len() + 1);
            images.push(CuratorImageAttachment {
                image_ref: image_ref.clone(),
                media_type: media_type.clone(),
                data: data.clone(),
            });
            json!({"kind": "image", "image_ref": image_ref, "media_type": media_type})
        }
        ContentBlock::AnthropicThinking {
            thinking,
            signature,
        } => json!({
            "kind": "anthropic_thinking",
            "thinking": thinking,
            "signature_present": !signature.is_empty()
        }),
        ContentBlock::OpenAIReasoning {
            id,
            summary,
            status,
            encrypted_content,
        } => json!({
            "kind": "openai_reasoning",
            "id": id,
            "summary": summary,
            "status": status,
            "encrypted_content_present": encrypted_content.is_some()
        }),
        ContentBlock::ToolUse {
            id,
            name,
            input,
            thought_signature,
        } => json!({
            "kind": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "thought_signature_present": thought_signature.is_some()
        }),
        ContentBlock::OpenAICompaction { encrypted_content } => json!({
            "kind": "legacy_openai_compaction",
            "encrypted_content_present": !encrypted_content.is_empty()
        }),
        other => serde_json::to_value(other).map_err(|error| {
            ContextCuratorError::InvalidResponse(format!(
                "failed to serialize complete curator source block: {error}"
            ))
        })?,
    };
    Ok(payload)
}

fn tool_source_exclusions(
    messages: &[StoredMessage],
    tools: &[ContextCuratorToolWork],
    target_index: &ContextTargetIndex<'_>,
) -> Result<BTreeMap<usize, BTreeSet<usize>>, ContextCuratorError> {
    let mut exclusions = BTreeMap::<usize, BTreeSet<usize>>::new();
    for tool in tools {
        let target = target_index
            .resolve_content_target(&tool.target)
            .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
        exclusions
            .entry(target.message_index)
            .or_default()
            .insert(target.block_index);
        for (message_index, message) in messages.iter().enumerate() {
            for (block_index, block) in message.content.iter().enumerate() {
                if matches!(block, ContentBlock::ToolUse { id, .. } if id == &tool.tool_call_id) {
                    exclusions
                        .entry(message_index)
                        .or_default()
                        .insert(block_index);
                }
            }
        }
    }
    Ok(exclusions)
}

fn tool_scope(
    messages: &[StoredMessage],
    tool: &ContextCuratorToolWork,
    all_tools: &[ContextCuratorToolWork],
    selected_ranges: &[(usize, usize)],
    exclusions: &BTreeMap<usize, BTreeSet<usize>>,
    target_index: &ContextTargetIndex<'_>,
) -> Result<Vec<ScopedMessage>, ContextCuratorError> {
    let target = target_index
        .resolve_content_target(&tool.target)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    let call_blocks = messages
        .iter()
        .enumerate()
        .flat_map(|(message_index, message)| {
            message
                .content
                .iter()
                .enumerate()
                .filter_map(move |(block_index, block)| {
                    matches!(block, ContentBlock::ToolUse { id, .. } if id == &tool.tool_call_id)
                        .then_some((message_index, block_index))
                })
        })
        .collect::<Vec<_>>();
    if call_blocks.len() != 1 {
        return Err(ContextCuratorError::InvalidResponse(format!(
            "tool task {:?} requires exactly one matching ToolUse, found {}",
            tool.request_id,
            call_blocks.len()
        )));
    }
    let (call_message_index, call_block_index) = call_blocks[0];
    let mut scope = vec![
        ScopedMessage {
            purpose: ContextCuratorSourcePurpose::PrimaryToolCall,
            message_index: call_message_index,
            block_ordinals: vec![call_block_index],
            includes_all_blocks: false,
        },
        ScopedMessage {
            purpose: ContextCuratorSourcePurpose::PrimaryToolResult,
            message_index: target.message_index,
            block_ordinals: vec![target.block_index],
            includes_all_blocks: false,
        },
    ];

    let own_source = BTreeSet::from([
        (call_message_index, call_block_index),
        (target.message_index, target.block_index),
    ]);
    let supporting_ordinals = |message_index: usize| -> Vec<usize> {
        if selected_ranges
            .iter()
            .any(|(start, end)| *start <= message_index && message_index <= *end)
        {
            return Vec::new();
        }
        let excluded = exclusions.get(&message_index);
        messages[message_index]
            .content
            .iter()
            .enumerate()
            .filter_map(|(block_index, _)| {
                (!own_source.contains(&(message_index, block_index))
                    && !excluded.is_some_and(|items| items.contains(&block_index)))
                .then_some(block_index)
            })
            .collect()
    };

    let before = call_message_index.min(target.message_index);
    if let Some(prior_user_index) = (0..before).rev().find(|index| {
        messages[*index].role == Role::User && !supporting_ordinals(*index).is_empty()
    }) {
        let ordinals = supporting_ordinals(prior_user_index);
        scope.push(scoped_support(messages, prior_user_index, ordinals));
    }
    for message_index in [call_message_index, target.message_index] {
        let ordinals = supporting_ordinals(message_index);
        if !ordinals.is_empty() {
            scope.push(scoped_support(messages, message_index, ordinals));
        }
    }
    for message_index in target.message_index.saturating_add(1)..messages.len() {
        let ordinals = supporting_ordinals(message_index);
        if !ordinals.is_empty() {
            scope.push(scoped_support(messages, message_index, ordinals));
        }
    }

    debug_assert!(
        all_tools
            .iter()
            .any(|candidate| candidate.request_id == tool.request_id)
    );
    Ok(scope)
}

fn scoped_support(
    messages: &[StoredMessage],
    message_index: usize,
    ordinals: Vec<usize>,
) -> ScopedMessage {
    let includes_all_blocks = ordinals.len() == messages[message_index].content.len();
    ScopedMessage {
        purpose: ContextCuratorSourcePurpose::SupportingConversation,
        message_index,
        block_ordinals: if includes_all_blocks {
            Vec::new()
        } else {
            ordinals
        },
        includes_all_blocks,
    }
}

fn public_scope(
    messages: &[StoredMessage],
    task: &ContextCuratorTask,
) -> Vec<ContextCuratorSourceScope> {
    let mut scope = task
        .scope
        .iter()
        .map(|item| ContextCuratorSourceScope {
            purpose: item.purpose,
            message_id: messages
                .get(item.message_index)
                .map(|message| message.id.clone()),
            stored_index: Some(item.message_index),
            block_ordinals: item.block_ordinals.clone(),
            includes_all_blocks: item.includes_all_blocks,
        })
        .collect::<Vec<_>>();
    scope.extend(
        task.active_summary_texts
            .iter()
            .map(|_| ContextCuratorSourceScope {
                purpose: ContextCuratorSourcePurpose::ActiveSummary,
                message_id: None,
                stored_index: None,
                block_ordinals: Vec::new(),
                includes_all_blocks: true,
            }),
    );
    scope
}

fn task_target_label(task: &ContextCuratorTask) -> String {
    match &task.kind {
        ContextCuratorTaskKind::Range(work) => format!(
            "range {}..{} ({} messages)",
            work.source_range.start_index_hint,
            work.source_range.end_index_hint,
            work.source_range.message_count
        ),
        ContextCuratorTaskKind::Tool(work) => {
            format!("{} result for call {}", work.tool_name, work.tool_call_id)
        }
    }
}

fn range_system_prompt(transaction: &str, task: &str) -> String {
    effective_prompt(
        RANGE_SUMMARIZER_BASE_PROMPT,
        RANGE_MANDATORY_INSTRUCTIONS,
        transaction,
        task,
    )
}

fn tool_system_prompt(transaction: &str) -> String {
    effective_prompt(
        TOOL_DISTILLER_BASE_PROMPT,
        TOOL_MANDATORY_INSTRUCTIONS,
        transaction,
        "",
    )
}

fn effective_prompt(base: &str, mandatory: &str, transaction: &str, task: &str) -> String {
    let mut prompt = format!("{base}\n\n{mandatory}");
    if !transaction.trim().is_empty() {
        prompt
            .push_str("\n\nUser-approved additional instructions for this context transaction:\n");
        prompt.push_str(transaction);
    }
    if !task.trim().is_empty() {
        prompt.push_str("\n\nUser-approved additional instructions for this specific range:\n");
        prompt.push_str(task);
    }
    if !transaction.trim().is_empty() || !task.trim().is_empty() {
        prompt.push_str(
            "\n\nThese additional instructions are additive. Ignore any part that conflicts with the base role, mandatory preservation guarantees, complete-source semantics, or output contract.",
        );
    }
    prompt
}

fn range_response_schema() -> Value {
    json!({
        "summary": "non-empty lossless summary",
        "file_change_digest": "evidence-based digest; may state that no files changed",
        "warnings": ["uncertainty or preservation warning"]
    })
}

fn tool_response_schema() -> Value {
    json!({
        "eligible": true,
        "replacement": "required and non-empty only when eligible",
        "preservation_rationale": "required and non-empty only when eligible",
        "ineligible_reason": "required and non-empty only when ineligible",
        "uncertainties": ["uncertainty"]
    })
}

fn pretty_json(value: &Value) -> Result<String, ContextCuratorError> {
    serde_json::to_string_pretty(value)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratorRangeResponse {
    summary: String,
    file_change_digest: String,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratorToolResponse {
    eligible: bool,
    replacement: Option<String>,
    preservation_rationale: Option<String>,
    ineligible_reason: Option<String>,
    uncertainties: Vec<String>,
}

fn parse_range_response(raw: &str) -> Result<ContextCuratorRangeArtifact, ContextCuratorError> {
    let response: CuratorRangeResponse = serde_json::from_str(raw.trim())
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    if response.summary.trim().is_empty() {
        return Err(ContextCuratorError::InvalidResponse(
            "range summary is empty".to_string(),
        ));
    }
    if response.file_change_digest.trim().is_empty() {
        return Err(ContextCuratorError::InvalidResponse(
            "range file-change digest is empty".to_string(),
        ));
    }
    Ok(ContextCuratorRangeArtifact {
        summary: response.summary,
        file_change_digest: response.file_change_digest,
        warnings: response.warnings,
    })
}

fn parse_tool_response(
    raw: &str,
    work: &ContextCuratorToolWork,
) -> Result<ContextCuratorToolArtifact, ContextCuratorError> {
    let response: CuratorToolResponse = serde_json::from_str(raw.trim())
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    if response.eligible {
        let replacement = response
            .replacement
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ContextCuratorError::InvalidResponse(
                    "eligible tool result has no replacement".to_string(),
                )
            })?;
        let rationale = response
            .preservation_rationale
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ContextCuratorError::InvalidResponse(
                    "eligible tool result has no preservation rationale".to_string(),
                )
            })?;
        if response.ineligible_reason.is_some() {
            return Err(ContextCuratorError::InvalidResponse(
                "eligible tool result also supplied an ineligible reason".to_string(),
            ));
        }
        let replacement_block = replacement_tool_result_block(work, replacement.clone());
        let replacement_token_estimate = estimate_content_block_tokens(&replacement_block);
        if work.original_token_estimate == 0
            || (replacement_token_estimate as u128).saturating_mul(100)
                >= (work.original_token_estimate as u128).saturating_mul(20)
        {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "eligible tool request {:?} replacement is not strictly below 20 percent ({} of {} estimated tokens)",
                work.request_id, replacement_token_estimate, work.original_token_estimate
            )));
        }
        Ok(ContextCuratorToolArtifact::Eligible {
            replacement,
            replacement_token_estimate,
            preservation_rationale: rationale,
            uncertainties: response.uncertainties,
        })
    } else {
        if response.replacement.is_some() || response.preservation_rationale.is_some() {
            return Err(ContextCuratorError::InvalidResponse(
                "ineligible tool result supplied eligible-only fields".to_string(),
            ));
        }
        let reason = response
            .ineligible_reason
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ContextCuratorError::InvalidResponse(
                    "ineligible tool result has no reason".to_string(),
                )
            })?;
        Ok(ContextCuratorToolArtifact::Ineligible {
            reason,
            uncertainties: response.uncertainties,
        })
    }
}

fn replacement_tool_result_block(
    work: &ContextCuratorToolWork,
    replacement: String,
) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: work.tool_call_id.clone(),
        content: replacement,
        is_error: work.is_error,
    }
}

fn task_prompt_version(role: StoredContextCuratorRole) -> &'static str {
    match role {
        StoredContextCuratorRole::RangeSummarizer => CONTEXT_RANGE_SUMMARIZER_PROMPT_VERSION,
        StoredContextCuratorRole::ToolResultDistiller => CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
    }
}

fn nonempty_owned(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::EventStream;
    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use futures::stream;
    use jcode_context_core::{build_content_target, build_message_range};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct RouteProviderState {
        model: Mutex<String>,
        route_selections: Mutex<Vec<RouteSelection>>,
        efforts: Mutex<Vec<String>>,
    }

    #[derive(Clone)]
    struct RouteProvider {
        name: &'static str,
        display_name: &'static str,
        state: Arc<RouteProviderState>,
    }

    impl RouteProvider {
        fn new(name: &'static str, display_name: &'static str, model: &str) -> Self {
            Self {
                name,
                display_name,
                state: Arc::new(RouteProviderState {
                    model: Mutex::new(model.to_string()),
                    ..RouteProviderState::default()
                }),
            }
        }

        fn route_selections(&self) -> Vec<RouteSelection> {
            self.state
                .route_selections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn efforts(&self) -> Vec<String> {
            self.state
                .efforts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    #[async_trait]
    impl Provider for RouteProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            unreachable!("route resolution does not invoke the provider")
        }

        fn name(&self) -> &str {
            self.name
        }

        fn display_name(&self) -> String {
            self.display_name.to_string()
        }

        fn model(&self) -> String {
            self.state
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn set_model(&self, model: &str) -> Result<()> {
            if model == "rejected-model" {
                bail!("route test rejected model {model}");
            }
            *self
                .state
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
            Ok(())
        }

        fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
            *self
                .state
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = selection.model.clone();
            self.state
                .route_selections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(selection.clone());
            Ok(())
        }

        fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
            if effort == "rejected-effort" {
                bail!("route test rejected effort {effort}");
            }
            self.state
                .efforts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(effort.to_string());
            Ok(())
        }

        fn reasoning_effort(&self) -> Option<String> {
            self.state
                .efforts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last()
                .cloned()
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    fn model_route(model: &str, provider: &str, api_method: &str, available: bool) -> ModelRoute {
        ModelRoute {
            model: model.to_string(),
            provider: provider.to_string(),
            api_method: api_method.to_string(),
            available,
            detail: format!("{provider} test route"),
            cheapness: None,
        }
    }

    #[test]
    fn route_resolution_defaults_to_the_unchanged_independent_active_fork() {
        let provider = RouteProvider::new("route-test", "Anthropic", "claude-fable-5");
        let resolved = resolve_context_curator_route(
            Arc::new(provider.clone()),
            &[],
            "claude-api",
            &ContextCuratorConfig::default(),
        )
        .expect("default independent route");

        assert_eq!(resolved.provider_name, "route-test");
        assert_eq!(resolved.provider_display_name, "Anthropic");
        assert_eq!(resolved.model, "claude-fable-5");
        assert_eq!(resolved.route, "claude-api");
        assert_eq!(resolved.effort, None);
        assert!(provider.route_selections().is_empty());
        assert!(provider.efforts().is_empty());
        assert_eq!(
            resolved.pricing.billing_mode,
            StoredContextBillingMode::Metered
        );
        assert_eq!(resolved.pricing.input_usd_per_million, Some(10.0));
    }

    #[test]
    fn route_resolution_applies_exact_provider_model_and_effort_selection() {
        let provider = RouteProvider::new("route-test", "Anthropic", "active-model");
        let routes = vec![
            model_route("target-model", "Anthropic", "claude-api", true),
            model_route("target-model", "Anthropic", "claude-oauth", false),
            model_route("other-model", "Anthropic", "claude-api", true),
        ];
        let config = ContextCuratorConfig {
            provider: Some("anthropic".to_string()),
            route: None,
            model: Some("target-model".to_string()),
            effort: Some("high".to_string()),
        };

        let resolved = resolve_context_curator_route(
            Arc::new(provider.clone()),
            &routes,
            "active-route",
            &config,
        )
        .expect("explicit route");

        assert_eq!(resolved.model, "target-model");
        assert_eq!(resolved.route, "claude-api");
        assert_eq!(resolved.effort.as_deref(), Some("high"));
        assert_eq!(provider.route_selections().len(), 1);
        assert_eq!(provider.route_selections()[0].api_method, "claude-api");
        assert_eq!(provider.efforts(), ["high"]);
    }

    #[test]
    fn route_resolution_applies_model_only_without_reselecting_the_active_route() {
        let provider = RouteProvider::new("route-test", "Route Test", "active-model");
        let config = ContextCuratorConfig {
            model: Some("replacement-model".to_string()),
            ..ContextCuratorConfig::default()
        };

        let resolved =
            resolve_context_curator_route(Arc::new(provider.clone()), &[], "active-route", &config)
                .expect("model-only route");

        assert_eq!(resolved.model, "replacement-model");
        assert_eq!(resolved.route, "active-route");
        assert!(provider.route_selections().is_empty());
    }

    #[test]
    fn route_resolution_rejects_missing_ambiguous_unavailable_and_empty_selectors() {
        let provider = RouteProvider::new("route-test", "Route Test", "target-model");
        let routes = vec![
            model_route("target-model", "Provider A", "route-a", true),
            model_route("target-model", "Provider A", "route-b", true),
            model_route("target-model", "Unavailable", "route-c", false),
        ];

        let mismatch = resolve_context_curator_route(
            Arc::new(provider.clone()),
            &routes,
            "active-route",
            &ContextCuratorConfig {
                provider: Some("missing".to_string()),
                route: None,
                model: Some("target-model".to_string()),
                effort: None,
            },
        )
        .expect_err("missing route must fail");
        assert!(mismatch.to_string().contains("no available route matches"));

        let ambiguous = resolve_context_curator_route(
            Arc::new(provider.clone()),
            &routes,
            "active-route",
            &ContextCuratorConfig {
                provider: Some("provider a".to_string()),
                route: None,
                model: Some("target-model".to_string()),
                effort: None,
            },
        )
        .expect_err("ambiguous route must fail");
        let message = ambiguous.to_string();
        assert!(message.contains("ambiguous"));
        assert!(message.contains("Provider A (route-a)"));
        assert!(message.contains("Provider A (route-b)"));

        for selector in ["unavailable", ""] {
            let error = resolve_context_curator_route(
                Arc::new(provider.clone()),
                &routes,
                "active-route",
                &ContextCuratorConfig {
                    provider: Some(selector.to_string()),
                    route: None,
                    model: Some("target-model".to_string()),
                    effort: None,
                },
            )
            .expect_err("unsafe selector must fail");
            let message = error.to_string();
            if selector.is_empty() {
                assert!(message.contains("selector is empty"));
            } else {
                assert!(message.contains("no available route matches"));
            }
        }
    }

    #[test]
    fn route_resolution_requires_stable_provider_model_and_route_identity() {
        for (provider, active_route, expected) in [
            (
                RouteProvider::new("", "Empty Provider", "model"),
                "active-route",
                "stable provider and model identity",
            ),
            (
                RouteProvider::new("route-test", "Empty Model", ""),
                "active-route",
                "stable provider and model identity",
            ),
            (
                RouteProvider::new("route-test", "Empty Route", "model"),
                "",
                "stable route identity",
            ),
        ] {
            let error = resolve_context_curator_route(
                Arc::new(provider),
                &[],
                active_route,
                &ContextCuratorConfig::default(),
            )
            .expect_err("unstable identity must fail");
            assert!(error.to_string().contains(expected));
        }
    }

    #[derive(Clone)]
    enum ScriptedBehavior {
        Events(Vec<StreamEvent>),
        RoleResponses { range: String, tool: String },
        EventsThenPending(Vec<StreamEvent>),
        CompletePending,
        CompleteError(String),
    }

    #[derive(Clone, Debug)]
    struct CapturedProviderCall {
        messages: Vec<Message>,
        tool_count: usize,
        system: String,
    }

    #[derive(Clone)]
    struct ScriptedProvider {
        name: String,
        model: String,
        supports_images: bool,
        behavior: ScriptedBehavior,
        calls: Arc<AtomicUsize>,
        call_notify: Arc<Notify>,
        captured: Arc<Mutex<Vec<CapturedProviderCall>>>,
    }

    impl ScriptedProvider {
        fn new(name: &str, behavior: ScriptedBehavior) -> Self {
            Self {
                name: name.to_string(),
                model: "curator-test-model".to_string(),
                supports_images: false,
                behavior,
                calls: Arc::new(AtomicUsize::new(0)),
                call_notify: Arc::new(Notify::new()),
                captured: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_image_support(mut self) -> Self {
            self.supports_images = true;
            self
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        async fn wait_for_call(&self) {
            loop {
                if self.call_count() > 0 {
                    return;
                }
                let notified = self.call_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.call_count() > 0 {
                    return;
                }
                notified.await;
            }
        }

        fn captured_calls(&self) -> Vec<CapturedProviderCall> {
            self.captured
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn complete(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
            system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.captured
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(CapturedProviderCall {
                    messages: messages.to_vec(),
                    tool_count: tools.len(),
                    system: system.to_string(),
                });
            self.call_notify.notify_waiters();
            match self.behavior.clone() {
                ScriptedBehavior::Events(events) => Ok(Box::pin(stream::iter(
                    events.into_iter().map(Ok::<_, anyhow::Error>),
                ))),
                ScriptedBehavior::EventsThenPending(events) => Ok(Box::pin(
                    stream::iter(events.into_iter().map(Ok::<_, anyhow::Error>))
                        .chain(stream::pending()),
                )),
                ScriptedBehavior::RoleResponses { range, tool } => {
                    let response = if system.starts_with(RANGE_SUMMARIZER_BASE_PROMPT) {
                        range
                    } else {
                        tool
                    };
                    Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::TextDelta(
                        response,
                    ))])))
                }
                ScriptedBehavior::CompletePending => {
                    futures::future::pending::<()>().await;
                    unreachable!("pending curator provider completed")
                }
                ScriptedBehavior::CompleteError(message) => bail!(message),
            }
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn display_name(&self) -> String {
            format!("{} display", self.name)
        }

        fn model(&self) -> String {
            self.model.clone()
        }

        fn context_window(&self) -> usize {
            100_000
        }

        fn supports_image_input(&self) -> bool {
            self.supports_images
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    fn message_with_result(content: &str) -> Vec<StoredMessage> {
        vec![StoredMessage {
            origin: None,
            id: "result".to_string(),
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call".to_string(),
                content: content.to_string(),
                is_error: Some(false),
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }]
    }

    fn tool_work(messages: &[StoredMessage], original: usize) -> ContextCuratorToolWork {
        let ContentBlock::ToolResult { is_error, .. } = &messages[0].content[0] else {
            panic!("tool-result fixture");
        };
        ContextCuratorToolWork {
            request_id: "tool-1".to_string(),
            target: build_content_target(messages, 0, 0).expect("target"),
            message_index: 0,
            tool_name: "read".to_string(),
            tool_call_id: "call".to_string(),
            tool_input: json!({"file_path": "src/lib.rs"}),
            is_error: *is_error,
            original_token_estimate: original,
        }
    }

    fn complete_tool_fixture(
        content: &str,
        original: usize,
    ) -> (Vec<StoredMessage>, ContextCuratorToolWork) {
        let messages = vec![
            StoredMessage {
                origin: None,
                id: "call-message".to_string(),
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call".to_string(),
                    name: "read".to_string(),
                    input: json!({"file_path": "src/lib.rs"}),
                    thought_signature: None,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            },
            StoredMessage {
                origin: None,
                id: "result".to_string(),
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: content.to_string(),
                    is_error: Some(false),
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            },
        ];
        let work = ContextCuratorToolWork {
            request_id: "tool-1".to_string(),
            target: build_content_target(&messages, 1, 0).expect("target"),
            message_index: 1,
            tool_name: "read".to_string(),
            tool_call_id: "call".to_string(),
            tool_input: json!({"file_path": "src/lib.rs"}),
            is_error: Some(false),
            original_token_estimate: original,
        };
        (messages, work)
    }

    fn range_work(messages: &[StoredMessage]) -> ContextCuratorRangeWork {
        ContextCuratorRangeWork {
            request_id: "range-1".to_string(),
            source_range: build_message_range(messages, 0, messages.len() - 1).expect("range"),
            file_evidence: complete_file_evidence(vec!["src/lib.rs".to_string()]),
            additional_instructions: String::new(),
        }
    }

    fn complete_file_evidence(changed_paths: Vec<String>) -> StoredContextFileEvidence {
        StoredContextFileEvidence {
            changed: StoredContextPathEvidence {
                paths: changed_paths,
                complete: true,
                warnings: Vec::new(),
            },
            read_or_inspected: StoredContextPathEvidence {
                complete: true,
                ..StoredContextPathEvidence::default()
            },
            searched_or_browsed: StoredContextPathEvidence {
                complete: true,
                ..StoredContextPathEvidence::default()
            },
        }
    }

    fn isolated_tool_task(messages: &[StoredMessage]) -> ContextCuratorTask {
        ContextCuratorTask {
            task_id: "tool-1".to_string(),
            role: StoredContextCuratorRole::ToolResultDistiller,
            kind: ContextCuratorTaskKind::Tool(tool_work(messages, 1_000)),
            system_prompt: tool_system_prompt(""),
            response_contract: pretty_json(&tool_response_schema()).expect("schema"),
            scope: Vec::new(),
            active_summary_texts: Vec::new(),
        }
    }

    fn pricing(
        billing_mode: StoredContextBillingMode,
        input: Option<f64>,
        output: Option<f64>,
        cache_read: Option<f64>,
        cache_write: Option<f64>,
    ) -> StoredContextPricingSnapshot {
        StoredContextPricingSnapshot {
            billing_mode,
            input_usd_per_million: input,
            output_usd_per_million: output,
            cache_read_usd_per_million: cache_read,
            cache_write_usd_per_million: cache_write,
            input_price_tiers: Vec::new(),
            cache_warmth: jcode_session_types::StoredContextCacheWarmth::Unknown,
        }
    }

    fn curator_route(
        provider: Arc<dyn Provider>,
        pricing: StoredContextPricingSnapshot,
    ) -> ContextCuratorRoute {
        ContextCuratorRoute {
            provider_name: provider.name().to_string(),
            provider_display_name: provider.display_name(),
            model: provider.model(),
            route: "curator-test-route".to_string(),
            effort: None,
            provider,
            pricing,
        }
    }

    fn ineligible_tool_response() -> String {
        serde_json::to_string(&json!({
            "eligible": false,
            "replacement": null,
            "preservation_rationale": null,
            "ineligible_reason": "safe reduction is unavailable",
            "uncertainties": ["later references may depend on the full output"]
        }))
        .expect("response JSON")
    }

    fn range_response() -> String {
        serde_json::to_string(&json!({
            "summary": "All operationally relevant range facts are preserved.",
            "file_change_digest": "Updated src/lib.rs.",
            "warnings": []
        }))
        .expect("response JSON")
    }

    #[test]
    fn role_parsers_reject_missing_unknown_malformed_and_non_document_json() {
        let messages = message_with_result(&"x".repeat(4_000));
        let work = tool_work(&messages, 4_000);
        for raw in [
            r#"{}"#,
            r#"{"eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[],"extra":true}"#,
            r#"[]"#,
            "not json",
            r#"```json
{"eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}
```"#,
            r#"{"eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]} trailing"#,
        ] {
            assert!(parse_tool_response(raw, &work).is_err());
        }
        for raw in [
            r#"{}"#,
            r#"{"summary":"summary","file_change_digest":"","warnings":[],"extra":true}"#,
            r#"{"summary":"summary","file_change_digest":"   ","warnings":[]}"#,
            r#"{"summary":"   ","file_change_digest":"","warnings":[]}"#,
            r#"[]"#,
            "not json",
        ] {
            assert!(parse_range_response(raw).is_err());
        }
    }

    #[test]
    fn range_parser_accepts_exact_single_artifact_contract() {
        let artifact = parse_range_response(&range_response()).expect("range artifact");
        assert_eq!(
            artifact.summary,
            "All operationally relevant range facts are preserved."
        );
        assert_eq!(artifact.file_change_digest, "Updated src/lib.rs.");
        assert!(artifact.warnings.is_empty());
    }

    #[test]
    fn tool_parser_rejects_inconsistent_eligible_and_ineligible_fields() {
        let messages = message_with_result(&"x".repeat(4_000));
        let work = tool_work(&messages, 4_000);
        for raw in [
            r#"{"eligible":true,"replacement":"short","preservation_rationale":null,"ineligible_reason":null,"uncertainties":[]}"#,
            r#"{"eligible":true,"replacement":"short","preservation_rationale":"complete","ineligible_reason":"contradiction","uncertainties":[]}"#,
            r#"{"eligible":true,"replacement":"short","preservation_rationale":"complete","ineligible_reason":"","uncertainties":[]}"#,
            r#"{"eligible":false,"replacement":"short","preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}"#,
            r#"{"eligible":false,"replacement":null,"preservation_rationale":"complete","ineligible_reason":"no","uncertainties":[]}"#,
            r#"{"eligible":false,"replacement":"","preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}"#,
            r#"{"eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":null,"uncertainties":[]}"#,
        ] {
            assert!(parse_tool_response(raw, &work).is_err());
        }
    }

    #[test]
    fn parser_enforces_strict_twenty_percent_boundary() {
        let messages = message_with_result(&"x".repeat(4_000));
        let replacement = "y".repeat(60);
        let replacement_block = ContentBlock::ToolResult {
            tool_use_id: "call".to_string(),
            content: replacement.clone(),
            is_error: Some(false),
        };
        let replacement_tokens = estimate_content_block_tokens(&replacement_block);
        let exact_original = replacement_tokens * 5;
        let exact = tool_work(&messages, exact_original);
        let raw = serde_json::to_string(&json!({
            "eligible": true,
            "replacement": replacement,
            "preservation_rationale": "complete",
            "ineligible_reason": null,
            "uncertainties": []
        }))
        .expect("json");
        assert!(parse_tool_response(&raw, &exact).is_err());

        let below = tool_work(&messages, exact_original + 1);
        assert!(matches!(
            parse_tool_response(&raw, &below).expect("below 20 percent"),
            ContextCuratorToolArtifact::Eligible { .. }
        ));
    }

    #[test]
    fn replacement_block_preserves_original_tool_result_error_metadata() {
        let mut messages = message_with_result("original error output");
        let ContentBlock::ToolResult { is_error, .. } = &mut messages[0].content[0] else {
            panic!("tool-result fixture");
        };
        *is_error = Some(true);
        let replacement = "distilled error".to_string();
        let mut work = tool_work(&messages, 0);
        let replacement_block = replacement_tool_result_block(&work, replacement.clone());
        assert!(matches!(
            replacement_block,
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: Some(true),
            } if tool_use_id == "call" && content == "distilled error"
        ));

        let replacement_tokens = estimate_content_block_tokens(&replacement_tool_result_block(
            &work,
            replacement.clone(),
        ));
        let raw = serde_json::to_string(&json!({
            "eligible": true,
            "replacement": replacement,
            "preservation_rationale": "the complete error remains actionable",
            "ineligible_reason": null,
            "uncertainties": []
        }))
        .expect("error-result response");
        work.original_token_estimate = replacement_tokens.saturating_mul(5);
        assert!(parse_tool_response(&raw, &work).is_err());

        work.original_token_estimate = work.original_token_estimate.saturating_add(1);
        assert!(matches!(
            parse_tool_response(&raw, &work)
                .expect("error-result replacement strictly below 20 percent"),
            ContextCuratorToolArtifact::Eligible {
                replacement_token_estimate,
                ..
            } if replacement_token_estimate == replacement_tokens
        ));
    }

    #[test]
    fn parser_retains_ineligible_candidate_reason_and_uncertainty() {
        let messages = message_with_result(&"x".repeat(1_000));
        let work = tool_work(&messages, 1_000);
        assert_eq!(
            parse_tool_response(&ineligible_tool_response(), &work).expect("ineligible artifact"),
            ContextCuratorToolArtifact::Ineligible {
                reason: "safe reduction is unavailable".to_string(),
                uncertainties: vec!["later references may depend on the full output".to_string()],
            }
        );
    }

    #[tokio::test]
    async fn three_ranges_and_five_tools_use_eight_isolated_role_specific_complete_calls() {
        let mut messages = Vec::new();
        let mut ranges = Vec::new();
        for index in 0..3 {
            messages.push(StoredMessage {
                origin: None,
                id: format!("range-message-{index}"),
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("RANGE_SOURCE_SENTINEL_{index}"),
                    cache_control: None,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            });
            let message_index = messages.len() - 1;
            ranges.push(ContextCuratorRangeWork {
                request_id: format!("range-{}", index + 1),
                source_range: build_message_range(&messages, message_index, message_index)
                    .expect("range"),
                file_evidence: complete_file_evidence(vec![format!("src/range_{index}.rs")]),
                additional_instructions: format!("RANGE_ONLY_INSTRUCTION_{index}"),
            });
        }
        let mut tools = Vec::new();
        for index in 0..5 {
            let call_id = format!("call-{index}");
            messages.push(StoredMessage {
                origin: None,
                id: format!("tool-call-message-{index}"),
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: "read".to_string(),
                    input: json!({"file_path": format!("src/tool_{index}.rs")}),
                    thought_signature: None,
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            });
            messages.push(StoredMessage {
                origin: None,
                id: format!("tool-result-message-{index}"),
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: format!("TOOL_RESULT_SENTINEL_{index}_{}", "x".repeat(2_000)),
                    is_error: Some(false),
                }],
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            });
            let message_index = messages.len() - 1;
            tools.push(ContextCuratorToolWork {
                request_id: format!("tool-{}", index + 1),
                target: build_content_target(&messages, message_index, 0).expect("target"),
                message_index,
                tool_name: "read".to_string(),
                tool_call_id: call_id,
                tool_input: json!({"file_path": format!("src/tool_{index}.rs")}),
                is_error: Some(false),
                original_token_estimate: 1_000,
            });
        }

        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::RoleResponses {
                range: range_response(),
                tool: serde_json::to_string(&json!({
                    "eligible": true,
                    "replacement": "Distilled output retained the exact operational result.",
                    "preservation_rationale": "All continuation-relevant facts are retained.",
                    "ineligible_reason": null,
                    "uncertainties": []
                }))
                .expect("tool response"),
            },
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let plan = build_context_curator_plan(
            &route,
            ContextCuratorPlanInput {
                session_id: "session",
                context_revision: 7,
                transcript_digest: 42,
                messages: &messages,
                ranges: &ranges,
                tools: &tools,
                active_summary_texts: &[],
                transaction_instructions: "TRANSACTION_WIDE_INSTRUCTION",
                selection_source: StoredContextCuratorSelectionSource::PerRunOverride,
            },
            ContextCuratorLimits::default(),
        )
        .expect("complete atomic plan");
        assert_eq!(plan.preview.tasks.len(), 8);
        assert!(!plan.preview.using_configured_default);
        for (index, task) in plan.preview.tasks.iter().take(3).enumerate() {
            let evidence = task.file_evidence.as_ref().expect("range file evidence");
            assert_eq!(evidence.changed.paths, [format!("src/range_{index}.rs")]);
            assert!(evidence.changed.complete);
            assert!(evidence.read_or_inspected.complete);
            assert!(evidence.searched_or_browsed.complete);
            assert_eq!(
                task.user_instructions.transaction_wide_chars,
                "TRANSACTION_WIDE_INSTRUCTION".chars().count()
            );
            assert_eq!(
                task.user_instructions.task_specific_chars,
                format!("RANGE_ONLY_INSTRUCTION_{index}").chars().count()
            );
        }
        for task in plan.preview.tasks.iter().skip(3) {
            assert!(task.file_evidence.is_none());
            assert_eq!(task.user_instructions.task_specific_chars, 0);
        }

        let artifacts = run_context_curator_plan(
            &route,
            &messages,
            &plan,
            &CancellationToken::new(),
            ContextCuratorLimits::default(),
            |_, _| {},
        )
        .await
        .expect("eight isolated curator calls");
        assert_eq!(artifacts.range_summaries.len(), 3);
        assert_eq!(artifacts.tool_distillations.len(), 5);
        assert_eq!(provider.call_count(), 8);

        let calls = provider.captured_calls();
        for (index, call) in calls.iter().take(3).enumerate() {
            assert!(call.system.starts_with(RANGE_SUMMARIZER_BASE_PROMPT));
            assert!(
                call.system
                    .contains("conversation_slice` as the authoritative primary source")
            );
            assert!(
                call.system
                    .contains("A search or directory browse does not prove that a file was read")
            );
            assert!(call.system.contains("TRANSACTION_WIDE_INSTRUCTION"));
            assert!(
                call.system
                    .contains(&format!("RANGE_ONLY_INSTRUCTION_{index}"))
            );
            for other in 0..3 {
                if other != index {
                    assert!(
                        !call
                            .system
                            .contains(&format!("RANGE_ONLY_INSTRUCTION_{other}"))
                    );
                }
            }
            let payload = match &call.messages[0].content[0] {
                ContentBlock::Text { text, .. } => text,
                other => panic!("expected JSON payload, got {other:?}"),
            };
            assert!(payload.contains(&format!("RANGE_SOURCE_SENTINEL_{index}")));
            for other in 0..3 {
                if other != index {
                    assert!(!payload.contains(&format!("RANGE_SOURCE_SENTINEL_{other}")));
                }
            }
            assert!(!payload.contains("TOOL_RESULT_SENTINEL_"));
            assert!(!payload.contains("request_id"));
            let payload_json: Value = serde_json::from_str(payload).expect("range payload JSON");
            assert_eq!(
                payload_json["harness_file_evidence"]["changed"]["paths"],
                json!([format!("src/range_{index}.rs")])
            );
            assert_eq!(
                payload_json["harness_file_evidence"]["read_or_inspected"]["complete"],
                json!(true)
            );
            assert_eq!(
                payload_json["harness_file_evidence"]["searched_or_browsed"]["paths"],
                json!([])
            );
        }
        for (index, call) in calls.iter().skip(3).take(5).enumerate() {
            assert!(call.system.starts_with(TOOL_DISTILLER_BASE_PROMPT));
            assert!(call.system.contains("TRANSACTION_WIDE_INSTRUCTION"));
            assert!(!call.system.contains("RANGE_ONLY_INSTRUCTION_"));
            let payload = match &call.messages[0].content[0] {
                ContentBlock::Text { text, .. } => text,
                other => panic!("expected JSON payload, got {other:?}"),
            };
            assert!(payload.contains(&format!("TOOL_RESULT_SENTINEL_{index}")));
            assert!(payload.contains(&format!("src/tool_{index}.rs")));
            for other in 0..5 {
                if other != index {
                    assert!(!payload.contains(&format!("TOOL_RESULT_SENTINEL_{other}")));
                }
            }
            assert!(!payload.contains("RANGE_SOURCE_SENTINEL_"));
            assert!(!payload.contains("request_id"));
        }
    }

    #[test]
    fn complete_atomic_source_overflow_fails_before_any_provider_call() {
        let messages = vec![StoredMessage {
            origin: None,
            id: "large-range".to_string(),
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "COMPLETE_SOURCE_SENTINEL".repeat(2_000),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }];
        let range = range_work(&messages);
        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(Vec::new()),
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let error = build_context_curator_plan(
            &route,
            ContextCuratorPlanInput {
                session_id: "session",
                context_revision: 0,
                transcript_digest: 1,
                messages: &messages,
                ranges: &[range],
                tools: &[],
                active_summary_texts: &[],
                transaction_instructions: "",
                selection_source: StoredContextCuratorSelectionSource::ConfiguredDefault,
            },
            ContextCuratorLimits {
                max_request_bytes: 128,
                ..ContextCuratorLimits::default()
            },
        )
        .expect_err("complete source must fail rather than truncate");
        assert!(matches!(error, ContextCuratorError::RequestTooLarge { .. }));
        assert_eq!(provider.call_count(), 0);

        let preview_error = build_context_curator_plan(
            &route,
            ContextCuratorPlanInput {
                session_id: "session",
                context_revision: 0,
                transcript_digest: 1,
                messages: &messages,
                ranges: &[range_work(&messages)],
                tools: &[],
                active_summary_texts: &[],
                transaction_instructions: "",
                selection_source: StoredContextCuratorSelectionSource::ConfiguredDefault,
            },
            ContextCuratorLimits {
                max_plan_preview_bytes: 1,
                ..ContextCuratorLimits::default()
            },
        )
        .expect_err("oversized exact preview must fail before generation");
        assert!(matches!(
            preview_error,
            ContextCuratorError::PlanPreviewTooLarge { .. }
        ));
        assert_eq!(provider.call_count(), 0);
    }

    #[test]
    fn exact_usage_cost_handles_split_inclusive_plain_unknown_and_non_metered_accounting() {
        let metered = pricing(
            StoredContextBillingMode::Metered,
            Some(10.0),
            Some(50.0),
            Some(1.0),
            Some(12.5),
        );
        let anthropic = exact_usage_cost(&metered, "anthropic", 100, 20, 40, 10)
            .expect("split accounting cost");
        assert!((anthropic - 0.002_165).abs() < 1e-12);
        let openai = exact_usage_cost(&metered, "openai", 100, 20, 40, 10)
            .expect("inclusive accounting cost");
        assert!((openai - 0.001_665).abs() < 1e-12);
        let unknown_plain = exact_usage_cost(&metered, "custom-provider", 100, 20, 0, 0)
            .expect("plain input/output cost does not need cache semantics");
        assert!((unknown_plain - 0.002).abs() < 1e-12);
        assert_eq!(
            exact_usage_cost(&metered, "custom-provider", 100, 20, 1, 0),
            None
        );

        let missing_read = pricing(
            StoredContextBillingMode::Metered,
            Some(10.0),
            Some(50.0),
            None,
            Some(12.5),
        );
        assert_eq!(
            exact_usage_cost(&missing_read, "anthropic", 100, 20, 1, 0),
            None
        );
        let missing_write = pricing(
            StoredContextBillingMode::Metered,
            Some(10.0),
            Some(50.0),
            Some(1.0),
            None,
        );
        assert_eq!(
            exact_usage_cost(&missing_write, "anthropic", 100, 20, 0, 1),
            None
        );
        for billing_mode in [
            StoredContextBillingMode::Subscription,
            StoredContextBillingMode::IncludedQuota,
            StoredContextBillingMode::Unknown,
        ] {
            let non_metered = pricing(billing_mode, Some(10.0), Some(50.0), Some(1.0), Some(12.5));
            assert_eq!(
                exact_usage_cost(&non_metered, "anthropic", 100, 20, 0, 0),
                None
            );
        }
    }

    #[test]
    fn exact_usage_cost_uses_input_triggered_input_and_output_price_tiers() {
        let mut tiered = pricing(
            StoredContextBillingMode::Metered,
            Some(5.0),
            Some(30.0),
            Some(0.5),
            None,
        );
        tiered.input_price_tiers = vec![jcode_session_types::StoredContextInputPriceTier {
            above_input_tokens: 100,
            input_usd_per_million: 10.0,
            output_usd_per_million: Some(45.0),
            cache_read_usd_per_million: Some(1.0),
            cache_write_usd_per_million: None,
        }];

        let at_boundary = exact_usage_cost(&tiered, "openai", 100, 20, 0, 0)
            .expect("base-band cost at the exact boundary");
        assert!((at_boundary - 0.001_1).abs() < 1e-12);

        let above_boundary = exact_usage_cost(&tiered, "openai", 101, 20, 0, 0)
            .expect("long-input tier cost above the boundary");
        assert!((above_boundary - 0.001_91).abs() < 1e-12);
    }

    #[test]
    fn curator_usage_requires_both_input_and_output_and_keeps_cache_fields_separate() {
        let provider = Arc::new(ScriptedProvider::new(
            "anthropic",
            ScriptedBehavior::Events(Vec::new()),
        ));
        let route = curator_route(
            provider,
            pricing(
                StoredContextBillingMode::Metered,
                Some(10.0),
                Some(50.0),
                Some(1.0),
                Some(12.5),
            ),
        );
        let messages = message_with_result("usage fixture");
        let task = isolated_tool_task(&messages);
        assert!(
            build_curator_usage(
                &route,
                &task,
                CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
                CuratorUsageAccumulator::default(),
            )
            .is_none()
        );
        assert!(
            build_curator_usage(
                &route,
                &task,
                CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
                CuratorUsageAccumulator {
                    input_tokens: Some(100),
                    ..CuratorUsageAccumulator::default()
                }
            )
            .is_none()
        );
        let usage = build_curator_usage(
            &route,
            &task,
            CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
            CuratorUsageAccumulator {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cache_read_input_tokens: Some(40),
                cache_creation_input_tokens: Some(10),
            },
        )
        .expect("complete usage");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cache_read_input_tokens, Some(40));
        assert_eq!(usage.cache_creation_input_tokens, Some(10));
        assert!((usage.cost_usd.expect("exact metered cost") - 0.002_165).abs() < 1e-12);
    }

    #[tokio::test]
    async fn response_collection_rejects_every_tool_output_shape() {
        let unexpected_tools = vec![
            StreamEvent::ToolUseStart {
                id: "call".to_string(),
                name: "bash".to_string(),
            },
            StreamEvent::ToolInputDelta("{}".to_string()),
            StreamEvent::ToolUseEnd,
            StreamEvent::ToolUseSignature("signature".to_string()),
            StreamEvent::ToolResult {
                tool_use_id: "call".to_string(),
                content: "result".to_string(),
                is_error: false,
            },
            StreamEvent::GeneratedImage {
                id: "image".to_string(),
                path: "image.png".to_string(),
                metadata_path: None,
                output_format: "png".to_string(),
                revised_prompt: None,
            },
            StreamEvent::NativeToolCall {
                request_id: "request".to_string(),
                tool_name: "bash".to_string(),
                input: json!({}),
            },
        ];
        for event in unexpected_tools {
            let provider = ScriptedProvider::new("curator", ScriptedBehavior::Events(vec![event]));
            assert!(matches!(
                collect_curator_response(&provider, &[], RANGE_SUMMARIZER_BASE_PROMPT, 1024,).await,
                Err(ContextCuratorError::UnexpectedToolUse)
            ));
        }
    }

    #[tokio::test]
    async fn response_collection_rejects_provider_errors_and_response_overflow() {
        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(vec![StreamEvent::Error {
                message: "provider event failed".to_string(),
                retry_after_secs: None,
            }]),
        );
        assert!(matches!(
            collect_curator_response(
                &provider,
                &[],
                RANGE_SUMMARIZER_BASE_PROMPT,
                1024,
            )
            .await,
            Err(ContextCuratorError::Provider(message)) if message == "provider event failed"
        ));

        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::CompleteError("provider completion failed".to_string()),
        );
        assert!(matches!(
            collect_curator_response(
                &provider,
                &[],
                RANGE_SUMMARIZER_BASE_PROMPT,
                1024,
            )
            .await,
            Err(ContextCuratorError::Provider(message)) if message.contains("provider completion failed")
        ));

        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(vec![StreamEvent::TextDelta("12345".to_string())]),
        );
        assert!(matches!(
            collect_curator_response(&provider, &[], RANGE_SUMMARIZER_BASE_PROMPT, 4).await,
            Err(ContextCuratorError::ResponseTooLarge { bytes: 5, limit: 4 })
        ));
    }

    #[tokio::test]
    async fn retry_rollback_discards_partial_json_and_usage() {
        let final_json = ineligible_tool_response();
        let provider = ScriptedProvider::new(
            "anthropic",
            ScriptedBehavior::Events(vec![
                StreamEvent::TextDelta("partial invalid JSON".to_string()),
                StreamEvent::TokenUsage {
                    input_tokens: Some(999),
                    output_tokens: Some(999),
                    cache_read_input_tokens: Some(999),
                    cache_creation_input_tokens: Some(999),
                },
                StreamEvent::RetryRollback { attempt: 1, max: 2 },
                StreamEvent::TextDelta(final_json.clone()),
                StreamEvent::TokenUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cache_read_input_tokens: Some(40),
                    cache_creation_input_tokens: Some(10),
                },
            ]),
        );
        let collected =
            collect_curator_response(&provider, &[], RANGE_SUMMARIZER_BASE_PROMPT, 16 * 1024)
                .await
                .expect("collected response");
        assert_eq!(collected.text, final_json);
        assert_eq!(collected.usage.input_tokens, Some(100));
        assert_eq!(collected.usage.output_tokens, Some(20));
        assert_eq!(collected.usage.cache_read_input_tokens, Some(40));
        assert_eq!(collected.usage.cache_creation_input_tokens, Some(10));
    }

    #[tokio::test]
    async fn canceled_request_fails_before_provider_poll_and_stream_cancellation_is_terminal() {
        let (messages, work) = complete_tool_fixture(&"x".repeat(1_000), 1_000);
        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::CompletePending,
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let canceled = CancellationToken::new();
        canceled.cancel();
        assert!(matches!(
            run_context_curator(
                &route,
                &messages,
                &[],
                std::slice::from_ref(&work),
                &[],
                &canceled,
                ContextCuratorLimits::default(),
            )
            .await,
            Err(ContextCuratorError::Canceled)
        ));
        assert_eq!(provider.call_count(), 0);

        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::EventsThenPending(vec![StreamEvent::TextDelta(
                "partial".to_string(),
            )]),
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task_messages = messages.clone();
        let task_work = work.clone();
        let task = tokio::spawn(async move {
            run_context_curator(
                &route,
                &task_messages,
                &[],
                &[task_work],
                &[],
                &task_cancellation,
                ContextCuratorLimits::default(),
            )
            .await
        });
        provider.wait_for_call().await;
        cancellation.cancel();
        assert!(matches!(
            task.await.expect("curator task"),
            Err(ContextCuratorError::Canceled)
        ));
    }

    #[tokio::test]
    async fn curator_timeout_and_request_bounds_fail_without_partial_artifacts() {
        let (messages, work) = complete_tool_fixture(&"x".repeat(1_000), 1_000);
        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::CompletePending,
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let timeout = run_context_curator(
            &route,
            &messages,
            &[],
            std::slice::from_ref(&work),
            &[],
            &CancellationToken::new(),
            ContextCuratorLimits {
                timeout: Duration::from_millis(10),
                ..ContextCuratorLimits::default()
            },
        )
        .await;
        assert!(matches!(
            timeout,
            Err(ContextCuratorError::TaskFailed {
                task_id,
                completed: 0,
                total: 1,
                reason,
            }) if task_id == "tool-1" && reason.contains("timed out")
        ));
        assert_eq!(provider.call_count(), 1);

        let bounded_provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(Vec::new()),
        ));
        let bounded_route = curator_route(
            bounded_provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        assert!(matches!(
            run_context_curator(
                &bounded_route,
                &messages,
                &[],
                std::slice::from_ref(&work),
                &[],
                &CancellationToken::new(),
                ContextCuratorLimits {
                    max_request_bytes: 1,
                    ..ContextCuratorLimits::default()
                },
            )
            .await,
            Err(ContextCuratorError::RequestTooLarge { .. })
        ));
        assert_eq!(bounded_provider.call_count(), 0);
    }

    #[tokio::test]
    async fn images_are_real_blocks_and_opaque_provider_state_is_not_embedded_in_json() {
        let image_data = "SECRET_IMAGE_DATA".repeat(100);
        let anthropic_signature = "SECRET_ANTHROPIC_SIGNATURE";
        let encrypted = "SECRET_ENCRYPTED_REASONING";
        let thought_signature = "SECRET_THOUGHT_SIGNATURE";
        let messages = vec![StoredMessage {
            origin: None,
            id: "image-message".to_string(),
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "inspect the image".to_string(),
                    cache_control: None,
                },
                ContentBlock::AnthropicThinking {
                    thinking: "anthropic reasoning needed for the summary".to_string(),
                    signature: anthropic_signature.to_string(),
                },
                ContentBlock::OpenAIReasoning {
                    id: "reasoning".to_string(),
                    summary: vec!["summary".to_string()],
                    encrypted_content: Some(encrypted.to_string()),
                    status: Some("completed".to_string()),
                },
                ContentBlock::ToolUse {
                    id: "call".to_string(),
                    name: "read".to_string(),
                    input: json!({"file_path": "image.png"}),
                    thought_signature: Some(thought_signature.to_string()),
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: image_data.clone(),
                },
            ],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }];
        let range = range_work(&messages);
        let provider = Arc::new(
            ScriptedProvider::new(
                "curator",
                ScriptedBehavior::Events(vec![StreamEvent::TextDelta(range_response())]),
            )
            .with_image_support(),
        );
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        let artifacts = run_context_curator(
            &route,
            &messages,
            std::slice::from_ref(&range),
            &[],
            &[],
            &CancellationToken::new(),
            ContextCuratorLimits::default(),
        )
        .await
        .expect("image-capable curator");
        assert!(artifacts.range_summaries.contains_key("range-1"));
        let calls = provider.captured_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_count, 0);
        assert_eq!(calls[0].system, range_system_prompt("", ""));
        assert!(calls[0].system.starts_with(RANGE_SUMMARIZER_BASE_PROMPT));
        let request_text = match &calls[0].messages[0].content[0] {
            ContentBlock::Text { text, .. } => text,
            other => panic!("expected JSON text, got {other:?}"),
        };
        assert!(!request_text.contains(&image_data));
        assert!(!request_text.contains(anthropic_signature));
        assert!(request_text.contains("anthropic reasoning needed for the summary"));
        assert!(request_text.contains("signature_present"));
        assert!(!request_text.contains(encrypted));
        assert!(!request_text.contains(thought_signature));
        assert!(
            calls[0].messages[0].content.iter().any(
                |block| matches!(block, ContentBlock::Image { data, .. } if data == &image_data)
            )
        );

        let unsupported = Arc::new(ScriptedProvider::new(
            "text-only-curator",
            ScriptedBehavior::Events(Vec::new()),
        ));
        let unsupported_route = curator_route(
            unsupported.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        assert!(matches!(
            run_context_curator(
                &unsupported_route,
                &messages,
                &[range],
                &[],
                &[],
                &CancellationToken::new(),
                ContextCuratorLimits::default(),
            )
            .await,
            Err(ContextCuratorError::ImagesUnsupported { count: 1, .. })
        ));
        assert_eq!(unsupported.call_count(), 0);
    }
}
