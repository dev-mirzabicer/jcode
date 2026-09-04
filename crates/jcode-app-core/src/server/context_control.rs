use crate::agent::Agent;
use crate::context::{ContextTransactionService, list_context_transactions};
use crate::protocol::{
    CONTEXT_HISTORY_DEFAULT_LIMIT, CONTEXT_HISTORY_MAX_LIMIT, CONTEXT_IDENTIFIER_MAX_CHARS,
    CONTEXT_MAX_DISTILLATION_SELECTIONS, CONTEXT_MAX_SUMMARY_RANGES,
    CONTEXT_MAX_TOOL_RESULT_SELECTIONS, CONTEXT_MESSAGE_DETAIL_DEFAULT_MAX_CHARS,
    CONTEXT_MESSAGE_DETAIL_MAX_CHARS, CONTEXT_PROTOCOL_MAX_EVENT_BYTES,
    CONTEXT_SNAPSHOT_DEFAULT_PAGE_SIZE, CONTEXT_SNAPSHOT_MAX_PAGE_SIZE, ContextCuratorSelection,
    ContextDraftRequest, ContextDraftStatus, ContextMessageRangeSelection,
    ContextReasoningSelectionRequest, ContextRequestKind, ContextServiceError,
    ContextTransactionDetail, ServerEvent,
};
use jcode_session_types::{StoredContextAuthorization, StoredContextEmergencyPolicy};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub(super) fn handle_get_context_editor_snapshot(
    id: u64,
    page_start: usize,
    page_size: Option<usize>,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        let page_size = bounded_value(
            page_size.unwrap_or(CONTEXT_SNAPSHOT_DEFAULT_PAGE_SIZE),
            CONTEXT_SNAPSHOT_MAX_PAGE_SIZE,
            "context snapshot page size",
        )?;
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        service
            .context_editor_snapshot_page(&mut agent, processing, page_start, page_size)
            .map(|snapshot| ServerEvent::ContextEditorSnapshot { id, snapshot })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::Snapshot,
        None,
        None,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "lazy detail identity and chunk coordinates are distinct protocol fields"
)]
pub(super) fn handle_get_context_message_detail(
    id: u64,
    expected_context_revision: u64,
    expected_transcript_digest: u64,
    message_id: String,
    block_ordinal: usize,
    start_char: usize,
    max_chars: Option<usize>,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        validate_identifier(&message_id, "message ID")?;
        let max_chars = bounded_value(
            max_chars.unwrap_or(CONTEXT_MESSAGE_DETAIL_DEFAULT_MAX_CHARS),
            CONTEXT_MESSAGE_DETAIL_MAX_CHARS,
            "context detail chunk size",
        )?;
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        service
            .context_message_detail(
                &agent,
                expected_context_revision,
                expected_transcript_digest,
                &message_id,
                block_ordinal,
                start_char,
                max_chars,
            )
            .map(|detail| ServerEvent::ContextMessageDetail { id, detail })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::MessageDetail,
        None,
        None,
    );
}

pub(super) fn handle_preview_context_ranges(
    id: u64,
    expected_context_revision: u64,
    expected_transcript_digest: u64,
    ranges: Vec<ContextMessageRangeSelection>,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        if ranges.is_empty() || ranges.len() > CONTEXT_MAX_SUMMARY_RANGES {
            return Err(ContextServiceError::InvalidSelection(format!(
                "summary range count must be between 1 and {CONTEXT_MAX_SUMMARY_RANGES}"
            )));
        }
        for range in &ranges {
            validate_identifier(&range.start_message_id, "range start message ID")?;
            validate_identifier(&range.end_message_id, "range end message ID")?;
        }
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        service
            .preview_context_ranges(
                &agent,
                expected_context_revision,
                expected_transcript_digest,
                &ranges,
            )
            .map(|preview| ServerEvent::ContextRangeClosurePreview { id, preview })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::RangeClosurePreview,
        None,
        None,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "curator preview preserves exact request and authoritative transcript identity"
)]
pub(super) fn handle_preview_context_curator_plan(
    id: u64,
    expected_context_revision: u64,
    expected_transcript_digest: u64,
    request: ContextDraftRequest,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_draft_request(&request).and_then(|()| {
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        service
            .preview_context_curator_plan(
                &mut agent,
                processing,
                expected_context_revision,
                expected_transcript_digest,
                request,
            )
            .map(|preview| ServerEvent::ContextCuratorPlanPreview { id, preview })
    });
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::CuratorPlanPreview,
        None,
        None,
    );
}

pub(super) fn handle_save_context_curator_default(
    id: u64,
    selection: ContextCuratorSelection,
    agent: &Arc<Mutex<Agent>>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        crate::context::validate_context_curator_selection(&selection)?;
        let config = crate::config::ContextCuratorConfig {
            provider: selection.provider.clone(),
            route: selection.route.clone(),
            model: selection.model.clone(),
            effort: selection.effort.clone(),
        };
        let resolved_route = {
            let agent = agent
                .try_lock()
                .map_err(|_| ContextServiceError::SessionBusy)?;
            crate::context::resolve_context_curator_route(
                agent.provider_fork(),
                &agent.model_routes(),
                &agent.context_route_identity(),
                &config,
            )
            .map_err(|error| ContextServiceError::Curator(error.to_string()))?
            .preview()
        };
        crate::config::Config::set_context_curator(&config)
            .map_err(|error| ContextServiceError::Persistence(error.to_string()))?;
        Ok(ServerEvent::ContextCuratorDefaultSaved {
            id,
            selection,
            resolved_route: Some(resolved_route),
            unavailable_reason: None,
        })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::SaveCuratorDefault,
        None,
        None,
    );
}

pub(super) fn handle_prepare_context_draft(
    id: u64,
    request: ContextDraftRequest,
    session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_draft_request(&request)
        .and_then(|()| service.prepare_draft(Arc::clone(agent), request, processing));
    match result {
        Ok(draft_id) => attach_draft_monitor(
            id,
            ContextRequestKind::PrepareDraft,
            draft_id,
            session_id.to_string(),
            service,
            event_tx,
        ),
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::PrepareDraft,
            None,
            None,
            error,
        ),
    }
}

pub(super) fn handle_cancel_context_draft(
    id: u64,
    draft_id: String,
    session_id: &str,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_identifier(&draft_id, "draft ID")
        .and_then(|()| draft_status_for_session(service, &draft_id, session_id).map(|_| ()))
        .and_then(|()| service.cancel_draft(&draft_id))
        .and_then(|()| draft_status_for_session(service, &draft_id, session_id));
    match result {
        Ok(status) => emit_draft_status(event_tx, id, ContextRequestKind::CancelDraft, status),
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::CancelDraft,
            Some(draft_id),
            None,
            error,
        ),
    }
}

pub(super) fn handle_get_context_draft_status(
    id: u64,
    draft_id: String,
    session_id: &str,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Err(error) = validate_identifier(&draft_id, "draft ID") {
        emit_rejection(
            event_tx,
            id,
            ContextRequestKind::DraftStatus,
            Some(draft_id),
            None,
            error,
        );
        return;
    }
    attach_draft_monitor(
        id,
        ContextRequestKind::DraftStatus,
        draft_id,
        session_id.to_string(),
        service,
        event_tx,
    );
}

pub(super) fn handle_preview_context_draft_selection(
    id: u64,
    draft_id: String,
    selected_distillation_ids: Vec<String>,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_identifier(&draft_id, "draft ID")
        .and_then(|()| validate_distillation_ids(Some(&selected_distillation_ids)))
        .and_then(|()| service.preview_draft_selection(agent, &draft_id, selected_distillation_ids))
        .map(|preview| ServerEvent::ContextDraftSelectionPreview { id, preview });
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::DraftSelectionPreview,
        Some(draft_id),
        None,
    );
}

pub(super) fn handle_apply_context_draft(
    id: u64,
    draft_id: String,
    selected_distillation_ids: Option<Vec<String>>,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_identifier(&draft_id, "draft ID")
        .and_then(|()| validate_distillation_ids(selected_distillation_ids.as_deref()))
        .and_then(|()| {
            service.apply_draft(agent, &draft_id, selected_distillation_ids, processing)
        });
    match result {
        Ok(result) => emit_checked(
            event_tx,
            ServerEvent::ContextTransactionApplied {
                id,
                draft_id,
                result,
            },
            id,
            ContextRequestKind::ApplyDraft,
            None,
            None,
        ),
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::ApplyDraft,
            Some(draft_id),
            None,
            error,
        ),
    }
}

pub(super) fn handle_list_context_transactions(
    id: u64,
    offset: usize,
    limit: Option<usize>,
    agent: &Arc<Mutex<Agent>>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        let limit = bounded_value(
            limit.unwrap_or(CONTEXT_HISTORY_DEFAULT_LIMIT),
            CONTEXT_HISTORY_MAX_LIMIT,
            "context history limit",
        )?;
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let revision = agent.context_view_state().revision;
        let transactions = list_context_transactions(agent.context_view_state());
        if offset > transactions.len() {
            return Err(ContextServiceError::InvalidSelection(format!(
                "context history offset {offset} exceeds {} transactions",
                transactions.len()
            )));
        }
        let end = offset.saturating_add(limit).min(transactions.len());
        let page = transactions[offset..end].to_vec();
        Ok(ServerEvent::ContextTransactionHistory {
            id,
            context_revision: revision,
            total_transactions: transactions.len(),
            offset,
            next_offset: (end < transactions.len()).then_some(end),
            transactions: page,
        })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::TransactionHistory,
        None,
        None,
    );
}

pub(super) fn handle_get_context_transaction_detail(
    id: u64,
    expected_context_revision: u64,
    transaction_id: String,
    agent: &Arc<Mutex<Agent>>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = (|| {
        validate_identifier(&transaction_id, "transaction ID")?;
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let state = agent.context_view_state();
        if state.revision != expected_context_revision {
            return Err(ContextServiceError::Stale(format!(
                "context revision changed from {expected_context_revision} to {}",
                state.revision
            )));
        }
        let transaction = state
            .transactions
            .iter()
            .find(|transaction| transaction.id == transaction_id)
            .cloned()
            .ok_or_else(|| ContextServiceError::TransactionNotFound(transaction_id.clone()))?;
        Ok(ServerEvent::ContextTransactionDetail {
            id,
            detail: Box::new(ContextTransactionDetail {
                session_id: agent.session_id().to_string(),
                context_revision: state.revision,
                transaction,
            }),
        })
    })();
    emit_result(
        event_tx,
        result,
        id,
        ContextRequestKind::TransactionDetail,
        None,
        Some(transaction_id),
    );
}

pub(super) fn handle_revert_context_transaction(
    id: u64,
    transaction_id: String,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_identifier(&transaction_id, "transaction ID")
        .and_then(|()| service.revert_transaction(agent, &transaction_id, processing));
    match result {
        Ok(result) => emit_checked(
            event_tx,
            ServerEvent::ContextTransactionReverted {
                id,
                transaction_id,
                result,
            },
            id,
            ContextRequestKind::RevertTransaction,
            None,
            None,
        ),
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::RevertTransaction,
            None,
            Some(transaction_id),
            error,
        ),
    }
}

pub(super) fn handle_reapply_context_transaction(
    id: u64,
    transaction_id: String,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = validate_identifier(&transaction_id, "transaction ID")
        .and_then(|()| service.reapply_transaction(agent, &transaction_id, processing));
    match result {
        Ok(result) => emit_checked(
            event_tx,
            ServerEvent::ContextTransactionReapplied {
                id,
                transaction_id,
                result,
            },
            id,
            ContextRequestKind::ReapplyTransaction,
            None,
            None,
        ),
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::ReapplyTransaction,
            None,
            Some(transaction_id),
            error,
        ),
    }
}

pub(super) fn handle_set_context_emergency_policy(
    id: u64,
    policy: StoredContextEmergencyPolicy,
    agent: &Arc<Mutex<Agent>>,
    service: &Arc<ContextTransactionService>,
    processing: bool,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    match service.set_emergency_policy(agent, policy, processing) {
        Ok((session_id, policy)) => {
            emit_checked(
                event_tx,
                ServerEvent::ContextEmergencyPolicyChanged {
                    id,
                    session_id,
                    policy,
                },
                id,
                ContextRequestKind::SetEmergencyPolicy,
                None,
                None,
            );
        }
        Err(error) => emit_rejection(
            event_tx,
            id,
            ContextRequestKind::SetEmergencyPolicy,
            None,
            None,
            error,
        ),
    }
}

pub(super) fn reject_legacy_context_request(
    id: u64,
    request: ContextRequestKind,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    emit_rejection(
        event_tx,
        id,
        request,
        None,
        None,
        ContextServiceError::InvalidSelection(
            "automatic compaction controls are retired; open the context editor with /compact or /context edit"
                .to_string(),
        ),
    );
}

fn attach_draft_monitor(
    id: u64,
    request: ContextRequestKind,
    draft_id: String,
    expected_session_id: String,
    service: &Arc<ContextTransactionService>,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let service = Arc::clone(service);
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut status = match draft_status_for_session(&service, &draft_id, &expected_session_id) {
            Ok(status) => status,
            Err(error) => {
                emit_rejection(&event_tx, id, request, Some(draft_id), None, error);
                return;
            }
        };
        loop {
            emit_draft_status(&event_tx, id, request, status.clone());
            if status.is_terminal() {
                return;
            }
            status = match service.wait_for_draft_update(&draft_id, &status).await {
                Ok(status) if status.identity().session_id == expected_session_id => status,
                Ok(_) => {
                    emit_rejection(
                        &event_tx,
                        id,
                        request,
                        Some(draft_id.clone()),
                        None,
                        ContextServiceError::DraftNotFound(draft_id),
                    );
                    return;
                }
                Err(error) => {
                    emit_rejection(&event_tx, id, request, Some(draft_id), None, error);
                    return;
                }
            };
        }
    });
}

fn draft_status_for_session(
    service: &ContextTransactionService,
    draft_id: &str,
    expected_session_id: &str,
) -> Result<ContextDraftStatus, ContextServiceError> {
    let status = service.draft_status(draft_id)?;
    if status.identity().session_id != expected_session_id {
        return Err(ContextServiceError::DraftNotFound(draft_id.to_string()));
    }
    Ok(status)
}

fn emit_draft_status(
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    id: u64,
    request: ContextRequestKind,
    status: ContextDraftStatus,
) {
    let (event, draft_id) = match status {
        ContextDraftStatus::Preparing { identity, progress } => {
            let draft_id = identity.draft_id.clone();
            (
                ServerEvent::ContextDraftProgress {
                    id,
                    draft_id: draft_id.clone(),
                    progress,
                },
                draft_id,
            )
        }
        ContextDraftStatus::Ready { draft } => {
            let draft_id = draft.identity.draft_id.clone();
            (ServerEvent::ContextDraftReady { id, draft }, draft_id)
        }
        ContextDraftStatus::Applying { identity } => {
            let draft_id = identity.draft_id.clone();
            (ServerEvent::ContextDraftApplying { id, identity }, draft_id)
        }
        ContextDraftStatus::Applied {
            identity,
            transaction_id,
            revision,
        } => {
            let draft_id = identity.draft_id.clone();
            (
                ServerEvent::ContextDraftApplied {
                    id,
                    identity,
                    transaction_id,
                    revision,
                },
                draft_id,
            )
        }
        ContextDraftStatus::Failed { identity, error } => {
            let draft_id = identity.draft_id.clone();
            let event = if matches!(error, ContextServiceError::Stale(_)) {
                ServerEvent::ContextDraftStale {
                    id,
                    identity,
                    error,
                }
            } else {
                ServerEvent::ContextDraftFailed {
                    id,
                    identity,
                    error,
                }
            };
            (event, draft_id)
        }
        ContextDraftStatus::Canceled { identity } => {
            let draft_id = identity.draft_id.clone();
            (ServerEvent::ContextDraftCanceled { id, identity }, draft_id)
        }
        ContextDraftStatus::Expired { identity } => {
            let draft_id = identity.draft_id.clone();
            (ServerEvent::ContextDraftExpired { id, identity }, draft_id)
        }
    };
    emit_checked(event_tx, event, id, request, Some(draft_id), None);
}

fn validate_draft_request(request: &ContextDraftRequest) -> Result<(), ContextServiceError> {
    match &request.authorization {
        StoredContextAuthorization::Manual { initiated_by } => {
            if let Some(initiated_by) = initiated_by {
                validate_identifier(initiated_by, "manual authorization initiator")?;
            }
        }
        StoredContextAuthorization::UnattendedEmergency { .. }
        | StoredContextAuthorization::LegacyMigration { .. } => {
            return Err(ContextServiceError::InvalidSelection(
                "client-prepared context drafts must use manual authorization".to_string(),
            ));
        }
    }
    if request.summary_ranges.len() > CONTEXT_MAX_SUMMARY_RANGES {
        return Err(ContextServiceError::InvalidSelection(format!(
            "at most {CONTEXT_MAX_SUMMARY_RANGES} summary ranges may be prepared at once"
        )));
    }
    if request.tool_results.len() > CONTEXT_MAX_TOOL_RESULT_SELECTIONS {
        return Err(ContextServiceError::InvalidSelection(format!(
            "at most {CONTEXT_MAX_TOOL_RESULT_SELECTIONS} tool results may be prepared at once"
        )));
    }
    for range in &request.summary_ranges {
        validate_identifier(&range.start_message_id, "summary range start message ID")?;
        validate_identifier(&range.end_message_id, "summary range end message ID")?;
    }
    if let Some(ContextReasoningSelectionRequest::MessageRanges { ranges }) = &request.reasoning {
        if ranges.len() > CONTEXT_MAX_SUMMARY_RANGES {
            return Err(ContextServiceError::InvalidSelection(format!(
                "at most {CONTEXT_MAX_SUMMARY_RANGES} reasoning ranges may be prepared at once"
            )));
        }
        for range in ranges {
            validate_identifier(&range.start_message_id, "reasoning range start message ID")?;
            validate_identifier(&range.end_message_id, "reasoning range end message ID")?;
        }
    }
    for result in &request.tool_results {
        validate_identifier(&result.message_id, "tool-result message ID")?;
    }
    crate::context::validate_context_curator_run_config(&request.curator)?;
    for item in &request.curator.range_instructions {
        validate_identifier(
            &item.range.start_message_id,
            "curator-instruction range start message ID",
        )?;
        validate_identifier(
            &item.range.end_message_id,
            "curator-instruction range end message ID",
        )?;
    }
    Ok(())
}

fn validate_distillation_ids(ids: Option<&[String]>) -> Result<(), ContextServiceError> {
    let Some(ids) = ids else {
        return Ok(());
    };
    if ids.len() > CONTEXT_MAX_DISTILLATION_SELECTIONS {
        return Err(ContextServiceError::InvalidSelection(format!(
            "at most {CONTEXT_MAX_DISTILLATION_SELECTIONS} distillation proposals may be selected"
        )));
    }
    for id in ids {
        validate_identifier(id, "distillation proposal ID")?;
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), ContextServiceError> {
    let chars = value.chars().count();
    if value.trim().is_empty() || chars > CONTEXT_IDENTIFIER_MAX_CHARS {
        return Err(ContextServiceError::InvalidSelection(format!(
            "{label} must contain between 1 and {CONTEXT_IDENTIFIER_MAX_CHARS} characters"
        )));
    }
    Ok(())
}

fn bounded_value(value: usize, maximum: usize, label: &str) -> Result<usize, ContextServiceError> {
    if value == 0 || value > maximum {
        return Err(ContextServiceError::InvalidSelection(format!(
            "{label} must be between 1 and {maximum}"
        )));
    }
    Ok(value)
}

fn emit_result(
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    result: Result<ServerEvent, ContextServiceError>,
    id: u64,
    request: ContextRequestKind,
    draft_id: Option<String>,
    transaction_id: Option<String>,
) {
    match result {
        Ok(event) => emit_checked(event_tx, event, id, request, draft_id, transaction_id),
        Err(error) => emit_rejection(event_tx, id, request, draft_id, transaction_id, error),
    }
}

fn emit_checked(
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    event: ServerEvent,
    id: u64,
    request: ContextRequestKind,
    draft_id: Option<String>,
    transaction_id: Option<String>,
) {
    match serde_json::to_vec(&event) {
        Ok(bytes) if bytes.len() <= CONTEXT_PROTOCOL_MAX_EVENT_BYTES => {
            let _ = event_tx.send(event);
        }
        Ok(bytes) => emit_rejection(
            event_tx,
            id,
            request,
            draft_id,
            transaction_id,
            ContextServiceError::Capacity(format!(
                "context protocol event requires {} bytes, exceeding the {}-byte bound",
                bytes.len(),
                CONTEXT_PROTOCOL_MAX_EVENT_BYTES
            )),
        ),
        Err(error) => emit_rejection(
            event_tx,
            id,
            request,
            draft_id,
            transaction_id,
            ContextServiceError::Runtime(format!(
                "context protocol event serialization failed: {error}"
            )),
        ),
    }
}

fn emit_rejection(
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    id: u64,
    request: ContextRequestKind,
    draft_id: Option<String>,
    transaction_id: Option<String>,
    error: ContextServiceError,
) {
    let fallback_draft_id = bounded_rejection_correlation(draft_id.as_deref());
    let fallback_transaction_id = bounded_rejection_correlation(transaction_id.as_deref());
    let event = ServerEvent::ContextRequestRejected {
        id,
        request,
        draft_id,
        transaction_id,
        error,
    };
    if serde_json::to_vec(&event).is_ok_and(|bytes| bytes.len() <= CONTEXT_PROTOCOL_MAX_EVENT_BYTES)
    {
        let _ = event_tx.send(event);
        return;
    }

    // Do not recurse through `emit_checked`: the event being replaced is itself
    // a rejection. Preserve only already-bounded correlation strings so normal
    // client reducers can resolve the pending request, while oversized attacker-
    // controlled identifiers cannot make the fallback exceed the wire bound.
    let fallback = ServerEvent::ContextRequestRejected {
        id,
        request,
        draft_id: fallback_draft_id,
        transaction_id: fallback_transaction_id,
        error: ContextServiceError::Capacity(format!(
            "context request rejection exceeded the {CONTEXT_PROTOCOL_MAX_EVENT_BYTES}-byte protocol bound"
        )),
    };
    debug_assert!(
        serde_json::to_vec(&fallback)
            .is_ok_and(|bytes| bytes.len() <= CONTEXT_PROTOCOL_MAX_EVENT_BYTES)
    );
    let _ = event_tx.send(fallback);
}

fn bounded_rejection_correlation(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| {
            !value.trim().is_empty() && value.chars().count() <= CONTEXT_IDENTIFIER_MAX_CHARS
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        ContextDraftEntry, ContextPersistence, ContextServiceLimits, ContextTransactionService,
        DraftEntryState,
    };
    use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
    use crate::protocol::{
        CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS, ContextCuratorRangeInstructions,
        ContextCuratorRunConfig, ContextDraftIdentity, ContextDraftPhase, ContextDraftProgress,
        ContextMessageDetailFormat, ContextMessageRangeSelection, ContextToolResultSelection,
    };
    use crate::provider::{
        ContextProjectionValidationOperation, ContextProviderFamily,
        ContextProviderValidationIdentity, ContextReasoningBlockKind,
        ContextRequestBuilderValidation, EventStream, Provider,
        context_projection_validation_report,
    };
    use crate::tool::Registry;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Utc;
    use jcode_context_core::authoritative_transcript_digest;
    use jcode_session_types::{StoredContextAuthorization, StoredLegacyContextSource};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct TestProviderState {
        forks: AtomicUsize,
        invalidations: AtomicUsize,
    }

    #[derive(Clone, Default)]
    struct TestProvider {
        state: Arc<TestProviderState>,
    }

    impl TestProvider {
        fn fork_count(&self) -> usize {
            self.state.forks.load(Ordering::SeqCst)
        }

        fn invalidation_count(&self) -> usize {
            self.state.invalidations.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for TestProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty::<Result<StreamEvent>>()))
        }

        fn name(&self) -> &str {
            "context-control-test"
        }

        fn display_name(&self) -> String {
            "Context Control Test".to_string()
        }

        fn model(&self) -> String {
            "context-control-model".to_string()
        }

        fn context_window(&self) -> usize {
            372_000
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> crate::provider::ContextProjectionValidationReport {
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: ContextProviderFamily::OpenRouterCompatible,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "context_control_test_builder_v1".to_string(),
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
            self.state.forks.fetch_add(1, Ordering::SeqCst);
            Arc::new(self.clone())
        }
    }

    #[derive(Default)]
    struct TestPersistence {
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    impl TestPersistence {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ContextPersistence for TestPersistence {
        fn persist(&self, _agent: &mut Agent) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("injected server context persistence failure");
            }
            Ok(())
        }
    }

    struct Fixture {
        agent: Arc<Mutex<Agent>>,
        service: Arc<ContextTransactionService>,
        provider: TestProvider,
        persistence: Arc<TestPersistence>,
    }

    fn fixture() -> Fixture {
        let provider = TestProvider::default();
        let provider_dyn: Arc<dyn Provider> = Arc::new(provider.clone());
        let mut agent = Agent::new(provider_dyn, Registry::empty());
        agent.add_message(
            Role::User,
            vec![
                ContentBlock::Text {
                    text: "A🙂éZ".to_string(),
                    cache_control: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "opaque-image-base64-must-never-cross-context-protocol".to_string(),
                },
            ],
        );
        agent.add_message(
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "historical replay reasoning".repeat(32),
                },
                ContentBlock::Text {
                    text: "visible answer".to_string(),
                    cache_control: None,
                },
            ],
        );
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "context-call".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: Some("opaque-thought-signature".to_string()),
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "context-call".to_string(),
                content: "large result ".repeat(64),
                is_error: Some(false),
            }],
        );
        let persistence = Arc::new(TestPersistence::default());
        let service = Arc::new(ContextTransactionService::with_persistence(
            ContextServiceLimits::default(),
            persistence.clone(),
        ));
        Fixture {
            agent: Arc::new(Mutex::new(agent)),
            service,
            provider,
            persistence,
        }
    }

    fn reasoning_request() -> ContextDraftRequest {
        ContextDraftRequest {
            summary_ranges: Vec::new(),
            reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 0,
            }),
            tool_results: Vec::new(),
            allow_shadowing_active_operations: false,
            curator: Default::default(),
            authorization: StoredContextAuthorization::Manual {
                initiated_by: Some("server-context-test".to_string()),
            },
        }
    }

    fn receive_now(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
        rx.try_recv().expect("handler emitted one immediate event")
    }

    async fn receive_async(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("context event timeout")
            .expect("context event channel remains open")
    }

    fn preparing_identity(agent: &Agent, draft_id: &str) -> ContextDraftIdentity {
        ContextDraftIdentity {
            draft_id: draft_id.to_string(),
            session_id: agent.session_id().to_string(),
            base_context_revision: agent.context_view_state().revision,
            raw_message_count: agent.messages().len(),
            transcript_digest: authoritative_transcript_digest(agent.messages()),
            provider_name: agent.provider_handle().name().to_string(),
            model: agent.provider_handle().model(),
            route: agent.context_route_identity(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        }
    }

    #[test]
    fn snapshot_and_detail_handlers_enforce_bounds_and_hide_opaque_payloads() {
        let Fixture { agent, service, .. } = fixture();
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_get_context_editor_snapshot(1, 0, None, &agent, &service, false, &tx);
        let snapshot = match receive_now(&mut rx) {
            ServerEvent::ContextEditorSnapshot { id, snapshot } => {
                assert_eq!(id, 1);
                snapshot
            }
            other => panic!("expected snapshot, got {other:?}"),
        };
        assert_eq!(snapshot.message_page_start, 0);
        assert_eq!(snapshot.messages.len(), 5);
        assert!(
            serde_json::to_string(&snapshot)
                .expect("snapshot JSON")
                .find("opaque-image-base64")
                .is_none()
        );

        handle_get_context_editor_snapshot(
            2,
            0,
            Some(CONTEXT_SNAPSHOT_MAX_PAGE_SIZE),
            &agent,
            &service,
            false,
            &tx,
        );
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextEditorSnapshot { id: 2, .. }
        ));
        for (id, page_start, page_size) in [
            (3, 0, Some(0)),
            (4, 0, Some(CONTEXT_SNAPSHOT_MAX_PAGE_SIZE + 1)),
            (5, snapshot.raw_message_count + 1, None),
        ] {
            handle_get_context_editor_snapshot(
                id, page_start, page_size, &agent, &service, false, &tx,
            );
            assert!(matches!(
                receive_now(&mut rx),
                ServerEvent::ContextRequestRejected {
                    id: actual,
                    request: ContextRequestKind::Snapshot,
                    error: ContextServiceError::InvalidSelection(_),
                    ..
                } if actual == id
            ));
        }

        let message_id = snapshot
            .messages
            .iter()
            .find(|message| message.preview.contains("A🙂éZ"))
            .expect("Unicode user message")
            .message_id
            .clone();
        handle_get_context_message_detail(
            6,
            snapshot.context_revision,
            snapshot.transcript_digest,
            message_id.clone(),
            0,
            1,
            Some(2),
            &agent,
            &service,
            &tx,
        );
        match receive_now(&mut rx) {
            ServerEvent::ContextMessageDetail { id, detail } => {
                assert_eq!(id, 6);
                assert_eq!(detail.content.text, "🙂é");
                assert_eq!(detail.content.start_char, 1);
                assert_eq!(detail.content.next_start_char, Some(3));
            }
            other => panic!("expected Unicode detail, got {other:?}"),
        }

        handle_get_context_message_detail(
            7,
            snapshot.context_revision,
            snapshot.transcript_digest,
            message_id.clone(),
            1,
            0,
            None,
            &agent,
            &service,
            &tx,
        );
        match receive_now(&mut rx) {
            ServerEvent::ContextMessageDetail { detail, .. } => {
                assert_eq!(detail.format, ContextMessageDetailFormat::MetadataOnly);
                assert_eq!(detail.image_media_type.as_deref(), Some("image/png"));
                assert!(detail.image_encoded_bytes.is_some());
                assert!(!detail.content.text.contains("opaque-image-base64"));
            }
            other => panic!("expected image metadata detail, got {other:?}"),
        }

        let tool_message = snapshot
            .messages
            .iter()
            .find(|message| {
                message
                    .blocks
                    .iter()
                    .any(|block| block.has_tool_thought_signature)
            })
            .expect("tool message with an opaque thought signature");
        let tool_block = tool_message
            .blocks
            .iter()
            .find(|block| block.has_tool_thought_signature)
            .expect("opaque tool block");
        handle_get_context_message_detail(
            14,
            snapshot.context_revision,
            snapshot.transcript_digest,
            tool_message.message_id.clone(),
            tool_block.ordinal,
            0,
            None,
            &agent,
            &service,
            &tx,
        );
        match receive_now(&mut rx) {
            ServerEvent::ContextMessageDetail { detail, .. } => {
                assert!(detail.opaque_signature_present);
                assert!(!detail.content.text.contains("opaque-thought-signature"));
            }
            other => panic!("expected opaque tool metadata, got {other:?}"),
        }

        for (id, revision, digest, requested_id, block) in [
            (
                8,
                snapshot.context_revision + 1,
                snapshot.transcript_digest,
                message_id.clone(),
                0,
            ),
            (
                9,
                snapshot.context_revision,
                snapshot.transcript_digest + 1,
                message_id.clone(),
                0,
            ),
            (
                10,
                snapshot.context_revision,
                snapshot.transcript_digest,
                "missing-message".to_string(),
                0,
            ),
            (
                11,
                snapshot.context_revision,
                snapshot.transcript_digest,
                message_id.clone(),
                99,
            ),
        ] {
            handle_get_context_message_detail(
                id,
                revision,
                digest,
                requested_id,
                block,
                0,
                None,
                &agent,
                &service,
                &tx,
            );
            let event = receive_now(&mut rx);
            assert!(matches!(
                event,
                ServerEvent::ContextRequestRejected {
                    id: actual,
                    request: ContextRequestKind::MessageDetail,
                    ..
                } if actual == id
            ));
            if id <= 9 {
                assert!(matches!(
                    event,
                    ServerEvent::ContextRequestRejected {
                        error: ContextServiceError::Stale(_),
                        ..
                    }
                ));
            }
        }

        for (id, max_chars) in [
            (12, Some(0)),
            (13, Some(CONTEXT_MESSAGE_DETAIL_MAX_CHARS + 1)),
        ] {
            handle_get_context_message_detail(
                id,
                snapshot.context_revision,
                snapshot.transcript_digest,
                message_id.clone(),
                0,
                0,
                max_chars,
                &agent,
                &service,
                &tx,
            );
            assert!(matches!(
                receive_now(&mut rx),
                ServerEvent::ContextRequestRejected {
                    id: actual,
                    request: ContextRequestKind::MessageDetail,
                    error: ContextServiceError::InvalidSelection(_),
                    ..
                } if actual == id
            ));
        }
    }

    #[tokio::test]
    async fn reasoning_only_prepare_never_forks_curator_and_reconnect_reuses_ready_draft() {
        let Fixture {
            agent,
            service,
            provider,
            ..
        } = fixture();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session_id = agent
            .try_lock()
            .expect("idle agent")
            .session_id()
            .to_string();

        handle_prepare_context_draft(
            20,
            reasoning_request(),
            &session_id,
            &agent,
            &service,
            false,
            &tx,
        );
        let (draft_id, ready) = loop {
            match receive_async(&mut rx).await {
                ServerEvent::ContextDraftProgress { id, .. } => assert_eq!(id, 20),
                ServerEvent::ContextDraftReady { id, draft } => {
                    assert_eq!(id, 20);
                    break (draft.identity.draft_id.clone(), draft);
                }
                other => panic!("unexpected prepare monitor event: {other:?}"),
            }
        };
        assert_eq!(provider.fork_count(), 0);
        assert!(ready.distillation_proposals.is_empty());
        assert!(ready.curator_usage.is_empty());

        let (status_a_tx, mut status_a_rx) = mpsc::unbounded_channel();
        let (status_b_tx, mut status_b_rx) = mpsc::unbounded_channel();
        handle_get_context_draft_status(21, draft_id.clone(), &session_id, &service, &status_a_tx);
        handle_get_context_draft_status(22, draft_id.clone(), &session_id, &service, &status_b_tx);
        for (expected_id, event) in [
            (21, receive_async(&mut status_a_rx).await),
            (22, receive_async(&mut status_b_rx).await),
        ] {
            assert!(matches!(
                event,
                ServerEvent::ContextDraftReady { id, draft }
                    if id == expected_id && draft.identity.draft_id == draft_id
            ));
        }
        assert_eq!(provider.fork_count(), 0);
        assert!(matches!(
            service.draft_status(&draft_id),
            Ok(ContextDraftStatus::Ready { .. })
        ));
    }

    #[tokio::test]
    async fn process_wide_drafts_are_inaccessible_and_immutable_across_sessions() {
        let Fixture {
            agent,
            service,
            provider,
            ..
        } = fixture();
        let owner_session_id = agent
            .try_lock()
            .expect("idle owner agent")
            .session_id()
            .to_string();
        let draft_id = service
            .prepare_draft(Arc::clone(&agent), reasoning_request(), false)
            .expect("prepare owner draft");
        assert!(matches!(
            service
                .wait_for_draft(&draft_id, Duration::from_secs(2))
                .await
                .expect("owner draft becomes ready"),
            ContextDraftStatus::Ready { .. }
        ));

        let other_provider: Arc<dyn Provider> = Arc::new(provider);
        let other_agent = Arc::new(Mutex::new(Agent::new(other_provider, Registry::empty())));
        let other_session_id = other_agent
            .try_lock()
            .expect("idle other agent")
            .session_id()
            .to_string();
        assert_ne!(owner_session_id, other_session_id);

        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_get_context_draft_status(26, draft_id.clone(), &other_session_id, &service, &tx);
        assert!(matches!(
            receive_async(&mut rx).await,
            ServerEvent::ContextRequestRejected {
                id: 26,
                request: ContextRequestKind::DraftStatus,
                error: ContextServiceError::DraftNotFound(_),
                ..
            }
        ));

        handle_cancel_context_draft(27, draft_id.clone(), &other_session_id, &service, &tx);
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextRequestRejected {
                id: 27,
                request: ContextRequestKind::CancelDraft,
                error: ContextServiceError::DraftNotFound(_),
                ..
            }
        ));

        handle_apply_context_draft(
            28,
            draft_id.clone(),
            None,
            &other_agent,
            &service,
            false,
            &tx,
        );
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextRequestRejected {
                id: 28,
                request: ContextRequestKind::ApplyDraft,
                error: ContextServiceError::DraftNotFound(_),
                ..
            }
        ));
        assert!(matches!(
            service.draft_status(&draft_id),
            Ok(ContextDraftStatus::Ready { .. })
        ));

        handle_get_context_draft_status(29, draft_id, &owner_session_id, &service, &tx);
        assert!(matches!(
            receive_async(&mut rx).await,
            ServerEvent::ContextDraftReady { id: 29, .. }
        ));
    }

    #[tokio::test]
    async fn cancel_notifies_multiple_monitors_once_without_polling_or_consuming_state() {
        let Fixture { agent, service, .. } = fixture();
        let identity = {
            let guard = agent.try_lock().expect("idle agent");
            preparing_identity(&guard, "draft-monitor")
        };
        let session_id = identity.session_id.clone();
        service.lock_store().entries.insert(
            identity.draft_id.clone(),
            ContextDraftEntry {
                identity: identity.clone(),
                progress: ContextDraftProgress {
                    phase: ContextDraftPhase::PreparingArtifacts,
                    completed_items: 1,
                    total_items: 2,
                },
                state: DraftEntryState::Preparing,
                cancellation: CancellationToken::new(),
                notify: Arc::new(Notify::new()),
                reserved_bytes: 512,
                generation_in_flight: true,
            },
        );

        let (monitor_a_tx, mut monitor_a_rx) = mpsc::unbounded_channel();
        let (monitor_b_tx, mut monitor_b_rx) = mpsc::unbounded_channel();
        attach_draft_monitor(
            30,
            ContextRequestKind::PrepareDraft,
            identity.draft_id.clone(),
            session_id.clone(),
            &service,
            &monitor_a_tx,
        );
        attach_draft_monitor(
            31,
            ContextRequestKind::DraftStatus,
            identity.draft_id.clone(),
            session_id.clone(),
            &service,
            &monitor_b_tx,
        );
        assert!(matches!(
            receive_async(&mut monitor_a_rx).await,
            ServerEvent::ContextDraftProgress { id: 30, .. }
        ));
        assert!(matches!(
            receive_async(&mut monitor_b_rx).await,
            ServerEvent::ContextDraftProgress { id: 31, .. }
        ));

        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
        handle_cancel_context_draft(
            32,
            identity.draft_id.clone(),
            &session_id,
            &service,
            &cancel_tx,
        );
        assert!(matches!(
            receive_async(&mut cancel_rx).await,
            ServerEvent::ContextDraftCanceled { id: 32, .. }
        ));
        assert!(matches!(
            receive_async(&mut monitor_a_rx).await,
            ServerEvent::ContextDraftCanceled { id: 30, .. }
        ));
        assert!(matches!(
            receive_async(&mut monitor_b_rx).await,
            ServerEvent::ContextDraftCanceled { id: 31, .. }
        ));
        tokio::task::yield_now().await;
        assert!(monitor_a_rx.try_recv().is_err());
        assert!(monitor_b_rx.try_recv().is_err());
        assert!(matches!(
            service.draft_status(&identity.draft_id),
            Ok(ContextDraftStatus::Canceled { .. })
        ));
    }

    #[test]
    fn apply_race_history_revert_reapply_and_policy_preserve_exact_correlation() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build context-control correlation test runtime");
        runtime.block_on(
            apply_race_history_revert_reapply_and_policy_preserve_exact_correlation_async(),
        );
    }

    async fn apply_race_history_revert_reapply_and_policy_preserve_exact_correlation_async() {
        crate::cache_invalidation::clear_for_tests();
        let Fixture {
            agent,
            service,
            provider,
            persistence,
        } = fixture();
        let draft_id = service
            .prepare_draft(Arc::clone(&agent), reasoning_request(), false)
            .expect("prepare reasoning-only draft");
        assert!(matches!(
            service
                .wait_for_draft(&draft_id, Duration::from_secs(2))
                .await
                .expect("ready draft"),
            ContextDraftStatus::Ready { .. }
        ));

        let (client_a_tx, mut client_a_rx) = mpsc::unbounded_channel();
        let (client_b_tx, mut client_b_rx) = mpsc::unbounded_channel();
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            for (id, tx) in [(40, client_a_tx), (41, client_b_tx)] {
                let barrier = Arc::clone(&barrier);
                let agent = Arc::clone(&agent);
                let service = Arc::clone(&service);
                let draft_id = draft_id.clone();
                scope.spawn(move || {
                    barrier.wait();
                    handle_apply_context_draft(id, draft_id, None, &agent, &service, false, &tx);
                });
            }
        });
        let events = [receive_now(&mut client_a_rx), receive_now(&mut client_b_rx)];
        let mut applied = None;
        let mut rejected = 0;
        for event in events {
            match event {
                ServerEvent::ContextTransactionApplied {
                    id,
                    draft_id: event_draft_id,
                    result,
                } => {
                    assert!(id == 40 || id == 41);
                    assert_eq!(event_draft_id, draft_id);
                    assert!(applied.replace(result).is_none());
                }
                ServerEvent::ContextRequestRejected {
                    id,
                    request: ContextRequestKind::ApplyDraft,
                    draft_id: Some(event_draft_id),
                    error,
                    ..
                } => {
                    assert!(id == 40 || id == 41);
                    assert_eq!(event_draft_id, draft_id);
                    assert!(matches!(
                        error,
                        ContextServiceError::SessionBusy
                            | ContextServiceError::DraftNotReady(_)
                            | ContextServiceError::DraftAlreadyApplied(_)
                    ));
                    rejected += 1;
                }
                other => panic!("unexpected apply race event: {other:?}"),
            }
        }
        let applied = applied.expect("exactly one apply succeeds");
        assert_eq!(rejected, 1);
        assert_eq!(applied.revision, 1);
        assert_eq!(persistence.calls(), 1);
        assert_eq!(provider.invalidation_count(), 1);

        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_list_context_transactions(42, 0, None, &agent, &tx);
        match receive_now(&mut rx) {
            ServerEvent::ContextTransactionHistory {
                id,
                context_revision,
                total_transactions,
                transactions,
                ..
            } => {
                assert_eq!(id, 42);
                assert_eq!(context_revision, 1);
                assert_eq!(total_transactions, 1);
                assert_eq!(transactions[0].id, applied.transaction.id);
            }
            other => panic!("expected transaction history, got {other:?}"),
        }
        handle_list_context_transactions(43, 2, None, &agent, &tx);
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextRequestRejected {
                id: 43,
                request: ContextRequestKind::TransactionHistory,
                error: ContextServiceError::InvalidSelection(_),
                ..
            }
        ));

        handle_revert_context_transaction(
            44,
            applied.transaction.id.clone(),
            &agent,
            &service,
            false,
            &tx,
        );
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextTransactionReverted { id: 44, ref result, .. }
                if result.revision == 2
        ));
        handle_reapply_context_transaction(
            45,
            applied.transaction.id.clone(),
            &agent,
            &service,
            false,
            &tx,
        );
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextTransactionReapplied { id: 45, ref result, .. }
                if result.revision == 3
        ));
        assert_eq!(persistence.calls(), 3);
        assert_eq!(provider.invalidation_count(), 3);

        let session_id = agent
            .try_lock()
            .expect("idle agent")
            .session_id()
            .to_string();
        let policy = StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 20,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: true,
            authorization_source: "explicit server test authorization".to_string(),
        };
        handle_set_context_emergency_policy(46, policy.clone(), &agent, &service, false, &tx);
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextEmergencyPolicyChanged {
                id: 46,
                session_id: ref event_session_id,
                policy: ref event_policy,
            } if event_session_id == &session_id && event_policy == &policy
        ));
        assert_eq!(
            agent
                .try_lock()
                .expect("idle agent")
                .context_view_state()
                .revision,
            3
        );
        assert_eq!(persistence.calls(), 4);
        assert_eq!(provider.invalidation_count(), 3);
    }

    #[tokio::test]
    async fn event_bounds_and_missing_draft_rejections_keep_originating_request_kind() {
        let Fixture { service, .. } = fixture();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let identity = ContextDraftIdentity {
            draft_id: "oversized-draft".to_string(),
            session_id: "session".to_string(),
            base_context_revision: 0,
            raw_message_count: 0,
            transcript_digest: 0,
            provider_name: "test".to_string(),
            model: "test".to_string(),
            route: "test".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        };
        emit_checked(
            &tx,
            ServerEvent::ContextDraftFailed {
                id: 50,
                identity,
                error: ContextServiceError::Runtime(
                    "x".repeat(CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1),
                ),
            },
            50,
            ContextRequestKind::PrepareDraft,
            Some("oversized-draft".to_string()),
            None,
        );
        assert!(matches!(
            receive_now(&mut rx),
            ServerEvent::ContextRequestRejected {
                id: 50,
                request: ContextRequestKind::PrepareDraft,
                draft_id: Some(ref draft_id),
                error: ContextServiceError::Capacity(_),
                ..
            } if draft_id == "oversized-draft"
        ));

        emit_rejection(
            &tx,
            53,
            ContextRequestKind::DraftStatus,
            Some("oversized-draft".to_string()),
            None,
            ContextServiceError::Runtime("x".repeat(CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1)),
        );
        let bounded_rejection = receive_now(&mut rx);
        assert!(matches!(
            bounded_rejection,
            ServerEvent::ContextRequestRejected {
                id: 53,
                request: ContextRequestKind::DraftStatus,
                draft_id: Some(ref draft_id),
                transaction_id: None,
                error: ContextServiceError::Capacity(_),
            } if draft_id == "oversized-draft"
        ));
        assert!(
            serde_json::to_vec(&bounded_rejection)
                .expect("bounded rejection serializes")
                .len()
                <= CONTEXT_PROTOCOL_MAX_EVENT_BYTES
        );

        emit_rejection(
            &tx,
            54,
            ContextRequestKind::ApplyDraft,
            Some("x".repeat(CONTEXT_IDENTIFIER_MAX_CHARS + 1)),
            None,
            ContextServiceError::Runtime("x".repeat(CONTEXT_PROTOCOL_MAX_EVENT_BYTES + 1)),
        );
        let sanitized_rejection = receive_now(&mut rx);
        assert!(matches!(
            sanitized_rejection,
            ServerEvent::ContextRequestRejected {
                id: 54,
                request: ContextRequestKind::ApplyDraft,
                draft_id: None,
                transaction_id: None,
                error: ContextServiceError::Capacity(_),
            }
        ));
        assert!(
            serde_json::to_vec(&sanitized_rejection)
                .expect("sanitized rejection serializes")
                .len()
                <= CONTEXT_PROTOCOL_MAX_EVENT_BYTES
        );

        attach_draft_monitor(
            51,
            ContextRequestKind::PrepareDraft,
            "missing-prepare".to_string(),
            "session".to_string(),
            &service,
            &tx,
        );
        assert!(matches!(
            receive_async(&mut rx).await,
            ServerEvent::ContextRequestRejected {
                id: 51,
                request: ContextRequestKind::PrepareDraft,
                draft_id: Some(ref draft_id),
                error: ContextServiceError::DraftNotFound(_),
                ..
            } if draft_id == "missing-prepare"
        ));
        attach_draft_monitor(
            52,
            ContextRequestKind::DraftStatus,
            "missing-status".to_string(),
            "session".to_string(),
            &service,
            &tx,
        );
        assert!(matches!(
            receive_async(&mut rx).await,
            ServerEvent::ContextRequestRejected {
                id: 52,
                request: ContextRequestKind::DraftStatus,
                draft_id: Some(ref draft_id),
                error: ContextServiceError::DraftNotFound(_),
                ..
            } if draft_id == "missing-status"
        ));
    }

    #[test]
    fn legacy_requests_are_typed_migration_rejections_without_context_mutation() {
        let Fixture {
            agent, provider, ..
        } = fixture();
        let state_before = agent
            .try_lock()
            .expect("idle agent")
            .context_view_state()
            .clone();
        let (tx, mut rx) = mpsc::unbounded_channel();

        for (id, request) in [
            (60, ContextRequestKind::LegacyCompact),
            (61, ContextRequestKind::LegacySetCompactionMode),
        ] {
            reject_legacy_context_request(id, request, &tx);
            match receive_now(&mut rx) {
                ServerEvent::ContextRequestRejected {
                    id: event_id,
                    request: event_request,
                    error: ContextServiceError::InvalidSelection(message),
                    ..
                } => {
                    assert_eq!(event_id, id);
                    assert_eq!(event_request, request);
                    assert!(message.contains("/compact"));
                    assert!(message.contains("/context edit"));
                }
                other => panic!("expected legacy request rejection, got {other:?}"),
            }
        }
        assert_eq!(
            agent.try_lock().expect("idle agent").context_view_state(),
            &state_before
        );
        assert_eq!(provider.invalidation_count(), 0);
    }

    #[test]
    fn draft_request_validation_rejects_all_protocol_bounds_before_service_work() {
        let Fixture { agent, service, .. } = fixture();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session_id = agent
            .try_lock()
            .expect("idle agent")
            .session_id()
            .to_string();
        let mut invalid_cases = vec![
            ContextDraftRequest {
                summary_ranges: vec![ContextMessageRangeSelection {
                    start_message_id: "".to_string(),
                    end_message_id: "end".to_string(),
                }],
                reasoning: None,
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::MessageRanges {
                    ranges: vec![ContextMessageRangeSelection {
                        start_message_id: "start".to_string(),
                        end_message_id: "".to_string(),
                    }],
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: None,
                tool_results: vec![ContextToolResultSelection {
                    message_id: "".to_string(),
                    block_ordinal: 0,
                }],
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual { initiated_by: None },
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::Manual {
                    initiated_by: Some("x".repeat(CONTEXT_IDENTIFIER_MAX_CHARS + 1)),
                },
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::UnattendedEmergency {
                    authorization_source: "forged-client-authorization".to_string(),
                    trigger: None,
                    scheduled_item_id: None,
                },
            },
            ContextDraftRequest {
                summary_ranges: Vec::new(),
                reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns: 5,
                }),
                tool_results: Vec::new(),
                allow_shadowing_active_operations: false,
                curator: Default::default(),
                authorization: StoredContextAuthorization::LegacyMigration {
                    source: StoredLegacyContextSource::JcodeTextCompaction,
                },
            },
        ];
        invalid_cases.push(ContextDraftRequest {
            summary_ranges: Vec::new(),
            reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            }),
            tool_results: Vec::new(),
            allow_shadowing_active_operations: false,
            curator: ContextCuratorRunConfig {
                selection: Some(ContextCuratorSelection {
                    provider: Some(String::new()),
                    ..ContextCuratorSelection::default()
                }),
                ..ContextCuratorRunConfig::default()
            },
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
        });
        invalid_cases.push(ContextDraftRequest {
            summary_ranges: Vec::new(),
            reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            }),
            tool_results: Vec::new(),
            allow_shadowing_active_operations: false,
            curator: ContextCuratorRunConfig {
                transaction_instructions: "x".repeat(CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS + 1),
                ..ContextCuratorRunConfig::default()
            },
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
        });
        invalid_cases.push(ContextDraftRequest {
            summary_ranges: Vec::new(),
            reasoning: Some(ContextReasoningSelectionRequest::KeepLatestAssistantTurns {
                protected_recent_assistant_turns: 5,
            }),
            tool_results: Vec::new(),
            allow_shadowing_active_operations: false,
            curator: ContextCuratorRunConfig {
                range_instructions: (0..33)
                    .map(|index| ContextCuratorRangeInstructions {
                        range: ContextMessageRangeSelection {
                            start_message_id: format!("start-{index}"),
                            end_message_id: format!("end-{index}"),
                        },
                        instructions: "x".repeat(CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS),
                    })
                    .collect(),
                ..ContextCuratorRunConfig::default()
            },
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
        });
        for (index, request) in invalid_cases.into_iter().enumerate() {
            let id = 70 + index as u64;
            handle_prepare_context_draft(id, request, &session_id, &agent, &service, false, &tx);
            assert!(matches!(
                receive_now(&mut rx),
                ServerEvent::ContextRequestRejected {
                    id: actual,
                    request: ContextRequestKind::PrepareDraft,
                    error: ContextServiceError::InvalidSelection(_),
                    ..
                } if actual == id
            ));
        }
        assert!(service.lock_store().entries.is_empty());
    }
}
