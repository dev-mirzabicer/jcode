use crate::message::Role;
use crate::protocol::{
    ContextCuratorPlanPreview, ContextCuratorRangeInstructions, ContextCuratorRunConfig,
    ContextCuratorSelection, ContextDraft, ContextDraftPhase, ContextDraftProgress,
    ContextDraftRequest, ContextDraftSelectionPreview, ContextEditorMessage, ContextEditorSnapshot,
    ContextMessageDetail, ContextMessageDetailFormat, ContextMessageRangeSelection,
    ContextOperationBadgeKind, ContextRangeClosurePreview, ContextReasoningSelectionRequest,
    ContextRequestKind, ContextServiceError, ContextTextChunk, ContextToolResultSelection,
    ContextTransactionDetail, ContextTransactionSummary,
};
use crate::tui::app::context_protocol::{
    ContextClientDraftState, ContextProtocolState, ContextTransactionHistoryPage,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jcode_session_types::{
    StoredContextAuthorization, StoredContextBlockKind, StoredContextEmergencyPolicy,
    StoredContextOperation, StoredContextTransactionStatusKind, StoredRangeBoundaryExpansionReason,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[path = "context_editor/curator_workspace.rs"]
mod curator_workspace;
#[path = "context_editor_debug.rs"]
mod debug_fixtures;

use curator_workspace::{
    CuratorGenerationOutcome, CuratorInstructionScope, CuratorWorkspacePane,
    CuratorWorkspaceSection, CuratorWorkspaceState,
};
#[cfg(test)]
use curator_workspace::{CuratorHitTarget, CuratorPlanDetail, curator_task_detail_lines};

const DEFAULT_PAGE_SIZE: usize = 250;
const DEFAULT_DETAIL_CHARS: usize = 16_384;
const NARROW_WIDTH: u16 = 92;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEditorOpenMode {
    Edit,
    History,
    Restore,
    UndoLatest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEditorPhase {
    Loading,
    Editing,
    ConfirmRangeClosure,
    PreparingDraft,
    ReviewDraft,
    History,
    InspectTransaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEditorModal {
    Search,
    ReasoningMenu,
    ReasoningKeepLatestInput,
    ToolScan,
    EmergencyPolicyMenu,
    EmergencyPolicyInput,
    ApplyConfirmation,
    RevertConfirmation,
    ReapplyConfirmation,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEditorPane {
    History,
    Preview,
    Operations,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextEditorAction {
    LoadSnapshot {
        page_start: usize,
        page_size: usize,
    },
    LoadDetail {
        context_revision: u64,
        transcript_digest: u64,
        message_id: String,
        block_ordinal: usize,
        start_char: usize,
        max_chars: usize,
    },
    PreviewRanges {
        context_revision: u64,
        transcript_digest: u64,
        ranges: Vec<ContextMessageRangeSelection>,
    },
    PreviewCuratorPlan {
        context_revision: u64,
        transcript_digest: u64,
        request: ContextDraftRequest,
    },
    SaveCuratorDefault(ContextCuratorSelection),
    PrepareDraft(ContextDraftRequest),
    CancelDraft {
        draft_id: String,
    },
    MonitorDraft {
        draft_id: String,
    },
    PreviewDraftSelection {
        draft_id: String,
        selected_distillation_ids: Vec<String>,
    },
    ApplyDraft {
        draft_id: String,
        selected_distillation_ids: Vec<String>,
    },
    LoadHistory {
        offset: usize,
        limit: usize,
    },
    LoadTransactionDetail {
        context_revision: u64,
        transaction_id: String,
    },
    RevertTransaction {
        transaction_id: String,
    },
    ReapplyTransaction {
        transaction_id: String,
    },
    SetEmergencyPolicy(StoredContextEmergencyPolicy),
    CopySafeMetadata(String),
}

#[derive(Clone, Debug, Default)]
struct HitRegions {
    list: Rect,
    preview: Rect,
    operations: Rect,
    toolbar: Vec<(Rect, ContextEditorToolbarAction)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ContextEditorToolbarAction {
    Range,
    Reasoning,
    ToggleOutput,
    ScanOutputs,
    Curator,
    Prepare,
    History,
    Policy,
    Detail,
    ConfirmRange,
    RejectRange,
    CancelDraft,
    ToggleProposal,
    Apply,
    Edit,
    Inspect,
    Revert,
    Reapply,
    CopyMetadata,
    NextHistoryPage,
    BackToHistory,
}

#[derive(Clone, Debug)]
struct ContextDetailBuffer {
    /// Exact immutable metadata with the raw chunk text removed.
    metadata: ContextMessageDetail,
    chunks: BTreeMap<usize, ContextTextChunk>,
}

impl ContextDetailBuffer {
    fn new(detail: ContextMessageDetail) -> Result<Self, String> {
        validate_detail_chunk(&detail.content)?;
        let mut metadata = detail.clone();
        metadata.content = metadata_only_chunk(detail.content.total_chars);
        let mut chunks = BTreeMap::new();
        chunks.insert(detail.content.start_char, detail.content);
        Ok(Self { metadata, chunks })
    }

    fn merge(&mut self, detail: ContextMessageDetail) -> Result<(), String> {
        validate_detail_chunk(&detail.content)?;
        let mut metadata = detail.clone();
        metadata.content = metadata_only_chunk(detail.content.total_chars);
        if self.metadata != metadata {
            return Err(format!(
                "Context detail metadata changed for message {} block {}. Refresh the authoritative snapshot.",
                detail.message_id, detail.block_ordinal
            ));
        }
        if let Some(existing) = self.chunks.get(&detail.content.start_char) {
            if existing == &detail.content {
                return Ok(());
            }
            return Err(format!(
                "Context detail chunk {} for message {} block {} conflicts with an already loaded chunk.",
                detail.content.start_char, detail.message_id, detail.block_ordinal
            ));
        }
        if self.chunks.values().any(|existing| {
            detail.content.start_char < existing.end_char
                && existing.start_char < detail.content.end_char
        }) {
            return Err(format!(
                "Context detail chunk {}..{} overlaps loaded content for message {} block {}.",
                detail.content.start_char,
                detail.content.end_char,
                detail.message_id,
                detail.block_ordinal
            ));
        }
        self.chunks
            .insert(detail.content.start_char, detail.content);
        Ok(())
    }

    fn contiguous_text(&self) -> (String, usize) {
        let mut text = String::new();
        let mut next = 0;
        for (start, chunk) in &self.chunks {
            if *start != next {
                break;
            }
            text.push_str(&chunk.text);
            next = chunk.end_char;
        }
        (text, next)
    }

    fn next_start_char(&self) -> Option<usize> {
        let (_, loaded) = self.contiguous_text();
        (loaded < self.metadata.content.total_chars).then_some(loaded)
    }
}

#[derive(Clone, Debug)]
pub struct ContextEditor {
    protocol_epoch: Option<u64>,
    open_mode: ContextEditorOpenMode,
    phase: ContextEditorPhase,
    modal: Option<ContextEditorModal>,
    focus: ContextEditorPane,
    snapshot: Option<ContextEditorSnapshot>,
    rows: BTreeMap<usize, ContextEditorMessage>,
    detail_buffers: BTreeMap<(String, usize), ContextDetailBuffer>,
    cursor: usize,
    block_cursor: usize,
    preview_scroll: usize,
    operations_scroll: usize,
    operations_max_scroll: usize,
    search_query: String,
    selected_message_ids: BTreeSet<String>,
    summary_anchor: Option<String>,
    pending_range_preview: Option<ContextRangeClosurePreview>,
    last_range_preview_result_id: Option<u64>,
    staged_ranges: Vec<crate::protocol::ContextClosedRangePreview>,
    allow_shadowing_active_operations: bool,
    reasoning: Option<ContextReasoningSelectionRequest>,
    last_keep_latest: usize,
    reasoning_input: String,
    tool_targets: BTreeSet<(String, usize)>,
    tool_scan_input: String,
    emergency_policy_input: String,
    curator_workspace: CuratorWorkspaceState,
    curator_selection: Option<ContextCuratorSelection>,
    curator_transaction_instructions: String,
    curator_range_instructions: BTreeMap<(String, String), String>,
    curator_plan: Option<ContextCuratorPlanPreview>,
    curator_plan_request: Option<ContextDraftRequest>,
    curator_plan_pending: bool,
    last_curator_default_result:
        Option<crate::tui::app::context_protocol::ContextCuratorDefaultResult>,
    draft_id: Option<String>,
    draft_progress: Option<ContextDraftProgress>,
    draft: Option<ContextDraft>,
    selected_distillation_ids: BTreeSet<String>,
    selection_preview: Option<ContextDraftSelectionPreview>,
    selection_preview_pending: bool,
    proposal_cursor: usize,
    history: Vec<ContextTransactionSummary>,
    history_total: usize,
    history_offset: usize,
    history_next_offset: Option<usize>,
    history_context_revision: Option<u64>,
    history_session_id: Option<String>,
    history_cursor: usize,
    transaction_detail: Option<ContextTransactionDetail>,
    error: Option<String>,
    status: Option<String>,
    stale: bool,
    pending_auto_page: Option<usize>,
    pending_follow_up_action: Option<ContextEditorAction>,
    last_snapshot_identity: Option<ContextEditorSnapshot>,
    last_detail_identity: Option<ContextMessageDetail>,
    last_draft_signature: Option<String>,
    last_history_signature: Option<ContextTransactionHistoryPage>,
    last_transaction_detail_signature: Option<(u64, String)>,
    last_transaction_outcome_signature: Option<(u64, u64, String)>,
    last_history_refresh_transaction_signature: Option<(String, u64)>,
    last_rejection_id: Option<u64>,
    last_emergency_policy: Option<StoredContextEmergencyPolicy>,
    hit_regions: HitRegions,
    rendered_message_start: usize,
    rendered_history_start: usize,
    narrow_layout: bool,
}

impl ContextEditor {
    pub fn new(mode: ContextEditorOpenMode) -> Self {
        Self::new_with_protocol_epoch(mode, None)
    }

    pub(crate) fn new_for_protocol_epoch(mode: ContextEditorOpenMode, epoch: u64) -> Self {
        Self::new_with_protocol_epoch(mode, Some(epoch))
    }

    fn new_with_protocol_epoch(mode: ContextEditorOpenMode, protocol_epoch: Option<u64>) -> Self {
        let phase = match mode {
            ContextEditorOpenMode::Edit => ContextEditorPhase::Loading,
            ContextEditorOpenMode::History
            | ContextEditorOpenMode::Restore
            | ContextEditorOpenMode::UndoLatest => ContextEditorPhase::History,
        };
        Self {
            protocol_epoch,
            open_mode: mode,
            phase,
            modal: None,
            focus: ContextEditorPane::History,
            snapshot: None,
            rows: BTreeMap::new(),
            detail_buffers: BTreeMap::new(),
            cursor: 0,
            block_cursor: 0,
            preview_scroll: 0,
            operations_scroll: 0,
            operations_max_scroll: 0,
            search_query: String::new(),
            selected_message_ids: BTreeSet::new(),
            summary_anchor: None,
            pending_range_preview: None,
            last_range_preview_result_id: None,
            staged_ranges: Vec::new(),
            allow_shadowing_active_operations: false,
            reasoning: None,
            last_keep_latest: 5,
            reasoning_input: "5".to_string(),
            tool_targets: BTreeSet::new(),
            tool_scan_input: "2000 5".to_string(),
            emergency_policy_input: "5 10 1 1 1".to_string(),
            curator_workspace: CuratorWorkspaceState::default(),
            curator_selection: None,
            curator_transaction_instructions: String::new(),
            curator_range_instructions: BTreeMap::new(),
            curator_plan: None,
            curator_plan_request: None,
            curator_plan_pending: false,
            last_curator_default_result: None,
            draft_id: None,
            draft_progress: None,
            draft: None,
            selected_distillation_ids: BTreeSet::new(),
            selection_preview: None,
            selection_preview_pending: false,
            proposal_cursor: 0,
            history: Vec::new(),
            history_total: 0,
            history_offset: 0,
            history_next_offset: None,
            history_context_revision: None,
            history_session_id: None,
            history_cursor: 0,
            transaction_detail: None,
            error: None,
            status: None,
            stale: false,
            pending_auto_page: None,
            pending_follow_up_action: None,
            last_snapshot_identity: None,
            last_detail_identity: None,
            last_draft_signature: None,
            last_history_signature: None,
            last_transaction_detail_signature: None,
            last_transaction_outcome_signature: None,
            last_history_refresh_transaction_signature: None,
            last_rejection_id: None,
            last_emergency_policy: None,
            hit_regions: HitRegions::default(),
            rendered_message_start: 0,
            rendered_history_start: 0,
            narrow_layout: false,
        }
    }

    pub fn initial_action(&self) -> ContextEditorAction {
        match self.open_mode {
            ContextEditorOpenMode::Edit => ContextEditorAction::LoadSnapshot {
                page_start: 0,
                page_size: DEFAULT_PAGE_SIZE,
            },
            ContextEditorOpenMode::History
            | ContextEditorOpenMode::Restore
            | ContextEditorOpenMode::UndoLatest => ContextEditorAction::LoadHistory {
                offset: 0,
                limit: DEFAULT_PAGE_SIZE,
            },
        }
    }

    pub fn phase(&self) -> ContextEditorPhase {
        self.phase
    }

    pub fn modal(&self) -> Option<ContextEditorModal> {
        self.modal
    }

    pub fn narrow_layout(&self) -> bool {
        self.narrow_layout
    }

    pub fn session_id(&self) -> Option<&str> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.session_id.as_str())
            .or(self.history_session_id.as_deref())
    }

    pub fn context_revision(&self) -> Option<u64> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.context_revision)
            .or(self.history_context_revision)
    }

    pub(crate) fn protocol_epoch(&self) -> Option<u64> {
        self.protocol_epoch
    }

    pub fn take_follow_up_action(&mut self) -> Option<ContextEditorAction> {
        self.pending_follow_up_action.take().or_else(|| {
            self.pending_auto_page
                .take()
                .map(|page_start| ContextEditorAction::LoadSnapshot {
                    page_start,
                    page_size: DEFAULT_PAGE_SIZE,
                })
        })
    }

    pub fn mark_stale(&mut self, reason: impl Into<String>) {
        self.stale = true;
        self.error = Some(reason.into());
        self.selection_preview_pending = false;
    }

    pub fn report_error(&mut self, error: impl Into<String>, stale: bool) {
        self.error = Some(error.into());
        self.stale |= stale;
        self.selection_preview_pending = false;
    }

    pub(crate) fn sync_protocol(&mut self, state: &ContextProtocolState) {
        if self
            .protocol_epoch
            .is_some_and(|epoch| state.active_editor_epoch() != Some(epoch))
        {
            return;
        }
        if let Some(snapshot) = state.snapshot.as_ref()
            && self.last_snapshot_identity.as_ref() != Some(snapshot)
        {
            self.apply_snapshot(snapshot.clone());
            self.last_snapshot_identity = Some(snapshot.clone());
        }

        if let Some(detail) = state.detail.as_ref()
            && self.last_detail_identity.as_ref() != Some(detail)
        {
            let key = (detail.message_id.clone(), detail.block_ordinal);
            let merge_result = if let Some(buffer) = self.detail_buffers.get_mut(&key) {
                buffer.merge(detail.clone())
            } else {
                ContextDetailBuffer::new(detail.clone()).map(|buffer| {
                    self.detail_buffers.insert(key, buffer);
                })
            };
            match merge_result {
                Ok(()) => {
                    self.last_detail_identity = Some(detail.clone());
                    let loaded = self
                        .detail_buffers
                        .get(&(detail.message_id.clone(), detail.block_ordinal))
                        .map(|buffer| buffer.contiguous_text().1)
                        .unwrap_or(0);
                    self.status = Some(format!(
                        "Loaded {} block {} ({} of {} characters)",
                        detail.message_id, detail.block_ordinal, loaded, detail.content.total_chars
                    ));
                    self.error = None;
                }
                Err(error) => self.mark_stale(error),
            }
        }

        if let (Some(result_id), Some(preview)) =
            (state.range_preview_result_id, state.range_preview.as_ref())
            && self.last_range_preview_result_id != Some(result_id)
        {
            self.last_range_preview_result_id = Some(result_id);
            self.pending_range_preview = Some(preview.clone());
            self.phase = ContextEditorPhase::ConfirmRangeClosure;
            self.error = None;
        }

        if let Some(plan) = state.curator_plan.as_ref()
            && self.curator_plan.as_ref() != Some(plan)
        {
            let task_count = plan.tasks.len();
            self.curator_plan = Some(plan.clone());
            self.curator_plan_pending = false;
            self.curator_workspace.detail_scroll = 0;
            self.curator_workspace_plan_accepted(task_count);
            self.status = Some(format!(
                "Validated {task_count} isolated curator call{}. Inspect prompts and source scope before generation.",
                if task_count == 1 { "" } else { "s" }
            ));
            self.error = None;
        }

        if let Some(result) = state.curator_default_result.as_ref()
            && self.last_curator_default_result.as_ref() != Some(result)
        {
            self.last_curator_default_result = Some(result.clone());
            if let Some(snapshot) = self.snapshot.as_mut() {
                snapshot.curator_default = result.selection.clone();
                snapshot.curator_route = result.resolved_route.clone();
                snapshot.curator_unavailable_reason = result.unavailable_reason.clone();
            }
            self.curator_selection = None;
            self.invalidate_curator_plan();
            self.curator_workspace_default_saved();
            self.status = Some(
                "Saved curator provider/route/model/effort as the durable default. Ephemeral instructions were not saved."
                    .to_string(),
            );
            self.error = None;
        }

        if let Some(draft_state) = state.draft.as_ref() {
            let signature = draft_state_signature(draft_state);
            if self.last_draft_signature.as_deref() != Some(signature.as_str()) {
                self.last_draft_signature = Some(signature);
                self.apply_draft_state(draft_state.clone());
            }
        }

        if let Some(preview) = state.selection_preview.as_ref()
            && self.selection_preview.as_ref() != Some(preview)
        {
            self.selection_preview = Some(preview.clone());
            self.selection_preview_pending = false;
            self.error = None;
        }

        if let Some(history) = state.history.as_ref()
            && self.last_history_signature.as_ref() != Some(history)
        {
            let selected_transaction_id = self
                .current_transaction()
                .map(|transaction| transaction.id.clone());
            let accepted = if history.offset == 0 {
                if self.draft.is_some() || self.selection_preview.is_some() {
                    self.stale = true;
                    self.error = Some(
                            "Authoritative transaction history changed. The generated review is stale and must be regenerated."
                                .to_string(),
                        );
                    self.draft = None;
                    self.draft_id = None;
                    self.draft_progress = None;
                    self.selection_preview = None;
                    self.selection_preview_pending = false;
                    self.selected_distillation_ids.clear();
                }
                self.history = history.transactions.clone();
                self.history_context_revision = Some(history.context_revision);
                self.history_session_id = state.accepted_session_id.clone();
                true
            } else if self.history_context_revision != Some(history.context_revision) {
                self.mark_stale(format!(
                        "Context history revision changed from {} to {} while loading page {}. Refresh page zero instead of guessing.",
                        self.history_context_revision
                            .map(|revision| revision.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        history.context_revision,
                        history.offset
                    ));
                false
            } else if history.offset > self.history.len() {
                self.mark_stale(format!(
                        "Context history page {} is not contiguous with {} loaded transaction(s). Refresh page zero.",
                        history.offset,
                        self.history.len()
                    ));
                false
            } else {
                let overlap = self.history.len().saturating_sub(history.offset);
                let overlap = overlap.min(history.transactions.len());
                let overlap_matches = self.history[history.offset..history.offset + overlap]
                    .iter()
                    .zip(history.transactions.iter().take(overlap))
                    .all(|(existing, incoming)| existing.id == incoming.id);
                if !overlap_matches {
                    self.mark_stale(
                            "Context history page overlaps loaded provenance with different transaction IDs. Refresh page zero."
                                .to_string(),
                        );
                    false
                } else {
                    let mut duplicate = None;
                    for transaction in history.transactions.iter().skip(overlap) {
                        if self
                            .history
                            .iter()
                            .any(|existing| existing.id == transaction.id)
                        {
                            duplicate = Some(transaction.id.clone());
                            break;
                        }
                    }
                    if let Some(duplicate) = duplicate {
                        self.mark_stale(format!(
                                "Context history page repeated transaction {duplicate} outside its contiguous overlap. Refresh page zero."
                            ));
                        false
                    } else {
                        self.history
                            .extend(history.transactions.iter().skip(overlap).cloned());
                        true
                    }
                }
            };
            if accepted {
                self.history_total = history.total_transactions;
                self.history_offset = history.offset;
                self.history_next_offset = history.next_offset;
                self.history_cursor = selected_transaction_id
                    .as_deref()
                    .and_then(|selected| {
                        self.history
                            .iter()
                            .position(|transaction| transaction.id == selected)
                    })
                    .unwrap_or_else(|| {
                        self.history_cursor
                            .min(self.history.len().saturating_sub(1))
                    });
                self.last_history_signature = Some(history.clone());
                self.phase = ContextEditorPhase::History;
                if self.open_mode == ContextEditorOpenMode::UndoLatest {
                    if let Some(index) = self
                        .history
                        .iter()
                        .position(|transaction| transaction.active)
                    {
                        self.history_cursor = index;
                        self.modal = Some(ContextEditorModal::RevertConfirmation);
                    } else {
                        self.status =
                            Some("No active context transaction is available to undo.".to_string());
                    }
                }
            }
        }

        if let Some(outcome) = state.transaction_result.as_ref() {
            let signature = (
                outcome.request_id,
                outcome.result.revision,
                outcome.correlation_id.clone(),
            );
            if self.last_transaction_outcome_signature.as_ref() != Some(&signature) {
                self.last_transaction_outcome_signature = Some(signature);
                let action = match outcome.request {
                    ContextRequestKind::ApplyDraft => "Applied",
                    ContextRequestKind::RevertTransaction => "Reverted",
                    ContextRequestKind::ReapplyTransaction => "Reapplied",
                    _ => "Updated",
                };
                let mut status = format!(
                    "{action} transaction {} at context revision {}.",
                    outcome.result.transaction.id, outcome.result.revision
                );
                if !outcome.result.warnings.is_empty() {
                    status.push_str(" Warnings: ");
                    status.push_str(&outcome.result.warnings.join(" | "));
                }
                self.status = Some(status);
                self.error = None;
                self.stale = false;
                self.snapshot = None;
                self.rows.clear();
                self.detail_buffers.clear();
                self.selected_message_ids.clear();
                self.summary_anchor = None;
                self.pending_range_preview = None;
                self.staged_ranges.clear();
                self.curator_selection = None;
                self.curator_transaction_instructions.clear();
                self.curator_range_instructions.clear();
                self.invalidate_curator_plan();
                self.curator_workspace.reset_for_session();
                self.reasoning = None;
                self.tool_targets.clear();
                self.draft_id = None;
                self.draft_progress = None;
                self.draft = None;
                self.selected_distillation_ids.clear();
                self.selection_preview = None;
                self.selection_preview_pending = false;
                self.transaction_detail = None;
                self.last_transaction_detail_signature = None;
                self.history.clear();
                self.history_total = 0;
                self.history_offset = 0;
                self.history_next_offset = None;
                self.history_context_revision = Some(outcome.result.revision);
                self.history_cursor = 0;
                self.last_history_signature = None;
                self.phase = ContextEditorPhase::History;
                self.preview_scroll = 0;
                self.pending_auto_page = None;
                self.queue_authoritative_history_refresh(
                    outcome.result.transaction.id.clone(),
                    outcome.result.revision,
                );
            }
        }

        if let Some(detail) = state.transaction_detail.as_ref() {
            let signature = (detail.context_revision, detail.transaction.id.clone());
            if self.last_transaction_detail_signature.as_ref() != Some(&signature) {
                self.transaction_detail = Some(detail.clone());
                self.last_transaction_detail_signature = Some(signature);
                self.phase = ContextEditorPhase::InspectTransaction;
                self.preview_scroll = 0;
            }
        }

        if let Some(policy) = state.emergency_policy.as_ref()
            && self.last_emergency_policy.as_ref() != Some(policy)
        {
            self.last_emergency_policy = Some(policy.clone());
            if let Some(snapshot) = self.snapshot.as_mut() {
                snapshot.emergency_policy = policy.clone();
            }
            if let Some(snapshot) = self.last_snapshot_identity.as_mut() {
                snapshot.emergency_policy = policy.clone();
            }
            self.status = Some(match policy {
                StoredContextEmergencyPolicy::Block => {
                    "Unattended context policy set to Block. Interactive and unattended turns will not mutate context automatically."
                        .to_string()
                }
                StoredContextEmergencyPolicy::Authorized {
                    protected_recent_assistant_turns,
                    target_headroom_percent,
                    allow_reasoning_suppression,
                    allow_tool_distillation,
                    allow_oldest_range_summary,
                    ..
                } => format!(
                    "Authorized explicitly unattended recovery: protect {protected_recent_assistant_turns} recent assistant turns, target {target_headroom_percent}% headroom, reasoning {}, tools {}, oldest summary {}.",
                    enabled_label(*allow_reasoning_suppression),
                    enabled_label(*allow_tool_distillation),
                    enabled_label(*allow_oldest_range_summary),
                ),
            });
            self.error = None;
        }

        if let Some(rejection) = state.last_rejection.as_ref()
            && self.last_rejection_id != Some(rejection.request_id)
        {
            self.last_rejection_id = Some(rejection.request_id);
            self.error = Some(rejection.error.to_string());
            self.selection_preview_pending = false;
            match rejection.request {
                ContextRequestKind::CuratorPlanPreview => {
                    self.curator_plan_pending = false;
                    self.curator_plan = None;
                    self.phase = ContextEditorPhase::Editing;
                    self.curator_workspace_plan_rejected();
                    self.status = Some(
                        "Exact curator plan validation failed. Staged operations and ephemeral settings were preserved; adjust them or retry."
                            .to_string(),
                    );
                }
                ContextRequestKind::SaveCuratorDefault => {
                    self.status = Some(
                        "The curator default was not saved. The prior durable default remains active."
                            .to_string(),
                    );
                }
                _ => {}
            }
        }
    }

    fn queue_authoritative_history_refresh(&mut self, transaction_id: String, revision: u64) {
        let signature = (transaction_id, revision);
        if self.last_history_refresh_transaction_signature.as_ref() == Some(&signature) {
            return;
        }
        self.last_history_refresh_transaction_signature = Some(signature);
        self.pending_follow_up_action = Some(ContextEditorAction::LoadHistory {
            offset: 0,
            limit: DEFAULT_PAGE_SIZE,
        });
    }

    fn apply_snapshot(&mut self, snapshot: ContextEditorSnapshot) {
        let previous_cursor_id = self.current_message().map(|message| message.message_id);
        let session_changed = self
            .snapshot
            .as_ref()
            .is_some_and(|existing| existing.session_id != snapshot.session_id);
        let historical_identity_changed = self.snapshot.as_ref().is_some_and(|existing| {
            existing.session_id == snapshot.session_id
                && (existing.context_revision != snapshot.context_revision
                    || existing.raw_message_count != snapshot.raw_message_count
                    || existing.transcript_digest != snapshot.transcript_digest
                    || existing.provider_name != snapshot.provider_name
                    || existing.provider_display_name != snapshot.provider_display_name
                    || existing.model != snapshot.model
                    || existing.route != snapshot.route)
        });
        if let Some(error) =
            self.snapshot_page_error(&snapshot, session_changed, historical_identity_changed)
        {
            self.mark_stale(error);
            return;
        }
        if session_changed {
            self.curator_workspace.reset_for_session();
            self.rows.clear();
            self.detail_buffers.clear();
            self.selected_message_ids.clear();
            self.summary_anchor = None;
            self.pending_range_preview = None;
            self.staged_ranges.clear();
            self.curator_selection = None;
            self.curator_transaction_instructions.clear();
            self.curator_range_instructions.clear();
            self.invalidate_curator_plan();
            self.reasoning = None;
            self.tool_targets.clear();
            self.draft = None;
            self.draft_id = None;
            self.selection_preview = None;
            self.stale = true;
            self.error = Some(
                "The editor session changed. All staged selections were cleared rather than retargeted by position."
                    .to_string(),
            );
        } else if historical_identity_changed {
            self.curator_workspace.active = false;
            self.curator_workspace.plan_dirty_reason =
                Some("Authoritative history, provider, model, or route changed".to_string());
            self.rows.clear();
            self.detail_buffers.clear();
            for range in &self.staged_ranges {
                self.selected_message_ids
                    .insert(range.requested.start_message_id.clone());
                self.selected_message_ids
                    .insert(range.requested.end_message_id.clone());
            }
            self.staged_ranges.clear();
            self.curator_range_instructions.clear();
            self.invalidate_curator_plan();
            self.pending_range_preview = None;
            self.allow_shadowing_active_operations = false;
            self.draft = None;
            self.draft_id = None;
            self.draft_progress = None;
            self.selected_distillation_ids.clear();
            self.selection_preview = None;
            self.selection_preview_pending = false;
            self.stale = true;
            self.phase = ContextEditorPhase::Editing;
            self.error = Some(
                "Authoritative history, provider, model, or route changed. Stable message and block selections were preserved where possible; closed ranges and generated review must be refreshed."
                    .to_string(),
            );
        }
        for message in &snapshot.messages {
            self.rows.insert(message.stored_index, message.clone());
        }
        self.pending_auto_page = snapshot.next_message_page_start;
        let complete_snapshot = snapshot.next_message_page_start.is_none()
            && self.rows.len() >= snapshot.raw_message_count;
        self.snapshot = Some(snapshot);
        if complete_snapshot {
            self.reconcile_stable_selections();
        }
        if let Some(previous_cursor_id) = previous_cursor_id
            && let Some(position) = self
                .visible_message_ids()
                .iter()
                .position(|message_id| message_id == &previous_cursor_id)
        {
            self.cursor = position;
        }
        if self.phase == ContextEditorPhase::Loading {
            self.phase = ContextEditorPhase::Editing;
            self.error = None;
            self.stale = false;
        }
        self.clamp_cursor();
    }

    fn snapshot_page_error(
        &self,
        snapshot: &ContextEditorSnapshot,
        session_changed: bool,
        historical_identity_changed: bool,
    ) -> Option<String> {
        let start = snapshot.message_page_start;
        let end = snapshot.message_page_end;
        if start > end || end > snapshot.raw_message_count {
            return Some(format!(
                "Context snapshot page {start}..{end} is outside the authoritative {}-message transcript.",
                snapshot.raw_message_count
            ));
        }
        if end.saturating_sub(start) != snapshot.messages.len() {
            return Some(format!(
                "Context snapshot page {start}..{end} declared {} row(s) but carried {}. Refresh page zero.",
                end.saturating_sub(start),
                snapshot.messages.len()
            ));
        }
        if snapshot.next_message_page_start != (end < snapshot.raw_message_count).then_some(end) {
            return Some(format!(
                "Context snapshot page {start}..{end} has an invalid continuation offset. Refresh page zero."
            ));
        }
        if (self.snapshot.is_none() || session_changed || historical_identity_changed) && start != 0
        {
            return Some(
                "A new authoritative context snapshot began after page zero. No selections were retargeted; refresh page zero."
                    .to_string(),
            );
        }

        let expected_start = (0..snapshot.raw_message_count)
            .find(|index| !self.rows.contains_key(index))
            .unwrap_or(snapshot.raw_message_count);
        if !session_changed && !historical_identity_changed && start != 0 && start != expected_start
        {
            return Some(format!(
                "Context snapshot page {start} is not contiguous with the next missing stored index {expected_start}. Refresh page zero."
            ));
        }

        let mut page_ids = BTreeSet::new();
        for (offset, message) in snapshot.messages.iter().enumerate() {
            let expected_index = start + offset;
            if message.stored_index != expected_index {
                return Some(format!(
                    "Context snapshot page {start}..{end} carried stored index {} where {expected_index} was required.",
                    message.stored_index
                ));
            }
            if !page_ids.insert(message.message_id.as_str()) {
                return Some(format!(
                    "Context snapshot page repeated stable message ID {}. Refresh page zero.",
                    message.message_id
                ));
            }
            if !session_changed && !historical_identity_changed {
                if let Some(existing) = self.rows.get(&expected_index)
                    && existing != message
                {
                    return Some(format!(
                        "Context snapshot changed stored index {expected_index} without changing authoritative transcript identity. Refresh page zero."
                    ));
                }
                if self.rows.iter().any(|(index, existing)| {
                    *index != expected_index && existing.message_id == message.message_id
                }) {
                    return Some(format!(
                        "Context snapshot moved stable message ID {} from another stored index. Refresh page zero.",
                        message.message_id
                    ));
                }
            }
        }
        None
    }

    fn reconcile_stable_selections(&mut self) {
        let message_ids = self
            .rows
            .values()
            .map(|message| message.message_id.clone())
            .collect::<BTreeSet<_>>();
        self.selected_message_ids
            .retain(|message_id| message_ids.contains(message_id));
        if self
            .summary_anchor
            .as_ref()
            .is_some_and(|message_id| !message_ids.contains(message_id))
        {
            self.summary_anchor = None;
        }
        self.tool_targets.retain(|(message_id, block_ordinal)| {
            self.rows.values().any(|message| {
                message.message_id == *message_id
                    && message
                        .blocks
                        .iter()
                        .any(|block| block.ordinal == *block_ordinal)
            })
        });
        if let Some(ContextReasoningSelectionRequest::MessageRanges { ranges }) =
            self.reasoning.as_mut()
        {
            ranges.retain(|range| {
                message_ids.contains(&range.start_message_id)
                    && message_ids.contains(&range.end_message_id)
            });
            if ranges.is_empty() {
                self.reasoning = None;
            }
        }
    }

    fn apply_draft_state(&mut self, state: ContextClientDraftState) {
        match state {
            ContextClientDraftState::Progress { draft_id, progress } => {
                self.draft_id = Some(draft_id);
                self.status = Some(match progress.phase {
                    ContextDraftPhase::Capturing => {
                        "Capturing and validating the exact atomic preparation plan.".to_string()
                    }
                    ContextDraftPhase::ClosingRanges => {
                        "Closing and structurally validating selected ranges.".to_string()
                    }
                    ContextDraftPhase::ExtractingChangeEvidence => {
                        "Extracting bounded change evidence for selected ranges.".to_string()
                    }
                    ContextDraftPhase::PreparingArtifacts => format!(
                        "Generating isolated curator calls: {}/{} completed.",
                        progress.completed_items, progress.total_items
                    ),
                    ContextDraftPhase::ValidatingProjection => format!(
                        "All {} isolated curator calls completed; validating one atomic provider projection.",
                        progress.total_items
                    ),
                    ContextDraftPhase::CalculatingEconomics => {
                        "Calculating exact token, cache, and selection economics.".to_string()
                    }
                    ContextDraftPhase::Ready => "Atomic context review is ready.".to_string(),
                });
                self.draft_progress = Some(progress);
                self.phase = ContextEditorPhase::PreparingDraft;
                self.curator_workspace.active = true;
                self.curator_workspace.clear_generation_outcome();
                self.curator_workspace.feedback = None;
            }
            ContextClientDraftState::Ready(draft) => {
                let draft = *draft;
                self.draft_id = Some(draft.identity.draft_id.clone());
                self.selected_distillation_ids = draft
                    .default_selected_distillation_ids()
                    .into_iter()
                    .collect();
                self.selection_preview = Some(ContextDraftSelectionPreview {
                    draft_id: draft.identity.draft_id.clone(),
                    selected_distillation_ids: self
                        .selected_distillation_ids
                        .iter()
                        .cloned()
                        .collect(),
                    preview: draft.preview.clone(),
                });
                self.selection_preview_pending = false;
                self.draft = Some(draft);
                self.draft_progress = None;
                self.phase = ContextEditorPhase::ReviewDraft;
                self.proposal_cursor = 0;
                self.stale = false;
                self.error = None;
                self.curator_workspace_draft_ready();
            }
            ContextClientDraftState::Applying(identity) => {
                self.draft_id = Some(identity.draft_id);
                self.phase = ContextEditorPhase::ReviewDraft;
                self.status = Some("Applying context transaction…".to_string());
                self.curator_workspace.feedback = None;
            }
            ContextClientDraftState::Applied {
                identity,
                transaction_id,
                revision,
            } => {
                self.draft_id = Some(identity.draft_id);
                self.status = Some(format!(
                    "Applied transaction {transaction_id} at context revision {revision}."
                ));
                self.phase = ContextEditorPhase::History;
                self.queue_authoritative_history_refresh(transaction_id, revision);
            }
            ContextClientDraftState::Failed { error, stale, .. } => {
                if matches!(&error, ContextServiceError::Curator(_)) {
                    self.status = Some(
                        "No curator artifacts were retained because the atomic preparation failed. Retrying will rerun every isolated curator call from a freshly validated exact plan."
                            .to_string(),
                    );
                }
                self.error = Some(error.to_string());
                self.stale = stale;
                self.phase = if self.draft.is_some() {
                    ContextEditorPhase::ReviewDraft
                } else {
                    ContextEditorPhase::Editing
                };
                if self.draft.is_none() {
                    self.curator_workspace
                        .mark_generation_outcome(CuratorGenerationOutcome::Failed);
                }
            }
            ContextClientDraftState::Canceled(_) => {
                self.status = Some(
                    if self.staged_ranges.is_empty() && self.tool_targets.is_empty() {
                        "Draft preparation canceled. Staged selections were preserved.".to_string()
                    } else {
                        "Draft preparation canceled. Staged selections were preserved, no curator artifacts were retained, and retrying will rerun every isolated call."
                        .to_string()
                    },
                );
                self.phase = ContextEditorPhase::Editing;
                self.curator_workspace
                    .mark_generation_outcome(CuratorGenerationOutcome::Canceled);
            }
            ContextClientDraftState::Expired(_) => {
                self.error = Some(
                    "The retained draft expired. Refresh the snapshot and prepare a new review."
                        .to_string(),
                );
                self.stale = true;
                self.phase = ContextEditorPhase::Editing;
                self.curator_workspace
                    .mark_generation_outcome(CuratorGenerationOutcome::Expired);
            }
        }
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        if self.curator_workspace_active() {
            return self.handle_curator_workspace_key(code, modifiers);
        }
        if let Some(modal) = self.modal {
            return self.handle_modal_key(modal, code, modifiers);
        }
        if code == KeyCode::Esc {
            if self.summary_anchor.take().is_some() {
                self.status = Some("Summary anchor canceled.".to_string());
                return (false, None);
            }
            if self.phase == ContextEditorPhase::ConfirmRangeClosure {
                self.pending_range_preview = None;
                self.phase = ContextEditorPhase::Editing;
                self.status = Some("Structural-closure preview rejected.".to_string());
                return (false, None);
            }
            if self.phase == ContextEditorPhase::InspectTransaction {
                self.phase = ContextEditorPhase::History;
                self.transaction_detail = None;
                return (false, None);
            }
            return (true, None);
        }
        if code == KeyCode::Char('?') {
            self.modal = Some(ContextEditorModal::Help);
            return (false, None);
        }
        if code == KeyCode::Char('/') {
            self.search_query.clear();
            self.modal = Some(ContextEditorModal::Search);
            return (false, None);
        }
        if code == KeyCode::Tab {
            self.focus = match self.focus {
                ContextEditorPane::History => ContextEditorPane::Preview,
                ContextEditorPane::Preview => ContextEditorPane::Operations,
                ContextEditorPane::Operations => ContextEditorPane::History,
            };
            return (false, None);
        }
        if matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
            let delta = if code == KeyCode::PageUp { -10 } else { 10 };
            match self.focus {
                ContextEditorPane::Preview => {
                    self.preview_scroll = self.preview_scroll.saturating_add_signed(delta);
                    return (false, None);
                }
                ContextEditorPane::Operations => {
                    self.operations_scroll = self
                        .operations_scroll
                        .saturating_add_signed(delta)
                        .min(self.operations_max_scroll);
                    return (false, None);
                }
                ContextEditorPane::History => {}
            }
        }

        match self.phase {
            ContextEditorPhase::Loading => (false, None),
            ContextEditorPhase::Editing => self.handle_editing_key(code, modifiers),
            ContextEditorPhase::ConfirmRangeClosure => match code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(preview) = self.pending_range_preview.take() {
                        self.allow_shadowing_active_operations =
                            !preview.shadowed_active_operations.is_empty();
                        self.staged_ranges = preview.ranges;
                        let retained = self
                            .staged_ranges
                            .iter()
                            .map(|range| canonical_editor_range_key(&range.requested))
                            .collect::<BTreeSet<_>>();
                        self.curator_range_instructions
                            .retain(|key, _| retained.contains(key));
                        self.curator_workspace.range_cursor = self
                            .curator_workspace
                            .range_cursor
                            .min(self.staged_ranges.len().saturating_sub(1));
                        self.invalidate_curator_plan();
                        self.summary_anchor = None;
                        self.phase = ContextEditorPhase::Editing;
                        self.status = Some(format!(
                            "Staged {} structurally closed summary range(s).",
                            self.staged_ranges.len()
                        ));
                    }
                    (false, None)
                }
                KeyCode::Char('n') => {
                    self.pending_range_preview = None;
                    self.phase = ContextEditorPhase::Editing;
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorPhase::PreparingDraft => match code {
                KeyCode::Char('c') => {
                    let action = self
                        .draft_id
                        .clone()
                        .map(|draft_id| ContextEditorAction::CancelDraft { draft_id });
                    (false, action)
                }
                _ => (false, None),
            },
            ContextEditorPhase::ReviewDraft => self.handle_review_key(code),
            ContextEditorPhase::History => self.handle_history_key(code),
            ContextEditorPhase::InspectTransaction => match code {
                KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                    (false, None)
                }
                KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') => {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                    (false, None)
                }
                KeyCode::Char('c') => {
                    let text = self
                        .transaction_detail
                        .as_ref()
                        .map(safe_transaction_metadata);
                    (false, text.map(ContextEditorAction::CopySafeMetadata))
                }
                _ => (false, None),
            },
        }
    }

    fn handle_editing_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                (false, None)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.move_block(-1);
                (false, None)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.move_block(1);
                (false, None)
            }
            KeyCode::PageUp => {
                self.move_cursor(-10);
                (false, None)
            }
            KeyCode::PageDown => {
                self.move_cursor(10);
                (false, None)
            }
            KeyCode::Char(' ') => {
                if let Some(message) = self.current_message()
                    && !self.selected_message_ids.remove(&message.message_id)
                {
                    self.selected_message_ids.insert(message.message_id);
                }
                (false, None)
            }
            KeyCode::Char('s') => self.handle_summary_anchor(),
            KeyCode::Char('x') => {
                if let Some(message) = self.current_message() {
                    let previous_len = self.staged_ranges.len();
                    self.staged_ranges.retain(|range| {
                        let start = range.source_range.start_index_hint;
                        let end = range.source_range.end_index_hint;
                        !(start <= message.stored_index && message.stored_index <= end)
                    });
                    if self.staged_ranges.len() != previous_len {
                        let retained = self
                            .staged_ranges
                            .iter()
                            .map(|range| canonical_editor_range_key(&range.requested))
                            .collect::<BTreeSet<_>>();
                        self.curator_range_instructions
                            .retain(|key, _| retained.contains(key));
                        self.curator_workspace.range_cursor = self
                            .curator_workspace
                            .range_cursor
                            .min(self.staged_ranges.len().saturating_sub(1));
                        self.invalidate_curator_plan();
                    }
                }
                (false, None)
            }
            KeyCode::Char('R') => {
                self.modal = Some(ContextEditorModal::ReasoningMenu);
                (false, None)
            }
            KeyCode::Char('d') => {
                self.toggle_current_tool_result();
                (false, None)
            }
            KeyCode::Char('D') => {
                self.modal = Some(ContextEditorModal::ToolScan);
                (false, None)
            }
            KeyCode::Char('C') => {
                self.open_curator_workspace(CuratorWorkspaceSection::Overview);
                (false, None)
            }
            KeyCode::Char('P') => {
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.processing)
                {
                    self.error = Some(
                        "Wait for the session to become idle before changing unattended authorization."
                            .to_string(),
                    );
                } else {
                    self.modal = Some(ContextEditorModal::EmergencyPolicyMenu);
                }
                (false, None)
            }
            KeyCode::Char('g') => self.prepare_action(),
            KeyCode::Char('H') => {
                self.phase = ContextEditorPhase::History;
                (
                    false,
                    Some(ContextEditorAction::LoadHistory {
                        offset: 0,
                        limit: DEFAULT_PAGE_SIZE,
                    }),
                )
            }
            KeyCode::Enter => (false, self.current_detail_action()),
            _ => (false, None),
        }
    }

    fn handle_summary_anchor(&mut self) -> (bool, Option<ContextEditorAction>) {
        let Some(message) = self.current_message() else {
            return (false, None);
        };
        let current_id = message.message_id;
        if let Some(anchor) = self.summary_anchor.take() {
            let Some(snapshot) = self.snapshot.as_ref() else {
                return (false, None);
            };
            let mut ranges = self
                .staged_ranges
                .iter()
                .map(|range| range.requested.clone())
                .collect::<Vec<_>>();
            ranges.push(ContextMessageRangeSelection {
                start_message_id: anchor,
                end_message_id: current_id,
            });
            (
                false,
                Some(ContextEditorAction::PreviewRanges {
                    context_revision: snapshot.context_revision,
                    transcript_digest: snapshot.transcript_digest,
                    ranges,
                }),
            )
        } else {
            self.summary_anchor = Some(current_id);
            self.status = Some("Summary anchor set. Move and press s again.".to_string());
            (false, None)
        }
    }

    fn prepare_action(&mut self) -> (bool, Option<ContextEditorAction>) {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.processing)
        {
            self.error = Some(
                "Wait for the current session turn to finish before preparing a context draft."
                    .to_string(),
            );
            return (false, None);
        }
        let mut request = self.current_draft_request();
        if request.is_empty() {
            self.error = Some(
                "Stage at least one summary, reasoning, or tool-result operation.".to_string(),
            );
            return (false, None);
        }
        let requires_curator =
            !request.summary_ranges.is_empty() || !request.tool_results.is_empty();
        if requires_curator && self.curator_selection.is_none() {
            let unavailable_reason = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.curator_unavailable_reason.clone());
            if let Some(reason) = unavailable_reason {
                self.open_curator_workspace(CuratorWorkspaceSection::Route);
                self.error = Some(format!(
                    "The configured curator default is unavailable: {reason}. Choose a capable temporary route or save a new default."
                ));
                return (false, None);
            }
        }
        if requires_curator
            && (self.curator_plan.as_ref().is_none()
                || self.curator_plan_request.as_ref() != Some(&request))
        {
            let Some(snapshot) = self.snapshot.as_ref() else {
                self.error = Some(
                    "Load the authoritative snapshot before previewing curator work.".to_string(),
                );
                return (false, None);
            };
            let context_revision = snapshot.context_revision;
            let transcript_digest = snapshot.transcript_digest;
            self.curator_plan = None;
            self.curator_plan_request = Some(request.clone());
            self.curator_plan_pending = true;
            self.phase = ContextEditorPhase::Editing;
            self.open_curator_workspace(CuratorWorkspaceSection::ExactCalls);
            self.curator_plan_pending = true;
            self.curator_plan_request = Some(request.clone());
            self.status = Some(
                "Checking every exact curator prompt and source scope without invoking a model."
                    .to_string(),
            );
            self.error = None;
            return (
                false,
                Some(ContextEditorAction::PreviewCuratorPlan {
                    context_revision,
                    transcript_digest,
                    request,
                }),
            );
        }
        if requires_curator {
            let Some(plan) = self.curator_plan.as_ref() else {
                self.error = Some(
                    "The exact curator plan is unavailable. Prepare a fresh preview before generation."
                        .to_string(),
                );
                return (false, None);
            };
            if plan.fingerprint.is_empty() {
                self.error = Some(
                    "The curator plan did not include an exact fingerprint. Prepare a fresh preview before generation."
                        .to_string(),
                );
                return (false, None);
            }
            request.curator.expected_plan_fingerprint = Some(plan.fingerprint.clone());
        }
        self.status = Some(if requires_curator {
            let total = request.summary_ranges.len() + request.tool_results.len();
            format!(
                "Starting {total} isolated curator call{} from the reviewed exact plan.",
                if total == 1 { "" } else { "s" }
            )
        } else {
            "Preparing one atomic context transaction without curator model calls.".to_string()
        });
        self.curator_workspace.feedback = None;
        self.phase = ContextEditorPhase::PreparingDraft;
        self.error = None;
        (false, Some(ContextEditorAction::PrepareDraft(request)))
    }

    fn current_draft_request(&self) -> ContextDraftRequest {
        ContextDraftRequest {
            summary_ranges: self
                .staged_ranges
                .iter()
                .map(|range| range.requested.clone())
                .collect(),
            reasoning: self.reasoning.clone(),
            tool_results: self
                .tool_targets
                .iter()
                .map(|(message_id, block_ordinal)| ContextToolResultSelection {
                    message_id: message_id.clone(),
                    block_ordinal: *block_ordinal,
                })
                .collect(),
            allow_shadowing_active_operations: self.allow_shadowing_active_operations,
            curator: ContextCuratorRunConfig {
                selection: self.curator_selection.clone(),
                transaction_instructions: self.curator_transaction_instructions.clone(),
                range_instructions: self
                    .staged_ranges
                    .iter()
                    .filter_map(|range| {
                        let instructions = self
                            .curator_range_instructions
                            .get(&canonical_editor_range_key(&range.requested))?;
                        (!instructions.is_empty()).then(|| ContextCuratorRangeInstructions {
                            range: range.requested.clone(),
                            instructions: instructions.clone(),
                        })
                    })
                    .collect(),
                expected_plan_fingerprint: None,
            },
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
        }
    }

    fn invalidate_curator_plan(&mut self) {
        self.curator_plan = None;
        self.curator_plan_request = None;
        self.curator_plan_pending = false;
        self.curator_workspace
            .mark_plan_dirty("Staged work or effective curator settings changed");
    }

    fn handle_review_key(&mut self, code: KeyCode) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.proposal_cursor = self.proposal_cursor.saturating_sub(1);
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self
                    .draft
                    .as_ref()
                    .map(|draft| draft.distillation_proposals.len().saturating_sub(1))
                    .unwrap_or(0);
                self.proposal_cursor = self.proposal_cursor.saturating_add(1).min(max);
                (false, None)
            }
            KeyCode::PageUp => {
                self.preview_scroll = self.preview_scroll.saturating_sub(10);
                (false, None)
            }
            KeyCode::PageDown => {
                self.preview_scroll = self.preview_scroll.saturating_add(10);
                (false, None)
            }
            KeyCode::Char(' ') => {
                let Some(draft) = self.draft.as_ref() else {
                    return (false, None);
                };
                let Some(proposal) = draft.distillation_proposals.get(self.proposal_cursor) else {
                    return (false, None);
                };
                if !self.selected_distillation_ids.remove(&proposal.proposal_id) {
                    self.selected_distillation_ids
                        .insert(proposal.proposal_id.clone());
                }
                let selected_distillation_ids = self
                    .selected_distillation_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                self.selection_preview_pending = true;
                self.selection_preview = None;
                (
                    false,
                    Some(ContextEditorAction::PreviewDraftSelection {
                        draft_id: draft.identity.draft_id.clone(),
                        selected_distillation_ids,
                    }),
                )
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if self.apply_disabled_reason().is_none() {
                    self.modal = Some(ContextEditorModal::ApplyConfirmation);
                }
                (false, None)
            }
            KeyCode::Char('e') => {
                self.phase = ContextEditorPhase::Editing;
                self.draft = None;
                self.selection_preview = None;
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_history_key(&mut self, code: KeyCode) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_cursor = self.history_cursor.saturating_sub(1);
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_cursor = self
                    .history_cursor
                    .saturating_add(1)
                    .min(self.history.len().saturating_sub(1));
                (false, None)
            }
            KeyCode::PageDown if self.history_next_offset.is_some() => (
                false,
                Some(ContextEditorAction::LoadHistory {
                    offset: self.history_next_offset.unwrap_or(0),
                    limit: DEFAULT_PAGE_SIZE,
                }),
            ),
            KeyCode::Enter => {
                let action = self.current_transaction().map(|transaction| {
                    ContextEditorAction::LoadTransactionDetail {
                        context_revision: self.history_context_revision.unwrap_or_default(),
                        transaction_id: transaction.id,
                    }
                });
                (false, action)
            }
            KeyCode::Char('r') => {
                if self
                    .current_transaction()
                    .is_some_and(|transaction| transaction.active)
                {
                    self.modal = Some(ContextEditorModal::RevertConfirmation);
                }
                (false, None)
            }
            KeyCode::Char('p') => {
                if self
                    .current_transaction()
                    .is_some_and(|transaction| !transaction.active)
                {
                    self.modal = Some(ContextEditorModal::ReapplyConfirmation);
                }
                (false, None)
            }
            KeyCode::Char('c') => {
                let text = self.current_transaction().map(|transaction| {
                    format!(
                        "context transaction {} · active={} · ranges={} · reasoning={} · distillations={}",
                        transaction.id,
                        transaction.active,
                        transaction.operation_counts.range_summaries,
                        transaction.operation_counts.reasoning_suppressions,
                        transaction.operation_counts.tool_result_distillations
                    )
                });
                (false, text.map(ContextEditorAction::CopySafeMetadata))
            }
            KeyCode::Char('e') => {
                self.phase = ContextEditorPhase::Editing;
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_modal_key(
        &mut self,
        modal: ContextEditorModal,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        if code == KeyCode::Esc {
            self.modal = None;
            return (false, None);
        }
        match modal {
            ContextEditorModal::Search => match code {
                KeyCode::Enter => {
                    self.modal = None;
                    self.cursor = 0;
                    self.clamp_cursor();
                    (false, None)
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.clamp_cursor();
                    (false, None)
                }
                KeyCode::Char(character) => {
                    self.search_query.push(character);
                    self.clamp_cursor();
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::ReasoningMenu => match code {
                KeyCode::Char('1') => {
                    self.reasoning_input = self.last_keep_latest.to_string();
                    self.modal = Some(ContextEditorModal::ReasoningKeepLatestInput);
                    (false, None)
                }
                KeyCode::Char('2') => {
                    let ranges = self.selected_message_runs();
                    if ranges.is_empty() {
                        self.error = Some(
                            "Select one or more stable message rows with Space first.".to_string(),
                        );
                    } else {
                        self.reasoning =
                            Some(ContextReasoningSelectionRequest::MessageRanges { ranges });
                        self.status = Some(
                            "Staged replayed-reasoning suppression for selected message ranges."
                                .to_string(),
                        );
                    }
                    self.modal = None;
                    (false, None)
                }
                KeyCode::Char('3') => {
                    self.reasoning = None;
                    self.modal = None;
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::ReasoningKeepLatestInput => match code {
                KeyCode::Enter => {
                    match self.reasoning_input.parse::<usize>() {
                        Ok(value) if value <= 1_000 => {
                            self.last_keep_latest = value;
                            self.reasoning =
                                Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                                    protected_recent_assistant_turns: value,
                                });
                            self.modal = None;
                            self.error = None;
                        }
                        _ => {
                            self.error = Some(
                                "Protected assistant turns must be an integer from 0 to 1000."
                                    .to_string(),
                            );
                        }
                    }
                    (false, None)
                }
                KeyCode::Backspace => {
                    self.reasoning_input.pop();
                    (false, None)
                }
                KeyCode::Char(character) if character.is_ascii_digit() => {
                    self.reasoning_input.push(character);
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::ToolScan => match code {
                KeyCode::Enter => {
                    if self.pending_auto_page.is_some() {
                        self.error = Some(
                            "Wait for the complete bounded history to finish loading before scanning."
                                .to_string(),
                        );
                        return (false, None);
                    }
                    let (minimum, protected) = match parse_tool_scan_input(&self.tool_scan_input) {
                        Ok(values) => values,
                        Err(error) => {
                            self.error = Some(error);
                            return (false, None);
                        }
                    };
                    self.scan_tool_results(minimum, protected);
                    self.error = None;
                    self.modal = None;
                    (false, None)
                }
                KeyCode::Backspace => {
                    self.tool_scan_input.pop();
                    (false, None)
                }
                KeyCode::Char(character)
                    if character.is_ascii_digit() || character.is_ascii_whitespace() =>
                {
                    self.tool_scan_input.push(character);
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::EmergencyPolicyMenu => match code {
                KeyCode::Char('1') => {
                    self.modal = None;
                    self.error = None;
                    (
                        false,
                        Some(ContextEditorAction::SetEmergencyPolicy(
                            StoredContextEmergencyPolicy::Block,
                        )),
                    )
                }
                KeyCode::Char('2') => {
                    self.modal = Some(ContextEditorModal::EmergencyPolicyInput);
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::EmergencyPolicyInput => match code {
                KeyCode::Enter => {
                    let session_id = self
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.session_id.as_str())
                        .unwrap_or("unknown");
                    match parse_emergency_policy_input(&self.emergency_policy_input, session_id) {
                        Ok(policy) => {
                            self.modal = None;
                            self.error = None;
                            (false, Some(ContextEditorAction::SetEmergencyPolicy(policy)))
                        }
                        Err(error) => {
                            self.error = Some(error);
                            (false, None)
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.emergency_policy_input.pop();
                    (false, None)
                }
                KeyCode::Char(character)
                    if character.is_ascii_digit() || character.is_ascii_whitespace() =>
                {
                    self.emergency_policy_input.push(character);
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::ApplyConfirmation => match code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(reason) = self.apply_disabled_reason() {
                        self.modal = None;
                        self.error = Some(format!(
                            "Cannot apply the context transaction because {reason}."
                        ));
                        return (false, None);
                    }
                    self.modal = None;
                    let action = self
                        .draft
                        .as_ref()
                        .map(|draft| ContextEditorAction::ApplyDraft {
                            draft_id: draft.identity.draft_id.clone(),
                            selected_distillation_ids: self
                                .selected_distillation_ids
                                .iter()
                                .cloned()
                                .collect(),
                        });
                    (false, action)
                }
                KeyCode::Char('n') => {
                    self.modal = None;
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::RevertConfirmation => match code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let transaction_id = self
                        .current_transaction()
                        .filter(|transaction| transaction.active)
                        .map(|transaction| transaction.id.clone());
                    self.modal = None;
                    let Some(transaction_id) = transaction_id else {
                        self.error = Some(
                            "Cannot revert because the selected transaction is no longer active."
                                .to_string(),
                        );
                        return (false, None);
                    };
                    let action = Some(ContextEditorAction::RevertTransaction { transaction_id });
                    (false, action)
                }
                KeyCode::Char('n') => {
                    self.modal = None;
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::ReapplyConfirmation => match code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let transaction_id = self
                        .current_transaction()
                        .filter(|transaction| !transaction.active)
                        .map(|transaction| transaction.id.clone());
                    self.modal = None;
                    let Some(transaction_id) = transaction_id else {
                        self.error = Some(
                            "Cannot reapply because the selected transaction is already active."
                                .to_string(),
                        );
                        return (false, None);
                    };
                    let action = Some(ContextEditorAction::ReapplyTransaction { transaction_id });
                    (false, action)
                }
                KeyCode::Char('n') => {
                    self.modal = None;
                    (false, None)
                }
                _ => (false, None),
            },
            ContextEditorModal::Help => {
                if matches!(code, KeyCode::Enter | KeyCode::Char('?')) {
                    self.modal = None;
                }
                (false, None)
            }
        }
    }

    fn preview_curator_plan_action(&mut self) -> Option<ContextEditorAction> {
        if self.curator_plan_pending {
            self.status = Some("Exact no-model validation is already in progress.".to_string());
            return None;
        }
        let request = self.current_draft_request();
        if request.summary_ranges.is_empty() && request.tool_results.is_empty() {
            self.error = Some(
                "Stage a range summary or tool-result candidate before previewing curator calls."
                    .to_string(),
            );
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let context_revision = snapshot.context_revision;
        let transcript_digest = snapshot.transcript_digest;
        self.curator_plan = None;
        self.curator_plan_request = Some(request.clone());
        self.curator_plan_pending = true;
        self.open_curator_workspace(CuratorWorkspaceSection::ExactCalls);
        self.curator_plan_pending = true;
        self.curator_plan_request = Some(request.clone());
        self.curator_workspace.plan_dirty_reason = None;
        self.status = Some(
            "Checking complete source, exact prompts, route identity, images, and request budgets without invoking a model."
                .to_string(),
        );
        Some(ContextEditorAction::PreviewCuratorPlan {
            context_revision,
            transcript_digest,
            request,
        })
    }

    fn effective_curator_selection(&self) -> Option<ContextCuratorSelection> {
        self.curator_selection.clone().or_else(|| {
            self.snapshot
                .as_ref()
                .map(|snapshot| snapshot.curator_default.clone())
        })
    }

    fn current_detail_action(&self) -> Option<ContextEditorAction> {
        self.current_message().and_then(|message| {
            let block = message.blocks.get(self.block_cursor)?;
            let snapshot = self.snapshot.as_ref()?;
            let start_char = self
                .detail_buffers
                .get(&(message.message_id.clone(), block.ordinal))
                .and_then(ContextDetailBuffer::next_start_char)
                .unwrap_or(0);
            if self
                .detail_buffers
                .get(&(message.message_id.clone(), block.ordinal))
                .is_some_and(|buffer| buffer.next_start_char().is_none())
            {
                return None;
            }
            Some(ContextEditorAction::LoadDetail {
                context_revision: snapshot.context_revision,
                transcript_digest: snapshot.transcript_digest,
                message_id: message.message_id,
                block_ordinal: block.ordinal,
                start_char,
                max_chars: DEFAULT_DETAIL_CHARS,
            })
        })
    }

    fn toolbar_items(&self) -> Vec<(&'static str, ContextEditorToolbarAction)> {
        match self.phase {
            ContextEditorPhase::Loading => Vec::new(),
            ContextEditorPhase::Editing => vec![
                ("Range", ContextEditorToolbarAction::Range),
                ("Reasoning", ContextEditorToolbarAction::Reasoning),
                ("Output", ContextEditorToolbarAction::ToggleOutput),
                ("Scan", ContextEditorToolbarAction::ScanOutputs),
                ("Curator", ContextEditorToolbarAction::Curator),
                ("Detail", ContextEditorToolbarAction::Detail),
                ("Prepare", ContextEditorToolbarAction::Prepare),
                ("History", ContextEditorToolbarAction::History),
                ("Policy", ContextEditorToolbarAction::Policy),
            ],
            ContextEditorPhase::ConfirmRangeClosure => vec![
                ("Confirm range", ContextEditorToolbarAction::ConfirmRange),
                ("Reject range", ContextEditorToolbarAction::RejectRange),
            ],
            ContextEditorPhase::PreparingDraft => vec![(
                "Cancel preparation",
                ContextEditorToolbarAction::CancelDraft,
            )],
            ContextEditorPhase::ReviewDraft => vec![
                (
                    "Toggle proposal",
                    ContextEditorToolbarAction::ToggleProposal,
                ),
                ("Apply", ContextEditorToolbarAction::Apply),
                ("Edit", ContextEditorToolbarAction::Edit),
            ],
            ContextEditorPhase::History => {
                let mut items = vec![
                    ("Inspect", ContextEditorToolbarAction::Inspect),
                    ("Revert", ContextEditorToolbarAction::Revert),
                    ("Reapply", ContextEditorToolbarAction::Reapply),
                    ("Copy", ContextEditorToolbarAction::CopyMetadata),
                    ("Editor", ContextEditorToolbarAction::Edit),
                ];
                if self.history_next_offset.is_some() {
                    items.push(("Next page", ContextEditorToolbarAction::NextHistoryPage));
                }
                items
            }
            ContextEditorPhase::InspectTransaction => vec![
                ("Back", ContextEditorToolbarAction::BackToHistory),
                ("Copy", ContextEditorToolbarAction::CopyMetadata),
            ],
        }
    }

    fn activate_toolbar(
        &mut self,
        action: ContextEditorToolbarAction,
    ) -> Option<ContextEditorAction> {
        if !self.toolbar_action_enabled(action) {
            return None;
        }
        let key = match action {
            ContextEditorToolbarAction::Range => Some(KeyCode::Char('s')),
            ContextEditorToolbarAction::Reasoning => Some(KeyCode::Char('R')),
            ContextEditorToolbarAction::ToggleOutput => Some(KeyCode::Char('d')),
            ContextEditorToolbarAction::ScanOutputs => Some(KeyCode::Char('D')),
            ContextEditorToolbarAction::Curator => Some(KeyCode::Char('C')),
            ContextEditorToolbarAction::Prepare => Some(KeyCode::Char('g')),
            ContextEditorToolbarAction::History => Some(KeyCode::Char('H')),
            ContextEditorToolbarAction::Policy => Some(KeyCode::Char('P')),
            ContextEditorToolbarAction::Detail => return self.current_detail_action(),
            ContextEditorToolbarAction::ConfirmRange => Some(KeyCode::Enter),
            ContextEditorToolbarAction::RejectRange => Some(KeyCode::Char('n')),
            ContextEditorToolbarAction::CancelDraft => Some(KeyCode::Char('c')),
            ContextEditorToolbarAction::ToggleProposal => Some(KeyCode::Char(' ')),
            ContextEditorToolbarAction::Apply => Some(KeyCode::Char('a')),
            ContextEditorToolbarAction::Edit => Some(KeyCode::Char('e')),
            ContextEditorToolbarAction::Inspect => Some(KeyCode::Enter),
            ContextEditorToolbarAction::Revert => Some(KeyCode::Char('r')),
            ContextEditorToolbarAction::Reapply => Some(KeyCode::Char('p')),
            ContextEditorToolbarAction::CopyMetadata => Some(KeyCode::Char('c')),
            ContextEditorToolbarAction::NextHistoryPage => {
                return self
                    .history_next_offset
                    .map(|offset| ContextEditorAction::LoadHistory {
                        offset,
                        limit: DEFAULT_PAGE_SIZE,
                    });
            }
            ContextEditorToolbarAction::BackToHistory => {
                self.phase = ContextEditorPhase::History;
                self.transaction_detail = None;
                return None;
            }
        };
        key.and_then(|key| self.handle_key(key, KeyModifiers::NONE).1)
    }

    fn toolbar_action_enabled(&self, action: ContextEditorToolbarAction) -> bool {
        match action {
            ContextEditorToolbarAction::Range => self.current_message().is_some(),
            ContextEditorToolbarAction::Reasoning | ContextEditorToolbarAction::History => true,
            ContextEditorToolbarAction::Curator => self.snapshot.is_some(),
            ContextEditorToolbarAction::Policy => !self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.processing),
            ContextEditorToolbarAction::ToggleOutput => {
                self.current_message().is_some_and(|message| {
                    message
                        .blocks
                        .get(self.block_cursor)
                        .is_some_and(|block| block.kind == StoredContextBlockKind::ToolResult)
                        && !self.message_in_staged_range(message.stored_index)
                })
            }
            ContextEditorToolbarAction::ScanOutputs => self.pending_auto_page.is_none(),
            ContextEditorToolbarAction::Prepare => {
                !self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.processing)
                    && (!self.staged_ranges.is_empty()
                        || self.reasoning.is_some()
                        || !self.tool_targets.is_empty())
            }
            ContextEditorToolbarAction::Detail => self.current_detail_action().is_some(),
            ContextEditorToolbarAction::ConfirmRange => self.pending_range_preview.is_some(),
            ContextEditorToolbarAction::RejectRange => true,
            ContextEditorToolbarAction::CancelDraft => self.draft_id.is_some(),
            ContextEditorToolbarAction::ToggleProposal => {
                self.draft.as_ref().is_some_and(|draft| {
                    draft
                        .distillation_proposals
                        .get(self.proposal_cursor)
                        .is_some()
                })
            }
            ContextEditorToolbarAction::Apply => self.apply_disabled_reason().is_none(),
            ContextEditorToolbarAction::Edit | ContextEditorToolbarAction::BackToHistory => true,
            ContextEditorToolbarAction::Inspect | ContextEditorToolbarAction::CopyMetadata => {
                match self.phase {
                    ContextEditorPhase::History => self.current_transaction().is_some(),
                    ContextEditorPhase::InspectTransaction => self.transaction_detail.is_some(),
                    _ => false,
                }
            }
            ContextEditorToolbarAction::Revert => self
                .current_transaction()
                .is_some_and(|transaction| transaction.active),
            ContextEditorToolbarAction::Reapply => self
                .current_transaction()
                .is_some_and(|transaction| !transaction.active),
            ContextEditorToolbarAction::NextHistoryPage => self.history_next_offset.is_some(),
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<ContextEditorAction> {
        if self.curator_workspace_active() {
            return self.handle_curator_workspace_mouse(mouse);
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                let position = Position::new(mouse.column, mouse.row);
                if self.hit_regions.operations.contains(position) {
                    self.operations_scroll = self.operations_scroll.saturating_sub(3);
                } else if self.hit_regions.preview.contains(position) {
                    self.preview_scroll = self.preview_scroll.saturating_sub(3);
                } else {
                    self.move_cursor(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                let position = Position::new(mouse.column, mouse.row);
                if self.hit_regions.operations.contains(position) {
                    self.operations_scroll = self
                        .operations_scroll
                        .saturating_add(3)
                        .min(self.operations_max_scroll);
                } else if self.hit_regions.preview.contains(position) {
                    self.preview_scroll = self.preview_scroll.saturating_add(3);
                } else {
                    self.move_cursor(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let position = Position::new(mouse.column, mouse.row);
                if let Some(action) = self
                    .hit_regions
                    .toolbar
                    .iter()
                    .find_map(|(area, action)| area.contains(position).then_some(*action))
                {
                    return self.activate_toolbar(action);
                }
                if self.hit_regions.list.contains(position) {
                    let relative = mouse.row.saturating_sub(self.hit_regions.list.y + 1) as usize;
                    if matches!(
                        self.phase,
                        ContextEditorPhase::History | ContextEditorPhase::InspectTransaction
                    ) {
                        self.history_cursor = self
                            .rendered_history_start
                            .saturating_add(relative)
                            .min(self.history.len().saturating_sub(1));
                    } else {
                        self.cursor = self
                            .rendered_message_start
                            .saturating_add(relative)
                            .min(self.visible_message_ids().len().saturating_sub(1));
                    }
                    self.focus = ContextEditorPane::History;
                } else if self.hit_regions.preview.contains(position) {
                    self.focus = ContextEditorPane::Preview;
                } else if self.hit_regions.operations.contains(position) {
                    self.focus = ContextEditorPane::Operations;
                }
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame) {
        if self.curator_workspace_active() {
            self.render_curator_workspace(frame);
            return;
        }
        let area = frame.area();
        self.narrow_layout = area.width < NARROW_WIDTH;
        frame.render_widget(Clear, area);
        let title = self.title();
        let outer = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let footer_height = if self.narrow_layout { 6 } else { 4 };
        let vertical = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(5),
            Constraint::Length(footer_height),
        ])
        .split(inner);
        self.render_header(frame, vertical[0]);

        if self.narrow_layout {
            let panes = Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)])
                .split(vertical[1]);
            self.render_list(frame, panes[0]);
            self.render_preview(frame, panes[1]);
        } else {
            let panes =
                Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
                    .split(vertical[1]);
            self.render_list(frame, panes[0]);
            self.render_preview(frame, panes[1]);
        }
        self.render_operations(frame, vertical[2]);
        self.render_footer(frame, vertical[3]);
        self.render_modal(frame, area);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let snapshot = self.snapshot.as_ref();
        let context = match snapshot {
            Some(snapshot) => {
                format!(
                    "{} / {} tokens · revision {} · {} · {}",
                    format_tokens(snapshot.projected_request_tokens),
                    format_tokens(snapshot.context_window),
                    snapshot.context_revision,
                    if snapshot.processing {
                        "processing"
                    } else {
                        "idle"
                    },
                    snapshot.provider_display_name
                )
            }
            None if matches!(
                self.phase,
                ContextEditorPhase::History | ContextEditorPhase::InspectTransaction
            ) =>
            {
                self.history_context_revision.map_or_else(
                    || "Loading authoritative context transaction history…".to_string(),
                    |revision| {
                        format!(
                            "Context revision {revision} · {} authoritative transaction(s)",
                            self.history_total
                        )
                    },
                )
            }
            None => "Loading authoritative context state…".to_string(),
        };
        let search = if self.search_query.is_empty() {
            String::new()
        } else {
            format!(" · search: {}", self.search_query)
        };
        let width = usize::from(area.width);
        let mut lines = vec![
            Line::from(one_line(&format!("{context}{search}"), width))
                .style(Style::default().fg(Color::Gray)),
        ];
        if let Some(error) = self.error.as_deref() {
            lines.push(
                Line::from(one_line(&format!("Error: {error}"), width))
                    .style(Style::default().fg(Color::Red)),
            );
        } else if let Some(status) = self.status.as_deref() {
            lines.push(
                Line::from(one_line(&format!("Status: {status}"), width))
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        self.hit_regions.list = area;
        let mut title = match self.phase {
            ContextEditorPhase::History | ContextEditorPhase::InspectTransaction => {
                format!(
                    " Transactions ({}/{}) ",
                    self.history.len(),
                    self.history_total
                )
            }
            _ => format!(
                " Authoritative history ({}/{}) ",
                self.rows.len(),
                self.total_rows()
            ),
        };
        let focused = self.focus == ContextEditorPane::History;
        if focused {
            title = format!("{}[focused] ", title.trim_end());
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = inner.height as usize;
        let lines = if matches!(
            self.phase,
            ContextEditorPhase::History | ContextEditorPhase::InspectTransaction
        ) {
            let start = self.history_cursor.saturating_sub(height.saturating_sub(1));
            self.rendered_history_start = start;
            self.history
                .iter()
                .enumerate()
                .skip(start)
                .take(height)
                .map(|(index, transaction)| {
                    let marker = if index == self.history_cursor {
                        "›"
                    } else {
                        " "
                    };
                    let status = transaction_status(transaction);
                    Line::from(format!(
                        "{marker} {status:<10} {} · R{} Q{} D{}",
                        short_id(&transaction.id),
                        transaction.operation_counts.range_summaries,
                        transaction.operation_counts.reasoning_suppressions,
                        transaction.operation_counts.tool_result_distillations
                    ))
                    .style(if index == self.history_cursor {
                        Style::default().fg(Color::Black).bg(Color::Yellow)
                    } else {
                        Style::default()
                    })
                })
                .collect::<Vec<_>>()
        } else {
            let ids = self.visible_message_ids();
            let start = self.cursor.saturating_sub(height.saturating_sub(1));
            self.rendered_message_start = start;
            ids.iter()
                .enumerate()
                .skip(start)
                .take(height)
                .filter_map(|(visible_index, id)| {
                    let message = self.message_by_id(id)?;
                    let marker = if visible_index == self.cursor {
                        "›"
                    } else {
                        " "
                    };
                    let selected = if self.selected_message_ids.contains(id) {
                        "●"
                    } else {
                        " "
                    };
                    let staged = if self.message_in_staged_range(message.stored_index) {
                        "Σ"
                    } else if self.summary_anchor.as_deref() == Some(id.as_str()) {
                        "A"
                    } else {
                        " "
                    };
                    let role = role_label(&message.role);
                    let preview =
                        one_line(&message.preview, inner.width.saturating_sub(24) as usize);
                    Some(
                        Line::from(format!(
                            "{marker}{selected}{staged} {:>5} {role} {:>6} {preview}",
                            message.stored_index,
                            format_tokens(message.raw_provider_tokens)
                        ))
                        .style(if visible_index == self.cursor {
                            Style::default().fg(Color::Black).bg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        self.hit_regions.preview = area;
        let focused = self.focus == ContextEditorPane::Preview;
        let title = if focused {
            " Preview / review [focused] "
        } else {
            " Preview / review "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = self.preview_lines(inner.width as usize);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.preview_scroll.min(u16::MAX as usize) as u16, 0))
                .wrap(Wrap { trim: false }),
            inner,
        );
    }

    fn render_operations(&mut self, frame: &mut Frame, area: Rect) {
        self.hit_regions.operations = area;
        let focused = self.focus == ContextEditorPane::Operations;
        let title = if focused {
            " Staged operations / status [focused] "
        } else {
            " Staged operations / status "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if focused {
                Style::default().fg(Color::Yellow)
            } else if self.error.is_some() {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = self.operations_lines();
        let wrap_width = usize::from(inner.width.max(1));
        let wrapped_lines = lines
            .iter()
            .map(|line| line.width().max(1).div_ceil(wrap_width))
            .sum::<usize>();
        let paragraph =
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(if self.error.is_some() {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Gray)
                });
        self.operations_max_scroll = wrapped_lines.saturating_sub(inner.height as usize);
        self.operations_scroll = self.operations_scroll.min(self.operations_max_scroll);
        frame.render_widget(
            paragraph.scroll((self.operations_scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
    }

    fn render_footer(&mut self, frame: &mut Frame, area: Rect) {
        let help = match self.phase {
            ContextEditorPhase::Editing => {
                "s range · Space select · R reasoning · d/D outputs · C curator · P policy · g review · H history · / search · ? help · Esc"
            }
            ContextEditorPhase::ConfirmRangeClosure => "Enter confirm closure · Esc reject",
            ContextEditorPhase::PreparingDraft => {
                "c cancel · Esc keeps draft running and closes editor"
            }
            ContextEditorPhase::ReviewDraft => {
                "Space toggle proposal · a apply · e edit · PgUp/PgDn scroll · Esc"
            }
            ContextEditorPhase::History => {
                "Enter inspect · r revert · p reapply · c copy metadata · e editor · Esc"
            }
            ContextEditorPhase::InspectTransaction => {
                "PgUp/PgDn scroll · c copy metadata · Esc history"
            }
            ContextEditorPhase::Loading => "Loading… · Esc",
        };
        self.hit_regions.toolbar.clear();
        let toolbar_rows = area.height.saturating_sub(1);
        let mut x = area.x;
        let mut y = area.y;
        for (label, action) in self.toolbar_items() {
            let enabled = self.toolbar_action_enabled(action);
            let text = if enabled {
                format!("[{label}]")
            } else {
                format!("({label})")
            };
            let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            if x.saturating_add(width) > area.right() {
                x = area.x;
                y = y.saturating_add(1);
            }
            if y >= area.y.saturating_add(toolbar_rows) || width > area.width {
                continue;
            }
            let button = Rect::new(x, y, width, 1);
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(if enabled {
                    Color::Cyan
                } else {
                    Color::DarkGray
                })),
                button,
            );
            if enabled {
                self.hit_regions.toolbar.push((button, action));
            }
            x = x.saturating_add(width.saturating_add(1));
        }
        if area.height > 0 {
            frame.render_widget(
                Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
                Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            );
        }
    }

    fn operations_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(format!(
            "{} summary range(s) · {} output candidate(s) · {} selected proposal(s)",
            self.staged_ranges.len(),
            self.tool_targets.len(),
            self.selected_distillation_ids.len()
        ))];
        if let Some(snapshot) = self.snapshot.as_ref() {
            lines.push(Line::from(emergency_policy_summary(
                &snapshot.emergency_policy,
            )));
            if let Some(route) = snapshot.curator_route.as_ref() {
                lines.push(Line::from(format!(
                    "Curator route: {} / {} / {} · model {} · effort {}",
                    route.provider_display_name,
                    route.provider_name,
                    route.route,
                    route.model,
                    route.effort.as_deref().unwrap_or("unspecified")
                )));
            } else if let Some(reason) = snapshot.curator_unavailable_reason.as_deref() {
                lines.push(Line::from(format!(
                    "Curator route unavailable for summaries/tool outputs: {reason}. Reasoning-only drafts remain available."
                )));
            } else {
                lines.push(Line::from(
                    "Curator route availability was not reported by this server.",
                ));
            }
            if let Some(selection) = self.curator_selection.as_ref() {
                lines.push(Line::from(format!(
                    "Per-run curator override: provider {} · route {} · model {} · effort {}",
                    selection.provider.as_deref().unwrap_or("active fork"),
                    selection.route.as_deref().unwrap_or("active route"),
                    selection.model.as_deref().unwrap_or("active model"),
                    selection.effort.as_deref().unwrap_or("provider default")
                )));
            } else {
                lines.push(Line::from("Curator selection source: configured default"));
            }
            lines.push(Line::from(format!(
                "Curator instructions: {} transaction character(s) · {} range override(s) · exact plan {}",
                self.curator_transaction_instructions.chars().count(),
                self.curator_range_instructions
                    .values()
                    .filter(|value| !value.is_empty())
                    .count(),
                if self.curator_plan_pending {
                    "pending"
                } else if self.curator_plan_request.as_ref() == Some(&self.current_draft_request())
                    && self.curator_plan.is_some()
                {
                    "validated"
                } else {
                    "missing/stale"
                }
            )));
        }
        for (index, range) in self.staged_ranges.iter().enumerate() {
            lines.push(Line::from(format!(
                "Summary {}: raw {}..{} · {} message(s) · {} source tokens · {} expansion(s)",
                index + 1,
                range.source_range.start_index_hint,
                range.source_range.end_index_hint,
                range.source_range.message_count,
                format_tokens(range.source_tokens),
                range.boundary_expansions.len()
            )));
        }
        lines.extend(self.reasoning_statistics_lines());
        if !self.tool_targets.is_empty() {
            lines.push(Line::from(format!(
                "Marked tool-result blocks: {}",
                self.tool_targets
                    .iter()
                    .map(|(message_id, ordinal)| format!("{}#{ordinal}", short_id(message_id)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(error) = self.error.as_ref() {
            lines.push(Line::from(format!("Error: {error}")));
        } else if let Some(status) = self.status.as_ref() {
            lines.push(Line::from(format!("Status: {status}")));
        }
        lines
    }

    fn reasoning_statistics_lines(&self) -> Vec<Line<'static>> {
        let Some(selection) = self.reasoning.as_ref() else {
            return vec![Line::from("Reasoning suppression: none staged")];
        };
        let (selected_indices, protected_indices) = match selection {
            ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns,
            } => {
                let assistants = self
                    .rows
                    .values()
                    .filter(|message| message.role == Role::Assistant)
                    .map(|message| message.stored_index)
                    .collect::<Vec<_>>();
                let protected_start = assistants
                    .len()
                    .saturating_sub(*protected_recent_assistant_turns);
                (
                    assistants
                        .iter()
                        .take(protected_start)
                        .copied()
                        .collect::<BTreeSet<_>>(),
                    assistants
                        .iter()
                        .skip(protected_start)
                        .copied()
                        .collect::<BTreeSet<_>>(),
                )
            }
            ContextReasoningSelectionRequest::MessageRanges { ranges } => {
                let mut indices = BTreeSet::new();
                for range in ranges {
                    let Some(start) = self
                        .rows
                        .values()
                        .find(|message| message.message_id == range.start_message_id)
                        .map(|message| message.stored_index)
                    else {
                        continue;
                    };
                    let Some(end) = self
                        .rows
                        .values()
                        .find(|message| message.message_id == range.end_message_id)
                        .map(|message| message.stored_index)
                    else {
                        continue;
                    };
                    let (start, end) = if start <= end {
                        (start, end)
                    } else {
                        (end, start)
                    };
                    indices.extend(start..=end);
                }
                (indices, BTreeSet::new())
            }
        };
        let mut assistant_turns = BTreeSet::new();
        let mut target_count = 0usize;
        let mut removable_tokens = 0usize;
        let mut kinds = BTreeSet::new();
        let mut trace_only_count = 0usize;
        let mut already_suppressed_count = 0usize;
        let mut active_summary_count = 0usize;
        let mut protected_count = 0usize;
        let mut non_replayable_count = 0usize;
        for message in self.rows.values() {
            for block in &message.blocks {
                if protected_indices.contains(&message.stored_index)
                    && block.provider_removable_reasoning
                {
                    protected_count = protected_count.saturating_add(1);
                    continue;
                }
                if !selected_indices.contains(&message.stored_index) {
                    continue;
                }
                if block.kind == StoredContextBlockKind::ReasoningTrace {
                    trace_only_count = trace_only_count.saturating_add(1);
                    non_replayable_count = non_replayable_count.saturating_add(1);
                } else if replayed_reasoning_kind(block.kind) && !block.provider_removable_reasoning
                {
                    non_replayable_count = non_replayable_count.saturating_add(1);
                } else if block.provider_removable_reasoning && message.summary_coverage.is_some() {
                    active_summary_count = active_summary_count.saturating_add(1);
                } else if block.provider_removable_reasoning
                    && block.active_operations.iter().any(|operation| {
                        operation.kind == ContextOperationBadgeKind::ReasoningSuppression
                    })
                {
                    already_suppressed_count = already_suppressed_count.saturating_add(1);
                } else if block.provider_removable_reasoning {
                    assistant_turns.insert(message.stored_index);
                    target_count = target_count.saturating_add(1);
                    removable_tokens =
                        removable_tokens.saturating_add(block.estimated_provider_tokens);
                    kinds.insert(block.kind);
                }
            }
        }
        let policy = match selection {
            ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns,
            } => format!("keep latest {protected_recent_assistant_turns} assistant turn(s)"),
            ContextReasoningSelectionRequest::MessageRanges { ranges } => {
                format!("manual stable ranges ({})", ranges.len())
            }
        };
        let mut lines = vec![Line::from(format!(
            "Reasoning suppression: {policy} · {} assistant turn(s) · {} replay block(s) · {} removable tokens · kinds {}",
            assistant_turns.len(),
            target_count,
            format_tokens(removable_tokens),
            if kinds.is_empty() {
                "none".to_string()
            } else {
                kinds
                    .iter()
                    .map(|kind| format!("{kind:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ))];
        lines.push(Line::from(format!(
            "Planning categories: {target_count} newly eligible · {already_suppressed_count} already suppressed · {active_summary_count} covered by active summaries · {protected_count} protected · {non_replayable_count} non-replayable"
        )));
        if trace_only_count > 0 {
            lines.push(Line::from(format!(
                "ReasoningTrace: {trace_only_count} visible history-only block(s); zero provider-token savings."
            )));
        }
        if self.pending_auto_page.is_some() {
            lines.push(Line::from(
                "Reasoning statistics are provisional until all bounded snapshot pages load.",
            ));
        }
        lines
    }

    fn render_modal(&self, frame: &mut Frame, area: Rect) {
        let Some(modal) = self.modal else {
            return;
        };
        let width = area.width.saturating_sub(8).clamp(20, 76);
        let height = match modal {
            ContextEditorModal::Help => 18,
            ContextEditorModal::ReasoningMenu => 9,
            ContextEditorModal::EmergencyPolicyMenu => {
                if width < 60 {
                    16
                } else {
                    12
                }
            }
            ContextEditorModal::EmergencyPolicyInput => {
                if width < 60 {
                    16
                } else {
                    14
                }
            }
            _ => 7,
        }
        .min(area.height.saturating_sub(4).max(3));
        let modal_area = centered_rect(area, width, height);
        frame.render_widget(Clear, modal_area);
        let (title, body) = match modal {
            ContextEditorModal::Search => (
                "Search",
                format!("Query: {}\n\nEnter apply · Esc cancel", self.search_query),
            ),
            ContextEditorModal::ReasoningMenu => (
                "Reasoning suppression",
                "1 Keep latest b assistant turns\n2 Suppress replayed reasoning in selected message runs\n3 Clear staged reasoning\n\nReasoningTrace is transcript-only and saves zero provider tokens."
                    .to_string(),
            ),
            ContextEditorModal::ReasoningKeepLatestInput => (
                "Protected recent assistant turns",
                format!("b = {}\n\nDigits only, 0..1000 · Enter stage · Esc", self.reasoning_input),
            ),
            ContextEditorModal::ToolScan => (
                "Mechanical tool-result scan",
                format!(
                    "minimum_tokens protected_recent_turns = {}\n\nThis only marks candidates. The curator remains authoritative.",
                    self.tool_scan_input
                ),
            ),
            ContextEditorModal::EmergencyPolicyMenu => (
                "Unattended context authorization",
                "1 Block unattended context surgery (safe default)\n2 Authorize one curator-backed provider-view transaction and one retry when an explicitly unattended turn cannot fit\n\nInteractive submits always remain manual. Raw transcript content, pending attachments, active tool pairs, and protected recent turns are never removed."
                    .to_string(),
            ),
            ContextEditorModal::EmergencyPolicyInput => (
                "Authorize unattended context surgery",
                format!(
                    "protected_turns headroom_percent reasoning tools oldest_summary\n{}\n\nFlags are 0 or 1. The policy permits at most one atomic transaction and one retry for explicitly unattended execution only. Enter authorize · Esc cancel",
                    self.emergency_policy_input
                ),
            ),
            ContextEditorModal::ApplyConfirmation => (
                "Apply transaction?",
                self.apply_confirmation_text(),
            ),
            ContextEditorModal::RevertConfirmation => (
                "Revert transaction?",
                "This preserves provenance and creates one new context revision.\n\nEnter/y confirm · n/Esc cancel"
                    .to_string(),
            ),
            ContextEditorModal::ReapplyConfirmation => (
                "Reapply transaction?",
                "Targets and provider validity are revalidated before persistence.\n\nEnter/y confirm · n/Esc cancel"
                    .to_string(),
            ),
            ContextEditorModal::Help => (
                "Context editor help",
                "Stable IDs, never wrapped rows, own every selection.\n\ns anchors structurally closed summary ranges.\nSpace selects stable messages or toggles a reviewed proposal.\nR stages replayed-reasoning suppression.\nd marks the selected ToolResult block. D scans mechanically.\nC opens per-run curator settings, exact prompts/source scope, and explicit default save.\nP configures explicit unattended emergency authorization.\ng first requires exact curator-plan review, then prepares one atomic transaction.\nTab changes pane focus. Arrow/vim keys navigate.\nOriginal Session.messages is never changed by these operations.\n\n? or Enter closes help."
                    .to_string(),
            ),
        };
        let paragraph = Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} "))
                    .border_style(Style::default().fg(Color::Magenta)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, modal_area);
    }

    fn preview_lines(&self, width: usize) -> Vec<Line<'static>> {
        match self.phase {
            ContextEditorPhase::ConfirmRangeClosure => self.range_preview_lines(width),
            ContextEditorPhase::PreparingDraft => self.progress_lines(),
            ContextEditorPhase::ReviewDraft => self.review_lines(width),
            ContextEditorPhase::History => self.history_preview_lines(),
            ContextEditorPhase::InspectTransaction => self.transaction_detail_lines(width),
            _ => self.message_preview_lines(width),
        }
    }

    fn message_preview_lines(&self, width: usize) -> Vec<Line<'static>> {
        let Some(message) = self.current_message() else {
            return vec![Line::from("No authoritative messages are available.")];
        };
        let mut lines = vec![
            Line::from(format!(
                "Stored message {} · {} · raw {} · projected {}",
                message.stored_index,
                message.message_id,
                format_tokens(message.raw_provider_tokens),
                format_tokens(message.projected_provider_tokens)
            )),
            Line::from(""),
            Line::from(one_line(&message.preview, width.saturating_sub(2))),
            Line::from(""),
        ];
        for (index, block) in message.blocks.iter().enumerate() {
            let selected = if index == self.block_cursor {
                "›"
            } else {
                " "
            };
            let marked = if self
                .tool_targets
                .contains(&(message.message_id.clone(), block.ordinal))
            {
                "●"
            } else {
                " "
            };
            lines.push(Line::from(format!(
                "{selected}{marked} block {} {:?} · {}{}",
                block.ordinal,
                block.kind,
                format_tokens(block.estimated_provider_tokens),
                if block.provider_removable_reasoning {
                    " · replay-removable"
                } else {
                    ""
                }
            )));
        }
        if let Some(block) = message.blocks.get(self.block_cursor) {
            lines.push(Line::from(""));
            if let Some(buffer) = self
                .detail_buffers
                .get(&(message.message_id.clone(), block.ordinal))
            {
                let metadata = &buffer.metadata;
                let (loaded_text, loaded_chars) = buffer.contiguous_text();
                lines.push(Line::from(format!(
                    "Loaded block detail · {:?} · {:?} · {} / {} characters",
                    metadata.block_kind,
                    metadata.format,
                    loaded_chars,
                    metadata.content.total_chars
                )));
                if let Some(semantic_id) = metadata.semantic_id.as_deref() {
                    lines.push(Line::from(format!("Semantic ID: {semantic_id}")));
                }
                if let Some(tool_name) = metadata.tool_name.as_deref() {
                    lines.push(Line::from(format!("Tool: {tool_name}")));
                }
                if let Some(tool_use_id) = metadata.tool_use_id.as_deref() {
                    lines.push(Line::from(format!("Tool call ID: {tool_use_id}")));
                }
                if let Some(is_error) = metadata.tool_result_is_error {
                    lines.push(Line::from(format!("Tool result error: {is_error}")));
                }
                if let Some(status) = metadata.provider_status.as_deref() {
                    lines.push(Line::from(format!("Provider status: {status}")));
                }
                if let Some(media_type) = metadata.image_media_type.as_deref() {
                    lines.push(Line::from(format!("Image media type: {media_type}")));
                }
                if let Some(encoded_bytes) = metadata.image_encoded_bytes {
                    lines.push(Line::from(format!(
                        "Encoded image payload: {encoded_bytes} bytes (body withheld)"
                    )));
                }
                if metadata.opaque_signature_present {
                    lines.push(Line::from(
                        "Opaque provider signature: present (value withheld)",
                    ));
                }
                if metadata.encrypted_state_present {
                    lines.push(Line::from(
                        "Encrypted provider state: present (value withheld)",
                    ));
                }
                if metadata.format != ContextMessageDetailFormat::MetadataOnly {
                    lines.push(Line::from(""));
                    if loaded_text.is_empty() && metadata.content.total_chars == 0 {
                        lines.push(Line::from("(empty content)"));
                    } else {
                        lines.extend(
                            loaded_text
                                .split('\n')
                                .map(|line| Line::from(line.to_string())),
                        );
                    }
                }
                if let Some(next) = buffer.next_start_char() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!(
                        "Enter loads the next detail chunk starting at character {next}."
                    )));
                }
            } else {
                lines.push(Line::from(
                    "Enter loads the selected block detail from the authoritative transcript.",
                ));
            }
        }
        if message
            .blocks
            .iter()
            .any(|block| block.kind == StoredContextBlockKind::ReasoningTrace)
        {
            lines.push(Line::from(""));
            lines.push(Line::from(
                "ReasoningTrace is transcript-only and contributes zero provider replay tokens.",
            ));
        }
        lines
    }

    fn range_preview_lines(&self, _width: usize) -> Vec<Line<'static>> {
        let Some(preview) = self.pending_range_preview.as_ref() else {
            return vec![Line::from("Waiting for authoritative structural closure…")];
        };
        let mut lines = vec![Line::from(format!(
            "{} range(s) · revision {} · transcript digest {}",
            preview.ranges.len(),
            preview.context_revision,
            preview.transcript_digest
        ))];
        for range in &preview.ranges {
            lines.push(Line::from(""));
            lines.push(Line::from(format!(
                "Requested {}..{}",
                range.requested.start_message_id, range.requested.end_message_id
            )));
            lines.push(Line::from(format!(
                "Closed {}..{} · {} messages · {}",
                range.source_range.start_index_hint,
                range.source_range.end_index_hint,
                range.source_range.message_count,
                format_tokens(range.source_tokens)
            )));
            for expansion in &range.boundary_expansions {
                lines.push(Line::from(format!(
                    "  + message {}: {}",
                    expansion.stored_index_hint,
                    expansion_reason(&expansion.reason)
                )));
            }
        }
        if !preview.shadowed_active_operations.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Shadowed active operations (explicit confirmation required):",
            ));
            for operation in &preview.shadowed_active_operations {
                lines.push(Line::from(format!("  • {operation}")));
            }
        }
        lines
    }

    fn progress_lines(&self) -> Vec<Line<'static>> {
        let progress = self.draft_progress.as_ref();
        vec![
            Line::from("Preparing one atomic context transaction."),
            Line::from(""),
            Line::from(format!(
                "Phase: {}",
                progress
                    .map(|progress| draft_phase_label(progress.phase))
                    .unwrap_or("capturing")
            )),
            Line::from(format!(
                "Items: {}/{}",
                progress
                    .map(|progress| progress.completed_items)
                    .unwrap_or(0),
                progress.map(|progress| progress.total_items).unwrap_or(0)
            )),
            Line::from(""),
            Line::from(
                "The editor remains open. Curator instructions never enter the coding agent context.",
            ),
        ]
    }

    fn review_lines(&self, _width: usize) -> Vec<Line<'static>> {
        let Some(draft) = self.draft.as_ref() else {
            return vec![Line::from("Waiting for ready draft…")];
        };
        let preview = self
            .selection_preview
            .as_ref()
            .map(|selection| &selection.preview)
            .unwrap_or(&draft.preview);
        let mut lines = vec![
            Line::from("Overall transaction"),
            Line::from(format!(
                "Revision {} → {} · raw messages unchanged: {}",
                preview.current_context_revision,
                preview.proposed_context_revision,
                preview.raw_stored_message_count
            )),
            Line::from(format!(
                "Required operations: {} · eligible proposals: {} · selected proposals: {}",
                draft.required_operations.len(),
                draft.distillation_proposals.len(),
                self.selected_distillation_ids.len()
            )),
            Line::from("Provider continuation will be reset once after persistence."),
        ];
        if self.selection_preview_pending {
            lines.push(Line::from(
                "Economics recalculation pending for the current proposal set.",
            ));
        } else {
            lines.push(Line::from(
                "Economics and provider validation reflect the exact selected proposal set.",
            ));
        }
        push_economics_lines(&mut lines, &preview.economics);
        push_validation_lines(
            &mut lines,
            &preview.validation,
            preview.formatter_placeholder_count,
        );
        for notice in &preview.notices {
            lines.push(Line::from(format!("Notice: {notice}")));
        }

        for (operation_index, operation) in draft.required_operations.iter().enumerate() {
            match operation {
                StoredContextOperation::RangeSummary(summary) => {
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!(
                        "Required range summary {} · raw {}..{} · {} message(s)",
                        operation_index + 1,
                        summary.source_range.start_index_hint,
                        summary.source_range.end_index_hint,
                        summary.source_range.message_count,
                    )));
                    lines.push(Line::from(format!(
                        "Source digest {} · tokens {} → {}",
                        summary.source_range.source_digest,
                        format_tokens(summary.source_token_estimate),
                        format_tokens(summary.replacement_token_estimate)
                    )));
                    push_multiline_section(&mut lines, "Summary text", &summary.summary_text);
                    push_multiline_section(
                        &mut lines,
                        "Files changed digest",
                        &summary.file_change_digest,
                    );
                    lines.push(Line::from(format!(
                        "Changed-file evidence: {}",
                        if summary.change_evidence_complete {
                            "complete for recognized structured mutations"
                        } else {
                            "potentially incomplete because indirect changes may exist"
                        }
                    )));
                    if summary.changed_files.is_empty() {
                        lines.push(Line::from("Changed paths: none recorded"));
                    } else {
                        lines.push(Line::from("Changed paths:"));
                        for path in &summary.changed_files {
                            lines.push(Line::from(format!("  • {path}")));
                        }
                    }
                    if summary.boundary_expansions.is_empty() {
                        lines.push(Line::from("Boundary expansions: none"));
                    } else {
                        lines.push(Line::from("Automatic boundary expansions:"));
                        for expansion in &summary.boundary_expansions {
                            lines.push(Line::from(format!(
                                "  • message {} ({}) · {}",
                                expansion.stored_index_hint,
                                expansion.message_id,
                                expansion_reason(&expansion.reason)
                            )));
                        }
                    }
                    if let Some(generator) = summary.generator.as_ref() {
                        push_generator_lines(&mut lines, generator);
                    }
                    for warning in &summary.warnings {
                        lines.push(Line::from(format!("Curator warning: {warning}")));
                    }
                    if let Some(legacy) = summary.legacy_coverage.as_ref() {
                        lines.push(Line::from(format!(
                            "Legacy coverage: through turn {} · original turns {} · compacted messages {}",
                            legacy.covers_up_to_turn,
                            legacy.original_turn_count,
                            legacy.compacted_count
                        )));
                    }
                    lines.push(Line::from(format!("Generated: {}", summary.created_at)));
                }
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!(
                        "Required reasoning suppression {} · {} target(s) · {} assistant turn(s)",
                        operation_index + 1,
                        suppression.targets.len(),
                        suppression.assistant_turns_affected,
                    )));
                    lines.push(Line::from(format!(
                        "Estimated replay tokens removed: {} · validation evidence version {}",
                        format_tokens(suppression.original_token_estimate),
                        suppression.validation_evidence_version
                    )));
                    lines.push(Line::from(format!(
                        "Selection: {}",
                        reasoning_selection_label(&suppression.selection)
                    )));
                    lines.push(Line::from(format!(
                        "Replay block kinds: {}",
                        if suppression.replay_block_kinds.is_empty() {
                            "none".to_string()
                        } else {
                            suppression
                                .replay_block_kinds
                                .iter()
                                .map(|kind| format!("{kind:?}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    )));
                    if suppression
                        .replay_block_kinds
                        .contains(&StoredContextBlockKind::ReasoningTrace)
                    {
                        lines.push(Line::from(
                            "ReasoningTrace is transcript-only and saves zero provider tokens.",
                        ));
                    }
                    push_provider_evidence_lines(&mut lines, &suppression.validation);
                }
                StoredContextOperation::ToolResultDistillation(_) => {}
            }
        }
        if !draft.distillation_proposals.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Eligible distillation proposals:"));
            for (index, proposal) in draft.distillation_proposals.iter().enumerate() {
                let cursor = if index == self.proposal_cursor {
                    "›"
                } else {
                    " "
                };
                let checked = if self
                    .selected_distillation_ids
                    .contains(&proposal.proposal_id)
                {
                    "[x]"
                } else {
                    "[ ]"
                };
                lines.push(Line::from(format!(
                    "{cursor} {checked} {} · proposal {} · call {}",
                    proposal.operation.tool_name,
                    proposal.proposal_id,
                    proposal.operation.tool_call_id,
                )));
                lines.push(Line::from(format!(
                    "    tokens {} → {} · retained {:.2}% · selected by default: {}",
                    format_tokens(proposal.operation.original_token_estimate),
                    format_tokens(proposal.operation.replacement_token_estimate),
                    proposal.operation.replacement_ratio_millionths as f64 / 10_000.0,
                    proposal.selected_by_default
                )));
                lines.push(Line::from(format!(
                    "    target message {} block {} · expected hash {}",
                    proposal.operation.target.message_id,
                    proposal.operation.target.block_ordinal_hint,
                    proposal.operation.target.expected_hash
                )));
                push_multiline_section(
                    &mut lines,
                    "    Replacement content",
                    &proposal.operation.replacement_content,
                );
                push_multiline_section(
                    &mut lines,
                    "    Preservation rationale",
                    &proposal.operation.preservation_rationale,
                );
                for uncertainty in &proposal.operation.uncertainties {
                    lines.push(Line::from(format!("    Uncertainty: {uncertainty}")));
                }
                push_generator_lines(&mut lines, &proposal.operation.generator);
                lines.push(Line::from(format!(
                    "    Generated: {}",
                    proposal.operation.created_at
                )));
            }
        }
        if !draft.ineligible_distillations.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Ineligible outputs:"));
            for item in &draft.ineligible_distillations {
                lines.push(Line::from(format!(
                    "  • {} · request {} · call {}",
                    item.tool_name, item.request_id, item.tool_call_id
                )));
                push_multiline_section(&mut lines, "    Ineligible reason", &item.reason);
                for uncertainty in &item.uncertainties {
                    lines.push(Line::from(format!("    Uncertainty: {uncertainty}")));
                }
            }
        }
        push_curator_usage_lines(&mut lines, &draft.curator_usage);
        lines
    }

    fn history_preview_lines(&self) -> Vec<Line<'static>> {
        let Some(transaction) = self.current_transaction() else {
            return vec![Line::from("No context transactions exist.")];
        };
        let mut lines = vec![
            Line::from(format!("Transaction {}", transaction.id)),
            Line::from(format!("Status: {}", transaction_status(&transaction))),
            Line::from(format!("Created: {}", transaction.created_at)),
            Line::from(format!("Base revision: {}", transaction.base_revision)),
            Line::from(format!(
                "Authorization: {}",
                context_authorization_label(&transaction.authorization)
            )),
            Line::from(format!(
                "Operations: {} summaries · {} reasoning · {} distillations",
                transaction.operation_counts.range_summaries,
                transaction.operation_counts.reasoning_suppressions,
                transaction.operation_counts.tool_result_distillations
            )),
        ];
        if let Some(application) = transaction.application {
            lines.push(Line::from(format!(
                "Applied with {} / {} / {}",
                application.provider, application.model, application.route
            )));
        }
        if let Some(economics) = transaction.economics {
            lines.push(Line::from(format!(
                "Tokens {} → {} · earliest changed {}",
                format_tokens(economics.projected_tokens_before),
                format_tokens(economics.projected_tokens_after),
                economics
                    .earliest_changed_provider_item
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            )));
        }
        lines
    }

    fn transaction_detail_lines(&self, _width: usize) -> Vec<Line<'static>> {
        let Some(detail) = self.transaction_detail.as_ref() else {
            return vec![Line::from("Loading transaction detail…")];
        };
        let transaction = &detail.transaction;
        let mut lines = vec![
            Line::from(format!(
                "Transaction {} · revision {}",
                transaction.id, detail.context_revision
            )),
            Line::from(format!(
                "Authorization: {}",
                context_authorization_label(&transaction.authorization)
            )),
            Line::from(format!("Created: {}", transaction.created_at)),
            Line::from(format!("Base revision: {}", transaction.base_revision)),
            Line::from(format!(
                "Status events: {}",
                transaction.status_events.len()
            )),
        ];
        for event in &transaction.status_events {
            lines.push(Line::from(format!(
                "  • revision {} · {:?} · {}",
                event.revision,
                event.kind,
                event.reason.as_deref().unwrap_or("no reason")
            )));
        }
        if let Some(audit) = transaction.emergency_audit.as_ref() {
            lines.extend(emergency_audit_lines(audit));
        }
        if let Some(application) = transaction.application.as_ref() {
            lines.push(Line::from(format!(
                "Application provider: {} · model: {} · route: {} · context window: {}",
                application.provider,
                application.model,
                application.route,
                application
                    .context_window
                    .map(format_tokens)
                    .unwrap_or_else(|| "unknown".to_string())
            )));
        }
        if let Some(economics) = transaction.economics.as_ref() {
            push_economics_lines(&mut lines, economics);
        }
        for (operation_index, operation) in transaction.operations.iter().enumerate() {
            lines.push(Line::from(""));
            match operation {
                StoredContextOperation::RangeSummary(summary) => {
                    lines.push(Line::from(format!(
                        "Range summary {} · raw {}..{} · {} message(s)",
                        operation_index + 1,
                        summary.source_range.start_index_hint,
                        summary.source_range.end_index_hint,
                        summary.source_range.message_count
                    )));
                    lines.push(Line::from(format!(
                        "Source digest {} · tokens {} → {}",
                        summary.source_range.source_digest,
                        format_tokens(summary.source_token_estimate),
                        format_tokens(summary.replacement_token_estimate)
                    )));
                    push_multiline_section(&mut lines, "Summary text", &summary.summary_text);
                    push_multiline_section(
                        &mut lines,
                        "Files changed digest",
                        &summary.file_change_digest,
                    );
                    lines.push(Line::from(format!(
                        "Changed-file evidence complete: {}",
                        summary.change_evidence_complete
                    )));
                    for path in &summary.changed_files {
                        lines.push(Line::from(format!("Changed path: {path}")));
                    }
                    for expansion in &summary.boundary_expansions {
                        lines.push(Line::from(format!(
                            "Boundary expansion: message {} ({}) · {}",
                            expansion.stored_index_hint,
                            expansion.message_id,
                            expansion_reason(&expansion.reason)
                        )));
                    }
                    if let Some(generator) = summary.generator.as_ref() {
                        push_generator_lines(&mut lines, generator);
                    }
                    for warning in &summary.warnings {
                        lines.push(Line::from(format!("Curator warning: {warning}")));
                    }
                }
                StoredContextOperation::ReasoningSuppression(suppression) => {
                    lines.push(Line::from(format!(
                        "Reasoning suppression {} · {} targets · {} assistant turn(s) · {}",
                        operation_index + 1,
                        suppression.targets.len(),
                        suppression.assistant_turns_affected,
                        format_tokens(suppression.original_token_estimate)
                    )));
                    lines.push(Line::from(format!(
                        "Selection: {} · block kinds: {} · evidence version {}",
                        reasoning_selection_label(&suppression.selection),
                        suppression
                            .replay_block_kinds
                            .iter()
                            .map(|kind| format!("{kind:?}"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        suppression.validation_evidence_version
                    )));
                    push_provider_evidence_lines(&mut lines, &suppression.validation);
                }
                StoredContextOperation::ToolResultDistillation(distillation) => {
                    lines.push(Line::from(format!(
                        "Tool distillation {} · {} · call {} · {} → {} · retained {:.2}%",
                        operation_index + 1,
                        distillation.tool_name,
                        distillation.tool_call_id,
                        format_tokens(distillation.original_token_estimate),
                        format_tokens(distillation.replacement_token_estimate),
                        distillation.replacement_ratio_millionths as f64 / 10_000.0
                    )));
                    lines.push(Line::from(format!(
                        "Target message {} block {} · expected hash {}",
                        distillation.target.message_id,
                        distillation.target.block_ordinal_hint,
                        distillation.target.expected_hash
                    )));
                    push_multiline_section(
                        &mut lines,
                        "Replacement content",
                        &distillation.replacement_content,
                    );
                    push_multiline_section(
                        &mut lines,
                        "Preservation rationale",
                        &distillation.preservation_rationale,
                    );
                    for uncertainty in &distillation.uncertainties {
                        lines.push(Line::from(format!("Uncertainty: {uncertainty}")));
                    }
                    push_generator_lines(&mut lines, &distillation.generator);
                    lines.push(Line::from(format!(
                        "Generated: {}",
                        distillation.created_at
                    )));
                }
            }
        }
        push_curator_usage_lines(&mut lines, &transaction.curator_usage);
        lines
    }

    fn title(&self) -> String {
        let phase = match self.phase {
            ContextEditorPhase::Loading => "loading",
            ContextEditorPhase::Editing => "editing",
            ContextEditorPhase::ConfirmRangeClosure => "confirm closure",
            ContextEditorPhase::PreparingDraft => "preparing review",
            ContextEditorPhase::ReviewDraft => "transaction review",
            ContextEditorPhase::History => "history",
            ContextEditorPhase::InspectTransaction => "transaction detail",
        };
        format!(" Context editor · {phase} ")
    }

    fn visible_message_ids(&self) -> Vec<String> {
        let query = self.search_query.trim().to_ascii_lowercase();
        self.rows
            .values()
            .filter(|message| {
                query.is_empty()
                    || message.preview.to_ascii_lowercase().contains(&query)
                    || message.message_id.to_ascii_lowercase().contains(&query)
                    || message.blocks.iter().any(|block| {
                        block
                            .tool_name
                            .as_deref()
                            .is_some_and(|name| name.to_ascii_lowercase().contains(&query))
                    })
            })
            .map(|message| message.message_id.clone())
            .collect()
    }

    fn current_message(&self) -> Option<ContextEditorMessage> {
        let ids = self.visible_message_ids();
        ids.get(self.cursor)
            .and_then(|id| self.message_by_id(id))
            .cloned()
    }

    fn message_by_id(&self, id: &str) -> Option<&ContextEditorMessage> {
        self.rows.values().find(|message| message.message_id == id)
    }

    fn current_transaction(&self) -> Option<ContextTransactionSummary> {
        self.history.get(self.history_cursor).cloned()
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible_message_ids().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = if delta.is_negative() {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as usize).min(len - 1)
        };
        self.block_cursor = 0;
        self.preview_scroll = 0;
    }

    fn move_block(&mut self, delta: isize) {
        let count = self
            .current_message()
            .map(|message| message.blocks.len())
            .unwrap_or(0);
        if count == 0 {
            self.block_cursor = 0;
            return;
        }
        self.block_cursor = if delta.is_negative() {
            self.block_cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.block_cursor
                .saturating_add(delta as usize)
                .min(count - 1)
        };
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self
            .cursor
            .min(self.visible_message_ids().len().saturating_sub(1));
        self.block_cursor = self.block_cursor.min(
            self.current_message()
                .map(|message| message.blocks.len())
                .unwrap_or(0)
                .saturating_sub(1),
        );
    }

    fn toggle_current_tool_result(&mut self) {
        let Some(message) = self.current_message() else {
            return;
        };
        let Some(block) = message.blocks.get(self.block_cursor) else {
            self.error = Some("The selected message has no block at this position.".to_string());
            return;
        };
        if block.kind != StoredContextBlockKind::ToolResult {
            self.error =
                Some("Select a ToolResult block with Left/Right before pressing d.".to_string());
            return;
        }
        if self.message_in_staged_range(message.stored_index) {
            self.error = Some(
                "This result is already inside a staged summary range and would be shadowed."
                    .to_string(),
            );
            return;
        }
        let target = (message.message_id, block.ordinal);
        if !self.tool_targets.remove(&target) {
            self.tool_targets.insert(target);
        }
        self.invalidate_curator_plan();
        self.error = None;
    }

    fn scan_tool_results(&mut self, minimum_tokens: usize, protected_recent_turns: usize) {
        let previous_target_count = self.tool_targets.len();
        let protected_start = self.protected_recent_assistant_start(protected_recent_turns);
        let staged_intervals = self
            .staged_ranges
            .iter()
            .map(|range| {
                (
                    range.source_range.start_index_hint,
                    range.source_range.end_index_hint,
                )
            })
            .collect::<Vec<_>>();
        for message in self.rows.values() {
            if protected_start.is_some_and(|start| message.stored_index >= start)
                || staged_intervals.iter().any(|(start, end)| {
                    *start <= message.stored_index && message.stored_index <= *end
                })
            {
                continue;
            }
            for block in &message.blocks {
                if block.kind == StoredContextBlockKind::ToolResult
                    && block.estimated_provider_tokens >= minimum_tokens
                {
                    self.tool_targets
                        .insert((message.message_id.clone(), block.ordinal));
                }
            }
        }
        self.status = Some(format!(
            "Marked {} mechanical candidate(s); curator eligibility is still required.",
            self.tool_targets.len()
        ));
        if self.tool_targets.len() != previous_target_count {
            self.invalidate_curator_plan();
        }
    }

    fn protected_recent_assistant_start(&self, protected: usize) -> Option<usize> {
        if protected == 0 {
            return None;
        }
        let assistants = self
            .rows
            .values()
            .filter(|message| message.role == Role::Assistant)
            .map(|message| message.stored_index)
            .collect::<Vec<_>>();
        assistants
            .get(assistants.len().saturating_sub(protected))
            .copied()
    }

    fn selected_message_runs(&self) -> Vec<ContextMessageRangeSelection> {
        let mut selected = self
            .rows
            .values()
            .filter(|message| self.selected_message_ids.contains(&message.message_id))
            .map(|message| (message.stored_index, message.message_id.clone()))
            .collect::<Vec<_>>();
        selected.sort_by_key(|(index, _)| *index);
        let mut runs = Vec::new();
        let mut start: Option<(usize, String)> = None;
        let mut previous: Option<(usize, String)> = None;
        for (index, id) in selected {
            match previous.as_ref() {
                Some((previous_index, _)) if previous_index.saturating_add(1) == index => {}
                Some((_, previous_id)) => {
                    let (_, start_id) = start.take().expect("selected run has a start");
                    runs.push(ContextMessageRangeSelection {
                        start_message_id: start_id,
                        end_message_id: previous_id.clone(),
                    });
                    start = Some((index, id.clone()));
                }
                None => start = Some((index, id.clone())),
            }
            previous = Some((index, id));
        }
        if let (Some((_, start_id)), Some((_, end_id))) = (start, previous) {
            runs.push(ContextMessageRangeSelection {
                start_message_id: start_id,
                end_message_id: end_id,
            });
        }
        runs
    }

    fn message_in_staged_range(&self, index: usize) -> bool {
        self.staged_ranges.iter().any(|range| {
            range.source_range.start_index_hint <= index
                && index <= range.source_range.end_index_hint
        })
    }

    fn apply_disabled_reason(&self) -> Option<&'static str> {
        let Some(draft) = self.draft.as_ref() else {
            return Some("no ready draft is available");
        };
        if draft.required_operations.is_empty() && self.selected_distillation_ids.is_empty() {
            return Some("the review contains no provider-context changes");
        }
        if self.stale {
            return Some("draft is stale");
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Some("authoritative snapshot is unavailable");
        };
        if snapshot.session_id != draft.identity.session_id
            || snapshot.context_revision != draft.identity.base_context_revision
            || snapshot.transcript_digest != draft.identity.transcript_digest
            || snapshot.raw_message_count != draft.identity.raw_message_count
            || snapshot.provider_name != draft.identity.provider_name
            || snapshot.model != draft.identity.model
            || snapshot.route != draft.identity.route
        {
            return Some("draft identity no longer matches the snapshot");
        }
        if snapshot.processing {
            return Some("session is processing");
        }
        if draft.required_operations.is_empty() && draft.distillation_proposals.is_empty() {
            return Some("draft contains no operations");
        }
        if draft.required_operations.iter().any(|operation| {
            matches!(operation, StoredContextOperation::RangeSummary(summary) if summary.summary_text.trim().is_empty())
        }) {
            return Some("required summary artifact is missing");
        }
        if self.selection_preview_pending {
            return Some("proposal economics are not current");
        }
        let Some(selection_preview) = self.selection_preview.as_ref() else {
            return Some("proposal economics are not current");
        };
        let selected_ids = self
            .selected_distillation_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if selection_preview.draft_id != draft.identity.draft_id
            || selection_preview.selected_distillation_ids != selected_ids
        {
            return Some("proposal economics do not match the selected IDs");
        }
        if selected_ids.iter().any(|selected| {
            !draft
                .distillation_proposals
                .iter()
                .any(|proposal| proposal.proposal_id == *selected)
        }) {
            return Some("selected proposal is not part of the ready draft");
        }
        if selection_preview.preview.current_context_revision
            != draft.identity.base_context_revision
            || selection_preview.preview.raw_stored_message_count
                != draft.identity.raw_message_count
        {
            return Some("selection preview identity is stale");
        }
        if !matches!(
            selection_preview.preview.validation.builder_status,
            jcode_provider_core::ContextProjectionValidationStatus::Supported
        ) {
            return Some("provider validation failed");
        }
        None
    }

    fn apply_confirmation_text(&self) -> String {
        let Some(draft) = self.draft.as_ref() else {
            return "No ready draft is available.".to_string();
        };
        let Some(selection_preview) = self.selection_preview.as_ref() else {
            return "Exact proposal economics are not current. Recalculate the selected proposal set before applying."
                .to_string();
        };
        let preview = &selection_preview.preview;
        format!(
            "Apply {} required operation(s) and {} selected distillation(s)?\nProjected provider tokens {} → {}.\nRaw stored messages remain unchanged.\n\nEnter/y confirm · n/Esc cancel",
            draft.required_operations.len(),
            self.selected_distillation_ids.len(),
            format_tokens(preview.economics.projected_tokens_before),
            format_tokens(preview.economics.projected_tokens_after)
        )
    }

    fn total_rows(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.raw_message_count)
            .unwrap_or(0)
    }

    pub fn debug_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "open": true,
            "open_mode": self.open_mode,
            "phase": self.phase,
            "modal": self.modal,
            "session_id": self.session_id().map(short_id),
            "context_revision": self.context_revision(),
            "loaded_rows": self.rows.len(),
            "loaded_detail_blocks": self.detail_buffers.len(),
            "loaded_detail_chunks": self.detail_buffers.values().map(|buffer| buffer.chunks.len()).sum::<usize>(),
            "total_rows": self.total_rows(),
            "selected_global_index": self.current_message().map(|message| message.stored_index),
            "selected_message_id": self.current_message().map(|message| short_id(&message.message_id)),
            "staged_ranges": self.staged_ranges.len(),
            "selected_messages": self.selected_message_ids.len(),
            "tool_targets": self.tool_targets.len(),
            "proposal_selections": self.selected_distillation_ids.len(),
            "draft_id": self.draft_id.as_deref().map(short_id),
            "draft_phase": self.draft_progress.as_ref().map(|progress| draft_phase_label(progress.phase)),
            "history_count": self.history.len(),
            "selected_transaction_id": self.current_transaction().map(|transaction| short_id(&transaction.id)),
            "pane_focus": self.focus,
            "preview_scroll": self.preview_scroll,
            "operations_scroll": self.operations_scroll,
            "narrow_layout": self.narrow_layout,
            "selection_preview_pending": self.selection_preview_pending,
            "curator_workspace_active": self.curator_workspace.active,
            "curator_workspace_section": self.curator_workspace.section,
            "curator_workspace_pane": self.curator_workspace.pane,
            "curator_workspace_narrow_detail": self.curator_workspace.narrow_detail_open,
            "curator_route_search_active": self.curator_workspace.route_search_active,
            "curator_instruction_editing": self.curator_workspace.instruction_editing,
            "curator_task_cursor": self.curator_workspace.task_cursor,
            "curator_review_cursor": self.curator_workspace.review_cursor,
            "curator_plan_dirty": self.curator_workspace.plan_dirty_reason.is_some(),
            "curator_generation_outcome": self.curator_workspace.generation_outcome,
            "curator_plan_pending": self.curator_plan_pending,
            "curator_plan_tasks": self.curator_plan.as_ref().map(|plan| plan.tasks.len()),
            "curator_plan_current": self.curator_plan.is_some()
                && self.curator_plan_request.as_ref() == Some(&self.current_draft_request()),
            "curator_route_options": self.snapshot.as_ref().map(|snapshot| snapshot.curator_route_options.len()).unwrap_or(0),
            "has_error": self.error.is_some(),
            "stale": self.stale,
        })
    }
}

fn draft_state_signature(state: &ContextClientDraftState) -> String {
    match state {
        ContextClientDraftState::Progress { draft_id, progress } => format!(
            "progress:{draft_id}:{:?}:{}:{}",
            progress.phase, progress.completed_items, progress.total_items
        ),
        ContextClientDraftState::Ready(draft) => format!(
            "ready:{}:{}:{}",
            draft.identity.draft_id,
            draft.preview.proposed_context_revision,
            draft.distillation_proposals.len()
        ),
        ContextClientDraftState::Applying(identity) => format!("applying:{}", identity.draft_id),
        ContextClientDraftState::Applied {
            identity,
            transaction_id,
            revision,
        } => format!("applied:{}:{transaction_id}:{revision}", identity.draft_id),
        ContextClientDraftState::Failed {
            identity, stale, ..
        } => {
            format!("failed:{}:{stale}", identity.draft_id)
        }
        ContextClientDraftState::Canceled(identity) => format!("canceled:{}", identity.draft_id),
        ContextClientDraftState::Expired(identity) => format!("expired:{}", identity.draft_id),
    }
}

fn emergency_policy_summary(policy: &jcode_session_types::StoredContextEmergencyPolicy) -> String {
    match policy {
        jcode_session_types::StoredContextEmergencyPolicy::Block => {
            "Emergency context policy: block · no unattended context surgery authorized".to_string()
        }
        jcode_session_types::StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns,
            target_headroom_percent,
            allow_reasoning_suppression,
            allow_tool_distillation,
            allow_oldest_range_summary,
            ..
        } => format!(
            "Emergency context policy: authorized · protect latest {protected_recent_assistant_turns} assistant turn(s) · target {target_headroom_percent}% headroom · reasoning {} · tool distillation {} · oldest-range summary {}",
            enabled_label(*allow_reasoning_suppression),
            enabled_label(*allow_tool_distillation),
            enabled_label(*allow_oldest_range_summary),
        ),
    }
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "allowed" } else { "blocked" }
}

fn parse_tool_scan_input(input: &str) -> Result<(usize, usize), String> {
    let values = input.split_whitespace().collect::<Vec<_>>();
    if values.len() != 2 {
        return Err(
            "Tool scan requires exactly two integers: minimum_tokens protected_recent_turns."
                .to_string(),
        );
    }
    let minimum_tokens = values[0]
        .parse::<usize>()
        .map_err(|_| "Tool scan minimum_tokens must be a non-negative integer.".to_string())?;
    let protected_recent_turns = values[1].parse::<usize>().map_err(|_| {
        "Tool scan protected_recent_turns must be an integer from 0 to 1000.".to_string()
    })?;
    if protected_recent_turns > 1_000 {
        return Err(
            "Tool scan protected_recent_turns must be an integer from 0 to 1000.".to_string(),
        );
    }
    Ok((minimum_tokens, protected_recent_turns))
}

fn parse_emergency_policy_input(
    input: &str,
    session_id: &str,
) -> Result<StoredContextEmergencyPolicy, String> {
    let parts = input.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err(
            "Enter exactly: protected_turns headroom_percent reasoning tools oldest_summary"
                .to_string(),
        );
    }
    let protected_recent_assistant_turns = parts[0]
        .parse::<usize>()
        .map_err(|_| "Protected turns must be an integer from 0 to 1000.".to_string())?;
    if protected_recent_assistant_turns > 1_000 {
        return Err("Protected turns must be an integer from 0 to 1000.".to_string());
    }
    let target_headroom_percent = parts[1]
        .parse::<u8>()
        .map_err(|_| "Target headroom must be an integer from 1 to 99.".to_string())?;
    if !(1..=99).contains(&target_headroom_percent) {
        return Err("Target headroom must be an integer from 1 to 99.".to_string());
    }
    let parse_flag = |value: &str, label: &str| match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("{label} must be 0 or 1.")),
    };
    let allow_reasoning_suppression = parse_flag(parts[2], "Reasoning")?;
    let allow_tool_distillation = parse_flag(parts[3], "Tools")?;
    let allow_oldest_range_summary = parse_flag(parts[4], "Oldest summary")?;
    if !allow_reasoning_suppression && !allow_tool_distillation && !allow_oldest_range_summary {
        return Err("Authorized policy must enable at least one operation category.".to_string());
    }
    Ok(StoredContextEmergencyPolicy::Authorized {
        protected_recent_assistant_turns,
        target_headroom_percent,
        allow_reasoning_suppression,
        allow_tool_distillation,
        allow_oldest_range_summary,
        authorization_source: format!("context_editor_session:{session_id}"),
    })
}

fn metadata_only_chunk(total_chars: usize) -> ContextTextChunk {
    ContextTextChunk {
        start_char: 0,
        end_char: 0,
        total_chars,
        text: String::new(),
        next_start_char: None,
    }
}

fn canonical_editor_range_key(selection: &ContextMessageRangeSelection) -> (String, String) {
    if selection.start_message_id <= selection.end_message_id {
        (
            selection.start_message_id.clone(),
            selection.end_message_id.clone(),
        )
    } else {
        (
            selection.end_message_id.clone(),
            selection.start_message_id.clone(),
        )
    }
}

fn validate_detail_chunk(chunk: &ContextTextChunk) -> Result<(), String> {
    if chunk.start_char > chunk.end_char || chunk.end_char > chunk.total_chars {
        return Err(format!(
            "Invalid context detail bounds {}..{} for {} total characters.",
            chunk.start_char, chunk.end_char, chunk.total_chars
        ));
    }
    let actual_chars = chunk.text.chars().count();
    let expected_chars = chunk.end_char.saturating_sub(chunk.start_char);
    if actual_chars != expected_chars {
        return Err(format!(
            "Context detail chunk {}..{} declared {expected_chars} characters but carried {actual_chars}.",
            chunk.start_char, chunk.end_char
        ));
    }
    let expected_next = (chunk.end_char < chunk.total_chars).then_some(chunk.end_char);
    if chunk.next_start_char != expected_next {
        return Err(format!(
            "Context detail continuation for chunk {}..{} was {:?}; expected {:?}.",
            chunk.start_char, chunk.end_char, chunk.next_start_char, expected_next
        ));
    }
    Ok(())
}

fn push_multiline_section(lines: &mut Vec<Line<'static>>, label: &str, text: &str) {
    lines.push(Line::from(format!("{label}:")));
    if text.is_empty() {
        lines.push(Line::from("  (empty)"));
        return;
    }
    lines.extend(text.split('\n').map(|line| Line::from(format!("  {line}"))));
}

fn push_economics_lines(
    lines: &mut Vec<Line<'static>>,
    economics: &jcode_session_types::StoredContextEconomics,
) {
    lines.push(Line::from(""));
    lines.push(Line::from("Token and cache economics"));
    lines.push(Line::from(format!(
        "Projected provider tokens {} → {} · deleted {}",
        format_tokens(economics.projected_tokens_before),
        format_tokens(economics.projected_tokens_after),
        format_tokens(economics.deleted_input_tokens)
    )));
    lines.push(Line::from(format!(
        "Estimated whole request {} → {}",
        economics
            .estimated_total_request_tokens_before
            .map(format_tokens)
            .unwrap_or_else(|| "unknown".to_string()),
        economics
            .estimated_total_request_tokens_after
            .map(format_tokens)
            .unwrap_or_else(|| "unknown".to_string())
    )));
    lines.push(Line::from(format!(
        "Unchanged provider prefix: {} item(s) · earliest changed item: {}",
        economics.unchanged_prefix_items,
        economics
            .earliest_changed_provider_item
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    )));
    lines.push(Line::from(format!(
        "Cache-affected suffix {} → {}",
        format_tokens(economics.old_affected_suffix_tokens),
        format_tokens(economics.new_affected_suffix_tokens)
    )));
    lines.push(Line::from(format!(
        "Context window: {} · safe input budget: {}",
        economics
            .context_window
            .map(format_tokens)
            .unwrap_or_else(|| "unknown".to_string()),
        economics
            .safe_input_budget
            .map(format_tokens)
            .unwrap_or_else(|| "unknown".to_string())
    )));
    if let (Some(window), Some(before), Some(after)) = (
        economics.context_window,
        economics.estimated_total_request_tokens_before,
        economics.estimated_total_request_tokens_after,
    ) && window > 0
    {
        lines.push(Line::from(format!(
            "Estimated context-window usage: {:.1}% → {:.1}%",
            before as f64 * 100.0 / window as f64,
            after as f64 * 100.0 / window as f64
        )));
    }
    lines.push(Line::from(format!(
        "First request delta: {} · recurring savings per turn: {} · break-even: {}",
        format_optional_usd(economics.first_request_delta_usd),
        format_optional_usd(economics.recurring_savings_per_turn_usd),
        economics
            .break_even_turns
            .map(|turns| format!("{turns} turn(s)"))
            .unwrap_or_else(|| "unknown".to_string())
    )));
    if let Some(pricing) = economics.pricing.as_ref() {
        lines.push(Line::from(format!(
            "Pricing evidence: {:?} billing · cache warmth {:?}",
            pricing.billing_mode, pricing.cache_warmth
        )));
        lines.push(Line::from(format!(
            "Rates per million: input {} · output {} · cache read {} · cache write {}",
            format_optional_usd(pricing.input_usd_per_million),
            format_optional_usd(pricing.output_usd_per_million),
            format_optional_usd(pricing.cache_read_usd_per_million),
            format_optional_usd(pricing.cache_write_usd_per_million)
        )));
        for tier in &pricing.input_price_tiers {
            lines.push(Line::from(format!(
                "Input price tier above {}: input ${:.6} · output {} · cache read {} · cache write {} per million",
                format_tokens(tier.above_input_tokens),
                tier.input_usd_per_million,
                format_optional_usd(tier.output_usd_per_million),
                format_optional_usd(tier.cache_read_usd_per_million),
                format_optional_usd(tier.cache_write_usd_per_million)
            )));
        }
    } else {
        lines.push(Line::from(
            "Pricing evidence: unavailable; no dollar precision claimed.",
        ));
    }
    for assumption in &economics.assumptions {
        lines.push(Line::from(format!("Economics assumption: {assumption}")));
    }
}

fn push_validation_lines(
    lines: &mut Vec<Line<'static>>,
    validation: &jcode_provider_core::ContextProjectionValidationReport,
    formatter_placeholder_count: usize,
) {
    lines.push(Line::from(""));
    lines.push(Line::from("Provider validation"));
    lines.push(Line::from(format!(
        "{} / {} / {:?} · model {} · status {:?}",
        validation.provider_display_name,
        validation.provider_name,
        validation.provider_family,
        validation.model,
        validation.builder_status
    )));
    lines.push(Line::from(format!(
        "Evidence tag: {} · normalized items: {} · formatter placeholders: {} (preview {})",
        validation.evidence_tag,
        validation.normalized_item_count,
        validation.formatter_placeholder_count,
        formatter_placeholder_count
    )));
    for note in &validation.normalization_notes {
        lines.push(Line::from(format!("Normalization note: {note}")));
    }
    for finding in &validation.findings {
        lines.push(Line::from(format!(
            "Finding {:?} / {:?} / {:?}{}: {}",
            finding.status,
            finding.stage,
            finding.operation_kind,
            finding
                .operation_id
                .as_deref()
                .map(|id| format!(" / {id}"))
                .unwrap_or_default(),
            finding.reason
        )));
    }
}

fn push_generator_lines(
    lines: &mut Vec<Line<'static>>,
    generator: &jcode_session_types::StoredContextArtifactGenerator,
) {
    lines.push(Line::from(format!(
        "Generator: {} / {} / {} · prompt {} · effort {}",
        generator.provider,
        generator.model,
        generator.route,
        generator.prompt_version,
        generator.effort.as_deref().unwrap_or("unspecified")
    )));
}

fn push_provider_evidence_lines(
    lines: &mut Vec<Line<'static>>,
    evidence: &[jcode_session_types::StoredProviderValidationEvidence],
) {
    if evidence.is_empty() {
        lines.push(Line::from("Stored provider validation evidence: none"));
        return;
    }
    lines.push(Line::from("Stored provider validation evidence:"));
    for item in evidence {
        lines.push(Line::from(format!(
            "  • {} / {} · builder {} · {:?} · checked {}",
            item.provider, item.model, item.request_builder, item.outcome, item.checked_at
        )));
        for warning in &item.warnings {
            lines.push(Line::from(format!("    Warning: {warning}")));
        }
    }
}

fn push_curator_usage_lines(
    lines: &mut Vec<Line<'static>>,
    usage: &[jcode_session_types::StoredContextCuratorUsage],
) {
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Curator usage records: {}",
        usage.len()
    )));
    for item in usage {
        lines.push(Line::from(format!(
            "  • {} / {} / {} · input {} · output {} · cache read {} · cache creation {} · cost {}",
            item.provider,
            item.model,
            item.route,
            item.input_tokens,
            item.output_tokens,
            item.cache_read_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            item.cache_creation_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            format_optional_usd(item.cost_usd)
        )));
    }
}

fn reasoning_selection_label(selection: &jcode_session_types::StoredReasoningSelection) -> String {
    match selection {
        jcode_session_types::StoredReasoningSelection::KeepLatestAssistantTurns {
            protected_recent_assistant_turns,
        } => format!(
            "keep replayed reasoning in the latest {protected_recent_assistant_turns} assistant turn(s)"
        ),
        jcode_session_types::StoredReasoningSelection::MessageRanges { ranges } => format!(
            "manual message ranges: {}",
            ranges
                .iter()
                .map(|range| format!("{}..{}", range.start_index_hint, range.end_index_hint))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn replayed_reasoning_kind(kind: StoredContextBlockKind) -> bool {
    matches!(
        kind,
        StoredContextBlockKind::Reasoning
            | StoredContextBlockKind::AnthropicThinking
            | StoredContextBlockKind::OpenAiReasoning
    )
}

fn format_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{:.2}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn format_optional_usd(value: Option<f64>) -> String {
    value
        .map(|value| format!("${value:.4}"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "U",
        Role::Assistant => "A",
    }
}

fn one_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn short_id(id: &str) -> String {
    let count = id.chars().count();
    if count <= 16 {
        id.to_string()
    } else {
        let prefix = id.chars().take(8).collect::<String>();
        let suffix = id.chars().skip(count.saturating_sub(5)).collect::<String>();
        format!("{prefix}…{suffix}")
    }
}

fn draft_phase_label(phase: ContextDraftPhase) -> &'static str {
    match phase {
        ContextDraftPhase::Capturing => "capturing selections",
        ContextDraftPhase::ClosingRanges => "closing ranges",
        ContextDraftPhase::ExtractingChangeEvidence => "extracting change evidence",
        ContextDraftPhase::PreparingArtifacts => "preparing summaries and outputs",
        ContextDraftPhase::ValidatingProjection => "validating provider projection",
        ContextDraftPhase::CalculatingEconomics => "calculating cache economics",
        ContextDraftPhase::Ready => "ready",
    }
}

fn transaction_status(transaction: &ContextTransactionSummary) -> &'static str {
    match transaction.latest_status {
        Some(StoredContextTransactionStatusKind::Applied) => "applied",
        Some(StoredContextTransactionStatusKind::Reverted) => "reverted",
        Some(StoredContextTransactionStatusKind::Reapplied) => "reapplied",
        Some(StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit) => "invalidated",
        None => "unknown",
    }
}

fn context_authorization_label(
    authorization: &jcode_session_types::StoredContextAuthorization,
) -> String {
    match authorization {
        jcode_session_types::StoredContextAuthorization::Manual { .. } => "manual".to_string(),
        jcode_session_types::StoredContextAuthorization::UnattendedEmergency {
            scheduled_item_id,
            ..
        } => scheduled_item_id
            .as_ref()
            .map(|id| format!("unattended emergency · scheduled item {id}"))
            .unwrap_or_else(|| "unattended emergency · session authorization".to_string()),
        jcode_session_types::StoredContextAuthorization::LegacyMigration { source } => {
            format!("legacy migration · {source:?}")
        }
    }
}

fn emergency_audit_lines(
    audit: &jcode_session_types::StoredContextEmergencyAudit,
) -> Vec<Line<'static>> {
    let (protected_turns, target_headroom, reasoning, tools, summary) = match &audit.policy {
        StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns,
            target_headroom_percent,
            allow_reasoning_suppression,
            allow_tool_distillation,
            allow_oldest_range_summary,
            ..
        } => (
            *protected_recent_assistant_turns,
            *target_headroom_percent,
            *allow_reasoning_suppression,
            *allow_tool_distillation,
            *allow_oldest_range_summary,
        ),
        StoredContextEmergencyPolicy::Block => (0, 0, false, false, false),
    };
    let retry = match &audit.retry_outcome {
        jcode_session_types::StoredContextEmergencyRetryOutcome::Pending => "pending".to_string(),
        jcode_session_types::StoredContextEmergencyRetryOutcome::Succeeded => {
            "succeeded".to_string()
        }
        jcode_session_types::StoredContextEmergencyRetryOutcome::Blocked {
            required_reduction_tokens,
        } => format!(
            "blocked · reduce by {}",
            format_tokens(*required_reduction_tokens)
        ),
        jcode_session_types::StoredContextEmergencyRetryOutcome::ProviderRejected => {
            "provider rejected".to_string()
        }
        jcode_session_types::StoredContextEmergencyRetryOutcome::Failed { .. } => {
            "failed · detail retained in persisted audit".to_string()
        }
    };
    vec![
        Line::from("Emergency context surgery audit:"),
        Line::from(format!(
            "  Trigger: {:?} · scheduled item: {} · retry: {retry}",
            audit.trigger_kind,
            audit.scheduled_item_id.as_deref().unwrap_or("none")
        )),
        Line::from(format!(
            "  Policy: protect {protected_turns} recent assistant turn(s) · target {target_headroom}% headroom · reasoning {} · tools {} · oldest summary {}",
            enabled_label(reasoning),
            enabled_label(tools),
            enabled_label(summary)
        )),
        Line::from(format!(
            "  Budget: projected {} · safe {} · fit reduction {} · target reduction {} · achieved {}",
            format_tokens(audit.projected_input_tokens),
            format_tokens(audit.safe_input_budget),
            format_tokens(audit.required_reduction_to_fit_tokens),
            format_tokens(audit.required_reduction_to_target_tokens),
            format_tokens(audit.achieved_reduction_tokens)
        )),
        Line::from(format!(
            "  Protected messages: {} · operation order: {} · provider error recorded: {}",
            audit.protected_message_count,
            audit
                .operation_order
                .iter()
                .map(|operation| format!("{operation:?}"))
                .collect::<Vec<_>>()
                .join(" → "),
            audit.provider_error.is_some()
        )),
    ]
}

fn expansion_reason(reason: &StoredRangeBoundaryExpansionReason) -> String {
    match reason {
        StoredRangeBoundaryExpansionReason::ToolPair { tool_use_id } => {
            format!("tool pair {tool_use_id}")
        }
        StoredRangeBoundaryExpansionReason::ParallelToolGroup => "parallel tool group".to_string(),
        StoredRangeBoundaryExpansionReason::AssociatedToolImage { tool_use_id } => tool_use_id
            .as_ref()
            .map(|id| format!("associated image for tool {id}"))
            .unwrap_or_else(|| "associated tool image".to_string()),
        StoredRangeBoundaryExpansionReason::ToolThoughtSignature { tool_use_id } => {
            format!("provider thought signature for tool {tool_use_id}")
        }
        StoredRangeBoundaryExpansionReason::ExistingSummaryBoundary { transaction_id } => {
            format!("existing summary boundary {transaction_id}")
        }
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn safe_transaction_metadata(detail: &ContextTransactionDetail) -> String {
    let transaction = &detail.transaction;
    format!(
        "context transaction {} · revision {} · status events {} · operations {} · curator usage records {}",
        transaction.id,
        detail.context_revision,
        transaction.status_events.len(),
        transaction.operations.len(),
        transaction.curator_usage.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ContextClosedRangePreview, ContextDistillationProposal, ContextDraftIdentity,
        ContextDraftPreview, ContextEditorBlock, ContextIneligibleDistillation,
        ContextOperationCounts, ContextTransactionResult,
    };
    use crate::tui::app::context_protocol::ContextTransactionOutcome;
    use chrono::{DateTime, Duration, Utc};
    use jcode_provider_core::{
        ContextProjectionOperationKind, ContextProjectionValidationFinding,
        ContextProjectionValidationReport, ContextProjectionValidationStage,
        ContextProjectionValidationStatus, ContextProviderFamily,
    };
    use jcode_session_types::{
        StoredContentTarget, StoredContextArtifactGenerator, StoredContextCuratorUsage,
        StoredContextEconomics, StoredContextStatusEvent, StoredContextTransaction,
        StoredProviderValidationEvidence, StoredProviderValidationOutcome,
        StoredRangeBoundaryExpansion, StoredRangeSummary, StoredReasoningSelection,
        StoredReasoningSuppression, StoredToolResultDistillation,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-12T18:00:00Z")
            .expect("valid context editor fixture timestamp")
            .with_timezone(&Utc)
    }

    fn message(index: usize, preview: &str) -> ContextEditorMessage {
        ContextEditorMessage {
            message_id: format!("message-{index}"),
            stored_index: index,
            role: if index.is_multiple_of(2) {
                Role::User
            } else {
                Role::Assistant
            },
            display_role: None,
            timestamp: Some(timestamp()),
            raw_provider_tokens: 10,
            projected_provider_tokens: 10,
            preview: preview.to_string(),
            blocks: vec![ContextEditorBlock {
                ordinal: 0,
                kind: StoredContextBlockKind::Text,
                semantic_id: None,
                estimated_provider_tokens: 10,
                tool_name: None,
                tool_use_id: None,
                tool_result_is_error: false,
                has_image_payload: false,
                has_tool_thought_signature: false,
                provider_removable_reasoning: false,
                active_operations: Vec::new(),
            }],
            tool_group_ids: Vec::new(),
            summary_coverage: None,
            active_operations: Vec::new(),
            removable_reasoning_kinds: Vec::new(),
        }
    }

    fn snapshot() -> ContextEditorSnapshot {
        ContextEditorSnapshot {
            session_id: "session-1".to_string(),
            context_revision: 4,
            raw_message_count: 3,
            transcript_digest: 99,
            processing: false,
            provider_name: "openai".to_string(),
            provider_display_name: "OpenAI".to_string(),
            model: "gpt-test".to_string(),
            route: "oauth".to_string(),
            context_window: 372_000,
            projected_request_tokens: 10_000,
            message_page_start: 0,
            message_page_end: 3,
            next_message_page_start: None,
            messages: vec![message(0, "alpha"), message(1, "beta"), message(2, "gamma")],
            active_transactions: Vec::new(),
            emergency_policy: jcode_session_types::StoredContextEmergencyPolicy::Block,
            curator_route: Some(crate::protocol::ContextCuratorRoutePreview {
                provider_name: "openai".to_string(),
                provider_display_name: "OpenAI".to_string(),
                model: "gpt-curator-test".to_string(),
                route: "oauth".to_string(),
                effort: Some("high".to_string()),
            }),
            curator_unavailable_reason: None,
            curator_default: ContextCuratorSelection {
                provider: Some("openai".to_string()),
                route: Some("oauth".to_string()),
                model: Some("gpt-curator-test".to_string()),
                effort: Some("high".to_string()),
            },
            curator_route_options: vec![crate::protocol::ContextCuratorRouteOption {
                provider: "anthropic".to_string(),
                route: "anthropic-api".to_string(),
                model: "claude-fable-5".to_string(),
                detail: "Anthropic API route".to_string(),
                efforts: vec!["low".to_string(), "high".to_string()],
            }],
        }
    }

    fn snapshot_page(start: usize, end: usize) -> ContextEditorSnapshot {
        let mut page = snapshot();
        page.message_page_start = start;
        page.message_page_end = end;
        page.next_message_page_start = (end < page.raw_message_count).then_some(end);
        page.messages = (start..end)
            .map(|index| message(index, ["alpha", "beta", "gamma"][index]))
            .collect();
        page
    }

    fn economics(before: usize, after: usize) -> StoredContextEconomics {
        StoredContextEconomics {
            projected_tokens_before: before,
            projected_tokens_after: after,
            estimated_total_request_tokens_before: Some(before.saturating_add(1_000)),
            estimated_total_request_tokens_after: Some(after.saturating_add(1_000)),
            unchanged_prefix_items: 2,
            earliest_changed_provider_item: Some(2),
            old_affected_suffix_tokens: before.saturating_sub(2_000),
            new_affected_suffix_tokens: after.saturating_sub(2_000),
            deleted_input_tokens: before.saturating_sub(after),
            context_window: Some(372_000),
            safe_input_budget: Some(370_000),
            pricing: None,
            first_request_delta_usd: None,
            recurring_savings_per_turn_usd: None,
            break_even_turns: None,
            assumptions: vec!["subscription route; dollars unknown".to_string()],
        }
    }

    fn validation(status: ContextProjectionValidationStatus) -> ContextProjectionValidationReport {
        ContextProjectionValidationReport {
            provider_family: ContextProviderFamily::OpenAiResponses,
            provider_name: "openai".to_string(),
            provider_display_name: "OpenAI".to_string(),
            model: "gpt-test".to_string(),
            evidence_tag: "context-editor-test-v1".to_string(),
            builder_status: status,
            normalized_item_count: 3,
            formatter_placeholder_count: 0,
            normalization_notes: Vec::new(),
            findings: Vec::new(),
        }
    }

    fn draft() -> ContextDraft {
        ContextDraft {
            identity: ContextDraftIdentity {
                draft_id: "draft-1".to_string(),
                session_id: "session-1".to_string(),
                base_context_revision: 4,
                raw_message_count: 3,
                transcript_digest: 99,
                provider_name: "openai".to_string(),
                model: "gpt-test".to_string(),
                route: "oauth".to_string(),
                created_at: timestamp(),
                expires_at: timestamp() + Duration::minutes(30),
            },
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            required_operations: vec![StoredContextOperation::ReasoningSuppression(
                StoredReasoningSuppression {
                    selection: StoredReasoningSelection::KeepLatestAssistantTurns {
                        protected_recent_assistant_turns: 5,
                    },
                    targets: Vec::new(),
                    assistant_turns_affected: 1,
                    replay_block_kinds: vec![StoredContextBlockKind::OpenAiReasoning],
                    original_token_estimate: 600,
                    validation_evidence_version: 1,
                    validation: Vec::new(),
                },
            )],
            distillation_proposals: Vec::new(),
            ineligible_distillations: Vec::new(),
            preview: ContextDraftPreview {
                raw_stored_message_count: 3,
                current_context_revision: 4,
                proposed_context_revision: 5,
                economics: economics(10_000, 4_000),
                validation: validation(ContextProjectionValidationStatus::Supported),
                formatter_placeholder_count: 0,
                operation_previews: Vec::new(),
                notices: Vec::new(),
            },
            curator_usage: Vec::new(),
        }
    }

    fn comprehensive_draft(prefix: &str) -> ContextDraft {
        let generator = StoredContextArtifactGenerator {
            provider: "curator-provider".to_string(),
            model: "curator-model".to_string(),
            route: "curator-route".to_string(),
            prompt_version: "context-curator-v1".to_string(),
            effort: Some("high".to_string()),
            role: None,
            selection_source: None,
            transaction_instructions: None,
            task_instructions: None,
        };
        let target = StoredContentTarget {
            message_id: "message-2".to_string(),
            stored_index_hint: 2,
            block_ordinal_hint: 0,
            kind: StoredContextBlockKind::ToolResult,
            semantic_id: Some("tool-call-1".to_string()),
            expected_hash: 4242,
        };
        let distillation = StoredToolResultDistillation {
            target,
            tool_name: "bash".to_string(),
            tool_call_id: "tool-call-1".to_string(),
            replacement_content: format!(
                "{prefix} replacement first line\n{prefix} REPLACEMENT_TAIL"
            ),
            original_token_estimate: 10_000,
            replacement_token_estimate: 1_000,
            replacement_ratio_millionths: 100_000,
            preservation_rationale: format!(
                "{prefix} rationale first line\n{prefix} RATIONALE_TAIL"
            ),
            uncertainties: vec![format!("{prefix} uncertainty tail")],
            generator: generator.clone(),
            created_at: timestamp(),
        };
        let mut result = draft();
        result.required_operations.insert(
            0,
            StoredContextOperation::RangeSummary(StoredRangeSummary {
                source_range: jcode_session_types::StoredMessageRange {
                    start_message_id: "message-0".to_string(),
                    end_message_id: "message-1".to_string(),
                    start_index_hint: 0,
                    end_index_hint: 1,
                    source_digest: 999,
                    message_count: 2,
                },
                summary_text: format!("{prefix} summary first line\n{prefix} SUMMARY_TAIL"),
                file_change_digest: format!(
                    "{prefix} digest first line\n{prefix} FILE_DIGEST_TAIL"
                ),
                changed_files: vec![format!("src/{prefix}_changed.rs")],
                change_evidence_complete: false,
                boundary_expansions: vec![StoredRangeBoundaryExpansion {
                    message_id: "message-2".to_string(),
                    stored_index_hint: 2,
                    reason: StoredRangeBoundaryExpansionReason::ToolPair {
                        tool_use_id: "tool-call-1".to_string(),
                    },
                }],
                generator: Some(generator.clone()),
                source_token_estimate: 8_000,
                replacement_token_estimate: 800,
                warnings: vec![format!("{prefix} CURATOR_WARNING_TAIL")],
                created_at: timestamp(),
                legacy_coverage: None,
            }),
        );
        if let StoredContextOperation::ReasoningSuppression(suppression) =
            &mut result.required_operations[1]
        {
            suppression
                .replay_block_kinds
                .push(StoredContextBlockKind::ReasoningTrace);
            suppression
                .validation
                .push(StoredProviderValidationEvidence {
                    provider: "openai".to_string(),
                    model: "gpt-test".to_string(),
                    request_builder: "responses-v1".to_string(),
                    checked_at: timestamp(),
                    outcome: StoredProviderValidationOutcome::Passed,
                    warnings: vec![format!("{prefix} PROVIDER_WARNING_TAIL")],
                });
        }
        result.distillation_proposals = vec![ContextDistillationProposal {
            proposal_id: "proposal-1".to_string(),
            selected_by_default: true,
            operation: distillation,
        }];
        result.ineligible_distillations = vec![ContextIneligibleDistillation {
            request_id: "ineligible-1".to_string(),
            tool_name: "read".to_string(),
            tool_call_id: "tool-call-2".to_string(),
            reason: format!("{prefix} ineligible first line\n{prefix} INELIGIBLE_TAIL"),
            uncertainties: vec![format!("{prefix} ineligible uncertainty tail")],
        }];
        result.preview.notices = vec![format!("{prefix} NOTICE_TAIL")];
        result.preview.validation.normalization_notes =
            vec![format!("{prefix} NORMALIZATION_TAIL")];
        result.preview.validation.findings = vec![ContextProjectionValidationFinding {
            operation_id: Some("range-1".to_string()),
            operation_kind: Some(ContextProjectionOperationKind::RangeSummary),
            status: ContextProjectionValidationStatus::Supported,
            stage: ContextProjectionValidationStage::RequestBuilder,
            reason: format!("{prefix} VALIDATION_FINDING_TAIL"),
        }];
        result.preview.economics.assumptions = vec![format!("{prefix} ECONOMICS_TAIL")];
        result.curator_usage = vec![StoredContextCuratorUsage {
            provider: "curator-provider".to_string(),
            model: "curator-model".to_string(),
            route: "curator-route".to_string(),
            effort: None,
            role: None,
            artifact_id: None,
            prompt_version: None,
            input_tokens: 1_000,
            output_tokens: 200,
            cache_read_input_tokens: Some(400),
            cache_creation_input_tokens: Some(100),
            cost_usd: Some(0.42),
        }];
        result
    }

    fn transaction_detail_from_draft(
        prefix: &str,
        draft: &ContextDraft,
    ) -> ContextTransactionDetail {
        let mut operations = draft.required_operations.clone();
        operations.extend(draft.distillation_proposals.iter().map(|proposal| {
            StoredContextOperation::ToolResultDistillation(proposal.operation.clone())
        }));
        ContextTransactionDetail {
            session_id: "session-1".to_string(),
            context_revision: 5,
            transaction: StoredContextTransaction {
                id: "transaction-detail-1".to_string(),
                base_revision: 4,
                created_at: timestamp(),
                authorization: StoredContextAuthorization::Manual {
                    initiated_by: Some(format!("{prefix} AUTHORIZATION_TAIL")),
                },
                operations,
                status_events: vec![StoredContextStatusEvent {
                    revision: 5,
                    timestamp: timestamp(),
                    kind: StoredContextTransactionStatusKind::Applied,
                    reason: Some(format!("{prefix} STATUS_REASON_TAIL")),
                }],
                application: None,
                economics: Some(draft.preview.economics.clone()),
                curator_usage: draft.curator_usage.clone(),
                emergency_audit: None,
            },
        }
    }

    fn ready_editor() -> ContextEditor {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft())));
        editor
    }

    fn transaction_summary(
        id: &str,
        active: bool,
        status_revision: u64,
    ) -> ContextTransactionSummary {
        ContextTransactionSummary {
            id: id.to_string(),
            created_at: timestamp(),
            base_revision: status_revision.saturating_sub(1),
            active,
            latest_status: Some(if active {
                StoredContextTransactionStatusKind::Applied
            } else {
                StoredContextTransactionStatusKind::Reverted
            }),
            latest_status_revision: Some(status_revision),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operation_counts: ContextOperationCounts::default(),
            application: None,
            economics: Some(economics(10_000, 4_000)),
        }
    }

    fn history_page(
        context_revision: u64,
        total_transactions: usize,
        offset: usize,
        next_offset: Option<usize>,
        ids: &[&str],
    ) -> ContextTransactionHistoryPage {
        ContextTransactionHistoryPage {
            context_revision,
            total_transactions,
            offset,
            next_offset,
            transactions: ids
                .iter()
                .enumerate()
                .map(|(index, id)| transaction_summary(id, true, context_revision + index as u64))
                .collect(),
        }
    }

    fn history_state(page: ContextTransactionHistoryPage) -> ContextProtocolState {
        let mut state = ContextProtocolState::default();
        state.accepted_session_id = Some("session-1".to_string());
        state.accepted_context_revision = Some(page.context_revision);
        state.history = Some(page);
        state
    }

    fn closed_range(start: usize, end: usize) -> ContextClosedRangePreview {
        ContextClosedRangePreview {
            requested: ContextMessageRangeSelection {
                start_message_id: format!("message-{start}"),
                end_message_id: format!("message-{end}"),
            },
            source_range: jcode_session_types::StoredMessageRange {
                start_message_id: format!("message-{start}"),
                end_message_id: format!("message-{end}"),
                start_index_hint: start,
                end_index_hint: end,
                source_digest: 123,
                message_count: end.saturating_sub(start).saturating_add(1),
            },
            boundary_expansions: Vec::new(),
            source_tokens: 500,
        }
    }

    fn detail(
        start_char: usize,
        end_char: usize,
        total_chars: usize,
        text: &str,
    ) -> ContextMessageDetail {
        ContextMessageDetail {
            session_id: "session-1".to_string(),
            context_revision: 4,
            transcript_digest: 99,
            message_id: "message-0".to_string(),
            stored_index: 0,
            role: Role::User,
            display_role: None,
            timestamp: Some(timestamp()),
            block_ordinal: 0,
            block_kind: StoredContextBlockKind::Text,
            format: ContextMessageDetailFormat::Text,
            content: ContextTextChunk {
                start_char,
                end_char,
                total_chars,
                text: text.to_string(),
                next_start_char: (end_char < total_chars).then_some(end_char),
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

    fn detail_state(detail: ContextMessageDetail) -> ContextProtocolState {
        let mut state = ContextProtocolState::default();
        state.detail = Some(detail);
        state
    }

    fn rendered_text(lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_editor(editor: &mut ContextEditor, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| editor.render(frame))
            .expect("render context editor");
    }

    fn render_editor_text(editor: &mut ContextEditor, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| editor.render(frame))
            .expect("render context editor");
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut text = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn context_block(
        ordinal: usize,
        kind: StoredContextBlockKind,
        estimated_provider_tokens: usize,
    ) -> ContextEditorBlock {
        ContextEditorBlock {
            ordinal,
            provider_removable_reasoning: matches!(
                kind,
                StoredContextBlockKind::Reasoning
                    | StoredContextBlockKind::AnthropicThinking
                    | StoredContextBlockKind::OpenAiReasoning
            ),
            kind,
            semantic_id: None,
            estimated_provider_tokens,
            tool_name: None,
            tool_use_id: None,
            tool_result_is_error: false,
            has_image_payload: false,
            has_tool_thought_signature: false,
            active_operations: Vec::new(),
        }
    }

    fn interaction_snapshot() -> ContextEditorSnapshot {
        let mut messages = (0..7)
            .map(|index| message(index, &format!("interaction row {index}")))
            .collect::<Vec<_>>();
        messages[1].blocks = vec![
            context_block(0, StoredContextBlockKind::Text, 40),
            context_block(1, StoredContextBlockKind::OpenAiReasoning, 300),
        ];
        messages[2].blocks = vec![context_block(0, StoredContextBlockKind::ToolResult, 5_000)];
        messages[4].blocks = vec![
            context_block(0, StoredContextBlockKind::Text, 50),
            context_block(1, StoredContextBlockKind::ReasoningTrace, 500),
            context_block(2, StoredContextBlockKind::ToolResult, 6_000),
            context_block(3, StoredContextBlockKind::ToolResult, 1_000),
        ];
        messages[6].blocks = vec![context_block(0, StoredContextBlockKind::ToolResult, 7_000)];
        for message in &mut messages {
            message.raw_provider_tokens = message
                .blocks
                .iter()
                .map(|block| block.estimated_provider_tokens)
                .sum();
            message.projected_provider_tokens = message.raw_provider_tokens;
        }
        ContextEditorSnapshot {
            raw_message_count: messages.len(),
            message_page_end: messages.len(),
            next_message_page_start: None,
            messages,
            ..snapshot()
        }
    }

    fn range_preview(
        ranges: Vec<ContextClosedRangePreview>,
        shadowed_active_operations: Vec<String>,
    ) -> ContextRangeClosurePreview {
        ContextRangeClosurePreview {
            session_id: "session-1".to_string(),
            context_revision: 4,
            transcript_digest: 99,
            ranges,
            shadowed_active_operations,
        }
    }

    fn interaction_signature(editor: &ContextEditor) -> String {
        format!(
            "{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}|{:?}|{:?}|{}|{}",
            editor.phase,
            editor.modal,
            editor.focus,
            editor.cursor,
            editor.block_cursor,
            editor.preview_scroll,
            editor.operations_scroll,
            editor.selected_message_ids,
            editor.summary_anchor,
            editor.staged_ranges,
            editor.reasoning,
            editor.tool_targets.len(),
            editor.proposal_cursor,
            editor.selected_distillation_ids,
            editor.history_cursor,
            editor.transaction_detail.is_some(),
            editor.selection_preview_pending,
        )
    }

    fn rectangles_overlap(left: Rect, right: Rect) -> bool {
        left.x < right.right()
            && right.x < left.right()
            && left.y < right.bottom()
            && right.y < left.bottom()
    }

    fn assert_toolbar_rectangles_do_not_overlap(editor: &ContextEditor) {
        for (index, (left, left_action)) in editor.hit_regions.toolbar.iter().enumerate() {
            for (right, right_action) in editor.hit_regions.toolbar.iter().skip(index + 1) {
                assert!(
                    !rectangles_overlap(*left, *right),
                    "toolbar rectangles for {left_action:?} and {right_action:?} overlap: {left:?} vs {right:?}"
                );
            }
        }
    }

    fn assert_toolbar_matches_key(
        mut mouse_editor: ContextEditor,
        action: ContextEditorToolbarAction,
        key: KeyCode,
    ) {
        render_editor(&mut mouse_editor, 140, 42);
        assert_toolbar_rectangles_do_not_overlap(&mouse_editor);
        let rectangle = mouse_editor
            .hit_regions
            .toolbar
            .iter()
            .find_map(|(area, candidate)| (*candidate == action).then_some(*area))
            .unwrap_or_else(|| panic!("enabled toolbar action {action:?} has no hit rectangle"));
        let mut keyboard_editor = mouse_editor.clone();
        let (_, keyboard_action) = keyboard_editor.handle_key(key, KeyModifiers::NONE);
        let mouse_action = mouse_editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rectangle.x.saturating_add(rectangle.width / 2),
            row: rectangle.y.saturating_add(rectangle.height / 2),
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(mouse_action, keyboard_action, "toolbar action {action:?}");
        assert_eq!(
            interaction_signature(&mouse_editor),
            interaction_signature(&keyboard_editor),
            "toolbar action {action:?} diverged from key {key:?}"
        );
    }

    #[test]
    fn unicode_detail_chunks_merge_by_character_offset_and_enter_loads_the_next_chunk() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.sync_protocol(&detail_state(detail(0, 2, 4, "αβ")));

        let (_, next) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            next,
            Some(ContextEditorAction::LoadDetail {
                context_revision: 4,
                transcript_digest: 99,
                message_id: "message-0".to_string(),
                block_ordinal: 0,
                start_char: 2,
                max_chars: DEFAULT_DETAIL_CHARS,
            })
        );

        editor.sync_protocol(&detail_state(detail(2, 4, 4, "🙂z")));
        let rendered = rendered_text(editor.message_preview_lines(100));
        assert!(rendered.contains("αβ🙂z"));
        assert!(rendered.contains("4 / 4 characters"));
        let (_, complete) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(complete, None);
    }

    #[test]
    fn conflicting_detail_chunk_is_rejected_without_overwriting_loaded_content() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.sync_protocol(&detail_state(detail(0, 2, 4, "αβ")));
        editor.sync_protocol(&detail_state(detail(0, 2, 4, "γδ")));

        assert!(editor.stale);
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("conflicts with an already loaded chunk"))
        );
        let rendered = rendered_text(editor.message_preview_lines(100));
        assert!(rendered.contains("αβ"));
        assert!(!rendered.contains("γδ"));
    }

    #[test]
    fn metadata_only_detail_discloses_presence_without_exposing_opaque_values() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        let mut metadata = detail(0, 0, 0, "");
        metadata.block_kind = StoredContextBlockKind::Image;
        metadata.format = ContextMessageDetailFormat::MetadataOnly;
        metadata.image_media_type = Some("image/png".to_string());
        metadata.image_encoded_bytes = Some(24_000);
        metadata.opaque_signature_present = true;
        metadata.encrypted_state_present = true;
        editor.sync_protocol(&detail_state(metadata));

        let rendered = rendered_text(editor.message_preview_lines(100));
        assert!(rendered.contains("image/png"));
        assert!(rendered.contains("24000 bytes (body withheld)"));
        assert!(rendered.contains("Opaque provider signature: present (value withheld)"));
        assert!(rendered.contains("Encrypted provider state: present (value withheld)"));
        assert!(!rendered.contains("base64"));
    }

    #[test]
    fn staged_reasoning_statistics_report_real_replay_blocks_and_trace_zero_savings() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let mut snapshot = snapshot();
        snapshot.messages[1].blocks = vec![
            ContextEditorBlock {
                ordinal: 0,
                kind: StoredContextBlockKind::OpenAiReasoning,
                semantic_id: Some("reasoning-1".to_string()),
                estimated_provider_tokens: 120,
                tool_name: None,
                tool_use_id: None,
                tool_result_is_error: false,
                has_image_payload: false,
                has_tool_thought_signature: false,
                provider_removable_reasoning: true,
                active_operations: Vec::new(),
            },
            ContextEditorBlock {
                ordinal: 1,
                kind: StoredContextBlockKind::ReasoningTrace,
                semantic_id: None,
                estimated_provider_tokens: 80,
                tool_name: None,
                tool_use_id: None,
                tool_result_is_error: false,
                has_image_payload: false,
                has_tool_thought_signature: false,
                provider_removable_reasoning: false,
                active_operations: Vec::new(),
            },
        ];
        editor.apply_snapshot(snapshot);
        editor.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 0,
        });

        let rendered = rendered_text(editor.reasoning_statistics_lines());
        assert!(rendered.contains("1 assistant turn(s)"));
        assert!(rendered.contains("1 replay block(s)"));
        assert!(rendered.contains("120 removable tokens"));
        assert!(rendered.contains("OpenAiReasoning"));
        assert!(rendered.contains("ReasoningTrace: 1"));
        assert!(rendered.contains("zero provider-token savings"));
    }

    #[test]
    fn staged_reasoning_statistics_distinguish_new_active_covered_protected_and_non_replayable() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let mut snapshot = interaction_snapshot();
        snapshot.raw_message_count = 8;
        snapshot.message_page_end = 8;
        snapshot.messages.push(message(7, "protected assistant"));
        for index in [1usize, 3, 5, 7] {
            snapshot.messages[index].blocks = vec![context_block(
                0,
                StoredContextBlockKind::OpenAiReasoning,
                100,
            )];
        }
        snapshot.messages[1].blocks.push(context_block(
            1,
            StoredContextBlockKind::ReasoningTrace,
            80,
        ));
        snapshot.messages[3].blocks[0].active_operations =
            vec![crate::protocol::ContextOperationBadge {
                transaction_id: "active-reasoning".to_string(),
                operation_index: 0,
                kind: ContextOperationBadgeKind::ReasoningSuppression,
            }];
        snapshot.messages[5].summary_coverage = Some(crate::protocol::ContextSummaryCoverage {
            transaction_id: "active-summary".to_string(),
            operation_index: 0,
        });
        editor.apply_snapshot(snapshot);
        editor.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 1,
        });

        let rendered = rendered_text(editor.reasoning_statistics_lines());
        assert!(rendered.contains("1 newly eligible"));
        assert!(rendered.contains("1 already suppressed"));
        assert!(rendered.contains("1 covered by active summaries"));
        assert!(rendered.contains("1 protected"));
        assert!(rendered.contains("1 non-replayable"));
    }

    #[test]
    fn mouse_hit_testing_uses_rendered_scroll_start_after_search() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let messages = (0..40)
            .map(|index| {
                message(
                    index,
                    if index % 2 == 0 {
                        "search-match"
                    } else {
                        "other"
                    },
                )
            })
            .collect::<Vec<_>>();
        editor.apply_snapshot(ContextEditorSnapshot {
            raw_message_count: messages.len(),
            message_page_end: messages.len(),
            messages,
            ..snapshot()
        });
        editor.search_query = "search-match".to_string();
        editor.cursor = 15;
        render_editor(&mut editor, 100, 28);
        assert!(editor.rendered_message_start > 0);
        let visible = editor.visible_message_ids();
        let expected_index = editor.rendered_message_start.saturating_add(1);
        let expected_id = visible[expected_index].clone();

        let _ = editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: editor.hit_regions.list.x.saturating_add(2),
            row: editor.hit_regions.list.y.saturating_add(2),
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(editor.cursor, expected_index);
        assert_eq!(
            editor.current_message().map(|message| message.message_id),
            Some(expected_id)
        );
    }

    #[test]
    fn history_mouse_hit_testing_uses_rendered_start_and_preserves_id_after_page_append() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::History);
        editor.phase = ContextEditorPhase::History;
        editor.history = (0..40)
            .map(|index| transaction_summary(&format!("transaction-{index}"), true, 4))
            .collect();
        editor.history_total = 41;
        editor.history_context_revision = Some(4);
        editor.history_session_id = Some("session-1".to_string());
        editor.history_cursor = 32;

        render_editor(&mut editor, 90, 24);
        assert!(editor.rendered_history_start > 0);
        let expected_index = editor.rendered_history_start + 1;
        let expected_id = editor.history[expected_index].id.clone();
        let _ = editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: editor.hit_regions.list.x.saturating_add(1),
            row: editor.hit_regions.list.y.saturating_add(2),
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(editor.current_transaction().unwrap().id, expected_id);

        editor.sync_protocol(&history_state(ContextTransactionHistoryPage {
            context_revision: 4,
            total_transactions: 41,
            offset: 40,
            next_offset: None,
            transactions: vec![transaction_summary("transaction-40", true, 4)],
        }));
        assert_eq!(editor.history.len(), 41);
        assert_eq!(editor.current_transaction().unwrap().id, expected_id);
    }

    #[test]
    fn narrow_layout_exposes_clickable_keyboard_equivalent_toolbar_actions() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(interaction_snapshot());
        editor.cursor = 4;
        editor.block_cursor = 2;
        editor.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 5,
        });
        render_editor(&mut editor, 70, 30);

        assert!(editor.narrow_layout());
        let actions = editor
            .hit_regions
            .toolbar
            .iter()
            .map(|(_, action)| *action)
            .collect::<BTreeSet<_>>();
        for required in [
            ContextEditorToolbarAction::Range,
            ContextEditorToolbarAction::Reasoning,
            ContextEditorToolbarAction::ToggleOutput,
            ContextEditorToolbarAction::ScanOutputs,
            ContextEditorToolbarAction::Detail,
            ContextEditorToolbarAction::Prepare,
            ContextEditorToolbarAction::History,
        ] {
            assert!(
                actions.contains(&required),
                "missing toolbar action {required:?}"
            );
        }

        let detail_rect = editor
            .hit_regions
            .toolbar
            .iter()
            .find_map(|(area, action)| {
                (*action == ContextEditorToolbarAction::Detail).then_some(*area)
            })
            .expect("detail toolbar rectangle");
        let action = editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: detail_rect.x,
            row: detail_rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            action,
            Some(ContextEditorAction::LoadDetail { .. })
        ));
    }

    #[test]
    fn operations_pane_scrolls_independently_by_keyboard_and_mouse() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.focus = ContextEditorPane::Operations;
        render_editor(&mut editor, 100, 28);
        let baseline_max_scroll = editor.operations_max_scroll;
        editor.staged_ranges = (0..20).map(|_| closed_range(0, 1)).collect();
        render_editor(&mut editor, 100, 28);
        assert!(editor.operations_max_scroll >= 10);
        let _ = editor.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(editor.operations_scroll, 10);

        let _ = editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: editor.hit_regions.operations.x.saturating_add(1),
            row: editor.hit_regions.operations.y.saturating_add(1),
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            editor.operations_scroll,
            13.min(editor.operations_max_scroll)
        );
        assert_eq!(editor.preview_scroll, 0);

        editor.staged_ranges.clear();
        editor.status = None;
        editor.error = None;
        render_editor(&mut editor, 100, 28);
        assert_eq!(editor.operations_max_scroll, baseline_max_scroll);
        assert_eq!(editor.operations_scroll, baseline_max_scroll);
    }

    #[test]
    fn curator_unavailability_blocks_only_curator_dependent_drafts() {
        let mut unavailable = snapshot();
        unavailable.curator_route = None;
        unavailable.curator_unavailable_reason =
            Some("no independent route is configured".to_string());
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(unavailable.clone());
        editor.staged_ranges.push(closed_range(0, 1));
        let (_, action) = editor.prepare_action();
        assert_eq!(action, None);
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no independent route is configured"))
        );

        let mut reasoning_only = ContextEditor::new(ContextEditorOpenMode::Edit);
        reasoning_only.apply_snapshot(unavailable);
        reasoning_only.reasoning =
            Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            });
        let (_, action) = reasoning_only.prepare_action();
        assert!(matches!(action, Some(ContextEditorAction::PrepareDraft(_))));
    }

    #[test]
    fn same_session_lifecycle_refresh_preserves_stable_intent_and_invalidates_generated_artifacts()
    {
        let mut editor = ready_editor();
        editor.selected_message_ids.insert("message-0".to_string());
        editor.tool_targets.insert(("message-0".to_string(), 0));
        editor.reasoning = Some(ContextReasoningSelectionRequest::MessageRanges {
            ranges: vec![ContextMessageRangeSelection {
                start_message_id: "message-0".to_string(),
                end_message_id: "message-1".to_string(),
            }],
        });
        editor.staged_ranges.push(closed_range(0, 1));
        let mut refreshed = snapshot();
        refreshed.transcript_digest = 100;
        refreshed.model = "gpt-test-updated".to_string();
        editor.apply_snapshot(refreshed);

        assert!(editor.stale);
        assert!(editor.selected_message_ids.contains("message-0"));
        assert!(editor.tool_targets.contains(&("message-0".to_string(), 0)));
        assert!(editor.reasoning.is_some());
        assert!(editor.staged_ranges.is_empty());
        assert!(editor.draft.is_none());
        assert!(editor.selection_preview.is_none());
        assert_eq!(editor.phase, ContextEditorPhase::Editing);
    }

    #[test]
    fn snapshot_pagination_preserves_unloaded_stable_ids_until_complete_reconciliation() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor
            .selected_message_ids
            .extend(["message-2".to_string(), "missing-message".to_string()]);

        editor.apply_snapshot(snapshot_page(0, 2));
        assert_eq!(editor.rows.len(), 2);
        assert_eq!(editor.pending_auto_page, Some(2));
        assert!(editor.selected_message_ids.contains("message-2"));
        assert!(editor.selected_message_ids.contains("missing-message"));

        editor.apply_snapshot(snapshot_page(2, 3));
        assert_eq!(editor.rows.len(), 3);
        assert_eq!(editor.pending_auto_page, None);
        assert!(editor.selected_message_ids.contains("message-2"));
        assert!(!editor.selected_message_ids.contains("missing-message"));
    }

    #[test]
    fn snapshot_pagination_rejects_nonzero_first_gap_duplicate_and_conflicting_pages() {
        let mut nonzero_first = ContextEditor::new(ContextEditorOpenMode::Edit);
        nonzero_first.apply_snapshot(snapshot_page(2, 3));
        assert!(nonzero_first.stale);
        assert!(nonzero_first.rows.is_empty());
        assert!(
            nonzero_first
                .error
                .as_deref()
                .is_some_and(|error| error.contains("began after page zero"))
        );

        let mut gap = ContextEditor::new(ContextEditorOpenMode::Edit);
        gap.apply_snapshot(snapshot_page(0, 1));
        let mut gap_page = snapshot_page(2, 3);
        gap_page.messages[0] = message(2, "gamma");
        gap.apply_snapshot(gap_page);
        assert!(gap.stale);
        assert_eq!(gap.rows.len(), 1);
        assert!(
            gap.error
                .as_deref()
                .is_some_and(|error| error.contains("not contiguous"))
        );

        let mut duplicate = ContextEditor::new(ContextEditorOpenMode::Edit);
        duplicate.apply_snapshot(snapshot_page(0, 2));
        let mut duplicate_page = snapshot_page(2, 3);
        duplicate_page.messages[0].message_id = "message-0".to_string();
        duplicate.apply_snapshot(duplicate_page);
        assert!(duplicate.stale);
        assert_eq!(duplicate.rows.len(), 2);
        assert!(
            duplicate
                .error
                .as_deref()
                .is_some_and(|error| error.contains("moved stable message ID"))
        );

        let mut conflicting = ContextEditor::new(ContextEditorOpenMode::Edit);
        conflicting.apply_snapshot(snapshot());
        let mut changed_page_zero = snapshot_page(0, 2);
        changed_page_zero.messages[0].preview = "changed without digest".to_string();
        conflicting.apply_snapshot(changed_page_zero);
        assert!(conflicting.stale);
        assert_eq!(
            conflicting
                .rows
                .get(&0)
                .map(|message| message.preview.as_str()),
            Some("alpha")
        );
        assert!(conflicting.error.as_deref().is_some_and(|error| {
            error.contains("without changing authoritative transcript identity")
        }));
    }

    #[test]
    fn session_switch_clears_stable_selections_instead_of_retargeting_by_position() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.selected_message_ids.insert("message-0".to_string());
        editor.tool_targets.insert(("message-0".to_string(), 0));
        let mut switched = snapshot();
        switched.session_id = "session-2".to_string();
        editor.apply_snapshot(switched);

        assert!(editor.selected_message_ids.is_empty());
        assert!(editor.tool_targets.is_empty());
        assert!(editor.stale);
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("session changed"))
        );
    }

    #[test]
    fn processing_only_snapshot_refresh_keeps_review_visible_and_disables_apply() {
        let mut editor = ready_editor();
        let mut processing = snapshot();
        processing.processing = true;
        editor.apply_snapshot(processing);

        assert_eq!(editor.phase, ContextEditorPhase::ReviewDraft);
        assert!(editor.draft.is_some());
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("session is processing")
        );
    }

    #[test]
    fn curator_workspace_controls_one_run_exact_review_and_saves_only_route_defaults() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.phase = ContextEditorPhase::Editing;
        editor.staged_ranges = vec![closed_range(0, 1)];

        assert_eq!(
            editor.handle_key(KeyCode::Char('C'), KeyModifiers::NONE).1,
            None
        );
        assert!(editor.curator_workspace.active);
        assert_eq!(
            editor.curator_workspace.section,
            CuratorWorkspaceSection::Overview
        );
        assert_eq!(editor.modal, None);
        let overview = render_editor_text(&mut editor, 120, 40);
        assert!(overview.contains("Prepare context review"));
        assert!(overview.contains("SAVED DEFAULT"));
        assert!(overview.contains("no model invoked yet"));

        editor.handle_key(KeyCode::Down, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            editor.curator_workspace.section,
            CuratorWorkspaceSection::Route
        );
        assert_eq!(editor.curator_workspace.pane, CuratorWorkspacePane::Detail);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            editor.curator_selection,
            Some(ContextCuratorSelection {
                provider: Some("anthropic".to_string()),
                route: Some("anthropic-api".to_string()),
                model: Some("claude-fable-5".to_string()),
                effort: None,
            })
        );
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            editor
                .curator_selection
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("high")
        );
        let route = render_editor_text(&mut editor, 120, 40);
        assert!(route.contains("TEMPORARY FOR THIS RUN"));
        assert!(route.contains("Nothing was saved") || route.contains("Save as default"));

        editor.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Down, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            editor.curator_workspace.section,
            CuratorWorkspaceSection::Instructions
        );
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        for character in "TRANSACTION_INSTRUCTION".chars() {
            editor.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        for character in "RANGE_INSTRUCTION".chars() {
            editor.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        let instructions = render_editor_text(&mut editor, 120, 40);
        assert!(instructions.contains("instructions are temporary"));
        assert!(instructions.contains("Needs re-validation"));

        let (_, action) = editor.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let ContextEditorAction::PreviewCuratorPlan { request, .. } =
            action.expect("exact no-model validation action")
        else {
            panic!("expected curator plan preview")
        };
        assert_eq!(request.curator.selection, editor.curator_selection.clone());
        assert_eq!(
            request.curator.transaction_instructions,
            "TRANSACTION_INSTRUCTION"
        );
        assert_eq!(request.curator.range_instructions.len(), 1);
        assert_eq!(
            request.curator.range_instructions[0].instructions,
            "RANGE_INSTRUCTION"
        );
        assert!(editor.curator_plan_pending);
        assert_eq!(
            editor.curator_workspace.section,
            CuratorWorkspaceSection::ExactCalls
        );
        let pending = render_editor_text(&mut editor, 120, 40);
        assert!(pending.contains("Checking exact calls"));
        assert!(pending.contains("No model is invoked"));

        let plan = ContextCuratorPlanPreview {
            session_id: "session-1".to_string(),
            context_revision: 4,
            transcript_digest: 99,
            route: crate::protocol::ContextCuratorRoutePreview {
                provider_name: "anthropic".to_string(),
                provider_display_name: "Anthropic".to_string(),
                model: "claude-fable-5".to_string(),
                route: "anthropic-api".to_string(),
                effort: Some("high".to_string()),
            },
            using_configured_default: false,
            tasks: vec![crate::protocol::ContextCuratorTaskPreview {
                task_id: "range-1".to_string(),
                role: jcode_session_types::StoredContextCuratorRole::RangeSummarizer,
                target_label: "range 0..0 (1 messages)".to_string(),
                effective_system_prompt: "EXACT_EFFECTIVE_PROMPT".to_string(),
                response_contract: "EXACT_RESPONSE_CONTRACT".to_string(),
                estimated_input_tokens: 1_000,
                safe_input_budget: 100_000,
                request_bytes: 5_000,
                request_byte_limit: 32 * 1024 * 1024,
                image_count: 0,
                source_scope: vec![crate::protocol::ContextCuratorSourceScope {
                    purpose: crate::protocol::ContextCuratorSourcePurpose::PrimaryRange,
                    message_id: Some("message-0".to_string()),
                    stored_index: Some(0),
                    block_ordinals: Vec::new(),
                    includes_all_blocks: true,
                }],
            }],
            fingerprint: "b".repeat(64),
        };
        editor.curator_plan_request = Some(request.clone());
        editor.curator_plan = Some(plan);
        editor.curator_plan_pending = false;
        editor.curator_workspace_plan_accepted(1);

        let overview = render_editor_text(&mut editor, 120, 40);
        assert!(overview.contains("range 0..0 (1 message)"));
        assert!(!overview.contains("1 messages"));

        editor.curator_workspace.plan_detail = CuratorPlanDetail::Prompt;
        let prompt = render_editor_text(&mut editor, 120, 40);
        assert!(prompt.contains("EXACT_EFFECTIVE_PROMPT"));
        editor.curator_workspace.plan_detail = CuratorPlanDetail::Contract;
        let contract = render_editor_text(&mut editor, 120, 40);
        assert!(contract.contains("EXACT_RESPONSE_CONTRACT"));
        editor.curator_workspace.plan_detail = CuratorPlanDetail::SourceScope;
        let source = render_editor_text(&mut editor, 120, 40);
        assert!(source.contains("primary range"));
        assert!(source.contains("message-0"));
        assert!(source.contains("all blocks"));
        editor.curator_workspace.plan_detail = CuratorPlanDetail::Integrity;
        let integrity = render_editor_text(&mut editor, 120, 40);
        assert!(integrity.contains("SHA-256 fingerprint"));
        assert!(integrity.contains("bbbbbbbbbbbbbbbb"));

        editor.curator_workspace.section = CuratorWorkspaceSection::Route;
        editor.curator_workspace.pane = CuratorWorkspacePane::Detail;
        let (_, save_overlay) = editor.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(save_overlay, None);
        let save_confirmation = render_editor_text(&mut editor, 120, 40);
        assert!(save_confirmation.contains("Only provider, route, model, and effort"));
        assert!(save_confirmation.contains("instructions remain temporary"));
        let (_, save) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            save,
            Some(ContextEditorAction::SaveCuratorDefault(
                editor.curator_selection.clone().expect("per-run selection")
            ))
        );

        let (_, prepare) = editor.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let mut reviewed_request = request;
        reviewed_request.curator.expected_plan_fingerprint = Some("b".repeat(64));
        assert_eq!(
            prepare,
            Some(ContextEditorAction::PrepareDraft(reviewed_request))
        );

        editor.phase = ContextEditorPhase::Editing;
        editor.curator_workspace.section = CuratorWorkspaceSection::Route;
        editor.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(editor.curator_selection, None);
        assert!(editor.curator_plan.is_none());
        assert_eq!(
            editor.curator_transaction_instructions,
            "TRANSACTION_INSTRUCTION"
        );
    }

    #[test]
    fn curator_workspace_unicode_instruction_editing_preserves_scopes_and_exact_bounds() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.phase = ContextEditorPhase::Editing;
        editor.staged_ranges = vec![closed_range(0, 1), closed_range(1, 2)];
        editor.open_curator_workspace(CuratorWorkspaceSection::Instructions);
        editor.curator_workspace.pane = CuratorWorkspacePane::Detail;
        editor.curator_workspace.instruction_scope = CuratorInstructionScope::Range;
        editor.curator_workspace.instruction_editing = true;
        editor.curator_range_instructions.clear();
        editor.prepare_instruction_editor_cursor();

        for character in "first α🙂".chars() {
            editor.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Char(']'), KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        for character in "second 漢🚀".chars() {
            editor.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Char('['), KeyModifiers::NONE);
        assert_eq!(
            editor
                .curator_range_instructions
                .get(&canonical_editor_range_key(
                    &editor.staged_ranges[0].requested
                ))
                .map(String::as_str),
            Some("first α🙂")
        );
        assert_eq!(
            editor
                .curator_range_instructions
                .get(&canonical_editor_range_key(
                    &editor.staged_ranges[1].requested
                ))
                .map(String::as_str),
            Some("second 漢🚀")
        );

        editor.curator_workspace.instruction_scope = CuratorInstructionScope::Transaction;
        editor.curator_transaction_instructions.clear();
        editor.curator_workspace.instruction_editing = true;
        editor.prepare_instruction_editor_cursor();
        for character in "α🙂漢".chars() {
            editor.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Home, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Delete, KeyModifiers::NONE);
        editor.handle_key(KeyCode::Char('β'), KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.curator_transaction_instructions, "β\n漢");
        assert_eq!(editor.curator_transaction_instructions.chars().count(), 3);

        editor.curator_transaction_instructions =
            "x".repeat(crate::protocol::CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS);
        editor.prepare_instruction_editor_cursor();
        editor.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(
            editor.curator_transaction_instructions.chars().count(),
            crate::protocol::CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS
        );

        editor.curator_transaction_instructions.clear();
        editor.curator_range_instructions.clear();
        for index in 0..32 {
            editor.curator_range_instructions.insert(
                (format!("other-start-{index}"), format!("other-end-{index}")),
                "z".repeat(crate::protocol::CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS),
            );
        }
        assert_eq!(
            editor.total_curator_instruction_chars(),
            crate::protocol::CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS
        );
        editor.curator_workspace.instruction_scope = CuratorInstructionScope::Range;
        editor.curator_workspace.range_cursor = 0;
        editor.prepare_instruction_editor_cursor();
        editor.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(
            editor
                .curator_range_instructions
                .get(&canonical_editor_range_key(
                    &editor.staged_ranges[0].requested
                ))
                .is_some_and(String::is_empty)
        );
        assert!(editor.curator_workspace.plan_dirty_reason.is_some());
    }

    #[test]
    fn curator_workspace_preserves_every_exact_and_generated_multiline_tail() {
        let mut plan_editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        plan_editor
            .apply_debug_fixture("curator-workspace-multi-task-plan")
            .expect("apply exact-plan fixture");
        let plan = plan_editor.curator_plan.as_ref().expect("debug plan");
        let mut exact = String::new();
        for task in &plan.tasks {
            exact.push_str(&rendered_text(curator_task_detail_lines(
                task,
                plan,
                CuratorPlanDetail::Prompt,
            )));
            exact.push_str(&rendered_text(curator_task_detail_lines(
                task,
                plan,
                CuratorPlanDetail::Contract,
            )));
        }
        for expected in [
            "PROMPT TAIL 0",
            "PROMPT TAIL 1",
            "TOOL PROMPT TAIL",
            "CONTRACT TAIL",
            "TOOL CONTRACT TAIL",
        ] {
            assert!(exact.contains(expected), "missing exact detail {expected}");
        }

        let mut draft = comprehensive_draft("WORKSPACE_FIELD");
        let mut generator = draft.distillation_proposals[0].operation.generator.clone();
        generator.selection_source =
            Some(jcode_session_types::StoredContextCuratorSelectionSource::PerRunOverride);
        generator.transaction_instructions =
            Some("WORKSPACE_FIELD TRANSACTION_INSTRUCTION_TAIL".to_string());
        generator.task_instructions = Some("WORKSPACE_FIELD TASK_INSTRUCTION_TAIL".to_string());
        for operation in &mut draft.required_operations {
            if let StoredContextOperation::RangeSummary(summary) = operation {
                summary.generator = Some(generator.clone());
            }
        }
        draft.distillation_proposals[0].operation.generator = generator;

        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft.clone())));
        let item_count = 1
            + draft.required_operations.len()
            + draft.distillation_proposals.len()
            + draft.ineligible_distillations.len()
            + draft.curator_usage.len();
        let mut review = String::new();
        for cursor in 0..item_count {
            editor.curator_workspace.review_cursor = cursor;
            review.push_str(&rendered_text(editor.curator_review_detail_lines()));
        }
        for expected in [
            "WORKSPACE_FIELD SUMMARY_TAIL",
            "WORKSPACE_FIELD FILE_DIGEST_TAIL",
            "WORKSPACE_FIELD CURATOR_WARNING_TAIL",
            "WORKSPACE_FIELD PROVIDER_WARNING_TAIL",
            "WORKSPACE_FIELD REPLACEMENT_TAIL",
            "WORKSPACE_FIELD RATIONALE_TAIL",
            "WORKSPACE_FIELD INELIGIBLE_TAIL",
            "WORKSPACE_FIELD NOTICE_TAIL",
            "WORKSPACE_FIELD NORMALIZATION_TAIL",
            "WORKSPACE_FIELD VALIDATION_FINDING_TAIL",
            "WORKSPACE_FIELD ECONOMICS_TAIL",
            "WORKSPACE_FIELD TRANSACTION_INSTRUCTION_TAIL",
            "WORKSPACE_FIELD TASK_INSTRUCTION_TAIL",
            "PerRunOverride",
        ] {
            assert!(
                review.contains(expected),
                "missing review detail {expected}"
            );
        }
    }

    #[test]
    fn curator_workspace_narrow_breadcrumbs_unwind_one_level_and_keep_actions_reachable() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor
            .apply_debug_fixture("curator-workspace-narrow")
            .expect("apply narrow workspace fixture");
        let rendered = render_editor_text(&mut editor, 72, 24);
        assert!(rendered.contains("Prepare context review > Exact calls"));
        assert!(rendered.contains("[Generate 3 calls]"));

        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 72, 24);
        assert!(rendered.contains("Call 1/3 > Overview"));
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 72, 24);
        assert!(rendered.contains("Call 1/3 > Exact prompt"));

        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 72, 24);
        assert!(rendered.contains("Prepare context review > Exact calls"));
        assert!(!rendered.contains("Call 1/3 >"));
        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 72, 24);
        assert!(rendered.contains("Prepare context review > Run sheet"));
        assert!(editor.curator_workspace.active);
    }

    #[test]
    fn curator_workspace_terminal_outcome_retains_progress_and_retries_all_calls() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor
            .apply_debug_fixture("curator-workspace-multi-task-plan")
            .expect("apply exact-plan fixture");
        let identity = draft().identity;
        editor.apply_draft_state(ContextClientDraftState::Progress {
            draft_id: identity.draft_id.clone(),
            progress: ContextDraftProgress {
                phase: ContextDraftPhase::PreparingArtifacts,
                completed_items: 2,
                total_items: 3,
            },
        });
        editor.apply_draft_state(ContextClientDraftState::Canceled(identity));
        assert_eq!(
            editor.draft_progress,
            Some(ContextDraftProgress {
                phase: ContextDraftPhase::PreparingArtifacts,
                completed_items: 2,
                total_items: 3,
            })
        );
        let rendered = render_editor_text(&mut editor, 120, 40);
        assert!(rendered.contains("Completed before stop: 2/3 isolated calls"));
        assert!(rendered.contains("No curator artifacts were retained"));
        assert!(rendered.contains("[Retry all calls]"));
        let (_, action) = editor.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let ContextEditorAction::PrepareDraft(request) = action.expect("retry all calls") else {
            panic!("expected complete preparation retry")
        };
        assert_eq!(
            request.curator.expected_plan_fingerprint.as_deref(),
            Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
        );
    }

    #[test]
    fn curator_workspace_default_terminal_and_review_states_remain_truthful() {
        let mut saved = ContextEditor::new(ContextEditorOpenMode::Edit);
        saved
            .apply_debug_fixture("curator-workspace-save-success")
            .expect("apply saved-default fixture");
        let rendered = render_editor_text(&mut saved, 120, 40);
        assert!(rendered.contains("SAVED DEFAULT Synthetic OpenRouter"));
        assert!(rendered.contains("(Default saved)"));
        let (_, action) = saved.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(action.is_none());
        assert!(!render_editor_text(&mut saved, 120, 40).contains("Save curator default"));

        let mut stopped = ContextEditor::new(ContextEditorOpenMode::Edit);
        stopped
            .apply_debug_fixture("curator-workspace-canceled")
            .expect("apply canceled fixture");
        let (_, action) = stopped.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(action.is_none());
        assert_eq!(stopped.curator_workspace.generation_outcome, None);
        assert_eq!(
            stopped.curator_workspace.section,
            CuratorWorkspaceSection::Overview
        );
        assert_eq!(stopped.phase, ContextEditorPhase::Editing);

        let mut review = ContextEditor::new(ContextEditorOpenMode::Edit);
        review
            .apply_debug_fixture("long-final-review")
            .expect("apply generated-review fixture");
        let rendered = render_editor_text(&mut review, 120, 40);
        assert!(rendered.contains(
            "1 required · 9 eligible · 1 ineligible · 1 curator call · generation complete"
        ));
        assert!(rendered.contains("before one atomic apply"));

        let narrow = render_editor_text(&mut review, 72, 24);
        assert!(narrow.contains("Prepare context review > Atomic review"));
        review.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(review.curator_workspace.narrow_detail_open);
        let detail = render_editor_text(&mut review, 72, 24);
        assert!(detail.contains("Prepare context review > Atomic review > Detail"));
        assert!(detail.contains("Atomic transaction overview"));
        assert!(!detail.contains("Apply atomic context transaction"));
        review.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!review.curator_workspace.narrow_detail_open);
        review.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            render_editor_text(&mut review, 72, 24).contains("Apply atomic context transaction")
        );
    }

    #[test]
    fn curator_workspace_mouse_targets_match_keyboard_state_transitions() {
        let click = |editor: &mut ContextEditor, rect: Rect| {
            editor.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
        };

        let mut routes = ContextEditor::new(ContextEditorOpenMode::Edit);
        routes
            .apply_debug_fixture("curator-workspace-route")
            .expect("apply route fixture");
        render_editor(&mut routes, 120, 40);
        let route_rect = routes
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| {
                matches!(target, CuratorHitTarget::Route(1)).then_some(*rect)
            })
            .expect("third route hit target");
        assert_eq!(click(&mut routes, route_rect), None);
        assert_eq!(
            routes
                .curator_selection
                .as_ref()
                .and_then(|selection| selection.model.as_deref()),
            Some("synthetic-fable-5")
        );

        let mut plan = ContextEditor::new(ContextEditorOpenMode::Edit);
        plan.apply_debug_fixture("curator-workspace-multi-task-plan")
            .expect("apply plan fixture");
        render_editor(&mut plan, 120, 40);
        let task_rect = plan
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| matches!(target, CuratorHitTarget::Task(1)).then_some(*rect))
            .expect("second task hit target");
        click(&mut plan, task_rect);
        assert_eq!(plan.curator_workspace.task_cursor, 1);
        let prompt_rect = plan
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| {
                matches!(
                    target,
                    CuratorHitTarget::PlanDetail(CuratorPlanDetail::Prompt)
                )
                .then_some(*rect)
            })
            .expect("prompt detail hit target");
        click(&mut plan, prompt_rect);
        assert_eq!(
            plan.curator_workspace.plan_detail,
            CuratorPlanDetail::Prompt
        );

        let mut instructions = ContextEditor::new(ContextEditorOpenMode::Edit);
        instructions
            .apply_debug_fixture("curator-workspace-instructions")
            .expect("apply instructions fixture");
        render_editor(&mut instructions, 120, 40);
        let range_rect = instructions
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| {
                matches!(target, CuratorHitTarget::Range(1)).then_some(*rect)
            })
            .expect("second stable range hit target");
        click(&mut instructions, range_rect);
        assert_eq!(instructions.curator_workspace.range_cursor, 1);

        let mut review = ContextEditor::new(ContextEditorOpenMode::Edit);
        review
            .apply_debug_fixture("long-final-review")
            .expect("apply review fixture");
        render_editor(&mut review, 120, 40);
        let proposal_index = 1 + review
            .draft
            .as_ref()
            .expect("review draft")
            .required_operations
            .len();
        let proposal_rect = review
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| {
                matches!(target, CuratorHitTarget::ReviewItem(index) if *index == proposal_index)
                    .then_some(*rect)
            })
            .expect("eligible proposal hit target");
        click(&mut review, proposal_rect);
        assert_eq!(review.curator_workspace.review_cursor, proposal_index);
        render_editor(&mut review, 120, 40);
        let toggle_rect = review
            .curator_workspace
            .hit_regions
            .targets
            .iter()
            .find_map(|(rect, target)| {
                matches!(
                    target,
                    CuratorHitTarget::Action(
                        curator_workspace::CuratorWorkspaceAction::ToggleProposal
                    )
                )
                .then_some(*rect)
            })
            .expect("toggle proposal action hit target");
        let action = click(&mut review, toggle_rect);
        assert!(matches!(
            action,
            Some(ContextEditorAction::PreviewDraftSelection { .. })
        ));
    }

    #[test]
    fn curator_failure_and_cancellation_disclose_full_atomic_retry_behavior() {
        let identity = draft().identity;
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.apply_draft_state(ContextClientDraftState::Failed {
            identity: identity.clone(),
            error: ContextServiceError::Curator("atomic task tool-2 failed".to_string()),
            stale: false,
        });
        let failed = editor.status.as_deref().expect("curator failure status");
        assert!(failed.contains("No curator artifacts were retained"));
        assert!(failed.contains("rerun every isolated curator call"));

        editor.tool_targets.insert(("message-2".to_string(), 0));
        editor.apply_draft_state(ContextClientDraftState::Canceled(identity));
        let canceled = editor.status.as_deref().expect("curator cancel status");
        assert!(canceled.contains("no curator artifacts were retained"));
        assert!(canceled.contains("rerun every isolated call"));
    }

    #[test]
    fn curator_plan_transport_failure_stays_in_workspace_and_releases_pending_state() {
        let mut protocol = ContextProtocolState::default();
        protocol.begin_snapshot_request(1);
        assert!(protocol.accept_snapshot(1, snapshot()));

        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.sync_protocol(&protocol);
        editor.staged_ranges = vec![closed_range(0, 1)];
        editor.open_curator_workspace(CuratorWorkspaceSection::ExactCalls);
        editor.curator_plan_pending = true;
        editor.curator_plan_request = Some(editor.current_draft_request());
        protocol.begin_curator_plan_request(2, "session-1".to_string(), 4, 99);
        assert!(protocol.accept_rejection(
            2,
            ContextRequestKind::CuratorPlanPreview,
            None,
            None,
            ContextServiceError::Runtime("transport unavailable".to_string()),
        ));

        editor.sync_protocol(&protocol);
        assert!(!editor.curator_plan_pending);
        assert_eq!(editor.phase, ContextEditorPhase::Editing);
        assert!(editor.curator_workspace.active);
        assert_eq!(
            editor.curator_workspace.section,
            CuratorWorkspaceSection::ExactCalls
        );
        assert_eq!(editor.modal, None);
        let rendered = render_editor_text(&mut editor, 120, 40);
        assert!(rendered.contains("Error: context service runtime error: transport unavailable"));
        assert!(rendered.contains("NEEDS RE-VALIDATION"));
        assert!(rendered.contains("Staged operations and temporary"));

        let cursor = editor.cursor;
        let (_, action) = editor.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(action.is_none());
        assert_eq!(
            editor.cursor, cursor,
            "workspace input must not leak to authoritative history"
        );

        editor.curator_plan_pending = true;
        assert!(editor.preview_curator_plan_action().is_none());
        assert!(
            editor
                .status
                .as_deref()
                .is_some_and(|status| status.contains("already in progress"))
        );
    }

    #[test]
    fn accepted_range_preview_is_consumed_once_and_identical_later_request_is_new() {
        let selection = ContextMessageRangeSelection {
            start_message_id: "message-0".to_string(),
            end_message_id: "message-1".to_string(),
        };
        let preview = range_preview(vec![closed_range(0, 1)], Vec::new());
        let mut protocol = ContextProtocolState::default();
        protocol.begin_snapshot_request(1);
        assert!(protocol.accept_snapshot(1, snapshot()));
        protocol.begin_range_preview_request(
            2,
            "session-1".to_string(),
            4,
            99,
            vec![selection.clone()],
        );
        assert!(protocol.accept_range_preview(2, preview.clone()));

        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.sync_protocol(&protocol);
        assert_eq!(editor.phase, ContextEditorPhase::ConfirmRangeClosure);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.phase, ContextEditorPhase::Editing);
        assert!(editor.pending_range_preview.is_none());

        editor.sync_protocol(&protocol);
        assert_eq!(
            editor.phase,
            ContextEditorPhase::Editing,
            "the accepted cached response must not reactivate after confirmation"
        );

        protocol.begin_range_preview_request(3, "session-1".to_string(), 4, 99, vec![selection]);
        assert!(protocol.accept_range_preview(3, preview));
        editor.sync_protocol(&protocol);
        assert_eq!(editor.phase, ContextEditorPhase::ConfirmRangeClosure);
        editor.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        editor.sync_protocol(&protocol);
        assert_eq!(
            editor.phase,
            ContextEditorPhase::Editing,
            "the accepted cached response must not reactivate after rejection"
        );
    }

    #[test]
    fn curator_route_identity_is_visible_without_hardcoded_model_assumptions() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        let rendered = rendered_text(editor.operations_lines());
        assert!(rendered.contains("OpenAI / openai / oauth"));
        assert!(rendered.contains("model gpt-curator-test"));
        assert!(rendered.contains("effort high"));
    }

    #[test]
    fn emergency_policy_is_visible_and_explicitly_controllable_without_exposing_provenance() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let mut blocked = snapshot();
        blocked.emergency_policy = jcode_session_types::StoredContextEmergencyPolicy::Block;
        editor.apply_snapshot(blocked);
        let rendered = rendered_text(editor.operations_lines());
        assert!(rendered.contains("Emergency context policy: block"));
        assert!(rendered.contains("no unattended context surgery authorized"));
        assert!(
            editor
                .toolbar_items()
                .iter()
                .any(|(_, action)| matches!(action, ContextEditorToolbarAction::Policy))
        );
        let (_, action) = editor.handle_key(KeyCode::Char('P'), KeyModifiers::NONE);
        assert!(action.is_none());
        assert_eq!(editor.modal, Some(ContextEditorModal::EmergencyPolicyMenu));
        let (_, action) = editor.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::SetEmergencyPolicy(
                StoredContextEmergencyPolicy::Block
            ))
        );

        editor.handle_key(KeyCode::Char('P'), KeyModifiers::NONE);
        editor.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        editor.emergency_policy_input = "7 15 1 0 1".to_string();
        let (_, action) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::SetEmergencyPolicy(
                StoredContextEmergencyPolicy::Authorized {
                    protected_recent_assistant_turns: 7,
                    target_headroom_percent: 15,
                    allow_reasoning_suppression: true,
                    allow_tool_distillation: false,
                    allow_oldest_range_summary: true,
                    authorization_source: "context_editor_session:session-1".to_string(),
                }
            ))
        );

        let mut authorized = snapshot();
        authorized.emergency_policy =
            jcode_session_types::StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns: 7,
                target_headroom_percent: 15,
                allow_reasoning_suppression: true,
                allow_tool_distillation: false,
                allow_oldest_range_summary: true,
                authorization_source: "sensitive source remains hidden".to_string(),
            };
        editor.apply_snapshot(authorized);
        let rendered = rendered_text(editor.operations_lines());
        assert!(rendered.contains("Emergency context policy: authorized"));
        assert!(rendered.contains("protect latest 7 assistant turn(s)"));
        assert!(rendered.contains("target 15% headroom"));
        assert!(rendered.contains("reasoning allowed"));
        assert!(rendered.contains("tool distillation blocked"));
        assert!(rendered.contains("oldest-range summary allowed"));
        assert!(!rendered.contains("sensitive source remains hidden"));
    }

    #[test]
    fn emergency_policy_control_is_processing_gated_and_validates_every_parameter() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let mut processing = snapshot();
        processing.processing = true;
        editor.apply_snapshot(processing);
        assert!(!editor.toolbar_action_enabled(ContextEditorToolbarAction::Policy));

        assert!(parse_emergency_policy_input("5 0 1 1 1", "session-1").is_err());
        assert!(parse_emergency_policy_input("1001 10 1 1 1", "session-1").is_err());
        assert!(parse_emergency_policy_input("5 10 0 0 0", "session-1").is_err());
        assert!(parse_emergency_policy_input("5 10 2 1 1", "session-1").is_err());
        assert!(parse_emergency_policy_input("5 10 1 1", "session-1").is_err());
    }

    #[test]
    fn emergency_policy_modals_preserve_complete_safety_copy_at_narrow_width() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());

        editor.handle_key(KeyCode::Char('P'), KeyModifiers::NONE);
        let menu = render_editor_text(&mut editor, 52, 40);
        let menu = menu
            .chars()
            .map(|character| {
                if ('\u{2500}'..='\u{257f}').contains(&character) {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(menu.contains("Interactive submits always remain manual."));
        assert!(
            menu.contains("Raw transcript content, pending attachments, active tool pairs, and")
        );
        assert!(menu.contains("protected recent turns are never removed."));

        editor.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        let input = render_editor_text(&mut editor, 52, 40);
        let input = input
            .chars()
            .map(|character| {
                if ('\u{2500}'..='\u{257f}').contains(&character) {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(input.contains("one atomic transaction"));
        assert!(input.contains("and one retry"));
        assert!(input.contains("explicitly unattended execution only"));
    }

    #[test]
    fn emergency_audit_presentation_discloses_effects_without_sensitive_free_text() {
        let audit = jcode_session_types::StoredContextEmergencyAudit {
            authorization_source: "secret-audit-source".to_string(),
            scheduled_item_id: Some("sched-public".to_string()),
            policy: StoredContextEmergencyPolicy::Authorized {
                protected_recent_assistant_turns: 5,
                target_headroom_percent: 10,
                allow_reasoning_suppression: true,
                allow_tool_distillation: true,
                allow_oldest_range_summary: false,
                authorization_source: "secret-policy-source".to_string(),
            },
            trigger_kind:
                jcode_session_types::StoredContextEmergencyTriggerKind::ProviderContextLimit,
            provider_error: Some("secret provider payload".to_string()),
            context_window: 100_000,
            safe_input_budget: 95_000,
            projected_input_tokens: 98_000,
            required_reduction_to_fit_tokens: 3_000,
            required_reduction_to_target_tokens: 12_500,
            achieved_reduction_tokens: 14_000,
            protected_recent_assistant_turns: 5,
            protected_message_count: 8,
            operation_order: vec![
                jcode_session_types::StoredContextEmergencyOperationKind::ReasoningSuppression,
                jcode_session_types::StoredContextEmergencyOperationKind::ToolResultDistillation,
            ],
            retry_outcome: jcode_session_types::StoredContextEmergencyRetryOutcome::Failed {
                detail: "secret retry detail".to_string(),
            },
        };
        let rendered = rendered_text(emergency_audit_lines(&audit));
        assert!(rendered.contains("sched-public"));
        assert!(rendered.contains("ProviderContextLimit"));
        assert!(rendered.contains("14.0K"));
        assert!(rendered.contains("provider error recorded: true"));
        assert!(!rendered.contains("secret-audit-source"));
        assert!(!rendered.contains("secret-policy-source"));
        assert!(!rendered.contains("secret provider payload"));
        assert!(!rendered.contains("secret retry detail"));
    }

    #[test]
    fn exact_apply_gating_rejects_missing_stale_or_mismatched_state() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("no ready draft is available")
        );

        editor = ready_editor();
        assert_eq!(editor.apply_disabled_reason(), None);

        editor.snapshot.as_mut().expect("snapshot").processing = true;
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("session is processing")
        );
        editor.snapshot.as_mut().expect("snapshot").processing = false;

        editor.snapshot.as_mut().expect("snapshot").model = "different-model".to_string();
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("draft identity no longer matches the snapshot")
        );
        editor.snapshot.as_mut().expect("snapshot").model = "gpt-test".to_string();

        editor.stale = true;
        assert_eq!(editor.apply_disabled_reason(), Some("draft is stale"));
    }

    #[test]
    fn no_change_review_is_visible_and_cannot_queue_apply() {
        let mut editor = ready_editor();
        let mut no_change = draft();
        no_change.required_operations.clear();
        no_change.distillation_proposals.clear();
        no_change.preview.operation_previews.clear();
        no_change.preview.proposed_context_revision = no_change.preview.current_context_revision;
        no_change.preview.notices = vec![
            "No provider-context change remains after exact filtering; this is a no-op."
                .to_string(),
        ];
        editor.apply_draft_state(ContextClientDraftState::Ready(Box::new(no_change)));

        assert_eq!(
            editor.apply_disabled_reason(),
            Some("the review contains no provider-context changes")
        );
        assert!(rendered_text(editor.review_lines(120)).contains("no-op"));
        let (_, action) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, None);
        assert_eq!(editor.modal, None);
    }

    #[test]
    fn exact_apply_gating_requires_the_current_selected_proposal_preview() {
        let mut editor = ready_editor();
        editor.selection_preview_pending = true;
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("proposal economics are not current")
        );

        editor.selection_preview_pending = false;
        editor.selection_preview = None;
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("proposal economics are not current")
        );
        assert!(
            editor
                .apply_confirmation_text()
                .contains("Exact proposal economics are not current")
        );

        editor.selection_preview = Some(ContextDraftSelectionPreview {
            draft_id: "different-draft".to_string(),
            selected_distillation_ids: Vec::new(),
            preview: draft().preview,
        });
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("proposal economics do not match the selected IDs")
        );

        editor.selection_preview = Some(ContextDraftSelectionPreview {
            draft_id: "draft-1".to_string(),
            selected_distillation_ids: vec!["unknown-proposal".to_string()],
            preview: draft().preview,
        });
        editor
            .selected_distillation_ids
            .insert("unknown-proposal".to_string());
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("selected proposal is not part of the ready draft")
        );
    }

    #[test]
    fn exact_apply_gating_uses_supported_selection_validation_and_exact_economics() {
        let mut editor = ready_editor();
        editor
            .selection_preview
            .as_mut()
            .expect("selection preview")
            .preview
            .validation
            .builder_status = ContextProjectionValidationStatus::Unsupported;
        assert_eq!(
            editor.apply_disabled_reason(),
            Some("provider validation failed")
        );

        let exact_preview = editor
            .selection_preview
            .as_mut()
            .expect("selection preview");
        exact_preview.preview.validation.builder_status =
            ContextProjectionValidationStatus::Supported;
        exact_preview.preview.economics = economics(9_500, 3_250);
        assert_eq!(editor.apply_disabled_reason(), None);
        let confirmation = editor.apply_confirmation_text();
        assert!(confirmation.contains("9.5K → 3.2K"));
        assert!(!confirmation.contains("10.0K → 4.0K"));
    }

    #[test]
    fn history_page_zero_replaces_authoritative_state_and_preserves_stable_cursor() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::History);
        let first = history_state(history_page(7, 3, 0, Some(2), &["tx-a", "tx-b"]));
        editor.sync_protocol(&first);
        editor.history_cursor = 1;

        let replacement = history_state(history_page(8, 2, 0, None, &["tx-c", "tx-b"]));
        editor.sync_protocol(&replacement);

        assert_eq!(
            editor
                .history
                .iter()
                .map(|transaction| transaction.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tx-c", "tx-b"]
        );
        assert_eq!(editor.history_cursor, 1);
        assert_eq!(editor.context_revision(), Some(8));
        assert_eq!(editor.session_id(), Some("session-1"));
        assert_eq!(editor.history_total, 2);
        assert_eq!(editor.history_next_offset, None);
    }

    #[test]
    fn history_contiguous_append_and_duplicate_delivery_are_idempotent() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::History);
        editor.sync_protocol(&history_state(history_page(
            7,
            4,
            0,
            Some(2),
            &["tx-a", "tx-b"],
        )));
        let second = history_state(history_page(7, 4, 2, None, &["tx-c", "tx-d"]));
        editor.sync_protocol(&second);
        editor.sync_protocol(&second);

        assert_eq!(
            editor
                .history
                .iter()
                .map(|transaction| transaction.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tx-a", "tx-b", "tx-c", "tx-d"]
        );
        assert_eq!(editor.history_offset, 2);
        assert_eq!(editor.history_next_offset, None);
        assert!(!editor.stale);
    }

    #[test]
    fn history_rejects_gap_revision_and_overlap_mismatch_without_accepting_metadata() {
        for invalid in [
            history_page(7, 4, 3, None, &["tx-d"]),
            history_page(8, 4, 2, None, &["tx-c", "tx-d"]),
            history_page(7, 4, 1, None, &["tx-x", "tx-c"]),
        ] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::History);
            editor.sync_protocol(&history_state(history_page(
                7,
                4,
                0,
                Some(2),
                &["tx-a", "tx-b"],
            )));
            editor.sync_protocol(&history_state(invalid));

            assert!(editor.stale);
            assert_eq!(
                editor
                    .history
                    .iter()
                    .map(|transaction| transaction.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["tx-a", "tx-b"]
            );
            assert_eq!(editor.history_offset, 0);
            assert_eq!(editor.history_next_offset, Some(2));
        }
    }

    #[test]
    fn page_zero_authoritative_history_change_stales_generated_review() {
        let mut editor = ready_editor();
        editor.sync_protocol(&history_state(history_page(7, 1, 0, None, &["tx-a"])));

        assert!(editor.stale);
        assert!(editor.draft.is_none());
        assert!(editor.selection_preview.is_none());
        assert_eq!(editor.phase, ContextEditorPhase::History);
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("generated review is stale"))
        );
    }

    #[test]
    fn transaction_outcomes_are_consumed_once_and_queue_one_authoritative_refresh() {
        for (kind, expected_action) in [
            (ContextRequestKind::ApplyDraft, "Applied"),
            (ContextRequestKind::RevertTransaction, "Reverted"),
            (ContextRequestKind::ReapplyTransaction, "Reapplied"),
        ] {
            let mut editor = ready_editor();
            let correlation_id = if kind == ContextRequestKind::ApplyDraft {
                "draft-1"
            } else {
                "transaction-1"
            };
            let mut result_transaction = transaction_summary("transaction-1", true, 5);
            result_transaction.latest_status = Some(match kind {
                ContextRequestKind::RevertTransaction => {
                    StoredContextTransactionStatusKind::Reverted
                }
                ContextRequestKind::ReapplyTransaction => {
                    StoredContextTransactionStatusKind::Reapplied
                }
                _ => StoredContextTransactionStatusKind::Applied,
            });
            let mut state = ContextProtocolState::default();
            state.transaction_result = Some(ContextTransactionOutcome {
                request_id: 44,
                request: kind,
                correlation_id: correlation_id.to_string(),
                result: ContextTransactionResult {
                    transaction: result_transaction,
                    revision: 5,
                    status: StoredContextTransactionStatusKind::Applied,
                    warnings: vec!["review warning".to_string()],
                },
            });

            editor.sync_protocol(&state);
            assert_eq!(editor.phase, ContextEditorPhase::History);
            assert!(editor.draft.is_none());
            assert!(
                editor
                    .status
                    .as_deref()
                    .is_some_and(|status| status.contains(expected_action))
            );
            assert!(
                editor
                    .status
                    .as_deref()
                    .is_some_and(|status| status.contains("review warning"))
            );
            assert_eq!(
                editor.take_follow_up_action(),
                Some(ContextEditorAction::LoadHistory {
                    offset: 0,
                    limit: DEFAULT_PAGE_SIZE,
                })
            );

            editor.sync_protocol(&state);
            assert_eq!(editor.take_follow_up_action(), None);
        }
    }

    #[test]
    fn search_preserves_stable_selections_and_anchor_escape_only_cancels_anchor() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        let _ = editor.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(editor.selected_message_ids.contains("message-0"));
        let _ = editor.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        let _ = editor.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let _ = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(editor.selected_message_ids.contains("message-0"));
        let _ = editor.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(editor.summary_anchor.is_some());
        let (close, _) = editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!close);
        assert!(editor.summary_anchor.is_none());
    }

    #[test]
    fn multiple_selected_runs_become_explicit_stable_ranges() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.selected_message_ids.insert("message-0".to_string());
        editor.selected_message_ids.insert("message-2".to_string());
        assert_eq!(
            editor.selected_message_runs(),
            vec![
                ContextMessageRangeSelection {
                    start_message_id: "message-0".to_string(),
                    end_message_id: "message-0".to_string(),
                },
                ContextMessageRangeSelection {
                    start_message_id: "message-2".to_string(),
                    end_message_id: "message-2".to_string(),
                },
            ]
        );
    }

    #[test]
    fn keyboard_state_machine_covers_nested_global_editing_and_range_actions() {
        let mut global = ContextEditor::new(ContextEditorOpenMode::Edit);
        global.apply_snapshot(interaction_snapshot());

        let (close, action) = global.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(!close);
        assert_eq!(action, None);
        assert_eq!(global.modal, Some(ContextEditorModal::Help));
        let (close, action) = global.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!close);
        assert_eq!(action, None);
        assert_eq!(global.modal, None);

        global.search_query = "discard me".to_string();
        let _ = global.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(global.modal, Some(ContextEditorModal::Search));
        assert!(global.search_query.is_empty());
        let _ = global.handle_key(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(global.focus, ContextEditorPane::History);
        let _ = global.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(global.focus, ContextEditorPane::Preview);
        let _ = global.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(global.focus, ContextEditorPane::Operations);
        let _ = global.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(global.focus, ContextEditorPane::History);

        global.focus = ContextEditorPane::Preview;
        let _ = global.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(global.preview_scroll, 10);
        let _ = global.handle_key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(global.preview_scroll, 0);
        global.staged_ranges = (0..20).map(|_| closed_range(0, 1)).collect();
        global.focus = ContextEditorPane::Operations;
        render_editor(&mut global, 100, 28);
        let _ = global.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            global.operations_scroll,
            10.min(global.operations_max_scroll)
        );
        let _ = global.handle_key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(global.operations_scroll, 0);

        let mut editing = ContextEditor::new(ContextEditorOpenMode::Edit);
        editing.apply_snapshot(interaction_snapshot());
        let _ = editing.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let _ = editing.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(editing.cursor, 2);
        let _ = editing.handle_key(KeyCode::Up, KeyModifiers::NONE);
        let _ = editing.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(editing.cursor, 0);
        let _ = editing.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(editing.cursor, 6);
        let _ = editing.handle_key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(editing.cursor, 0);

        editing.cursor = 4;
        let _ = editing.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let _ = editing.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(editing.block_cursor, 2);
        let _ = editing.handle_key(KeyCode::Left, KeyModifiers::NONE);
        let _ = editing.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(editing.block_cursor, 0);
        let _ = editing.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(editing.selected_message_ids.contains("message-4"));
        let _ = editing.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!editing.selected_message_ids.contains("message-4"));

        editing.cursor = 0;
        let _ = editing.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(editing.summary_anchor.as_deref(), Some("message-0"));
        editing.cursor = 2;
        let (_, action) = editing.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::PreviewRanges {
                context_revision: 4,
                transcript_digest: 99,
                ranges: vec![ContextMessageRangeSelection {
                    start_message_id: "message-0".to_string(),
                    end_message_id: "message-2".to_string(),
                }],
            })
        );

        editing.pending_range_preview = Some(range_preview(
            vec![closed_range(0, 2)],
            vec!["active-operation-1".to_string()],
        ));
        editing.phase = ContextEditorPhase::ConfirmRangeClosure;
        let mut rejected = editing.clone();
        let (close, action) = rejected.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!close);
        assert_eq!(action, None);
        assert_eq!(rejected.phase, ContextEditorPhase::Editing);
        assert!(rejected.pending_range_preview.is_none());
        assert!(rejected.staged_ranges.is_empty());

        let (_, action) = editing.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, None);
        assert_eq!(editing.phase, ContextEditorPhase::Editing);
        assert_eq!(editing.staged_ranges, vec![closed_range(0, 2)]);
        assert!(editing.allow_shadowing_active_operations);
        editing.cursor = 1;
        let _ = editing.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(editing.staged_ranges.is_empty());

        editing.cursor = 4;
        editing.block_cursor = 2;
        let (_, detail_action) = editing.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            detail_action,
            Some(ContextEditorAction::LoadDetail {
                context_revision: 4,
                transcript_digest: 99,
                message_id: "message-4".to_string(),
                block_ordinal: 2,
                start_char: 0,
                max_chars: DEFAULT_DETAIL_CHARS,
            })
        );
        let _ = editing.handle_key(KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(editing.modal, Some(ContextEditorModal::ReasoningMenu));
        let _ = editing.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        let _ = editing.handle_key(KeyCode::Char('D'), KeyModifiers::NONE);
        assert_eq!(editing.modal, Some(ContextEditorModal::ToolScan));
        let _ = editing.handle_key(KeyCode::Esc, KeyModifiers::NONE);

        editing.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 5,
        });
        let (_, prepare) = editing.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(matches!(
            prepare,
            Some(ContextEditorAction::PrepareDraft(ContextDraftRequest {
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5
                }),
                ..
            }))
        ));

        let mut history = ContextEditor::new(ContextEditorOpenMode::Edit);
        history.apply_snapshot(interaction_snapshot());
        let (_, action) = history.handle_key(KeyCode::Char('H'), KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::LoadHistory {
                offset: 0,
                limit: DEFAULT_PAGE_SIZE,
            })
        );
        assert_eq!(history.phase, ContextEditorPhase::History);

        let mut anchor = ContextEditor::new(ContextEditorOpenMode::Edit);
        anchor.apply_snapshot(interaction_snapshot());
        let _ = anchor.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        let (close, action) = anchor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!close);
        assert_eq!(action, None);
        assert!(anchor.summary_anchor.is_none());
        let (close, action) = anchor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(close);
        assert_eq!(action, None);
    }

    #[test]
    fn modal_state_machine_unwinds_first_and_validates_exact_boundaries() {
        for modal in [
            ContextEditorModal::Search,
            ContextEditorModal::ReasoningMenu,
            ContextEditorModal::ReasoningKeepLatestInput,
            ContextEditorModal::ToolScan,
            ContextEditorModal::ApplyConfirmation,
            ContextEditorModal::RevertConfirmation,
            ContextEditorModal::ReapplyConfirmation,
            ContextEditorModal::Help,
        ] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor.apply_snapshot(interaction_snapshot());
            editor.modal = Some(modal);
            let (close, action) = editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                !close,
                "Esc closed the editor instead of unwinding {modal:?}"
            );
            assert_eq!(action, None);
            assert_eq!(editor.modal, None);
        }

        let mut search = ContextEditor::new(ContextEditorOpenMode::Edit);
        search.apply_snapshot(interaction_snapshot());
        search.selected_message_ids.insert("message-0".to_string());
        search.modal = Some(ContextEditorModal::Search);
        for character in "row 4x".chars() {
            let _ = search.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
        }
        let _ = search.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        let _ = search.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(search.search_query, "row 4");
        assert_eq!(search.current_message().unwrap().message_id, "message-4");
        assert!(search.selected_message_ids.contains("message-0"));

        let mut reasoning = ContextEditor::new(ContextEditorOpenMode::Edit);
        reasoning.apply_snapshot(interaction_snapshot());
        reasoning
            .selected_message_ids
            .extend(["message-0".to_string(), "message-1".to_string()]);
        reasoning.modal = Some(ContextEditorModal::ReasoningMenu);
        let _ = reasoning.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(
            reasoning.reasoning,
            Some(ContextReasoningSelectionRequest::MessageRanges {
                ranges: vec![ContextMessageRangeSelection {
                    start_message_id: "message-0".to_string(),
                    end_message_id: "message-1".to_string(),
                }],
            })
        );
        reasoning.modal = Some(ContextEditorModal::ReasoningMenu);
        let _ = reasoning.handle_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert!(reasoning.reasoning.is_none());
        reasoning.modal = Some(ContextEditorModal::ReasoningMenu);
        let _ = reasoning.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(
            reasoning.modal,
            Some(ContextEditorModal::ReasoningKeepLatestInput)
        );

        for value in [0, 1_000] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor.apply_snapshot(interaction_snapshot());
            editor.modal = Some(ContextEditorModal::ReasoningKeepLatestInput);
            editor.reasoning_input = value.to_string();
            let _ = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(editor.modal, None);
            assert_eq!(
                editor.reasoning,
                Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: value,
                })
            );
        }
        for invalid in ["", "non-numeric", "1001"] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor.apply_snapshot(interaction_snapshot());
            editor.modal = Some(ContextEditorModal::ReasoningKeepLatestInput);
            editor.reasoning_input = invalid.to_string();
            let _ = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                editor.modal,
                Some(ContextEditorModal::ReasoningKeepLatestInput)
            );
            assert!(editor.reasoning.is_none());
            assert!(
                editor
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("0 to 1000"))
            );
        }

        assert_eq!(parse_tool_scan_input("0 0"), Ok((0, 0)));
        assert_eq!(parse_tool_scan_input("5500 1000"), Ok((5_500, 1_000)));
        for invalid in ["", "5500", "5500 5 extra", "x 5", "5500 1001"] {
            assert!(
                parse_tool_scan_input(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }

        let mut help = ContextEditor::new(ContextEditorOpenMode::Edit);
        help.apply_snapshot(interaction_snapshot());
        help.modal = Some(ContextEditorModal::Help);
        let _ = help.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(help.modal, None);
    }

    #[test]
    fn tool_navigation_and_scan_use_exact_block_identity_and_exclusions() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(interaction_snapshot());
        editor.cursor = 4;
        assert_eq!(editor.block_cursor, 0);
        let _ = editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let _ = editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(editor.block_cursor, 2);
        let _ = editor.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            editor.tool_targets,
            BTreeSet::from([("message-4".to_string(), 2)])
        );
        let _ = editor.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(editor.tool_targets.is_empty());
        let _ = editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let _ = editor.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            editor.tool_targets,
            BTreeSet::from([("message-4".to_string(), 3)])
        );
        let _ = editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(editor.block_cursor, 2);

        editor.tool_targets.clear();
        editor.staged_ranges = vec![closed_range(2, 2)];
        editor.modal = Some(ContextEditorModal::ToolScan);
        editor.tool_scan_input = "5500 1".to_string();
        let _ = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.modal, None);
        assert_eq!(
            editor.tool_targets,
            BTreeSet::from([("message-4".to_string(), 2)])
        );
        assert!(
            editor
                .status
                .as_deref()
                .is_some_and(|status| status.contains("mechanical candidate"))
        );

        editor.modal = Some(ContextEditorModal::ToolScan);
        editor.tool_scan_input = "invalid".to_string();
        let previous_targets = editor.tool_targets.clone();
        let _ = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.modal, Some(ContextEditorModal::ToolScan));
        assert_eq!(editor.tool_targets, previous_targets);
        assert!(editor.error.is_some());
        let _ = editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(editor.modal, None);

        editor.cursor = 2;
        editor.block_cursor = 0;
        let _ = editor.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(editor.tool_targets, previous_targets);
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("inside a staged summary range"))
        );
    }

    #[test]
    fn draft_review_history_and_transaction_detail_actions_are_exactly_gated() {
        let mut preparing = ContextEditor::new(ContextEditorOpenMode::Edit);
        preparing.apply_snapshot(interaction_snapshot());
        preparing.phase = ContextEditorPhase::PreparingDraft;
        let (_, action) = preparing.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(action, None);
        preparing.draft_id = Some("draft-cancel".to_string());
        let (_, action) = preparing.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::CancelDraft {
                draft_id: "draft-cancel".to_string(),
            })
        );
        let before = interaction_signature(&preparing);
        let (_, action) = preparing.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(action, None);
        assert_eq!(interaction_signature(&preparing), before);

        let mut review = ContextEditor::new(ContextEditorOpenMode::Edit);
        review.apply_snapshot(snapshot());
        let comprehensive = comprehensive_draft("review-matrix");
        review.apply_draft_state(ContextClientDraftState::Ready(Box::new(
            comprehensive.clone(),
        )));
        assert_eq!(
            review.selected_distillation_ids,
            BTreeSet::from(["proposal-1".to_string()])
        );
        review.curator_workspace.review_cursor = 1 + comprehensive.required_operations.len();
        let (_, action) = review.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::PreviewDraftSelection {
                draft_id: "draft-1".to_string(),
                selected_distillation_ids: Vec::new(),
            })
        );
        assert!(review.selected_distillation_ids.is_empty());
        assert!(review.selection_preview_pending);
        assert!(review.selection_preview.is_none());
        let _ = review.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            !render_editor_text(&mut review, 120, 40).contains("Apply atomic context transaction")
        );
        review.selection_preview_pending = false;
        review.selection_preview = Some(ContextDraftSelectionPreview {
            draft_id: comprehensive.identity.draft_id.clone(),
            selected_distillation_ids: Vec::new(),
            preview: comprehensive.preview.clone(),
        });
        let _ = review.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            render_editor_text(&mut review, 120, 40).contains("Apply atomic context transaction")
        );
        let (_, action) = review.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            action,
            Some(ContextEditorAction::ApplyDraft {
                draft_id: "draft-1".to_string(),
                selected_distillation_ids: Vec::new(),
            })
        );
        let _ = review.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            render_editor_text(&mut review, 120, 40).contains("Apply atomic context transaction")
        );
        review
            .snapshot
            .as_mut()
            .expect("review snapshot")
            .processing = true;
        let (_, action) = review.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, None);
        assert_eq!(review.modal, None);
        assert!(
            review
                .error
                .as_deref()
                .is_some_and(|error| error.contains("session is processing"))
        );

        let mut edit_preserves_intent = ContextEditor::new(ContextEditorOpenMode::Edit);
        edit_preserves_intent.apply_snapshot(snapshot());
        edit_preserves_intent.staged_ranges = vec![closed_range(0, 1)];
        edit_preserves_intent.reasoning =
            Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            });
        edit_preserves_intent
            .tool_targets
            .insert(("message-2".to_string(), 0));
        edit_preserves_intent.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft())));
        let _ = edit_preserves_intent.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(edit_preserves_intent.phase, ContextEditorPhase::Editing);
        assert_eq!(
            edit_preserves_intent.staged_ranges,
            vec![closed_range(0, 1)]
        );
        assert!(edit_preserves_intent.reasoning.is_some());
        assert_eq!(edit_preserves_intent.tool_targets.len(), 1);
        assert!(edit_preserves_intent.draft.is_none());
        assert!(edit_preserves_intent.selection_preview.is_none());

        let mut processing_snapshot = interaction_snapshot();
        processing_snapshot.processing = true;
        let mut processing = ContextEditor::new(ContextEditorOpenMode::Edit);
        processing.apply_snapshot(processing_snapshot);
        processing.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 5,
        });
        let (_, action) = processing.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(action, None);
        assert_eq!(processing.phase, ContextEditorPhase::Editing);
        assert!(
            processing
                .error
                .as_deref()
                .is_some_and(|error| error.contains("turn to finish"))
        );

        let active = transaction_summary("active", true, 8);
        let inactive = transaction_summary("inactive", false, 7);
        let mut history = ContextEditor::new(ContextEditorOpenMode::History);
        history.phase = ContextEditorPhase::History;
        history.history = vec![active.clone(), inactive.clone()];
        history.history_total = 3;
        history.history_context_revision = Some(8);
        history.history_next_offset = Some(2);
        let _ = history.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let _ = history.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(history.history_cursor, 1);
        let _ = history.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(history.history_cursor, 0);
        let (_, inspect) = history.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            inspect,
            Some(ContextEditorAction::LoadTransactionDetail {
                context_revision: 8,
                transaction_id: "active".to_string(),
            })
        );
        let _ = history.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(history.modal, Some(ContextEditorModal::RevertConfirmation));
        let (_, revert) = history.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            revert,
            Some(ContextEditorAction::RevertTransaction {
                transaction_id: "active".to_string(),
            })
        );
        let _ = history.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(history.modal, Some(ContextEditorModal::RevertConfirmation));
        history.history[0].active = false;
        let (_, revert) = history.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(revert, None);
        assert_eq!(history.modal, None);
        assert!(
            history
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no longer active"))
        );
        history.history_cursor = 1;
        let _ = history.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
        assert_eq!(history.modal, Some(ContextEditorModal::ReapplyConfirmation));
        let (_, reapply) = history.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(
            reapply,
            Some(ContextEditorAction::ReapplyTransaction {
                transaction_id: "inactive".to_string(),
            })
        );
        let _ = history.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
        assert_eq!(history.modal, Some(ContextEditorModal::ReapplyConfirmation));
        history.history[1].active = true;
        let (_, reapply) = history.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(reapply, None);
        assert_eq!(history.modal, None);
        assert!(
            history
                .error
                .as_deref()
                .is_some_and(|error| error.contains("already active"))
        );
        let (_, next_page) = history.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            next_page,
            Some(ContextEditorAction::LoadHistory {
                offset: 2,
                limit: DEFAULT_PAGE_SIZE,
            })
        );
        let (_, copied) = history.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(matches!(
            copied,
            Some(ContextEditorAction::CopySafeMetadata(text)) if text.contains("inactive")
        ));

        let mut undo = ContextEditor::new(ContextEditorOpenMode::UndoLatest);
        let mut undo_page = history_page(8, 2, 0, None, &["old-reverted", "latest-active"]);
        undo_page.transactions[0] = transaction_summary("old-reverted", false, 7);
        undo_page.transactions[1] = transaction_summary("latest-active", true, 8);
        undo.sync_protocol(&history_state(undo_page));
        assert_eq!(undo.history_cursor, 1);
        assert_eq!(undo.modal, Some(ContextEditorModal::RevertConfirmation));

        let detail_draft = comprehensive_draft("detail-matrix");
        let transaction_detail = transaction_detail_from_draft("detail-matrix", &detail_draft);
        let mut inspect = ContextEditor::new(ContextEditorOpenMode::History);
        inspect.phase = ContextEditorPhase::InspectTransaction;
        inspect.transaction_detail = Some(transaction_detail);
        let _ = inspect.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let _ = inspect.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(inspect.preview_scroll, 2);
        let _ = inspect.handle_key(KeyCode::Up, KeyModifiers::NONE);
        let _ = inspect.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(inspect.preview_scroll, 0);
        let (_, copied) = inspect.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(matches!(
            copied,
            Some(ContextEditorAction::CopySafeMetadata(text)) if text.contains("transaction-detail-1")
        ));
        let (close, action) = inspect.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!close);
        assert_eq!(action, None);
        assert_eq!(inspect.phase, ContextEditorPhase::History);
        assert!(inspect.transaction_detail.is_none());
    }

    #[test]
    fn every_enabled_toolbar_click_matches_its_keyboard_equivalent_and_disabled_actions_are_inert()
    {
        let editing = || {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor.apply_snapshot(interaction_snapshot());
            editor.cursor = 4;
            editor.block_cursor = 2;
            editor.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            });
            editor
        };
        for (action, key) in [
            (ContextEditorToolbarAction::Range, KeyCode::Char('s')),
            (ContextEditorToolbarAction::Reasoning, KeyCode::Char('R')),
            (ContextEditorToolbarAction::ToggleOutput, KeyCode::Char('d')),
            (ContextEditorToolbarAction::ScanOutputs, KeyCode::Char('D')),
            (ContextEditorToolbarAction::Prepare, KeyCode::Char('g')),
            (ContextEditorToolbarAction::History, KeyCode::Char('H')),
            (ContextEditorToolbarAction::Detail, KeyCode::Enter),
        ] {
            assert_toolbar_matches_key(editing(), action, key);
        }

        let mut closure = editing();
        closure.phase = ContextEditorPhase::ConfirmRangeClosure;
        closure.pending_range_preview = Some(range_preview(vec![closed_range(0, 2)], Vec::new()));
        assert_toolbar_matches_key(
            closure.clone(),
            ContextEditorToolbarAction::ConfirmRange,
            KeyCode::Enter,
        );
        assert_toolbar_matches_key(
            closure,
            ContextEditorToolbarAction::RejectRange,
            KeyCode::Char('n'),
        );

        let mut preparing = editing();
        preparing.phase = ContextEditorPhase::PreparingDraft;
        preparing.draft_id = Some("draft-cancel".to_string());
        assert_toolbar_matches_key(
            preparing,
            ContextEditorToolbarAction::CancelDraft,
            KeyCode::Char('c'),
        );

        let mut review = ContextEditor::new(ContextEditorOpenMode::Edit);
        review.apply_snapshot(snapshot());
        review.apply_draft_state(ContextClientDraftState::Ready(Box::new(
            comprehensive_draft("toolbar"),
        )));
        review.curator_workspace.active = false;
        assert_toolbar_matches_key(
            review.clone(),
            ContextEditorToolbarAction::ToggleProposal,
            KeyCode::Char(' '),
        );
        assert_toolbar_matches_key(
            review.clone(),
            ContextEditorToolbarAction::Apply,
            KeyCode::Char('a'),
        );
        assert_toolbar_matches_key(review, ContextEditorToolbarAction::Edit, KeyCode::Char('e'));

        let mut active_history = ContextEditor::new(ContextEditorOpenMode::History);
        active_history.phase = ContextEditorPhase::History;
        active_history.history = vec![transaction_summary("active", true, 8)];
        active_history.history_total = 2;
        active_history.history_context_revision = Some(8);
        active_history.history_next_offset = Some(1);
        assert_toolbar_matches_key(
            active_history.clone(),
            ContextEditorToolbarAction::Inspect,
            KeyCode::Enter,
        );
        assert_toolbar_matches_key(
            active_history.clone(),
            ContextEditorToolbarAction::Revert,
            KeyCode::Char('r'),
        );
        assert_toolbar_matches_key(
            active_history.clone(),
            ContextEditorToolbarAction::CopyMetadata,
            KeyCode::Char('c'),
        );
        assert_toolbar_matches_key(
            active_history,
            ContextEditorToolbarAction::NextHistoryPage,
            KeyCode::PageDown,
        );

        let mut inactive_history = ContextEditor::new(ContextEditorOpenMode::History);
        inactive_history.phase = ContextEditorPhase::History;
        inactive_history.history = vec![transaction_summary("inactive", false, 8)];
        inactive_history.history_total = 1;
        inactive_history.history_context_revision = Some(8);
        assert_toolbar_matches_key(
            inactive_history,
            ContextEditorToolbarAction::Reapply,
            KeyCode::Char('p'),
        );

        let detail_draft = comprehensive_draft("toolbar-detail");
        let mut detail = ContextEditor::new(ContextEditorOpenMode::History);
        detail.phase = ContextEditorPhase::InspectTransaction;
        detail.transaction_detail = Some(transaction_detail_from_draft(
            "toolbar-detail",
            &detail_draft,
        ));
        assert_toolbar_matches_key(
            detail.clone(),
            ContextEditorToolbarAction::BackToHistory,
            KeyCode::Esc,
        );
        assert_toolbar_matches_key(
            detail,
            ContextEditorToolbarAction::CopyMetadata,
            KeyCode::Char('c'),
        );

        let mut disabled_apply = ready_editor();
        disabled_apply
            .snapshot
            .as_mut()
            .expect("snapshot")
            .processing = true;
        let rendered = render_editor_text(&mut disabled_apply, 120, 36);
        assert!(rendered.contains("(Apply transaction)"));
        assert!(
            disabled_apply
                .hit_regions
                .toolbar
                .iter()
                .all(|(_, action)| *action != ContextEditorToolbarAction::Apply)
        );
        assert_eq!(
            disabled_apply.activate_toolbar(ContextEditorToolbarAction::Apply),
            None
        );

        let mut active_only = ContextEditor::new(ContextEditorOpenMode::History);
        active_only.phase = ContextEditorPhase::History;
        active_only.history = vec![transaction_summary("active", true, 8)];
        let rendered = render_editor_text(&mut active_only, 120, 36);
        assert!(rendered.contains("(Reapply)"));
        assert!(
            active_only
                .hit_regions
                .toolbar
                .iter()
                .all(|(_, action)| *action != ContextEditorToolbarAction::Reapply)
        );
    }

    #[test]
    fn extreme_narrow_layout_keeps_focus_actions_and_scrolling_usable() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(interaction_snapshot());
        editor.cursor = 4;
        editor.block_cursor = 2;
        editor.reasoning = Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
            protected_recent_assistant_turns: 5,
        });
        let rendered = render_editor_text(&mut editor, 54, 34);
        assert!(editor.narrow_layout());
        assert!(rendered.contains("[focused]"));
        assert_toolbar_rectangles_do_not_overlap(&editor);
        let actions = editor
            .hit_regions
            .toolbar
            .iter()
            .map(|(_, action)| *action)
            .collect::<BTreeSet<_>>();
        for required in [
            ContextEditorToolbarAction::Range,
            ContextEditorToolbarAction::Reasoning,
            ContextEditorToolbarAction::ToggleOutput,
            ContextEditorToolbarAction::ScanOutputs,
            ContextEditorToolbarAction::Detail,
            ContextEditorToolbarAction::Prepare,
            ContextEditorToolbarAction::History,
        ] {
            assert!(
                actions.contains(&required),
                "missing {required:?} at 54 columns"
            );
        }

        let _ = editor.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 54, 34);
        assert!(rendered.contains("Preview / review [focused]"));
        let _ = editor.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        let rendered = render_editor_text(&mut editor, 54, 34);
        assert!(rendered.contains("Staged operations / status [focused]"));

        editor.staged_ranges = (0..30).map(|_| closed_range(0, 1)).collect();
        render_editor(&mut editor, 54, 34);
        assert!(editor.operations_max_scroll > 0);
        for _ in 0..100 {
            let _ = editor.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        }
        assert_eq!(editor.operations_scroll, editor.operations_max_scroll);
        for _ in 0..100 {
            let _ = editor.handle_key(KeyCode::PageUp, KeyModifiers::NONE);
        }
        assert_eq!(editor.operations_scroll, 0);

        let mut review = ready_editor();
        let rendered = render_editor_text(&mut review, 54, 34);
        assert!(review.narrow_layout());
        assert!(rendered.contains("Prepare context review"));
        assert!(rendered.contains("[Apply transaction]"));
        assert!(rendered.contains("[Edit run]"));
        assert!(rendered.contains("Atomic transaction review"));

        let rendered = render_editor_text(&mut review, 52, 28);
        assert!(review.narrow_layout());
        assert!(rendered.contains("[Apply transaction]"));
        assert!(rendered.contains("Enter inspect"));
        assert!(rendered.contains("Space toggle"));
    }

    #[test]
    fn critical_status_errors_and_history_context_are_visible_without_scrolling() {
        for (fixture, expected) in [
            (
                "keep-latest-statistics",
                "Status: Resolved 11 replayed reasoning blocks",
            ),
            (
                "trace-only-zero-saving",
                "Status: Visible ReasoningTrace text is transcript-only",
            ),
            (
                "failure-retry",
                "Error: Synthetic curator output omitted a requested artifact",
            ),
            ("expired-draft", "Error: The retained draft expired"),
            (
                "stale-review",
                "Error: Authoritative history changed after generation",
            ),
        ] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor
                .apply_debug_fixture(fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture} failed: {error}"));
            let rendered = render_editor_text(&mut editor, 140, 48);
            assert!(rendered.contains(expected), "fixture {fixture}: {rendered}");
        }

        let mut invalid = ContextEditor::new(ContextEditorOpenMode::Edit);
        invalid
            .apply_debug_fixture("invalid-reasoning-input")
            .expect("invalid reasoning fixture");
        let rendered = render_editor_text(&mut invalid, 52, 48);
        assert!(
            rendered.contains("Error: Protected recent assistant turns"),
            "{rendered}"
        );

        for fixture in ["history", "transaction-detail"] {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::History);
            editor
                .apply_debug_fixture(fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture} failed: {error}"));
            let rendered = render_editor_text(&mut editor, 140, 48);
            assert!(
                rendered.contains("Context revision 12 · 36 authoritative transaction(s)"),
                "fixture {fixture}: {rendered}"
            );
            assert!(!rendered.contains("Loading authoritative context state"));
        }
    }

    #[test]
    fn production_debug_fixture_matrix_renders_and_exposes_only_safe_metadata() {
        let names = ContextEditor::debug_fixture_names();
        assert!(names.len() >= 35, "visual acceptance matrix is incomplete");
        assert_eq!(
            names.iter().copied().collect::<BTreeSet<_>>().len(),
            names.len(),
            "debug fixture names must be unique"
        );

        for name in names {
            let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
            editor
                .apply_debug_fixture(name)
                .unwrap_or_else(|error| panic!("fixture {name} failed: {error}"));
            let (width, height) = if *name == "narrow-terminal" {
                (72, 34)
            } else {
                (140, 48)
            };
            render_editor(&mut editor, width, height);
            let summary = serde_json::to_string(&editor.debug_summary())
                .expect("serialize debug fixture summary");
            assert!(summary.contains("\"open\":true"), "fixture {name}");
            assert!(!summary.contains("synthetic-debug-source-not-rendered"));
            assert!(!summary.contains("replacement_content"));
            assert!(!summary.contains("preservation_rationale"));
            assert!(!summary.contains("summary_text"));
        }
    }

    #[test]
    fn debug_summary_never_contains_raw_preview_content() {
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        let mut sensitive_snapshot = snapshot();
        sensitive_snapshot.messages[0].preview = "FAKE_SECRET_CONTEXT_EDITOR".to_string();
        editor.apply_snapshot(sensitive_snapshot);
        let encoded =
            serde_json::to_string(&editor.debug_summary()).expect("serialize debug state");
        assert!(!encoded.contains("FAKE_SECRET_CONTEXT_EDITOR"));
        assert!(encoded.contains("message-0"));
    }

    #[test]
    fn full_review_and_transaction_detail_preserve_every_multiline_tail() {
        let draft = comprehensive_draft("LONG_FIELD");
        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(snapshot());
        editor.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft.clone())));

        let review = rendered_text(editor.review_lines(80));
        for expected in [
            "LONG_FIELD SUMMARY_TAIL",
            "LONG_FIELD FILE_DIGEST_TAIL",
            "LONG_FIELD CURATOR_WARNING_TAIL",
            "LONG_FIELD PROVIDER_WARNING_TAIL",
            "LONG_FIELD REPLACEMENT_TAIL",
            "LONG_FIELD RATIONALE_TAIL",
            "LONG_FIELD INELIGIBLE_TAIL",
            "LONG_FIELD NOTICE_TAIL",
            "LONG_FIELD NORMALIZATION_TAIL",
            "LONG_FIELD VALIDATION_FINDING_TAIL",
            "LONG_FIELD ECONOMICS_TAIL",
        ] {
            assert!(review.contains(expected), "missing review field {expected}");
        }

        editor.transaction_detail = Some(transaction_detail_from_draft("LONG_FIELD", &draft));
        let detail = rendered_text(editor.transaction_detail_lines(80));
        assert!(
            !detail.contains("LONG_FIELD AUTHORIZATION_TAIL"),
            "authorization provenance must not be rendered as free text"
        );
        for expected in [
            "LONG_FIELD STATUS_REASON_TAIL",
            "LONG_FIELD SUMMARY_TAIL",
            "LONG_FIELD FILE_DIGEST_TAIL",
            "LONG_FIELD CURATOR_WARNING_TAIL",
            "LONG_FIELD PROVIDER_WARNING_TAIL",
            "LONG_FIELD REPLACEMENT_TAIL",
            "LONG_FIELD RATIONALE_TAIL",
            "LONG_FIELD ECONOMICS_TAIL",
        ] {
            assert!(detail.contains(expected), "missing detail field {expected}");
        }
    }

    #[test]
    fn debug_summary_omits_sensitive_detail_draft_and_transaction_content() {
        const SECRET: &str = "FAKE_SECRET_CONTEXT_ADMINISTRATION";
        let draft = comprehensive_draft(SECRET);
        let mut sensitive_snapshot = snapshot();
        sensitive_snapshot.curator_route = None;
        sensitive_snapshot.curator_unavailable_reason = Some(format!("{SECRET} route failure"));
        sensitive_snapshot.messages[0].preview = format!("{SECRET} preview");

        let mut editor = ContextEditor::new(ContextEditorOpenMode::Edit);
        editor.apply_snapshot(sensitive_snapshot);
        editor.apply_draft_state(ContextClientDraftState::Ready(Box::new(draft.clone())));
        editor.transaction_detail = Some(transaction_detail_from_draft(SECRET, &draft));
        let mut sensitive_detail =
            detail(0, SECRET.chars().count(), SECRET.chars().count(), SECRET);
        sensitive_detail.provider_status = Some(format!("{SECRET} provider status"));
        sensitive_detail.image_media_type = Some(format!("{SECRET}/image"));
        sensitive_detail.opaque_signature_present = true;
        sensitive_detail.encrypted_state_present = true;
        editor.detail_buffers.insert(
            ("message-0".to_string(), 0),
            ContextDetailBuffer::new(sensitive_detail).expect("valid sensitive detail fixture"),
        );
        editor.status = Some(format!("{SECRET} status"));
        editor.error = Some(format!("{SECRET} error"));

        let encoded =
            serde_json::to_string(&editor.debug_summary()).expect("serialize debug state");
        assert!(!encoded.contains(SECRET));
        assert!(encoded.contains("message-0"));
        assert!(encoded.contains("draft-1"));
        assert!(encoded.contains("transaction"));
        assert!(encoded.contains("loaded_detail_blocks"));
    }
}
