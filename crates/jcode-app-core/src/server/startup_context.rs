use super::ServerIdentity;
use crate::message::ContentBlock;
use crate::protocol::{
    STARTUP_CONTEXT_DIRECTORY_DEFAULT_PAGE_SIZE, STARTUP_CONTEXT_DIRECTORY_MAX_PAGE_SIZE,
    STARTUP_CONTEXT_FILE_DETAIL_DEFAULT_MAX_CHARS, STARTUP_CONTEXT_FILE_DETAIL_MAX_CHARS,
    STARTUP_CONTEXT_FILE_PREVIEW_DEFAULT_MAX_CHARS, STARTUP_CONTEXT_FILE_PREVIEW_MAX_CHARS,
    STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS, STARTUP_CONTEXT_PATH_MAX_CHARS,
    STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES, STARTUP_CONTEXT_PROTOCOL_VERSION,
    STARTUP_CONTEXT_QUERY_MAX_CHARS, STARTUP_CONTEXT_SEARCH_DEFAULT_MAX_RESULTS,
    STARTUP_CONTEXT_SEARCH_MAX_RESULTS, STARTUP_CONTEXT_STATUS_DEFAULT_PAGE_SIZE,
    STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE, ServerEvent, StartupContextBatchKind,
    StartupContextCompactStatus, StartupContextDeliveryState, StartupContextDirectoryEntry,
    StartupContextDirectoryEntryKind, StartupContextDirectoryPage, StartupContextEditorSnapshot,
    StartupContextFailure, StartupContextFailureKind, StartupContextFileDetail,
    StartupContextFileIssueKind, StartupContextFileIssueSnapshot, StartupContextFilePreview,
    StartupContextFileReceiptSnapshot, StartupContextLeaseAvailability,
    StartupContextLeaseOwnerSnapshot, StartupContextLeaseSnapshot, StartupContextObservedState,
    StartupContextOperation, StartupContextPathClassification, StartupContextPlanEntrySnapshot,
    StartupContextProjectKind, StartupContextProjectSnapshot, StartupContextSearchResults,
    StartupContextStatusSnapshot, StartupContextStatusState, StartupContextTargetType,
    StartupContextUnsupportedContent,
};
use crate::session::Session;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jcode_base::startup_context::{
    ActiveProject, ProjectKey, StartupBrowserEntry, StartupBrowserEntryKind, StartupBrowserError,
    StartupContext, StartupContextError, StartupFileIssue, StartupFileIssueKind,
    StartupPathClassification, StartupProjectPlan, StartupTargetType, StartupUnsupportedContent,
};
use jcode_session_types::{
    StoredStartupBatchDeliveryState, StoredStartupBatchKind, StoredStartupContextReceipt,
    StoredStartupContextState, StoredStartupFileIssue, StoredStartupFileIssueKind,
    StoredStartupObservedState, StoredStartupPathClassification, StoredStartupTargetType,
    StoredStartupUnsupportedContent,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
#[cfg(any(not(unix), test))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const DEFAULT_EDITOR_LEASE_SECS: u64 = 90;
const DEFAULT_EDITOR_REAPER_SECS: u64 = 15;
const MAX_ACTIVE_SEARCHES: usize = 128;
const MAX_SEARCHES_PER_CONNECTION: usize = 4;
const MAX_FAILURE_MESSAGE_CHARS: usize = 4 * 1024;
const MAX_OWNER_METADATA_BYTES: u64 = 64 * 1024;
const OWNER_METADATA_SCHEMA_VERSION: u32 = 1;

pub(super) mod apply;

#[derive(Clone)]
pub(super) struct StartupContextCoordinator {
    inner: Arc<CoordinatorInner>,
}

struct CoordinatorInner {
    engine: StartupContext,
    ownership_dir: PathBuf,
    transactions_dir: PathBuf,
    server_id: String,
    server_name: String,
    process_start_identity: String,
    lease_duration: Duration,
    state: StdMutex<CoordinatorState>,
}

#[derive(Default)]
struct CoordinatorState {
    leases: HashMap<String, EditorLeaseRecord>,
    searches: HashMap<(String, u64), Arc<AtomicBool>>,
    active_applies: std::collections::HashSet<String>,
    active_apply_projects: HashMap<String, usize>,
}

struct EditorLeaseRecord {
    lease_id: String,
    project: ActiveProject,
    owner_session_id: String,
    owner_connection_id: String,
    server_name: String,
    plan_revision: u64,
    acquired_at: DateTime<Utc>,
    renewed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    renewed_instant: Instant,
    guard: CrossProcessEditorGuard,
}

#[derive(Clone)]
pub(super) struct StartupContextSessionSnapshot {
    pub(super) session_id: String,
    pub(super) working_dir: Option<String>,
    pub(super) receipt: Option<StoredStartupContextReceipt>,
}

impl StartupContextSessionSnapshot {
    pub(super) fn from_session(session: &Session) -> Self {
        Self {
            session_id: session.id.clone(),
            working_dir: session.working_dir.clone(),
            receipt: session.startup_context.clone(),
        }
    }
}

pub(super) enum OpenEditorOutcome {
    Opened(StartupContextEditorSnapshot),
    Busy {
        project: StartupContextProjectSnapshot,
        owner: Option<StartupContextLeaseOwnerSnapshot>,
    },
}

#[derive(Clone)]
pub(super) struct LeaseRequest {
    lease_id: String,
    project_key_digest: String,
    expected_plan_revision: Option<u64>,
    owner_session_id: String,
    owner_connection_id: String,
}

pub(super) struct FileDetailRequest<'a> {
    pub(super) batch_id: &'a str,
    pub(super) spec_id: &'a str,
    pub(super) message_id: &'a str,
    pub(super) expected_sha256: &'a str,
    pub(super) start_char: usize,
    pub(super) max_chars: Option<usize>,
}

#[derive(Clone)]
struct LeaseContext {
    project: ActiveProject,
    plan: StartupProjectPlan,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct EditorOwnerMetadata {
    schema_version: u32,
    project_key_digest: String,
    lease_id: String,
    server_id: String,
    server_name: String,
    session_id: String,
    connection_id: String,
    pid: u32,
    process_start_identity: String,
    acquired_at: DateTime<Utc>,
    renewed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

struct OwnershipPaths {
    lock: PathBuf,
    owner: PathBuf,
}

struct CrossProcessEditorGuard {
    #[cfg(unix)]
    _file: File,
    owner_path: PathBuf,
    lease_id: String,
    #[cfg(not(unix))]
    recovery_key: String,
}

impl Drop for CrossProcessEditorGuard {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        let Ok(_recovery) = FallbackRecoveryGuard::acquire(&self.recovery_key) else {
            return;
        };
        if read_owner_metadata(&self.owner_path)
            .as_ref()
            .is_some_and(|owner| owner.lease_id == self.lease_id)
        {
            let _ = std::fs::remove_file(&self.owner_path);
            let _ = std::fs::remove_file(self.owner_path.with_extension("bak"));
        }
    }
}

impl StartupContextCoordinator {
    pub(super) fn new(identity: &ServerIdentity) -> Self {
        let coordinator = Self::from_durable_state_dir(
            crate::storage::durable_state_dir(),
            identity.id.clone(),
            identity.name.clone(),
            Duration::from_secs(DEFAULT_EDITOR_LEASE_SECS),
        );
        let recovered = coordinator.recover_interrupted_transactions();
        if recovered > 0 {
            crate::logging::info(&format!(
                "Recovered {recovered} interrupted Startup Context apply transaction(s)"
            ));
        }
        coordinator
    }

    fn from_durable_state_dir(
        durable_state_dir: PathBuf,
        server_id: String,
        server_name: String,
        lease_duration: Duration,
    ) -> Self {
        let engine = StartupContext::from_durable_state_dir(durable_state_dir.clone());
        Self {
            inner: Arc::new(CoordinatorInner {
                engine,
                ownership_dir: durable_state_dir
                    .join("startup-context")
                    .join("editor-leases"),
                transactions_dir: durable_state_dir
                    .join("startup-context")
                    .join("apply-transactions"),
                server_id,
                server_name,
                process_start_identity: current_process_start_identity(),
                lease_duration,
                state: StdMutex::new(CoordinatorState::default()),
            }),
        }
    }

    #[cfg(test)]
    fn for_test(durable_state_dir: PathBuf, server_name: &str, lease_duration: Duration) -> Self {
        Self::from_durable_state_dir(
            durable_state_dir,
            format!("test-{server_name}"),
            server_name.to_string(),
            lease_duration,
        )
    }

    pub(super) fn reaper_interval() -> Duration {
        Duration::from_secs(DEFAULT_EDITOR_REAPER_SECS)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CoordinatorState> {
        match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                crate::logging::warn(
                    "Recovering poisoned Startup Context coordinator state after an interrupted owner task",
                );
                self.inner.state.clear_poison();
                poisoned.into_inner()
            }
        }
    }

    pub(super) fn expire_abandoned_leases(&self) -> usize {
        let mut state = self.lock_state();
        expire_locked(&mut state, self.inner.lease_duration)
    }

    pub(super) fn release_connection(&self, connection_id: &str) -> usize {
        let mut state = self.lock_state();
        cancel_connection_searches_locked(&state, connection_id);
        let before = state.leases.len();
        state
            .leases
            .retain(|_, lease| lease.owner_connection_id != connection_id);
        before.saturating_sub(state.leases.len())
    }

    pub(super) async fn compact_status(
        &self,
        session: StartupContextSessionSnapshot,
    ) -> StartupContextCompactStatus {
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || coordinator.compact_status_sync(session))
            .await
            .unwrap_or_else(|error| {
                error_compact_status(
                    "unknown".to_string(),
                    failure(
                        StartupContextOperation::Status,
                        StartupContextFailureKind::Internal,
                        format!("Startup Context status task failed: {error}"),
                        true,
                    ),
                )
            })
    }

    fn compact_status_sync(
        &self,
        session: StartupContextSessionSnapshot,
    ) -> StartupContextCompactStatus {
        let Some(working_dir) = session.working_dir.as_deref() else {
            return error_compact_status(
                session.session_id,
                failure(
                    StartupContextOperation::Status,
                    StartupContextFailureKind::ProjectIdentity,
                    "session has no bound working directory",
                    false,
                ),
            );
        };
        let project = match self.inner.engine.resolve_project(working_dir) {
            Ok(project) => project,
            Err(error) => {
                return error_compact_status(
                    session.session_id,
                    startup_context_error_failure(StartupContextOperation::Status, error),
                );
            }
        };
        let loaded_plan = match self.inner.engine.load_project_plan(&project) {
            Ok(plan) => plan,
            Err(error) => {
                return error_compact_status_with_project(
                    session.session_id,
                    project_snapshot(&project),
                    startup_context_error_failure(StartupContextOperation::Status, error),
                );
            }
        };
        let lease = self.lease_availability(&project);
        let pending_apply_count = self.pending_apply_count(&session.session_id);
        let mut status = compact_status_from_parts(
            session.session_id,
            &project,
            loaded_plan.plan(),
            session.receipt.as_ref(),
            lease,
        );
        status.pending_update_count = status
            .pending_update_count
            .saturating_add(pending_apply_count);
        status
    }

    pub(super) async fn status_snapshot(
        &self,
        session: StartupContextSessionSnapshot,
        file_page_start: usize,
        file_page_size: Option<usize>,
        issue_page_start: usize,
        issue_page_size: Option<usize>,
    ) -> StartupContextStatusSnapshot {
        let compact = self.compact_status(session.clone()).await;
        let file_page_size = bounded_page_size(
            file_page_size,
            STARTUP_CONTEXT_STATUS_DEFAULT_PAGE_SIZE,
            STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE,
        );
        let issue_page_size = bounded_page_size(
            issue_page_size,
            STARTUP_CONTEXT_STATUS_DEFAULT_PAGE_SIZE,
            STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE,
        );
        let receipt = session.receipt.as_ref();
        let all_files = receipt
            .into_iter()
            .flat_map(|receipt| receipt.batches.iter())
            .flat_map(|batch| {
                batch.files.iter().map(move |file| {
                    stored_file_receipt_snapshot(
                        batch.id.as_str(),
                        batch.kind,
                        batch.delivery_state,
                        file,
                    )
                })
            })
            .collect::<Vec<_>>();
        let all_issues = receipt
            .into_iter()
            .flat_map(|receipt| receipt.blocked_issues.iter())
            .map(stored_issue_snapshot)
            .collect::<Vec<_>>();
        let (file_page_start, file_page_end, next_file_page_start, files) =
            page(&all_files, file_page_start, file_page_size);
        let (issue_page_start, issue_page_end, next_issue_page_start, issues) =
            page(&all_issues, issue_page_start, issue_page_size);
        StartupContextStatusSnapshot {
            compact,
            total_files: all_files.len(),
            file_page_start,
            file_page_end,
            next_file_page_start,
            files,
            total_issues: all_issues.len(),
            issue_page_start,
            issue_page_end,
            next_issue_page_start,
            issues,
        }
    }

    pub(super) async fn open_editor(
        &self,
        session_id: String,
        connection_id: String,
        working_dir: String,
    ) -> Result<OpenEditorOutcome, StartupContextFailure> {
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            coordinator.open_editor_sync(session_id, connection_id, working_dir)
        })
        .await
        .map_err(|error| {
            failure(
                StartupContextOperation::OpenEditor,
                StartupContextFailureKind::Internal,
                format!("Startup Context editor task failed: {error}"),
                true,
            )
        })?
    }

    fn open_editor_sync(
        &self,
        session_id: String,
        connection_id: String,
        working_dir: String,
    ) -> Result<OpenEditorOutcome, StartupContextFailure> {
        let project = self
            .inner
            .engine
            .resolve_project(&working_dir)
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::OpenEditor, error)
            })?;
        let digest = project.key().digest();
        let project_view = project_snapshot(&project);
        let lease_id = crate::id::new_id("startup_lease");
        let now = Utc::now();
        let expires_at = now + chrono_duration(self.inner.lease_duration);
        let metadata = EditorOwnerMetadata {
            schema_version: OWNER_METADATA_SCHEMA_VERSION,
            project_key_digest: digest.clone(),
            lease_id: lease_id.clone(),
            server_id: self.inner.server_id.clone(),
            server_name: self.inner.server_name.clone(),
            session_id: session_id.clone(),
            connection_id: connection_id.clone(),
            pid: std::process::id(),
            process_start_identity: self.inner.process_start_identity.clone(),
            acquired_at: now,
            renewed_at: now,
            expires_at,
        };

        let mut state = self.lock_state();
        expire_locked(&mut state, self.inner.lease_duration);
        if let Some(existing) = state.leases.get(&digest) {
            return Ok(OpenEditorOutcome::Busy {
                project: project_view.clone(),
                owner: Some(existing.owner_snapshot()),
            });
        }
        let paths = self.ownership_paths(&digest);
        let guard = match CrossProcessEditorGuard::try_acquire(&paths, &metadata)? {
            GuardAcquireOutcome::Acquired(guard) => guard,
            GuardAcquireOutcome::Busy(owner) => {
                return Ok(OpenEditorOutcome::Busy {
                    project: project_view.clone(),
                    owner: owner.as_ref().map(owner_snapshot),
                });
            }
        };
        let loaded_plan = self
            .inner
            .engine
            .load_project_plan(&project)
            .map_err(|error| {
                startup_context_error_failure(StartupContextOperation::OpenEditor, error)
            })?;
        let record = EditorLeaseRecord {
            lease_id: lease_id.clone(),
            project: project.clone(),
            owner_session_id: session_id,
            owner_connection_id: connection_id,
            server_name: self.inner.server_name.clone(),
            plan_revision: loaded_plan.plan().revision(),
            acquired_at: now,
            renewed_at: now,
            expires_at,
            renewed_instant: Instant::now(),
            guard,
        };
        let lease = record.snapshot();
        let plan_entries = loaded_plan
            .plan()
            .entries()
            .iter()
            .map(plan_entry_snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        let editor = StartupContextEditorSnapshot {
            lease,
            project: project_view,
            plan_revision: loaded_plan.plan().revision(),
            plan_entries,
        };
        state.leases.insert(digest, record);
        Ok(OpenEditorOutcome::Opened(editor))
    }

    pub(super) async fn renew_lease(
        &self,
        request: LeaseRequest,
    ) -> Result<StartupContextLeaseSnapshot, StartupContextFailure> {
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || coordinator.renew_lease_sync(request))
            .await
            .map_err(|error| {
                failure(
                    StartupContextOperation::RenewLease,
                    StartupContextFailureKind::Internal,
                    format!("Startup Context lease renewal task failed: {error}"),
                    true,
                )
            })?
    }

    fn renew_lease_sync(
        &self,
        request: LeaseRequest,
    ) -> Result<StartupContextLeaseSnapshot, StartupContextFailure> {
        validate_lease_request(&request, StartupContextOperation::RenewLease)?;
        let context = self.validate_lease_sync(&request, StartupContextOperation::RenewLease)?;
        let now = Utc::now();
        let expires_at = now + chrono_duration(self.inner.lease_duration);
        let mut state = self.lock_state();
        let record = state
            .leases
            .get_mut(&request.project_key_digest)
            .ok_or_else(|| lease_not_found(StartupContextOperation::RenewLease))?;
        validate_lease_record(
            record,
            &request,
            self.inner.lease_duration,
            StartupContextOperation::RenewLease,
        )?;
        let metadata = record.owner_metadata(
            &self.inner.server_id,
            &self.inner.server_name,
            &self.inner.process_start_identity,
            now,
            expires_at,
        );
        record.guard.update_owner(&metadata)?;
        record.plan_revision = context.plan.revision();
        record.renewed_at = now;
        record.expires_at = expires_at;
        record.renewed_instant = Instant::now();
        Ok(record.snapshot())
    }

    pub(super) fn close_editor(
        &self,
        request: LeaseRequest,
    ) -> Result<String, StartupContextFailure> {
        validate_lease_request(&request, StartupContextOperation::CloseEditor)?;
        let mut state = self.lock_state();
        expire_locked(&mut state, self.inner.lease_duration);
        let record = state
            .leases
            .get(&request.project_key_digest)
            .ok_or_else(|| lease_not_found(StartupContextOperation::CloseEditor))?;
        validate_lease_record(
            record,
            &request,
            self.inner.lease_duration,
            StartupContextOperation::CloseEditor,
        )?;
        let lease_id = record.lease_id.clone();
        cancel_connection_searches_locked(&state, &request.owner_connection_id);
        state.leases.remove(&request.project_key_digest);
        Ok(lease_id)
    }

    pub(super) async fn list_directory(
        &self,
        request: LeaseRequest,
        directory: String,
        page_start: usize,
        page_size: Option<usize>,
    ) -> Result<StartupContextDirectoryPage, StartupContextFailure> {
        validate_max_chars(
            &directory,
            STARTUP_CONTEXT_PATH_MAX_CHARS,
            StartupContextOperation::ListDirectory,
            "project-relative directory",
        )?;
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let context = coordinator
                .validate_lease_sync(&request, StartupContextOperation::ListDirectory)?;
            let size = bounded_page_size(
                page_size,
                STARTUP_CONTEXT_DIRECTORY_DEFAULT_PAGE_SIZE,
                STARTUP_CONTEXT_DIRECTORY_MAX_PAGE_SIZE,
            );
            let page = coordinator
                .inner
                .engine
                .list_project_directory(&context.project, Path::new(&directory), page_start, size)
                .map_err(|error| browser_failure(StartupContextOperation::ListDirectory, error))?;
            let selected = selected_path_index(&context.plan);
            Ok(StartupContextDirectoryPage {
                project_key_digest: request.project_key_digest,
                plan_revision: context.plan.revision(),
                directory: path_string(page.directory(), StartupContextOperation::ListDirectory)?,
                total_entries: page.total_entries(),
                page_start: page.page_start(),
                page_end: page.page_end(),
                next_page_start: page.next_page_start(),
                entries: page
                    .entries()
                    .iter()
                    .map(|entry| directory_entry_snapshot(entry, &selected))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .await
        .map_err(|error| {
            failure(
                StartupContextOperation::ListDirectory,
                StartupContextFailureKind::Internal,
                format!("Startup Context directory task failed: {error}"),
                true,
            )
        })?
    }

    pub(super) async fn preview_file(
        &self,
        request: LeaseRequest,
        path: String,
        start_char: usize,
        max_chars: Option<usize>,
    ) -> Result<StartupContextFilePreview, StartupContextFailure> {
        validate_bounded_text(
            &path,
            STARTUP_CONTEXT_PATH_MAX_CHARS,
            StartupContextOperation::PreviewFile,
            "preview path",
        )?;
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let context =
                coordinator.validate_lease_sync(&request, StartupContextOperation::PreviewFile)?;
            let max_chars = bounded_page_size(
                max_chars,
                STARTUP_CONTEXT_FILE_PREVIEW_DEFAULT_MAX_CHARS,
                STARTUP_CONTEXT_FILE_PREVIEW_MAX_CHARS,
            );
            let preview = coordinator
                .inner
                .engine
                .preview_current_file(&context.project, Path::new(&path), start_char, max_chars)
                .map_err(|error| browser_failure(StartupContextOperation::PreviewFile, error))?;
            Ok(StartupContextFilePreview {
                project_key_digest: request.project_key_digest,
                plan_revision: context.plan.revision(),
                logical_path: path_string(
                    preview.logical_path(),
                    StartupContextOperation::PreviewFile,
                )?,
                resolved_path: path_string(
                    preview.resolved_path(),
                    StartupContextOperation::PreviewFile,
                )?,
                classification: path_classification(preview.classification()),
                requires_external_approval: preview.classification()
                    == StartupPathClassification::External,
                sha256: preview.sha256().to_string(),
                bytes: preview.bytes(),
                estimated_tokens: preview.estimated_tokens(),
                total_chars: preview.total_chars(),
                start_char: preview.start_char(),
                end_char: preview.end_char(),
                next_start_char: preview.next_start_char(),
                truncated: preview.next_start_char().is_some(),
                content: preview.content().to_string(),
            })
        })
        .await
        .map_err(|error| {
            failure(
                StartupContextOperation::PreviewFile,
                StartupContextFailureKind::Internal,
                format!("Startup Context preview task failed: {error}"),
                true,
            )
        })?
    }

    pub(super) fn start_search(
        &self,
        request_id: u64,
        request: LeaseRequest,
        query: String,
        max_results: Option<usize>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<(), StartupContextFailure> {
        validate_bounded_text(
            &query,
            STARTUP_CONTEXT_QUERY_MAX_CHARS,
            StartupContextOperation::SearchFiles,
            "search query",
        )?;
        validate_lease_request(&request, StartupContextOperation::SearchFiles)?;
        let key = (request.owner_connection_id.clone(), request_id);
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.lock_state();
            expire_locked(&mut state, self.inner.lease_duration);
            let record = state
                .leases
                .get(&request.project_key_digest)
                .ok_or_else(|| lease_not_found(StartupContextOperation::SearchFiles))?;
            validate_lease_record(
                record,
                &request,
                self.inner.lease_duration,
                StartupContextOperation::SearchFiles,
            )?;
            if state.searches.contains_key(&key) {
                return Err(failure(
                    StartupContextOperation::SearchFiles,
                    StartupContextFailureKind::InvalidRequest,
                    "a Startup Context search with this request ID is already active",
                    false,
                ));
            }
            let connection_searches = state
                .searches
                .keys()
                .filter(|(connection_id, _)| connection_id == &request.owner_connection_id)
                .count();
            if state.searches.len() >= MAX_ACTIVE_SEARCHES
                || connection_searches >= MAX_SEARCHES_PER_CONNECTION
            {
                return Err(failure(
                    StartupContextOperation::SearchFiles,
                    StartupContextFailureKind::InvalidRequest,
                    "too many Startup Context searches are already active",
                    true,
                ));
            }
            state.searches.insert(key.clone(), Arc::clone(&cancel));
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            let blocking = coordinator.clone();
            let result = tokio::task::spawn_blocking(move || {
                let context =
                    blocking.validate_lease_sync(&request, StartupContextOperation::SearchFiles)?;
                let max_results = bounded_page_size(
                    max_results,
                    STARTUP_CONTEXT_SEARCH_DEFAULT_MAX_RESULTS,
                    STARTUP_CONTEXT_SEARCH_MAX_RESULTS,
                );
                let results = blocking
                    .inner
                    .engine
                    .search_project_files(&context.project, &query, max_results, &cancel)
                    .map_err(|error| {
                        browser_failure(StartupContextOperation::SearchFiles, error)
                    })?;
                if results.canceled() {
                    return Ok(None);
                }
                let selected = selected_path_index(&context.plan);
                Ok(Some(StartupContextSearchResults {
                    project_key_digest: request.project_key_digest,
                    plan_revision: context.plan.revision(),
                    query: results.query().to_string(),
                    visited_entries: results.visited_entries(),
                    omitted_results: results.omitted_results(),
                    truncated: results.truncated(),
                    results: results
                        .results()
                        .iter()
                        .map(|entry| directory_entry_snapshot(entry, &selected))
                        .collect::<Result<Vec<_>, StartupContextFailure>>()?,
                }))
            })
            .await;
            let was_canceled = coordinator.finish_search(&key);
            match result {
                Ok(Ok(Some(results))) if !was_canceled => {
                    emit_checked(
                        &event_tx,
                        request_id,
                        StartupContextOperation::SearchFiles,
                        ServerEvent::StartupContextSearchResults {
                            id: request_id,
                            results,
                        },
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(failure)) if !was_canceled => {
                    let _ = event_tx.send(ServerEvent::StartupContextFailed {
                        id: request_id,
                        failure,
                    });
                }
                Err(error) if !was_canceled => {
                    let _ = event_tx.send(ServerEvent::StartupContextFailed {
                        id: request_id,
                        failure: failure(
                            StartupContextOperation::SearchFiles,
                            StartupContextFailureKind::Internal,
                            format!("Startup Context search task failed: {error}"),
                            true,
                        ),
                    });
                }
                _ => {}
            }
        });
        Ok(())
    }

    pub(super) fn cancel_search(&self, connection_id: &str, request_id: u64) -> bool {
        let state = self.lock_state();
        let Some(cancel) = state.searches.get(&(connection_id.to_string(), request_id)) else {
            return false;
        };
        cancel.store(true, AtomicOrdering::Relaxed);
        true
    }

    fn finish_search(&self, key: &(String, u64)) -> bool {
        let mut state = self.lock_state();
        state
            .searches
            .remove(key)
            .is_some_and(|cancel| cancel.load(AtomicOrdering::Relaxed))
    }

    pub(super) fn file_detail(
        &self,
        session: &Session,
        request: FileDetailRequest<'_>,
    ) -> Result<StartupContextFileDetail, StartupContextFailure> {
        let FileDetailRequest {
            batch_id,
            spec_id,
            message_id,
            expected_sha256,
            start_char,
            max_chars,
        } = request;
        for (label, value) in [
            ("batch ID", batch_id),
            ("file specification ID", spec_id),
            ("message ID", message_id),
            ("SHA-256", expected_sha256),
        ] {
            validate_bounded_text(
                value,
                STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS,
                StartupContextOperation::FileDetail,
                label,
            )?;
        }
        let receipt = session.startup_context.as_ref().ok_or_else(|| {
            failure(
                StartupContextOperation::FileDetail,
                StartupContextFailureKind::ReceiptNotFound,
                "session has no Startup Context receipt",
                false,
            )
        })?;
        let batch = receipt
            .batches
            .iter()
            .find(|batch| batch.id == batch_id)
            .ok_or_else(|| {
                failure(
                    StartupContextOperation::FileDetail,
                    StartupContextFailureKind::ReceiptNotFound,
                    "Startup Context batch was not found in the session receipt",
                    false,
                )
            })?;
        let file = batch
            .files
            .iter()
            .find(|file| file.spec_id == spec_id)
            .ok_or_else(|| {
                failure(
                    StartupContextOperation::FileDetail,
                    StartupContextFailureKind::ReceiptNotFound,
                    "Startup Context file was not found in the requested batch",
                    false,
                )
            })?;
        if file.message_id != message_id {
            return Err(failure(
                StartupContextOperation::FileDetail,
                StartupContextFailureKind::MessageMismatch,
                "requested message identity does not match the receipt-owned file",
                false,
            ));
        }
        if file.sha256 != expected_sha256 {
            return Err(failure(
                StartupContextOperation::FileDetail,
                StartupContextFailureKind::DigestMismatch,
                "requested digest does not match the receipt-owned file",
                false,
            ));
        }
        let message = session
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .ok_or_else(|| {
                failure(
                    StartupContextOperation::FileDetail,
                    StartupContextFailureKind::MessageMismatch,
                    "receipt-owned Startup Context message is missing from authoritative history",
                    false,
                )
            })?;
        let content = match message.content.as_slice() {
            [ContentBlock::Text { .. }, ContentBlock::Text { text, .. }] => text,
            _ => {
                return Err(failure(
                    StartupContextOperation::FileDetail,
                    StartupContextFailureKind::MessageMismatch,
                    "receipt-owned Startup Context message has an invalid content-block shape",
                    false,
                ));
            }
        };
        let actual_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        if actual_sha256 != file.sha256 {
            return Err(failure(
                StartupContextOperation::FileDetail,
                StartupContextFailureKind::DigestMismatch,
                "authoritative Startup Context message no longer matches its receipt digest",
                false,
            ));
        }
        let max_chars = bounded_page_size(
            max_chars,
            STARTUP_CONTEXT_FILE_DETAIL_DEFAULT_MAX_CHARS,
            STARTUP_CONTEXT_FILE_DETAIL_MAX_CHARS,
        );
        let (content, total_chars, start_char, end_char, next_start_char) =
            char_chunk(content, start_char, max_chars);
        Ok(StartupContextFileDetail {
            session_id: session.id.clone(),
            batch_id: batch.id.clone(),
            spec_id: file.spec_id.clone(),
            message_id: file.message_id.clone(),
            sha256: file.sha256.clone(),
            total_chars,
            start_char,
            end_char,
            next_start_char,
            content,
        })
    }

    fn validate_lease_sync(
        &self,
        request: &LeaseRequest,
        operation: StartupContextOperation,
    ) -> Result<LeaseContext, StartupContextFailure> {
        validate_lease_request(request, operation)?;
        let project = {
            let mut state = self.lock_state();
            expire_locked(&mut state, self.inner.lease_duration);
            let record = state
                .leases
                .get(&request.project_key_digest)
                .ok_or_else(|| lease_not_found(operation))?;
            validate_lease_record(record, request, self.inner.lease_duration, operation)?;
            record.project.clone()
        };
        let plan = self
            .inner
            .engine
            .load_project_plan(&project)
            .map_err(|error| startup_context_error_failure(operation, error))?
            .into_plan();
        if let Some(expected) = request.expected_plan_revision
            && expected != plan.revision()
        {
            return Err(failure(
                operation,
                StartupContextFailureKind::StalePlanRevision,
                format!(
                    "Startup Context plan revision is stale: expected {expected}, current revision is {}",
                    plan.revision()
                ),
                true,
            ));
        }
        Ok(LeaseContext { project, plan })
    }

    fn lease_availability(&self, project: &ActiveProject) -> StartupContextLeaseAvailability {
        let digest = project.key().digest();
        let mut state = self.lock_state();
        expire_locked(&mut state, self.inner.lease_duration);
        if let Some(lease) = state.leases.get(&digest) {
            return StartupContextLeaseAvailability::Busy {
                owner: Some(lease.owner_snapshot()),
            };
        }
        drop(state);
        let paths = self.ownership_paths(&digest);
        match CrossProcessEditorGuard::probe(&paths) {
            GuardProbeOutcome::Available => StartupContextLeaseAvailability::Available,
            GuardProbeOutcome::Busy(owner) => StartupContextLeaseAvailability::Busy {
                owner: owner.as_ref().as_ref().map(owner_snapshot),
            },
        }
    }

    fn ownership_paths(&self, project_key_digest: &str) -> OwnershipPaths {
        OwnershipPaths {
            lock: self
                .inner
                .ownership_dir
                .join(format!("{project_key_digest}.lock")),
            owner: self
                .inner
                .ownership_dir
                .join(format!("{project_key_digest}.owner.json")),
        }
    }
}

#[cfg(test)]
pub(super) fn test_coordinator() -> Arc<StartupContextCoordinator> {
    let state = tempfile::tempdir().expect("temporary Startup Context test state");
    Arc::new(StartupContextCoordinator::for_test(
        state.keep(),
        "test-server",
        Duration::from_secs(30),
    ))
}

impl EditorLeaseRecord {
    fn snapshot(&self) -> StartupContextLeaseSnapshot {
        StartupContextLeaseSnapshot {
            lease_id: self.lease_id.clone(),
            project_key_digest: self.project.key().digest(),
            owner_session_id: self.owner_session_id.clone(),
            acquired_at: self.acquired_at,
            renewed_at: self.renewed_at,
            expires_at: self.expires_at,
            plan_revision: self.plan_revision,
        }
    }

    fn owner_snapshot(&self) -> StartupContextLeaseOwnerSnapshot {
        StartupContextLeaseOwnerSnapshot {
            server_name: self.server_name.clone(),
            session_id: self.owner_session_id.clone(),
            acquired_at: self.acquired_at,
            renewed_at: self.renewed_at,
            expires_at: self.expires_at,
        }
    }

    fn owner_metadata(
        &self,
        server_id: &str,
        server_name: &str,
        process_start_identity: &str,
        renewed_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> EditorOwnerMetadata {
        EditorOwnerMetadata {
            schema_version: OWNER_METADATA_SCHEMA_VERSION,
            project_key_digest: self.project.key().digest(),
            lease_id: self.lease_id.clone(),
            server_id: server_id.to_string(),
            server_name: server_name.to_string(),
            session_id: self.owner_session_id.clone(),
            connection_id: self.owner_connection_id.clone(),
            pid: std::process::id(),
            process_start_identity: process_start_identity.to_string(),
            acquired_at: self.acquired_at,
            renewed_at,
            expires_at,
        }
    }
}

enum GuardAcquireOutcome {
    Acquired(CrossProcessEditorGuard),
    Busy(Option<EditorOwnerMetadata>),
}

enum GuardProbeOutcome {
    Available,
    Busy(Box<Option<EditorOwnerMetadata>>),
}

impl CrossProcessEditorGuard {
    fn try_acquire(
        paths: &OwnershipPaths,
        metadata: &EditorOwnerMetadata,
    ) -> Result<GuardAcquireOutcome, StartupContextFailure> {
        crate::storage::ensure_dir(paths.owner.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|error| {
                failure(
                    StartupContextOperation::OpenEditor,
                    StartupContextFailureKind::PlanStorage,
                    format!("could not create Startup Context editor ownership directory: {error}"),
                    true,
                )
            })?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&paths.lock)
                .map_err(|error| {
                    ownership_io_failure(StartupContextOperation::OpenEditor, error)
                })?;
            crate::platform::set_permissions_owner_only(&paths.lock).map_err(|error| {
                ownership_io_failure(StartupContextOperation::OpenEditor, error)
            })?;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Ok(GuardAcquireOutcome::Busy(read_owner_metadata(&paths.owner)));
            }
            set_close_on_exec(&file).map_err(|error| {
                ownership_io_failure(StartupContextOperation::OpenEditor, error)
            })?;
            write_owner_metadata(&paths.owner, metadata, StartupContextOperation::OpenEditor)?;
            Ok(GuardAcquireOutcome::Acquired(Self {
                _file: file,
                owner_path: paths.owner.clone(),
                lease_id: metadata.lease_id.clone(),
            }))
        }

        #[cfg(not(unix))]
        {
            let recovery_key = metadata.project_key_digest.clone();
            let _recovery = FallbackRecoveryGuard::acquire(&recovery_key)?;
            if let Some(owner) = read_owner_metadata(&paths.owner) {
                if process_identity_matches(owner.pid, &owner.process_start_identity) {
                    return Ok(GuardAcquireOutcome::Busy(Some(owner)));
                }
                let _ = std::fs::remove_file(&paths.owner);
            } else if paths.owner.exists() {
                let _ = std::fs::remove_file(&paths.owner);
            }
            create_owner_metadata_exclusive(&paths.owner, metadata)?;
            Ok(GuardAcquireOutcome::Acquired(Self {
                owner_path: paths.owner.clone(),
                lease_id: metadata.lease_id.clone(),
                recovery_key,
            }))
        }
    }

    fn probe(paths: &OwnershipPaths) -> GuardProbeOutcome {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if !paths.lock.exists() && !paths.owner.exists() {
                return GuardProbeOutcome::Available;
            }
            let Ok(file) = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&paths.lock)
            else {
                return GuardProbeOutcome::Busy(Box::new(read_owner_metadata(&paths.owner)));
            };
            let _ = crate::platform::set_permissions_owner_only(&paths.lock);
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                GuardProbeOutcome::Available
            } else {
                GuardProbeOutcome::Busy(Box::new(read_owner_metadata(&paths.owner)))
            }
        }

        #[cfg(not(unix))]
        {
            match read_owner_metadata(&paths.owner) {
                Some(owner)
                    if process_identity_matches(owner.pid, &owner.process_start_identity) =>
                {
                    GuardProbeOutcome::Busy(Box::new(Some(owner)))
                }
                _ => GuardProbeOutcome::Available,
            }
        }
    }

    fn update_owner(&self, metadata: &EditorOwnerMetadata) -> Result<(), StartupContextFailure> {
        #[cfg(not(unix))]
        let _recovery = FallbackRecoveryGuard::acquire(&self.recovery_key)?;
        write_owner_metadata(
            &self.owner_path,
            metadata,
            StartupContextOperation::RenewLease,
        )
    }
}

#[cfg(unix)]
fn set_close_on_exec(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn write_owner_metadata(
    path: &Path,
    metadata: &EditorOwnerMetadata,
    operation: StartupContextOperation,
) -> Result<(), StartupContextFailure> {
    crate::storage::write_json_secret(path, metadata).map_err(|error| {
        failure(
            operation,
            StartupContextFailureKind::PlanStorage,
            format!("could not persist Startup Context editor owner metadata: {error}"),
            true,
        )
    })?;
    let _ = std::fs::remove_file(path.with_extension("bak"));
    Ok(())
}

#[cfg(any(not(unix), test))]
fn create_owner_metadata_exclusive(
    path: &Path,
    metadata: &EditorOwnerMetadata,
) -> Result<(), StartupContextFailure> {
    let bytes = serde_json::to_vec(metadata).map_err(|error| {
        failure(
            StartupContextOperation::OpenEditor,
            StartupContextFailureKind::Internal,
            format!("could not encode Startup Context editor owner metadata: {error}"),
            false,
        )
    })?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| ownership_io_failure(StartupContextOperation::OpenEditor, error))?;
    crate::platform::set_permissions_owner_only(path)
        .map_err(|error| ownership_io_failure(StartupContextOperation::OpenEditor, error))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| ownership_io_failure(StartupContextOperation::OpenEditor, error))
}

fn read_owner_metadata(path: &Path) -> Option<EditorOwnerMetadata> {
    if std::fs::metadata(path).ok()?.len() > MAX_OWNER_METADATA_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn owner_snapshot(metadata: &EditorOwnerMetadata) -> StartupContextLeaseOwnerSnapshot {
    StartupContextLeaseOwnerSnapshot {
        server_name: metadata.server_name.clone(),
        session_id: metadata.session_id.clone(),
        acquired_at: metadata.acquired_at,
        renewed_at: metadata.renewed_at,
        expires_at: metadata.expires_at,
    }
}

fn expire_locked(state: &mut CoordinatorState, lease_duration: Duration) -> usize {
    let before = state.leases.len();
    let active_projects = &state.active_apply_projects;
    state.leases.retain(|project_digest, lease| {
        active_projects.contains_key(project_digest)
            || lease.renewed_instant.elapsed() < lease_duration
    });
    before.saturating_sub(state.leases.len())
}

fn cancel_connection_searches_locked(state: &CoordinatorState, connection_id: &str) {
    for ((owner_connection_id, _), cancel) in &state.searches {
        if owner_connection_id == connection_id {
            cancel.store(true, AtomicOrdering::Relaxed);
        }
    }
}

fn validate_lease_record(
    record: &EditorLeaseRecord,
    request: &LeaseRequest,
    lease_duration: Duration,
    operation: StartupContextOperation,
) -> Result<(), StartupContextFailure> {
    if record.renewed_instant.elapsed() >= lease_duration {
        return Err(failure(
            operation,
            StartupContextFailureKind::LeaseExpired,
            "Startup Context editor lease expired",
            true,
        ));
    }
    if record.lease_id != request.lease_id
        || record.owner_session_id != request.owner_session_id
        || record.owner_connection_id != request.owner_connection_id
        || record.project.key().digest() != request.project_key_digest
    {
        return Err(failure(
            operation,
            StartupContextFailureKind::LeaseOwnerMismatch,
            "Startup Context editor lease does not belong to this session and connection",
            false,
        ));
    }
    if let Some(expected) = request.expected_plan_revision
        && expected != record.plan_revision
    {
        return Err(failure(
            operation,
            StartupContextFailureKind::StalePlanRevision,
            format!(
                "Startup Context editor expected plan revision {expected}, but its lease was opened at revision {}",
                record.plan_revision
            ),
            true,
        ));
    }
    Ok(())
}

fn validate_lease_request(
    request: &LeaseRequest,
    operation: StartupContextOperation,
) -> Result<(), StartupContextFailure> {
    validate_bounded_text(
        &request.lease_id,
        STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS,
        operation,
        "lease ID",
    )?;
    validate_bounded_text(
        &request.project_key_digest,
        STARTUP_CONTEXT_IDENTIFIER_MAX_CHARS,
        operation,
        "project key digest",
    )?;
    if request.project_key_digest.len() != 64
        || !request
            .project_key_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(failure(
            operation,
            StartupContextFailureKind::InvalidRequest,
            "project key digest must be exactly 64 hexadecimal characters",
            false,
        ));
    }
    Ok(())
}

fn compact_status_from_parts(
    session_id: String,
    project: &ActiveProject,
    plan: &StartupProjectPlan,
    receipt: Option<&StoredStartupContextReceipt>,
    lease: StartupContextLeaseAvailability,
) -> StartupContextCompactStatus {
    let state = receipt
        .map(|receipt| stored_status_state(&receipt.state))
        .unwrap_or(StartupContextStatusState::Unprepared);
    let receipt_file_count = receipt
        .map(|receipt| receipt.batches.iter().map(|batch| batch.files.len()).sum())
        .unwrap_or(0);
    let captured_bytes = receipt
        .map(|receipt| {
            receipt
                .batches
                .iter()
                .flat_map(|batch| batch.files.iter())
                .map(|file| file.bytes)
                .sum()
        })
        .unwrap_or(0);
    let estimated_tokens = receipt
        .map(|receipt| {
            receipt
                .batches
                .iter()
                .flat_map(|batch| batch.files.iter())
                .map(|file| file.estimated_tokens)
                .sum()
        })
        .unwrap_or(0);
    let stale_file_count = receipt
        .map(|receipt| {
            receipt
                .batches
                .iter()
                .flat_map(|batch| batch.files.iter())
                .filter(|file| {
                    !matches!(
                        file.latest_observation.state,
                        StoredStartupObservedState::Current
                    )
                })
                .count()
        })
        .unwrap_or(0);
    StartupContextCompactStatus {
        protocol_version: STARTUP_CONTEXT_PROTOCOL_VERSION,
        session_id,
        state,
        project: Some(project_snapshot(project)),
        plan_revision: plan.revision(),
        plan_entry_count: plan.entries().len(),
        receipt_plan_revision: receipt.map(|receipt| receipt.plan_revision),
        receipt_file_count,
        captured_bytes,
        estimated_tokens,
        blocked_issue_count: receipt
            .map(|receipt| receipt.blocked_issues.len())
            .unwrap_or(0),
        pending_update_count: receipt
            .map(|receipt| receipt.pending_updates.len())
            .unwrap_or(0),
        stale_file_count,
        lease,
        error: None,
    }
}

fn error_compact_status(
    session_id: String,
    error: StartupContextFailure,
) -> StartupContextCompactStatus {
    StartupContextCompactStatus {
        protocol_version: STARTUP_CONTEXT_PROTOCOL_VERSION,
        session_id,
        state: StartupContextStatusState::Error,
        project: None,
        plan_revision: 0,
        plan_entry_count: 0,
        receipt_plan_revision: None,
        receipt_file_count: 0,
        captured_bytes: 0,
        estimated_tokens: 0,
        blocked_issue_count: 0,
        pending_update_count: 0,
        stale_file_count: 0,
        lease: StartupContextLeaseAvailability::Available,
        error: Some(error),
    }
}

fn error_compact_status_with_project(
    session_id: String,
    project: StartupContextProjectSnapshot,
    error: StartupContextFailure,
) -> StartupContextCompactStatus {
    let mut status = error_compact_status(session_id, error);
    status.project = Some(project);
    status
}

fn stored_status_state(state: &StoredStartupContextState) -> StartupContextStatusState {
    match state {
        StoredStartupContextState::Empty => StartupContextStatusState::Empty,
        StoredStartupContextState::Prepared => StartupContextStatusState::Prepared,
        StoredStartupContextState::Blocked => StartupContextStatusState::Blocked,
        StoredStartupContextState::Dispatched => StartupContextStatusState::Dispatched,
        StoredStartupContextState::ProviderAccepted => StartupContextStatusState::ProviderAccepted,
        StoredStartupContextState::MetadataRepair { .. } => {
            StartupContextStatusState::MetadataRepair
        }
    }
}

fn project_snapshot(project: &ActiveProject) -> StartupContextProjectSnapshot {
    let kind = match project.key() {
        ProjectKey::Git { .. } => StartupContextProjectKind::Git,
        ProjectKey::Directory { .. } => StartupContextProjectKind::Directory,
    };
    StartupContextProjectSnapshot {
        key_digest: project.key().digest(),
        kind,
        active_root: project.active_root().to_string_lossy().into_owned(),
    }
}

fn plan_entry_snapshot(
    spec: &jcode_base::startup_context::StartupFileSpec,
) -> Result<StartupContextPlanEntrySnapshot, StartupContextFailure> {
    Ok(StartupContextPlanEntrySnapshot {
        spec_id: spec.id().to_string(),
        logical_path: path_string(spec.path().as_path(), StartupContextOperation::OpenEditor)?,
        approved_external_target: spec
            .external_approval()
            .map(|approval| {
                path_string(
                    approval.approved_resolved_target(),
                    StartupContextOperation::OpenEditor,
                )
            })
            .transpose()?,
    })
}

fn selected_path_index(plan: &StartupProjectPlan) -> HashMap<PathBuf, String> {
    plan.entries()
        .iter()
        .map(|entry| (entry.path().as_path().to_path_buf(), entry.id().to_string()))
        .collect()
}

fn directory_entry_snapshot(
    entry: &StartupBrowserEntry,
    selected: &HashMap<PathBuf, String>,
) -> Result<StartupContextDirectoryEntry, StartupContextFailure> {
    Ok(StartupContextDirectoryEntry {
        name: entry.name().to_string(),
        project_relative_path: entry.project_relative_path().to_string_lossy().into_owned(),
        resolved_path: entry.resolved_path().to_string_lossy().into_owned(),
        path_valid_utf8: entry.path_valid_utf8(),
        kind: match entry.kind() {
            StartupBrowserEntryKind::File => StartupContextDirectoryEntryKind::File,
            StartupBrowserEntryKind::Directory => StartupContextDirectoryEntryKind::Directory,
            StartupBrowserEntryKind::Symlink => StartupContextDirectoryEntryKind::Symlink,
            StartupBrowserEntryKind::Other => StartupContextDirectoryEntryKind::Other,
        },
        classification: path_classification(entry.classification()),
        navigable: entry.navigable(),
        bytes: entry.bytes(),
        selected_spec_id: selected.get(entry.project_relative_path()).cloned(),
    })
}

fn stored_file_receipt_snapshot(
    batch_id: &str,
    batch_kind: StoredStartupBatchKind,
    delivery_state: StoredStartupBatchDeliveryState,
    file: &jcode_session_types::StoredStartupFileReceipt,
) -> StartupContextFileReceiptSnapshot {
    StartupContextFileReceiptSnapshot {
        batch_id: batch_id.to_string(),
        batch_kind: match batch_kind {
            StoredStartupBatchKind::Initial => StartupContextBatchKind::Initial,
            StoredStartupBatchKind::Late => StartupContextBatchKind::Late,
        },
        delivery_state: match delivery_state {
            StoredStartupBatchDeliveryState::Captured => StartupContextDeliveryState::Captured,
            StoredStartupBatchDeliveryState::Dispatched => StartupContextDeliveryState::Dispatched,
            StoredStartupBatchDeliveryState::ProviderAccepted => {
                StartupContextDeliveryState::ProviderAccepted
            }
        },
        spec_id: file.spec_id.clone(),
        message_id: file.message_id.clone(),
        ordinal: file.ordinal,
        logical_path: file.logical_path.clone(),
        resolved_path: file.resolved_path.clone(),
        classification: stored_path_classification(file.classification),
        sha256: file.sha256.clone(),
        bytes: file.bytes,
        estimated_tokens: file.estimated_tokens,
        latest_observation: stored_observed_state(&file.latest_observation.state),
        notification_count: file.notification_count,
    }
}

fn stored_issue_snapshot(issue: &StoredStartupFileIssue) -> StartupContextFileIssueSnapshot {
    StartupContextFileIssueSnapshot {
        input_index: issue.input_index,
        spec_id: issue.spec_id.clone(),
        logical_path: issue.logical_path.clone(),
        kind: stored_issue_kind(&issue.kind),
    }
}

fn stored_issue_kind(kind: &StoredStartupFileIssueKind) -> StartupContextFileIssueKind {
    match kind {
        StoredStartupFileIssueKind::EmptyPath => StartupContextFileIssueKind::EmptyPath,
        StoredStartupFileIssueKind::InvalidPathEncoding => {
            StartupContextFileIssueKind::InvalidPathEncoding
        }
        StoredStartupFileIssueKind::PathTraversal => StartupContextFileIssueKind::PathTraversal,
        StoredStartupFileIssueKind::Missing => StartupContextFileIssueKind::Missing,
        StoredStartupFileIssueKind::BrokenSymlink => StartupContextFileIssueKind::BrokenSymlink,
        StoredStartupFileIssueKind::Unreadable { detail } => {
            StartupContextFileIssueKind::Unreadable {
                detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
            }
        }
        StoredStartupFileIssueKind::UnsupportedTarget { target_type } => {
            StartupContextFileIssueKind::UnsupportedTarget {
                target_type: stored_target_type(*target_type),
            }
        }
        StoredStartupFileIssueKind::UnsupportedContent { content } => {
            StartupContextFileIssueKind::UnsupportedContent {
                content: stored_unsupported_content(*content),
            }
        }
        StoredStartupFileIssueKind::NonUtf8 => StartupContextFileIssueKind::NonUtf8,
        StoredStartupFileIssueKind::ExternalApprovalRequired { resolved_target } => {
            StartupContextFileIssueKind::ExternalApprovalRequired {
                resolved_target: resolved_target.clone(),
            }
        }
        StoredStartupFileIssueKind::ExternalTargetChanged {
            approved_target,
            resolved_target,
        } => StartupContextFileIssueKind::ExternalTargetChanged {
            approved_target: approved_target.clone(),
            resolved_target: resolved_target.clone(),
        },
        StoredStartupFileIssueKind::InvalidExternalApproval { detail } => {
            StartupContextFileIssueKind::InvalidExternalApproval {
                detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
            }
        }
        StoredStartupFileIssueKind::DuplicateSelection { first_input_index } => {
            StartupContextFileIssueKind::DuplicateSelection {
                first_input_index: *first_input_index,
            }
        }
        StoredStartupFileIssueKind::TooManyEntries { count, limit } => {
            StartupContextFileIssueKind::TooManyEntries {
                count: *count,
                limit: *limit,
            }
        }
        StoredStartupFileIssueKind::FileTooLarge { bytes, limit } => {
            StartupContextFileIssueKind::FileTooLarge {
                bytes: *bytes,
                limit: *limit,
            }
        }
        StoredStartupFileIssueKind::BatchTooLarge { bytes, limit } => {
            StartupContextFileIssueKind::BatchTooLarge {
                bytes: *bytes,
                limit: *limit,
            }
        }
        StoredStartupFileIssueKind::ChangedDuringCapture => {
            StartupContextFileIssueKind::ChangedDuringCapture
        }
        StoredStartupFileIssueKind::DirectoryOutsideProject => {
            StartupContextFileIssueKind::DirectoryOutsideProject
        }
        StoredStartupFileIssueKind::DirectoryReadFailed { detail } => {
            StartupContextFileIssueKind::DirectoryReadFailed {
                detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
            }
        }
    }
}

fn domain_issue_snapshot(issue: &StartupFileIssue) -> StartupContextFileIssueSnapshot {
    StartupContextFileIssueSnapshot {
        input_index: issue
            .input_index()
            .and_then(|value| u32::try_from(value).ok()),
        spec_id: issue.spec_id().map(ToString::to_string),
        logical_path: issue
            .logical_path()
            .map(|path| path.to_string_lossy().into_owned()),
        kind: domain_issue_kind(issue.kind()),
    }
}

fn domain_issue_kind(kind: &StartupFileIssueKind) -> StartupContextFileIssueKind {
    match kind {
        StartupFileIssueKind::EmptyPath => StartupContextFileIssueKind::EmptyPath,
        StartupFileIssueKind::InvalidPathEncoding => {
            StartupContextFileIssueKind::InvalidPathEncoding
        }
        StartupFileIssueKind::PathTraversal => StartupContextFileIssueKind::PathTraversal,
        StartupFileIssueKind::Missing => StartupContextFileIssueKind::Missing,
        StartupFileIssueKind::BrokenSymlink => StartupContextFileIssueKind::BrokenSymlink,
        StartupFileIssueKind::Unreadable { detail } => StartupContextFileIssueKind::Unreadable {
            detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
        },
        StartupFileIssueKind::UnsupportedTarget { target_type } => {
            StartupContextFileIssueKind::UnsupportedTarget {
                target_type: domain_target_type(*target_type),
            }
        }
        StartupFileIssueKind::UnsupportedContent { content } => {
            StartupContextFileIssueKind::UnsupportedContent {
                content: domain_unsupported_content(*content),
            }
        }
        StartupFileIssueKind::NonUtf8 => StartupContextFileIssueKind::NonUtf8,
        StartupFileIssueKind::ExternalApprovalRequired { resolved_target } => {
            StartupContextFileIssueKind::ExternalApprovalRequired {
                resolved_target: resolved_target.to_string_lossy().into_owned(),
            }
        }
        StartupFileIssueKind::ExternalTargetChanged {
            approved_target,
            resolved_target,
        } => StartupContextFileIssueKind::ExternalTargetChanged {
            approved_target: approved_target.to_string_lossy().into_owned(),
            resolved_target: resolved_target.to_string_lossy().into_owned(),
        },
        StartupFileIssueKind::InvalidExternalApproval { detail } => {
            StartupContextFileIssueKind::InvalidExternalApproval {
                detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
            }
        }
        StartupFileIssueKind::DuplicateSelection { first_input_index } => {
            StartupContextFileIssueKind::DuplicateSelection {
                first_input_index: u32::try_from(*first_input_index).unwrap_or(u32::MAX),
            }
        }
        StartupFileIssueKind::TooManyEntries { count, limit } => {
            StartupContextFileIssueKind::TooManyEntries {
                count: u32::try_from(*count).unwrap_or(u32::MAX),
                limit: u32::try_from(*limit).unwrap_or(u32::MAX),
            }
        }
        StartupFileIssueKind::FileTooLarge { bytes, limit } => {
            StartupContextFileIssueKind::FileTooLarge {
                bytes: *bytes,
                limit: *limit,
            }
        }
        StartupFileIssueKind::BatchTooLarge { bytes, limit } => {
            StartupContextFileIssueKind::BatchTooLarge {
                bytes: *bytes,
                limit: *limit,
            }
        }
        StartupFileIssueKind::ChangedDuringCapture => {
            StartupContextFileIssueKind::ChangedDuringCapture
        }
        StartupFileIssueKind::DirectoryOutsideProject => {
            StartupContextFileIssueKind::DirectoryOutsideProject
        }
        StartupFileIssueKind::DirectoryReadFailed { detail } => {
            StartupContextFileIssueKind::DirectoryReadFailed {
                detail: bounded_string(detail, MAX_FAILURE_MESSAGE_CHARS),
            }
        }
    }
}

fn stored_observed_state(state: &StoredStartupObservedState) -> StartupContextObservedState {
    match state {
        StoredStartupObservedState::Current => StartupContextObservedState::Current,
        StoredStartupObservedState::Changed { sha256, bytes } => {
            StartupContextObservedState::Changed {
                sha256: sha256.clone(),
                bytes: *bytes,
            }
        }
        StoredStartupObservedState::Missing => StartupContextObservedState::Missing,
        StoredStartupObservedState::Unreadable => StartupContextObservedState::Unreadable,
        StoredStartupObservedState::Unsupported => StartupContextObservedState::Unsupported,
    }
}

fn stored_path_classification(
    classification: StoredStartupPathClassification,
) -> StartupContextPathClassification {
    match classification {
        StoredStartupPathClassification::Project => StartupContextPathClassification::Project,
        StoredStartupPathClassification::External => StartupContextPathClassification::External,
    }
}

fn path_classification(
    classification: StartupPathClassification,
) -> StartupContextPathClassification {
    match classification {
        StartupPathClassification::Project => StartupContextPathClassification::Project,
        StartupPathClassification::External => StartupContextPathClassification::External,
    }
}

fn stored_target_type(target: StoredStartupTargetType) -> StartupContextTargetType {
    match target {
        StoredStartupTargetType::Directory => StartupContextTargetType::Directory,
        StoredStartupTargetType::SymlinkToDirectory => StartupContextTargetType::SymlinkToDirectory,
        StoredStartupTargetType::DeviceOrSpecial => StartupContextTargetType::DeviceOrSpecial,
    }
}

fn domain_target_type(target: StartupTargetType) -> StartupContextTargetType {
    match target {
        StartupTargetType::Directory => StartupContextTargetType::Directory,
        StartupTargetType::SymlinkToDirectory => StartupContextTargetType::SymlinkToDirectory,
        StartupTargetType::DeviceOrSpecial => StartupContextTargetType::DeviceOrSpecial,
    }
}

fn stored_unsupported_content(
    content: StoredStartupUnsupportedContent,
) -> StartupContextUnsupportedContent {
    match content {
        StoredStartupUnsupportedContent::Binary => StartupContextUnsupportedContent::Binary,
        StoredStartupUnsupportedContent::Pdf => StartupContextUnsupportedContent::Pdf,
        StoredStartupUnsupportedContent::Image => StartupContextUnsupportedContent::Image,
    }
}

fn domain_unsupported_content(
    content: StartupUnsupportedContent,
) -> StartupContextUnsupportedContent {
    match content {
        StartupUnsupportedContent::Binary => StartupContextUnsupportedContent::Binary,
        StartupUnsupportedContent::Pdf => StartupContextUnsupportedContent::Pdf,
        StartupUnsupportedContent::Image => StartupContextUnsupportedContent::Image,
    }
}

fn browser_failure(
    operation: StartupContextOperation,
    error: StartupBrowserError,
) -> StartupContextFailure {
    match error {
        StartupBrowserError::InvalidPath { detail, .. } => failure(
            operation,
            StartupContextFailureKind::InvalidPath,
            detail,
            false,
        ),
        StartupBrowserError::Io { detail, .. } => {
            failure(operation, StartupContextFailureKind::Io, detail, true)
        }
        StartupBrowserError::FileIssue(issue) => failure(
            operation,
            StartupContextFailureKind::InvalidPath,
            "Startup Context file validation failed",
            false,
        )
        .with_issue(domain_issue_snapshot(&issue)),
    }
}

fn startup_context_error_failure(
    operation: StartupContextOperation,
    error: StartupContextError,
) -> StartupContextFailure {
    let kind = match error {
        StartupContextError::ProjectIdentity { .. } => StartupContextFailureKind::ProjectIdentity,
        StartupContextError::PlanStorage { .. }
        | StartupContextError::UnsupportedPlanSchema { .. }
        | StartupContextError::InvalidStoredPlan { .. } => StartupContextFailureKind::PlanStorage,
        StartupContextError::StalePlanRevision { .. } => {
            StartupContextFailureKind::StalePlanRevision
        }
        _ => StartupContextFailureKind::InvalidRequest,
    };
    failure(operation, kind, error.to_string(), false)
}

fn ownership_io_failure(
    operation: StartupContextOperation,
    error: std::io::Error,
) -> StartupContextFailure {
    failure(
        operation,
        StartupContextFailureKind::PlanStorage,
        format!("Startup Context editor ownership I/O failed: {error}"),
        true,
    )
}

fn lease_not_found(operation: StartupContextOperation) -> StartupContextFailure {
    failure(
        operation,
        StartupContextFailureKind::LeaseNotFound,
        "Startup Context editor lease was not found or already released",
        true,
    )
}

pub(super) fn failure(
    operation: StartupContextOperation,
    kind: StartupContextFailureKind,
    message: impl Into<String>,
    retryable: bool,
) -> StartupContextFailure {
    StartupContextFailure {
        operation,
        kind,
        message: bounded_string(&message.into(), MAX_FAILURE_MESSAGE_CHARS),
        retryable,
        issues: Vec::new(),
    }
}

trait StartupContextFailureExt {
    fn with_issue(self, issue: StartupContextFileIssueSnapshot) -> Self;
}

impl StartupContextFailureExt for StartupContextFailure {
    fn with_issue(mut self, issue: StartupContextFileIssueSnapshot) -> Self {
        self.issues.push(issue);
        self
    }
}

fn validate_bounded_text(
    value: &str,
    max_chars: usize,
    operation: StartupContextOperation,
    label: &str,
) -> Result<(), StartupContextFailure> {
    let count = value.chars().count();
    if value.is_empty() || count > max_chars {
        return Err(failure(
            operation,
            StartupContextFailureKind::InvalidRequest,
            format!("{label} must contain between 1 and {max_chars} characters"),
            false,
        ));
    }
    Ok(())
}

fn validate_max_chars(
    value: &str,
    max_chars: usize,
    operation: StartupContextOperation,
    label: &str,
) -> Result<(), StartupContextFailure> {
    if value.chars().count() > max_chars {
        return Err(failure(
            operation,
            StartupContextFailureKind::InvalidRequest,
            format!("{label} must contain at most {max_chars} characters"),
            false,
        ));
    }
    Ok(())
}

fn bounded_string(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn path_string(
    path: &Path,
    operation: StartupContextOperation,
) -> Result<String, StartupContextFailure> {
    path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        failure(
            operation,
            StartupContextFailureKind::InvalidPath,
            "Startup Context path is not valid UTF-8",
            false,
        )
    })
}

fn bounded_page_size(value: Option<usize>, default: usize, maximum: usize) -> usize {
    value.unwrap_or(default).clamp(1, maximum)
}

fn page<T: Clone>(
    values: &[T],
    requested_start: usize,
    page_size: usize,
) -> (usize, usize, Option<usize>, Vec<T>) {
    let start = requested_start.min(values.len());
    let end = start.saturating_add(page_size).min(values.len());
    let next = (end < values.len()).then_some(end);
    (start, end, next, values[start..end].to_vec())
}

fn char_chunk(
    text: &str,
    requested_start: usize,
    max_chars: usize,
) -> (String, usize, usize, usize, Option<usize>) {
    let total_chars = text.chars().count();
    let start = requested_start.min(total_chars);
    let end = start.saturating_add(max_chars).min(total_chars);
    let content = text
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    (
        content,
        total_chars,
        start,
        end,
        (end < total_chars).then_some(end),
    )
}

fn chrono_duration(duration: Duration) -> ChronoDuration {
    ChronoDuration::from_std(duration).unwrap_or_else(|_| ChronoDuration::seconds(i64::MAX))
}

fn current_process_start_identity() -> String {
    process_start_identity(std::process::id()).unwrap_or_else(|| {
        format!(
            "process-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        )
    })
}

#[cfg(target_os = "linux")]
fn process_start_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close_paren = stat.rfind(')')?;
    stat.get(close_paren + 2..)?
        .split_whitespace()
        .nth(19)
        .map(ToOwned::to_owned)
}

#[cfg(windows)]
fn process_start_identity(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return None;
    }
    let mut creation = std::mem::zeroed();
    let mut exit = std::mem::zeroed();
    let mut kernel = std::mem::zeroed();
    let mut user = std::mem::zeroed();
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle) };
    (ok != 0).then(|| {
        let value = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        value.to_string()
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_start_identity(pid: u32) -> Option<String> {
    crate::platform::is_process_running(pid).then(|| format!("pid-{pid}"))
}

#[cfg(any(not(unix), test))]
fn process_identity_matches(pid: u32, expected: &str) -> bool {
    process_start_identity(pid).as_deref() == Some(expected)
}

#[cfg(not(unix))]
struct FallbackRecoveryGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(not(unix))]
impl FallbackRecoveryGuard {
    fn acquire(key: &str) -> Result<Self, StartupContextFailure> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
            use windows_sys::Win32::System::Threading::{
                CreateMutexW, INFINITE, WaitForSingleObject,
            };
            let name = format!("Local\\jcode-startup-context-{key}");
            let mut wide = name.encode_utf16().collect::<Vec<_>>();
            wide.push(0);
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
            if handle == 0 {
                return Err(failure(
                    StartupContextOperation::OpenEditor,
                    StartupContextFailureKind::PlanStorage,
                    "could not create the Startup Context fallback recovery mutex",
                    true,
                ));
            }
            let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
            if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                return Err(failure(
                    StartupContextOperation::OpenEditor,
                    StartupContextFailureKind::PlanStorage,
                    "could not acquire the Startup Context fallback recovery mutex",
                    true,
                ));
            }
            Ok(Self { handle })
        }

        #[cfg(not(windows))]
        {
            let _ = key;
            Ok(Self {})
        }
    }
}

#[cfg(all(not(unix), windows))]
impl Drop for FallbackRecoveryGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(all(not(unix), not(windows)))]
impl Drop for FallbackRecoveryGuard {
    fn drop(&mut self) {}
}

pub(super) fn lease_request(
    lease_id: String,
    project_key_digest: String,
    expected_plan_revision: Option<u64>,
    session_id: String,
    connection_id: String,
) -> LeaseRequest {
    LeaseRequest {
        lease_id,
        project_key_digest,
        expected_plan_revision,
        owner_session_id: session_id,
        owner_connection_id: connection_id,
    }
}

pub(super) fn emit_checked(
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    id: u64,
    operation: StartupContextOperation,
    event: ServerEvent,
) -> bool {
    if serde_json::to_vec(&event)
        .is_ok_and(|bytes| bytes.len() <= STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES)
    {
        let _ = event_tx.send(event);
        return true;
    }
    let _ = event_tx.send(ServerEvent::StartupContextFailed {
        id,
        failure: failure(
            operation,
            StartupContextFailureKind::EventTooLarge,
            format!(
                "Startup Context response exceeded the {}-byte protocol bound",
                STARTUP_CONTEXT_PROTOCOL_MAX_EVENT_BYTES
            ),
            false,
        ),
    });
    false
}

#[cfg(test)]
mod tests;
