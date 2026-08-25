use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current on-disk schema for persisted provider-context projections.
pub const STORED_CONTEXT_VIEW_SCHEMA_VERSION: u32 = 1;
pub const STORED_CONTEXT_EVIDENCE_MAX_PATHS_PER_CATEGORY: usize = 256;
pub const STORED_CONTEXT_EVIDENCE_MAX_PATH_CHARS: usize = 1_024;
pub const STORED_CONTEXT_EVIDENCE_MAX_WARNINGS_PER_CATEGORY: usize = 64;
pub const STORED_CONTEXT_EVIDENCE_MAX_WARNING_CHARS: usize = 512;

fn current_context_view_schema_version() -> u32 {
    STORED_CONTEXT_VIEW_SCHEMA_VERSION
}

/// Reversible provider-facing context state derived from the authoritative transcript.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextViewState {
    #[serde(default = "current_context_view_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transactions: Vec<StoredContextTransaction>,
    #[serde(
        default,
        skip_serializing_if = "StoredContextEmergencyPolicy::is_block"
    )]
    pub emergency_policy: StoredContextEmergencyPolicy,
}

impl Default for StoredContextViewState {
    fn default() -> Self {
        Self {
            schema_version: STORED_CONTEXT_VIEW_SCHEMA_VERSION,
            revision: 0,
            transactions: Vec::new(),
            emergency_policy: StoredContextEmergencyPolicy::Block,
        }
    }
}

impl StoredContextViewState {
    pub fn is_default(&self) -> bool {
        self.schema_version == STORED_CONTEXT_VIEW_SCHEMA_VERSION
            && self.revision == 0
            && self.transactions.is_empty()
            && self.emergency_policy.is_block()
    }

    pub fn active_transactions(
        &self,
    ) -> impl DoubleEndedIterator<Item = &StoredContextTransaction> {
        self.transactions
            .iter()
            .filter(|transaction| transaction.is_active())
    }

    pub fn active_transaction_count(&self) -> usize {
        self.active_transactions().count()
    }

    pub fn latest_active_transaction(&self) -> Option<&StoredContextTransaction> {
        self.active_transactions().next_back()
    }
}

/// One atomic provider-context revision. Source transcript content is never duplicated here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextTransaction {
    pub id: String,
    pub base_revision: u64,
    pub created_at: DateTime<Utc>,
    pub authorization: StoredContextAuthorization,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<StoredContextOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_events: Vec<StoredContextStatusEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<StoredContextApplication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub economics: Option<StoredContextEconomics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curator_usage: Vec<StoredContextCuratorUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emergency_audit: Option<StoredContextEmergencyAudit>,
}

impl StoredContextTransaction {
    pub fn latest_status(&self) -> Option<&StoredContextStatusEvent> {
        self.status_events.last()
    }

    pub fn is_active(&self) -> bool {
        self.latest_status()
            .map(|event| event.kind.is_active())
            .unwrap_or(false)
    }

    pub fn operation_counts(&self) -> StoredContextOperationCounts {
        let mut counts = StoredContextOperationCounts::default();
        for operation in &self.operations {
            match operation {
                StoredContextOperation::RangeSummary(_) => counts.range_summaries += 1,
                StoredContextOperation::ReasoningSuppression(_) => {
                    counts.reasoning_suppressions += 1;
                }
                StoredContextOperation::ToolResultDistillation(_) => {
                    counts.tool_result_distillations += 1;
                }
            }
        }
        counts
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoredContextOperationCounts {
    pub range_summaries: usize,
    pub reasoning_suppressions: usize,
    pub tool_result_distillations: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredContextAuthorization {
    Manual {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiated_by: Option<String>,
    },
    UnattendedEmergency {
        authorization_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_item_id: Option<String>,
    },
    LegacyMigration {
        source: StoredLegacyContextSource,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredLegacyContextSource {
    JcodeTextCompaction,
    OpenAiNativeCompaction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextStatusEvent {
    pub revision: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: StoredContextTransactionStatusKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextTransactionStatusKind {
    Applied,
    Reverted,
    Reapplied,
    InvalidatedByTranscriptEdit,
}

impl StoredContextTransactionStatusKind {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Applied | Self::Reapplied)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "operation", rename_all = "snake_case")]
pub enum StoredContextOperation {
    RangeSummary(StoredRangeSummary),
    ReasoningSuppression(StoredReasoningSuppression),
    ToolResultDistillation(StoredToolResultDistillation),
}

/// Stable locator for one source content block. Hash verification is mandatory before use.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContentTarget {
    pub message_id: String,
    pub stored_index_hint: usize,
    pub block_ordinal_hint: usize,
    pub kind: StoredContextBlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_id: Option<String>,
    pub expected_hash: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextBlockKind {
    Text,
    Reasoning,
    ReasoningTrace,
    AnthropicThinking,
    OpenAiReasoning,
    ToolUse,
    ToolResult,
    Image,
    OpenAiCompaction,
}

/// Closed inclusive message interval over the authoritative stored transcript.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessageRange {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_index_hint: usize,
    pub end_index_hint: usize,
    pub source_digest: u64,
    pub message_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredRangeSummary {
    pub source_range: StoredMessageRange,
    pub summary_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_change_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    /// False means shell-mediated or otherwise indirect changes may be missing.
    #[serde(default)]
    pub change_evidence_complete: bool,
    /// Structured, category-specific harness evidence for summaries generated by
    /// current Jcode versions. `None` identifies legacy summaries whose evidence
    /// was limited to `changed_files` and `change_evidence_complete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_evidence: Option<StoredContextFileEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundary_expansions: Vec<StoredRangeBoundaryExpansion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<StoredContextArtifactGenerator>,
    #[serde(default)]
    pub source_token_estimate: usize,
    #[serde(default)]
    pub replacement_token_estimate: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_coverage: Option<StoredLegacyCompactionCoverage>,
}

impl StoredRangeSummary {
    /// Return a complete category model for both current and legacy summaries.
    ///
    /// Legacy summaries never collected read or searched-path evidence. Those
    /// categories therefore remain explicitly incomplete instead of silently
    /// appearing complete and empty.
    pub fn effective_file_evidence(&self) -> StoredContextFileEvidence {
        if let Some(evidence) = self.file_evidence.as_ref() {
            return evidence.clone();
        }

        let changed_warning = (!self.change_evidence_complete).then(|| {
            "Legacy summary changed-file evidence was incomplete and did not preserve a category-specific reason."
                .to_string()
        });
        StoredContextFileEvidence {
            changed: StoredContextPathEvidence {
                paths: self.changed_files.clone(),
                complete: self.change_evidence_complete,
                warnings: changed_warning.into_iter().collect(),
            },
            read_or_inspected: StoredContextPathEvidence {
                complete: false,
                warnings: vec![
                    "Legacy summary predates structured read or inspected-file evidence."
                        .to_string(),
                ],
                ..StoredContextPathEvidence::default()
            },
            searched_or_browsed: StoredContextPathEvidence {
                complete: false,
                warnings: vec![
                    "Legacy summary predates structured searched or browsed-path evidence."
                        .to_string(),
                ],
                ..StoredContextPathEvidence::default()
            },
        }
    }
}

/// Harness-generated file evidence for one selected authoritative range.
/// Categories remain separate because search and browse operations never prove
/// that a file was read, and reads never prove that a file changed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextFileEvidence {
    #[serde(default)]
    pub changed: StoredContextPathEvidence,
    #[serde(default)]
    pub read_or_inspected: StoredContextPathEvidence,
    #[serde(default)]
    pub searched_or_browsed: StoredContextPathEvidence,
}

/// One evidence category with explicit completeness and bounded explanations.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextPathEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredRangeBoundaryExpansion {
    pub message_id: String,
    pub stored_index_hint: usize,
    pub reason: StoredRangeBoundaryExpansionReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredRangeBoundaryExpansionReason {
    ToolPair {
        tool_use_id: String,
    },
    ParallelToolGroup,
    AssociatedToolImage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    ToolThoughtSignature {
        tool_use_id: String,
    },
    ExistingSummaryBoundary {
        transaction_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredLegacyCompactionCoverage {
    pub covers_up_to_turn: usize,
    pub original_turn_count: usize,
    pub compacted_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredReasoningSuppression {
    pub selection: StoredReasoningSelection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<StoredContentTarget>,
    #[serde(default)]
    pub assistant_turns_affected: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replay_block_kinds: Vec<StoredContextBlockKind>,
    #[serde(default)]
    pub original_token_estimate: usize,
    #[serde(default)]
    pub validation_evidence_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation: Vec<StoredProviderValidationEvidence>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredReasoningSelection {
    KeepLatestAssistantTurns {
        protected_recent_assistant_turns: usize,
    },
    MessageRanges {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ranges: Vec<StoredMessageRange>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredToolResultDistillation {
    pub target: StoredContentTarget,
    pub tool_name: String,
    pub tool_call_id: String,
    pub replacement_content: String,
    pub original_token_estimate: usize,
    pub replacement_token_estimate: usize,
    /// Replacement/original ratio in millionths, stored for stable review display.
    pub replacement_ratio_millionths: u32,
    pub preservation_rationale: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertainties: Vec<String>,
    pub generator: StoredContextArtifactGenerator,
    pub created_at: DateTime<Utc>,
}

impl StoredToolResultDistillation {
    pub fn calculated_replacement_ratio_millionths(&self) -> Option<u32> {
        if self.original_token_estimate == 0 {
            return None;
        }
        let ratio = (self.replacement_token_estimate as u128).saturating_mul(1_000_000)
            / self.original_token_estimate as u128;
        Some(u32::try_from(ratio).unwrap_or(u32::MAX))
    }

    /// Strict comparison. A replacement exactly at the limit is not eligible.
    pub fn is_strictly_below_percent(&self, percent: u8) -> bool {
        self.original_token_estimate > 0
            && (self.replacement_token_estimate as u128).saturating_mul(100)
                < (self.original_token_estimate as u128).saturating_mul(percent as u128)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextArtifactGenerator {
    pub provider: String,
    pub model: String,
    pub route: String,
    pub prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<StoredContextCuratorRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<StoredContextCuratorSelectionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_instructions: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextCuratorRole {
    RangeSummarizer,
    ToolResultDistiller,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextCuratorSelectionSource {
    ConfiguredDefault,
    PerRunOverride,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredProviderValidationEvidence {
    pub provider: String,
    pub model: String,
    pub request_builder: String,
    pub checked_at: DateTime<Utc>,
    pub outcome: StoredProviderValidationOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredProviderValidationOutcome {
    Passed,
    Unsupported,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextApplication {
    pub provider: String,
    pub model: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextEconomics {
    pub projected_tokens_before: usize,
    pub projected_tokens_after: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_total_request_tokens_before: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_total_request_tokens_after: Option<usize>,
    pub unchanged_prefix_items: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earliest_changed_provider_item: Option<usize>,
    pub old_affected_suffix_tokens: usize,
    pub new_affected_suffix_tokens: usize,
    pub deleted_input_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_input_budget: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<StoredContextPricingSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_request_delta_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_savings_per_turn_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_even_turns: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextPricingSnapshot {
    pub billing_mode: StoredContextBillingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_usd_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_usd_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_per_million: Option<f64>,
    /// `None` means a separately authoritative cache-write rate is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_price_tiers: Vec<StoredContextInputPriceTier>,
    #[serde(default)]
    pub cache_warmth: StoredContextCacheWarmth,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextInputPriceTier {
    /// Applies only when total request input is strictly greater than this value.
    pub above_input_tokens: usize,
    pub input_usd_per_million: f64,
    /// `None` inherits the pricing snapshot's preceding output rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_usd_per_million: Option<f64>,
    /// Cache-rate absence means unknown for this tier rather than inherited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_per_million: Option<f64>,
    /// `None` means a separately authoritative cache-write rate is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_per_million: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextBillingMode {
    Metered,
    Subscription,
    IncludedQuota,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextCacheWarmth {
    Warm,
    Cold,
    #[default]
    Unknown,
}

/// Usage from curator-only requests. This never belongs to a coding assistant message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredContextCuratorUsage {
    pub provider: String,
    pub model: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<StoredContextCuratorRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum StoredContextEmergencyPolicy {
    #[default]
    Block,
    Authorized {
        protected_recent_assistant_turns: usize,
        target_headroom_percent: u8,
        allow_reasoning_suppression: bool,
        allow_tool_distillation: bool,
        allow_oldest_range_summary: bool,
        authorization_source: String,
    },
}

impl StoredContextEmergencyPolicy {
    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block)
    }

    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized { .. })
    }
}

/// Exact authorization attached to one explicitly unattended turn.
///
/// The policy is copied rather than referenced so scheduler restart, later
/// session-policy changes, and child creation cannot silently alter the
/// authority used by an already-dispatched task.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredUnattendedContextAuthorization {
    pub policy: StoredContextEmergencyPolicy,
    pub authorization_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_item_id: Option<String>,
}

impl StoredUnattendedContextAuthorization {
    pub fn is_authorized(&self) -> bool {
        self.policy.is_authorized()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextEmergencyTriggerKind {
    PreflightLimit,
    ProviderContextLimit,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredContextEmergencyOperationKind {
    ReasoningSuppression,
    ToolResultDistillation,
    OldestRangeSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StoredContextEmergencyRetryOutcome {
    Pending,
    Succeeded,
    Blocked { required_reduction_tokens: usize },
    ProviderRejected,
    Failed { detail: String },
}

/// Bounded provenance for one emergency context transaction.
///
/// Provider errors and authorization-source text may contain sensitive source
/// material. Export redaction must scrub them, and logs/debug summaries must
/// expose only IDs, counts, booleans, and bounded outcome categories. Generated
/// summaries and replacements remain inspectable through ordinary transaction
/// operations and are deliberately not duplicated here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextEmergencyAudit {
    pub authorization_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_item_id: Option<String>,
    pub policy: StoredContextEmergencyPolicy,
    pub trigger_kind: StoredContextEmergencyTriggerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_error: Option<String>,
    pub context_window: usize,
    pub safe_input_budget: usize,
    pub projected_input_tokens: usize,
    pub required_reduction_to_fit_tokens: usize,
    pub required_reduction_to_target_tokens: usize,
    pub achieved_reduction_tokens: usize,
    pub protected_recent_assistant_turns: usize,
    pub protected_message_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_order: Vec<StoredContextEmergencyOperationKind>,
    pub retry_outcome: StoredContextEmergencyRetryOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-10T12:34:56Z")
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn target(kind: StoredContextBlockKind, ordinal: usize, hash: u64) -> StoredContentTarget {
        StoredContentTarget {
            message_id: format!("message-{ordinal}"),
            stored_index_hint: ordinal,
            block_ordinal_hint: ordinal,
            kind,
            semantic_id: Some(format!("semantic-{ordinal}")),
            expected_hash: hash,
        }
    }

    fn generator() -> StoredContextArtifactGenerator {
        StoredContextArtifactGenerator {
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            route: "test-route".to_string(),
            prompt_version: "context-range-summarizer-v2".to_string(),
            effort: Some("high".to_string()),
            role: Some(StoredContextCuratorRole::RangeSummarizer),
            selection_source: Some(StoredContextCuratorSelectionSource::PerRunOverride),
            transaction_instructions: Some("Preserve benchmark evidence.".to_string()),
            task_instructions: Some("Keep exact compiler diagnostics.".to_string()),
        }
    }

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    struct SessionEnvelope {
        #[serde(default, skip_serializing_if = "StoredContextViewState::is_default")]
        context_view: StoredContextViewState,
    }

    #[test]
    fn missing_context_state_loads_as_default_and_default_is_omitted() {
        let loaded: SessionEnvelope = serde_json::from_str("{}").expect("load old session shape");
        assert!(loaded.context_view.is_default());
        assert_eq!(serde_json::to_string(&loaded).expect("serialize"), "{}");

        let minimal: StoredContextViewState =
            serde_json::from_str("{}").expect("load minimal context state");
        assert!(minimal.is_default());
    }

    #[test]
    fn complete_context_state_round_trips_without_losing_provenance() {
        let closed_range = StoredMessageRange {
            start_message_id: "message-1".to_string(),
            end_message_id: "message-4".to_string(),
            start_index_hint: 1,
            end_index_hint: 4,
            source_digest: 0xdead_beef,
            message_count: 4,
        };
        let validation = StoredProviderValidationEvidence {
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            request_builder: "responses-v1".to_string(),
            checked_at: timestamp(),
            outcome: StoredProviderValidationOutcome::Passed,
            warnings: vec!["synthetic fixture".to_string()],
        };
        let transaction = StoredContextTransaction {
            id: "context-transaction-1".to_string(),
            base_revision: 3,
            created_at: timestamp(),
            authorization: StoredContextAuthorization::UnattendedEmergency {
                authorization_source: "scheduled_item:sched-7".to_string(),
                trigger: Some("provider_context_limit".to_string()),
                scheduled_item_id: Some("sched-7".to_string()),
            },
            operations: vec![
                StoredContextOperation::RangeSummary(StoredRangeSummary {
                    source_range: closed_range.clone(),
                    summary_text: "Exact durable summary.".to_string(),
                    file_change_digest: "Changed parser.rs.".to_string(),
                    changed_files: Vec::new(),
                    change_evidence_complete: true,
                    file_evidence: Some(StoredContextFileEvidence {
                        changed: StoredContextPathEvidence {
                            paths: vec!["src/parser.rs".to_string()],
                            complete: true,
                            warnings: Vec::new(),
                        },
                        read_or_inspected: StoredContextPathEvidence {
                            paths: vec!["src/lexer.rs".to_string()],
                            complete: true,
                            warnings: Vec::new(),
                        },
                        searched_or_browsed: StoredContextPathEvidence {
                            paths: vec!["src".to_string()],
                            complete: false,
                            warnings: vec![
                                "A shell command may have searched additional paths.".to_string(),
                            ],
                        },
                    }),
                    boundary_expansions: vec![StoredRangeBoundaryExpansion {
                        message_id: "message-4".to_string(),
                        stored_index_hint: 4,
                        reason: StoredRangeBoundaryExpansionReason::ToolPair {
                            tool_use_id: "call-4".to_string(),
                        },
                    }],
                    generator: Some(generator()),
                    source_token_estimate: 10_000,
                    replacement_token_estimate: 1_000,
                    warnings: Vec::new(),
                    created_at: timestamp(),
                    legacy_coverage: None,
                }),
                StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
                    selection: StoredReasoningSelection::KeepLatestAssistantTurns {
                        protected_recent_assistant_turns: 5,
                    },
                    targets: vec![target(
                        StoredContextBlockKind::OpenAiReasoning,
                        2,
                        0x1234_5678,
                    )],
                    assistant_turns_affected: 1,
                    replay_block_kinds: vec![StoredContextBlockKind::OpenAiReasoning],
                    original_token_estimate: 2_500,
                    validation_evidence_version: 1,
                    validation: vec![validation],
                }),
                StoredContextOperation::ToolResultDistillation(StoredToolResultDistillation {
                    target: target(StoredContextBlockKind::ToolResult, 3, 0xabcd_ef01),
                    tool_name: "bash".to_string(),
                    tool_call_id: "call-3".to_string(),
                    replacement_content: "100 tests passed; exit 0.".to_string(),
                    original_token_estimate: 5_000,
                    replacement_token_estimate: 500,
                    replacement_ratio_millionths: 100_000,
                    preservation_rationale: "Retains all observed outcomes.".to_string(),
                    uncertainties: Vec::new(),
                    generator: generator(),
                    created_at: timestamp(),
                }),
            ],
            status_events: vec![
                StoredContextStatusEvent {
                    revision: 4,
                    timestamp: timestamp(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: None,
                },
                StoredContextStatusEvent {
                    revision: 5,
                    timestamp: timestamp(),
                    kind: StoredContextTransactionStatusKind::Reverted,
                    reason: Some("user review".to_string()),
                },
                StoredContextStatusEvent {
                    revision: 6,
                    timestamp: timestamp(),
                    kind: StoredContextTransactionStatusKind::Reapplied,
                    reason: None,
                },
            ],
            application: Some(StoredContextApplication {
                provider: "test-provider".to_string(),
                model: "test-model".to_string(),
                route: "test-route".to_string(),
                context_window: Some(372_000),
            }),
            economics: Some(StoredContextEconomics {
                projected_tokens_before: 350_000,
                projected_tokens_after: 200_000,
                estimated_total_request_tokens_before: Some(368_000),
                estimated_total_request_tokens_after: Some(218_000),
                unchanged_prefix_items: 12,
                earliest_changed_provider_item: Some(12),
                old_affected_suffix_tokens: 180_000,
                new_affected_suffix_tokens: 30_000,
                deleted_input_tokens: 150_000,
                context_window: Some(372_000),
                safe_input_budget: Some(368_000),
                pricing: Some(StoredContextPricingSnapshot {
                    billing_mode: StoredContextBillingMode::Metered,
                    input_usd_per_million: Some(5.0),
                    output_usd_per_million: Some(30.0),
                    cache_read_usd_per_million: Some(0.5),
                    cache_write_usd_per_million: Some(6.25),
                    input_price_tiers: vec![StoredContextInputPriceTier {
                        above_input_tokens: 272_000,
                        input_usd_per_million: 10.0,
                        output_usd_per_million: Some(45.0),
                        cache_read_usd_per_million: Some(1.0),
                        cache_write_usd_per_million: Some(12.5),
                    }],
                    cache_warmth: StoredContextCacheWarmth::Warm,
                }),
                first_request_delta_usd: Some(0.125),
                recurring_savings_per_turn_usd: Some(0.075),
                break_even_turns: Some(2),
                assumptions: vec!["cache remains warm".to_string()],
            }),
            curator_usage: vec![StoredContextCuratorUsage {
                provider: "test-provider".to_string(),
                model: "test-model".to_string(),
                route: "test-route".to_string(),
                effort: Some("high".to_string()),
                role: Some(StoredContextCuratorRole::RangeSummarizer),
                artifact_id: Some("range-1".to_string()),
                prompt_version: Some("context-range-summarizer-v2".to_string()),
                input_tokens: 15_000,
                output_tokens: 2_000,
                cache_read_input_tokens: Some(10_000),
                cache_creation_input_tokens: Some(5_000),
                cost_usd: Some(0.25),
            }],
            emergency_audit: Some(StoredContextEmergencyAudit {
                authorization_source: "scheduled_item:sched-7".to_string(),
                scheduled_item_id: Some("sched-7".to_string()),
                policy: StoredContextEmergencyPolicy::Authorized {
                    protected_recent_assistant_turns: 5,
                    target_headroom_percent: 20,
                    allow_reasoning_suppression: true,
                    allow_tool_distillation: true,
                    allow_oldest_range_summary: true,
                    authorization_source: "schedule_tool_session:session-1".to_string(),
                },
                trigger_kind: StoredContextEmergencyTriggerKind::ProviderContextLimit,
                provider_error: Some("provider context length exceeded".to_string()),
                context_window: 372_000,
                safe_input_budget: 367_904,
                projected_input_tokens: 370_000,
                required_reduction_to_fit_tokens: 2_096,
                required_reduction_to_target_tokens: 75_683,
                achieved_reduction_tokens: 80_000,
                protected_recent_assistant_turns: 5,
                protected_message_count: 12,
                operation_order: vec![
                    StoredContextEmergencyOperationKind::ReasoningSuppression,
                    StoredContextEmergencyOperationKind::ToolResultDistillation,
                    StoredContextEmergencyOperationKind::OldestRangeSummary,
                ],
                retry_outcome: StoredContextEmergencyRetryOutcome::Succeeded,
            }),
        };
        let state = StoredContextViewState {
            schema_version: STORED_CONTEXT_VIEW_SCHEMA_VERSION,
            revision: 6,
            transactions: vec![transaction],
            emergency_policy: StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns: 5,
                target_headroom_percent: 20,
                allow_reasoning_suppression: true,
                allow_tool_distillation: true,
                allow_oldest_range_summary: true,
                authorization_source: "scheduled-item-7".to_string(),
            },
        };

        let encoded = serde_json::to_string_pretty(&state).expect("serialize state");
        let decoded: StoredContextViewState =
            serde_json::from_str(&encoded).expect("deserialize state");
        assert_eq!(decoded, state);
        assert_eq!(decoded.revision, 6);
        assert_eq!(decoded.transactions[0].operations.len(), 3);
        assert_eq!(
            decoded.transactions[0].status_events[2].kind,
            StoredContextTransactionStatusKind::Reapplied
        );
        assert_eq!(
            decoded.transactions[0].operations[1],
            state.transactions[0].operations[1]
        );
        let StoredContextOperation::RangeSummary(summary) = &decoded.transactions[0].operations[0]
        else {
            panic!("expected range summary");
        };
        let evidence = summary.effective_file_evidence();
        assert_eq!(evidence.changed.paths, ["src/parser.rs"]);
        assert_eq!(evidence.read_or_inspected.paths, ["src/lexer.rs"]);
        assert_eq!(evidence.searched_or_browsed.paths, ["src"]);
        assert!(!evidence.searched_or_browsed.complete);
        assert!(summary.changed_files.is_empty());
    }

    #[test]
    fn legacy_range_summary_defaults_to_honest_category_specific_uncertainty() {
        let summary: StoredRangeSummary = serde_json::from_value(serde_json::json!({
            "source_range": {
                "start_message_id": "message-1",
                "end_message_id": "message-2",
                "start_index_hint": 1,
                "end_index_hint": 2,
                "source_digest": 7,
                "message_count": 2
            },
            "summary_text": "Legacy summary",
            "file_change_digest": "Changed src/lib.rs",
            "changed_files": ["src/lib.rs"],
            "change_evidence_complete": true,
            "created_at": "2026-08-10T12:34:56Z"
        }))
        .expect("deserialize legacy range summary");

        assert!(summary.file_evidence.is_none());
        let evidence = summary.effective_file_evidence();
        assert_eq!(evidence.changed.paths, ["src/lib.rs"]);
        assert!(evidence.changed.complete);
        assert!(!evidence.read_or_inspected.complete);
        assert!(evidence.read_or_inspected.paths.is_empty());
        assert!(
            evidence.read_or_inspected.warnings[0]
                .contains("predates structured read or inspected-file evidence")
        );
        assert!(!evidence.searched_or_browsed.complete);
    }

    #[test]
    fn latest_status_controls_activity_without_destroying_history() {
        let mut transaction = StoredContextTransaction {
            id: "transaction".to_string(),
            base_revision: 0,
            created_at: timestamp(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: Vec::new(),
            status_events: Vec::new(),
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: None,
        };
        assert!(!transaction.is_active());

        transaction.status_events.push(StoredContextStatusEvent {
            revision: 1,
            timestamp: timestamp(),
            kind: StoredContextTransactionStatusKind::Applied,
            reason: None,
        });
        assert!(transaction.is_active());

        transaction.status_events.push(StoredContextStatusEvent {
            revision: 2,
            timestamp: timestamp(),
            kind: StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
            reason: Some("source range removed by rewind".to_string()),
        });
        assert!(!transaction.is_active());
        assert_eq!(transaction.status_events.len(), 2);
    }

    #[test]
    fn distillation_ratio_gate_is_strictly_below_twenty_percent() {
        let mut distillation = StoredToolResultDistillation {
            target: target(StoredContextBlockKind::ToolResult, 1, 9),
            tool_name: "bash".to_string(),
            tool_call_id: "call".to_string(),
            replacement_content: "replacement".to_string(),
            original_token_estimate: 100,
            replacement_token_estimate: 20,
            replacement_ratio_millionths: 200_000,
            preservation_rationale: "fixture".to_string(),
            uncertainties: Vec::new(),
            generator: generator(),
            created_at: timestamp(),
        };
        assert!(!distillation.is_strictly_below_percent(20));
        assert_eq!(
            distillation.calculated_replacement_ratio_millionths(),
            Some(200_000)
        );

        distillation.replacement_token_estimate = 19;
        assert!(distillation.is_strictly_below_percent(20));
        assert_eq!(
            distillation.calculated_replacement_ratio_millionths(),
            Some(190_000)
        );
    }
}
