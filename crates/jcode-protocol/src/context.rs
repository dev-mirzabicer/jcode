use chrono::{DateTime, Utc};
use jcode_message_types::Role;
use jcode_provider_core::ContextProjectionValidationReport;
use jcode_session_types::{
    StoredContextApplication, StoredContextAuthorization, StoredContextBlockKind,
    StoredContextCuratorUsage, StoredContextEconomics, StoredContextEmergencyPolicy,
    StoredContextOperation, StoredContextTransaction, StoredContextTransactionStatusKind,
    StoredDisplayRole, StoredMessageRange, StoredRangeBoundaryExpansion,
    StoredToolResultDistillation,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

pub const CONTEXT_SNAPSHOT_DEFAULT_PAGE_SIZE: usize = 500;
pub const CONTEXT_SNAPSHOT_MAX_PAGE_SIZE: usize = 1_000;
pub const CONTEXT_MESSAGE_DETAIL_DEFAULT_MAX_CHARS: usize = 16 * 1024;
pub const CONTEXT_MESSAGE_DETAIL_MAX_CHARS: usize = 64 * 1024;
pub const CONTEXT_HISTORY_DEFAULT_LIMIT: usize = 100;
pub const CONTEXT_HISTORY_MAX_LIMIT: usize = 256;
pub const CONTEXT_MAX_SUMMARY_RANGES: usize = 256;
pub const CONTEXT_MAX_TOOL_RESULT_SELECTIONS: usize = 1_024;
pub const CONTEXT_MAX_DISTILLATION_SELECTIONS: usize = 1_024;
pub const CONTEXT_IDENTIFIER_MAX_CHARS: usize = 512;
pub const CONTEXT_PROTOCOL_MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextOperationBadgeKind {
    RangeSummary,
    ReasoningSuppression,
    ToolResultDistillation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextOperationBadge {
    pub transaction_id: String,
    pub operation_index: usize,
    pub kind: ContextOperationBadgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSummaryCoverage {
    pub transaction_id: String,
    pub operation_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEditorBlock {
    pub ordinal: usize,
    pub kind: StoredContextBlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_id: Option<String>,
    pub estimated_provider_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_result_is_error: bool,
    #[serde(default)]
    pub has_image_payload: bool,
    #[serde(default)]
    pub has_tool_thought_signature: bool,
    #[serde(default)]
    pub provider_removable_reasoning: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_operations: Vec<ContextOperationBadge>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextEditorMessage {
    pub message_id: String,
    pub stored_index: usize,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_role: Option<StoredDisplayRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub raw_provider_tokens: usize,
    pub projected_provider_tokens: usize,
    pub preview: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<ContextEditorBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_group_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_coverage: Option<ContextSummaryCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_operations: Vec<ContextOperationBadge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removable_reasoning_kinds: Vec<StoredContextBlockKind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextEditorSnapshot {
    pub session_id: String,
    pub context_revision: u64,
    pub raw_message_count: usize,
    pub transcript_digest: u64,
    pub processing: bool,
    pub provider_name: String,
    pub provider_display_name: String,
    pub model: String,
    pub route: String,
    pub context_window: usize,
    pub projected_request_tokens: usize,
    #[serde(default)]
    pub message_page_start: usize,
    #[serde(default)]
    pub message_page_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_message_page_start: Option<usize>,
    pub messages: Vec<ContextEditorMessage>,
    pub active_transactions: Vec<ContextTransactionSummary>,
    pub emergency_policy: StoredContextEmergencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_route: Option<ContextCuratorRoutePreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCuratorRoutePreview {
    pub provider_name: String,
    pub provider_display_name: String,
    pub model: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMessageDetailFormat {
    Text,
    Json,
    MetadataOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTextChunk {
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_char: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextMessageDetail {
    pub session_id: String,
    pub context_revision: u64,
    pub transcript_digest: u64,
    pub message_id: String,
    pub stored_index: usize,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_role: Option<StoredDisplayRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub block_ordinal: usize,
    pub block_kind: StoredContextBlockKind,
    pub format: ContextMessageDetailFormat,
    pub content: ContextTextChunk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_encoded_bytes: Option<usize>,
    #[serde(default)]
    pub opaque_signature_present: bool,
    #[serde(default)]
    pub encrypted_state_present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMessageRangeSelection {
    pub start_message_id: String,
    pub end_message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextClosedRangePreview {
    pub requested: ContextMessageRangeSelection,
    pub source_range: StoredMessageRange,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundary_expansions: Vec<StoredRangeBoundaryExpansion>,
    pub source_tokens: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRangeClosurePreview {
    pub session_id: String,
    pub context_revision: u64,
    pub transcript_digest: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<ContextClosedRangePreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadowed_active_operations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextReasoningSelectionRequest {
    KeepLatestAssistantTurns {
        protected_recent_assistant_turns: usize,
    },
    MessageRanges {
        ranges: Vec<ContextMessageRangeSelection>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextToolResultSelection {
    pub message_id: String,
    pub block_ordinal: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDraftRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_ranges: Vec<ContextMessageRangeSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ContextReasoningSelectionRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ContextToolResultSelection>,
    #[serde(default)]
    pub allow_shadowing_active_operations: bool,
    pub authorization: StoredContextAuthorization,
}

impl ContextDraftRequest {
    pub fn is_empty(&self) -> bool {
        self.summary_ranges.is_empty() && self.reasoning.is_none() && self.tool_results.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDraftPhase {
    Capturing,
    ClosingRanges,
    ExtractingChangeEvidence,
    PreparingArtifacts,
    ValidatingProjection,
    CalculatingEconomics,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDraftProgress {
    pub phase: ContextDraftPhase,
    pub completed_items: usize,
    pub total_items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDraftIdentity {
    pub draft_id: String,
    pub session_id: String,
    pub base_context_revision: u64,
    pub raw_message_count: usize,
    pub transcript_digest: u64,
    pub provider_name: String,
    pub model: String,
    pub route: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextDraftPreview {
    pub raw_stored_message_count: usize,
    pub current_context_revision: u64,
    pub proposed_context_revision: u64,
    pub economics: StoredContextEconomics,
    pub validation: ContextProjectionValidationReport,
    pub formatter_placeholder_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_previews: Vec<ContextOperationPreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextDraftSelectionPreview {
    pub draft_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_distillation_ids: Vec<String>,
    pub preview: ContextDraftPreview,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextOperationPreview {
    RangeSummary {
        request_id: String,
        source_range: StoredMessageRange,
        source_tokens: usize,
        replacement_tokens: usize,
        changed_files: Vec<String>,
        change_evidence_complete: bool,
    },
    ReasoningSuppression {
        target_count: usize,
        assistant_turns_affected: usize,
        replay_block_kinds: Vec<StoredContextBlockKind>,
        removed_tokens: usize,
    },
    ToolResultDistillation {
        proposal_id: String,
        tool_name: String,
        tool_call_id: String,
        original_tokens: usize,
        replacement_tokens: usize,
        selected_by_default: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextDistillationProposal {
    pub proposal_id: String,
    pub selected_by_default: bool,
    pub operation: StoredToolResultDistillation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextIneligibleDistillation {
    pub request_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertainties: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextDraft {
    pub identity: ContextDraftIdentity,
    pub authorization: StoredContextAuthorization,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_operations: Vec<StoredContextOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distillation_proposals: Vec<ContextDistillationProposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ineligible_distillations: Vec<ContextIneligibleDistillation>,
    pub preview: ContextDraftPreview,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curator_usage: Vec<StoredContextCuratorUsage>,
}

impl ContextDraft {
    pub fn default_selected_distillation_ids(&self) -> Vec<String> {
        self.distillation_proposals
            .iter()
            .filter(|proposal| proposal.selected_by_default)
            .map(|proposal| proposal.proposal_id.clone())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ContextDraftStatus {
    Preparing {
        identity: ContextDraftIdentity,
        progress: ContextDraftProgress,
    },
    Ready {
        draft: Box<ContextDraft>,
    },
    Applying {
        identity: ContextDraftIdentity,
    },
    Applied {
        identity: ContextDraftIdentity,
        transaction_id: String,
        revision: u64,
    },
    Failed {
        identity: ContextDraftIdentity,
        error: ContextServiceError,
    },
    Canceled {
        identity: ContextDraftIdentity,
    },
    Expired {
        identity: ContextDraftIdentity,
    },
}

impl ContextDraftStatus {
    pub fn identity(&self) -> &ContextDraftIdentity {
        match self {
            Self::Preparing { identity, .. }
            | Self::Applying { identity }
            | Self::Applied { identity, .. }
            | Self::Failed { identity, .. }
            | Self::Canceled { identity }
            | Self::Expired { identity } => identity,
            Self::Ready { draft } => &draft.identity,
        }
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Preparing { .. } | Self::Applying { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ContextServiceError {
    SessionBusy,
    EmptyRequest,
    DraftNotFound(String),
    DraftNotReady(String),
    DraftAlreadyApplied(String),
    DraftExpired(String),
    DraftCanceled(String),
    TransactionNotFound(String),
    TransactionNotActive(String),
    TransactionAlreadyActive(String),
    Capacity(String),
    InvalidSelection(String),
    Conflict(String),
    Stale(String),
    Curator(String),
    Projection(String),
    ProviderValidation(String),
    Persistence(String),
    RevisionOverflow,
    Runtime(String),
}

impl fmt::Display for ContextServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionBusy => {
                formatter.write_str("session is processing or its Agent lock is busy")
            }
            Self::EmptyRequest => formatter.write_str("context draft contains no operations"),
            Self::DraftNotFound(id) => write!(formatter, "context draft not found: {id}"),
            Self::DraftNotReady(id) => write!(formatter, "context draft is not ready: {id}"),
            Self::DraftAlreadyApplied(id) => {
                write!(formatter, "context draft was already applied: {id}")
            }
            Self::DraftExpired(id) => write!(formatter, "context draft expired: {id}"),
            Self::DraftCanceled(id) => write!(formatter, "context draft was canceled: {id}"),
            Self::TransactionNotFound(id) => {
                write!(formatter, "context transaction not found: {id}")
            }
            Self::TransactionNotActive(id) => {
                write!(formatter, "context transaction is not active: {id}")
            }
            Self::TransactionAlreadyActive(id) => {
                write!(formatter, "context transaction is already active: {id}")
            }
            Self::Capacity(reason) => write!(formatter, "context draft store is full: {reason}"),
            Self::InvalidSelection(reason) => {
                write!(formatter, "invalid context selection: {reason}")
            }
            Self::Conflict(reason) => write!(formatter, "context operation conflict: {reason}"),
            Self::Stale(reason) => write!(formatter, "context draft is stale: {reason}"),
            Self::Curator(reason) => write!(formatter, "context curator failed: {reason}"),
            Self::Projection(reason) => write!(formatter, "context projection failed: {reason}"),
            Self::ProviderValidation(reason) => {
                write!(formatter, "provider validation failed: {reason}")
            }
            Self::Persistence(reason) => write!(formatter, "context persistence failed: {reason}"),
            Self::RevisionOverflow => formatter.write_str("context revision overflow"),
            Self::Runtime(reason) => write!(formatter, "context service runtime error: {reason}"),
        }
    }
}

impl Error for ContextServiceError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextOperationCounts {
    pub range_summaries: usize,
    pub reasoning_suppressions: usize,
    pub tool_result_distillations: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextTransactionSummary {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub base_revision: u64,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_status: Option<StoredContextTransactionStatusKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_status_revision: Option<u64>,
    pub authorization: StoredContextAuthorization,
    pub operation_counts: ContextOperationCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<StoredContextApplication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub economics: Option<StoredContextEconomics>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextTransactionResult {
    pub transaction: ContextTransactionSummary,
    pub revision: u64,
    pub status: StoredContextTransactionStatusKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextTransactionDetail {
    pub session_id: String,
    pub context_revision: u64,
    pub transaction: StoredContextTransaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRequestKind {
    Snapshot,
    MessageDetail,
    RangeClosurePreview,
    PrepareDraft,
    CancelDraft,
    DraftStatus,
    DraftSelectionPreview,
    ApplyDraft,
    TransactionHistory,
    TransactionDetail,
    RevertTransaction,
    ReapplyTransaction,
    SetEmergencyPolicy,
    LegacyCompact,
    LegacySetCompactionMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextActionRequiredReason {
    PreflightLimit,
    ProviderContextLimit,
    PayloadTooLarge,
}

pub const CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE: &str =
    "Partial provider output remains visible in memory but could not be persisted durably.";
pub const CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE: &str = "Provider output began, but no structurally complete partial response was available to persist.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPendingInputMetadata {
    pub request_id: u64,
    pub content_chars: usize,
    pub content_digest: u64,
    #[serde(default)]
    pub content_sha256: String,
    pub image_count: usize,
}

impl ContextPendingInputMetadata {
    pub fn new(request_id: u64, content: &str, image_count: usize) -> Self {
        Self {
            request_id,
            content_chars: content.chars().count(),
            content_digest: pending_input_digest(content),
            content_sha256: pending_input_sha256(content),
            image_count,
        }
    }

    pub fn matches(&self, request_id: u64, content: &str, image_count: usize) -> bool {
        self.request_id == request_id
            && self.content_chars == content.chars().count()
            && self.content_digest == pending_input_digest(content)
            && !self.content_sha256.is_empty()
            && self.content_sha256 == pending_input_sha256(content)
            && self.image_count == image_count
    }
}

/// Compact compatibility digest. Exact restoration additionally requires the
/// SHA-256 fingerprint below. Raw pending input is never sent or logged.
pub fn pending_input_digest(content: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    content.as_bytes().iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

fn pending_input_sha256(content: &str) -> String {
    use sha2::{Digest, Sha256};

    format!("{:x}", Sha256::digest(content.as_bytes()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPressureLevel {
    Normal,
    Notice,
    Urgent,
    Blocked,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequestTokenBreakdown {
    pub system_tokens: usize,
    pub tool_definition_tokens: usize,
    pub historical_message_tokens: usize,
    pub pending_input_tokens: usize,
    pub memory_tokens: usize,
}

impl ContextRequestTokenBreakdown {
    pub fn projected_input_tokens(&self) -> usize {
        self.system_tokens
            .saturating_add(self.tool_definition_tokens)
            .saturating_add(self.historical_message_tokens)
            .saturating_add(self.pending_input_tokens)
            .saturating_add(self.memory_tokens)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPreflightReport {
    pub context_revision: u64,
    pub pressure: ContextPressureLevel,
    pub context_window: usize,
    pub safe_input_budget: usize,
    pub projected_input_tokens: usize,
    pub required_reduction_tokens: usize,
    pub remaining_context_tokens: usize,
    pub remaining_safe_input_tokens: usize,
    pub semantics: jcode_provider_core::ContextWindowSemantics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_max_output_tokens: Option<usize>,
    pub output_reserve_tokens: usize,
    pub estimator_margin_tokens: usize,
    pub exact_output_reserve_known: bool,
    pub breakdown: ContextRequestTokenBreakdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPayloadPressure {
    pub image_count: usize,
    pub estimated_base64_bytes: usize,
}
