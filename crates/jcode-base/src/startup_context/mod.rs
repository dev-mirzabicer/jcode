//! Server-side Startup Context domain.
//!
//! This module owns project identity, private ordered project plans, path
//! normalization, complete stable UTF-8 capture, and typed preparation
//! outcomes. It intentionally has no TUI, protocol, session-mutation, or
//! provider-dispatch behavior.

mod capture;
mod plan;
mod project;
mod selection;
mod types;

pub use types::*;

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
