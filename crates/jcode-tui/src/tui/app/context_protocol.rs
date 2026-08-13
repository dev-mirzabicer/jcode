use crate::protocol::{
    ContextActionRequiredReason, ContextDraft, ContextDraftIdentity, ContextDraftProgress,
    ContextEditorSnapshot, ContextMessageDetail, ContextPendingInputMetadata, ContextRequestKind,
    ContextServiceError, ContextTransactionResult, ContextTransactionSummary,
};
use jcode_session_types::StoredContextEmergencyPolicy;

#[derive(Default)]
pub(in crate::tui::app) struct ContextProtocolState {
    pub snapshot_request_id: Option<u64>,
    pub snapshot: Option<ContextEditorSnapshot>,
    detail_request: Option<ContextDetailRequest>,
    pub detail: Option<ContextMessageDetail>,
    pub draft_monitor_request_id: Option<u64>,
    draft_monitor_request_kind: Option<ContextRequestKind>,
    pub tracked_draft_id: Option<String>,
    pub draft: Option<ContextClientDraftState>,
    pub history_request_id: Option<u64>,
    pub history: Option<ContextTransactionHistoryPage>,
    transaction_request: Option<ContextTransactionRequest>,
    pub transaction_result: Option<ContextTransactionOutcome>,
    pub policy_request_id: Option<u64>,
    pub emergency_policy: Option<StoredContextEmergencyPolicy>,
    pub last_rejection: Option<ContextRequestRejection>,
    pub action_required: Option<ContextActionRequiredState>,
    pub accepted_session_id: Option<String>,
    pub accepted_context_revision: Option<u64>,
    pub accepted_transcript_digest: Option<u64>,
}

pub(in crate::tui::app) struct ContextDetailRequest {
    id: u64,
    session_id: String,
    context_revision: u64,
    transcript_digest: u64,
    message_id: String,
    block_ordinal: usize,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Step 8 reduces every draft event into this complete review state; Step 9 renders the retained fields"
    )
)]
pub(in crate::tui::app) enum ContextClientDraftState {
    Progress {
        draft_id: String,
        progress: ContextDraftProgress,
    },
    Ready(Box<ContextDraft>),
    Applying(ContextDraftIdentity),
    Applied {
        identity: ContextDraftIdentity,
        transaction_id: String,
        revision: u64,
    },
    Failed {
        identity: ContextDraftIdentity,
        error: ContextServiceError,
        stale: bool,
    },
    Canceled(ContextDraftIdentity),
    Expired(ContextDraftIdentity),
}

impl ContextClientDraftState {
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Progress { .. } | Self::Applying(_))
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Step 8 retains the bounded history page; Step 9 renders and navigates its fields"
    )
)]
pub(in crate::tui::app) struct ContextTransactionHistoryPage {
    pub context_revision: u64,
    pub total_transactions: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub transactions: Vec<ContextTransactionSummary>,
}

struct ContextTransactionRequest {
    id: u64,
    kind: ContextRequestKind,
    correlation_id: String,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Step 8 retains the correlated transaction outcome; Step 9 renders its review feedback"
    )
)]
pub(in crate::tui::app) struct ContextTransactionOutcome {
    pub request_id: u64,
    pub request: ContextRequestKind,
    pub correlation_id: String,
    pub result: ContextTransactionResult,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Step 8 retains typed rejection metadata; Step 9 renders request-specific recovery copy"
    )
)]
pub(in crate::tui::app) struct ContextRequestRejection {
    pub request_id: u64,
    pub request: ContextRequestKind,
    pub draft_id: Option<String>,
    pub transaction_id: Option<String>,
    pub error: ContextServiceError,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Step 8 retains prompt-safe blocked metadata; Step 10 consumes it for composer restoration"
    )
)]
pub(in crate::tui::app) struct ContextActionRequiredState {
    pub request_id: u64,
    pub session_id: String,
    pub context_revision: u64,
    pub reason: ContextActionRequiredReason,
    pub required_reduction_tokens: usize,
    pub pending_input: Option<ContextPendingInputMetadata>,
    pub details: Vec<String>,
    pub automatic_retry: bool,
}

impl ContextProtocolState {
    pub fn accept_history(&mut self, session_id: &str, context_revision: u64) {
        let session_changed = self.accepted_session_id.as_deref() != Some(session_id);
        if session_changed {
            self.clear_session_scoped();
            self.accepted_session_id = Some(session_id.to_string());
        } else {
            // A full same-session History payload can represent rewind, repair, or
            // reconnect state whose authoritative transcript changed without a
            // context-view revision change. The caller deliberately invokes this
            // only for History payloads selected for application, never for a
            // metadata-only model-catalog refresh.
            self.invalidate_revision_scoped();
        }
        self.accepted_context_revision = Some(context_revision);
        self.accepted_transcript_digest = None;
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 editor transport records snapshot request correlation before dispatch"
        )
    )]
    pub fn begin_snapshot_request(&mut self, id: u64) {
        self.last_rejection = None;
        self.snapshot_request_id = Some(id);
        self.detail_request = None;
        self.detail = None;
    }

    pub fn accept_snapshot(&mut self, id: u64, snapshot: ContextEditorSnapshot) -> bool {
        if self.snapshot_request_id != Some(id)
            || self
                .accepted_session_id
                .as_deref()
                .is_some_and(|session_id| session_id != snapshot.session_id)
        {
            return false;
        }
        self.snapshot_request_id = None;
        self.detail_request = None;
        self.detail = None;
        self.accepted_session_id = Some(snapshot.session_id.clone());
        self.accepted_context_revision = Some(snapshot.context_revision);
        self.accepted_transcript_digest = Some(snapshot.transcript_digest);
        self.emergency_policy = Some(snapshot.emergency_policy.clone());
        self.snapshot = Some(snapshot);
        true
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 editor transport records exact lazy-detail identity before dispatch"
        )
    )]
    pub fn begin_detail_request(
        &mut self,
        id: u64,
        session_id: String,
        context_revision: u64,
        transcript_digest: u64,
        message_id: String,
        block_ordinal: usize,
    ) {
        self.last_rejection = None;
        self.detail_request = Some(ContextDetailRequest {
            id,
            session_id,
            context_revision,
            transcript_digest,
            message_id,
            block_ordinal,
        });
    }

    pub fn accept_detail(&mut self, id: u64, detail: ContextMessageDetail) -> bool {
        let Some(expected) = self.detail_request.as_ref() else {
            return false;
        };
        if expected.id != id
            || expected.session_id != detail.session_id
            || expected.context_revision != detail.context_revision
            || expected.transcript_digest != detail.transcript_digest
            || expected.message_id != detail.message_id
            || expected.block_ordinal != detail.block_ordinal
            || self.accepted_session_id.as_deref() != Some(detail.session_id.as_str())
            || self.accepted_context_revision != Some(detail.context_revision)
            || self.accepted_transcript_digest != Some(detail.transcript_digest)
        {
            return false;
        }
        self.detail_request = None;
        self.detail = Some(detail);
        true
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 editor transport binds prepare progress to the initiating request"
        )
    )]
    pub fn begin_prepare_draft(&mut self, id: u64) {
        self.last_rejection = None;
        self.draft_monitor_request_id = Some(id);
        self.draft_monitor_request_kind = Some(ContextRequestKind::PrepareDraft);
        self.tracked_draft_id = None;
        self.draft = None;
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 reconnect flow binds retained draft monitoring before status dispatch"
        )
    )]
    pub fn begin_draft_monitor(&mut self, id: u64, draft_id: String) {
        self.last_rejection = None;
        self.draft_monitor_request_id = Some(id);
        self.draft_monitor_request_kind = Some(ContextRequestKind::DraftStatus);
        self.tracked_draft_id = Some(draft_id);
        self.draft = None;
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 cancel flow binds the retained draft before dispatch"
        )
    )]
    pub fn begin_cancel_draft(&mut self, id: u64, draft_id: String) {
        self.last_rejection = None;
        self.draft_monitor_request_id = Some(id);
        self.draft_monitor_request_kind = Some(ContextRequestKind::CancelDraft);
        self.tracked_draft_id = Some(draft_id);
        self.draft = None;
    }

    pub fn accept_draft_progress(
        &mut self,
        id: u64,
        draft_id: String,
        progress: ContextDraftProgress,
    ) -> bool {
        if !self.matches_draft_event(id, &draft_id)
            || self
                .draft
                .as_ref()
                .is_some_and(ContextClientDraftState::is_terminal)
        {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Progress { draft_id, progress });
        true
    }

    pub fn accept_draft_ready(&mut self, id: u64, draft: Box<ContextDraft>) -> bool {
        let draft_id = draft.identity.draft_id.clone();
        if !self.matches_draft_event(id, &draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Ready(draft));
        true
    }

    pub fn accept_draft_applying(&mut self, id: u64, identity: ContextDraftIdentity) -> bool {
        if !self.matches_draft_event(id, &identity.draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Applying(identity));
        true
    }

    pub fn accept_draft_applied(
        &mut self,
        id: u64,
        identity: ContextDraftIdentity,
        transaction_id: String,
        revision: u64,
    ) -> bool {
        if !self.matches_draft_event(id, &identity.draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Applied {
            identity,
            transaction_id,
            revision,
        });
        true
    }

    pub fn accept_draft_failed(
        &mut self,
        id: u64,
        identity: ContextDraftIdentity,
        error: ContextServiceError,
        stale: bool,
    ) -> bool {
        if !self.matches_draft_event(id, &identity.draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Failed {
            identity,
            error,
            stale,
        });
        true
    }

    pub fn accept_draft_canceled(&mut self, id: u64, identity: ContextDraftIdentity) -> bool {
        if !self.matches_draft_event(id, &identity.draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Canceled(identity));
        true
    }

    pub fn accept_draft_expired(&mut self, id: u64, identity: ContextDraftIdentity) -> bool {
        if !self.matches_draft_event(id, &identity.draft_id) {
            return false;
        }
        self.draft = Some(ContextClientDraftState::Expired(identity));
        true
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 history view records page request correlation before dispatch"
        )
    )]
    pub fn begin_history_request(&mut self, id: u64) {
        self.last_rejection = None;
        self.history_request_id = Some(id);
    }

    pub fn accept_transaction_history(
        &mut self,
        id: u64,
        context_revision: u64,
        total_transactions: usize,
        offset: usize,
        next_offset: Option<usize>,
        transactions: Vec<ContextTransactionSummary>,
    ) -> bool {
        if self.history_request_id != Some(id) {
            return false;
        }
        if self.accepted_context_revision != Some(context_revision) {
            self.invalidate_revision_scoped();
            self.accepted_context_revision = Some(context_revision);
        }
        self.history_request_id = None;
        self.history = Some(ContextTransactionHistoryPage {
            context_revision,
            total_transactions,
            offset,
            next_offset,
            transactions,
        });
        true
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 apply, revert, and reapply actions record exact correlation before dispatch"
        )
    )]
    pub fn begin_transaction_request(
        &mut self,
        id: u64,
        kind: ContextRequestKind,
        correlation_id: String,
    ) {
        self.last_rejection = None;
        self.transaction_request = Some(ContextTransactionRequest {
            id,
            kind,
            correlation_id,
        });
    }

    pub fn accept_transaction_result(
        &mut self,
        id: u64,
        kind: ContextRequestKind,
        correlation_id: String,
        result: ContextTransactionResult,
    ) -> bool {
        let Some(expected) = self.transaction_request.as_ref() else {
            return false;
        };
        if expected.id != id || expected.kind != kind || expected.correlation_id != correlation_id {
            return false;
        }
        self.invalidate_revision_scoped();
        self.accepted_context_revision = Some(result.revision);
        self.transaction_request = None;
        self.transaction_result = Some(ContextTransactionOutcome {
            request_id: id,
            request: kind,
            correlation_id,
            result,
        });
        true
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Step 9 policy controls record request correlation before dispatch"
        )
    )]
    pub fn begin_policy_request(&mut self, id: u64) {
        self.last_rejection = None;
        self.policy_request_id = Some(id);
    }

    pub fn accept_policy(
        &mut self,
        id: u64,
        session_id: String,
        policy: StoredContextEmergencyPolicy,
    ) -> bool {
        if self.policy_request_id != Some(id)
            || self
                .accepted_session_id
                .as_deref()
                .is_some_and(|accepted| accepted != session_id)
        {
            return false;
        }
        self.policy_request_id = None;
        self.accepted_session_id.get_or_insert(session_id);
        self.emergency_policy = Some(policy);
        true
    }

    pub fn accept_rejection(
        &mut self,
        id: u64,
        request: ContextRequestKind,
        draft_id: Option<String>,
        transaction_id: Option<String>,
        error: ContextServiceError,
    ) -> bool {
        let uncorrelated_capacity_fallback = matches!(&error, ContextServiceError::Capacity(_))
            && draft_id.is_none()
            && transaction_id.is_none();
        let matches_pending = match request {
            ContextRequestKind::Snapshot => self.snapshot_request_id == Some(id),
            ContextRequestKind::MessageDetail => self
                .detail_request
                .as_ref()
                .is_some_and(|expected| expected.id == id),
            ContextRequestKind::PrepareDraft => {
                self.draft_monitor_request_id == Some(id)
                    && self.draft_monitor_request_kind == Some(request)
                    && !self
                        .draft
                        .as_ref()
                        .is_some_and(ContextClientDraftState::is_terminal)
                    && match (self.tracked_draft_id.as_deref(), draft_id.as_deref()) {
                        (Some(expected), Some(actual)) => expected == actual,
                        (None, _) => true,
                        (Some(_), None) => false,
                    }
            }
            ContextRequestKind::CancelDraft | ContextRequestKind::DraftStatus => {
                self.draft_monitor_request_id == Some(id)
                    && self.draft_monitor_request_kind == Some(request)
                    && !self
                        .draft
                        .as_ref()
                        .is_some_and(ContextClientDraftState::is_terminal)
                    && (self.tracked_draft_id.as_deref() == draft_id.as_deref()
                        || uncorrelated_capacity_fallback)
            }
            ContextRequestKind::ApplyDraft
            | ContextRequestKind::RevertTransaction
            | ContextRequestKind::ReapplyTransaction => {
                let correlation = if request == ContextRequestKind::ApplyDraft {
                    draft_id.as_deref()
                } else {
                    transaction_id.as_deref()
                };
                self.transaction_request.as_ref().is_some_and(|expected| {
                    expected.id == id
                        && expected.kind == request
                        && (Some(expected.correlation_id.as_str()) == correlation
                            || uncorrelated_capacity_fallback)
                })
            }
            ContextRequestKind::TransactionHistory => self.history_request_id == Some(id),
            ContextRequestKind::SetEmergencyPolicy => self.policy_request_id == Some(id),
            ContextRequestKind::LegacyCompact | ContextRequestKind::LegacySetCompactionMode => true,
        };
        if !matches_pending {
            return false;
        }

        match request {
            ContextRequestKind::Snapshot => self.snapshot_request_id = None,
            ContextRequestKind::MessageDetail => self.detail_request = None,
            ContextRequestKind::PrepareDraft
            | ContextRequestKind::CancelDraft
            | ContextRequestKind::DraftStatus => {
                self.draft_monitor_request_id = None;
                self.draft_monitor_request_kind = None;
                self.tracked_draft_id = None;
            }
            ContextRequestKind::ApplyDraft
            | ContextRequestKind::RevertTransaction
            | ContextRequestKind::ReapplyTransaction => self.transaction_request = None,
            ContextRequestKind::TransactionHistory => self.history_request_id = None,
            ContextRequestKind::SetEmergencyPolicy => self.policy_request_id = None,
            ContextRequestKind::LegacyCompact | ContextRequestKind::LegacySetCompactionMode => {}
        }
        self.last_rejection = Some(ContextRequestRejection {
            request_id: id,
            request,
            draft_id,
            transaction_id,
            error,
        });
        true
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "blocked-turn metadata must stay exact for Step 10 prompt restoration"
    )]
    pub fn accept_action_required(
        &mut self,
        id: u64,
        session_id: String,
        context_revision: u64,
        reason: ContextActionRequiredReason,
        required_reduction_tokens: usize,
        pending_input: Option<ContextPendingInputMetadata>,
        details: Vec<String>,
        automatic_retry: bool,
    ) -> bool {
        if self
            .accepted_session_id
            .as_deref()
            .is_some_and(|accepted| accepted != session_id)
        {
            return false;
        }
        self.action_required = Some(ContextActionRequiredState {
            request_id: id,
            session_id,
            context_revision,
            reason,
            required_reduction_tokens,
            pending_input,
            details,
            automatic_retry,
        });
        true
    }

    fn matches_draft_event(&mut self, id: u64, draft_id: &str) -> bool {
        if self.draft_monitor_request_id != Some(id) {
            return false;
        }
        match self.tracked_draft_id.as_deref() {
            Some(expected) => expected == draft_id,
            None => {
                self.tracked_draft_id = Some(draft_id.to_string());
                true
            }
        }
    }

    fn invalidate_revision_scoped(&mut self) {
        self.snapshot_request_id = None;
        self.snapshot = None;
        self.detail_request = None;
        self.detail = None;
        self.draft_monitor_request_id = None;
        self.draft_monitor_request_kind = None;
        self.tracked_draft_id = None;
        self.draft = None;
        self.history_request_id = None;
        self.history = None;
        self.accepted_transcript_digest = None;
    }

    fn clear_session_scoped(&mut self) {
        self.invalidate_revision_scoped();
        self.transaction_request = None;
        self.transaction_result = None;
        self.policy_request_id = None;
        self.emergency_policy = None;
        self.last_rejection = None;
        self.action_required = None;
        self.accepted_session_id = None;
        self.accepted_context_revision = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use crate::protocol::{
        ContextDraftPhase, ContextDraftPreview, ContextEditorMessage, ContextMessageDetailFormat,
        ContextOperationCounts, ContextTextChunk,
    };
    use chrono::{DateTime, Duration, Utc};
    use jcode_provider_core::{
        ContextProjectionValidationReport, ContextProjectionValidationStatus, ContextProviderFamily,
    };
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextBlockKind, StoredContextEconomics,
        StoredContextTransactionStatusKind,
    };

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-12T18:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn identity() -> ContextDraftIdentity {
        ContextDraftIdentity {
            draft_id: "draft-1".to_string(),
            session_id: "session-1".to_string(),
            base_context_revision: 4,
            raw_message_count: 1,
            transcript_digest: 99,
            provider_name: "openai".to_string(),
            model: "gpt-test".to_string(),
            route: "oauth".to_string(),
            created_at: timestamp(),
            expires_at: timestamp() + Duration::minutes(30),
        }
    }

    fn economics() -> StoredContextEconomics {
        StoredContextEconomics {
            projected_tokens_before: 100,
            projected_tokens_after: 50,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            unchanged_prefix_items: 0,
            earliest_changed_provider_item: Some(0),
            old_affected_suffix_tokens: 100,
            new_affected_suffix_tokens: 50,
            deleted_input_tokens: 50,
            context_window: Some(1_000),
            safe_input_budget: Some(950),
            pricing: None,
            first_request_delta_usd: None,
            recurring_savings_per_turn_usd: None,
            break_even_turns: None,
            assumptions: Vec::new(),
        }
    }

    fn transaction_summary() -> ContextTransactionSummary {
        ContextTransactionSummary {
            id: "transaction-1".to_string(),
            created_at: timestamp(),
            base_revision: 4,
            active: true,
            latest_status: Some(StoredContextTransactionStatusKind::Applied),
            latest_status_revision: Some(5),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operation_counts: ContextOperationCounts::default(),
            application: None,
            economics: Some(economics()),
        }
    }

    fn transaction_result() -> ContextTransactionResult {
        ContextTransactionResult {
            transaction: transaction_summary(),
            revision: 5,
            status: StoredContextTransactionStatusKind::Applied,
            warnings: Vec::new(),
        }
    }

    fn draft() -> ContextDraft {
        ContextDraft {
            identity: identity(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            required_operations: Vec::new(),
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview: ContextDraftPreview {
                raw_stored_message_count: 1,
                current_context_revision: 4,
                proposed_context_revision: 5,
                economics: economics(),
                validation: ContextProjectionValidationReport {
                    provider_family: ContextProviderFamily::OpenAiResponses,
                    provider_name: "openai".to_string(),
                    provider_display_name: "OpenAI".to_string(),
                    model: "gpt-test".to_string(),
                    evidence_tag: "fixture-v1".to_string(),
                    builder_status: ContextProjectionValidationStatus::Supported,
                    normalized_item_count: 1,
                    formatter_placeholder_count: 0,
                    normalization_notes: Vec::new(),
                    findings: Vec::new(),
                },
                formatter_placeholder_count: 0,
                operation_previews: Vec::new(),
                notices: Vec::new(),
            },
            curator_usage: Vec::new(),
        }
    }

    fn snapshot(id: &str, revision: u64, digest: u64) -> ContextEditorSnapshot {
        ContextEditorSnapshot {
            session_id: id.to_string(),
            context_revision: revision,
            raw_message_count: 1,
            transcript_digest: digest,
            processing: false,
            provider_name: "openai".to_string(),
            provider_display_name: "OpenAI".to_string(),
            model: "gpt-test".to_string(),
            route: "oauth".to_string(),
            context_window: 1_000,
            projected_request_tokens: 100,
            message_page_start: 0,
            message_page_end: 1,
            next_message_page_start: None,
            messages: vec![ContextEditorMessage {
                message_id: "message-1".to_string(),
                stored_index: 0,
                role: Role::User,
                display_role: None,
                timestamp: Some(timestamp()),
                raw_provider_tokens: 4,
                projected_provider_tokens: 4,
                preview: "hello".to_string(),
                blocks: Vec::new(),
                tool_group_ids: Vec::new(),
                summary_coverage: None,
                active_operations: Vec::new(),
                removable_reasoning_kinds: Vec::new(),
            }],
            active_transactions: Vec::new(),
            emergency_policy: StoredContextEmergencyPolicy::Block,
        }
    }

    fn detail(revision: u64, digest: u64) -> ContextMessageDetail {
        ContextMessageDetail {
            session_id: "session-1".to_string(),
            context_revision: revision,
            transcript_digest: digest,
            message_id: "message-1".to_string(),
            stored_index: 0,
            role: Role::User,
            display_role: None,
            timestamp: Some(timestamp()),
            block_ordinal: 0,
            block_kind: StoredContextBlockKind::Text,
            format: ContextMessageDetailFormat::Text,
            content: ContextTextChunk {
                start_char: 0,
                end_char: 5,
                total_chars: 5,
                text: "hello".to_string(),
                next_start_char: None,
            },
            semantic_id: None,
            tool_name: None,
            tool_use_id: None,
            tool_result_is_error: None,
            provider_status: None,
            image_media_type: None,
            image_encoded_bytes: None,
            opaque_signature_present: false,
            encrypted_state_present: false,
        }
    }

    #[test]
    fn snapshot_and_detail_accept_only_latest_exact_identity() {
        let mut state = ContextProtocolState::default();
        state.accept_history("session-1", 4);
        state.begin_snapshot_request(10);
        state.begin_snapshot_request(11);
        assert!(!state.accept_snapshot(10, snapshot("session-1", 4, 99)));
        assert!(state.accept_snapshot(11, snapshot("session-1", 4, 99)));
        assert_eq!(
            state.snapshot.as_ref().map(|value| value.transcript_digest),
            Some(99)
        );

        state.begin_detail_request(
            12,
            "session-1".to_string(),
            4,
            99,
            "message-1".to_string(),
            0,
        );
        assert!(!state.accept_detail(12, detail(4, 100)));
        assert!(state.accept_detail(12, detail(4, 99)));
        assert_eq!(
            state
                .detail
                .as_ref()
                .map(|value| value.content.text.as_str()),
            Some("hello")
        );

        state.begin_snapshot_request(13);
        assert!(!state.accept_snapshot(13, snapshot("session-2", 4, 99)));
        assert_eq!(state.accepted_session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn draft_reducer_enforces_request_draft_and_terminal_monotonicity() {
        let mut state = ContextProtocolState::default();
        state.begin_prepare_draft(20);
        let progress = ContextDraftProgress {
            phase: ContextDraftPhase::PreparingArtifacts,
            completed_items: 1,
            total_items: 2,
        };
        assert!(state.accept_draft_progress(20, "draft-1".to_string(), progress.clone()));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Progress { draft_id, progress })
                if draft_id == "draft-1" && progress.completed_items == 1
        ));
        assert!(!state.accept_draft_progress(20, "wrong-draft".to_string(), progress.clone()));
        assert!(state.accept_draft_ready(20, Box::new(draft())));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Ready(value))
                if value.identity.draft_id == "draft-1"
        ));
        assert!(!state.accept_draft_progress(20, "draft-1".to_string(), progress));

        state.begin_draft_monitor(21, "draft-1".to_string());
        assert!(state.accept_draft_applying(21, identity()));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Applying(value))
                if value.draft_id == "draft-1"
        ));
        assert!(!state.accept_draft_applied(22, identity(), "transaction-1".to_string(), 5,));
        assert!(state.accept_draft_applied(21, identity(), "transaction-1".to_string(), 5,));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Applied {
                identity,
                transaction_id,
                revision: 5,
            }) if identity.draft_id == "draft-1" && transaction_id == "transaction-1"
        ));

        state.begin_draft_monitor(23, "draft-1".to_string());
        assert!(state.accept_draft_failed(
            23,
            identity(),
            ContextServiceError::Stale("revision changed".to_string()),
            true,
        ));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Failed {
                identity,
                error: ContextServiceError::Stale(reason),
                stale: true,
            }) if identity.draft_id == "draft-1" && reason == "revision changed"
        ));

        state.begin_draft_monitor(24, "draft-1".to_string());
        assert!(state.accept_draft_canceled(24, identity()));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Canceled(value)) if value.draft_id == "draft-1"
        ));
        state.begin_draft_monitor(25, "draft-1".to_string());
        assert!(state.accept_draft_expired(25, identity()));
        assert!(matches!(
            state.draft.as_ref(),
            Some(ContextClientDraftState::Expired(value)) if value.draft_id == "draft-1"
        ));
    }

    #[test]
    fn transaction_success_updates_revision_and_invalidates_stale_review_state() {
        let mut state = ContextProtocolState::default();
        state.accept_history("session-1", 4);
        state.begin_snapshot_request(30);
        assert!(state.accept_snapshot(30, snapshot("session-1", 4, 99)));
        state.begin_detail_request(
            31,
            "session-1".to_string(),
            4,
            99,
            "message-1".to_string(),
            0,
        );
        assert!(state.accept_detail(31, detail(4, 99)));
        state.begin_prepare_draft(32);
        assert!(state.accept_draft_ready(32, Box::new(draft())));

        state.begin_transaction_request(33, ContextRequestKind::ApplyDraft, "draft-1".to_string());
        assert!(!state.accept_transaction_result(
            33,
            ContextRequestKind::ApplyDraft,
            "wrong-draft".to_string(),
            transaction_result(),
        ));
        assert!(state.accept_transaction_result(
            33,
            ContextRequestKind::ApplyDraft,
            "draft-1".to_string(),
            transaction_result(),
        ));
        assert_eq!(state.accepted_context_revision, Some(5));
        assert!(state.snapshot.is_none());
        assert!(state.detail.is_none());
        assert!(state.draft.is_none());
        let outcome = state
            .transaction_result
            .as_ref()
            .expect("retained transaction result");
        assert_eq!(outcome.request_id, 33);
        assert_eq!(outcome.request, ContextRequestKind::ApplyDraft);
        assert_eq!(outcome.correlation_id, "draft-1");
        assert_eq!(outcome.result.revision, 5);

        state.begin_history_request(34);
        assert!(state.accept_transaction_history(34, 5, 1, 0, None, vec![transaction_summary()],));
        let history = state.history.as_ref().expect("history page retained");
        assert_eq!(history.context_revision, 5);
        assert_eq!(history.total_transactions, 1);
        assert_eq!(history.offset, 0);
        assert_eq!(history.next_offset, None);
        assert_eq!(history.transactions[0].id, "transaction-1");
    }

    #[test]
    fn history_policy_rejection_and_action_required_are_session_scoped() {
        let mut state = ContextProtocolState::default();
        state.accept_history("session-1", 4);
        state.begin_policy_request(40);
        assert!(!state.accept_policy(
            40,
            "session-2".to_string(),
            StoredContextEmergencyPolicy::Block,
        ));
        assert!(state.accept_policy(
            40,
            "session-1".to_string(),
            StoredContextEmergencyPolicy::Block,
        ));
        assert!(matches!(
            state.emergency_policy,
            Some(StoredContextEmergencyPolicy::Block)
        ));

        state.begin_history_request(41);
        assert!(!state.accept_rejection(
            40,
            ContextRequestKind::TransactionHistory,
            None,
            None,
            ContextServiceError::Runtime("late history failure".to_string()),
        ));
        assert!(state.last_rejection.is_none());
        assert!(state.accept_rejection(
            41,
            ContextRequestKind::TransactionHistory,
            None,
            None,
            ContextServiceError::Runtime("current history failure".to_string()),
        ));
        assert_eq!(state.history_request_id, None);

        state.begin_transaction_request(
            42,
            ContextRequestKind::ApplyDraft,
            "oversized-client-draft-id".to_string(),
        );
        assert!(state.accept_rejection(
            42,
            ContextRequestKind::ApplyDraft,
            None,
            None,
            ContextServiceError::Capacity("bounded rejection fallback".to_string()),
        ));
        assert!(state.transaction_request.is_none());

        state.begin_cancel_draft(43, "draft-to-cancel".to_string());
        assert!(!state.accept_rejection(
            43,
            ContextRequestKind::DraftStatus,
            Some("draft-to-cancel".to_string()),
            None,
            ContextServiceError::Runtime("wrong request kind".to_string()),
        ));
        assert!(state.accept_rejection(
            43,
            ContextRequestKind::CancelDraft,
            Some("draft-to-cancel".to_string()),
            None,
            ContextServiceError::Runtime("cancel failed".to_string()),
        ));
        assert_eq!(state.draft_monitor_request_id, None);

        assert!(state.accept_rejection(
            44,
            ContextRequestKind::LegacyCompact,
            Some("draft-1".to_string()),
            Some("transaction-1".to_string()),
            ContextServiceError::InvalidSelection("use /context edit".to_string()),
        ));
        let rejection = state.last_rejection.as_ref().expect("rejection retained");
        assert_eq!(rejection.request_id, 44);
        assert_eq!(rejection.request, ContextRequestKind::LegacyCompact);
        assert_eq!(rejection.draft_id.as_deref(), Some("draft-1"));
        assert_eq!(rejection.transaction_id.as_deref(), Some("transaction-1"));
        assert!(matches!(
            rejection.error,
            ContextServiceError::InvalidSelection(_)
        ));

        let pending = ContextPendingInputMetadata {
            request_id: 77,
            content_chars: 25,
            content_digest: 123,
            image_count: 1,
        };
        assert!(!state.accept_action_required(
            42,
            "session-2".to_string(),
            4,
            ContextActionRequiredReason::PreflightLimit,
            512,
            Some(pending.clone()),
            vec!["too large".to_string()],
            false,
        ));
        assert!(state.accept_action_required(
            42,
            "session-1".to_string(),
            4,
            ContextActionRequiredReason::PreflightLimit,
            512,
            Some(pending),
            vec!["too large".to_string()],
            false,
        ));
        let action = state.action_required.as_ref().expect("action retained");
        assert_eq!(action.request_id, 42);
        assert_eq!(action.session_id, "session-1");
        assert_eq!(action.context_revision, 4);
        assert_eq!(action.reason, ContextActionRequiredReason::PreflightLimit);
        assert_eq!(action.required_reduction_tokens, 512);
        assert_eq!(
            action.pending_input.as_ref().map(|value| value.request_id),
            Some(77)
        );
        assert_eq!(action.details, vec!["too large"]);
        assert!(!action.automatic_retry);

        state.accept_history("session-2", 0);
        assert!(state.emergency_policy.is_none());
        assert!(state.last_rejection.is_none());
        assert!(state.action_required.is_none());
        assert!(state.transaction_result.is_none());
        assert_eq!(state.accepted_session_id.as_deref(), Some("session-2"));
        assert_eq!(state.accepted_context_revision, Some(0));
    }
}
