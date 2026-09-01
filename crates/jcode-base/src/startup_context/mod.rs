//! Server-side Startup Context domain.
//!
//! This module owns project identity, private ordered project plans, path
//! normalization, complete stable UTF-8 capture, and typed preparation
//! outcomes. It intentionally has no TUI, protocol, session-mutation, or
//! provider-dispatch behavior.

mod browser;
mod capture;
mod observation;
mod plan;
mod project;
mod selection;
mod types;

pub use browser::*;
pub use types::*;

use jcode_session_types::StoredStartupFileSpec;
use plan::StartupPlanStore;
use std::path::{Path, PathBuf};

/// Deep facade for every Phase 1 domain operation that does not mutate a session.
#[derive(Clone, Debug)]
pub struct StartupContext {
    plan_store: StartupPlanStore,
    max_plan_entries: usize,
    max_batch_bytes: u64,
    max_capture_attempts: usize,
}

impl Default for StartupContext {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupContext {
    pub fn new() -> Self {
        Self::from_durable_state_dir(crate::storage::durable_state_dir())
    }

    /// Construct a facade rooted in a caller-provided durable-state directory.
    ///
    /// Production callers normally use [`Self::new`]. Sandboxed servers and
    /// tests can supply their isolated durable state without mutating global
    /// environment variables.
    pub fn from_durable_state_dir(durable_state_dir: impl Into<PathBuf>) -> Self {
        let max_plan_entries = DEFAULT_MAX_STARTUP_PLAN_ENTRIES;
        let projects_dir = durable_state_dir
            .into()
            .join("startup-context")
            .join("projects");
        Self {
            plan_store: StartupPlanStore::new(projects_dir, max_plan_entries),
            max_plan_entries,
            max_batch_bytes: DEFAULT_MAX_STARTUP_BATCH_BYTES,
            max_capture_attempts: DEFAULT_MAX_CAPTURE_ATTEMPTS,
        }
    }

    pub fn resolve_project(
        &self,
        launch_dir: impl AsRef<Path>,
    ) -> Result<ActiveProject, StartupContextError> {
        project::resolve_project(launch_dir.as_ref())
    }

    pub fn load_project_plan(
        &self,
        project: &ActiveProject,
    ) -> Result<LoadedStartupProjectPlan, StartupContextError> {
        self.plan_store.load(project)
    }

    pub fn preview_selection<I>(
        &self,
        project: &ActiveProject,
        inputs: I,
    ) -> StartupSelectionPreview
    where
        I: IntoIterator<Item = StartupSelectionInput>,
    {
        selection::preview_selection(
            project,
            inputs.into_iter().collect(),
            self.max_plan_entries,
            self.max_batch_bytes,
        )
    }

    /// Revalidate a durable queued selection through the same normalization
    /// path as an interactive request. No file content is accepted from the
    /// stored shape; complete capture still happens separately at drain time.
    pub fn preview_stored_selection(
        &self,
        project: &ActiveProject,
        specs: &[StoredStartupFileSpec],
    ) -> Result<StartupSelectionPreview, StartupContextError> {
        let inputs = specs
            .iter()
            .cloned()
            .map(StartupFileSpec::from_stored)
            .map(|spec| {
                spec.map(|spec| {
                    let mut input = StartupSelectionInput::existing(
                        spec.id().clone(),
                        spec.path().as_path().to_path_buf(),
                    );
                    if let Some(approval) = spec.external_approval() {
                        input = input.with_external_approval(
                            approval.approved_resolved_target().to_path_buf(),
                        );
                    }
                    input
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.preview_selection(project, inputs))
    }

    /// Project a valid preview to the durable path-only queue representation.
    pub fn store_selection(
        &self,
        preview: &StartupSelectionPreview,
    ) -> Result<Vec<StoredStartupFileSpec>, StartupContextError> {
        if !preview.is_valid() {
            return Err(StartupContextError::InvalidSelection {
                issue_count: preview.issue_count(),
            });
        }
        preview
            .selected()
            .map(|selected| selected.spec().to_stored())
            .collect()
    }

    pub fn expand_directory(
        &self,
        project: &ActiveProject,
        directory: impl AsRef<Path>,
    ) -> StartupSelectionPreview {
        selection::expand_directory(
            project,
            directory.as_ref(),
            self.max_plan_entries,
            self.max_batch_bytes,
        )
    }

    pub fn save_project_plan(
        &self,
        project: &ActiveProject,
        expected_revision: u64,
        preview: &StartupSelectionPreview,
    ) -> Result<StartupProjectPlan, StartupContextError> {
        self.plan_store.save(project, expected_revision, preview)
    }

    pub fn prepare_project_plan_transition(
        &self,
        project: &ActiveProject,
        expected_revision: u64,
        preview: &StartupSelectionPreview,
    ) -> Result<StartupProjectPlanTransition, StartupContextError> {
        self.plan_store
            .prepare_transition(project, expected_revision, preview)
    }

    pub fn commit_project_plan_transition(
        &self,
        project: &ActiveProject,
        transition: &StartupProjectPlanTransition,
    ) -> Result<StartupProjectPlanCommitOutcome, StartupContextError> {
        self.plan_store.commit_transition(project, transition)
    }

    pub fn prepare_project_plan(
        &self,
        project: &ActiveProject,
        plan: &StartupProjectPlan,
        failure_policy: StartupFailurePolicy,
    ) -> Result<StartupPreparationOutcome, StartupContextError> {
        capture::prepare_plan(
            project,
            plan,
            failure_policy,
            self.max_batch_bytes,
            self.max_capture_attempts,
        )
    }

    pub fn prepare_selection(
        &self,
        project: &ActiveProject,
        plan_revision: u64,
        preview: &StartupSelectionPreview,
        failure_policy: StartupFailurePolicy,
    ) -> Result<StartupPreparationOutcome, StartupContextError> {
        capture::prepare_preview(
            project,
            plan_revision,
            preview,
            failure_policy,
            self.max_batch_bytes,
            self.max_capture_attempts,
        )
    }

    /// Observe one previously captured receipt file using the same path,
    /// external-target, stability, size, and UTF-8 support rules as capture.
    pub(crate) fn observe_receipt_file(
        &self,
        project: &ActiveProject,
        file: &jcode_session_types::StoredStartupFileReceipt,
    ) -> StartupObservedState {
        observation::observe_receipt_file(
            project,
            file,
            self.max_batch_bytes,
            self.max_capture_attempts,
        )
    }

    #[cfg(test)]
    fn with_limits(mut self, max_plan_entries: usize, max_batch_bytes: u64) -> Self {
        self.max_plan_entries = max_plan_entries;
        self.max_batch_bytes = max_batch_bytes;
        self.plan_store = StartupPlanStore::new(
            self.plan_store.projects_dir().to_path_buf(),
            max_plan_entries,
        );
        self
    }
}

#[cfg(test)]
mod tests;
