use crate::agent::Agent;
use crate::context::change_digest::extract_context_change_evidence;
use crate::context::curator::{
    ContextCuratorArtifacts, ContextCuratorLimits, ContextCuratorRangeWork, ContextCuratorRoute,
    ContextCuratorToolArtifact, ContextCuratorToolWork, resolve_context_curator_route,
    run_context_curator,
};
use crate::context::provider_validation::require_supported_projected_messages;
use crate::context::{ContextPersistence, SessionContextPersistence};
use crate::message::ContentBlock;
use crate::protocol::{
    ContextDistillationProposal, ContextDraft, ContextDraftIdentity, ContextDraftPhase,
    ContextDraftPreview, ContextDraftProgress, ContextDraftRequest, ContextDraftStatus,
    ContextIneligibleDistillation, ContextOperationPreview, ContextReasoningSelectionRequest,
    ContextServiceError,
};
#[cfg(test)]
use crate::protocol::{ContextMessageRangeSelection, ContextToolResultSelection};
#[cfg(test)]
use crate::provider::ContextProjectionValidationReport;
use crate::provider::{
    ContextProjectionOperationKind, ContextProjectionValidationOperation, ContextReasoningBlockKind,
};
use chrono::{DateTime, Utc};
use jcode_context_core::{
    ContextEconomicsInput, ContextTargetIndex, analyze_cache_prefix,
    authoritative_transcript_digest, build_content_target, build_message_range,
    calculate_context_economics, close_message_ranges, estimate_content_block_tokens,
    estimate_message_tokens, project_context, resolve_reasoning_suppression_for_ranges,
    resolve_reasoning_suppression_keep_latest, validate_context_state,
};
use jcode_session_types::{
    StoredContextAuthorization, StoredContextBlockKind, StoredContextCuratorUsage,
    StoredContextEconomics, StoredContextOperation, StoredContextStatusEvent,
    StoredContextTransaction, StoredContextTransactionStatusKind, StoredContextViewState,
    StoredMessage, StoredMessageRange, StoredRangeSummary, StoredReasoningSuppression,
    StoredToolResultDistillation,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TERMINAL_DRAFT_RESERVATION_FLOOR_BYTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct ContextServiceLimits {
    pub max_drafts: usize,
    pub max_total_bytes: usize,
    pub ttl: Duration,
    pub curator: ContextCuratorLimits,
}

impl Default for ContextServiceLimits {
    fn default() -> Self {
        Self {
            max_drafts: 32,
            max_total_bytes: 64 * 1024 * 1024,
            ttl: Duration::from_secs(30 * 60),
            curator: ContextCuratorLimits::default(),
        }
    }
}

pub struct ContextTransactionService {
    pub(crate) drafts: Mutex<ContextDraftStore>,
    pub(crate) persistence: Arc<dyn ContextPersistence>,
    pub(crate) limits: ContextServiceLimits,
}

impl Default for ContextTransactionService {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextTransactionService {
    pub fn new() -> Self {
        Self::with_persistence(
            ContextServiceLimits::default(),
            Arc::new(SessionContextPersistence),
        )
    }

    pub fn with_persistence(
        limits: ContextServiceLimits,
        persistence: Arc<dyn ContextPersistence>,
    ) -> Self {
        Self {
            drafts: Mutex::new(ContextDraftStore::default()),
            persistence,
            limits,
        }
    }

    pub fn context_editor_snapshot(
        &self,
        agent: &mut Agent,
        processing: bool,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        let provider = agent.provider_handle();
        let projected_request_tokens = agent.current_context_request_token_estimate();
        let mut snapshot =
            crate::context::build_context_editor_snapshot(crate::context::ContextSnapshotInput {
                session_id: agent.session_id(),
                messages: agent.messages(),
                context_view: agent.context_view_state(),
                processing,
                provider: provider.as_ref(),
                route: &agent.context_route_identity(),
            })
            .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
        if let Some(projected_request_tokens) = projected_request_tokens {
            snapshot.projected_request_tokens = projected_request_tokens;
        }
        Ok(snapshot)
    }

    pub fn context_editor_snapshot_page(
        &self,
        agent: &mut Agent,
        processing: bool,
        page_start: usize,
        page_size: usize,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        let snapshot = self.context_editor_snapshot(agent, processing)?;
        crate::context::paginate_context_editor_snapshot(snapshot, page_start, page_size)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "lazy detail identity and bounded chunk coordinates are independent protocol fields"
    )]
    pub fn context_message_detail(
        &self,
        agent: &Agent,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        message_id: &str,
        block_ordinal: usize,
        start_char: usize,
        max_chars: usize,
    ) -> Result<crate::context::ContextMessageDetail, ContextServiceError> {
        crate::context::build_context_message_detail(crate::context::ContextMessageDetailInput {
            session_id: agent.session_id(),
            messages: agent.messages(),
            context_view: agent.context_view_state(),
            expected_context_revision,
            expected_transcript_digest,
            message_id,
            block_ordinal,
            start_char,
            max_chars,
        })
        .map_err(|error| {
            let detail = error.to_string();
            if detail.contains("revision changed") || detail.contains("digest changed") {
                ContextServiceError::Stale(detail)
            } else {
                ContextServiceError::InvalidSelection(detail)
            }
        })
    }

    pub fn prepare_draft(
        self: &Arc<Self>,
        agent: Arc<AsyncMutex<Agent>>,
        request: ContextDraftRequest,
        processing: bool,
    ) -> Result<String, ContextServiceError> {
        self.prepare_draft_with_curator_config(
            agent,
            request,
            processing,
            &crate::config::config().context.curator,
        )
    }

    fn prepare_draft_with_curator_config(
        self: &Arc<Self>,
        agent: Arc<AsyncMutex<Agent>>,
        request: ContextDraftRequest,
        processing: bool,
        curator_config: &crate::config::ContextCuratorConfig,
    ) -> Result<String, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        if request.is_empty() {
            return Err(ContextServiceError::EmptyRequest);
        }
        let guard = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let draft_id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = created_at
            + chrono::Duration::from_std(self.limits.ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(30));
        let identity = ContextDraftIdentity {
            draft_id: draft_id.clone(),
            session_id: guard.session_id().to_string(),
            base_context_revision: guard.context_view_state().revision,
            raw_message_count: guard.messages().len(),
            transcript_digest: authoritative_transcript_digest(guard.messages()),
            provider_name: guard.provider_handle().name().to_string(),
            model: guard.provider_handle().model(),
            route: guard.context_route_identity(),
            created_at,
            expires_at,
        };
        let capture = capture_context_draft(&guard, identity.clone(), request)?;
        let route = if capture.ranges.is_empty() && capture.tools.is_empty() {
            None
        } else {
            let provider_fork = guard.provider_fork();
            let model_routes = guard.model_routes();
            Some(
                resolve_context_curator_route(
                    provider_fork,
                    &model_routes,
                    &identity.route,
                    curator_config,
                )
                .map_err(|error| ContextServiceError::Curator(error.to_string()))?,
            )
        };
        drop(guard);

        let reserved_bytes = serde_json::to_vec(&capture)
            .map(|bytes| bytes.len())
            .unwrap_or(self.limits.max_total_bytes);
        let cancellation = CancellationToken::new();
        let notify = Arc::new(Notify::new());
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            ContextServiceError::Runtime(format!("no Tokio runtime is available: {error}"))
        })?;
        {
            let mut store = self.lock_store();
            store.insert_preparing(
                ContextDraftEntry {
                    identity: identity.clone(),
                    progress: ContextDraftProgress {
                        phase: ContextDraftPhase::Capturing,
                        completed_items: 0,
                        total_items: capture.ranges.len().saturating_add(capture.tools.len()),
                    },
                    state: DraftEntryState::Preparing,
                    cancellation: cancellation.clone(),
                    notify: Arc::clone(&notify),
                    reserved_bytes,
                    generation_in_flight: true,
                },
                self.limits,
            )?;
        }

        let service = Arc::clone(self);
        runtime.spawn(async move {
            service
                .prepare_draft_task(agent, capture, route, cancellation)
                .await;
        });
        Ok(draft_id)
    }

    pub fn draft_status(&self, draft_id: &str) -> Result<ContextDraftStatus, ContextServiceError> {
        let mut store = self.lock_store();
        store.expire_entries(Utc::now());
        store
            .entries
            .get(draft_id)
            .map(ContextDraftEntry::public_status)
            .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))
    }

    pub fn cancel_draft(&self, draft_id: &str) -> Result<(), ContextServiceError> {
        let mut store = self.lock_store();
        store.expire_entries(Utc::now());
        let entry = store
            .entries
            .get_mut(draft_id)
            .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?;
        match entry.state {
            DraftEntryState::Preparing => {
                entry.cancellation.cancel();
                entry.state = DraftEntryState::Canceled;
                entry.notify.notify_waiters();
                Ok(())
            }
            DraftEntryState::Ready(_) => {
                entry.state = DraftEntryState::Canceled;
                entry.refresh_terminal_reservation();
                entry.notify.notify_waiters();
                Ok(())
            }
            DraftEntryState::Canceled => Ok(()),
            DraftEntryState::Expired => {
                Err(ContextServiceError::DraftExpired(draft_id.to_string()))
            }
            DraftEntryState::Applied { .. } => Err(ContextServiceError::DraftAlreadyApplied(
                draft_id.to_string(),
            )),
            _ => Err(ContextServiceError::DraftNotReady(draft_id.to_string())),
        }
    }

    pub async fn wait_for_draft(
        &self,
        draft_id: &str,
        timeout: Duration,
    ) -> Result<ContextDraftStatus, ContextServiceError> {
        let wait = async {
            loop {
                let notify = {
                    let mut store = self.lock_store();
                    store.expire_entries(Utc::now());
                    let entry = store
                        .entries
                        .get(draft_id)
                        .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?;
                    Arc::clone(&entry.notify)
                };
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let status = {
                    let mut store = self.lock_store();
                    store.expire_entries(Utc::now());
                    store
                        .entries
                        .get(draft_id)
                        .map(ContextDraftEntry::public_status)
                        .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?
                };
                if !matches!(status, ContextDraftStatus::Preparing { .. }) {
                    return Ok(status);
                }
                notified.await;
            }
        };
        tokio::time::timeout(timeout, wait).await.map_err(|_| {
            ContextServiceError::Runtime("waiting for context draft timed out".to_string())
        })?
    }

    pub async fn wait_for_draft_update(
        &self,
        draft_id: &str,
        previous: &ContextDraftStatus,
    ) -> Result<ContextDraftStatus, ContextServiceError> {
        loop {
            let notify = {
                let mut store = self.lock_store();
                store.expire_entries(Utc::now());
                let entry = store
                    .entries
                    .get(draft_id)
                    .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?;
                Arc::clone(&entry.notify)
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let status = self.draft_status(draft_id)?;
            if &status != previous {
                return Ok(status);
            }
            notified.await;
        }
    }

    pub(crate) fn lock_store(&self) -> std::sync::MutexGuard<'_, ContextDraftStore> {
        self.drafts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn prepare_draft_task(
        self: Arc<Self>,
        agent: Arc<AsyncMutex<Agent>>,
        capture: CapturedContextDraft,
        route: Option<ContextCuratorRoute>,
        cancellation: CancellationToken,
    ) {
        let draft_id = capture.identity.draft_id.clone();
        self.update_progress(
            &draft_id,
            ContextDraftPhase::PreparingArtifacts,
            0,
            capture.ranges.len().saturating_add(capture.tools.len()),
        );
        let artifacts = match route.as_ref() {
            Some(route) => {
                run_context_curator(
                    route,
                    &capture.messages,
                    &capture.ranges,
                    &capture.tools,
                    &capture.active_summary_texts,
                    &cancellation,
                    self.limits.curator,
                )
                .await
            }
            None => Ok(ContextCuratorArtifacts::default()),
        };
        let artifacts = match artifacts {
            Ok(artifacts) => artifacts,
            Err(error) => {
                drop(capture);
                self.finish_failed(&draft_id, ContextServiceError::Curator(error.to_string()));
                return;
            }
        };
        if cancellation.is_cancelled() {
            drop(artifacts);
            drop(capture);
            self.finish_canceled(&draft_id);
            return;
        }
        self.update_progress(
            &draft_id,
            ContextDraftPhase::ValidatingProjection,
            capture.ranges.len().saturating_add(capture.tools.len()),
            capture.ranges.len().saturating_add(capture.tools.len()),
        );

        let guard = match agent.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                drop(artifacts);
                drop(capture);
                self.finish_failed(&draft_id, ContextServiceError::SessionBusy);
                return;
            }
        };
        if let Err(error) = validate_capture_identity(&guard, &capture.identity) {
            drop(guard);
            drop(artifacts);
            drop(capture);
            self.finish_failed(&draft_id, error);
            return;
        }
        let draft = build_ready_draft(&guard, capture, route, artifacts);
        drop(guard);
        match draft {
            Ok(draft) => self.finish_ready(draft),
            Err(error) => self.finish_failed(&error.draft_id, error.error),
        }
    }

    fn update_progress(
        &self,
        draft_id: &str,
        phase: ContextDraftPhase,
        completed_items: usize,
        total_items: usize,
    ) {
        let mut store = self.lock_store();
        if let Some(entry) = store.entries.get_mut(draft_id)
            && matches!(entry.state, DraftEntryState::Preparing)
        {
            entry.progress = ContextDraftProgress {
                phase,
                completed_items,
                total_items,
            };
            entry.notify.notify_waiters();
        }
    }

    fn finish_ready(&self, draft: ContextDraft) {
        let draft_id = draft.identity.draft_id.clone();
        let bytes = serde_json::to_vec(&draft)
            .map(|bytes| bytes.len())
            .unwrap_or(self.limits.max_total_bytes);
        let mut store = self.lock_store();
        let Some(current) = store.entries.get(&draft_id) else {
            return;
        };
        let current_reserved_bytes = current.reserved_bytes;
        if matches!(
            current.state,
            DraftEntryState::Canceled | DraftEntryState::Expired
        ) {
            let entry = store
                .entries
                .get_mut(&draft_id)
                .expect("draft entry was present above");
            entry.generation_in_flight = false;
            entry.refresh_terminal_reservation();
            entry.notify.notify_waiters();
            return;
        }
        if !matches!(current.state, DraftEntryState::Preparing) {
            return;
        }
        let retained_bytes = store
            .total_bytes()
            .saturating_sub(current_reserved_bytes)
            .saturating_add(bytes);
        let entry = store
            .entries
            .get_mut(&draft_id)
            .expect("draft entry was present above");
        entry.generation_in_flight = false;
        if bytes > self.limits.max_total_bytes || retained_bytes > self.limits.max_total_bytes {
            entry.state = DraftEntryState::Failed(ContextServiceError::Capacity(format!(
                "prepared draft would retain {retained_bytes} bytes against the {}-byte bound",
                self.limits.max_total_bytes
            )));
            entry.refresh_terminal_reservation();
        } else {
            entry.progress.phase = ContextDraftPhase::Ready;
            entry.state = DraftEntryState::Ready(draft);
            entry.reserved_bytes = bytes;
        }
        entry.notify.notify_waiters();
        store.enforce_total_bytes(self.limits.max_total_bytes);
    }

    fn finish_failed(&self, draft_id: &str, error: ContextServiceError) {
        let mut store = self.lock_store();
        if let Some(entry) = store.entries.get_mut(draft_id) {
            entry.generation_in_flight = false;
            if matches!(
                entry.state,
                DraftEntryState::Canceled | DraftEntryState::Expired
            ) {
                entry.refresh_terminal_reservation();
            } else if matches!(entry.state, DraftEntryState::Preparing) {
                entry.state = DraftEntryState::Failed(error);
                entry.refresh_terminal_reservation();
            } else if matches!(entry.state, DraftEntryState::Failed(_)) {
                entry.refresh_terminal_reservation();
            }
            entry.notify.notify_waiters();
        }
        store.enforce_total_bytes(self.limits.max_total_bytes);
    }

    fn finish_canceled(&self, draft_id: &str) {
        let mut store = self.lock_store();
        if let Some(entry) = store.entries.get_mut(draft_id) {
            entry.generation_in_flight = false;
            if matches!(entry.state, DraftEntryState::Preparing) {
                entry.state = DraftEntryState::Canceled;
            }
            if matches!(
                entry.state,
                DraftEntryState::Failed(_) | DraftEntryState::Canceled | DraftEntryState::Expired
            ) {
                entry.refresh_terminal_reservation();
            }
            entry.notify.notify_waiters();
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct CapturedRange {
    request_id: String,
    source_range: StoredMessageRange,
    boundary_expansions: Vec<jcode_session_types::StoredRangeBoundaryExpansion>,
    changed_files: Vec<String>,
    change_evidence_complete: bool,
    change_evidence_warnings: Vec<String>,
    source_token_estimate: usize,
}

#[derive(Serialize)]
struct CapturedContextDraft {
    identity: ContextDraftIdentity,
    authorization: StoredContextAuthorization,
    messages: Vec<StoredMessage>,
    base_context_view: StoredContextViewState,
    ranges: Vec<ContextCuratorRangeWork>,
    range_metadata: Vec<CapturedRange>,
    reasoning: Option<StoredReasoningSuppression>,
    tools: Vec<ContextCuratorToolWork>,
    active_summary_texts: Vec<String>,
    notices: Vec<String>,
}

fn capture_context_draft(
    agent: &Agent,
    identity: ContextDraftIdentity,
    request: ContextDraftRequest,
) -> Result<CapturedContextDraft, ContextServiceError> {
    let messages = agent.messages().to_vec();
    let base_context_view = agent.context_view_state().clone();
    validate_context_state(&base_context_view)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let message_indices = unique_message_indices(&messages)?;
    let requested_ranges = request
        .summary_ranges
        .iter()
        .map(|range| {
            Ok((
                *message_indices
                    .get(&range.start_message_id)
                    .ok_or_else(|| {
                        ContextServiceError::InvalidSelection(format!(
                            "range start message not found: {}",
                            range.start_message_id
                        ))
                    })?,
                *message_indices.get(&range.end_message_id).ok_or_else(|| {
                    ContextServiceError::InvalidSelection(format!(
                        "range end message not found: {}",
                        range.end_message_id
                    ))
                })?,
            ))
        })
        .collect::<Result<Vec<_>, ContextServiceError>>()?;
    let closed_ranges = if requested_ranges.is_empty() {
        Vec::new()
    } else {
        close_message_ranges(&messages, &base_context_view, &requested_ranges)
            .map_err(|error| ContextServiceError::Conflict(error.to_string()))?
    };
    reject_active_summary_overlap(&messages, &base_context_view, &closed_ranges)?;
    let shadowed = active_block_operations_shadowed(&messages, &base_context_view, &closed_ranges)?;
    if !shadowed.is_empty() && !request.allow_shadowing_active_operations {
        return Err(ContextServiceError::Conflict(format!(
            "selected summaries would shadow active operations: {}",
            shadowed.join(", ")
        )));
    }
    let summary_intervals = closed_ranges
        .iter()
        .map(|range| (range.start, range.end))
        .collect::<Vec<_>>();
    let mut range_metadata = Vec::new();
    let mut ranges = Vec::new();
    for (index, closed) in closed_ranges.iter().enumerate() {
        let source_range = closed
            .to_stored_range(&messages)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        let evidence = extract_context_change_evidence(&messages, &source_range)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        let source_token_estimate = messages[closed.start..=closed.end]
            .iter()
            .map(|message| estimate_message_tokens(&message.to_message()))
            .fold(0usize, usize::saturating_add);
        let request_id = format!("range-{}", index + 1);
        ranges.push(ContextCuratorRangeWork {
            request_id: request_id.clone(),
            source_range: source_range.clone(),
            changed_files: evidence.changed_files.clone(),
            change_evidence_complete: evidence.complete,
            change_evidence_warnings: evidence.warnings.clone(),
        });
        range_metadata.push(CapturedRange {
            request_id,
            source_range,
            boundary_expansions: closed.expansions.clone(),
            changed_files: evidence.changed_files,
            change_evidence_complete: evidence.complete,
            change_evidence_warnings: evidence.warnings,
            source_token_estimate,
        });
    }

    let reasoning = match request.reasoning {
        Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns,
        }) => Some(
            resolve_reasoning_suppression_keep_latest(&messages, protected_recent_assistant_turns)
                .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?,
        ),
        Some(ContextReasoningSelectionRequest::MessageRanges { ranges }) => {
            let ranges = ranges
                .iter()
                .map(|range| {
                    let start = *message_indices
                        .get(&range.start_message_id)
                        .ok_or_else(|| {
                            ContextServiceError::InvalidSelection(format!(
                                "reasoning range start message not found: {}",
                                range.start_message_id
                            ))
                        })?;
                    let end = *message_indices.get(&range.end_message_id).ok_or_else(|| {
                        ContextServiceError::InvalidSelection(format!(
                            "reasoning range end message not found: {}",
                            range.end_message_id
                        ))
                    })?;
                    build_message_range(&messages, start, end)
                        .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))
                })
                .collect::<Result<Vec<_>, ContextServiceError>>()?;
            Some(
                resolve_reasoning_suppression_for_ranges(&messages, &ranges)
                    .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?,
            )
        }
        None => None,
    };
    let (reasoning, omitted_reasoning_targets) = match reasoning {
        Some(suppression) => {
            let (suppression, omitted) =
                filter_shadowed_reasoning(&messages, suppression, &summary_intervals)?;
            (Some(suppression), omitted)
        }
        None => (None, 0),
    };

    let mut notices = shadowed
        .into_iter()
        .map(|operation| format!("Selected summaries shadow active operation {operation}."))
        .collect::<Vec<_>>();
    if omitted_reasoning_targets > 0 {
        notices.push(format!(
            "Selected summaries already replace {omitted_reasoning_targets} staged replayed-reasoning block target(s); those targets were omitted."
        ));
    }
    let mut tools = Vec::new();
    let mut seen_targets = BTreeSet::new();
    for (index, selection) in request.tool_results.iter().enumerate() {
        let message_index = *message_indices.get(&selection.message_id).ok_or_else(|| {
            ContextServiceError::InvalidSelection(format!(
                "tool-result message not found: {}",
                selection.message_id
            ))
        })?;
        if interval_contains(&summary_intervals, message_index) {
            notices.push(format!(
                "Tool result {} block {} is inside a selected summary range and was omitted.",
                selection.message_id, selection.block_ordinal
            ));
            continue;
        }
        let target = build_content_target(&messages, message_index, selection.block_ordinal)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        if !seen_targets.insert((target.message_id.clone(), target.expected_hash)) {
            return Err(ContextServiceError::InvalidSelection(format!(
                "tool-result target was selected more than once: {} block {}",
                selection.message_id, selection.block_ordinal
            )));
        }
        let block = messages[message_index]
            .content
            .get(selection.block_ordinal)
            .ok_or_else(|| {
                ContextServiceError::InvalidSelection(format!(
                    "tool-result block is out of bounds: {} block {}",
                    selection.message_id, selection.block_ordinal
                ))
            })?;
        let ContentBlock::ToolResult {
            tool_use_id,
            content: _,
            is_error,
        } = block
        else {
            return Err(ContextServiceError::InvalidSelection(format!(
                "selected block is not a ToolResult: {} block {}",
                selection.message_id, selection.block_ordinal
            )));
        };
        let (tool_name, tool_input) = find_unique_tool_call(&messages, tool_use_id)?;
        tools.push(ContextCuratorToolWork {
            request_id: format!("tool-{}", index + 1),
            target,
            message_index,
            tool_name,
            tool_call_id: tool_use_id.clone(),
            tool_input,
            is_error: *is_error,
            original_token_estimate: estimate_content_block_tokens(block),
        });
    }
    let reasoning_has_targets = reasoning
        .as_ref()
        .is_some_and(|suppression| !suppression.targets.is_empty());
    if range_metadata.is_empty() && tools.is_empty() && !reasoning_has_targets {
        return Err(ContextServiceError::EmptyRequest);
    }

    let active_summary_texts = base_context_view
        .active_transactions()
        .flat_map(|transaction| transaction.operations.iter())
        .filter_map(|operation| match operation {
            StoredContextOperation::RangeSummary(summary) => Some(summary.summary_text.clone()),
            _ => None,
        })
        .collect();
    Ok(CapturedContextDraft {
        identity,
        authorization: request.authorization,
        messages,
        base_context_view,
        ranges,
        range_metadata,
        reasoning,
        tools,
        active_summary_texts,
        notices,
    })
}

fn unique_message_indices(
    messages: &[StoredMessage],
) -> Result<HashMap<String, usize>, ContextServiceError> {
    let mut indices = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if indices.insert(message.id.clone(), index).is_some() {
            return Err(ContextServiceError::InvalidSelection(format!(
                "duplicate stored message ID: {}",
                message.id
            )));
        }
    }
    Ok(indices)
}

fn reject_active_summary_overlap(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    ranges: &[jcode_context_core::ClosedMessageRange],
) -> Result<(), ContextServiceError> {
    let target_index = ContextTargetIndex::new(messages);
    for transaction in state.active_transactions() {
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            let StoredContextOperation::RangeSummary(summary) = operation else {
                continue;
            };
            let (start, end) = target_index
                .resolve_message_range(&summary.source_range)
                .map_err(|error| ContextServiceError::Stale(error.to_string()))?;
            if ranges
                .iter()
                .any(|range| range.start <= end && start <= range.end)
            {
                return Err(ContextServiceError::Conflict(format!(
                    "selected range overlaps active summary {} operation {}",
                    transaction.id, operation_index
                )));
            }
        }
    }
    Ok(())
}

fn active_block_operations_shadowed(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    ranges: &[jcode_context_core::ClosedMessageRange],
) -> Result<Vec<String>, ContextServiceError> {
    let target_index = ContextTargetIndex::new(messages);
    let mut shadowed = BTreeSet::new();
    for transaction in state.active_transactions() {
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            let targets = match operation {
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    suppression.targets.iter().collect::<Vec<_>>()
                }
                StoredContextOperation::ToolResultDistillation(distillation) => {
                    vec![&distillation.target]
                }
                StoredContextOperation::RangeSummary(_) => continue,
            };
            for target in targets {
                let resolved = target_index
                    .resolve_content_target(target)
                    .map_err(|error| ContextServiceError::Stale(error.to_string()))?;
                if ranges.iter().any(|range| {
                    range.start <= resolved.message_index && resolved.message_index <= range.end
                }) {
                    shadowed.insert(format!("{}:{}", transaction.id, operation_index));
                }
            }
        }
    }
    Ok(shadowed.into_iter().collect())
}

fn filter_shadowed_reasoning(
    messages: &[StoredMessage],
    mut suppression: StoredReasoningSuppression,
    summary_intervals: &[(usize, usize)],
) -> Result<(StoredReasoningSuppression, usize), ContextServiceError> {
    let target_index = ContextTargetIndex::new(messages);
    let original_target_count = suppression.targets.len();
    let mut retained = Vec::new();
    let mut turns = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut tokens = 0usize;
    for target in suppression.targets {
        let resolved = target_index
            .resolve_content_target(&target)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        if interval_contains(summary_intervals, resolved.message_index) {
            continue;
        }
        turns.insert(resolved.message_index);
        kinds.insert(target.kind);
        tokens = tokens.saturating_add(estimate_content_block_tokens(
            &messages[resolved.message_index].content[resolved.block_index],
        ));
        retained.push(target);
    }
    suppression.targets = retained;
    suppression.assistant_turns_affected = turns.len();
    suppression.replay_block_kinds = kinds.into_iter().collect();
    suppression.original_token_estimate = tokens;
    let omitted = original_target_count.saturating_sub(suppression.targets.len());
    Ok((suppression, omitted))
}

fn interval_contains(intervals: &[(usize, usize)], index: usize) -> bool {
    intervals
        .iter()
        .any(|(start, end)| *start <= index && index <= *end)
}

fn find_unique_tool_call(
    messages: &[StoredMessage],
    tool_use_id: &str,
) -> Result<(String, serde_json::Value), ContextServiceError> {
    let matches = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                id, name, input, ..
            } if id == tool_use_id => Some((name.clone(), input.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [found] => Ok(found.clone()),
        [] => Err(ContextServiceError::InvalidSelection(format!(
            "matching ToolUse not found for result {tool_use_id}"
        ))),
        _ => Err(ContextServiceError::InvalidSelection(format!(
            "matching ToolUse is ambiguous for result {tool_use_id}"
        ))),
    }
}

struct DraftBuildFailure {
    draft_id: String,
    error: ContextServiceError,
}

fn build_ready_draft(
    agent: &Agent,
    capture: CapturedContextDraft,
    route: Option<ContextCuratorRoute>,
    artifacts: ContextCuratorArtifacts,
) -> Result<ContextDraft, DraftBuildFailure> {
    let draft_id = capture.identity.draft_id.clone();
    build_ready_draft_inner(agent, capture, route, artifacts)
        .map_err(|error| DraftBuildFailure { draft_id, error })
}

fn build_ready_draft_inner(
    agent: &Agent,
    capture: CapturedContextDraft,
    route: Option<ContextCuratorRoute>,
    artifacts: ContextCuratorArtifacts,
) -> Result<ContextDraft, ContextServiceError> {
    let generator = match route.as_ref() {
        Some(route) => Some(route.generator()),
        None if capture.range_metadata.is_empty() && capture.tools.is_empty() => None,
        None => {
            return Err(ContextServiceError::Curator(
                "generated context artifacts have no independent curator route identity"
                    .to_string(),
            ));
        }
    };
    let now = Utc::now();
    let mut required_operations = Vec::new();
    for metadata in &capture.range_metadata {
        let artifact = artifacts
            .range_summaries
            .get(&metadata.request_id)
            .ok_or_else(|| {
                ContextServiceError::Curator(format!(
                    "missing generated range artifact {}",
                    metadata.request_id
                ))
            })?;
        let mut warnings = metadata.change_evidence_warnings.clone();
        warnings.extend(artifact.warnings.clone());
        required_operations.push(StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range: metadata.source_range.clone(),
            summary_text: artifact.summary.clone(),
            file_change_digest: artifact.file_change_digest.clone(),
            changed_files: metadata.changed_files.clone(),
            change_evidence_complete: metadata.change_evidence_complete,
            boundary_expansions: metadata.boundary_expansions.clone(),
            generator: generator.clone(),
            source_token_estimate: metadata.source_token_estimate,
            replacement_token_estimate: 0,
            warnings,
            created_at: now,
            legacy_coverage: None,
        }));
    }
    if let Some(reasoning) = capture.reasoning.clone()
        && !reasoning.targets.is_empty()
    {
        required_operations.push(StoredContextOperation::ReasoningSuppression(reasoning));
    }

    let tool_by_id = capture
        .tools
        .iter()
        .map(|tool| (tool.request_id.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    let mut proposals = Vec::new();
    let mut ineligible = Vec::new();
    for (request_id, artifact) in artifacts.tool_distillations {
        let work = tool_by_id.get(request_id.as_str()).ok_or_else(|| {
            ContextServiceError::Curator(format!("unknown tool artifact {request_id}"))
        })?;
        match artifact {
            ContextCuratorToolArtifact::Eligible {
                replacement,
                replacement_token_estimate,
                preservation_rationale,
                uncertainties,
            } => {
                let ratio = ((replacement_token_estimate as u128).saturating_mul(1_000_000)
                    / work.original_token_estimate.max(1) as u128)
                    .min(u32::MAX as u128) as u32;
                let operation = StoredToolResultDistillation {
                    target: work.target.clone(),
                    tool_name: work.tool_name.clone(),
                    tool_call_id: work.tool_call_id.clone(),
                    replacement_content: replacement,
                    original_token_estimate: work.original_token_estimate,
                    replacement_token_estimate,
                    replacement_ratio_millionths: ratio,
                    preservation_rationale,
                    uncertainties,
                    generator: generator.clone().ok_or_else(|| {
                        ContextServiceError::Curator(format!(
                            "tool artifact {request_id} has no independent curator route identity"
                        ))
                    })?,
                    created_at: now,
                };
                if !operation.is_strictly_below_percent(20) {
                    return Err(ContextServiceError::Curator(format!(
                        "tool artifact {request_id} failed the strict below-20-percent gate"
                    )));
                }
                proposals.push(ContextDistillationProposal {
                    proposal_id: request_id,
                    selected_by_default: true,
                    operation,
                });
            }
            ContextCuratorToolArtifact::Ineligible {
                reason,
                uncertainties,
            } => ineligible.push(ContextIneligibleDistillation {
                request_id,
                tool_name: work.tool_name.clone(),
                tool_call_id: work.tool_call_id.clone(),
                reason,
                uncertainties,
            }),
        }
    }

    let mut operations = required_operations.clone();
    operations.extend(proposals.iter().map(|proposal| {
        StoredContextOperation::ToolResultDistillation(proposal.operation.clone())
    }));
    let proposed_revision = capture
        .base_context_view
        .revision
        .checked_add(1)
        .ok_or(ContextServiceError::RevisionOverflow)?;
    fill_range_replacement_estimates(
        &capture.messages,
        &capture.base_context_view,
        &capture.identity.draft_id,
        proposed_revision,
        capture.authorization.clone(),
        &mut operations,
    )?;
    copy_filled_range_estimates(&operations, &mut required_operations);
    let provider = agent.provider_handle();
    let pricing = crate::provider::pricing::context_pricing_snapshot(
        &provider.model(),
        &provider.display_name(),
        &capture.identity.route,
        jcode_session_types::StoredContextCacheWarmth::Unknown,
    );
    let preview = build_preview(ContextDraftPreviewInput {
        provider: provider.as_ref(),
        messages: &capture.messages,
        base_state: &capture.base_context_view,
        transaction_id: &capture.identity.draft_id,
        proposed_revision,
        authorization: capture.authorization.clone(),
        operations: &operations,
        pricing: Some(&pricing),
        estimated_total_request_tokens_before: agent.current_context_request_token_estimate(),
        notices: capture.notices,
        ranges: &capture.range_metadata,
        proposals: &proposals,
    })?;
    Ok(ContextDraft {
        identity: capture.identity,
        authorization: capture.authorization,
        required_operations,
        distillation_proposals: proposals,
        ineligible_distillations: ineligible,
        preview,
        curator_usage: artifacts.usage.into_iter().collect(),
    })
}

fn fill_range_replacement_estimates(
    messages: &[StoredMessage],
    base_state: &StoredContextViewState,
    transaction_id: &str,
    revision: u64,
    authorization: StoredContextAuthorization,
    operations: &mut [StoredContextOperation],
) -> Result<(), ContextServiceError> {
    let state = state_with_transaction(
        base_state,
        transaction_id,
        revision,
        authorization,
        operations.to_vec(),
        None,
        Vec::new(),
    );
    let projection = project_context(messages, &state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    for (message, source) in projection.messages.iter().zip(&projection.sources) {
        let jcode_context_core::ProjectedMessageSource::RangeSummary { operation, .. } = source
        else {
            continue;
        };
        if operation.transaction_id != transaction_id {
            continue;
        }
        if let Some(StoredContextOperation::RangeSummary(summary)) =
            operations.get_mut(operation.operation_index)
        {
            summary.replacement_token_estimate = estimate_message_tokens(message);
        }
    }
    Ok(())
}

fn copy_filled_range_estimates(
    all_operations: &[StoredContextOperation],
    required_operations: &mut [StoredContextOperation],
) {
    for (source, destination) in all_operations.iter().zip(required_operations.iter_mut()) {
        if let (
            StoredContextOperation::RangeSummary(source),
            StoredContextOperation::RangeSummary(destination),
        ) = (source, destination)
        {
            destination.replacement_token_estimate = source.replacement_token_estimate;
        }
    }
}

pub(crate) struct ContextDraftPreviewInput<'a> {
    pub(crate) provider: &'a dyn crate::provider::Provider,
    pub(crate) messages: &'a [StoredMessage],
    pub(crate) base_state: &'a StoredContextViewState,
    pub(crate) transaction_id: &'a str,
    pub(crate) proposed_revision: u64,
    pub(crate) authorization: StoredContextAuthorization,
    pub(crate) operations: &'a [StoredContextOperation],
    pub(crate) pricing: Option<&'a jcode_session_types::StoredContextPricingSnapshot>,
    pub(crate) estimated_total_request_tokens_before: Option<usize>,
    pub(crate) notices: Vec<String>,
    pub(crate) ranges: &'a [CapturedRange],
    pub(crate) proposals: &'a [ContextDistillationProposal],
}

pub(crate) fn build_preview(
    input: ContextDraftPreviewInput<'_>,
) -> Result<ContextDraftPreview, ContextServiceError> {
    let ContextDraftPreviewInput {
        provider,
        messages,
        base_state,
        transaction_id,
        proposed_revision,
        authorization,
        operations,
        pricing,
        estimated_total_request_tokens_before,
        notices,
        ranges,
        proposals,
    } = input;
    let before = project_context(messages, base_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let proposed_state = state_with_transaction(
        base_state,
        transaction_id,
        proposed_revision,
        authorization,
        operations.to_vec(),
        None,
        Vec::new(),
    );
    validate_context_state(&proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let after = project_context(messages, &proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let validation_operations = projection_validation_operations(&proposed_state);
    let validation =
        require_supported_projected_messages(provider, &after.messages, &validation_operations)
            .map_err(|error| ContextServiceError::ProviderValidation(error.to_string()))?;
    let analysis = analyze_cache_prefix(&before.messages, &after.messages);
    let estimated_total_request_tokens_after = estimated_total_request_tokens_before
        .and_then(|total| total.checked_sub(analysis.old_total_tokens))
        .map(|non_message_tokens| non_message_tokens.saturating_add(analysis.new_total_tokens));
    let economics = calculate_context_economics(ContextEconomicsInput {
        analysis: &analysis,
        estimated_total_request_tokens_before,
        estimated_total_request_tokens_after,
        context_window: Some(provider.context_window()),
        safe_input_budget: None,
        pricing,
        resulting_suffix_cacheable: after.diagnostics.projected_provider_token_estimate >= 1_024,
    });
    let range_by_start = ranges
        .iter()
        .map(|range| (range.source_range.start_message_id.as_str(), range))
        .collect::<BTreeMap<_, _>>();
    let proposal_by_target = proposals
        .iter()
        .map(|proposal| {
            (
                (
                    proposal.operation.target.message_id.as_str(),
                    proposal.operation.target.expected_hash,
                ),
                proposal,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let operation_previews = operations
        .iter()
        .map(|operation| match operation {
            StoredContextOperation::RangeSummary(summary) => {
                let metadata = range_by_start
                    .get(summary.source_range.start_message_id.as_str())
                    .copied();
                ContextOperationPreview::RangeSummary {
                    request_id: metadata
                        .map(|metadata| metadata.request_id.clone())
                        .unwrap_or_default(),
                    source_range: summary.source_range.clone(),
                    source_tokens: summary.source_token_estimate,
                    replacement_tokens: summary.replacement_token_estimate,
                    changed_files: summary.changed_files.clone(),
                    change_evidence_complete: summary.change_evidence_complete,
                }
            }
            StoredContextOperation::ReasoningSuppression(suppression) => {
                ContextOperationPreview::ReasoningSuppression {
                    target_count: suppression.targets.len(),
                    assistant_turns_affected: suppression.assistant_turns_affected,
                    replay_block_kinds: suppression.replay_block_kinds.clone(),
                    removed_tokens: suppression.original_token_estimate,
                }
            }
            StoredContextOperation::ToolResultDistillation(distillation) => {
                let proposal = proposal_by_target
                    .get(&(
                        distillation.target.message_id.as_str(),
                        distillation.target.expected_hash,
                    ))
                    .copied();
                ContextOperationPreview::ToolResultDistillation {
                    proposal_id: proposal
                        .map(|proposal| proposal.proposal_id.clone())
                        .unwrap_or_default(),
                    tool_name: distillation.tool_name.clone(),
                    tool_call_id: distillation.tool_call_id.clone(),
                    original_tokens: distillation.original_token_estimate,
                    replacement_tokens: distillation.replacement_token_estimate,
                    selected_by_default: proposal
                        .is_some_and(|proposal| proposal.selected_by_default),
                }
            }
        })
        .collect();
    Ok(ContextDraftPreview {
        raw_stored_message_count: messages.len(),
        current_context_revision: base_state.revision,
        proposed_context_revision: proposed_revision,
        economics,
        formatter_placeholder_count: validation.formatter_placeholder_count,
        validation,
        operation_previews,
        notices,
    })
}

pub(crate) fn state_with_transaction(
    base_state: &StoredContextViewState,
    transaction_id: &str,
    revision: u64,
    authorization: StoredContextAuthorization,
    operations: Vec<StoredContextOperation>,
    economics: Option<StoredContextEconomics>,
    curator_usage: Vec<StoredContextCuratorUsage>,
) -> StoredContextViewState {
    let mut state = base_state.clone();
    state.revision = revision;
    state.transactions.push(StoredContextTransaction {
        id: transaction_id.to_string(),
        base_revision: base_state.revision,
        created_at: Utc::now(),
        authorization,
        operations,
        status_events: vec![StoredContextStatusEvent {
            revision,
            timestamp: Utc::now(),
            kind: StoredContextTransactionStatusKind::Applied,
            reason: None,
        }],
        application: None,
        economics,
        curator_usage,
    });
    state
}

pub(crate) fn projection_validation_operations(
    state: &StoredContextViewState,
) -> Vec<ContextProjectionValidationOperation> {
    let mut operations = Vec::new();
    for transaction in state.active_transactions() {
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            match operation {
                StoredContextOperation::RangeSummary(_) => {
                    operations.push(ContextProjectionValidationOperation {
                        id: format!("{}:{operation_index}:range", transaction.id),
                        kind: ContextProjectionOperationKind::RangeSummary,
                    });
                }
                StoredContextOperation::ToolResultDistillation(_) => {
                    operations.push(ContextProjectionValidationOperation {
                        id: format!("{}:{operation_index}:tool", transaction.id),
                        kind: ContextProjectionOperationKind::ToolResultDistillation,
                    });
                }
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    for block_kind in &suppression.replay_block_kinds {
                        let Some(block_kind) = provider_reasoning_kind(*block_kind) else {
                            continue;
                        };
                        operations.push(ContextProjectionValidationOperation {
                            id: format!(
                                "{}:{operation_index}:reasoning:{block_kind:?}",
                                transaction.id
                            ),
                            kind: ContextProjectionOperationKind::ReasoningSuppression {
                                block_kind,
                            },
                        });
                    }
                }
            }
        }
    }
    operations
}

fn provider_reasoning_kind(kind: StoredContextBlockKind) -> Option<ContextReasoningBlockKind> {
    match kind {
        StoredContextBlockKind::Reasoning => Some(ContextReasoningBlockKind::GenericReasoning),
        StoredContextBlockKind::ReasoningTrace => Some(ContextReasoningBlockKind::ReasoningTrace),
        StoredContextBlockKind::AnthropicThinking => {
            Some(ContextReasoningBlockKind::AnthropicThinking)
        }
        StoredContextBlockKind::OpenAiReasoning => Some(ContextReasoningBlockKind::OpenAiReasoning),
        _ => None,
    }
}

pub(crate) fn validate_capture_identity(
    agent: &Agent,
    identity: &ContextDraftIdentity,
) -> Result<(), ContextServiceError> {
    if agent.session_id() != identity.session_id {
        return Err(ContextServiceError::Stale("session ID changed".to_string()));
    }
    if agent.context_view_state().revision != identity.base_context_revision {
        return Err(ContextServiceError::Stale(format!(
            "context revision changed from {} to {}",
            identity.base_context_revision,
            agent.context_view_state().revision
        )));
    }
    if agent.messages().len() != identity.raw_message_count {
        return Err(ContextServiceError::Stale(format!(
            "raw message count changed from {} to {}",
            identity.raw_message_count,
            agent.messages().len()
        )));
    }
    if authoritative_transcript_digest(agent.messages()) != identity.transcript_digest {
        return Err(ContextServiceError::Stale(
            "authoritative transcript digest changed".to_string(),
        ));
    }
    let provider = agent.provider_handle();
    if provider.name() != identity.provider_name {
        return Err(ContextServiceError::Stale(format!(
            "provider changed from {} to {}",
            identity.provider_name,
            provider.name()
        )));
    }
    if provider.model() != identity.model {
        return Err(ContextServiceError::Stale(format!(
            "model changed from {} to {}",
            identity.model,
            provider.model()
        )));
    }
    if agent.context_route_identity() != identity.route {
        return Err(ContextServiceError::Stale(format!(
            "route changed from {} to {}",
            identity.route,
            agent.context_route_identity()
        )));
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct ContextDraftStore {
    pub(crate) entries: BTreeMap<String, ContextDraftEntry>,
}

impl ContextDraftStore {
    fn insert_preparing(
        &mut self,
        entry: ContextDraftEntry,
        limits: ContextServiceLimits,
    ) -> Result<(), ContextServiceError> {
        self.expire_entries(Utc::now());
        self.evict_terminal_until(limits.max_drafts.saturating_sub(1), limits.max_total_bytes);
        if self.entries.len() >= limits.max_drafts {
            return Err(ContextServiceError::Capacity(format!(
                "{} drafts are already retained",
                self.entries.len()
            )));
        }
        if entry.reserved_bytes > limits.max_total_bytes {
            return Err(ContextServiceError::Capacity(format!(
                "captured draft requires {} bytes, exceeding the {}-byte store bound",
                entry.reserved_bytes, limits.max_total_bytes
            )));
        }
        while self.total_bytes().saturating_add(entry.reserved_bytes) > limits.max_total_bytes {
            if !self.evict_oldest_terminal() {
                return Err(ContextServiceError::Capacity(format!(
                    "captured drafts require {} bytes and no terminal draft can be evicted",
                    self.total_bytes().saturating_add(entry.reserved_bytes)
                )));
            }
        }
        self.entries.insert(entry.identity.draft_id.clone(), entry);
        Ok(())
    }

    pub(crate) fn expire_entries(&mut self, now: DateTime<Utc>) {
        for entry in self.entries.values_mut() {
            if now < entry.identity.expires_at {
                continue;
            }
            match entry.state {
                DraftEntryState::Preparing => {
                    entry.cancellation.cancel();
                    entry.state = DraftEntryState::Expired;
                    entry.notify.notify_waiters();
                }
                DraftEntryState::Ready(_) => {
                    entry.state = DraftEntryState::Expired;
                    entry.refresh_terminal_reservation();
                    entry.notify.notify_waiters();
                }
                DraftEntryState::Applying(_)
                | DraftEntryState::Applied { .. }
                | DraftEntryState::Failed(_)
                | DraftEntryState::Canceled
                | DraftEntryState::Expired => {}
            }
        }
    }

    fn total_bytes(&self) -> usize {
        self.entries.values().fold(0usize, |total, entry| {
            total.saturating_add(entry.reserved_bytes)
        })
    }

    pub(crate) fn enforce_total_bytes(&mut self, max_total_bytes: usize) {
        while self.total_bytes() > max_total_bytes {
            if !self.evict_oldest_terminal() {
                break;
            }
        }
    }

    fn evict_terminal_until(&mut self, max_items: usize, max_total_bytes: usize) {
        while self.entries.len() > max_items || self.total_bytes() > max_total_bytes {
            if !self.evict_oldest_terminal() {
                break;
            }
        }
    }

    fn evict_oldest_terminal(&mut self) -> bool {
        let candidate = self
            .entries
            .values()
            .filter(|entry| entry.is_evictable())
            .min_by_key(|entry| entry.identity.created_at)
            .map(|entry| entry.identity.draft_id.clone());
        candidate.and_then(|id| self.entries.remove(&id)).is_some()
    }
}

pub(crate) struct ContextDraftEntry {
    pub(crate) identity: ContextDraftIdentity,
    pub(crate) progress: ContextDraftProgress,
    pub(crate) state: DraftEntryState,
    pub(crate) cancellation: CancellationToken,
    pub(crate) notify: Arc<Notify>,
    pub(crate) reserved_bytes: usize,
    pub(crate) generation_in_flight: bool,
}

impl ContextDraftEntry {
    pub(crate) fn public_status(&self) -> ContextDraftStatus {
        match &self.state {
            DraftEntryState::Preparing => ContextDraftStatus::Preparing {
                identity: self.identity.clone(),
                progress: self.progress.clone(),
            },
            DraftEntryState::Ready(draft) => ContextDraftStatus::Ready {
                draft: Box::new(draft.clone()),
            },
            DraftEntryState::Applying(_) => ContextDraftStatus::Applying {
                identity: self.identity.clone(),
            },
            DraftEntryState::Applied {
                transaction_id,
                revision,
            } => ContextDraftStatus::Applied {
                identity: self.identity.clone(),
                transaction_id: transaction_id.clone(),
                revision: *revision,
            },
            DraftEntryState::Failed(error) => ContextDraftStatus::Failed {
                identity: self.identity.clone(),
                error: error.clone(),
            },
            DraftEntryState::Canceled => ContextDraftStatus::Canceled {
                identity: self.identity.clone(),
            },
            DraftEntryState::Expired => ContextDraftStatus::Expired {
                identity: self.identity.clone(),
            },
        }
    }

    fn is_evictable(&self) -> bool {
        !self.generation_in_flight
            && matches!(
                self.state,
                DraftEntryState::Applied { .. }
                    | DraftEntryState::Failed(_)
                    | DraftEntryState::Canceled
                    | DraftEntryState::Expired
            )
    }

    pub(crate) fn refresh_terminal_reservation(&mut self) {
        debug_assert!(matches!(
            self.state,
            DraftEntryState::Applied { .. }
                | DraftEntryState::Failed(_)
                | DraftEntryState::Canceled
                | DraftEntryState::Expired
        ));
        self.reserved_bytes = serde_json::to_vec(&self.public_status())
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
            .max(TERMINAL_DRAFT_RESERVATION_FLOOR_BYTES);
    }
}

pub(crate) enum DraftEntryState {
    Preparing,
    Ready(ContextDraft),
    Applying(ContextDraft),
    Applied {
        transaction_id: String,
        revision: u64,
    },
    Failed(ContextServiceError),
    Canceled,
    Expired,
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::provider::{ContextProjectionValidationStatus, ContextProviderFamily};
    use jcode_session_types::{
        StoredContextBillingMode, StoredContextEconomics, StoredContextPricingSnapshot,
    };

    fn identity(id: &str, expires_at: DateTime<Utc>) -> ContextDraftIdentity {
        ContextDraftIdentity {
            draft_id: id.to_string(),
            session_id: "session".to_string(),
            base_context_revision: 0,
            raw_message_count: 0,
            transcript_digest: 0,
            provider_name: "provider".to_string(),
            model: "model".to_string(),
            route: "route".to_string(),
            created_at: Utc::now(),
            expires_at,
        }
    }

    fn draft(id: &str, expires_at: DateTime<Utc>) -> ContextDraft {
        ContextDraft {
            identity: identity(id, expires_at),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            required_operations: Vec::new(),
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview: ContextDraftPreview {
                raw_stored_message_count: 0,
                current_context_revision: 0,
                proposed_context_revision: 1,
                economics: StoredContextEconomics {
                    projected_tokens_before: 0,
                    projected_tokens_after: 0,
                    estimated_total_request_tokens_before: None,
                    estimated_total_request_tokens_after: None,
                    unchanged_prefix_items: 0,
                    earliest_changed_provider_item: None,
                    old_affected_suffix_tokens: 0,
                    new_affected_suffix_tokens: 0,
                    deleted_input_tokens: 0,
                    context_window: None,
                    safe_input_budget: None,
                    pricing: Some(StoredContextPricingSnapshot {
                        billing_mode: StoredContextBillingMode::Unknown,
                        input_usd_per_million: None,
                        output_usd_per_million: None,
                        cache_read_usd_per_million: None,
                        cache_write_usd_per_million: None,
                        input_price_tiers: Vec::new(),
                        cache_warmth: Default::default(),
                    }),
                    first_request_delta_usd: None,
                    recurring_savings_per_turn_usd: None,
                    break_even_turns: None,
                    assumptions: Vec::new(),
                },
                validation: ContextProjectionValidationReport {
                    provider_family: ContextProviderFamily::Unknown,
                    provider_name: "provider".to_string(),
                    provider_display_name: "Provider".to_string(),
                    model: "model".to_string(),
                    evidence_tag: "test".to_string(),
                    builder_status: ContextProjectionValidationStatus::Supported,
                    normalized_item_count: 0,
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

    fn entry(
        id: &str,
        state: DraftEntryState,
        expires_at: DateTime<Utc>,
        reserved_bytes: usize,
        generation_in_flight: bool,
    ) -> ContextDraftEntry {
        ContextDraftEntry {
            identity: identity(id, expires_at),
            progress: ContextDraftProgress {
                phase: ContextDraftPhase::Capturing,
                completed_items: 0,
                total_items: 1,
            },
            state,
            cancellation: CancellationToken::new(),
            notify: Arc::new(Notify::new()),
            reserved_bytes,
            generation_in_flight,
        }
    }

    #[test]
    fn terminal_reservation_accounts_for_the_complete_retained_status() {
        let mut failed = entry(
            "large-error",
            DraftEntryState::Failed(ContextServiceError::Runtime("x".repeat(8 * 1024))),
            Utc::now() + chrono::Duration::minutes(1),
            0,
            false,
        );

        failed.refresh_terminal_reservation();

        let serialized_status = serde_json::to_vec(&failed.public_status())
            .expect("terminal status must serialize")
            .len();
        assert!(serialized_status > TERMINAL_DRAFT_RESERVATION_FLOOR_BYTES);
        assert_eq!(failed.reserved_bytes, serialized_status);
    }

    #[test]
    fn store_bounds_never_evict_inflight_entries() {
        let mut store = ContextDraftStore::default();
        let limits = ContextServiceLimits {
            max_drafts: 1,
            max_total_bytes: 1_024,
            ..ContextServiceLimits::default()
        };
        store
            .insert_preparing(
                entry(
                    "first",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    600,
                    true,
                ),
                limits,
            )
            .expect("first preparing draft");
        let error = store
            .insert_preparing(
                entry(
                    "second",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    200,
                    true,
                ),
                limits,
            )
            .expect_err("in-flight entry cannot be evicted");
        assert!(matches!(error, ContextServiceError::Capacity(_)));
        assert!(store.entries.contains_key("first"));
    }

    #[test]
    fn expired_inflight_reservation_survives_until_late_result_is_suppressed() {
        let service = ContextTransactionService::new();
        service.lock_store().entries.insert(
            "expired".to_string(),
            entry(
                "expired",
                DraftEntryState::Preparing,
                Utc::now() - chrono::Duration::seconds(1),
                4_096,
                true,
            ),
        );
        {
            let mut store = service.lock_store();
            store.expire_entries(Utc::now());
            let entry = store.entries.get("expired").expect("expired entry");
            assert!(matches!(entry.state, DraftEntryState::Expired));
            assert_eq!(entry.reserved_bytes, 4_096);
            assert!(entry.generation_in_flight);
            assert!(!entry.is_evictable());
        }

        service.finish_ready(draft("expired", Utc::now()));

        let store = service.lock_store();
        let entry = store.entries.get("expired").expect("late result entry");
        assert!(matches!(entry.state, DraftEntryState::Expired));
        assert_eq!(entry.reserved_bytes, 512);
        assert!(!entry.generation_in_flight);
        assert!(entry.is_evictable());
    }

    #[test]
    fn applying_draft_does_not_expire_mid_commit() {
        let mut store = ContextDraftStore::default();
        store.entries.insert(
            "applying".to_string(),
            entry(
                "applying",
                DraftEntryState::Applying(draft("applying", Utc::now())),
                Utc::now() - chrono::Duration::seconds(1),
                1_024,
                false,
            ),
        );
        store.expire_entries(Utc::now());
        assert!(matches!(
            store.entries["applying"].state,
            DraftEntryState::Applying(_)
        ));
    }

    #[tokio::test]
    async fn wait_for_draft_cannot_miss_terminal_notification() {
        let service = Arc::new(ContextTransactionService::new());
        service.lock_store().entries.insert(
            "wait".to_string(),
            entry(
                "wait",
                DraftEntryState::Preparing,
                Utc::now() + chrono::Duration::minutes(1),
                512,
                true,
            ),
        );
        let waiter = {
            let service = Arc::clone(&service);
            tokio::spawn(
                async move { service.wait_for_draft("wait", Duration::from_secs(1)).await },
            )
        };
        tokio::task::yield_now().await;
        service.finish_failed("wait", ContextServiceError::Runtime("done".to_string()));
        assert!(matches!(
            waiter.await.expect("waiter task").expect("wait status"),
            ContextDraftStatus::Failed { .. }
        ));
    }

    #[test]
    fn byte_bound_and_zero_item_bound_reject_without_insertion() {
        let mut store = ContextDraftStore::default();
        let limits = ContextServiceLimits {
            max_drafts: 1,
            max_total_bytes: 100,
            ..ContextServiceLimits::default()
        };
        assert!(matches!(
            store.insert_preparing(
                entry(
                    "oversized",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    101,
                    true,
                ),
                limits,
            ),
            Err(ContextServiceError::Capacity(_))
        ));
        assert!(store.entries.is_empty());

        let zero_items = ContextServiceLimits {
            max_drafts: 0,
            max_total_bytes: 1_024,
            ..ContextServiceLimits::default()
        };
        assert!(matches!(
            store.insert_preparing(
                entry(
                    "zero",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    1,
                    true,
                ),
                zero_items,
            ),
            Err(ContextServiceError::Capacity(_))
        ));
        assert!(store.entries.is_empty());
    }

    #[test]
    fn terminal_eviction_is_oldest_first_and_inflight_terminal_entries_are_protected() {
        let mut store = ContextDraftStore::default();
        let mut oldest = entry(
            "oldest",
            DraftEntryState::Failed(ContextServiceError::Runtime("old".to_string())),
            Utc::now() + chrono::Duration::minutes(1),
            512,
            false,
        );
        oldest.identity.created_at = Utc::now() - chrono::Duration::minutes(2);
        let mut newest = entry(
            "newest",
            DraftEntryState::Failed(ContextServiceError::Runtime("new".to_string())),
            Utc::now() + chrono::Duration::minutes(1),
            512,
            false,
        );
        newest.identity.created_at = Utc::now() - chrono::Duration::minutes(1);
        store.entries.insert("oldest".to_string(), oldest);
        store.entries.insert("newest".to_string(), newest);
        store
            .insert_preparing(
                entry(
                    "incoming",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    128,
                    true,
                ),
                ContextServiceLimits {
                    max_drafts: 2,
                    max_total_bytes: 2_048,
                    ..ContextServiceLimits::default()
                },
            )
            .expect("oldest terminal eviction");
        assert!(!store.entries.contains_key("oldest"));
        assert!(store.entries.contains_key("newest"));
        assert!(store.entries.contains_key("incoming"));

        let mut protected = ContextDraftStore::default();
        protected.entries.insert(
            "canceled-inflight".to_string(),
            entry(
                "canceled-inflight",
                DraftEntryState::Canceled,
                Utc::now() + chrono::Duration::minutes(1),
                900,
                true,
            ),
        );
        assert!(matches!(
            protected.insert_preparing(
                entry(
                    "blocked",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    100,
                    true,
                ),
                ContextServiceLimits {
                    max_drafts: 1,
                    max_total_bytes: 1_024,
                    ..ContextServiceLimits::default()
                },
            ),
            Err(ContextServiceError::Capacity(_))
        ));
        assert!(protected.entries.contains_key("canceled-inflight"));
    }

    #[test]
    fn ready_cancel_and_expiry_release_bytes_while_applying_is_immutable() {
        let service = ContextTransactionService::new();
        service.lock_store().entries.insert(
            "ready".to_string(),
            entry(
                "ready",
                DraftEntryState::Ready(draft("ready", Utc::now() + chrono::Duration::minutes(1))),
                Utc::now() + chrono::Duration::minutes(1),
                4_096,
                false,
            ),
        );
        service.cancel_draft("ready").expect("cancel ready draft");
        {
            let store = service.lock_store();
            let ready = &store.entries["ready"];
            assert!(matches!(ready.state, DraftEntryState::Canceled));
            assert_eq!(ready.reserved_bytes, 512);
        }

        let mut store = ContextDraftStore::default();
        store.entries.insert(
            "expired-ready".to_string(),
            entry(
                "expired-ready",
                DraftEntryState::Ready(draft("expired-ready", Utc::now())),
                Utc::now() - chrono::Duration::seconds(1),
                8_192,
                false,
            ),
        );
        store.entries.insert(
            "applying".to_string(),
            entry(
                "applying",
                DraftEntryState::Applying(draft("applying", Utc::now())),
                Utc::now() - chrono::Duration::seconds(1),
                2_048,
                false,
            ),
        );
        store.expire_entries(Utc::now());
        assert!(matches!(
            store.entries["expired-ready"].state,
            DraftEntryState::Expired
        ));
        assert_eq!(store.entries["expired-ready"].reserved_bytes, 512);
        assert!(matches!(
            store.entries["applying"].state,
            DraftEntryState::Applying(_)
        ));

        let service = ContextTransactionService::new();
        service.lock_store().entries.insert(
            "applying".to_string(),
            entry(
                "applying",
                DraftEntryState::Applying(draft("applying", Utc::now())),
                Utc::now() + chrono::Duration::minutes(1),
                2_048,
                false,
            ),
        );
        assert!(matches!(
            service.cancel_draft("applying"),
            Err(ContextServiceError::DraftNotReady(_))
        ));
        assert!(matches!(
            service.lock_store().entries["applying"].state,
            DraftEntryState::Applying(_)
        ));
    }

    #[test]
    fn late_terminal_results_preserve_status_and_release_generation_reservations() {
        let service = ContextTransactionService::new();
        service.lock_store().entries.insert(
            "expired".to_string(),
            entry("expired", DraftEntryState::Expired, Utc::now(), 4_096, true),
        );
        service.finish_failed(
            "expired",
            ContextServiceError::Runtime("late failure".to_string()),
        );
        {
            let store = service.lock_store();
            let expired = &store.entries["expired"];
            assert!(matches!(expired.state, DraftEntryState::Expired));
            assert_eq!(expired.reserved_bytes, 512);
            assert!(!expired.generation_in_flight);
        }

        service.lock_store().entries.insert(
            "failed".to_string(),
            entry(
                "failed",
                DraftEntryState::Failed(ContextServiceError::Runtime("first".to_string())),
                Utc::now(),
                8_192,
                true,
            ),
        );
        service.finish_canceled("failed");
        let store = service.lock_store();
        let failed = &store.entries["failed"];
        assert!(matches!(
            failed.state,
            DraftEntryState::Failed(ContextServiceError::Runtime(ref reason)) if reason == "first"
        ));
        assert_eq!(failed.reserved_bytes, 512);
        assert!(!failed.generation_in_flight);
    }

    #[test]
    fn oversized_ready_result_fails_boundedly_and_saturating_reservations_never_overflow() {
        let limits = ContextServiceLimits {
            max_drafts: 2,
            max_total_bytes: 700,
            ..ContextServiceLimits::default()
        };
        let service = ContextTransactionService::with_persistence(
            limits,
            Arc::new(SessionContextPersistence),
        );
        service.lock_store().entries.insert(
            "large-ready".to_string(),
            entry(
                "large-ready",
                DraftEntryState::Preparing,
                Utc::now() + chrono::Duration::minutes(1),
                100,
                true,
            ),
        );
        service.finish_ready(draft(
            "large-ready",
            Utc::now() + chrono::Duration::minutes(1),
        ));
        {
            let store = service.lock_store();
            assert!(matches!(
                store.entries["large-ready"].state,
                DraftEntryState::Failed(ContextServiceError::Capacity(_))
            ));
            assert!(store.total_bytes() <= limits.max_total_bytes);
        }

        let mut store = ContextDraftStore::default();
        store.entries.insert(
            "saturated".to_string(),
            entry(
                "saturated",
                DraftEntryState::Preparing,
                Utc::now() + chrono::Duration::minutes(1),
                usize::MAX - 10,
                true,
            ),
        );
        assert!(matches!(
            store.insert_preparing(
                entry(
                    "overflow",
                    DraftEntryState::Preparing,
                    Utc::now() + chrono::Duration::minutes(1),
                    20,
                    true,
                ),
                ContextServiceLimits {
                    max_drafts: 2,
                    max_total_bytes: usize::MAX - 1,
                    ..ContextServiceLimits::default()
                },
            ),
            Err(ContextServiceError::Capacity(_))
        ));
        assert_eq!(store.entries.len(), 1);
    }

    #[tokio::test]
    async fn every_waiter_observes_the_same_terminal_transition() {
        let service = Arc::new(ContextTransactionService::new());
        service.lock_store().entries.insert(
            "multi-wait".to_string(),
            entry(
                "multi-wait",
                DraftEntryState::Preparing,
                Utc::now() + chrono::Duration::minutes(1),
                512,
                true,
            ),
        );
        let first = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .wait_for_draft("multi-wait", Duration::from_secs(1))
                    .await
            })
        };
        let second = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .wait_for_draft("multi-wait", Duration::from_secs(1))
                    .await
            })
        };
        tokio::task::yield_now().await;
        service.finish_failed(
            "multi-wait",
            ContextServiceError::Runtime("terminal".to_string()),
        );
        for result in [
            first.await.expect("first waiter"),
            second.await.expect("second waiter"),
        ] {
            assert!(matches!(
                result.expect("wait result"),
                ContextDraftStatus::Failed {
                    error: ContextServiceError::Runtime(ref reason),
                    ..
                } if reason == "terminal"
            ));
        }
    }
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;
    use crate::context::ContextPersistence;
    use crate::message::{Message, Role, StreamEvent, ToolDefinition};
    use crate::provider::{
        ContextProjectionValidationOperation, ContextProjectionValidationReport,
        ContextProviderFamily, ContextProviderValidationIdentity, ContextReasoningBlockKind,
        ContextRequestBuilderValidation, EventStream, Provider,
        context_projection_validation_report,
    };
    use crate::session::Session;
    use crate::tool::Registry;
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::{StreamExt, stream};
    use std::sync::Weak;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    const RAW_RESULT_SENTINEL: &str = "RAW_CURATOR_ONLY_RESULT_SENTINEL";
    const DISTILLED_RESULT: &str = "Distilled result: command succeeded and changed no files.";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ProviderInstance {
        Live,
        CuratorFork,
    }

    #[derive(Clone, Debug)]
    struct RecordedProviderCall {
        messages: Vec<Message>,
        system: String,
    }

    struct DraftProviderState {
        live_calls: Mutex<Vec<RecordedProviderCall>>,
        curator_calls: Mutex<Vec<RecordedProviderCall>>,
        model: Mutex<String>,
        changed_name: AtomicBool,
        gate_curator: AtomicBool,
        curator_started: Semaphore,
        curator_release: Semaphore,
        invalidations: AtomicUsize,
        cancel_before_ready: Mutex<Option<(Weak<ContextTransactionService>, String)>>,
    }

    impl DraftProviderState {
        fn new() -> Self {
            Self {
                live_calls: Mutex::new(Vec::new()),
                curator_calls: Mutex::new(Vec::new()),
                model: Mutex::new("draft-model".to_string()),
                changed_name: AtomicBool::new(false),
                gate_curator: AtomicBool::new(false),
                curator_started: Semaphore::new(0),
                curator_release: Semaphore::new(0),
                invalidations: AtomicUsize::new(0),
                cancel_before_ready: Mutex::new(None),
            }
        }
    }

    #[derive(Clone)]
    struct DraftProvider {
        state: Arc<DraftProviderState>,
        instance: ProviderInstance,
    }

    impl DraftProvider {
        fn new() -> Self {
            Self {
                state: Arc::new(DraftProviderState::new()),
                instance: ProviderInstance::Live,
            }
        }

        fn gate_curator(&self) {
            self.state.gate_curator.store(true, Ordering::SeqCst);
        }

        async fn wait_for_curator_start(&self) {
            self.state
                .curator_started
                .acquire()
                .await
                .expect("curator-start semaphore")
                .forget();
        }

        fn release_curator(&self) {
            self.state.curator_release.add_permits(1);
        }

        fn set_changed_name(&self, changed: bool) {
            self.state.changed_name.store(changed, Ordering::SeqCst);
        }

        fn set_test_model(&self, model: &str) {
            *self
                .state
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
        }

        fn cancel_after_generation_before_ready(
            &self,
            service: &Arc<ContextTransactionService>,
            draft_id: &str,
        ) {
            *self
                .state
                .cancel_before_ready
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((Arc::downgrade(service), draft_id.to_string()));
        }

        fn live_calls(&self) -> Vec<RecordedProviderCall> {
            self.state
                .live_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn curator_calls(&self) -> Vec<RecordedProviderCall> {
            self.state
                .curator_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    #[async_trait]
    impl Provider for DraftProvider {
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
            system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            let call = RecordedProviderCall {
                messages: messages.to_vec(),
                system: system.to_string(),
            };
            match self.instance {
                ProviderInstance::Live => {
                    self.state
                        .live_calls
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(call);
                    Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    })])))
                }
                ProviderInstance::CuratorFork => {
                    self.state
                        .curator_calls
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(call);
                    if self.state.gate_curator.load(Ordering::SeqCst) {
                        self.state.curator_started.add_permits(1);
                        self.state
                            .curator_release
                            .acquire()
                            .await
                            .expect("curator-release semaphore")
                            .forget();
                    }
                    let response = serde_json::to_string(&serde_json::json!({
                        "range_summaries": [],
                        "tool_distillations": [{
                            "request_id": "tool-1",
                            "eligible": true,
                            "replacement": DISTILLED_RESULT,
                            "preservation_rationale": "The exact success state and absence of file changes are preserved.",
                            "ineligible_reason": null,
                            "uncertainties": []
                        }]
                    }))?;
                    Ok(Box::pin(stream::iter(vec![
                        Ok(StreamEvent::TextDelta(response)),
                        Ok(StreamEvent::TokenUsage {
                            input_tokens: Some(120),
                            output_tokens: Some(30),
                            cache_read_input_tokens: Some(40),
                            cache_creation_input_tokens: Some(10),
                        }),
                        Ok(StreamEvent::MessageEnd {
                            stop_reason: Some("end_turn".to_string()),
                        }),
                    ])))
                }
            }
        }

        fn name(&self) -> &str {
            if self.state.changed_name.load(Ordering::SeqCst) {
                "draft-provider-changed"
            } else {
                "draft-provider"
            }
        }

        fn display_name(&self) -> String {
            "Draft Provider".to_string()
        }

        fn model(&self) -> String {
            self.state
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn set_model(&self, model: &str) -> Result<()> {
            self.set_test_model(model);
            Ok(())
        }

        fn context_window(&self) -> usize {
            372_000
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> ContextProjectionValidationReport {
            if let Some((service, draft_id)) = self
                .state
                .cancel_before_ready
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                && let Some(service) = service.upgrade()
            {
                service
                    .cancel_draft(&draft_id)
                    .expect("cancel generated draft before ready storage");
            }
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: ContextProviderFamily::OpenRouterCompatible,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "draft_orchestration_builder_v1".to_string(),
                },
                operations,
                Some(ContextReasoningBlockKind::GenericReasoning),
                Ok(ContextRequestBuilderValidation::new(messages.len())),
            )
        }

        fn invalidate_context_continuation(&self, _reason: &str) {
            self.state.invalidations.fetch_add(1, Ordering::SeqCst);
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self {
                state: Arc::clone(&self.state),
                instance: ProviderInstance::CuratorFork,
            })
        }
    }

    #[derive(Default)]
    struct NoopPersistence {
        calls: AtomicUsize,
    }

    impl NoopPersistence {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ContextPersistence for NoopPersistence {
        fn persist(&self, _agent: &mut Agent) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn stored(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            id: id.to_string(),
            role,
            content,
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn test_session() -> Session {
        let mut session = Session::create(None, None);
        session.append_stored_message(stored(
            "tool-call-message",
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "tool-call".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "printf success"}),
                thought_signature: Some("provider-state-must-survive".to_string()),
            }],
        ));
        session.append_stored_message(stored(
            "tool-result-message",
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "tool-call".to_string(),
                content: format!(
                    "{} {}",
                    RAW_RESULT_SENTINEL,
                    "verbose output ".repeat(2_000)
                ),
                is_error: Some(false),
            }],
        ));
        session.append_stored_message(stored(
            "trace-message",
            Role::Assistant,
            vec![ContentBlock::ReasoningTrace {
                text: "history-only trace one".to_string(),
            }],
        ));
        session
    }

    fn test_agent(provider: &DraftProvider) -> Arc<AsyncMutex<Agent>> {
        let provider: Arc<dyn Provider> = Arc::new(provider.clone());
        Arc::new(AsyncMutex::new(Agent::new_with_session(
            provider,
            Registry::empty(),
            test_session(),
            None,
        )))
    }

    fn request() -> ContextDraftRequest {
        ContextDraftRequest {
            summary_ranges: Vec::new(),
            reasoning: None,
            tool_results: vec![ContextToolResultSelection {
                message_id: "tool-result-message".to_string(),
                block_ordinal: 0,
            }],
            allow_shadowing_active_operations: false,
            authorization: StoredContextAuthorization::Manual {
                initiated_by: Some("orchestration-test".to_string()),
            },
        }
    }

    fn test_service(
        limits: ContextServiceLimits,
    ) -> (Arc<ContextTransactionService>, Arc<NoopPersistence>) {
        let persistence = Arc::new(NoopPersistence::default());
        (
            Arc::new(ContextTransactionService::with_persistence(
                limits,
                persistence.clone(),
            )),
            persistence,
        )
    }

    fn prepare(
        service: &Arc<ContextTransactionService>,
        agent: Arc<AsyncMutex<Agent>>,
    ) -> Result<String, ContextServiceError> {
        service.prepare_draft_with_curator_config(
            agent,
            request(),
            false,
            &crate::config::ContextCuratorConfig::default(),
        )
    }

    async fn ready_draft(
        service: &Arc<ContextTransactionService>,
        agent: Arc<AsyncMutex<Agent>>,
    ) -> (String, ContextDraft) {
        let draft_id = prepare(service, agent).expect("prepare draft");
        let status = service
            .wait_for_draft(&draft_id, Duration::from_secs(2))
            .await
            .expect("ready draft status");
        let ContextDraftStatus::Ready { draft } = status else {
            panic!("expected ready draft, got {status:?}");
        };
        (draft_id, *draft)
    }

    async fn wait_for_generation_release(service: &ContextTransactionService, draft_id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notify = {
                    let store = service.lock_store();
                    let entry = store.entries.get(draft_id).expect("retained draft entry");
                    if !entry.generation_in_flight {
                        return;
                    }
                    Arc::clone(&entry.notify)
                };
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let released = {
                    let store = service.lock_store();
                    !store
                        .entries
                        .get(draft_id)
                        .expect("retained draft entry")
                        .generation_in_flight
                };
                if released {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("draft generation reservation released");
    }

    #[tokio::test]
    async fn preparing_entry_reserves_only_the_owned_capture_bytes() {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());
        let draft_request = request();
        let draft_id = service
            .prepare_draft_with_curator_config(
                Arc::clone(&agent),
                draft_request.clone(),
                false,
                &crate::config::ContextCuratorConfig::default(),
            )
            .expect("prepare gated draft");
        provider.wait_for_curator_start().await;

        let (identity, reserved_bytes) = {
            let store = service.lock_store();
            let entry = store.entries.get(&draft_id).expect("preparing entry");
            (entry.identity.clone(), entry.reserved_bytes)
        };
        let expected_capture_bytes = {
            let guard = agent.lock().await;
            let capture = capture_context_draft(&guard, identity, draft_request)
                .expect("rebuild deterministic captured task state");
            serde_json::to_vec(&capture)
                .expect("serialize captured task state")
                .len()
        };

        service.cancel_draft(&draft_id).expect("cancel gated draft");
        provider.release_curator();
        wait_for_generation_release(&service, &draft_id).await;

        assert_eq!(reserved_bytes, expected_capture_bytes);
        let store = service.lock_store();
        let entry = store.entries.get(&draft_id).expect("canceled entry");
        assert!(matches!(entry.state, DraftEntryState::Canceled));
        assert_eq!(entry.reserved_bytes, 512);
    }

    #[tokio::test]
    async fn independent_fork_contains_all_curator_administration_and_ready_status_has_no_raw_capture()
     {
        let provider = DraftProvider::new();
        let agent = test_agent(&provider);
        let (service, persistence) = test_service(ContextServiceLimits::default());
        let (draft_id, draft) = ready_draft(&service, Arc::clone(&agent)).await;
        let estimated_request_before = draft
            .preview
            .economics
            .estimated_total_request_tokens_before
            .expect("draft must retain whole-request token evidence");
        let estimated_request_after = draft
            .preview
            .economics
            .estimated_total_request_tokens_after
            .expect("draft must derive the proposed whole-request estimate");
        assert!(estimated_request_before >= draft.preview.economics.projected_tokens_before);
        assert!(estimated_request_after >= draft.preview.economics.projected_tokens_after);
        let snapshot = {
            let mut guard = agent.lock().await;
            service
                .context_editor_snapshot(&mut guard, false)
                .expect("authoritative editor snapshot")
        };
        assert_eq!(snapshot.projected_request_tokens, estimated_request_before);

        assert_eq!(provider.live_calls().len(), 0);
        let curator_calls = provider.curator_calls();
        assert_eq!(curator_calls.len(), 1);
        assert!(
            curator_calls[0]
                .system
                .contains("user-authorized, reversible provider-context transaction")
        );
        let curator_messages = serde_json::to_string(&curator_calls[0].messages).expect("messages");
        assert!(curator_messages.contains("tool_distillation_requests"));
        assert!(curator_messages.contains(RAW_RESULT_SENTINEL));
        assert_eq!(draft.default_selected_distillation_ids(), vec!["tool-1"]);
        assert_eq!(draft.curator_usage.len(), 1);
        assert_eq!(draft.curator_usage[0].input_tokens, 120);
        assert_eq!(draft.curator_usage[0].output_tokens, 30);

        let status_json = serde_json::to_string(
            &service
                .draft_status(&draft_id)
                .expect("serialized ready status"),
        )
        .expect("status JSON");
        assert!(!status_json.contains(RAW_RESULT_SENTINEL));

        let applied = service
            .apply_draft(&agent, &draft_id, None, false)
            .expect("apply draft");
        assert_eq!(applied.revision, 1);
        assert_eq!(persistence.calls.load(Ordering::SeqCst), 1);
        let (provider_handle, projected) = {
            let mut guard = agent.lock().await;
            let transaction = &guard.context_view_state().transactions[0];
            assert_eq!(transaction.curator_usage.len(), 1);
            let economics = transaction
                .economics
                .as_ref()
                .expect("applied transaction economics");
            assert!(economics.estimated_total_request_tokens_before.is_some());
            assert!(economics.estimated_total_request_tokens_after.is_some());
            assert!(
                guard
                    .messages()
                    .iter()
                    .all(|message| message.token_usage.is_none())
            );
            let projected = guard.provider_messages().expect("projected coding request");
            (guard.provider_handle(), projected)
        };
        let projected_json = serde_json::to_string(&projected).expect("projected JSON");
        assert!(projected_json.contains(DISTILLED_RESULT));
        assert!(!projected_json.contains(RAW_RESULT_SENTINEL));
        let mut stream = provider_handle
            .complete(&projected, &[], "normal coding system prompt", None)
            .await
            .expect("normal coding request");
        while let Some(event) = stream.next().await {
            event.expect("normal coding event");
        }
        let live_calls = provider.live_calls();
        assert_eq!(live_calls.len(), 1);
        assert_eq!(live_calls[0].system, "normal coding system prompt");
        let live_json = serde_json::to_string(&live_calls[0].messages).expect("live messages");
        assert!(!live_json.contains("tool_distillation_requests"));
        assert!(!live_json.contains("context-curator-v1"));
        assert!(!live_json.contains(RAW_RESULT_SENTINEL));
    }

    #[tokio::test]
    async fn busy_before_prepare_and_before_apply_never_changes_draft_or_context_state() {
        let provider = DraftProvider::new();
        let agent = test_agent(&provider);
        let (service, persistence) = test_service(ContextServiceLimits::default());
        assert!(matches!(
            service.prepare_draft_with_curator_config(
                Arc::clone(&agent),
                request(),
                true,
                &crate::config::ContextCuratorConfig::default(),
            ),
            Err(ContextServiceError::SessionBusy)
        ));
        let guard = agent.lock().await;
        assert!(matches!(
            prepare(&service, Arc::clone(&agent)),
            Err(ContextServiceError::SessionBusy)
        ));
        drop(guard);

        let (draft_id, _) = ready_draft(&service, Arc::clone(&agent)).await;
        let guard = agent.lock().await;
        assert!(matches!(
            service.apply_draft(&agent, &draft_id, None, false),
            Err(ContextServiceError::SessionBusy)
        ));
        drop(guard);
        assert!(matches!(
            service.draft_status(&draft_id).expect("ready status"),
            ContextDraftStatus::Ready { .. }
        ));
        assert_eq!(persistence.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            agent.lock().await.context_view_state().revision,
            0,
            "busy apply must not mutate context state"
        );
    }

    #[derive(Clone, Copy)]
    enum StaleMutation {
        TranscriptAppend,
        ContextRevision,
        ProviderName,
        Model,
        Route,
    }

    async fn assert_gated_mutation_is_stale(mutation: StaleMutation, expected: &str) {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, persistence) = test_service(ContextServiceLimits::default());
        let draft_id = prepare(&service, Arc::clone(&agent)).expect("prepare gated draft");
        provider.wait_for_curator_start().await;
        match mutation {
            StaleMutation::TranscriptAppend => {
                agent.lock().await.add_message(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: "concurrent append".to_string(),
                        cache_control: None,
                    }],
                );
            }
            StaleMutation::ContextRevision => {
                let mut guard = agent.lock().await;
                let mut state = guard.context_view_state().clone();
                state.revision = 1;
                guard.replace_context_view_state(state);
            }
            StaleMutation::ProviderName => provider.set_changed_name(true),
            StaleMutation::Model => provider.set_test_model("changed-model"),
            StaleMutation::Route => agent
                .lock()
                .await
                .set_session_provider_key(Some("changed-route".to_string())),
        }
        provider.release_curator();
        let status = service
            .wait_for_draft(&draft_id, Duration::from_secs(2))
            .await
            .expect("terminal stale status");
        assert!(matches!(
            status,
            ContextDraftStatus::Failed {
                error: ContextServiceError::Stale(ref reason),
                ..
            } if reason.contains(expected)
        ));
        assert_eq!(persistence.calls.load(Ordering::SeqCst), 0);
        assert!(
            agent
                .lock()
                .await
                .context_view_state()
                .transactions
                .is_empty()
        );
    }

    #[tokio::test]
    async fn every_captured_identity_dimension_is_revalidated_after_generation() {
        assert_gated_mutation_is_stale(StaleMutation::TranscriptAppend, "raw message count").await;
        assert_gated_mutation_is_stale(StaleMutation::ContextRevision, "context revision").await;
        assert_gated_mutation_is_stale(StaleMutation::ProviderName, "provider changed").await;
        assert_gated_mutation_is_stale(StaleMutation::Model, "model changed").await;
        assert_gated_mutation_is_stale(StaleMutation::Route, "route changed").await;
    }

    #[test]
    fn history_only_same_count_mutation_is_stale_even_when_provider_replay_is_unchanged() {
        let provider = DraftProvider::new();
        let original_session = test_session();
        let mutated_session = {
            let mut session = original_session.clone();
            session.messages[2].content = vec![ContentBlock::ReasoningTrace {
                text: "history-only trace two".to_string(),
            }];
            session
        };
        let original_provider: Arc<dyn Provider> = Arc::new(provider.clone());
        let original =
            Agent::new_with_session(original_provider, Registry::empty(), original_session, None);
        let identity = ContextDraftIdentity {
            draft_id: "digest-stale".to_string(),
            session_id: original.session_id().to_string(),
            base_context_revision: original.context_view_state().revision,
            raw_message_count: original.messages().len(),
            transcript_digest: authoritative_transcript_digest(original.messages()),
            provider_name: original.provider_handle().name().to_string(),
            model: original.provider_handle().model(),
            route: original.context_route_identity(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(1),
        };
        let mutated_provider: Arc<dyn Provider> = Arc::new(provider);
        let mutated =
            Agent::new_with_session(mutated_provider, Registry::empty(), mutated_session, None);
        assert_eq!(mutated.messages().len(), identity.raw_message_count);
        assert!(matches!(
            validate_capture_identity(&mutated, &identity),
            Err(ContextServiceError::Stale(reason)) if reason.contains("transcript digest")
        ));
    }

    #[test]
    fn staged_reasoning_inside_a_selected_summary_is_omitted_with_an_explicit_notice() {
        let provider = DraftProvider::new();
        let provider_handle: Arc<dyn Provider> = Arc::new(provider.clone());
        let mut session = test_session();
        session.append_stored_message(stored(
            "reasoning-message",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "replayed reasoning inside the selected summary".to_string(),
                },
                ContentBlock::Text {
                    text: "visible answer".to_string(),
                    cache_control: None,
                },
            ],
        ));
        let agent = Agent::new_with_session(provider_handle, Registry::empty(), session, None);
        let identity = ContextDraftIdentity {
            draft_id: "reasoning-shadow-notice".to_string(),
            session_id: agent.session_id().to_string(),
            base_context_revision: agent.context_view_state().revision,
            raw_message_count: agent.messages().len(),
            transcript_digest: authoritative_transcript_digest(agent.messages()),
            provider_name: agent.provider_handle().name().to_string(),
            model: agent.provider_handle().model(),
            route: agent.context_route_identity(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(1),
        };
        let range = ContextMessageRangeSelection {
            start_message_id: "reasoning-message".to_string(),
            end_message_id: "reasoning-message".to_string(),
        };
        let capture = capture_context_draft(
            &agent,
            identity,
            ContextDraftRequest {
                summary_ranges: vec![range.clone()],
                reasoning: Some(ContextReasoningSelectionRequest::MessageRanges {
                    ranges: vec![range],
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
        )
        .expect("capture summary with overlapping staged reasoning");

        assert!(
            capture
                .reasoning
                .as_ref()
                .is_some_and(|suppression| suppression.targets.is_empty())
        );
        assert!(capture.notices.iter().any(|notice| {
            notice.contains("1 staged replayed-reasoning block target")
                && notice.contains("those targets were omitted")
        }));
    }

    #[tokio::test]
    async fn reasoning_only_draft_does_not_require_or_invoke_a_curator_route() {
        let provider = DraftProvider::new();
        let agent = test_agent(&provider);
        let reasoning_message_id = {
            let mut guard = agent.lock().await;
            guard.add_message(
                Role::Assistant,
                vec![
                    ContentBlock::Reasoning {
                        text: "replayed reasoning that can be suppressed directly".to_string(),
                    },
                    ContentBlock::Text {
                        text: "visible answer remains".to_string(),
                        cache_control: None,
                    },
                ],
            );
            guard
                .messages()
                .last()
                .expect("new reasoning message")
                .id
                .clone()
        };
        let (service, persistence) = test_service(ContextServiceLimits::default());
        let draft_id = service
            .prepare_draft_with_curator_config(
                Arc::clone(&agent),
                ContextDraftRequest {
                    summary_ranges: Vec::new(),
                    reasoning: Some(ContextReasoningSelectionRequest::MessageRanges {
                        ranges: vec![ContextMessageRangeSelection {
                            start_message_id: reasoning_message_id.clone(),
                            end_message_id: reasoning_message_id,
                        }],
                    }),
                    tool_results: Vec::new(),
                    allow_shadowing_active_operations: false,
                    authorization: StoredContextAuthorization::Manual { initiated_by: None },
                },
                false,
                &crate::config::ContextCuratorConfig {
                    provider: Some("intentionally-missing-curator-route".to_string()),
                    model: Some("intentionally-missing-curator-model".to_string()),
                    effort: Some("high".to_string()),
                },
            )
            .expect("reasoning-only draft must not resolve the unused curator route");
        let status = service
            .wait_for_draft(&draft_id, Duration::from_secs(2))
            .await
            .expect("reasoning-only ready status");
        let ContextDraftStatus::Ready { draft } = status else {
            panic!("expected ready reasoning-only draft, got {status:?}");
        };

        assert!(provider.curator_calls().is_empty());
        assert_eq!(persistence.calls(), 0);
        assert!(draft.curator_usage.is_empty());
        assert!(draft.distillation_proposals.is_empty());
        assert!(matches!(
            draft.required_operations.as_slice(),
            [StoredContextOperation::ReasoningSuppression(suppression)]
                if suppression.targets.len() == 1
        ));
    }

    #[tokio::test]
    async fn busy_agent_at_completion_and_cancellation_remain_terminal_without_late_ready_overwrite()
     {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());
        let draft_id = prepare(&service, Arc::clone(&agent)).expect("prepare gated draft");
        provider.wait_for_curator_start().await;
        let guard = agent.lock().await;
        provider.release_curator();
        let status = service
            .wait_for_draft(&draft_id, Duration::from_secs(2))
            .await
            .expect("busy completion status");
        assert!(matches!(
            status,
            ContextDraftStatus::Failed {
                error: ContextServiceError::SessionBusy,
                ..
            }
        ));
        drop(guard);

        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());
        let draft_id = prepare(&service, agent).expect("prepare cancelable draft");
        provider.wait_for_curator_start().await;
        service
            .cancel_draft(&draft_id)
            .expect("cancel preparing draft");
        provider.release_curator();
        wait_for_generation_release(&service, &draft_id).await;
        let store = service.lock_store();
        let entry = &store.entries[&draft_id];
        assert!(matches!(entry.state, DraftEntryState::Canceled));
        assert!(!entry.generation_in_flight);
        assert_eq!(entry.reserved_bytes, 512);
    }

    #[tokio::test]
    async fn cancellation_after_generation_before_ready_storage_is_terminal_and_bounded() {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());
        let draft_id = prepare(&service, Arc::clone(&agent)).expect("prepare gated draft");
        provider.wait_for_curator_start().await;
        provider.cancel_after_generation_before_ready(&service, &draft_id);
        provider.release_curator();

        let status = service
            .wait_for_draft(&draft_id, Duration::from_secs(2))
            .await
            .expect("terminal draft status");
        let ContextDraftStatus::Canceled { identity } = status else {
            panic!("expected canceled draft, got {status:?}");
        };
        assert_eq!(identity.draft_id, draft_id);
        wait_for_generation_release(&service, &draft_id).await;

        let store = service.lock_store();
        let entry = store
            .entries
            .get(&draft_id)
            .expect("retained canceled draft");
        assert!(matches!(entry.state, DraftEntryState::Canceled));
        assert_eq!(entry.reserved_bytes, 512);
        drop(store);
        assert!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .transactions
                .is_empty()
        );
    }

    #[tokio::test]
    async fn expired_draft_status_survives_reconnect_and_cannot_apply() {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let expected_session_id = agent
            .try_lock()
            .expect("idle agent")
            .session_id()
            .to_string();
        let limits = ContextServiceLimits {
            ttl: Duration::ZERO,
            ..ContextServiceLimits::default()
        };
        let (service, persistence) = test_service(limits);
        let draft_id = prepare(&service, Arc::clone(&agent)).expect("prepare expiring draft");
        provider.wait_for_curator_start().await;

        let first = service.draft_status(&draft_id).expect("expired status");
        let second = service
            .draft_status(&draft_id)
            .expect("reconnect-style expired status lookup");
        for status in [&first, &second] {
            let ContextDraftStatus::Expired { identity } = status else {
                panic!("expected expired draft, got {status:?}");
            };
            assert_eq!(identity.draft_id, draft_id);
            assert_eq!(identity.session_id, expected_session_id);
        }
        let status_json = serde_json::to_string(&second).expect("serialize expired status");
        assert!(!status_json.contains(RAW_RESULT_SENTINEL));

        provider.release_curator();
        wait_for_generation_release(&service, &draft_id).await;
        let store = service.lock_store();
        let entry = store
            .entries
            .get(&draft_id)
            .expect("retained expired draft");
        assert!(matches!(entry.state, DraftEntryState::Expired));
        assert_eq!(entry.reserved_bytes, 512);
        drop(store);

        assert!(matches!(
            service.apply_draft(&agent, &draft_id, None, false),
            Err(ContextServiceError::DraftExpired(id)) if id == draft_id
        ));
        assert_eq!(persistence.calls(), 0);
        assert!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .transactions
                .is_empty()
        );
    }
}
