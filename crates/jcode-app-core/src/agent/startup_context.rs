use super::Agent;
use crate::session::{Session, StartupContextInstallError, StartupContextInstallOutcome};
use crate::startup_context::{
    StartupContext, StartupContextError, StartupFailurePolicy, StartupFileIssue, StartupPreparation,
};
use jcode_session_types::StoredStartupContextState;
use jcode_session_types::{StoredStartupContextBlock, StoredStartupContextBlockKind};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug)]
pub(crate) struct StartupContextActionRequiredError {
    pub(crate) action: crate::protocol::StartupContextActionRequired,
}

impl fmt::Display for StartupContextActionRequiredError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.action.detail)
    }
}

impl Error for StartupContextActionRequiredError {}

pub fn startup_file_issue_code(
    kind: &crate::startup_context::StartupFileIssueKind,
) -> &'static str {
    use crate::startup_context::StartupFileIssueKind as Kind;
    match kind {
        Kind::EmptyPath => "empty_path",
        Kind::InvalidPathEncoding => "invalid_path_encoding",
        Kind::PathTraversal => "path_traversal",
        Kind::Missing => "missing",
        Kind::BrokenSymlink => "broken_symlink",
        Kind::Unreadable { .. } => "unreadable",
        Kind::UnsupportedTarget { .. } => "unsupported_target",
        Kind::UnsupportedContent { .. } => "unsupported_content",
        Kind::NonUtf8 => "non_utf8",
        Kind::ExternalApprovalRequired { .. } => "external_approval_required",
        Kind::ExternalTargetChanged { .. } => "external_target_changed",
        Kind::InvalidExternalApproval { .. } => "invalid_external_approval",
        Kind::DuplicateSelection { .. } => "duplicate_selection",
        Kind::TooManyEntries { .. } => "too_many_entries",
        Kind::FileTooLarge { .. } => "file_too_large",
        Kind::BatchTooLarge { .. } => "batch_too_large",
        Kind::ChangedDuringCapture => "changed_during_capture",
        Kind::DirectoryOutsideProject => "directory_outside_project",
        Kind::DirectoryReadFailed { .. } => "directory_read_failed",
    }
}

pub fn stored_startup_file_issue_code(
    kind: &jcode_session_types::StoredStartupFileIssueKind,
) -> &'static str {
    use jcode_session_types::StoredStartupFileIssueKind as Kind;
    match kind {
        Kind::EmptyPath => "empty_path",
        Kind::InvalidPathEncoding => "invalid_path_encoding",
        Kind::PathTraversal => "path_traversal",
        Kind::Missing => "missing",
        Kind::BrokenSymlink => "broken_symlink",
        Kind::Unreadable { .. } => "unreadable",
        Kind::UnsupportedTarget { .. } => "unsupported_target",
        Kind::UnsupportedContent { .. } => "unsupported_content",
        Kind::NonUtf8 => "non_utf8",
        Kind::ExternalApprovalRequired { .. } => "external_approval_required",
        Kind::ExternalTargetChanged { .. } => "external_target_changed",
        Kind::InvalidExternalApproval { .. } => "invalid_external_approval",
        Kind::DuplicateSelection { .. } => "duplicate_selection",
        Kind::TooManyEntries { .. } => "too_many_entries",
        Kind::FileTooLarge { .. } => "file_too_large",
        Kind::BatchTooLarge { .. } => "batch_too_large",
        Kind::ChangedDuringCapture => "changed_during_capture",
        Kind::DirectoryOutsideProject => "directory_outside_project",
        Kind::DirectoryReadFailed { .. } => "directory_read_failed",
    }
}

/// Whether a fresh Agent participates in Startup Context.
///
/// Activation is deliberately explicit at production construction sites. A
/// working directory alone never opts an internal worker into primary-session
/// context capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupContextActivation {
    Disabled,
    Primary {
        failure_policy: StartupFailurePolicy,
        caller: StartupContextCaller,
    },
}

impl StartupContextActivation {
    pub const fn primary(caller: StartupContextCaller) -> Self {
        Self::Primary {
            failure_policy: StartupFailurePolicy::Block,
            caller,
        }
    }
}

/// The product caller that requested a new primary context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupContextCaller {
    InteractiveTui,
    InteractiveRepl,
    RunCommand,
    HarnessApi,
    Clear,
    Transfer,
    FutureUnattended,
}

impl StartupContextCaller {
    pub const fn label(self) -> &'static str {
        match self {
            Self::InteractiveTui => "interactive_tui",
            Self::InteractiveRepl => "interactive_repl",
            Self::RunCommand => "run_command",
            Self::HarnessApi => "harness_api",
            Self::Clear => "clear",
            Self::Transfer => "transfer",
            Self::FutureUnattended => "future_unattended",
        }
    }

    const fn retains_blocked_session(self) -> bool {
        matches!(
            self,
            Self::InteractiveTui | Self::InteractiveRepl | Self::Clear
        )
    }
}

#[derive(Clone, Debug)]
pub enum StartupContextActivationOutcome {
    Disabled,
    Installed {
        state: StoredStartupContextState,
        file_count: usize,
        captured_bytes: u64,
        estimated_tokens: u64,
        issue_count: usize,
    },
    PreparationBlocked {
        caller: StartupContextCaller,
        block: StoredStartupContextBlock,
    },
    Diagnostic {
        caller: StartupContextCaller,
        preparation: StartupPreparation,
    },
}

impl StartupContextActivationOutcome {
    pub fn is_blocked(&self) -> bool {
        matches!(
            self,
            Self::Installed {
                state: StoredStartupContextState::Blocked,
                ..
            } | Self::PreparationBlocked { .. }
        )
    }

    pub fn issue_count(&self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::Installed { issue_count, .. } => *issue_count,
            Self::PreparationBlocked { .. } => 0,
            Self::Diagnostic { preparation, .. } => preparation.issue_count(),
        }
    }
}

#[derive(Debug)]
pub enum StartupContextActivationError {
    Domain {
        caller: StartupContextCaller,
        source: StartupContextError,
    },
    Blocked {
        caller: StartupContextCaller,
        preparation: Box<StartupPreparation>,
    },
    Install {
        caller: StartupContextCaller,
        source: StartupContextInstallError,
    },
    Cleanup {
        caller: StartupContextCaller,
        activation_error: String,
        source: anyhow::Error,
    },
}

impl StartupContextActivationError {
    pub fn caller(&self) -> StartupContextCaller {
        match self {
            Self::Domain { caller, .. }
            | Self::Blocked { caller, .. }
            | Self::Install { caller, .. }
            | Self::Cleanup { caller, .. } => *caller,
        }
    }

    pub fn issues(&self) -> Vec<&StartupFileIssue> {
        match self {
            Self::Blocked { preparation, .. } => preparation.issues().collect(),
            Self::Domain { .. } | Self::Install { .. } | Self::Cleanup { .. } => Vec::new(),
        }
    }

    pub fn preparation(&self) -> Option<&StartupPreparation> {
        match self {
            Self::Blocked { preparation, .. } => Some(preparation.as_ref()),
            Self::Domain { .. } | Self::Install { .. } | Self::Cleanup { .. } => None,
        }
    }
}

impl fmt::Display for StartupContextActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain { caller, source } => write!(
                formatter,
                "Startup Context preparation failed for {}: {source}",
                caller.label()
            ),
            Self::Blocked {
                caller,
                preparation,
            } => write!(
                formatter,
                "Startup Context blocked {} because {} required file issue(s) remain unresolved",
                caller.label(),
                preparation.issue_count()
            ),
            Self::Install { caller, source } => write!(
                formatter,
                "Startup Context installation failed for {}: {source}",
                caller.label()
            ),
            Self::Cleanup {
                caller,
                activation_error,
                source,
            } => write!(
                formatter,
                "Startup Context rejected {}, then unpublished-session cleanup failed: {source}; original failure: {activation_error}",
                caller.label()
            ),
        }
    }
}

impl Error for StartupContextActivationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Domain { source, .. } => Some(source),
            Self::Install { source, .. } => Some(source),
            Self::Cleanup { source, .. } => Some(source.as_ref()),
            Self::Blocked { .. } => None,
        }
    }
}

impl Agent {
    pub fn activate_startup_context(
        &mut self,
        activation: StartupContextActivation,
    ) -> Result<StartupContextActivationOutcome, StartupContextActivationError> {
        let outcome = activate_session_startup_context(&mut self.session, activation)?;
        if !matches!(outcome, StartupContextActivationOutcome::Disabled) {
            self.reseed_context_runtime_from_session();
            self.provider.invalidate_context_continuation(
                "startup context activated for new primary session",
            );
        }
        Ok(outcome)
    }

    /// Observe immutable Startup Context snapshots immediately before a real
    /// user turn. Callers must not use this for tool continuations, provider
    /// retries, redraws, or background-only activity.
    pub fn observe_startup_context_before_user_turn(
        &mut self,
    ) -> Result<
        crate::session::StartupContextObservationOutcome,
        crate::session::StartupContextObservationError,
    > {
        self.observe_startup_context_before_user_turn_with(&StartupContext::new())
    }

    pub(crate) fn observe_startup_context_before_user_turn_with(
        &mut self,
        engine: &StartupContext,
    ) -> Result<
        crate::session::StartupContextObservationOutcome,
        crate::session::StartupContextObservationError,
    > {
        let outcome = self
            .session
            .observe_startup_context_before_user_turn(engine)?;
        if outcome.provider_history_changed() {
            self.reseed_context_runtime_from_session();
        }
        Ok(outcome)
    }
}

pub(crate) fn activate_session_startup_context(
    session: &mut Session,
    activation: StartupContextActivation,
) -> Result<StartupContextActivationOutcome, StartupContextActivationError> {
    activate_session_startup_context_with_engine(session, activation, &StartupContext::new())
}

fn activate_session_startup_context_with_engine(
    session: &mut Session,
    activation: StartupContextActivation,
    engine: &StartupContext,
) -> Result<StartupContextActivationOutcome, StartupContextActivationError> {
    let StartupContextActivation::Primary {
        failure_policy,
        caller,
    } = activation
    else {
        return Ok(StartupContextActivationOutcome::Disabled);
    };

    let working_dir = session
        .working_dir
        .as_deref()
        .map(std::path::Path::new)
        .ok_or_else(|| StartupContextActivationError::Domain {
            caller,
            source: StartupContextError::ProjectIdentity {
                path: std::path::PathBuf::new(),
                detail: "new primary session has no bound working directory".to_string(),
            },
        })?;
    let project = match engine.resolve_project(working_dir) {
        Ok(project) => project,
        Err(source) => return handle_domain_failure(session, caller, source),
    };
    let plan = match engine.load_project_plan(&project) {
        Ok(plan) => plan,
        Err(source) => return handle_domain_failure(session, caller, source),
    };
    let prepared = engine
        .prepare_project_plan(&project, plan.plan(), failure_policy)
        .map_err(|source| StartupContextActivationError::Domain { caller, source })?;

    if matches!(
        prepared,
        crate::startup_context::StartupPreparationOutcome::Diagnostic(_)
    ) {
        return Ok(StartupContextActivationOutcome::Diagnostic {
            caller,
            preparation: prepared.into_preparation(),
        });
    }
    if matches!(
        prepared,
        crate::startup_context::StartupPreparationOutcome::Blocked(_)
    ) && !caller.retains_blocked_session()
    {
        return Err(StartupContextActivationError::Blocked {
            caller,
            preparation: Box::new(prepared.into_preparation()),
        });
    }

    let issue_count = prepared.preparation().issue_count();
    let StartupContextInstallOutcome {
        state,
        file_count,
        captured_bytes,
        estimated_tokens,
    } = session
        .install_prepared_startup_context(prepared)
        .map_err(|source| StartupContextActivationError::Install { caller, source })?;
    Ok(StartupContextActivationOutcome::Installed {
        state,
        file_count,
        captured_bytes,
        estimated_tokens,
        issue_count,
    })
}

fn handle_domain_failure(
    session: &mut Session,
    caller: StartupContextCaller,
    source: StartupContextError,
) -> Result<StartupContextActivationOutcome, StartupContextActivationError> {
    if !caller.retains_blocked_session() {
        return Err(StartupContextActivationError::Domain { caller, source });
    }
    let kind = match &source {
        StartupContextError::ProjectIdentity { .. } => {
            StoredStartupContextBlockKind::ProjectIdentity
        }
        StartupContextError::PlanStorage { .. }
        | StartupContextError::UnsupportedPlanSchema { .. }
        | StartupContextError::InvalidStoredPlan { .. }
        | StartupContextError::PlanProjectMismatch => StoredStartupContextBlockKind::PlanStorage,
        _ => return Err(StartupContextActivationError::Domain { caller, source }),
    };
    let block = StoredStartupContextBlock {
        kind,
        message: source.to_string(),
        blocked_at: chrono::Utc::now(),
    };
    session
        .install_startup_context_block(block.clone())
        .map_err(|source| StartupContextActivationError::Install {
            caller,
            source: StartupContextInstallError::Persistence(source),
        })?;
    Ok(StartupContextActivationOutcome::PreparationBlocked { caller, block })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::startup_context::{StartupPreparationOutcome, StartupSelectionInput};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct RecordingProvider {
        calls: std::sync::Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("ok".to_string())),
                Ok(StreamEvent::MessageEnd { stop_reason: None }),
            ])))
        }

        fn name(&self) -> &str {
            "recording-startup-provider"
        }

        fn fork(&self) -> std::sync::Arc<dyn Provider> {
            std::sync::Arc::new(self.clone())
        }
    }

    struct TestHome {
        _guard: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
        _home: tempfile::TempDir,
    }

    impl TestHome {
        fn new() -> Self {
            let guard = crate::storage::lock_test_env();
            let previous = std::env::var_os("JCODE_HOME");
            let home = tempfile::tempdir().expect("test JCODE_HOME");
            crate::env::set_var("JCODE_HOME", home.path());
            Self {
                _guard: guard,
                previous,
                _home: home,
            }
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => crate::env::set_var("JCODE_HOME", value),
                None => crate::env::remove_var("JCODE_HOME"),
            }
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        StartupContext,
        std::path::PathBuf,
    ) {
        let project = tempfile::tempdir().expect("project tempdir");
        let state = tempfile::tempdir().expect("state tempdir");
        let engine = StartupContext::from_durable_state_dir(state.path());
        let file = project.path().join("required.txt");
        std::fs::write(&file, "synthetic required context").expect("write required file");
        let active = engine
            .resolve_project(project.path())
            .expect("resolve project");
        let preview =
            engine.preview_selection(&active, [StartupSelectionInput::new("required.txt")]);
        engine
            .save_project_plan(&active, 0, &preview)
            .expect("save project plan");
        (project, state, engine, file)
    }

    fn session(project: &std::path::Path) -> Session {
        let mut session = Session::create(None, None);
        session.working_dir = Some(project.to_string_lossy().into_owned());
        session.ensure_initial_session_context_message();
        session
    }

    #[test]
    fn primary_activation_installs_valid_context_and_rejects_noninteractive_blocking() {
        let _home = TestHome::new();
        let (project, _state, engine, file) = fixture();
        let mut ready = session(project.path());
        let ready_outcome = activate_session_startup_context_with_engine(
            &mut ready,
            StartupContextActivation::primary(StartupContextCaller::RunCommand),
            &engine,
        )
        .expect("valid run activation");
        assert!(matches!(
            ready_outcome,
            StartupContextActivationOutcome::Installed {
                state: StoredStartupContextState::Prepared,
                file_count: 1,
                issue_count: 0,
                ..
            }
        ));
        assert_eq!(
            ready.startup_context.as_ref().unwrap().batches[0]
                .files
                .len(),
            1
        );

        std::fs::remove_file(file).expect("remove selected file");
        let mut rejected = session(project.path());
        let error = activate_session_startup_context_with_engine(
            &mut rejected,
            StartupContextActivation::primary(StartupContextCaller::HarnessApi),
            &engine,
        )
        .expect_err("Harness creation must reject blocked context");
        assert!(matches!(
            error,
            StartupContextActivationError::Blocked { .. }
        ));
        assert_eq!(error.issues().len(), 1);
        assert!(rejected.startup_context.is_none());
    }

    #[test]
    fn interactive_blocking_persists_repairable_state_and_diagnostic_is_non_mutating() {
        let _home = TestHome::new();
        let (project, _state, engine, file) = fixture();
        std::fs::remove_file(file).expect("remove selected file");

        let mut interactive = session(project.path());
        let outcome = activate_session_startup_context_with_engine(
            &mut interactive,
            StartupContextActivation::primary(StartupContextCaller::InteractiveTui),
            &engine,
        )
        .expect("interactive blocked state remains usable");
        assert!(outcome.is_blocked());
        assert_eq!(outcome.issue_count(), 1);
        assert_eq!(
            interactive.startup_context.as_ref().unwrap().state,
            StoredStartupContextState::Blocked
        );
        assert!(interactive.mark_startup_context_dispatched().is_err());

        let mut diagnostic = session(project.path());
        let outcome = activate_session_startup_context_with_engine(
            &mut diagnostic,
            StartupContextActivation::Primary {
                failure_policy: StartupFailurePolicy::InjectDiagnostic,
                caller: StartupContextCaller::FutureUnattended,
            },
            &engine,
        )
        .expect("diagnostic seam");
        assert!(matches!(
            outcome,
            StartupContextActivationOutcome::Diagnostic { preparation, .. }
                if preparation.issue_count() == 1
        ));
        assert!(diagnostic.startup_context.is_none());

        let active = engine.resolve_project(project.path()).unwrap();
        let plan = engine.load_project_plan(&active).unwrap();
        assert!(matches!(
            engine
                .prepare_project_plan(&active, plan.plan(), StartupFailurePolicy::InjectDiagnostic)
                .unwrap(),
            StartupPreparationOutcome::Diagnostic(_)
        ));
    }

    #[test]
    fn corrupt_plan_is_durable_for_interactive_repair_and_rejected_noninteractively() {
        let _home = TestHome::new();
        let (project, state, engine, _file) = fixture();
        let projects_dir = state.path().join("startup-context").join("projects");
        let plan_path = std::fs::read_dir(&projects_dir)
            .expect("read projects dir")
            .next()
            .expect("plan entry")
            .expect("plan dir entry")
            .path();
        std::fs::write(&plan_path, b"not json").expect("corrupt primary");
        std::fs::write(plan_path.with_extension("bak"), b"also not json").expect("corrupt backup");

        let mut interactive = session(project.path());
        let outcome = activate_session_startup_context_with_engine(
            &mut interactive,
            StartupContextActivation::primary(StartupContextCaller::InteractiveTui),
            &engine,
        )
        .expect("interactive storage failure remains repairable");
        assert!(matches!(
            outcome,
            StartupContextActivationOutcome::PreparationBlocked {
                block: StoredStartupContextBlock {
                    kind: StoredStartupContextBlockKind::PlanStorage,
                    ..
                },
                ..
            }
        ));
        assert!(interactive.mark_startup_context_dispatched().is_err());
        let loaded = Session::load(&interactive.id).expect("reload blocked session");
        assert_eq!(
            loaded.startup_context_block,
            interactive.startup_context_block
        );

        let mut run = session(project.path());
        assert!(matches!(
            activate_session_startup_context_with_engine(
                &mut run,
                StartupContextActivation::primary(StartupContextCaller::RunCommand),
                &engine,
            ),
            Err(StartupContextActivationError::Domain { .. })
        ));
        assert!(run.startup_context_block.is_none());
    }

    #[test]
    fn rejected_noninteractive_agent_creation_leaves_no_orphan_session() {
        let _home = TestHome::new();
        let project = tempfile::tempdir().expect("project tempdir");
        let required = project.path().join("required.txt");
        std::fs::write(&required, "remove me").expect("write startup file");
        let engine = StartupContext::new();
        let active = engine
            .resolve_project(project.path())
            .expect("resolve project");
        let preview =
            engine.preview_selection(&active, [StartupSelectionInput::new("required.txt")]);
        engine
            .save_project_plan(&active, 0, &preview)
            .expect("save plan");
        std::fs::remove_file(required).expect("remove selected file");

        let active_before = crate::session::active_session_ids();
        let sessions_dir = crate::storage::jcode_dir().unwrap().join("sessions");
        let files_before = std::fs::read_dir(&sessions_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<std::collections::HashSet<_>>();
        let provider = RecordingProvider::default();
        let calls = std::sync::Arc::clone(&provider.calls);

        let result = Agent::new_with_startup_context(
            std::sync::Arc::new(provider),
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::HarnessApi),
        );
        assert!(matches!(
            result,
            Err(StartupContextActivationError::Blocked { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(crate::session::active_session_ids(), active_before);
        let files_after = std::fs::read_dir(&sessions_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(files_after, files_before);
    }

    #[tokio::test]
    async fn interactive_block_prevents_provider_call_and_clear_recaptures_after_repair() {
        let _home = TestHome::new();
        let project = tempfile::tempdir().expect("project tempdir");
        let required = project.path().join("required.txt");
        std::fs::write(&required, "repairable context").expect("write startup file");
        let engine = StartupContext::new();
        let active = engine
            .resolve_project(project.path())
            .expect("resolve project");
        let preview =
            engine.preview_selection(&active, [StartupSelectionInput::new("required.txt")]);
        engine
            .save_project_plan(&active, 0, &preview)
            .expect("save plan");
        std::fs::remove_file(&required).expect("remove required file");

        let provider = RecordingProvider::default();
        let calls = std::sync::Arc::clone(&provider.calls);
        let (mut agent, outcome) = Agent::new_with_startup_context(
            std::sync::Arc::new(provider),
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::InteractiveRepl),
        )
        .expect("interactive blocked agent remains repairable");
        assert!(outcome.is_blocked());
        assert!(agent.run_once_capture("must not dispatch").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        std::fs::write(&required, "repaired context").expect("repair required file");
        let clear_outcome = agent.clear().expect("clear recaptures repaired default");
        assert!(!clear_outcome.is_blocked());
        let text = agent
            .run_once_capture("dispatch after repair")
            .await
            .expect("provider turn after repair");
        assert_eq!(text, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
