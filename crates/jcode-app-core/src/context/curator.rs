use crate::config::ContextCuratorConfig;
use crate::message::{ContentBlock, Message, Role, StreamEvent};
use crate::provider::{ModelRoute, Provider, RouteSelection};
use futures::StreamExt;
use jcode_context_core::{
    ContextTargetIndex, context_token_rates_for_input_tokens, estimate_content_block_tokens,
};
use jcode_session_types::{
    StoredContentTarget, StoredContextArtifactGenerator, StoredContextBillingMode,
    StoredContextCuratorUsage, StoredContextPricingSnapshot, StoredMessage, StoredMessageRange,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const CONTEXT_CURATOR_PROMPT_VERSION: &str = "context-curator-v1";

const CURATOR_SYSTEM_PROMPT: &str = r#"You prepare artifacts for a user-authorized, reversible provider-context transaction. Return exactly one JSON object matching the supplied schema. Do not use markdown fences or commentary.

Range summaries must preserve every fact that can affect continued work: user intent and preferences, decisions and rejected alternatives, exact constraints and invariants, end-of-range implementation state, files and symbols changed, commands and observed results, failures, unresolved issues, next steps, provider/environment facts, and operationally relevant IDs, hashes, paths, versions, values, and error strings. Never claim unverified work passed. Never invent changed files. Prefer precise facts over vague prose.

Tool-result distillation is eligible only when the replacement preserves every fact that could affect continued work and is strictly below the supplied token target. Preserve exact errors and failing names, paths and line numbers, hashes, IDs, ports, versions, values, test counts, exit status, user-visible output, warnings, uncertainty, negative findings, and information relied upon later. Mark a result ineligible when this cannot be done safely. A completely unnecessary result still needs a concise explicit distilled marker.

Jcode owns all authoritative targets and hashes. Never create or modify target IDs. Never omit a requested ID, duplicate an ID, or return an unknown ID."#;

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
    pub fn generator(&self) -> StoredContextArtifactGenerator {
        StoredContextArtifactGenerator {
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            route: self.route.clone(),
            prompt_version: CONTEXT_CURATOR_PROMPT_VERSION.to_string(),
            effort: self.effort.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ContextCuratorLimits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout: Duration,
}

impl Default for ContextCuratorLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 32 * 1024 * 1024,
            max_response_bytes: 2 * 1024 * 1024,
            timeout: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ContextCuratorRangeWork {
    pub request_id: String,
    pub source_range: StoredMessageRange,
    pub changed_files: Vec<String>,
    pub change_evidence_complete: bool,
    pub change_evidence_warnings: Vec<String>,
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
    pub usage: Option<StoredContextCuratorUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCuratorError {
    Route(String),
    RequestTooLarge { bytes: usize, limit: usize },
    ImagesUnsupported { count: usize, provider: String },
    ResponseTooLarge { bytes: usize, limit: usize },
    Timeout,
    Canceled,
    Provider(String),
    UnexpectedToolUse,
    UnexpectedProviderEvent(String),
    InvalidResponse(String),
}

impl fmt::Display for ContextCuratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(reason) => write!(formatter, "context curator route is unavailable: {reason}"),
            Self::RequestTooLarge { bytes, limit } => write!(
                formatter,
                "context curator request is {bytes} bytes, exceeding the {limit}-byte bound"
            ),
            Self::ImagesUnsupported { count, provider } => write!(
                formatter,
                "context curator material contains {count} image(s), but route {provider} does not support image input"
            ),
            Self::ResponseTooLarge { bytes, limit } => write!(
                formatter,
                "context curator response reached {bytes} bytes, exceeding the {limit}-byte bound"
            ),
            Self::Timeout => formatter.write_str("context curator request timed out"),
            Self::Canceled => formatter.write_str("context curator request was canceled"),
            Self::Provider(reason) => write!(formatter, "context curator provider failed: {reason}"),
            Self::UnexpectedToolUse => formatter.write_str(
                "context curator attempted a tool call; artifact generation requires JSON text only",
            ),
            Self::UnexpectedProviderEvent(event) => write!(
                formatter,
                "context curator emitted unsupported provider event {event}; artifact generation requires JSON text only"
            ),
            Self::InvalidResponse(reason) => {
                write!(formatter, "context curator returned invalid structured output: {reason}")
            }
        }
    }
}

impl Error for ContextCuratorError {}

pub(crate) fn resolve_context_curator_route(
    provider_fork: Arc<dyn Provider>,
    model_routes: &[ModelRoute],
    active_route: &str,
    config: &ContextCuratorConfig,
) -> Result<ContextCuratorRoute, ContextCuratorError> {
    let mut route = active_route.to_string();

    if let Some(provider_selector) = config.provider.as_deref() {
        let requested_model = config
            .model
            .clone()
            .unwrap_or_else(|| provider_fork.model());
        let matches = model_routes
            .iter()
            .filter(|candidate| candidate.available && candidate.model == requested_model)
            .filter(|candidate| route_selector_matches(provider_selector, candidate))
            .collect::<Vec<_>>();
        let selected = match matches.as_slice() {
            [selected] => *selected,
            [] => {
                return Err(ContextCuratorError::Route(format!(
                    "no available route matches provider {:?} and model {:?}",
                    provider_selector, requested_model
                )));
            }
            _ => {
                let choices = matches
                    .iter()
                    .map(|candidate| format!("{} ({})", candidate.provider, candidate.api_method))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ContextCuratorError::Route(format!(
                    "provider {:?} and model {:?} are ambiguous; matching routes: {choices}",
                    provider_selector, requested_model
                )));
            }
        };
        let selection = RouteSelection::from_model_route(selected);
        provider_fork
            .set_route_selection(&selection)
            .map_err(|error| ContextCuratorError::Route(error.to_string()))?;
        route = selected.api_method.clone();
    } else if let Some(model) = config.model.as_deref() {
        provider_fork
            .set_model(model)
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
        provider: provider_fork,
        provider_name,
        provider_display_name,
        model,
        route,
        effort: config.effort.clone(),
        pricing,
    })
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

pub(crate) async fn run_context_curator(
    route: &ContextCuratorRoute,
    messages: &[StoredMessage],
    ranges: &[ContextCuratorRangeWork],
    tools: &[ContextCuratorToolWork],
    active_summary_texts: &[String],
    cancellation: &CancellationToken,
    limits: ContextCuratorLimits,
) -> Result<ContextCuratorArtifacts, ContextCuratorError> {
    if cancellation.is_cancelled() {
        return Err(ContextCuratorError::Canceled);
    }
    if ranges.is_empty() && tools.is_empty() {
        return Ok(ContextCuratorArtifacts::default());
    }
    let request = build_curator_request(messages, ranges, tools, active_summary_texts)?;
    let request_json = serde_json::to_string(&request)
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    let request_bytes = request_json.len().saturating_add(
        request
            .images
            .iter()
            .map(|image| image.data.len())
            .fold(0usize, usize::saturating_add),
    );
    if request_bytes > limits.max_request_bytes {
        return Err(ContextCuratorError::RequestTooLarge {
            bytes: request_bytes,
            limit: limits.max_request_bytes,
        });
    }
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

    let call = collect_curator_response(
        route.provider.as_ref(),
        &provider_messages,
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

    parse_curator_response(&collected.text, ranges, tools).map(|mut artifacts| {
        artifacts.usage = build_curator_usage(route, collected.usage);
        artifacts
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
    max_response_bytes: usize,
) -> Result<CollectedCuratorResponse, ContextCuratorError> {
    let response = provider
        .complete(messages, &[], CURATOR_SYSTEM_PROMPT, None)
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
            StreamEvent::Compaction { .. } => {
                return Err(ContextCuratorError::UnexpectedProviderEvent(
                    "native_compaction".to_string(),
                ));
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
    /// `input_tokens` excludes the separately reported cache-read and cache-write tokens.
    Split,
    /// `input_tokens` includes the separately reported cache-read and cache-write subsets.
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

#[derive(Serialize)]
struct CuratorRequestPayload {
    contract_version: &'static str,
    response_schema: Value,
    range_requests: Vec<CuratorRangeRequestPayload>,
    tool_distillation_requests: Vec<CuratorToolRequestPayload>,
    conversation_messages: Vec<CuratorMessagePayload>,
    active_summary_texts: Vec<String>,
    #[serde(skip)]
    images: Vec<CuratorImageAttachment>,
}

#[derive(Serialize)]
struct CuratorRangeRequestPayload {
    request_id: String,
    start_message_id: String,
    end_message_id: String,
    message_count: usize,
    changed_files: Vec<String>,
    change_evidence_complete: bool,
    change_evidence_warnings: Vec<String>,
}

#[derive(Serialize)]
struct CuratorToolRequestPayload {
    request_id: String,
    tool_name: String,
    tool_call_id: String,
    result_message_id: String,
    result_block_ordinal: usize,
    tool_input: Value,
    original_token_estimate: usize,
    replacement_must_be_below_tokens: usize,
}

#[derive(Serialize)]
struct CuratorMessagePayload {
    message_id: String,
    stored_index: usize,
    role: Role,
    blocks: Vec<Value>,
}

struct CuratorImageAttachment {
    image_ref: String,
    media_type: String,
    data: String,
}

fn build_curator_request(
    messages: &[StoredMessage],
    ranges: &[ContextCuratorRangeWork],
    tools: &[ContextCuratorToolWork],
    active_summary_texts: &[String],
) -> Result<CuratorRequestPayload, ContextCuratorError> {
    let target_index = ContextTargetIndex::new(messages);
    let mut included_indices = BTreeSet::new();
    for range in ranges {
        let (start, end) = target_index
            .resolve_message_range(&range.source_range)
            .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
        included_indices.extend(start..=end);
        if start > 0 {
            included_indices.insert(start - 1);
        }
        if end + 1 < messages.len() {
            included_indices.insert(end + 1);
        }
    }
    for tool in tools {
        included_indices.insert(tool.message_index);
        included_indices.extend(tool.message_index.saturating_sub(2)..messages.len());
        if let Some(call_index) = find_tool_call_message(messages, &tool.tool_call_id) {
            included_indices.insert(call_index);
        }
    }

    let mut images = Vec::new();
    let conversation_messages = included_indices
        .into_iter()
        .filter_map(|index| messages.get(index).map(|message| (index, message)))
        .map(|(stored_index, message)| CuratorMessagePayload {
            message_id: message.id.clone(),
            stored_index,
            role: message.role.clone(),
            blocks: message
                .content
                .iter()
                .enumerate()
                .map(|(block_index, block)| {
                    curator_block_payload(message, block_index, block, &mut images)
                })
                .collect(),
        })
        .collect();

    let response_schema = json!({
        "range_summaries": [{
            "request_id": "range request ID",
            "summary": "non-empty information-preserving summary",
            "file_change_digest": "evidence-based digest; may be empty when no files changed",
            "warnings": ["uncertainty or preservation warning"]
        }],
        "tool_distillations": [{
            "request_id": "tool request ID",
            "eligible": true,
            "replacement": "required and non-empty only when eligible",
            "preservation_rationale": "required and non-empty only when eligible",
            "ineligible_reason": "required and non-empty only when ineligible",
            "uncertainties": ["uncertainty"]
        }]
    });
    Ok(CuratorRequestPayload {
        contract_version: CONTEXT_CURATOR_PROMPT_VERSION,
        response_schema,
        range_requests: ranges
            .iter()
            .map(|range| CuratorRangeRequestPayload {
                request_id: range.request_id.clone(),
                start_message_id: range.source_range.start_message_id.clone(),
                end_message_id: range.source_range.end_message_id.clone(),
                message_count: range.source_range.message_count,
                changed_files: range.changed_files.clone(),
                change_evidence_complete: range.change_evidence_complete,
                change_evidence_warnings: range.change_evidence_warnings.clone(),
            })
            .collect(),
        tool_distillation_requests: tools
            .iter()
            .map(|tool| CuratorToolRequestPayload {
                request_id: tool.request_id.clone(),
                tool_name: tool.tool_name.clone(),
                tool_call_id: tool.tool_call_id.clone(),
                result_message_id: tool.target.message_id.clone(),
                result_block_ordinal: tool.target.block_ordinal_hint,
                tool_input: tool.tool_input.clone(),
                original_token_estimate: tool.original_token_estimate,
                replacement_must_be_below_tokens: tool
                    .original_token_estimate
                    .saturating_mul(20)
                    .saturating_sub(1)
                    / 100,
            })
            .collect(),
        conversation_messages,
        active_summary_texts: active_summary_texts.to_vec(),
        images,
    })
}

fn curator_block_payload(
    message: &StoredMessage,
    block_index: usize,
    block: &ContentBlock,
    images: &mut Vec<CuratorImageAttachment>,
) -> Value {
    match block {
        ContentBlock::Image { media_type, data } => {
            let image_ref = format!("{}-image-{block_index}", message.id);
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
        other => {
            serde_json::to_value(other).unwrap_or_else(|_| json!({"kind": "unserializable_block"}))
        }
    }
}

fn find_tool_call_message(messages: &[StoredMessage], tool_call_id: &str) -> Option<usize> {
    messages.iter().position(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == tool_call_id))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratorResponsePayload {
    range_summaries: Vec<CuratorRangeResponse>,
    tool_distillations: Vec<CuratorToolResponse>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratorRangeResponse {
    request_id: String,
    summary: String,
    file_change_digest: String,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CuratorToolResponse {
    request_id: String,
    eligible: bool,
    replacement: Option<String>,
    preservation_rationale: Option<String>,
    ineligible_reason: Option<String>,
    uncertainties: Vec<String>,
}

fn parse_curator_response(
    raw: &str,
    ranges: &[ContextCuratorRangeWork],
    tools: &[ContextCuratorToolWork],
) -> Result<ContextCuratorArtifacts, ContextCuratorError> {
    let response: CuratorResponsePayload = serde_json::from_str(raw.trim())
        .map_err(|error| ContextCuratorError::InvalidResponse(error.to_string()))?;
    let expected_ranges = ranges
        .iter()
        .map(|range| range.request_id.as_str())
        .collect::<BTreeSet<_>>();
    let expected_tools = tools
        .iter()
        .map(|tool| tool.request_id.as_str())
        .collect::<BTreeSet<_>>();
    let tool_by_id = tools
        .iter()
        .map(|tool| (tool.request_id.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    let mut artifacts = ContextCuratorArtifacts::default();

    for range in response.range_summaries {
        if !expected_ranges.contains(range.request_id.as_str()) {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "unknown range request ID {:?}",
                range.request_id
            )));
        }
        if artifacts.range_summaries.contains_key(&range.request_id) {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "duplicate range request ID {:?}",
                range.request_id
            )));
        }
        if range.summary.trim().is_empty() {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "range {:?} has an empty summary",
                range.request_id
            )));
        }
        artifacts.range_summaries.insert(
            range.request_id,
            ContextCuratorRangeArtifact {
                summary: range.summary,
                file_change_digest: range.file_change_digest,
                warnings: range.warnings,
            },
        );
    }
    if artifacts.range_summaries.len() != expected_ranges.len() {
        let missing = expected_ranges
            .into_iter()
            .filter(|id| !artifacts.range_summaries.contains_key(*id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ContextCuratorError::InvalidResponse(format!(
            "missing range summary request IDs: {missing}"
        )));
    }

    for tool in response.tool_distillations {
        if !expected_tools.contains(tool.request_id.as_str()) {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "unknown tool request ID {:?}",
                tool.request_id
            )));
        }
        if artifacts.tool_distillations.contains_key(&tool.request_id) {
            return Err(ContextCuratorError::InvalidResponse(format!(
                "duplicate tool request ID {:?}",
                tool.request_id
            )));
        }
        let work = tool_by_id[tool.request_id.as_str()];
        let artifact = if tool.eligible {
            let replacement = tool
                .replacement
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ContextCuratorError::InvalidResponse(format!(
                        "eligible tool request {:?} has no replacement",
                        tool.request_id
                    ))
                })?;
            let rationale = tool
                .preservation_rationale
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ContextCuratorError::InvalidResponse(format!(
                        "eligible tool request {:?} has no preservation rationale",
                        tool.request_id
                    ))
                })?;
            if tool
                .ineligible_reason
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(ContextCuratorError::InvalidResponse(format!(
                    "eligible tool request {:?} also supplied an ineligible reason",
                    tool.request_id
                )));
            }
            let replacement_block = replacement_tool_result_block(work, replacement.clone());
            let replacement_token_estimate = estimate_content_block_tokens(&replacement_block);
            if work.original_token_estimate == 0
                || (replacement_token_estimate as u128).saturating_mul(100)
                    >= (work.original_token_estimate as u128).saturating_mul(20)
            {
                return Err(ContextCuratorError::InvalidResponse(format!(
                    "eligible tool request {:?} replacement is not strictly below 20 percent ({} of {} estimated tokens)",
                    tool.request_id, replacement_token_estimate, work.original_token_estimate
                )));
            }
            ContextCuratorToolArtifact::Eligible {
                replacement,
                replacement_token_estimate,
                preservation_rationale: rationale,
                uncertainties: tool.uncertainties,
            }
        } else {
            if tool
                .replacement
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || tool
                    .preservation_rationale
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(ContextCuratorError::InvalidResponse(format!(
                    "ineligible tool request {:?} supplied eligible-only fields",
                    tool.request_id
                )));
            }
            let reason = tool
                .ineligible_reason
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ContextCuratorError::InvalidResponse(format!(
                        "ineligible tool request {:?} has no reason",
                        tool.request_id
                    ))
                })?;
            ContextCuratorToolArtifact::Ineligible {
                reason,
                uncertainties: tool.uncertainties,
            }
        };
        artifacts
            .tool_distillations
            .insert(tool.request_id, artifact);
    }
    if artifacts.tool_distillations.len() != expected_tools.len() {
        let missing = expected_tools
            .into_iter()
            .filter(|id| !artifacts.tool_distillations.contains_key(*id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ContextCuratorError::InvalidResponse(format!(
            "missing tool distillation request IDs: {missing}"
        )));
    }
    Ok(artifacts)
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
                    model: Some("target-model".to_string()),
                    effort: None,
                },
            )
            .expect_err("unsafe selector must fail");
            assert!(error.to_string().contains("no available route matches"));
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

    fn range_work(messages: &[StoredMessage]) -> ContextCuratorRangeWork {
        ContextCuratorRangeWork {
            request_id: "range-1".to_string(),
            source_range: build_message_range(messages, 0, messages.len() - 1).expect("range"),
            changed_files: vec!["src/lib.rs".to_string()],
            change_evidence_complete: true,
            change_evidence_warnings: Vec::new(),
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
            "range_summaries": [],
            "tool_distillations": [{
                "request_id": "tool-1",
                "eligible": false,
                "replacement": null,
                "preservation_rationale": null,
                "ineligible_reason": "safe reduction is unavailable",
                "uncertainties": ["later references may depend on the full output"]
            }]
        }))
        .expect("response JSON")
    }

    fn range_response() -> String {
        serde_json::to_string(&json!({
            "range_summaries": [{
                "request_id": "range-1",
                "summary": "All operationally relevant range facts are preserved.",
                "file_change_digest": "Updated src/lib.rs.",
                "warnings": []
            }],
            "tool_distillations": []
        }))
        .expect("response JSON")
    }

    #[test]
    fn parser_rejects_missing_duplicate_unknown_and_malformed_tool_ids() {
        let messages = message_with_result(&"x".repeat(1_000));
        let work = tool_work(&messages, 1_000);
        for raw in [
            r#"{"range_summaries":[],"tool_distillations":[]}"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"unknown","eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}]}"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]},{"request_id":"tool-1","eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}]}"#,
            "not json",
        ] {
            assert!(parse_curator_response(raw, &[], std::slice::from_ref(&work)).is_err());
        }
    }

    #[test]
    fn parser_rejects_missing_duplicate_unknown_and_empty_range_artifacts() {
        let messages = message_with_result("context");
        let range = range_work(&messages);
        for raw in [
            r#"{"range_summaries":[],"tool_distillations":[]}"#,
            r#"{"range_summaries":[{"request_id":"unknown","summary":"summary","file_change_digest":"","warnings":[]}],"tool_distillations":[]}"#,
            r#"{"range_summaries":[{"request_id":"range-1","summary":"summary","file_change_digest":"","warnings":[]},{"request_id":"range-1","summary":"summary","file_change_digest":"","warnings":[]}],"tool_distillations":[]}"#,
            r#"{"range_summaries":[{"request_id":"range-1","summary":"   ","file_change_digest":"","warnings":[]}],"tool_distillations":[]}"#,
        ] {
            assert!(parse_curator_response(raw, std::slice::from_ref(&range), &[]).is_err());
        }
    }

    #[test]
    fn parser_rejects_inconsistent_tool_fields_unknown_fields_and_non_document_json() {
        let messages = message_with_result(&"x".repeat(4_000));
        let work = tool_work(&messages, 4_000);
        for raw in [
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":true,"replacement":"short","preservation_rationale":null,"ineligible_reason":null,"uncertainties":[]}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":true,"replacement":"short","preservation_rationale":"complete","ineligible_reason":"contradiction","uncertainties":[]}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":false,"replacement":"short","preservation_rationale":null,"ineligible_reason":"no","uncertainties":[]}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":false,"replacement":null,"preservation_rationale":"complete","ineligible_reason":"no","uncertainties":[]}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":null,"uncertainties":[]}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[{"request_id":"tool-1","eligible":false,"replacement":null,"preservation_rationale":null,"ineligible_reason":"no","uncertainties":[],"extra":true}] }"#,
            r#"{"range_summaries":[],"tool_distillations":[],"extra":true}"#,
            r#"```json
{"range_summaries":[],"tool_distillations":[]}
```"#,
            r#"{"range_summaries":[],"tool_distillations":[]} trailing"#,
        ] {
            assert!(parse_curator_response(raw, &[], std::slice::from_ref(&work)).is_err());
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
            "range_summaries": [],
            "tool_distillations": [{
                "request_id": "tool-1",
                "eligible": true,
                "replacement": replacement,
                "preservation_rationale": "complete",
                "ineligible_reason": null,
                "uncertainties": []
            }]
        }))
        .expect("json");
        assert!(parse_curator_response(&raw, &[], &[exact]).is_err());

        let below = tool_work(&messages, exact_original + 1);
        let artifacts = parse_curator_response(&raw, &[], &[below]).expect("below 20 percent");
        assert!(matches!(
            artifacts.tool_distillations.get("tool-1"),
            Some(ContextCuratorToolArtifact::Eligible { .. })
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
            "range_summaries": [],
            "tool_distillations": [{
                "request_id": "tool-1",
                "eligible": true,
                "replacement": replacement,
                "preservation_rationale": "the complete error remains actionable",
                "ineligible_reason": null,
                "uncertainties": []
            }]
        }))
        .expect("error-result response");
        work.original_token_estimate = replacement_tokens.saturating_mul(5);
        assert!(
            parse_curator_response(&raw, &[], std::slice::from_ref(&work)).is_err(),
            "an error-result replacement at exactly 20 percent must be rejected"
        );

        work.original_token_estimate = work.original_token_estimate.saturating_add(1);
        let artifacts = parse_curator_response(&raw, &[], &[work])
            .expect("error-result replacement strictly below 20 percent");
        assert!(matches!(
            artifacts.tool_distillations.get("tool-1"),
            Some(ContextCuratorToolArtifact::Eligible {
                replacement_token_estimate,
                ..
            }) if *replacement_token_estimate == replacement_tokens
        ));
    }

    #[test]
    fn parser_retains_ineligible_candidates_with_reason_and_uncertainty() {
        let messages = message_with_result(&"x".repeat(1_000));
        let work = tool_work(&messages, 1_000);
        let artifacts = parse_curator_response(
            &ineligible_tool_response(),
            &[],
            std::slice::from_ref(&work),
        )
        .expect("ineligible artifact");
        assert_eq!(
            artifacts.tool_distillations.get("tool-1"),
            Some(&ContextCuratorToolArtifact::Ineligible {
                reason: "safe reduction is unavailable".to_string(),
                uncertainties: vec!["later references may depend on the full output".to_string()],
            })
        );
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
        assert!(build_curator_usage(&route, CuratorUsageAccumulator::default()).is_none());
        assert!(
            build_curator_usage(
                &route,
                CuratorUsageAccumulator {
                    input_tokens: Some(100),
                    ..CuratorUsageAccumulator::default()
                }
            )
            .is_none()
        );
        let usage = build_curator_usage(
            &route,
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
    async fn response_collection_rejects_every_tool_and_native_output_shape() {
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
                collect_curator_response(&provider, &[], 1024).await,
                Err(ContextCuratorError::UnexpectedToolUse)
            ));
        }

        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(vec![StreamEvent::Compaction {
                trigger: "native".to_string(),
                pre_tokens: Some(1),
                openai_encrypted_content: Some("opaque".to_string()),
            }]),
        );
        assert!(matches!(
            collect_curator_response(&provider, &[], 1024).await,
            Err(ContextCuratorError::UnexpectedProviderEvent(event)) if event == "native_compaction"
        ));
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
            collect_curator_response(&provider, &[], 1024).await,
            Err(ContextCuratorError::Provider(message)) if message == "provider event failed"
        ));

        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::CompleteError("provider completion failed".to_string()),
        );
        assert!(matches!(
            collect_curator_response(&provider, &[], 1024).await,
            Err(ContextCuratorError::Provider(message)) if message.contains("provider completion failed")
        ));

        let provider = ScriptedProvider::new(
            "curator",
            ScriptedBehavior::Events(vec![StreamEvent::TextDelta("12345".to_string())]),
        );
        assert!(matches!(
            collect_curator_response(&provider, &[], 4).await,
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
        let collected = collect_curator_response(&provider, &[], 16 * 1024)
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
        let messages = message_with_result(&"x".repeat(1_000));
        let work = tool_work(&messages, 1_000);
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
        let messages = message_with_result(&"x".repeat(1_000));
        let work = tool_work(&messages, 1_000);
        let provider = Arc::new(ScriptedProvider::new(
            "curator",
            ScriptedBehavior::CompletePending,
        ));
        let route = curator_route(
            provider.clone(),
            pricing(StoredContextBillingMode::Unknown, None, None, None, None),
        );
        assert!(matches!(
            run_context_curator(
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
            .await,
            Err(ContextCuratorError::Timeout)
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
        assert_eq!(calls[0].system, CURATOR_SYSTEM_PROMPT);
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
