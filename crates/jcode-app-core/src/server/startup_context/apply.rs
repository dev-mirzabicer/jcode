use super::*;
use crate::agent::Agent;
use crate::protocol::{
    STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS, STARTUP_CONTEXT_PATH_MAX_CHARS,
    STARTUP_CONTEXT_SELECTION_MAX_ENTRIES, StartupContextApplyPhase, StartupContextApplyStatus,
    StartupContextApplyTargetState, StartupContextSelectionEntrySnapshot,
    StartupContextSelectionInput, StartupContextSelectionPreview,
};
use crate::session::{
    DurableStartupContextSessionPersistence, PreparedStartupContextSessionApply, Session,
    StartupContextSessionApplyError, StartupContextSessionApplyOutcome,
    StartupContextSessionPersistence,
};
use jcode_base::startup_context::{
    ActiveProject, PreparedStartupEntry, StartupFailurePolicy, StartupFileSpecId,
    StartupPathClassification, StartupProjectPlanTransition, StartupSelectionEntry,
    StartupSelectionInput as DomainSelectionInput,
    StartupSelectionPreview as DomainSelectionPreview,
};
use jcode_session_types::StoredStartupFileSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

const APPLY_RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub(in crate::server) struct ApplySelectionRequest {
    pub(in crate::server) lease: LeaseRequest,
    pub(in crate::server) operation_id: String,
    pub(in crate::server) selection: Vec<StartupContextSelectionInput>,
    pub(in crate::server) save_project_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedStartupApplyRecord {
    schema_version: u32,
    operation_id: String,
    wire_request_fingerprint: String,
    session_id: String,
    lease_id: String,
    owner_connection_id: String,
    project_active_root: String,
    project_key_digest: String,
    expected_plan_revision: u64,
    selection: Vec<StoredStartupFileSpec>,
    save_project_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_transition: Option<StartupProjectPlanTransition>,
    phase: StartupContextApplyPhase,
    session_target: StartupContextApplyTargetState,
    project_default_target: StartupContextApplyTargetState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared_session: Option<PreparedStartupContextSessionApply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch_id: Option<String>,
    #[serde(default)]
    file_count: usize,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<StartupContextFailure>,
}

impl PersistedStartupApplyRecord {
    fn status(&self) -> StartupContextApplyStatus {
        StartupContextApplyStatus {
            operation_id: self.operation_id.clone(),
            session_id: self.session_id.clone(),
            phase: self.phase,
            session_target: self.session_target.clone(),
            project_default_target: self.project_default_target.clone(),
            batch_id: self.batch_id.clone(),
            file_count: self.file_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
            failure: self.failure.clone(),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.phase,
            StartupContextApplyPhase::Succeeded
                | StartupContextApplyPhase::Failed
                | StartupContextApplyPhase::Canceled
        )
    }

    fn matches_wire_request(&self, session_id: &str, fingerprint: &str) -> bool {
        self.session_id == session_id && self.wire_request_fingerprint == fingerprint
    }

    fn clear_prepared_material(&mut self) {
        self.prepared_session = None;
    }

    fn clear_recovery_material(&mut self) {
        self.lease_id.clear();
        self.owner_connection_id.clear();
        self.project_active_root.clear();
        self.project_key_digest.clear();
        self.expected_plan_revision = 0;
        self.selection.clear();
        self.plan_transition = None;
        self.prepared_session = None;
    }
}

struct ValidatedApplySelection {
    project: ActiveProject,
    preview: DomainSelectionPreview,
    stored_selection: Vec<StoredStartupFileSpec>,
    plan_transition: Option<StartupProjectPlanTransition>,
    resulting_plan_revision: u64,
}

struct ApplyClaim {
    coordinator: StartupContextCoordinator,
    operation_id: String,
    project_key_digest: String,
}

impl Drop for ApplyClaim {
    fn drop(&mut self) {
        let mut state = self.coordinator.lock_state();
        state.active_applies.remove(&self.operation_id);
        let remove_project = if let Some(count) = state
            .active_apply_projects
            .get_mut(&self.project_key_digest)
        {
            *count = count.saturating_sub(1);
            *count == 0
        } else {
            false
        };
        if remove_project {
            state.active_apply_projects.remove(&self.project_key_digest);
        }
    }
}

impl StartupContextCoordinator {
    pub(in crate::server) async fn preview_selection(
        &self,
        request: LeaseRequest,
        selection: Vec<StartupContextSelectionInput>,
    ) -> Result<StartupContextSelectionPreview, StartupContextFailure> {
        validate_wire_selection(&selection, StartupContextOperation::PreviewSelection)?;
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let validated = coordinator.validate_apply_selection_sync(
                &request,
                &selection,
                false,
                StartupContextOperation::PreviewSelection,
            )?;
            coordinator.selection_preview_snapshot(
                &validated.project,
                validated.resulting_plan_revision,
                &validated.preview,
            )
        })
        .await
        .map_err(|error| {
            failure(
                StartupContextOperation::PreviewSelection,
                StartupContextFailureKind::Internal,
                format!("Startup Context selection preview task failed: {error}"),
                true,
            )
        })?
    }

    pub(in crate::server) async fn apply_selection(
        &self,
        request: ApplySelectionRequest,
        agent: Arc<Mutex<Agent>>,
        busy_hint: bool,
    ) -> Result<StartupContextApplyStatus, StartupContextFailure> {
        validate_operation_id(
            &request.operation_id,
            StartupContextOperation::ApplySelection,
        )?;
        validate_wire_selection(&request.selection, StartupContextOperation::ApplySelection)?;

        let wire_fingerprint = wire_apply_request_fingerprint(
            &request.lease.owner_session_id,
            request.lease.expected_plan_revision.unwrap_or_default(),
            &request.selection,
            request.save_project_default,
        )?;
        let _claim =
            match self.claim_apply(&request.operation_id, &request.lease.project_key_digest) {
                Some(claim) => claim,
                None => {
                    return match self.load_apply_record(&request.operation_id) {
                        Ok(record)
                            if record.matches_wire_request(
                                &request.lease.owner_session_id,
                                &wire_fingerprint,
                            ) =>
                        {
                            Ok(record.status())
                        }
                        Ok(_) => Err(failure(
                            StartupContextOperation::ApplySelection,
                            StartupContextFailureKind::OperationConflict,
                            "Startup Context operation ID is already bound to a different request",
                            false,
                        )),
                        Err(error) if error.kind == StartupContextFailureKind::ApplyNotFound => {
                            Ok(StartupContextApplyStatus {
                                operation_id: request.operation_id.clone(),
                                session_id: request.lease.owner_session_id.clone(),
                                phase: StartupContextApplyPhase::Applying,
                                session_target: StartupContextApplyTargetState::Pending,
                                project_default_target: if request.save_project_default {
                                    StartupContextApplyTargetState::Pending
                                } else {
                                    StartupContextApplyTargetState::NotRequested
                                },
                                batch_id: None,
                                file_count: 0,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                                failure: None,
                            })
                        }
                        Err(error) => Err(error),
                    };
                }
            };

        let existing = self.load_apply_record(&request.operation_id).ok();
        if let Some(existing) = existing.as_ref() {
            if !existing.matches_wire_request(&request.lease.owner_session_id, &wire_fingerprint) {
                return Err(failure(
                    StartupContextOperation::ApplySelection,
                    StartupContextFailureKind::OperationConflict,
                    "Startup Context operation ID is already bound to a different request",
                    false,
                ));
            }
            if existing.is_terminal() || busy_hint || agent.try_lock().is_err() {
                return Ok(existing.status());
            }
        } else if let Some(other_operation) = self
            .pending_operation_ids_for_session(&request.lease.owner_session_id)
            .into_iter()
            .next()
        {
            return Err(failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::OperationConflict,
                format!(
                    "another Startup Context apply is still pending for this session ({other_operation})"
                ),
                true,
            ));
        }

        let validated = if existing.is_none() {
            let coordinator = self.clone();
            let lease = request.lease.clone();
            let selection = request.selection.clone();
            let save_project_default = request.save_project_default;
            Some(
                tokio::task::spawn_blocking(move || {
                    coordinator.validate_apply_selection_sync(
                        &lease,
                        &selection,
                        save_project_default,
                        StartupContextOperation::ApplySelection,
                    )
                })
                .await
                .map_err(|error| {
                    failure(
                        StartupContextOperation::ApplySelection,
                        StartupContextFailureKind::Internal,
                        format!("Startup Context apply validation task failed: {error}"),
                        true,
                    )
                })??,
            )
        } else {
            None
        };

        if let Some(validated) = validated.as_ref()
            && !validated.preview.is_valid()
        {
            let issues = validated
                .preview
                .issues()
                .map(domain_issue_snapshot)
                .collect::<Vec<_>>();
            return Err(failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::InvalidRequest,
                format!(
                    "Startup Context selection has {} unresolved issue(s)",
                    issues.len()
                ),
                false,
            )
            .with_issue_list(issues));
        }

        let now = Utc::now();
        let mut record = match existing {
            Some(record) => record,
            None => {
                let validated = validated.ok_or_else(|| {
                    failure(
                        StartupContextOperation::ApplySelection,
                        StartupContextFailureKind::Internal,
                        "new Startup Context apply lost its validated selection before intent persistence",
                        true,
                    )
                })?;
                PersistedStartupApplyRecord {
                    schema_version: APPLY_RECORD_SCHEMA_VERSION,
                    operation_id: request.operation_id.clone(),
                    wire_request_fingerprint: wire_fingerprint,
                    session_id: request.lease.owner_session_id.clone(),
                    lease_id: request.lease.lease_id.clone(),
                    owner_connection_id: request.lease.owner_connection_id.clone(),
                    project_active_root: path_string(
                        validated.project.active_root(),
                        StartupContextOperation::ApplySelection,
                    )?,
                    project_key_digest: request.lease.project_key_digest.clone(),
                    expected_plan_revision: request
                        .lease
                        .expected_plan_revision
                        .unwrap_or_default(),
                    selection: validated.stored_selection,
                    save_project_default: request.save_project_default,
                    plan_transition: validated.plan_transition,
                    phase: if busy_hint {
                        StartupContextApplyPhase::Queued
                    } else {
                        StartupContextApplyPhase::Applying
                    },
                    session_target: StartupContextApplyTargetState::Pending,
                    project_default_target: if request.save_project_default {
                        StartupContextApplyTargetState::Pending
                    } else {
                        StartupContextApplyTargetState::NotRequested
                    },
                    prepared_session: None,
                    batch_id: None,
                    file_count: 0,
                    created_at: now,
                    updated_at: now,
                    failure: None,
                }
            }
        };
        self.save_apply_record(&record)?;

        if busy_hint || agent.try_lock().is_err() {
            record.phase = StartupContextApplyPhase::Queued;
            record.updated_at = Utc::now();
            self.save_apply_record(&record)?;
            return Ok(record.status());
        }

        let session_snapshot = {
            let guard = agent.try_lock().map_err(|_| {
                failure(
                    StartupContextOperation::ApplySelection,
                    StartupContextFailureKind::Internal,
                    "session became busy while preparing Startup Context apply",
                    true,
                )
            })?;
            guard.startup_context_session().clone()
        };
        if record.prepared_session.is_none() {
            let coordinator = self.clone();
            record = tokio::task::spawn_blocking(move || {
                coordinator.prepare_record_for_session(record, &session_snapshot)
            })
            .await
            .map_err(|error| {
                failure(
                    StartupContextOperation::ApplySelection,
                    StartupContextFailureKind::Internal,
                    format!("Startup Context capture task failed: {error}"),
                    true,
                )
            })??;
        }
        if record.is_terminal() {
            return Ok(record.status());
        }

        let Ok(mut guard) = agent.try_lock() else {
            record.phase = StartupContextApplyPhase::Queued;
            record.clear_prepared_material();
            record.updated_at = Utc::now();
            self.save_apply_record(&record)?;
            self.remove_apply_backup(&record.operation_id);
            return Ok(record.status());
        };
        match self.commit_record(&mut record, guard.startup_context_session_mut()) {
            Ok(()) => Ok(record.status()),
            Err(error) if matches!(error.kind, StartupContextFailureKind::Recovery) => {
                Ok(record.status())
            }
            Err(error) => Err(error),
        }
    }

    pub(in crate::server) fn cancel_apply(
        &self,
        lease: LeaseRequest,
        operation_id: &str,
    ) -> Result<StartupContextApplyStatus, StartupContextFailure> {
        validate_operation_id(operation_id, StartupContextOperation::CancelApply)?;
        let _ = self.validate_lease_sync(&lease, StartupContextOperation::CancelApply)?;
        let _claim = self
            .claim_apply(operation_id, &lease.project_key_digest)
            .ok_or_else(|| {
                failure(
                    StartupContextOperation::CancelApply,
                    StartupContextFailureKind::OperationConflict,
                    "Startup Context apply is currently being committed and cannot be canceled",
                    true,
                )
            })?;
        let mut record = self.load_apply_record(operation_id)?;
        if record.session_id != lease.owner_session_id
            || record.project_key_digest != lease.project_key_digest
        {
            return Err(failure(
                StartupContextOperation::CancelApply,
                StartupContextFailureKind::LeaseOwnerMismatch,
                "Startup Context apply is owned by a different session or project",
                false,
            ));
        }
        let targets_uncommitted = matches!(
            record.session_target,
            StartupContextApplyTargetState::Pending
        ) && matches!(
            record.project_default_target,
            StartupContextApplyTargetState::Pending | StartupContextApplyTargetState::NotRequested
        );
        if record.phase != StartupContextApplyPhase::Queued || !targets_uncommitted {
            return Err(failure(
                StartupContextOperation::CancelApply,
                StartupContextFailureKind::OperationConflict,
                "only a queued Startup Context apply with no committed target can be canceled",
                false,
            ));
        }
        record.phase = StartupContextApplyPhase::Canceled;
        record.session_target = StartupContextApplyTargetState::Canceled;
        record.project_default_target = if record.save_project_default {
            StartupContextApplyTargetState::Canceled
        } else {
            StartupContextApplyTargetState::NotRequested
        };
        record.clear_recovery_material();
        record.failure = None;
        record.updated_at = Utc::now();
        self.save_apply_record(&record)?;
        self.remove_apply_backup(operation_id);
        Ok(record.status())
    }

    pub(in crate::server) fn apply_status(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<StartupContextApplyStatus, StartupContextFailure> {
        validate_operation_id(operation_id, StartupContextOperation::ApplyStatus)?;
        let record = self.load_apply_record(operation_id)?;
        if record.session_id != session_id {
            return Err(failure(
                StartupContextOperation::ApplyStatus,
                StartupContextFailureKind::ApplyNotFound,
                "Startup Context apply operation was not found for this session",
                false,
            ));
        }
        Ok(record.status())
    }

    /// Drain every durable apply for this session while the caller owns the idle Agent.
    /// This is invoked immediately before and after provider turns so a queued
    /// late batch is present before any later user prompt can be appended.
    pub(in crate::server) fn drain_pending_for_agent(
        &self,
        agent: &mut Agent,
    ) -> Vec<StartupContextApplyStatus> {
        let session_id = agent.session_id().to_string();
        let mut results = Vec::new();
        for operation_id in self.pending_operation_ids_for_session(&session_id) {
            let project_key_digest = match self.load_apply_record(&operation_id) {
                Ok(record) => record.project_key_digest,
                Err(_) => continue,
            };
            let Some(_claim) = self.claim_apply(&operation_id, &project_key_digest) else {
                continue;
            };
            let mut record = match self.load_apply_record(&operation_id) {
                Ok(record) => record,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "Could not load queued Startup Context apply {operation_id}: {}",
                        error.message
                    ));
                    continue;
                }
            };
            let _project_guard = match self.acquire_project_guard_for_record(&record) {
                Ok(guard) => guard,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "Startup Context apply {operation_id} is waiting for project ownership: {}",
                        error.message
                    ));
                    continue;
                }
            };
            if record.is_terminal() {
                continue;
            }
            if record.prepared_session.is_none() {
                match self.prepare_record_for_session(record, agent.startup_context_session()) {
                    Ok(next) => record = next,
                    Err(error) => {
                        crate::logging::warn(&format!(
                            "Could not prepare queued Startup Context apply {operation_id}: {}",
                            error.message
                        ));
                        continue;
                    }
                }
            }
            if !record.is_terminal()
                && let Err(error) =
                    self.commit_record(&mut record, agent.startup_context_session_mut())
            {
                crate::logging::warn(&format!(
                    "Startup Context apply {operation_id} requires recovery: {}",
                    error.message
                ));
            }
            results.push(record.status());
        }
        results
    }

    pub(in crate::server) fn pending_apply_count(&self, session_id: &str) -> usize {
        self.pending_operation_ids_for_session(session_id).len()
    }

    pub(in crate::server) fn recover_interrupted_transactions(&self) -> usize {
        let mut recovered = 0usize;
        let records = self.load_all_apply_records();
        let mut by_session = HashMap::<String, Vec<String>>::new();
        for record in records {
            if record.is_terminal() {
                self.remove_apply_backup(&record.operation_id);
                continue;
            }
            by_session
                .entry(record.session_id)
                .or_default()
                .push(record.operation_id);
        }
        for (session_id, operation_ids) in by_session {
            let Ok(mut session) = Session::load(&session_id) else {
                crate::logging::warn(&format!(
                    "Could not load session {session_id} while recovering Startup Context apply transactions"
                ));
                continue;
            };
            for operation_id in operation_ids {
                let project_key_digest = match self.load_apply_record(&operation_id) {
                    Ok(record) => record.project_key_digest,
                    Err(_) => continue,
                };
                let Some(_claim) = self.claim_apply(&operation_id, &project_key_digest) else {
                    continue;
                };
                let Ok(mut record) = self.load_apply_record(&operation_id) else {
                    continue;
                };
                let _project_guard = match self.acquire_project_guard_for_record(&record) {
                    Ok(guard) => guard,
                    Err(error) => {
                        crate::logging::warn(&format!(
                            "Startup Context apply {operation_id} is waiting for project ownership during restart recovery: {}",
                            error.message
                        ));
                        continue;
                    }
                };
                if record.prepared_session.is_none() {
                    match self.prepare_record_for_session(record, &session) {
                        Ok(next) => record = next,
                        Err(error) => {
                            crate::logging::warn(&format!(
                                "Could not prepare Startup Context apply {operation_id} during restart recovery: {}",
                                error.message
                            ));
                            continue;
                        }
                    }
                }
                if record.is_terminal() {
                    recovered = recovered.saturating_add(1);
                } else {
                    match self.commit_record(&mut record, &mut session) {
                        Ok(()) => recovered = recovered.saturating_add(1),
                        Err(error) => crate::logging::warn(&format!(
                            "Startup Context apply {operation_id} remains recoverable after server restart: {}",
                            error.message
                        )),
                    }
                }
            }
        }
        recovered
    }

    fn validate_apply_selection_sync(
        &self,
        request: &LeaseRequest,
        selection: &[StartupContextSelectionInput],
        save_project_default: bool,
        operation: StartupContextOperation,
    ) -> Result<ValidatedApplySelection, StartupContextFailure> {
        let context = self.validate_lease_sync(request, operation)?;
        let inputs = selection
            .iter()
            .map(domain_selection_input)
            .collect::<Result<Vec<_>, _>>()?;
        let preview = self
            .inner
            .engine
            .preview_selection(&context.project, inputs);
        for selected in preview.selected() {
            if let Some(expected_id) = selection
                .get(selected.input_index())
                .and_then(|input| input.existing_spec_id.as_deref())
                && expected_id != selected.spec().id().as_str()
            {
                return Err(failure(
                    operation,
                    StartupContextFailureKind::InvalidRequest,
                    format!(
                        "Startup Context file specification ID does not match normalized path at selection index {}",
                        selected.input_index()
                    ),
                    false,
                ));
            }
        }
        let stored_selection = if preview.is_valid() {
            self.inner
                .engine
                .store_selection(&preview)
                .map_err(|error| startup_context_error_failure(operation, error))?
        } else {
            Vec::new()
        };
        let plan_transition = if save_project_default && preview.is_valid() {
            Some(
                self.inner
                    .engine
                    .prepare_project_plan_transition(
                        &context.project,
                        context.plan.revision(),
                        &preview,
                    )
                    .map_err(|error| startup_context_error_failure(operation, error))?,
            )
        } else {
            None
        };
        let resulting_plan_revision = plan_transition
            .as_ref()
            .map_or(context.plan.revision(), |transition| {
                transition.proposed_revision()
            });
        Ok(ValidatedApplySelection {
            project: context.project,
            preview,
            stored_selection,
            plan_transition,
            resulting_plan_revision,
        })
    }

    fn selection_preview_snapshot(
        &self,
        project: &ActiveProject,
        plan_revision: u64,
        preview: &DomainSelectionPreview,
    ) -> Result<StartupContextSelectionPreview, StartupContextFailure> {
        let preparation = self
            .inner
            .engine
            .prepare_selection(project, plan_revision, preview, StartupFailurePolicy::Block)
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::PreviewSelection, error)
            })?
            .into_preparation();
        if preview.entries().len() != preparation.entries().len() {
            return Err(failure(
                StartupContextOperation::PreviewSelection,
                StartupContextFailureKind::Internal,
                "Startup Context selection preview and complete capture produced different entry counts",
                true,
            ));
        }
        let entries = preview
            .entries()
            .iter()
            .zip(preparation.entries())
            .map(|(entry, prepared)| match (entry, prepared) {
                (StartupSelectionEntry::Selected(_), PreparedStartupEntry::Issue(issue)) => {
                    Ok(StartupContextSelectionEntrySnapshot::Issue {
                        issue: domain_issue_snapshot(issue),
                    })
                }
                (
                    StartupSelectionEntry::Selected(selected),
                    PreparedStartupEntry::Captured(file),
                ) => Ok(StartupContextSelectionEntrySnapshot::Selected {
                    input_index: selected.input_index(),
                    spec_id: selected.spec().id().to_string(),
                    logical_path: path_string(
                        selected.spec().path().as_path(),
                        StartupContextOperation::PreviewSelection,
                    )?,
                    resolved_path: path_string(
                        selected.resolved_path(),
                        StartupContextOperation::PreviewSelection,
                    )?,
                    classification: path_classification(selected.classification()),
                    bytes: file.bytes(),
                    estimated_tokens: file.estimated_tokens(),
                    requires_external_approval: selected.classification()
                        == StartupPathClassification::External,
                }),
                (StartupSelectionEntry::Issue(_), PreparedStartupEntry::Issue(issue)) => {
                    Ok(StartupContextSelectionEntrySnapshot::Issue {
                        issue: domain_issue_snapshot(issue),
                    })
                }
                (StartupSelectionEntry::Issue(_), PreparedStartupEntry::Captured(_)) => {
                    Err(failure(
                        StartupContextOperation::PreviewSelection,
                        StartupContextFailureKind::Internal,
                        "Startup Context captured a file for an invalid selection entry",
                        true,
                    ))
                }
            })
            .collect::<Result<Vec<_>, StartupContextFailure>>()?;
        let batch_issues = preparation
            .batch_issues()
            .iter()
            .map(domain_issue_snapshot)
            .collect::<Vec<_>>();
        let aggregate_bytes = preparation.captured_bytes();
        let aggregate_estimated_tokens = preparation.estimated_tokens();
        let selected_count = entries
            .iter()
            .filter(|entry| matches!(entry, StartupContextSelectionEntrySnapshot::Selected { .. }))
            .count();
        let issue_count = entries.len().saturating_sub(selected_count) + batch_issues.len();
        Ok(StartupContextSelectionPreview {
            project_key_digest: project.key().digest(),
            plan_revision,
            entry_count: preview.entries().len(),
            selected_count,
            issue_count,
            aggregate_bytes,
            aggregate_estimated_tokens,
            entries,
            batch_issues,
        })
    }

    fn prepare_record_for_session(
        &self,
        mut record: PersistedStartupApplyRecord,
        session: &Session,
    ) -> Result<PersistedStartupApplyRecord, StartupContextFailure> {
        let project = self.resolve_record_project(&record)?;
        let preview = self
            .inner
            .engine
            .preview_stored_selection(&project, &record.selection)
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::ApplySelection, error)
            })?;
        if !preview.is_valid() {
            let issues = preview
                .issues()
                .map(domain_issue_snapshot)
                .collect::<Vec<_>>();
            let failure = failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::InvalidRequest,
                format!(
                    "Startup Context apply capture has {} unresolved issue(s)",
                    issues.len()
                ),
                false,
            )
            .with_issue_list(issues);
            record.phase = StartupContextApplyPhase::Failed;
            record.session_target = target_failure(&failure);
            record.project_default_target = if record.save_project_default {
                target_failure(&failure)
            } else {
                StartupContextApplyTargetState::NotRequested
            };
            record.failure = Some(failure);
            record.clear_recovery_material();
            record.updated_at = Utc::now();
            self.save_apply_record(&record)?;
            self.remove_apply_backup(&record.operation_id);
            return Ok(record);
        }
        let plan_revision = record.plan_transition.as_ref().map_or(
            record.expected_plan_revision,
            StartupProjectPlanTransition::proposed_revision,
        );
        let preparation = self
            .inner
            .engine
            .prepare_selection(
                &project,
                plan_revision,
                &preview,
                StartupFailurePolicy::Block,
            )
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::ApplySelection, error)
            })?;
        if preparation.preparation().issue_count() != 0 {
            let issues = preparation
                .preparation()
                .issues()
                .map(domain_issue_snapshot)
                .collect::<Vec<_>>();
            let failure = failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::InvalidRequest,
                format!(
                    "Startup Context apply capture has {} unresolved issue(s)",
                    issues.len()
                ),
                false,
            )
            .with_issue_list(issues);
            record.phase = StartupContextApplyPhase::Failed;
            record.session_target = target_failure(&failure);
            record.project_default_target = if record.save_project_default {
                target_failure(&failure)
            } else {
                StartupContextApplyTargetState::NotRequested
            };
            record.failure = Some(failure);
            record.clear_recovery_material();
            record.updated_at = Utc::now();
            self.save_apply_record(&record)?;
            self.remove_apply_backup(&record.operation_id);
            return Ok(record);
        }
        let transition = session
            .prepare_startup_context_session_apply(&record.operation_id, preparation)
            .map_err(session_apply_failure)?;
        record.batch_id = transition.batch_id().map(ToOwned::to_owned);
        record.prepared_session = Some(transition);
        record.phase = StartupContextApplyPhase::Applying;
        record.failure = None;
        record.updated_at = Utc::now();
        self.save_apply_record(&record)?;
        Ok(record)
    }

    fn commit_record(
        &self,
        record: &mut PersistedStartupApplyRecord,
        session: &mut Session,
    ) -> Result<(), StartupContextFailure> {
        self.commit_record_with(record, session, &DurableStartupContextSessionPersistence)
    }

    fn commit_record_with(
        &self,
        record: &mut PersistedStartupApplyRecord,
        session: &mut Session,
        persistence: &dyn StartupContextSessionPersistence,
    ) -> Result<(), StartupContextFailure> {
        let project = self.resolve_record_project(record)?;
        if let Some(transition) = record.plan_transition.as_ref()
            && !matches!(
                record.project_default_target,
                StartupContextApplyTargetState::Applied { .. }
            )
        {
            match self
                .inner
                .engine
                .commit_project_plan_transition(&project, transition)
            {
                Ok(_) => {
                    record.project_default_target = StartupContextApplyTargetState::Applied {
                        revision: Some(transition.proposed_revision()),
                    };
                    record.updated_at = Utc::now();
                    self.save_apply_record(record)?;
                }
                Err(error) => {
                    let failure = startup_context_error_failure(
                        StartupContextOperation::ApplySelection,
                        error,
                    );
                    record.phase = StartupContextApplyPhase::RecoveryRequired;
                    record.project_default_target = target_failure(&failure);
                    record.failure = Some(failure.clone());
                    record.updated_at = Utc::now();
                    self.save_apply_record(record)?;
                    return Err(recovery_failure(
                        "project default could not be committed; the durable apply will retry",
                        failure,
                    ));
                }
            }
        }

        let transition = record.prepared_session.as_ref().ok_or_else(|| {
            failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::Recovery,
                "Startup Context apply is missing its prepared session transition",
                true,
            )
        })?;
        match session.apply_prepared_startup_context_session_with(transition, persistence) {
            Ok(StartupContextSessionApplyOutcome::Applied {
                batch_id,
                file_count,
            }) => {
                record.batch_id = batch_id;
                record.file_count = file_count;
                record.session_target = StartupContextApplyTargetState::Applied { revision: None };
            }
            Ok(StartupContextSessionApplyOutcome::AlreadyApplied { batch_id }) => {
                record.batch_id = batch_id;
                record.session_target = StartupContextApplyTargetState::Applied { revision: None };
            }
            Ok(StartupContextSessionApplyOutcome::Unchanged) => {
                record.batch_id = None;
                record.file_count = 0;
                record.session_target = StartupContextApplyTargetState::Unchanged;
            }
            Err(error) => {
                let stale_session = matches!(&error, StartupContextSessionApplyError::StaleSession);
                let session_failure = session_apply_failure(error);
                record.phase = if stale_session {
                    record.clear_prepared_material();
                    StartupContextApplyPhase::Queued
                } else {
                    StartupContextApplyPhase::RecoveryRequired
                };
                record.session_target = target_failure(&session_failure);
                record.failure = Some(session_failure.clone());
                record.updated_at = Utc::now();
                self.save_apply_record(record)?;
                return Err(recovery_failure(
                    "session target could not be committed; the durable apply will complete forward without rolling back an already committed project default",
                    session_failure,
                ));
            }
        }

        record.phase = StartupContextApplyPhase::Succeeded;
        if record.plan_transition.is_none() {
            record.project_default_target = StartupContextApplyTargetState::NotRequested;
        }
        record.failure = None;
        self.advance_live_lease_revision(record);
        record.clear_recovery_material();
        record.updated_at = Utc::now();
        self.save_apply_record(record)?;
        self.remove_apply_backup(&record.operation_id);
        Ok(())
    }

    fn resolve_record_project(
        &self,
        record: &PersistedStartupApplyRecord,
    ) -> Result<ActiveProject, StartupContextFailure> {
        let project = self
            .inner
            .engine
            .resolve_project(Path::new(&record.project_active_root))
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::ApplySelection, error)
            })?;
        if project.key().digest() != record.project_key_digest {
            return Err(failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::Recovery,
                "Startup Context apply project identity no longer matches its recovery record",
                false,
            ));
        }
        Ok(project)
    }

    fn claim_apply(&self, operation_id: &str, project_key_digest: &str) -> Option<ApplyClaim> {
        let mut state = self.lock_state();
        if !state.active_applies.insert(operation_id.to_string()) {
            return None;
        }
        *state
            .active_apply_projects
            .entry(project_key_digest.to_string())
            .or_default() += 1;
        Some(ApplyClaim {
            coordinator: self.clone(),
            operation_id: operation_id.to_string(),
            project_key_digest: project_key_digest.to_string(),
        })
    }

    fn advance_live_lease_revision(&self, record: &PersistedStartupApplyRecord) {
        let Some(transition) = record.plan_transition.as_ref() else {
            return;
        };
        let mut state = self.lock_state();
        let Some(lease) = state.leases.get_mut(&record.project_key_digest) else {
            return;
        };
        if lease.lease_id == record.lease_id
            && lease.owner_session_id == record.session_id
            && lease.owner_connection_id == record.owner_connection_id
        {
            lease.plan_revision = transition.proposed_revision();
        }
    }

    fn acquire_project_guard_for_record(
        &self,
        record: &PersistedStartupApplyRecord,
    ) -> Result<Option<CrossProcessEditorGuard>, StartupContextFailure> {
        {
            let mut state = self.lock_state();
            expire_locked(&mut state, self.inner.lease_duration);
            if let Some(lease) = state.leases.get(&record.project_key_digest) {
                if lease.lease_id == record.lease_id
                    && lease.owner_session_id == record.session_id
                    && lease.owner_connection_id == record.owner_connection_id
                {
                    return Ok(None);
                }
                return Err(failure(
                    StartupContextOperation::ApplySelection,
                    StartupContextFailureKind::LeaseBusy,
                    "another live Startup Context editor owns this project while the queued apply waits",
                    true,
                ));
            }
        }

        let now = Utc::now();
        let lease_id = format!(
            "startup_apply_recovery_{:x}",
            Sha256::digest(record.operation_id.as_bytes())
        );
        let metadata = EditorOwnerMetadata {
            schema_version: OWNER_METADATA_SCHEMA_VERSION,
            project_key_digest: record.project_key_digest.clone(),
            lease_id,
            server_id: self.inner.server_id.clone(),
            server_name: self.inner.server_name.clone(),
            session_id: record.session_id.clone(),
            connection_id: "startup-apply-recovery".to_string(),
            pid: std::process::id(),
            process_start_identity: self.inner.process_start_identity.clone(),
            acquired_at: now,
            renewed_at: now,
            expires_at: now + chrono_duration(self.inner.lease_duration),
        };
        let paths = self.ownership_paths(&record.project_key_digest);
        match CrossProcessEditorGuard::try_acquire(&paths, &metadata)? {
            GuardAcquireOutcome::Acquired(guard) => Ok(Some(guard)),
            GuardAcquireOutcome::Busy(_) => Err(failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::LeaseBusy,
                "another Jcode server owns this project while the queued Startup Context apply waits",
                true,
            )),
        }
    }

    fn apply_record_path(&self, operation_id: &str) -> PathBuf {
        let digest = Sha256::digest(operation_id.as_bytes());
        self.inner.transactions_dir.join(format!("{digest:x}.json"))
    }

    fn save_apply_record(
        &self,
        record: &PersistedStartupApplyRecord,
    ) -> Result<(), StartupContextFailure> {
        #[cfg(test)]
        {
            let remaining = self
                .inner
                .apply_record_fail_after
                .load(AtomicOrdering::SeqCst);
            if remaining > 0
                && self
                    .inner
                    .apply_record_fail_after
                    .fetch_sub(1, AtomicOrdering::SeqCst)
                    == 1
            {
                return Err(failure(
                    StartupContextOperation::ApplySelection,
                    StartupContextFailureKind::Recovery,
                    "injected Startup Context apply recovery-record persistence failure",
                    true,
                ));
            }
        }
        let path = self.apply_record_path(&record.operation_id);
        crate::storage::write_json_secret(&path, record).map_err(|error| {
            failure(
                StartupContextOperation::ApplySelection,
                StartupContextFailureKind::Recovery,
                format!("could not persist Startup Context apply recovery record: {error}"),
                true,
            )
        })
    }

    fn load_apply_record(
        &self,
        operation_id: &str,
    ) -> Result<PersistedStartupApplyRecord, StartupContextFailure> {
        let path = self.apply_record_path(operation_id);
        if !path.exists() {
            return Err(failure(
                StartupContextOperation::ApplyStatus,
                StartupContextFailureKind::ApplyNotFound,
                "Startup Context apply operation was not found",
                false,
            ));
        }
        let record =
            crate::storage::read_json::<PersistedStartupApplyRecord>(&path).map_err(|error| {
                failure(
                    StartupContextOperation::ApplyStatus,
                    StartupContextFailureKind::Recovery,
                    format!("could not read Startup Context apply recovery record: {error}"),
                    true,
                )
            })?;
        if record.schema_version != APPLY_RECORD_SCHEMA_VERSION
            || record.operation_id != operation_id
        {
            return Err(failure(
                StartupContextOperation::ApplyStatus,
                StartupContextFailureKind::Recovery,
                "Startup Context apply recovery record identity or schema is invalid",
                false,
            ));
        }
        Ok(record)
    }

    fn load_all_apply_records(&self) -> Vec<PersistedStartupApplyRecord> {
        let Ok(entries) = std::fs::read_dir(&self.inner.transactions_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .filter_map(|entry| {
                let path = entry.path();
                match crate::storage::read_json::<PersistedStartupApplyRecord>(&path) {
                    Ok(record)
                        if record.schema_version == APPLY_RECORD_SCHEMA_VERSION
                            && self.apply_record_path(&record.operation_id) == path =>
                    {
                        Some(record)
                    }
                    Ok(record) => {
                        crate::logging::warn(&format!(
                            "Ignoring invalid Startup Context apply recovery record {} (schema={}, operation_id={})",
                            path.display(),
                            record.schema_version,
                            record.operation_id
                        ));
                        None
                    }
                    Err(error) => {
                        crate::logging::warn(&format!(
                            "Could not read Startup Context apply recovery record {}: {error}",
                            path.display()
                        ));
                        None
                    }
                }
            })
            .collect()
    }

    fn pending_operation_ids_for_session(&self, session_id: &str) -> Vec<String> {
        self.load_all_apply_records()
            .into_iter()
            .filter(|record| record.session_id == session_id && !record.is_terminal())
            .map(|record| record.operation_id)
            .collect()
    }

    fn remove_apply_backup(&self, operation_id: &str) {
        let path = self.apply_record_path(operation_id);
        let _ = std::fs::remove_file(path.with_extension("bak"));
    }

    #[cfg(test)]
    fn fail_apply_record_save_on_nth(&self, save_number: usize) {
        self.inner
            .apply_record_fail_after
            .store(save_number, AtomicOrdering::SeqCst);
    }
}

fn validate_operation_id(
    operation_id: &str,
    operation: StartupContextOperation,
) -> Result<(), StartupContextFailure> {
    if operation_id.trim().is_empty() {
        return Err(failure(
            operation,
            StartupContextFailureKind::InvalidRequest,
            "Startup Context operation ID cannot be blank",
            false,
        ));
    }
    validate_bounded_text(
        operation_id,
        STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS,
        operation,
        "operation ID",
    )
}

fn validate_wire_selection(
    selection: &[StartupContextSelectionInput],
    operation: StartupContextOperation,
) -> Result<(), StartupContextFailure> {
    if selection.len() > STARTUP_CONTEXT_SELECTION_MAX_ENTRIES {
        return Err(failure(
            operation,
            StartupContextFailureKind::InvalidRequest,
            format!(
                "Startup Context selection contains {} entries, exceeding the {} entry limit",
                selection.len(),
                STARTUP_CONTEXT_SELECTION_MAX_ENTRIES
            ),
            false,
        ));
    }
    for input in selection {
        validate_bounded_text(
            &input.path,
            STARTUP_CONTEXT_PATH_MAX_CHARS,
            operation,
            "selection path",
        )?;
        if let Some(id) = input.existing_spec_id.as_deref() {
            validate_bounded_text(
                id,
                STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS,
                operation,
                "file specification ID",
            )?;
        }
        if let Some(target) = input.approved_external_target.as_deref() {
            validate_bounded_text(
                target,
                STARTUP_CONTEXT_PATH_MAX_CHARS,
                operation,
                "approved external target",
            )?;
        }
    }
    Ok(())
}

fn domain_selection_input(
    input: &StartupContextSelectionInput,
) -> Result<DomainSelectionInput, StartupContextFailure> {
    if let Some(existing) = input.existing_spec_id.as_ref() {
        let _ = StartupFileSpecId::parse(existing.clone()).map_err(|error| {
            startup_context_error_failure(StartupContextOperation::PreviewSelection, error)
        })?;
    }
    let mut domain = DomainSelectionInput::new(PathBuf::from(&input.path));
    if let Some(target) = input.approved_external_target.as_ref() {
        domain = domain.with_external_approval(PathBuf::from(target));
    }
    Ok(domain)
}

fn wire_apply_request_fingerprint(
    session_id: &str,
    expected_plan_revision: u64,
    selection: &[StartupContextSelectionInput],
    save_project_default: bool,
) -> Result<String, StartupContextFailure> {
    let bytes = serde_json::to_vec(&(
        session_id,
        expected_plan_revision,
        selection,
        save_project_default,
    ))
    .map_err(|error| {
        failure(
            StartupContextOperation::ApplySelection,
            StartupContextFailureKind::Internal,
            format!("could not fingerprint Startup Context apply request: {error}"),
            false,
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn target_failure(failure: &StartupContextFailure) -> StartupContextApplyTargetState {
    StartupContextApplyTargetState::Failed {
        message: bounded_string(&failure.message, MAX_FAILURE_MESSAGE_CHARS),
        retryable: failure.retryable,
    }
}

fn session_apply_failure(error: StartupContextSessionApplyError) -> StartupContextFailure {
    let retryable = matches!(
        &error,
        StartupContextSessionApplyError::Persistence(_)
            | StartupContextSessionApplyError::StaleSession
    );
    let kind = match &error {
        StartupContextSessionApplyError::BlockedPreparation { .. }
        | StartupContextSessionApplyError::DiagnosticPreparationUnsupported
        | StartupContextSessionApplyError::InvalidPreparation { .. }
        | StartupContextSessionApplyError::SessionMismatch => {
            StartupContextFailureKind::InvalidRequest
        }
        StartupContextSessionApplyError::InvalidExistingReceipt { .. }
        | StartupContextSessionApplyError::Instruction(_)
        | StartupContextSessionApplyError::StaleSession
        | StartupContextSessionApplyError::InvalidContextProjection { .. }
        | StartupContextSessionApplyError::Persistence(_) => StartupContextFailureKind::Recovery,
    };
    failure(
        StartupContextOperation::ApplySelection,
        kind,
        error.to_string(),
        retryable,
    )
}

fn recovery_failure(message: &str, source: StartupContextFailure) -> StartupContextFailure {
    failure(
        StartupContextOperation::ApplySelection,
        StartupContextFailureKind::Recovery,
        format!("{message}: {}", source.message),
        true,
    )
}

trait FailureIssueListExt {
    fn with_issue_list(self, issues: Vec<StartupContextFileIssueSnapshot>) -> Self;
}

impl FailureIssueListExt for StartupContextFailure {
    fn with_issue_list(mut self, issues: Vec<StartupContextFileIssueSnapshot>) -> Self {
        self.issues = issues;
        self
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
