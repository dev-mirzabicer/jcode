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
pub use reasoning_invalidation::{
    ContextRequestPrefix, ReasoningInvalidationOutcome, RequestPrefixSource,
    describe_reasoning_invalidation, reasoning_reconciliation_needed, reconcile_before_request,
};
pub use snapshot::*;

pub(crate) fn admit_mutation(
    session: Option<String>,
) -> Result<Option<crate::runtime_lifecycle::admission::WorkPermit>, ContextServiceError> {
    crate::runtime_lifecycle::admission::preparation("context-mutation", session)
        .map_err(|error| ContextServiceError::Runtime(error.to_string()))
}
