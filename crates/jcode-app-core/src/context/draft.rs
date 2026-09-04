use crate::agent::Agent;
use crate::context::change_digest::extract_context_file_evidence;
use crate::context::commit::selected_distillation_operations;
#[cfg(test)]
use crate::context::curator::RANGE_SUMMARIZER_BASE_PROMPT;
use crate::context::curator::{
    CONTEXT_RANGE_SUMMARIZER_PROMPT_VERSION, CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
    ContextCuratorArtifacts, ContextCuratorLimits, ContextCuratorPlan, ContextCuratorPlanInput,
    ContextCuratorRangeWork, ContextCuratorRoute, ContextCuratorToolArtifact,
    ContextCuratorToolWork, build_context_curator_plan, curator_route_options,
    resolve_context_curator_route, run_context_curator_plan,
};
use crate::context::provider_validation::require_supported_projected_messages;
use crate::context::{
    ContextPersistence, DirectContextSessionPersistence, DirectSessionContextPersistence,
    SessionContextPersistence,
};
use crate::message::ContentBlock;
#[cfg(test)]
use crate::protocol::ContextToolResultSelection;
use crate::protocol::{
    CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS, CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS,
    CONTEXT_IDENTIFIER_MAX_CHARS, CONTEXT_MAX_SUMMARY_RANGES, ContextClosedRangePreview,
    ContextCuratorPlanPreview, ContextCuratorRunConfig, ContextCuratorSelection,
    ContextDistillationProposal, ContextDraft, ContextDraftIdentity, ContextDraftPhase,
    ContextDraftPreview, ContextDraftProgress, ContextDraftRequest, ContextDraftSelectionPreview,
    ContextDraftStatus, ContextIneligibleDistillation, ContextMessageRangeSelection,
    ContextOperationPreview, ContextRangeClosurePreview, ContextReasoningSelectionRequest,
    ContextServiceError,
};
use crate::provider::ContextProjectionValidationReport;
use crate::provider::{
    ContextProjectionOperationKind, ContextProjectionValidationOperation,
    ContextReasoningBlockKind, ModelRoute, Provider,
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
    StoredContextAuthorization, StoredContextBlockKind, StoredContextCuratorRole,
    StoredContextCuratorSelectionSource, StoredContextCuratorUsage, StoredContextEconomics,
    StoredContextFileEvidence, StoredContextOperation, StoredContextStatusEvent,
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

fn bounded_context_metadata(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    let mut bounded = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn curator_selection_from_config(
    config: &crate::config::ContextCuratorConfig,
) -> ContextCuratorSelection {
    ContextCuratorSelection {
        provider: config.provider.clone(),
        route: config.route.clone(),
        model: config.model.clone(),
        effort: config.effort.clone(),
    }
}

fn effective_curator_config(
    configured_default: &crate::config::ContextCuratorConfig,
    run: &ContextCuratorRunConfig,
) -> (
    crate::config::ContextCuratorConfig,
    StoredContextCuratorSelectionSource,
) {
    match run.selection.as_ref() {
        None => (
            configured_default.clone(),
            StoredContextCuratorSelectionSource::ConfiguredDefault,
        ),
        Some(selection) => (
            crate::config::ContextCuratorConfig {
                provider: selection.provider.clone(),
                route: selection.route.clone(),
                model: selection.model.clone(),
                effort: selection.effort.clone(),
            },
            StoredContextCuratorSelectionSource::PerRunOverride,
        ),
    }
}

pub(crate) fn validate_context_curator_selection(
    selection: &ContextCuratorSelection,
) -> Result<(), ContextServiceError> {
    for (value, label) in [
        (selection.provider.as_deref(), "curator provider"),
        (selection.route.as_deref(), "curator route"),
        (selection.model.as_deref(), "curator model"),
        (selection.effort.as_deref(), "curator effort"),
    ] {
        if let Some(value) = value {
            let chars = value.chars().count();
            if value.trim().is_empty() || chars > CONTEXT_IDENTIFIER_MAX_CHARS {
                return Err(ContextServiceError::InvalidSelection(format!(
                    "{label} must contain between 1 and {CONTEXT_IDENTIFIER_MAX_CHARS} characters"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_context_curator_run_config(
    run: &ContextCuratorRunConfig,
) -> Result<(), ContextServiceError> {
    if let Some(selection) = run.selection.as_ref() {
        validate_context_curator_selection(selection)?;
    }
    if run.range_instructions.len() > CONTEXT_MAX_SUMMARY_RANGES {
        return Err(ContextServiceError::InvalidSelection(format!(
            "at most {CONTEXT_MAX_SUMMARY_RANGES} range-specific curator instructions may be supplied"
        )));
    }
    let mut total_chars = run.transaction_instructions.chars().count();
    if total_chars > CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS {
        return Err(ContextServiceError::InvalidSelection(format!(
            "transaction-wide curator instructions contain {total_chars} characters, exceeding the {CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS}-character per-field bound"
        )));
    }
    for item in &run.range_instructions {
        let chars = item.instructions.chars().count();
        if chars > CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS {
            return Err(ContextServiceError::InvalidSelection(format!(
                "range-specific curator instructions contain {chars} characters, exceeding the {CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS}-character per-field bound"
            )));
        }
        total_chars = total_chars.saturating_add(chars);
    }
    if total_chars > CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS {
        return Err(ContextServiceError::InvalidSelection(format!(
            "curator instructions contain {total_chars} characters in total, exceeding the {CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS}-character bound"
        )));
    }
    if let Some(fingerprint) = run.expected_plan_fingerprint.as_deref()
        && (fingerprint.len() != 64
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    {
        return Err(ContextServiceError::InvalidSelection(
            "curator plan fingerprint must be exactly 64 lowercase hexadecimal characters"
                .to_string(),
        ));
    }
    Ok(())
}

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
    pub(crate) direct_session_persistence: Arc<dyn DirectContextSessionPersistence>,
    pub(crate) limits: ContextServiceLimits,
}

/// Owned, immutable inputs for preparing a draft outside an Agent lock.
///
/// Local TUI mode and server mode use the same capture, curator, projection,
/// provider-validation, and economics pipeline. Current session identity is
/// revalidated again before application.
pub struct ContextDraftRuntimeInput {
    pub session_id: String,
    pub messages: Vec<StoredMessage>,
    pub context_view: StoredContextViewState,
    pub provider: Arc<dyn Provider>,
    pub route: String,
    pub model_routes: Vec<ModelRoute>,
    pub estimated_total_request_tokens_before: Option<usize>,
    pub active_agent_profile_message_id: Option<String>,
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
            direct_session_persistence: Arc::new(DirectSessionContextPersistence),
            limits,
        }
    }

    /// Construct the service with explicit persistence boundaries.
    ///
    /// This supports integration tests at owning application boundaries while
    /// keeping persistence failure injection out of transaction semantics.
    pub fn with_persistence_boundaries(
        limits: ContextServiceLimits,
        persistence: Arc<dyn ContextPersistence>,
        direct_session_persistence: Arc<dyn DirectContextSessionPersistence>,
    ) -> Self {
        Self {
            drafts: Mutex::new(ContextDraftStore::default()),
            persistence,
            direct_session_persistence,
            limits,
        }
    }

    pub fn context_editor_snapshot(
        &self,
        agent: &mut Agent,
        processing: bool,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        let provider = agent.provider_handle();
        self.context_editor_snapshot_for_session(
            agent.session_id(),
            agent.messages(),
            agent.context_view_state(),
            processing,
            provider.as_ref(),
            &agent.context_route_identity(),
            agent.current_context_request_token_estimate(),
            agent.active_transition_message_id(),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "snapshot construction keeps exact session, provider, route, and request-estimate identity"
    )]
    pub fn context_editor_snapshot_for_session(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        projected_request_tokens: Option<usize>,
        active_agent_profile_message_id: Option<&str>,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        self.context_editor_snapshot_for_session_with_curator_config_and_active_profile(
            session_id,
            messages,
            context_view,
            processing,
            provider,
            route,
            projected_request_tokens,
            active_agent_profile_message_id,
            &crate::config::config().context.curator,
        )
    }

    #[cfg(test)]
    #[expect(
        clippy::too_many_arguments,
        reason = "legacy test helper retains exact session, provider, route, request-estimate, and curator-selection identity"
    )]
    fn context_editor_snapshot_for_session_with_curator_config(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        projected_request_tokens: Option<usize>,
        curator_config: &crate::config::ContextCuratorConfig,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        self.context_editor_snapshot_for_session_with_curator_config_and_active_profile(
            session_id,
            messages,
            context_view,
            processing,
            provider,
            route,
            projected_request_tokens,
            None,
            curator_config,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "snapshot construction keeps exact session, provider, route, request-estimate, profile, and curator-selection identity"
    )]
    fn context_editor_snapshot_for_session_with_curator_config_and_active_profile(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        projected_request_tokens: Option<usize>,
        active_agent_profile_message_id: Option<&str>,
        curator_config: &crate::config::ContextCuratorConfig,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        let mut snapshot =
            crate::context::build_context_editor_snapshot(crate::context::ContextSnapshotInput {
                session_id,
                messages,
                context_view,
                processing,
                provider,
                route,
            })
            .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
        for message in &mut snapshot.messages {
            message.active_agent_profile =
                active_agent_profile_message_id == Some(message.message_id.as_str());
        }
        if let Some(projected_request_tokens) = projected_request_tokens {
            snapshot.projected_request_tokens = projected_request_tokens;
        }
        snapshot.curator_default = curator_selection_from_config(curator_config);
        snapshot.curator_route_options = curator_route_options(&provider.model_routes());
        match resolve_context_curator_route(
            provider.fork(),
            &provider.model_routes(),
            route,
            curator_config,
        ) {
            Ok(curator_route) => {
                snapshot.curator_route = Some(crate::protocol::ContextCuratorRoutePreview {
                    provider_name: curator_route.provider_name,
                    provider_display_name: curator_route.provider_display_name,
                    model: curator_route.model,
                    route: curator_route.route,
                    effort: curator_route.effort,
                });
                snapshot.curator_unavailable_reason = None;
            }
            Err(error) => {
                snapshot.curator_route = None;
                snapshot.curator_unavailable_reason = Some(bounded_context_metadata(
                    &error.to_string(),
                    CONTEXT_IDENTIFIER_MAX_CHARS,
                ));
            }
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
        reason = "paged snapshots preserve every authoritative identity dimension"
    )]
    pub fn context_editor_snapshot_page_for_session(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        projected_request_tokens: Option<usize>,
        active_agent_profile_message_id: Option<&str>,
        page_start: usize,
        page_size: usize,
    ) -> Result<crate::context::ContextEditorSnapshot, ContextServiceError> {
        let snapshot = self.context_editor_snapshot_for_session(
            session_id,
            messages,
            context_view,
            processing,
            provider,
            route,
            projected_request_tokens,
            active_agent_profile_message_id,
        )?;
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

    #[expect(
        clippy::too_many_arguments,
        reason = "lazy detail identity and bounded chunk coordinates are independent protocol fields"
    )]
    pub fn context_message_detail_for_session(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        message_id: &str,
        block_ordinal: usize,
        start_char: usize,
        max_chars: usize,
    ) -> Result<crate::context::ContextMessageDetail, ContextServiceError> {
        crate::context::build_context_message_detail(crate::context::ContextMessageDetailInput {
            session_id,
            messages,
            context_view,
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

    pub fn preview_context_ranges(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        ranges: &[ContextMessageRangeSelection],
    ) -> Result<ContextRangeClosurePreview, ContextServiceError> {
        self.preview_context_ranges_with_active_profile(
            session_id,
            messages,
            context_view,
            expected_context_revision,
            expected_transcript_digest,
            None,
            ranges,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "range preview keeps exact session, transcript, context revision, active-profile, and selection identity"
    )]
    pub fn preview_context_ranges_with_active_profile(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        active_agent_profile_message_id: Option<&str>,
        ranges: &[ContextMessageRangeSelection],
    ) -> Result<ContextRangeClosurePreview, ContextServiceError> {
        if context_view.revision != expected_context_revision {
            return Err(ContextServiceError::Stale(format!(
                "context revision changed from {expected_context_revision} to {}",
                context_view.revision
            )));
        }
        let transcript_digest = authoritative_transcript_digest(messages);
        if transcript_digest != expected_transcript_digest {
            return Err(ContextServiceError::Stale(format!(
                "transcript digest changed from {expected_transcript_digest} to {transcript_digest}"
            )));
        }
        if ranges.is_empty() {
            return Err(ContextServiceError::InvalidSelection(
                "at least one summary range is required".to_string(),
            ));
        }

        let resolved = resolve_summary_ranges(messages, context_view, ranges)?;
        reject_active_agent_profile_ranges(
            messages,
            active_agent_profile_message_id,
            &resolved.closed_ranges,
        )?;
        let previews = resolved
            .closed_ranges
            .iter()
            .map(|closed| {
                let requested = resolved
                    .requested_ranges
                    .iter()
                    .find(|requested| {
                        requested.start == closed.requested_start
                            && requested.end == closed.requested_end
                    })
                    .ok_or_else(|| {
                        ContextServiceError::Runtime(format!(
                            "closed range {}..={} lost its requested stable-message identity",
                            closed.requested_start, closed.requested_end
                        ))
                    })?;
                let source_range = closed
                    .to_stored_range(messages)
                    .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
                let source_tokens = messages[closed.start..=closed.end]
                    .iter()
                    .map(|message| estimate_message_tokens(&message.to_message()))
                    .fold(0usize, usize::saturating_add);
                Ok(ContextClosedRangePreview {
                    requested: requested.selection.clone(),
                    source_range,
                    boundary_expansions: closed.expansions.clone(),
                    source_tokens,
                })
            })
            .collect::<Result<Vec<_>, ContextServiceError>>()?;
        Ok(ContextRangeClosurePreview {
            session_id: session_id.to_string(),
            context_revision: context_view.revision,
            transcript_digest,
            ranges: previews,
            shadowed_active_operations: resolved.shadowed_active_operations,
        })
    }

    pub fn preview_context_curator_plan(
        &self,
        agent: &mut Agent,
        processing: bool,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        request: ContextDraftRequest,
    ) -> Result<ContextCuratorPlanPreview, ContextServiceError> {
        let provider = agent.provider_handle();
        self.preview_context_curator_plan_for_session_with_active_profile(
            agent.session_id(),
            agent.messages(),
            agent.context_view_state(),
            processing,
            provider.as_ref(),
            &agent.context_route_identity(),
            &agent.model_routes(),
            expected_context_revision,
            expected_transcript_digest,
            request,
            agent.active_transition_message_id(),
            &crate::config::config().context.curator,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "exact curator preview retains authoritative session, route, transcript, and configured-default identity"
    )]
    pub fn preview_context_curator_plan_for_session(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        model_routes: &[ModelRoute],
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        request: ContextDraftRequest,
        configured_default: &crate::config::ContextCuratorConfig,
    ) -> Result<ContextCuratorPlanPreview, ContextServiceError> {
        self.preview_context_curator_plan_for_session_with_active_profile(
            session_id,
            messages,
            context_view,
            processing,
            provider,
            route,
            model_routes,
            expected_context_revision,
            expected_transcript_digest,
            request,
            None,
            configured_default,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "exact curator preview retains authoritative session, route, transcript, profile, and configured-default identity"
    )]
    pub fn preview_context_curator_plan_for_session_with_active_profile(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        processing: bool,
        provider: &dyn Provider,
        route: &str,
        model_routes: &[ModelRoute],
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        request: ContextDraftRequest,
        active_agent_profile_message_id: Option<&str>,
        configured_default: &crate::config::ContextCuratorConfig,
    ) -> Result<ContextCuratorPlanPreview, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        if context_view.revision != expected_context_revision {
            return Err(ContextServiceError::Stale(format!(
                "context revision changed from {expected_context_revision} to {}",
                context_view.revision
            )));
        }
        let transcript_digest = authoritative_transcript_digest(messages);
        if transcript_digest != expected_transcript_digest {
            return Err(ContextServiceError::Stale(format!(
                "transcript digest changed from {expected_transcript_digest} to {transcript_digest}"
            )));
        }
        if request.is_empty() {
            return Err(ContextServiceError::EmptyRequest);
        }
        let (effective_config, _) = effective_curator_config(configured_default, &request.curator);
        let now = Utc::now();
        let identity = ContextDraftIdentity {
            draft_id: format!("preview-{}", Uuid::new_v4()),
            session_id: session_id.to_string(),
            base_context_revision: context_view.revision,
            raw_message_count: messages.len(),
            transcript_digest,
            provider_name: provider.name().to_string(),
            model: provider.model(),
            route: route.to_string(),
            created_at: now,
            expires_at: now,
        };
        let capture = capture_context_draft_with_active_profile(
            messages,
            context_view,
            identity,
            request,
            active_agent_profile_message_id,
        )?;
        if capture.ranges.is_empty() && capture.tools.is_empty() {
            return Err(ContextServiceError::InvalidSelection(
                "the staged request contains no range-summary or tool-distillation curator task"
                    .to_string(),
            ));
        }
        let curator_route =
            resolve_context_curator_route(provider.fork(), model_routes, route, &effective_config)
                .map_err(|error| ContextServiceError::Curator(error.to_string()))?;
        self.preview_plan_from_capture(&curator_route, &capture)
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
        let (effective_curator_config, _) =
            effective_curator_config(curator_config, &request.curator);
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
        let capture = capture_context_draft_with_active_profile(
            guard.messages(),
            guard.context_view_state(),
            identity.clone(),
            request,
            guard.active_transition_message_id(),
        )?;
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
                    &effective_curator_config,
                )
                .map_err(|error| ContextServiceError::Curator(error.to_string()))?,
            )
        };
        let plan = route
            .as_ref()
            .map(|route| build_plan_for_capture(route, &capture, self.limits.curator, true))
            .transpose()?;
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
                .prepare_draft_task(agent, capture, route, plan, cancellation)
                .await;
        });
        Ok(draft_id)
    }

    pub fn prepare_draft_for_session(
        self: &Arc<Self>,
        input: ContextDraftRuntimeInput,
        request: ContextDraftRequest,
        processing: bool,
    ) -> Result<String, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        if request.is_empty() {
            return Err(ContextServiceError::EmptyRequest);
        }
        let configured_default = crate::config::config().context.curator.clone();
        let (effective_curator_config, _) =
            effective_curator_config(&configured_default, &request.curator);
        let draft_id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = created_at
            + chrono::Duration::from_std(self.limits.ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(30));
        let identity = ContextDraftIdentity {
            draft_id: draft_id.clone(),
            session_id: input.session_id,
            base_context_revision: input.context_view.revision,
            raw_message_count: input.messages.len(),
            transcript_digest: authoritative_transcript_digest(&input.messages),
            provider_name: input.provider.name().to_string(),
            model: input.provider.model(),
            route: input.route,
            created_at,
            expires_at,
        };
        let capture = capture_context_draft_with_active_profile(
            &input.messages,
            &input.context_view,
            identity.clone(),
            request,
            input.active_agent_profile_message_id.as_deref(),
        )?;
        let route = if capture.ranges.is_empty() && capture.tools.is_empty() {
            None
        } else {
            Some(
                resolve_context_curator_route(
                    input.provider.fork(),
                    &input.model_routes,
                    &identity.route,
                    &effective_curator_config,
                )
                .map_err(|error| ContextServiceError::Curator(error.to_string()))?,
            )
        };
        let plan = route
            .as_ref()
            .map(|route| build_plan_for_capture(route, &capture, self.limits.curator, true))
            .transpose()?;
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
                .prepare_draft_task_for_session(
                    capture,
                    route,
                    plan,
                    input.provider,
                    input.estimated_total_request_tokens_before,
                    cancellation,
                )
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

    /// Fail every non-applying draft captured for a session whose lifecycle identity changed.
    /// Applying drafts remain immutable while their atomic commit owns the session boundary.
    pub fn invalidate_session_drafts(&self, session_id: &str, reason: &str) -> usize {
        let reason = bounded_context_metadata(reason, 256);
        let mut store = self.lock_store();
        let invalidated = store.invalidate_session(session_id, &reason);
        store.enforce_total_bytes(self.limits.max_total_bytes);
        invalidated
    }

    pub fn preview_draft_selection(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
        draft_id: &str,
        selected_distillation_ids: Vec<String>,
    ) -> Result<ContextDraftSelectionPreview, ContextServiceError> {
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let draft = self.ready_draft_for_session(draft_id, agent.session_id())?;
        validate_capture_identity(&agent, &draft.identity)?;
        build_draft_selection_preview(
            agent.provider_handle().as_ref(),
            agent.messages(),
            agent.context_view_state(),
            agent.current_context_request_token_estimate(),
            &draft,
            selected_distillation_ids,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "local preview revalidates every independent provider-context identity dimension"
    )]
    pub fn preview_draft_selection_for_session(
        &self,
        session_id: &str,
        messages: &[StoredMessage],
        context_view: &StoredContextViewState,
        provider: &dyn Provider,
        route: &str,
        estimated_total_request_tokens_before: Option<usize>,
        draft_id: &str,
        selected_distillation_ids: Vec<String>,
    ) -> Result<ContextDraftSelectionPreview, ContextServiceError> {
        let draft = self.ready_draft_for_session(draft_id, session_id)?;
        validate_capture_identity_parts(
            session_id,
            messages,
            context_view,
            provider,
            route,
            &draft.identity,
        )?;
        build_draft_selection_preview(
            provider,
            messages,
            context_view,
            estimated_total_request_tokens_before,
            &draft,
            selected_distillation_ids,
        )
    }

    fn ready_draft_for_session(
        &self,
        draft_id: &str,
        expected_session_id: &str,
    ) -> Result<ContextDraft, ContextServiceError> {
        let mut store = self.lock_store();
        store.expire_entries(Utc::now());
        let entry = store
            .entries
            .get(draft_id)
            .ok_or_else(|| ContextServiceError::DraftNotFound(draft_id.to_string()))?;
        if entry.identity.session_id != expected_session_id {
            return Err(ContextServiceError::DraftNotFound(draft_id.to_string()));
        }
        match &entry.state {
            DraftEntryState::Ready(draft) => Ok(draft.clone()),
            DraftEntryState::Applied { .. } => Err(ContextServiceError::DraftAlreadyApplied(
                draft_id.to_string(),
            )),
            DraftEntryState::Canceled => {
                Err(ContextServiceError::DraftCanceled(draft_id.to_string()))
            }
            DraftEntryState::Expired => {
                Err(ContextServiceError::DraftExpired(draft_id.to_string()))
            }
            _ => Err(ContextServiceError::DraftNotReady(draft_id.to_string())),
        }
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

    fn preview_plan_from_capture(
        &self,
        route: &ContextCuratorRoute,
        capture: &CapturedContextDraft,
    ) -> Result<ContextCuratorPlanPreview, ContextServiceError> {
        build_plan_for_capture(route, capture, self.limits.curator, false).map(|plan| plan.preview)
    }

    async fn prepare_draft_task(
        self: Arc<Self>,
        agent: Arc<AsyncMutex<Agent>>,
        capture: CapturedContextDraft,
        route: Option<ContextCuratorRoute>,
        plan: Option<ContextCuratorPlan>,
        cancellation: CancellationToken,
    ) {
        let draft_id = capture.identity.draft_id.clone();
        self.update_progress(
            &draft_id,
            ContextDraftPhase::PreparingArtifacts,
            0,
            capture.ranges.len().saturating_add(capture.tools.len()),
        );
        let total_items = capture.ranges.len().saturating_add(capture.tools.len());
        let artifacts = match (route.as_ref(), plan.as_ref()) {
            (Some(route), Some(plan)) => {
                run_context_curator_plan(
                    route,
                    &capture.messages,
                    plan,
                    &cancellation,
                    self.limits.curator,
                    |completed, _| {
                        self.update_progress(
                            &draft_id,
                            ContextDraftPhase::PreparingArtifacts,
                            completed,
                            total_items,
                        );
                    },
                )
                .await
            }
            (None, None) => Ok(ContextCuratorArtifacts::default()),
            _ => Err(
                crate::context::curator::ContextCuratorError::InvalidResponse(
                    "curator route and atomic plan availability diverged".to_string(),
                ),
            ),
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
        let provider = guard.provider_handle();
        let estimated_total_request_tokens_before = guard.current_context_request_token_estimate();
        let draft = build_ready_draft(
            provider.as_ref(),
            estimated_total_request_tokens_before,
            capture,
            route,
            artifacts,
        );
        drop(guard);
        match draft {
            Ok(draft) => self.finish_ready(draft),
            Err(error) => self.finish_failed(&error.draft_id, error.error),
        }
    }

    async fn prepare_draft_task_for_session(
        self: Arc<Self>,
        capture: CapturedContextDraft,
        route: Option<ContextCuratorRoute>,
        plan: Option<ContextCuratorPlan>,
        provider: Arc<dyn Provider>,
        estimated_total_request_tokens_before: Option<usize>,
        cancellation: CancellationToken,
    ) {
        let draft_id = capture.identity.draft_id.clone();
        self.update_progress(
            &draft_id,
            ContextDraftPhase::PreparingArtifacts,
            0,
            capture.ranges.len().saturating_add(capture.tools.len()),
        );
        let total_items = capture.ranges.len().saturating_add(capture.tools.len());
        let artifacts = match (route.as_ref(), plan.as_ref()) {
            (Some(route), Some(plan)) => {
                run_context_curator_plan(
                    route,
                    &capture.messages,
                    plan,
                    &cancellation,
                    self.limits.curator,
                    |completed, _| {
                        self.update_progress(
                            &draft_id,
                            ContextDraftPhase::PreparingArtifacts,
                            completed,
                            total_items,
                        );
                    },
                )
                .await
            }
            (None, None) => Ok(ContextCuratorArtifacts::default()),
            _ => Err(
                crate::context::curator::ContextCuratorError::InvalidResponse(
                    "curator route and atomic plan availability diverged".to_string(),
                ),
            ),
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
        let draft = build_ready_draft(
            provider.as_ref(),
            estimated_total_request_tokens_before,
            capture,
            route,
            artifacts,
        );
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
            DraftEntryState::Failed(_) | DraftEntryState::Canceled | DraftEntryState::Expired
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

fn build_plan_for_capture(
    route: &ContextCuratorRoute,
    capture: &CapturedContextDraft,
    limits: ContextCuratorLimits,
    require_reviewed_fingerprint: bool,
) -> Result<ContextCuratorPlan, ContextServiceError> {
    let plan = build_context_curator_plan(
        route,
        ContextCuratorPlanInput {
            session_id: &capture.identity.session_id,
            context_revision: capture.identity.base_context_revision,
            transcript_digest: capture.identity.transcript_digest,
            messages: &capture.messages,
            ranges: &capture.ranges,
            tools: &capture.tools,
            active_summary_texts: &capture.active_summary_texts,
            transaction_instructions: &capture.transaction_instructions,
            selection_source: capture.curator_selection_source,
        },
        limits,
    )
    .map_err(|error| ContextServiceError::Curator(error.to_string()))?;
    if require_reviewed_fingerprint
        && matches!(
            &capture.authorization,
            StoredContextAuthorization::Manual { .. }
        )
    {
        let Some(expected) = capture.expected_plan_fingerprint.as_deref() else {
            return Err(ContextServiceError::InvalidSelection(
                "manual curator preparation requires an exact reviewed plan fingerprint"
                    .to_string(),
            ));
        };
        if expected != plan.preview.fingerprint {
            return Err(ContextServiceError::Stale(
                "the effective curator route, prompts, limits, or source scope changed after review; prepare a fresh exact preview"
                    .to_string(),
            ));
        }
    }
    Ok(plan)
}

#[derive(Clone, Serialize)]
pub(crate) struct CapturedRange {
    request_id: String,
    source_range: StoredMessageRange,
    boundary_expansions: Vec<jcode_session_types::StoredRangeBoundaryExpansion>,
    file_evidence: StoredContextFileEvidence,
    source_token_estimate: usize,
}

struct ResolvedSummaryRanges {
    requested_ranges: Vec<ResolvedRequestedSummaryRange>,
    closed_ranges: Vec<jcode_context_core::ClosedMessageRange>,
    shadowed_active_operations: Vec<String>,
}

struct ResolvedRequestedSummaryRange {
    selection: ContextMessageRangeSelection,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReasoningFilterStats {
    selected_summary_targets: usize,
    active_summary_targets: usize,
    already_suppressed_targets: usize,
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
    curator_selection_source: StoredContextCuratorSelectionSource,
    transaction_instructions: String,
    expected_plan_fingerprint: Option<String>,
    notices: Vec<String>,
}

#[cfg(test)]
fn capture_context_draft(
    messages: &[StoredMessage],
    context_view: &StoredContextViewState,
    identity: ContextDraftIdentity,
    request: ContextDraftRequest,
) -> Result<CapturedContextDraft, ContextServiceError> {
    capture_context_draft_with_active_profile(messages, context_view, identity, request, None)
}

fn capture_context_draft_with_active_profile(
    messages: &[StoredMessage],
    context_view: &StoredContextViewState,
    identity: ContextDraftIdentity,
    request: ContextDraftRequest,
    active_agent_profile_message_id: Option<&str>,
) -> Result<CapturedContextDraft, ContextServiceError> {
    validate_context_curator_run_config(&request.curator)?;
    let messages = messages.to_vec();
    let base_context_view = context_view.clone();
    validate_context_state(&base_context_view)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    project_context(&messages, &base_context_view)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let message_indices = unique_message_indices(&messages)?;
    let range_instructions =
        resolve_range_instructions(&request.summary_ranges, &request.curator.range_instructions)?;
    let resolved = resolve_summary_ranges(&messages, &base_context_view, &request.summary_ranges)?;
    reject_active_agent_profile_ranges(
        &messages,
        active_agent_profile_message_id,
        &resolved.closed_ranges,
    )?;
    let requested_ranges = resolved.requested_ranges;
    let closed_ranges = resolved.closed_ranges;
    let shadowed = resolved.shadowed_active_operations;
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
        let evidence = extract_context_file_evidence(&messages, &source_range)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        let source_token_estimate = messages[closed.start..=closed.end]
            .iter()
            .map(|message| estimate_message_tokens(&message.to_message()))
            .fold(0usize, usize::saturating_add);
        let request_id = format!("range-{}", index + 1);
        let requested = requested_ranges
            .iter()
            .find(|requested| {
                requested.start == closed.requested_start && requested.end == closed.requested_end
            })
            .ok_or_else(|| {
                ContextServiceError::Runtime(format!(
                    "closed range {}..={} lost its requested stable-message identity",
                    closed.requested_start, closed.requested_end
                ))
            })?;
        let additional_instructions = range_instructions
            .get(&canonical_range_key(&requested.selection))
            .cloned()
            .unwrap_or_default();
        ranges.push(ContextCuratorRangeWork {
            request_id: request_id.clone(),
            source_range: source_range.clone(),
            file_evidence: evidence.clone(),
            additional_instructions,
        });
        range_metadata.push(CapturedRange {
            request_id,
            source_range,
            boundary_expansions: closed.expansions.clone(),
            file_evidence: evidence,
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
                    let first = *message_indices
                        .get(&range.start_message_id)
                        .ok_or_else(|| {
                            ContextServiceError::InvalidSelection(format!(
                                "reasoning range start message not found: {}",
                                range.start_message_id
                            ))
                        })?;
                    let second = *message_indices.get(&range.end_message_id).ok_or_else(|| {
                        ContextServiceError::InvalidSelection(format!(
                            "reasoning range end message not found: {}",
                            range.end_message_id
                        ))
                    })?;
                    let (start, end) = ordered_interval(first, second);
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
    let (reasoning, reasoning_filter_stats) = match reasoning {
        Some(suppression) => {
            let (suppression, stats) = filter_effective_reasoning(
                &messages,
                &base_context_view,
                suppression,
                &summary_intervals,
            )?;
            (Some(suppression), stats)
        }
        None => (None, ReasoningFilterStats::default()),
    };

    let mut notices = shadowed
        .into_iter()
        .map(|operation| format!("Selected summaries shadow active operation {operation}."))
        .collect::<Vec<_>>();
    if reasoning_filter_stats.selected_summary_targets > 0 {
        notices.push(format!(
            "Selected summaries already replace {} staged replayed-reasoning block target(s); those targets were omitted.",
            reasoning_filter_stats.selected_summary_targets
        ));
    }
    if reasoning_filter_stats.active_summary_targets > 0 {
        notices.push(format!(
            "Active range summaries already remove {} requested replayed-reasoning block target(s) from provider context; those targets are already satisfied.",
            reasoning_filter_stats.active_summary_targets
        ));
    }
    if reasoning_filter_stats.already_suppressed_targets > 0 {
        notices.push(format!(
            "Active context transactions already suppressed {} requested replayed-reasoning block target(s); only newly eligible exact targets remain staged.",
            reasoning_filter_stats.already_suppressed_targets
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
        curator_selection_source: if request.curator.selection.is_some() {
            StoredContextCuratorSelectionSource::PerRunOverride
        } else {
            StoredContextCuratorSelectionSource::ConfiguredDefault
        },
        transaction_instructions: request.curator.transaction_instructions,
        expected_plan_fingerprint: request.curator.expected_plan_fingerprint,
        notices,
    })
}

fn reject_active_agent_profile_ranges(
    messages: &[StoredMessage],
    active_agent_profile_message_id: Option<&str>,
    ranges: &[jcode_context_core::ClosedMessageRange],
) -> Result<(), ContextServiceError> {
    let Some(message_id) = active_agent_profile_message_id else {
        return Ok(());
    };
    let message_index = messages
        .iter()
        .position(|message| message.id == message_id)
        .ok_or_else(|| {
            ContextServiceError::Stale(format!(
                "active agent profile message {message_id} is missing from authoritative history"
            ))
        })?;
    if ranges
        .iter()
        .any(|range| range.start <= message_index && message_index <= range.end)
    {
        return Err(ContextServiceError::Conflict(format!(
            "selected range includes active agent profile message {message_id}; switch or explicitly replace the system prompt before transforming it"
        )));
    }
    Ok(())
}

fn canonical_range_key(selection: &ContextMessageRangeSelection) -> (String, String) {
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

fn resolve_range_instructions(
    selected_ranges: &[ContextMessageRangeSelection],
    instructions: &[crate::protocol::ContextCuratorRangeInstructions],
) -> Result<BTreeMap<(String, String), String>, ContextServiceError> {
    let selected = selected_ranges
        .iter()
        .map(canonical_range_key)
        .collect::<BTreeSet<_>>();
    let mut resolved = BTreeMap::new();
    for item in instructions {
        let key = canonical_range_key(&item.range);
        if !selected.contains(&key) {
            return Err(ContextServiceError::InvalidSelection(format!(
                "range-specific curator instructions reference an unstaged range {}..{}",
                item.range.start_message_id, item.range.end_message_id
            )));
        }
        if resolved.insert(key, item.instructions.clone()).is_some() {
            return Err(ContextServiceError::InvalidSelection(format!(
                "range-specific curator instructions were supplied more than once for {}..{}",
                item.range.start_message_id, item.range.end_message_id
            )));
        }
    }
    Ok(resolved)
}

fn resolve_summary_ranges(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    ranges: &[ContextMessageRangeSelection],
) -> Result<ResolvedSummaryRanges, ContextServiceError> {
    let message_indices = unique_message_indices(messages)?;
    let mut requested_ranges = ranges
        .iter()
        .map(|range| {
            let first = *message_indices
                .get(&range.start_message_id)
                .ok_or_else(|| {
                    ContextServiceError::InvalidSelection(format!(
                        "range start message not found: {}",
                        range.start_message_id
                    ))
                })?;
            let second = *message_indices.get(&range.end_message_id).ok_or_else(|| {
                ContextServiceError::InvalidSelection(format!(
                    "range end message not found: {}",
                    range.end_message_id
                ))
            })?;
            let (start, end) = ordered_interval(first, second);
            Ok(ResolvedRequestedSummaryRange {
                selection: ContextMessageRangeSelection {
                    start_message_id: messages[start].id.clone(),
                    end_message_id: messages[end].id.clone(),
                },
                start,
                end,
            })
        })
        .collect::<Result<Vec<_>, ContextServiceError>>()?;
    requested_ranges.sort_by_key(|range| (range.start, range.end));
    let closed_ranges = if requested_ranges.is_empty() {
        Vec::new()
    } else {
        let requested_indices = requested_ranges
            .iter()
            .map(|range| (range.start, range.end))
            .collect::<Vec<_>>();
        close_message_ranges(messages, state, &requested_indices)
            .map_err(|error| ContextServiceError::Conflict(error.to_string()))?
    };
    reject_active_summary_overlap(messages, state, &closed_ranges)?;
    let shadowed_active_operations =
        active_block_operations_shadowed(messages, state, &closed_ranges)?;
    Ok(ResolvedSummaryRanges {
        requested_ranges,
        closed_ranges,
        shadowed_active_operations,
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

fn ordered_interval(first: usize, second: usize) -> (usize, usize) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn filter_effective_reasoning(
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    mut suppression: StoredReasoningSuppression,
    summary_intervals: &[(usize, usize)],
) -> Result<(StoredReasoningSuppression, ReasoningFilterStats), ContextServiceError> {
    let target_index = ContextTargetIndex::new(messages);
    let mut active_summary_intervals = Vec::new();
    let mut active_suppression_targets = BTreeSet::new();
    for transaction in state.active_transactions() {
        for operation in &transaction.operations {
            match operation {
                StoredContextOperation::RangeSummary(summary) => {
                    active_summary_intervals.push(
                        target_index
                            .resolve_message_range(&summary.source_range)
                            .map_err(|error| ContextServiceError::Stale(error.to_string()))?,
                    );
                }
                StoredContextOperation::ReasoningSuppression(active) => {
                    for target in &active.targets {
                        let resolved = target_index
                            .resolve_content_target(target)
                            .map_err(|error| ContextServiceError::Stale(error.to_string()))?;
                        active_suppression_targets
                            .insert((resolved.message_index, resolved.block_index));
                    }
                }
                StoredContextOperation::ToolResultDistillation(_) => {}
            }
        }
    }

    let mut stats = ReasoningFilterStats::default();
    let mut retained = Vec::new();
    let mut turns = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut tokens = 0usize;
    for target in suppression.targets {
        let resolved = target_index
            .resolve_content_target(&target)
            .map_err(|error| ContextServiceError::InvalidSelection(error.to_string()))?;
        if interval_contains(summary_intervals, resolved.message_index) {
            stats.selected_summary_targets = stats.selected_summary_targets.saturating_add(1);
            continue;
        }
        if interval_contains(&active_summary_intervals, resolved.message_index) {
            stats.active_summary_targets = stats.active_summary_targets.saturating_add(1);
            continue;
        }
        if active_suppression_targets.contains(&(resolved.message_index, resolved.block_index)) {
            stats.already_suppressed_targets = stats.already_suppressed_targets.saturating_add(1);
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
    Ok((suppression, stats))
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
    provider: &dyn Provider,
    estimated_total_request_tokens_before: Option<usize>,
    capture: CapturedContextDraft,
    route: Option<ContextCuratorRoute>,
    artifacts: ContextCuratorArtifacts,
) -> Result<ContextDraft, DraftBuildFailure> {
    let draft_id = capture.identity.draft_id.clone();
    build_ready_draft_inner(
        provider,
        estimated_total_request_tokens_before,
        capture,
        route,
        artifacts,
    )
    .map_err(|error| DraftBuildFailure { draft_id, error })
}

fn build_ready_draft_inner(
    provider: &dyn Provider,
    estimated_total_request_tokens_before: Option<usize>,
    capture: CapturedContextDraft,
    route: Option<ContextCuratorRoute>,
    artifacts: ContextCuratorArtifacts,
) -> Result<ContextDraft, ContextServiceError> {
    let curator_route = match route.as_ref() {
        Some(route) => Some(route),
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
        let work = capture
            .ranges
            .iter()
            .find(|range| range.request_id == metadata.request_id)
            .ok_or_else(|| {
                ContextServiceError::Curator(format!(
                    "missing captured range work {}",
                    metadata.request_id
                ))
            })?;
        let artifact = artifacts
            .range_summaries
            .get(&metadata.request_id)
            .ok_or_else(|| {
                ContextServiceError::Curator(format!(
                    "missing generated range artifact {}",
                    metadata.request_id
                ))
            })?;
        required_operations.push(StoredContextOperation::RangeSummary(StoredRangeSummary {
            source_range: metadata.source_range.clone(),
            summary_text: artifact.summary.clone(),
            file_change_digest: artifact.file_change_digest.clone(),
            changed_files: Vec::new(),
            change_evidence_complete: false,
            file_evidence: Some(metadata.file_evidence.clone()),
            boundary_expansions: metadata.boundary_expansions.clone(),
            generator: Some(
                curator_route
                    .ok_or_else(|| {
                        ContextServiceError::Curator(format!(
                            "range artifact {} has no independent curator route identity",
                            metadata.request_id
                        ))
                    })?
                    .generator(
                        StoredContextCuratorRole::RangeSummarizer,
                        CONTEXT_RANGE_SUMMARIZER_PROMPT_VERSION,
                        capture.curator_selection_source,
                        &capture.transaction_instructions,
                        &work.additional_instructions,
                    ),
            ),
            source_token_estimate: metadata.source_token_estimate,
            replacement_token_estimate: 0,
            warnings: artifact.warnings.clone(),
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
                    generator: curator_route
                        .ok_or_else(|| {
                            ContextServiceError::Curator(format!(
                                "tool artifact {request_id} has no independent curator route identity"
                            ))
                        })?
                        .generator(
                            StoredContextCuratorRole::ToolResultDistiller,
                            CONTEXT_TOOL_DISTILLER_PROMPT_VERSION,
                            capture.curator_selection_source,
                            &capture.transaction_instructions,
                            "",
                        ),
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
    let proposed_revision = proposed_revision(&capture.base_context_view, &operations)?;
    if !operations.is_empty() {
        fill_range_replacement_estimates(
            &capture.messages,
            &capture.base_context_view,
            &capture.identity.draft_id,
            proposed_revision,
            capture.authorization.clone(),
            &mut operations,
        )?;
    }
    copy_filled_range_estimates(&operations, &mut required_operations);
    let pricing = crate::provider::pricing::context_pricing_snapshot(
        &provider.model(),
        &provider.display_name(),
        &capture.identity.route,
        jcode_session_types::StoredContextCacheWarmth::Unknown,
    );
    let mut notices = capture.notices;
    if operations.is_empty() {
        notices.push(
            "No provider-context change remains after exact active-operation and proposal filtering. This review is a no-op and cannot be applied."
                .to_string(),
        );
    }
    let preview = build_preview(ContextDraftPreviewInput {
        provider,
        messages: &capture.messages,
        base_state: &capture.base_context_view,
        transaction_id: &capture.identity.draft_id,
        proposed_revision,
        authorization: capture.authorization.clone(),
        operations: &operations,
        pricing: Some(&pricing),
        estimated_total_request_tokens_before,
        notices,
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
        curator_usage: artifacts.usage,
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
    let proposed_state = proposed_state_for_operations(
        base_state,
        transaction_id,
        proposed_revision,
        authorization,
        operations,
    )?;
    validate_context_state(&proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let after = project_context(messages, &proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let validation_operations = projection_validation_operations(&proposed_state);
    let validation = validation_for_preview(
        provider,
        &after.messages,
        &validation_operations,
        !operations.is_empty(),
    )?;
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
                    file_evidence: summary.file_evidence.clone().map(Box::new),
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

fn build_draft_selection_preview(
    provider: &dyn crate::provider::Provider,
    messages: &[StoredMessage],
    base_state: &StoredContextViewState,
    estimated_total_request_tokens_before: Option<usize>,
    draft: &ContextDraft,
    selected_distillation_ids: Vec<String>,
) -> Result<ContextDraftSelectionPreview, ContextServiceError> {
    let selected_operations =
        selected_distillation_operations(draft, Some(&selected_distillation_ids))?;
    let mut operations = draft.required_operations.clone();
    operations.extend(selected_operations);
    let proposed_revision = proposed_revision(base_state, &operations)?;
    let before = project_context(messages, base_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let proposed_state = proposed_state_for_operations(
        base_state,
        &draft.identity.draft_id,
        proposed_revision,
        draft.authorization.clone(),
        &operations,
    )?;
    validate_context_state(&proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let after = project_context(messages, &proposed_state)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let validation_operations = projection_validation_operations(&proposed_state);
    let validation = validation_for_preview(
        provider,
        &after.messages,
        &validation_operations,
        !operations.is_empty(),
    )?;
    let analysis = analyze_cache_prefix(&before.messages, &after.messages);
    let estimated_total_request_tokens_after = estimated_total_request_tokens_before
        .and_then(|total| total.checked_sub(analysis.old_total_tokens))
        .map(|non_message_tokens| non_message_tokens.saturating_add(analysis.new_total_tokens));
    let pricing = crate::provider::pricing::context_pricing_snapshot(
        &provider.model(),
        &provider.display_name(),
        &draft.identity.route,
        jcode_session_types::StoredContextCacheWarmth::Unknown,
    );
    let economics = calculate_context_economics(ContextEconomicsInput {
        analysis: &analysis,
        estimated_total_request_tokens_before,
        estimated_total_request_tokens_after,
        context_window: Some(provider.context_window()),
        safe_input_budget: None,
        pricing: Some(&pricing),
        resulting_suffix_cacheable: after.diagnostics.projected_provider_token_estimate >= 1_024,
    });
    let mut preview = draft.preview.clone();
    preview.current_context_revision = base_state.revision;
    preview.proposed_context_revision = proposed_revision;
    preview.economics = economics;
    preview.formatter_placeholder_count = validation.formatter_placeholder_count;
    preview.validation = validation;
    Ok(ContextDraftSelectionPreview {
        draft_id: draft.identity.draft_id.clone(),
        selected_distillation_ids,
        preview,
    })
}

fn proposed_revision(
    base_state: &StoredContextViewState,
    operations: &[StoredContextOperation],
) -> Result<u64, ContextServiceError> {
    if operations.is_empty() {
        Ok(base_state.revision)
    } else {
        base_state
            .revision
            .checked_add(1)
            .ok_or(ContextServiceError::RevisionOverflow)
    }
}

fn validation_for_preview(
    provider: &dyn Provider,
    messages: &[crate::message::Message],
    operations: &[ContextProjectionValidationOperation],
    changes_context: bool,
) -> Result<ContextProjectionValidationReport, ContextServiceError> {
    if !changes_context {
        return Ok(provider.validate_projected_context(messages, operations));
    }
    require_supported_projected_messages(provider, messages, operations)
        .map_err(|error| ContextServiceError::ProviderValidation(error.to_string()))
}

fn proposed_state_for_operations(
    base_state: &StoredContextViewState,
    transaction_id: &str,
    proposed_revision: u64,
    authorization: StoredContextAuthorization,
    operations: &[StoredContextOperation],
) -> Result<StoredContextViewState, ContextServiceError> {
    if operations.is_empty() {
        if proposed_revision != base_state.revision {
            return Err(ContextServiceError::Runtime(format!(
                "no-change preview proposed revision {proposed_revision} instead of current revision {}",
                base_state.revision
            )));
        }
        return Ok(base_state.clone());
    }
    if proposed_revision <= base_state.revision {
        return Err(ContextServiceError::Runtime(format!(
            "context-changing preview proposed non-advancing revision {proposed_revision} from {}",
            base_state.revision
        )));
    }
    Ok(state_with_transaction(
        base_state,
        transaction_id,
        proposed_revision,
        authorization,
        operations.to_vec(),
        None,
        Vec::new(),
    ))
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
    state_with_transaction_and_audit(
        base_state,
        transaction_id,
        revision,
        authorization,
        operations,
        economics,
        curator_usage,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "transaction state owns complete persisted provenance"
)]
pub(crate) fn state_with_transaction_and_audit(
    base_state: &StoredContextViewState,
    transaction_id: &str,
    revision: u64,
    authorization: StoredContextAuthorization,
    operations: Vec<StoredContextOperation>,
    economics: Option<StoredContextEconomics>,
    curator_usage: Vec<StoredContextCuratorUsage>,
    emergency_audit: Option<jcode_session_types::StoredContextEmergencyAudit>,
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
        emergency_audit,
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
    let provider = agent.provider_handle();
    validate_capture_identity_parts(
        agent.session_id(),
        agent.messages(),
        agent.context_view_state(),
        provider.as_ref(),
        &agent.context_route_identity(),
        identity,
    )
}

pub(crate) fn validate_capture_identity_parts(
    session_id: &str,
    messages: &[StoredMessage],
    context_view: &StoredContextViewState,
    provider: &dyn Provider,
    route: &str,
    identity: &ContextDraftIdentity,
) -> Result<(), ContextServiceError> {
    if session_id != identity.session_id {
        return Err(ContextServiceError::Stale("session ID changed".to_string()));
    }
    if context_view.revision != identity.base_context_revision {
        return Err(ContextServiceError::Stale(format!(
            "context revision changed from {} to {}",
            identity.base_context_revision, context_view.revision
        )));
    }
    if messages.len() != identity.raw_message_count {
        return Err(ContextServiceError::Stale(format!(
            "raw message count changed from {} to {}",
            identity.raw_message_count,
            messages.len()
        )));
    }
    if authoritative_transcript_digest(messages) != identity.transcript_digest {
        return Err(ContextServiceError::Stale(
            "authoritative transcript digest changed".to_string(),
        ));
    }
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
    if route != identity.route {
        return Err(ContextServiceError::Stale(format!(
            "route changed from {} to {}",
            identity.route, route
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

    fn invalidate_session(&mut self, session_id: &str, reason: &str) -> usize {
        let mut invalidated = 0usize;
        for entry in self.entries.values_mut() {
            if entry.identity.session_id != session_id {
                continue;
            }
            match entry.state {
                DraftEntryState::Preparing => {
                    entry.cancellation.cancel();
                    entry.state =
                        DraftEntryState::Failed(ContextServiceError::Stale(reason.to_string()));
                    entry.notify.notify_waiters();
                    invalidated += 1;
                }
                DraftEntryState::Ready(_) => {
                    entry.state =
                        DraftEntryState::Failed(ContextServiceError::Stale(reason.to_string()));
                    entry.refresh_terminal_reservation();
                    entry.notify.notify_waiters();
                    invalidated += 1;
                }
                DraftEntryState::Applying(_)
                | DraftEntryState::Applied { .. }
                | DraftEntryState::Failed(_)
                | DraftEntryState::Canceled
                | DraftEntryState::Expired => {}
            }
        }
        invalidated
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
    fn lifecycle_invalidation_is_session_scoped_terminal_and_never_mutates_applying_drafts() {
        let expires_at = Utc::now() + chrono::Duration::minutes(1);
        let mut store = ContextDraftStore::default();
        let mut preparing = entry(
            "preparing",
            DraftEntryState::Preparing,
            expires_at,
            4_096,
            true,
        );
        preparing.identity.session_id = "discarded-session".to_string();
        let preparing_token = preparing.cancellation.clone();
        let mut ready = entry(
            "ready",
            DraftEntryState::Ready(draft("ready", expires_at)),
            expires_at,
            2_048,
            false,
        );
        ready.identity.session_id = "discarded-session".to_string();
        let mut applying = entry(
            "applying",
            DraftEntryState::Applying(draft("applying", expires_at)),
            expires_at,
            2_048,
            false,
        );
        applying.identity.session_id = "discarded-session".to_string();
        let mut other = entry("other", DraftEntryState::Preparing, expires_at, 1_024, true);
        other.identity.session_id = "other-session".to_string();
        store.entries.insert("preparing".to_string(), preparing);
        store.entries.insert("ready".to_string(), ready);
        store.entries.insert("applying".to_string(), applying);
        store.entries.insert("other".to_string(), other);

        assert_eq!(
            store.invalidate_session("discarded-session", "session lifecycle changed"),
            2
        );

        assert!(preparing_token.is_cancelled());
        for id in ["preparing", "ready"] {
            assert!(matches!(
                store.entries[id].state,
                DraftEntryState::Failed(ContextServiceError::Stale(_))
            ));
        }
        assert!(matches!(
            store.entries["applying"].state,
            DraftEntryState::Applying(_)
        ));
        assert!(matches!(
            store.entries["other"].state,
            DraftEntryState::Preparing
        ));
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
        ContextRequestBuilderValidation, EventStream, Provider, RouteSelection,
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
        model: Arc<Mutex<String>>,
        model_routes: Arc<Mutex<Vec<ModelRoute>>>,
        effort: Arc<Mutex<Option<String>>>,
        empty_identity: Arc<AtomicBool>,
    }

    impl DraftProvider {
        fn new() -> Self {
            Self {
                state: Arc::new(DraftProviderState::new()),
                instance: ProviderInstance::Live,
                model: Arc::new(Mutex::new("draft-model".to_string())),
                model_routes: Arc::new(Mutex::new(Vec::new())),
                effort: Arc::new(Mutex::new(None)),
                empty_identity: Arc::new(AtomicBool::new(false)),
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
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
        }

        fn set_model_routes(&self, routes: Vec<ModelRoute>) {
            *self
                .model_routes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = routes;
        }

        fn set_empty_identity(&self, empty: bool) {
            self.empty_identity.store(empty, Ordering::SeqCst);
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
                    let response = if system.starts_with(RANGE_SUMMARIZER_BASE_PROMPT) {
                        serde_json::to_string(&serde_json::json!({
                            "summary": "All operationally relevant range facts are preserved.",
                            "file_change_digest": "No structured file changes were observed.",
                            "warnings": []
                        }))?
                    } else {
                        serde_json::to_string(&serde_json::json!({
                            "eligible": true,
                            "replacement": DISTILLED_RESULT,
                            "preservation_rationale": "The exact success state and absence of file changes are preserved.",
                            "ineligible_reason": null,
                            "uncertainties": []
                        }))?
                    };
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
            if self.empty_identity.load(Ordering::SeqCst) {
                ""
            } else if self.state.changed_name.load(Ordering::SeqCst) {
                "draft-provider-changed"
            } else {
                "draft-provider"
            }
        }

        fn display_name(&self) -> String {
            "Draft Provider".to_string()
        }

        fn model(&self) -> String {
            if self.empty_identity.load(Ordering::SeqCst) {
                return String::new();
            }
            self.model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn set_model(&self, model: &str) -> Result<()> {
            self.set_test_model(model);
            Ok(())
        }

        fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
            self.set_test_model(&selection.model);
            Ok(())
        }

        fn model_routes(&self) -> Vec<ModelRoute> {
            self.model_routes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
            *self
                .effort
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(effort.to_string());
            Ok(())
        }

        fn reasoning_effort(&self) -> Option<String> {
            self.effort
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
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
                model: Arc::new(Mutex::new(self.model())),
                model_routes: Arc::new(Mutex::new(self.model_routes())),
                effort: Arc::new(Mutex::new(
                    self.effort
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone(),
                )),
                empty_identity: Arc::new(AtomicBool::new(
                    self.empty_identity.load(Ordering::SeqCst),
                )),
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
            curator: Default::default(),
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
        let mut request = request();
        let fingerprint = {
            let mut guard = agent
                .try_lock()
                .map_err(|_| ContextServiceError::SessionBusy)?;
            let revision = guard.context_view_state().revision;
            let digest = authoritative_transcript_digest(guard.messages());
            service
                .preview_context_curator_plan(&mut guard, false, revision, digest, request.clone())?
                .fingerprint
        };
        request.curator.expected_plan_fingerprint = Some(fingerprint);
        service.prepare_draft_with_curator_config(
            agent,
            request,
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

    fn curator_model_route(model: &str, provider: &str, api_method: &str) -> ModelRoute {
        ModelRoute {
            model: model.to_string(),
            provider: provider.to_string(),
            api_method: api_method.to_string(),
            available: true,
            detail: "curator preview test route".to_string(),
            cheapness: None,
        }
    }

    #[test]
    fn context_editor_snapshot_resolves_default_and_explicit_curator_routes_independently() {
        let service = ContextTransactionService::new();
        let provider = DraftProvider::new();
        let session = test_session();

        let default_snapshot = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                Some(12_345),
                &crate::config::ContextCuratorConfig::default(),
            )
            .expect("default curator preview");
        let default_route = default_snapshot
            .curator_route
            .expect("default independent fork route");
        assert_eq!(default_route.provider_name, "draft-provider");
        assert_eq!(default_route.provider_display_name, "Draft Provider");
        assert_eq!(default_route.model, "draft-model");
        assert_eq!(default_route.route, "active-route");
        assert_eq!(default_route.effort, None);
        assert_eq!(default_snapshot.projected_request_tokens, 12_345);
        assert_eq!(default_snapshot.curator_unavailable_reason, None);

        provider.set_model_routes(vec![curator_model_route(
            "selected-curator-model",
            "curator-upstream",
            "curator-api",
        )]);
        let live_model_before = provider.model();
        let explicit_snapshot = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                None,
                &crate::config::ContextCuratorConfig {
                    provider: Some("curator-upstream".to_string()),
                    route: None,
                    model: Some("selected-curator-model".to_string()),
                    effort: Some("high".to_string()),
                },
            )
            .expect("explicit curator preview");
        let explicit_route = explicit_snapshot
            .curator_route
            .expect("explicit independent route");
        assert_eq!(explicit_route.provider_name, "draft-provider");
        assert_eq!(explicit_route.model, "selected-curator-model");
        assert_eq!(explicit_route.route, "curator-api");
        assert_eq!(explicit_route.effort.as_deref(), Some("high"));
        assert_eq!(explicit_snapshot.curator_unavailable_reason, None);
        assert_eq!(provider.model(), live_model_before);
        assert_eq!(
            provider
                .effort
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_deref(),
            None,
            "curator selection must not mutate the live provider"
        );
    }

    #[test]
    fn context_editor_snapshot_reports_bounded_unavailable_ambiguous_and_unstable_routes() {
        let service = ContextTransactionService::new();
        let provider = DraftProvider::new();
        let session = test_session();

        let unavailable = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                None,
                &crate::config::ContextCuratorConfig {
                    provider: Some("missing-provider".to_string()),
                    route: None,
                    model: Some("missing-model".to_string()),
                    effort: None,
                },
            )
            .expect("unavailable route remains a usable snapshot");
        assert_eq!(unavailable.curator_route, None);
        assert!(
            unavailable
                .curator_unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no available route matches"))
        );

        provider.set_model_routes(vec![
            curator_model_route("same-model", "same-provider", "route-a"),
            curator_model_route("same-model", "same-provider", "route-b"),
        ]);
        let ambiguous = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                None,
                &crate::config::ContextCuratorConfig {
                    provider: Some("same-provider".to_string()),
                    route: None,
                    model: Some("same-model".to_string()),
                    effort: None,
                },
            )
            .expect("ambiguous route remains a usable snapshot");
        assert_eq!(ambiguous.curator_route, None);
        assert!(
            ambiguous
                .curator_unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("ambiguous"))
        );

        provider.set_empty_identity(true);
        let unstable = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                None,
                &crate::config::ContextCuratorConfig::default(),
            )
            .expect("unstable route identity remains a usable snapshot");
        assert_eq!(unstable.curator_route, None);
        assert!(
            unstable
                .curator_unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("stable provider and model identity"))
        );

        provider.set_empty_identity(false);
        provider.set_model_routes(Vec::new());
        let long_selector = "🙂".repeat(CONTEXT_IDENTIFIER_MAX_CHARS * 2);
        let bounded = service
            .context_editor_snapshot_for_session_with_curator_config(
                &session.id,
                &session.messages,
                &session.context_view,
                false,
                &provider,
                "active-route",
                None,
                &crate::config::ContextCuratorConfig {
                    provider: Some(long_selector),
                    route: None,
                    model: Some("missing-model".to_string()),
                    effort: None,
                },
            )
            .expect("long resolver error remains bounded");
        let reason = bounded
            .curator_unavailable_reason
            .expect("bounded unavailable reason");
        assert!(reason.chars().count() <= CONTEXT_IDENTIFIER_MAX_CHARS);
        assert!(reason.contains("exceeding the 512-character bound"));
        assert!(!reason.contains(RAW_RESULT_SENTINEL));
    }

    #[tokio::test]
    async fn manual_curator_generation_requires_the_exact_reviewed_plan() {
        let provider = DraftProvider::new();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());

        let missing = service
            .prepare_draft_with_curator_config(
                Arc::clone(&agent),
                request(),
                false,
                &crate::config::ContextCuratorConfig::default(),
            )
            .expect_err("unreviewed manual generation must fail");
        assert!(matches!(missing, ContextServiceError::InvalidSelection(_)));

        let mut reviewed = request();
        let fingerprint = {
            let guard = agent.lock().await;
            let provider_handle = guard.provider_handle();
            let route = guard.context_route_identity();
            let routes = guard.model_routes();
            service
                .preview_context_curator_plan_for_session(
                    guard.session_id(),
                    guard.messages(),
                    guard.context_view_state(),
                    false,
                    provider_handle.as_ref(),
                    &route,
                    &routes,
                    guard.context_view_state().revision,
                    authoritative_transcript_digest(guard.messages()),
                    reviewed.clone(),
                    &crate::config::ContextCuratorConfig::default(),
                )
                .expect("exact manual preview")
                .fingerprint
        };
        reviewed.curator.expected_plan_fingerprint = Some(fingerprint);
        let stale = service
            .prepare_draft_with_curator_config(
                Arc::clone(&agent),
                reviewed,
                false,
                &crate::config::ContextCuratorConfig {
                    model: Some("changed-curator-model".to_string()),
                    ..crate::config::ContextCuratorConfig::default()
                },
            )
            .expect_err("changed effective curator plan must fail");
        assert!(matches!(stale, ContextServiceError::Stale(_)));
        assert!(provider.curator_calls().is_empty());
        assert!(service.lock_store().entries.is_empty());
    }

    #[tokio::test]
    async fn preparing_entry_reserves_only_the_owned_capture_bytes() {
        let provider = DraftProvider::new();
        provider.gate_curator();
        let agent = test_agent(&provider);
        let (service, _) = test_service(ContextServiceLimits::default());
        let mut draft_request = request();
        let fingerprint = {
            let mut guard = agent.lock().await;
            let revision = guard.context_view_state().revision;
            let digest = authoritative_transcript_digest(guard.messages());
            service
                .preview_context_curator_plan(
                    &mut guard,
                    false,
                    revision,
                    digest,
                    draft_request.clone(),
                )
                .expect("review exact gated curator plan")
                .fingerprint
        };
        draft_request.curator.expected_plan_fingerprint = Some(fingerprint);
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
            let capture = capture_context_draft(
                guard.messages(),
                guard.context_view_state(),
                identity,
                draft_request,
            )
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
                .contains("__tool-result distiller__")
        );
        assert!(!curator_calls[0].system.contains("__range summarizer__"));
        let curator_messages = serde_json::to_string(&curator_calls[0].messages).expect("messages");
        assert!(curator_messages.contains("complete_tool_result"));
        assert!(!curator_messages.contains("tool_distillation_requests"));
        assert!(!curator_messages.contains("request_id"));
        assert!(curator_messages.contains(RAW_RESULT_SENTINEL));
        assert_eq!(draft.default_selected_distillation_ids(), vec!["tool-1"]);
        assert_eq!(draft.curator_usage.len(), 1);
        assert_eq!(draft.curator_usage[0].input_tokens, 120);
        assert_eq!(draft.curator_usage[0].output_tokens, 30);
        assert_eq!(
            draft.curator_usage[0].role,
            Some(StoredContextCuratorRole::ToolResultDistiller)
        );
        assert_eq!(
            draft.curator_usage[0].artifact_id.as_deref(),
            Some("tool-1")
        );
        assert_eq!(
            draft.curator_usage[0].prompt_version.as_deref(),
            Some(CONTEXT_TOOL_DISTILLER_PROMPT_VERSION)
        );

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
            let StoredContextOperation::ToolResultDistillation(distillation) =
                &transaction.operations[0]
            else {
                panic!("expected persisted tool distillation")
            };
            assert_eq!(
                distillation.generator.role,
                Some(StoredContextCuratorRole::ToolResultDistiller)
            );
            assert_eq!(
                distillation.generator.selection_source,
                Some(StoredContextCuratorSelectionSource::ConfiguredDefault)
            );
            assert_eq!(
                distillation.generator.prompt_version,
                CONTEXT_TOOL_DISTILLER_PROMPT_VERSION
            );
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
        assert!(!live_json.contains("complete_tool_result"));
        assert!(!live_json.contains(CONTEXT_TOOL_DISTILLER_PROMPT_VERSION));
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
            agent.messages(),
            agent.context_view_state(),
            identity,
            ContextDraftRequest {
                summary_ranges: vec![range.clone()],
                reasoning: Some(ContextReasoningSelectionRequest::MessageRanges {
                    ranges: vec![range],
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
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
                    curator: Default::default(),
                    authorization: StoredContextAuthorization::Manual { initiated_by: None },
                },
                false,
                &crate::config::ContextCuratorConfig {
                    provider: Some("intentionally-missing-curator-route".to_string()),
                    route: None,
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

    #[test]
    fn repeated_reasoning_suppression_treats_active_exact_targets_as_satisfied() {
        let messages = vec![stored(
            "reasoning-message",
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "historical replayed reasoning".repeat(40),
            }],
        )];
        let existing =
            resolve_reasoning_suppression_keep_latest(&messages, 0).expect("existing suppression");
        let context_view = state_with_transaction(
            &StoredContextViewState::default(),
            "existing-suppression",
            1,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(existing)],
            None,
            Vec::new(),
        );
        let context_view: StoredContextViewState = serde_json::from_slice(
            &serde_json::to_vec(&context_view).expect("serialize persisted context state"),
        )
        .expect("reload persisted context state");
        let identity = ContextDraftIdentity {
            draft_id: "repeated-reasoning".to_string(),
            session_id: "session-repeated-reasoning".to_string(),
            base_context_revision: context_view.revision,
            raw_message_count: messages.len(),
            transcript_digest: authoritative_transcript_digest(&messages),
            provider_name: "draft-provider".to_string(),
            model: "draft-model".to_string(),
            route: "draft-route".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        };

        let capture = capture_context_draft(
            &messages,
            &context_view,
            identity,
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 0,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
        )
        .expect("redundant reasoning request remains a visible no-change capture");

        let reasoning = capture.reasoning.expect("reasoning selection is retained");
        assert!(reasoning.targets.is_empty());
        assert_eq!(reasoning.assistant_turns_affected, 0);
        assert_eq!(reasoning.original_token_estimate, 0);
        assert!(
            capture
                .notices
                .iter()
                .any(|notice| notice.contains("already suppressed"))
        );
    }

    #[test]
    fn partial_reasoning_overlap_retains_only_new_exact_targets() {
        let messages = vec![
            stored(
                "reasoning-one",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "first historical reasoning".repeat(20),
                }],
            ),
            stored(
                "reasoning-two",
                Role::Assistant,
                vec![ContentBlock::OpenAIReasoning {
                    id: "reasoning-item-two".to_string(),
                    summary: vec!["second historical reasoning".repeat(20)],
                    encrypted_content: None,
                    status: None,
                }],
            ),
        ];
        let first_range = build_message_range(&messages, 0, 0).expect("first range");
        let existing = resolve_reasoning_suppression_for_ranges(&messages, &[first_range])
            .expect("existing suppression");
        let context_view = state_with_transaction(
            &StoredContextViewState::default(),
            "existing-suppression",
            1,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(existing)],
            None,
            Vec::new(),
        );
        let capture = capture_context_draft(
            &messages,
            &context_view,
            ContextDraftIdentity {
                draft_id: "partial-overlap".to_string(),
                session_id: "session-partial-overlap".to_string(),
                base_context_revision: 1,
                raw_message_count: messages.len(),
                transcript_digest: authoritative_transcript_digest(&messages),
                provider_name: "draft-provider".to_string(),
                model: "draft-model".to_string(),
                route: "draft-route".to_string(),
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(30),
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 0,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
        )
        .expect("partial overlap capture");

        let reasoning = capture.reasoning.expect("reasoning selection");
        assert_eq!(reasoning.targets.len(), 1);
        assert_eq!(reasoning.targets[0].message_id, "reasoning-two");
        assert_eq!(reasoning.assistant_turns_affected, 1);
        assert!(
            capture
                .notices
                .iter()
                .any(|notice| notice.contains("already suppressed 1"))
        );
    }

    #[test]
    fn duplicate_active_reasoning_transforms_remain_strictly_invalid() {
        let messages = vec![stored(
            "reasoning-message",
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "duplicated persisted transform target".repeat(20),
            }],
        )];
        let suppression =
            resolve_reasoning_suppression_keep_latest(&messages, 0).expect("suppression");
        let first = state_with_transaction(
            &StoredContextViewState::default(),
            "first-suppression",
            1,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(
                suppression.clone(),
            )],
            None,
            Vec::new(),
        );
        let duplicated = state_with_transaction(
            &first,
            "duplicate-suppression",
            2,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(suppression)],
            None,
            Vec::new(),
        );

        let error = match capture_context_draft(
            &messages,
            &duplicated,
            ContextDraftIdentity {
                draft_id: "strict-duplicate".to_string(),
                session_id: "session-strict-duplicate".to_string(),
                base_context_revision: 2,
                raw_message_count: messages.len(),
                transcript_digest: authoritative_transcript_digest(&messages),
                provider_name: "draft-provider".to_string(),
                model: "draft-model".to_string(),
                route: "draft-route".to_string(),
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(30),
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 0,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
        ) {
            Ok(_) => panic!("duplicate active persisted transforms must remain invalid"),
            Err(error) => error,
        };

        assert!(matches!(error, ContextServiceError::Projection(_)));
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn reverted_reapplied_and_invalidated_reasoning_history_controls_future_eligibility() {
        let messages = vec![stored(
            "reasoning-message",
            Role::Assistant,
            vec![ContentBlock::AnthropicThinking {
                thinking: "signed historical thinking".repeat(20),
                signature: "signature".to_string(),
            }],
        )];
        let suppression =
            resolve_reasoning_suppression_keep_latest(&messages, 0).expect("suppression");
        let mut state = state_with_transaction(
            &StoredContextViewState::default(),
            "history-suppression",
            1,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(suppression)],
            None,
            Vec::new(),
        );
        let capture_targets = |state: &StoredContextViewState| {
            capture_context_draft(
                &messages,
                state,
                ContextDraftIdentity {
                    draft_id: format!("history-{}", state.revision),
                    session_id: "session-history".to_string(),
                    base_context_revision: state.revision,
                    raw_message_count: messages.len(),
                    transcript_digest: authoritative_transcript_digest(&messages),
                    provider_name: "draft-provider".to_string(),
                    model: "draft-model".to_string(),
                    route: "draft-route".to_string(),
                    created_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::minutes(30),
                },
                ContextDraftRequest {
                    summary_ranges: Vec::new(),
                    reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                        protected_recent_assistant_turns: 0,
                    }),
                    tool_results: Vec::new(),
                    allow_shadowing_active_operations: false,
                    curator: Default::default(),
                    authorization: StoredContextAuthorization::Manual { initiated_by: None },
                },
            )
            .expect("history capture")
            .reasoning
            .expect("reasoning selection")
            .targets
            .len()
        };
        assert_eq!(capture_targets(&state), 0);

        state.revision = 2;
        state.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 2,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reverted,
                reason: Some("test revert".to_string()),
            });
        assert_eq!(capture_targets(&state), 1);

        state.revision = 3;
        state.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 3,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reapplied,
                reason: Some("test reapply".to_string()),
            });
        assert_eq!(capture_targets(&state), 0);

        state.revision = 4;
        state.transactions[0]
            .status_events
            .push(StoredContextStatusEvent {
                revision: 4,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit,
                reason: Some("test invalidation".to_string()),
            });
        assert_eq!(capture_targets(&state), 1);
    }

    #[test]
    fn active_agent_profile_is_locked_before_range_preview_or_curator_capture() {
        let service = ContextTransactionService::new();
        let provider = DraftProvider::new();
        let messages = vec![
            stored(
                "ordinary-message",
                Role::User,
                vec![ContentBlock::Text {
                    text: "ordinary context".to_string(),
                    cache_control: None,
                }],
            ),
            stored(
                "active-profile-message",
                Role::User,
                vec![ContentBlock::Text {
                    text: "SYNTHETIC_ACTIVE_PROFILE".to_string(),
                    cache_control: None,
                }],
            ),
        ];
        let state = StoredContextViewState::default();
        let digest = authoritative_transcript_digest(&messages);
        let snapshot = service
            .context_editor_snapshot_for_session_with_curator_config_and_active_profile(
                "session-profile-lock",
                &messages,
                &state,
                false,
                &provider,
                "draft-route",
                None,
                Some("active-profile-message"),
                &crate::config::ContextCuratorConfig::default(),
            )
            .expect("profile-aware snapshot");
        assert!(!snapshot.messages[0].active_agent_profile);
        assert!(snapshot.messages[1].active_agent_profile);

        let error = service
            .preview_context_ranges_with_active_profile(
                "session-profile-lock",
                &messages,
                &state,
                0,
                digest,
                Some("active-profile-message"),
                &[ContextMessageRangeSelection {
                    start_message_id: "ordinary-message".to_string(),
                    end_message_id: "active-profile-message".to_string(),
                }],
            )
            .expect_err("active profile range must reject before curator work");
        assert!(matches!(error, ContextServiceError::Conflict(_)));

        service
            .preview_context_ranges_with_active_profile(
                "session-profile-lock",
                &messages,
                &state,
                0,
                digest,
                Some("active-profile-message"),
                &[ContextMessageRangeSelection {
                    start_message_id: "ordinary-message".to_string(),
                    end_message_id: "ordinary-message".to_string(),
                }],
            )
            .expect("unrelated context remains transformable");
    }

    #[test]
    fn backward_manual_reasoning_ranges_equal_forward_ranges() {
        let messages = vec![
            stored(
                "reasoning-one",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "first".repeat(20),
                }],
            ),
            stored(
                "reasoning-two",
                Role::Assistant,
                vec![ContentBlock::Reasoning {
                    text: "second".repeat(20),
                }],
            ),
        ];
        let capture = |start: &str, end: &str| {
            capture_context_draft(
                &messages,
                &StoredContextViewState::default(),
                ContextDraftIdentity {
                    draft_id: format!("{start}-{end}"),
                    session_id: "session-manual-ranges".to_string(),
                    base_context_revision: 0,
                    raw_message_count: messages.len(),
                    transcript_digest: authoritative_transcript_digest(&messages),
                    provider_name: "draft-provider".to_string(),
                    model: "draft-model".to_string(),
                    route: "draft-route".to_string(),
                    created_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::minutes(30),
                },
                ContextDraftRequest {
                    summary_ranges: Vec::new(),
                    reasoning: Some(ContextReasoningSelectionRequest::MessageRanges {
                        ranges: vec![ContextMessageRangeSelection {
                            start_message_id: start.to_string(),
                            end_message_id: end.to_string(),
                        }],
                    }),
                    tool_results: Vec::new(),
                    allow_shadowing_active_operations: false,
                    curator: Default::default(),
                    authorization: StoredContextAuthorization::Manual { initiated_by: None },
                },
            )
            .expect("manual reasoning capture")
            .reasoning
            .expect("reasoning selection")
        };

        assert_eq!(
            capture("reasoning-one", "reasoning-two"),
            capture("reasoning-two", "reasoning-one")
        );
    }

    #[test]
    fn redundant_reasoning_builds_a_visible_non_mutating_review() {
        let provider = DraftProvider::new();
        let messages = vec![stored(
            "reasoning-message",
            Role::Assistant,
            vec![ContentBlock::Reasoning {
                text: "historical replayed reasoning".repeat(40),
            }],
        )];
        let existing =
            resolve_reasoning_suppression_keep_latest(&messages, 0).expect("existing suppression");
        let state = state_with_transaction(
            &StoredContextViewState::default(),
            "existing-suppression",
            1,
            StoredContextAuthorization::Manual { initiated_by: None },
            vec![StoredContextOperation::ReasoningSuppression(existing)],
            None,
            Vec::new(),
        );
        let capture = capture_context_draft(
            &messages,
            &state,
            ContextDraftIdentity {
                draft_id: "no-change-review".to_string(),
                session_id: "session-no-change".to_string(),
                base_context_revision: 1,
                raw_message_count: messages.len(),
                transcript_digest: authoritative_transcript_digest(&messages),
                provider_name: provider.name().to_string(),
                model: provider.model(),
                route: "draft-route".to_string(),
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(30),
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 0,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
        )
        .expect("redundant capture");
        let draft = build_ready_draft_inner(
            &provider,
            None,
            capture,
            None,
            ContextCuratorArtifacts::default(),
        )
        .expect("no-change ready review");

        assert!(draft.required_operations.is_empty());
        assert!(draft.distillation_proposals.is_empty());
        assert_eq!(draft.preview.current_context_revision, 1);
        assert_eq!(draft.preview.proposed_context_revision, 1);
        assert!(draft.preview.operation_previews.is_empty());
        assert!(
            draft
                .preview
                .notices
                .iter()
                .any(|notice| notice.contains("no-op and cannot be applied"))
        );
    }

    #[test]
    fn forward_and_backward_summary_ranges_produce_the_same_canonical_preview() {
        let session = test_session();
        let service = ContextTransactionService::new();
        let digest = authoritative_transcript_digest(&session.messages);
        let forward = service
            .preview_context_ranges(
                &session.id,
                &session.messages,
                &session.context_view,
                session.context_view.revision,
                digest,
                &[ContextMessageRangeSelection {
                    start_message_id: "tool-call-message".to_string(),
                    end_message_id: "tool-result-message".to_string(),
                }],
            )
            .expect("forward range preview");
        let backward = service
            .preview_context_ranges(
                &session.id,
                &session.messages,
                &session.context_view,
                session.context_view.revision,
                digest,
                &[ContextMessageRangeSelection {
                    start_message_id: "tool-result-message".to_string(),
                    end_message_id: "tool-call-message".to_string(),
                }],
            )
            .expect("backward range preview");

        assert_eq!(backward, forward);
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
