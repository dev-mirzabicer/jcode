mod change_digest;
mod commit;
mod curator;
mod draft;
mod history;
pub mod preflight;
pub mod provider_validation;
mod reasoning_invalidation;
#[cfg(test)]
pub(crate) mod reasoning_invalidation_tests;
mod snapshot;

pub use crate::protocol::{
    ContextDistillationProposal, ContextDraft, ContextDraftIdentity, ContextDraftPhase,
    ContextDraftPreview, ContextDraftProgress, ContextDraftRequest, ContextDraftStatus,
    ContextEditorBlock, ContextEditorMessage, ContextEditorSnapshot, ContextIneligibleDistillation,
    ContextMessageDetail, ContextMessageDetailFormat, ContextMessageRangeSelection,
    ContextOperationBadge, ContextOperationBadgeKind, ContextOperationCounts,
    ContextOperationPreview, ContextReasoningSelectionRequest, ContextRequestKind,
    ContextServiceError, ContextSummaryCoverage, ContextTextChunk, ContextToolResultSelection,
    ContextTransactionResult, ContextTransactionSummary,
};
pub use change_digest::*;
pub use commit::*;
pub use curator::*;
pub use draft::*;
pub use history::*;
pub use preflight::*;
/// The label of a client-identity sync (the Claude OAuth billing header
/// version) in the cache-invalidation journal and as a cause of reasoning it
/// invalidates (INT-01/WP-06 R27). Probe G6.6 measured that the API neither
/// caches nor binds that block today, and the binding digest hashes it as
/// fixed text, so a sync currently invalidates nothing; the record keeps the
/// change attributable if that stops being true.
pub const CLIENT_IDENTITY_TRANSITION: &str = "OAuth client identity sync";
mod operator_notice;
pub use operator_notice::with_operator_notices;
pub use reasoning_invalidation::{
    ContextRequestPrefix, ProviderReportedReasoning, ReasoningInvalidationOutcome,
    RequestPrefixSource, describe_reasoning_invalidation, reasoning_reconciliation_needed,
    reconcile_before_request,
};
pub use snapshot::*;

pub(crate) fn admit_mutation(
    session: Option<String>,
) -> Result<Option<crate::runtime_lifecycle::admission::WorkPermit>, ContextServiceError> {
    crate::runtime_lifecycle::admission::preparation("context-mutation", session)
        .map_err(|error| ContextServiceError::Runtime(error.to_string()))
}
