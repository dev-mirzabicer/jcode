use super::{Agent, PrimaryInstructionActivationError};
use crate::session::{Session, StartupContextInstallError, StartupContextInstallOutcome};
use crate::startup_context::{
    StartupContext, StartupContextError, StartupFailurePolicy, StartupFileIssue, StartupPreparation,
};
use jcode_session_types::StoredStartupContextState;
use jcode_session_types::{StoredStartupContextBlock, StoredStartupContextBlockKind};
use std::error::Error;
use std::fmt;
use std::path::Path;

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
    Instruction {
        caller: StartupContextCaller,
        source: PrimaryInstructionActivationError,
    },
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
            Self::Instruction { caller, .. }
            | Self::Domain { caller, .. }
            | Self::Blocked { caller, .. }
            | Self::Install { caller, .. }
            | Self::Cleanup { caller, .. } => *caller,
        }
    }

    pub fn issues(&self) -> Vec<&StartupFileIssue> {
        match self {
            Self::Blocked { preparation, .. } => preparation.issues().collect(),
            Self::Instruction { .. }
            | Self::Domain { .. }
            | Self::Install { .. }
            | Self::Cleanup { .. } => Vec::new(),
        }
    }

    pub fn preparation(&self) -> Option<&StartupPreparation> {
        match self {
            Self::Blocked { preparation, .. } => Some(preparation.as_ref()),
            Self::Instruction { .. }
            | Self::Domain { .. }
            | Self::Install { .. }
            | Self::Cleanup { .. } => None,
        }
    }
}

impl fmt::Display for StartupContextActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instruction { caller, source } => write!(
                formatter,
                "instruction activation failed for {}: {source}",
                caller.label()
            ),
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
            Self::Instruction { source, .. } => Some(source),
            Self::Domain { source, .. } => Some(source),
            Self::Install { source, .. } => Some(source),
            Self::Cleanup { source, .. } => Some(source.as_ref()),
            Self::Blocked { .. } => None,
        }
    }
}

impl Agent {
    pub(super) fn compose_primary_instructions_for_session(
        &self,
        session: &Session,
        selection: crate::instruction::AgentSelection,
    ) -> Result<crate::instruction::SystemPromptActivation, PrimaryInstructionActivationError> {
        let working_dir = session.working_dir.as_deref().map(Path::new);
        let skills = self.current_skills_snapshot_for_working_dir(working_dir);
        let available_skills = skills
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect::<Vec<_>>();
        crate::instruction::SystemPromptComposer::from_repository_service(
            self.instruction_repositories.clone(),
        )
        .activate(crate::instruction::SystemPromptActivationRequest {
            working_dir,
            selection,
            is_selfdev: session.is_canary,
            capabilities: crate::prompt::PromptCapabilities::current(),
            available_skills: &available_skills,
        })
        .map_err(PrimaryInstructionActivationError::from)
    }

    pub fn activate_primary_instructions(
        &mut self,
        selection: crate::instruction::AgentSelection,
    ) -> Result<crate::instruction::SystemPromptActivation, PrimaryInstructionActivationError> {
        if let Some(current) = self.session.system_prompt.as_ref()
            && selection.matches_stored(&current.active_agent)
        {
            return Ok(crate::instruction::SystemPromptActivation {
                state: current.clone(),
                initialized_global_store: false,
            });
        }
        let activation = self.compose_primary_instructions_for_session(&self.session, selection)?;
        let previous = self.session.clone();
        self.session.install_system_prompt(activation.state.clone());
        if let Err(error) = self.session.save() {
            self.session = previous;
            return Err(PrimaryInstructionActivationError::Persistence(error));
        }
        self.provider
            .invalidate_context_continuation("frozen system prompt activated");
        Ok(activation)
    }

    pub fn list_primary_agents(
        &self,
    ) -> Result<Vec<crate::instruction::AgentCatalogEntry>, PrimaryInstructionActivationError> {
        let working_dir = self.session.working_dir.as_deref().map(Path::new);
        crate::instruction::SystemPromptComposer::from_repository_service(
            self.instruction_repositories.clone(),
        )
        .list_primary_agents(working_dir)
        .map_err(PrimaryInstructionActivationError::from)
    }

    pub async fn change_primary_agent(
        &mut self,
        selection: crate::instruction::AgentSelection,
        mode: super::AgentProfileChangeMode,
    ) -> Result<super::AgentProfileChangeOutcome, PrimaryInstructionActivationError> {
        let current = self
            .session
            .active_agent()
            .cloned()
            .ok_or(crate::session::AgentProfileSessionError::MissingActivation)?;
        let dispatched = self.session.first_provider_dispatch_at().is_some();

        if mode == super::AgentProfileChangeMode::Ordinary && selection.matches_stored(&current) {
            return Ok(super::AgentProfileChangeOutcome::NoChange { agent: current });
        }

        let working_dir = self.session.working_dir.as_deref().map(Path::new);
        let composer = crate::instruction::SystemPromptComposer::from_repository_service(
            self.instruction_repositories.clone(),
        );

        if !dispatched {
            let activation =
                self.compose_primary_instructions_for_session(&self.session, selection)?;
            if mode == super::AgentProfileChangeMode::Ordinary
                && activation.state.active_agent == current
            {
                return Ok(super::AgentProfileChangeOutcome::NoChange { agent: current });
            }
            let mut candidate = self.session.clone();
            candidate.install_system_prompt(activation.state.clone());
            candidate
                .save()
                .map_err(PrimaryInstructionActivationError::Persistence)?;
            self.session = candidate;
            let agent = activation.state.active_agent;
            return Ok(super::AgentProfileChangeOutcome::Provisional { agent });
        }

        match mode {
            super::AgentProfileChangeMode::Ordinary => {
                let transition = composer.render_agent_transition(working_dir, selection)?;
                if transition.agent == current {
                    return Ok(super::AgentProfileChangeOutcome::NoChange { agent: current });
                }
                let agent = transition.agent.clone();
                let mut candidate = self.session.clone();
                let message_id = candidate.append_agent_profile_transition(transition)?;
                candidate.projected_messages_for_provider().map_err(|error| {
                    PrimaryInstructionActivationError::Composition(
                        crate::instruction::SystemPromptActivationError::Compatibility(format!(
                            "append agent profile produced an invalid provider projection: {error}"
                        )),
                    )
                })?;
                candidate
                    .save()
                    .map_err(PrimaryInstructionActivationError::Persistence)?;
                self.session = candidate;
                self.reseed_context_runtime_from_session();
                Ok(super::AgentProfileChangeOutcome::Appended { agent, message_id })
            }
            super::AgentProfileChangeMode::ReplaceSystem => {
                let skills = self.current_skills_snapshot_for_working_dir(working_dir);
                let available_skills = skills
                    .list()
                    .iter()
                    .map(|skill| crate::prompt::SkillInfo {
                        name: skill.name.clone(),
                        description: skill.description.clone(),
                    })
                    .collect::<Vec<_>>();
                let replacement = composer.replace_system_prompt(
                    crate::instruction::SystemPromptActivationRequest {
                        working_dir,
                        selection,
                        is_selfdev: self.session.is_canary,
                        capabilities: crate::prompt::PromptCapabilities::current(),
                        available_skills: &available_skills,
                    },
                    &current,
                )?;
                let agent = replacement.activation.state.active_agent.clone();
                let mut candidate = self.session.clone();
                let audit_message_id = candidate.apply_system_prompt_replacement(
                    replacement.activation.state,
                    replacement.audit_sentence,
                )?;
                candidate.provider_session_id = None;
                let projected = candidate.projected_messages_for_provider().map_err(|error| {
                    PrimaryInstructionActivationError::Composition(
                        crate::instruction::SystemPromptActivationError::Compatibility(format!(
                            "replace system prompt produced an invalid provider projection: {error}"
                        )),
                    )
                })?;
                let mut split = self.build_system_prompt_split(None);
                split.static_part = candidate
                    .system_prompt_text()
                    .unwrap_or_default()
                    .to_string();
                let tools = match self.locked_tools.as_ref() {
                    Some(tools) => tools.clone(),
                    None => self.tool_definitions_for_debug().await,
                };
                let breakdown =
                    crate::context::request_token_breakdown(&projected, 0, 0, &split, &tools);
                let preflight = crate::context::evaluate_context_preflight(
                    candidate.context_view.revision,
                    self.provider.context_request_budget(),
                    breakdown,
                );
                if preflight.pressure == crate::protocol::ContextPressureLevel::Blocked {
                    return Err(PrimaryInstructionActivationError::ContextPreflight {
                        projected_input_tokens: preflight.projected_input_tokens,
                        safe_input_budget: preflight.safe_input_budget,
                        required_reduction_tokens: preflight.required_reduction_tokens,
                    });
                }
                candidate
                    .save()
                    .map_err(PrimaryInstructionActivationError::Persistence)?;
                self.session = candidate;
                crate::cache_invalidation::record(
                    "agent system replacement",
                    format!("active agent replaced with {}", agent.id),
                );
                self.cache_tracker.reset();
                self.provider_session_id = None;
                self.provider
                    .invalidate_context_continuation("agent system prompt replaced");
                self.reseed_context_runtime_from_session();
                Ok(super::AgentProfileChangeOutcome::Replaced {
                    agent,
                    audit_message_id: Some(audit_message_id),
                })
            }
        }
    }

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
        invalidations: std::sync::Arc<AtomicUsize>,
        systems: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
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
            self.systems
                .lock()
                .expect("record systems")
                .push(_system.to_string());
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

        fn invalidate_context_continuation(&self, _reason: &str) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
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

        fn path(&self) -> &std::path::Path {
            self._home.path()
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

    #[tokio::test]
    async fn primary_requests_freeze_static_prompt_across_disk_edits_and_restore() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider = RecordingProvider::default();
        let provider_handle: std::sync::Arc<dyn Provider> = std::sync::Arc::new(provider.clone());
        let (mut agent, _) = Agent::new_with_startup_context_and_agent(
            provider_handle,
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::RunCommand),
            crate::instruction::AgentSelection::Default,
            false,
        )
        .expect("primary activation");
        let frozen = agent
            .system_prompt_text()
            .expect("frozen system prompt")
            .to_string();
        let session_id = agent.session_id().to_string();

        agent.run_once("first turn").await.expect("first turn");
        assert!(agent.first_provider_dispatch_at().is_some());
        let common_path = home.path().join("instructions/system/common.md");
        let common = crate::instruction::InstructionDocument {
            id: crate::instruction::InstructionId::parse("common").expect("id"),
            kind: crate::instruction::InstructionKind::System,
            scope: crate::instruction::InstructionScope::Global,
            template_mode: crate::instruction::TemplateMode::Plain,
            metadata: crate::instruction::InstructionMetadata::default(),
            body: "SYNTHETIC_DISK_EDIT".to_string(),
            path: std::path::PathBuf::from("system/common.md"),
        };
        std::fs::write(common_path, common.to_markdown().expect("serialize"))
            .expect("edit managed source");
        agent.run_once("second turn").await.expect("second turn");

        let systems = provider.systems.lock().expect("systems").clone();
        assert_eq!(systems.len(), 2);
        assert_eq!(systems[0], frozen);
        assert_eq!(systems[1], frozen);
        assert!(!systems[1].contains("SYNTHETIC_DISK_EDIT"));

        let restored_provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(provider.clone());
        let mut restored = Agent::new_with_disabled_startup_context(
            restored_provider,
            crate::tool::Registry::empty(),
            None,
        );
        restored.restore_session(&session_id).expect("restore");
        assert_eq!(restored.system_prompt_text(), Some(frozen.as_str()));
        restored
            .run_once("restored turn")
            .await
            .expect("restored turn");
        let systems = provider.systems.lock().expect("systems");
        assert_eq!(systems.len(), 3);
        assert_eq!(systems[2], frozen);
    }

    #[test]
    fn direct_clear_retains_agent_renders_current_source_and_clears_active_skill() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let (mut agent, _) = Agent::new_with_startup_context_and_agent(
            provider,
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::InteractiveRepl),
            crate::instruction::AgentSelection::Default,
            false,
        )
        .expect("primary activation");
        let old_session_id = agent.session_id().to_string();
        let retained_agent = agent.active_agent().cloned().expect("active agent");
        agent.session.active_skill = Some(crate::session::StoredActiveSkill {
            skill_id: "synthetic-skill".to_string(),
            rendered_text: "SYNTHETIC_ACTIVE_SKILL".to_string(),
        });
        let common = crate::instruction::InstructionDocument {
            id: crate::instruction::InstructionId::parse("common").expect("id"),
            kind: crate::instruction::InstructionKind::System,
            scope: crate::instruction::InstructionScope::Global,
            template_mode: crate::instruction::TemplateMode::Plain,
            metadata: crate::instruction::InstructionMetadata::default(),
            body: "SYNTHETIC_DIRECT_CLEAR_CURRENT_SOURCE".to_string(),
            path: std::path::PathBuf::from("system/common.md"),
        };
        std::fs::write(
            home.path().join("instructions/system/common.md"),
            common.to_markdown().expect("serialize"),
        )
        .expect("edit managed common");

        agent.clear().expect("direct clear");
        assert_ne!(agent.session_id(), old_session_id);
        assert_eq!(agent.active_agent(), Some(&retained_agent));
        assert!(
            agent
                .system_prompt_text()
                .expect("new frozen prompt")
                .contains("SYNTHETIC_DIRECT_CLEAR_CURRENT_SOURCE")
        );
        assert!(agent.first_provider_dispatch_at().is_none());
        assert!(agent.session.active_skill.is_none());
        let stored = Session::load(agent.session_id()).expect("load cleared session");
        assert_eq!(stored.active_agent(), Some(&retained_agent));
        assert!(stored.system_prompt_text().is_some());
    }

    #[test]
    fn failed_direct_clear_does_not_replace_the_live_session() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let (mut agent, _) = Agent::new_with_startup_context_and_agent(
            provider,
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::InteractiveRepl),
            crate::instruction::AgentSelection::Default,
            false,
        )
        .expect("primary activation");
        let before = agent.session.clone();
        let kernel = home.path().join("instructions/system/kernel.md");
        std::fs::remove_file(kernel).expect("damage store");

        agent.clear().expect_err("damaged store must block clear");
        assert_eq!(agent.session.id, before.id);
        assert_eq!(agent.session.system_prompt, before.system_prompt);
        assert_eq!(
            serde_json::to_vec(&agent.session.messages).expect("serialize current messages"),
            serde_json::to_vec(&before.messages).expect("serialize previous messages")
        );
        assert_eq!(
            agent.session.provider_session_id,
            before.provider_session_id
        );
    }

    #[test]
    fn old_session_migration_uses_compatibility_agent_even_when_default_changed() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        crate::instruction::SystemPromptComposer::new()
            .activate(crate::instruction::SystemPromptActivationRequest {
                working_dir: Some(project.path()),
                selection: crate::instruction::AgentSelection::Default,
                is_selfdev: false,
                capabilities: crate::prompt::PromptCapabilities { mermaid: false },
                available_skills: &[],
            })
            .expect("initialize store");
        let synthetic = crate::instruction::InstructionDocument {
            id: crate::instruction::InstructionId::parse("synthetic").expect("id"),
            kind: crate::instruction::InstructionKind::Agent,
            scope: crate::instruction::InstructionScope::Global,
            template_mode: crate::instruction::TemplateMode::Plain,
            metadata: crate::instruction::InstructionMetadata {
                display_name: Some("Synthetic".to_string()),
                description: Some("synthetic fixture".to_string()),
                agent: Some(crate::instruction::AgentMetadata {
                    availability: crate::instruction::AgentAvailability::Both,
                }),
                ..crate::instruction::InstructionMetadata::default()
            },
            body: "SYNTHETIC_DEFAULT".to_string(),
            path: std::path::PathBuf::from("agents/synthetic.md"),
        };
        std::fs::write(
            home.path().join("instructions/agents/synthetic.md"),
            synthetic.to_markdown().expect("serialize"),
        )
        .expect("write synthetic agent");
        std::fs::write(
            home.path().join("instructions/instruction-store.toml"),
            "schema_version = 1\ndefault_agent = \"global:synthetic\"\n",
        )
        .expect("write default");

        let mut old =
            Session::create_with_id("session_old_prompt_migration".to_string(), None, None);
        old.working_dir = Some(project.path().to_string_lossy().into_owned());
        old.provider_session_id = Some("stale-provider-continuation".to_string());
        old.save().expect("save old session");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let mut agent = Agent::new_with_disabled_startup_context(
            provider,
            crate::tool::Registry::empty(),
            None,
        );
        agent
            .restore_session("session_old_prompt_migration")
            .expect("migrate old session");
        assert_eq!(
            agent.active_agent().map(|agent| agent.id.as_str()),
            Some("jcode")
        );
        assert!(agent.provider_session_id.is_none());
        assert!(agent.session.provider_session_id.is_none());
        assert!(
            !agent
                .system_prompt_text()
                .expect("migrated system")
                .contains("SYNTHETIC_DEFAULT")
        );
        let stored = Session::load("session_old_prompt_migration").expect("reload migrated");
        assert_eq!(
            stored.active_agent().map(|agent| agent.id.as_str()),
            Some("jcode")
        );
        assert!(stored.provider_session_id.is_none());
    }

    #[test]
    fn failed_old_session_migration_leaves_live_agent_untouched() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let (mut agent, _) = Agent::new_with_startup_context_and_agent(
            provider,
            crate::tool::Registry::empty(),
            project.path().to_str(),
            StartupContextActivation::primary(StartupContextCaller::RunCommand),
            crate::instruction::AgentSelection::Default,
            false,
        )
        .expect("current primary activation");
        agent.provider_session_id = Some("live-runtime-continuation".to_string());
        agent.session.provider_session_id = Some("live-stored-continuation".to_string());
        agent.pending_alerts.push("preserve-alert".to_string());
        agent.current_turn_system_reminder = Some("preserve-reminder".to_string());
        agent.locked_tools = Some(Vec::new());
        agent.mcp_late_register_resolved = true;
        agent.background_tool_signal.fire();
        agent.graceful_shutdown.fire();
        agent
            .soft_interrupt_queue
            .lock()
            .expect("soft interrupt queue")
            .push(crate::agent::SoftInterruptMessage {
                content: "preserve-interrupt".to_string(),
                images: Vec::new(),
                urgent: true,
                source: crate::agent::SoftInterruptSource::User,
                unattended_context: None,
            });
        let before_session = agent.session.clone();
        let before_provider_session_id = agent.provider_session_id.clone();

        let mut target =
            Session::create_with_id("session_failed_prompt_migration".to_string(), None, None);
        target.working_dir = Some(project.path().to_string_lossy().into_owned());
        target.provider_session_id = Some("target-stale-continuation".to_string());
        target.save().expect("save legacy target");
        let kernel = home.path().join("instructions/system/kernel.md");
        let kernel_content = std::fs::read(&kernel).expect("read kernel");
        std::fs::remove_file(&kernel).expect("damage store");

        agent
            .restore_session(&target.id)
            .expect_err("damaged migration must fail");
        assert_eq!(agent.session.id, before_session.id);
        assert_eq!(agent.session.system_prompt, before_session.system_prompt);
        assert_eq!(
            agent.session.provider_session_id,
            before_session.provider_session_id
        );
        assert_eq!(agent.provider_session_id, before_provider_session_id);
        assert_eq!(agent.pending_alerts, ["preserve-alert"]);
        assert_eq!(
            agent.current_turn_system_reminder.as_deref(),
            Some("preserve-reminder")
        );
        assert!(agent.locked_tools.is_some());
        assert!(agent.mcp_late_register_resolved);
        assert!(agent.background_tool_signal.is_set());
        assert!(agent.graceful_shutdown.is_set());
        assert_eq!(
            agent
                .soft_interrupt_queue
                .lock()
                .expect("soft interrupt queue")
                .first()
                .map(|message| message.content.as_str()),
            Some("preserve-interrupt")
        );
        let unchanged_target = Session::load(&target.id).expect("reload unchanged target");
        assert!(unchanged_target.system_prompt.is_none());
        assert_eq!(
            unchanged_target.provider_session_id.as_deref(),
            Some("target-stale-continuation")
        );

        std::fs::write(&kernel, kernel_content).expect("repair store");
        agent
            .restore_session(&target.id)
            .expect("restore after repair");
        assert!(agent.provider_session_id.is_none());
        assert!(agent.session.provider_session_id.is_none());
        let migrated = Session::load(&target.id).expect("reload migrated target");
        assert!(migrated.system_prompt.is_some());
        assert!(migrated.provider_session_id.is_none());
    }

    #[test]
    fn unqualified_same_id_selection_re_resolves_project_specificity() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let mut agent = Agent::new_with_disabled_startup_context(
            provider,
            crate::tool::Registry::empty(),
            project.path().to_str(),
        );
        agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Explicit(
                crate::instruction::InstructionSelector::global(
                    crate::instruction::InstructionKind::Agent,
                    "jcode",
                )
                .expect("global selector"),
            ))
            .expect("global activation");
        assert_eq!(
            agent.active_agent().map(|agent| agent.scope),
            Some(crate::instruction::InstructionScope::Global)
        );

        let project_agent = crate::instruction::InstructionDocument {
            id: crate::instruction::InstructionId::parse("jcode").expect("id"),
            kind: crate::instruction::InstructionKind::Agent,
            scope: crate::instruction::InstructionScope::Project,
            template_mode: crate::instruction::TemplateMode::Plain,
            metadata: crate::instruction::InstructionMetadata {
                display_name: Some("Project Jcode".to_string()),
                description: Some("synthetic project agent".to_string()),
                agent: Some(crate::instruction::AgentMetadata {
                    availability: crate::instruction::AgentAvailability::Both,
                }),
                ..crate::instruction::InstructionMetadata::default()
            },
            body: "SYNTHETIC_PROJECT_SPECIFIC_JCODE".to_string(),
            path: std::path::PathBuf::from("agents/jcode.md"),
        };
        crate::instruction::InstructionRepositoryService::new()
            .configure_non_git_project(
                project.path(),
                "setup-project-specific-jcode",
                None,
                &crate::instruction::InstructionStoreSeed {
                    manifest: crate::instruction::InstructionStoreManifest::current(),
                    files: vec![crate::instruction::InstructionSeedFile {
                        relative_path: project_agent.path.clone(),
                        content: project_agent.to_markdown().expect("serialize").into_bytes(),
                    }],
                },
                &[],
            )
            .expect("configure project store");

        agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Explicit(
                crate::instruction::InstructionSelector::unqualified(
                    crate::instruction::InstructionKind::Agent,
                    "jcode",
                )
                .expect("unqualified selector"),
            ))
            .expect("resolve project specificity");
        assert_eq!(
            agent.active_agent().map(|agent| agent.scope),
            Some(crate::instruction::InstructionScope::Project)
        );
        assert!(
            agent
                .system_prompt_text()
                .expect("project prompt")
                .contains("SYNTHETIC_PROJECT_SPECIFIC_JCODE")
        );
        assert!(home.path().join("instructions").is_dir());
    }

    #[test]
    fn same_agent_selection_is_source_free_and_replacement_save_failure_restores_state() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider: std::sync::Arc<dyn Provider> =
            std::sync::Arc::new(RecordingProvider::default());
        let mut agent = Agent::new_with_disabled_startup_context(
            provider,
            crate::tool::Registry::empty(),
            project.path().to_str(),
        );
        agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Default)
            .expect("initial activation");
        let before = agent.session.clone();

        let store = home.path().join("instructions");
        let moved_store = home.path().join("instructions-away");
        std::fs::rename(&store, &moved_store).expect("move store");
        let same =
            agent.activate_primary_instructions(crate::instruction::AgentSelection::Explicit(
                crate::instruction::InstructionSelector::global(
                    crate::instruction::InstructionKind::Agent,
                    "jcode",
                )
                .expect("selector"),
            ));
        assert!(same.is_ok(), "same-agent no-op must not read the store");
        assert_eq!(agent.session.system_prompt, before.system_prompt);
        assert_eq!(agent.session.updated_at, before.updated_at);
        std::fs::rename(&moved_store, &store).expect("restore store");

        let replacement = crate::instruction::InstructionDocument {
            id: crate::instruction::InstructionId::parse("replacement").expect("id"),
            kind: crate::instruction::InstructionKind::Agent,
            scope: crate::instruction::InstructionScope::Global,
            template_mode: crate::instruction::TemplateMode::Plain,
            metadata: crate::instruction::InstructionMetadata {
                display_name: Some("Replacement".to_string()),
                description: Some("synthetic replacement".to_string()),
                agent: Some(crate::instruction::AgentMetadata {
                    availability: crate::instruction::AgentAvailability::Both,
                }),
                ..crate::instruction::InstructionMetadata::default()
            },
            body: "SYNTHETIC_REPLACEMENT".to_string(),
            path: std::path::PathBuf::from("agents/replacement.md"),
        };
        std::fs::write(
            store.join("agents/replacement.md"),
            replacement.to_markdown().expect("serialize"),
        )
        .expect("write replacement");
        let sessions = home.path().join("sessions");
        std::fs::remove_dir_all(&sessions).expect("remove test sessions");
        std::fs::write(&sessions, "block session directory").expect("block session saves");

        let error = agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Explicit(
                crate::instruction::InstructionSelector::global(
                    crate::instruction::InstructionKind::Agent,
                    "replacement",
                )
                .expect("selector"),
            ))
            .expect_err("replacement persistence must fail");
        assert!(matches!(
            error,
            PrimaryInstructionActivationError::Persistence(_)
        ));
        assert_eq!(agent.session.system_prompt, before.system_prompt);
        assert_eq!(agent.session.updated_at, before.updated_at);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_dispatch_switch_appends_and_explicit_replace_resets_only_prompt_state() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider = RecordingProvider::default();
        let provider_handle: std::sync::Arc<dyn Provider> = std::sync::Arc::new(provider.clone());
        let mut agent = Agent::new_with_disabled_startup_context(
            provider_handle,
            crate::tool::Registry::empty(),
            project.path().to_str(),
        );
        agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Default)
            .expect("initial activation");
        agent
            .session
            .system_prompt
            .as_mut()
            .expect("system prompt")
            .first_provider_dispatch_at = Some(chrono::Utc::now());
        agent.session.model = Some("synthetic-model".to_string());
        agent.session.reasoning_effort = Some("high".to_string());
        agent.session.provider_session_id = Some("stored-continuation".to_string());
        agent.provider_session_id = Some("live-continuation".to_string());
        let _ = agent.tool_definitions().await;
        assert!(agent.locked_tools.is_some());
        let invalidations_before = provider.invalidations.load(Ordering::SeqCst);

        let store = home.path().join("instructions");
        let agent_path = store.join("agents/reviewer.md");
        std::fs::write(
            &agent_path,
            "---\nid: reviewer\nkind: agent\nname: Reviewer\ndescription: Synthetic reviewer\navailability: both\n---\n\nSYNTHETIC_APPEND_PROFILE",
        )
        .expect("write reviewer");
        let selection = crate::instruction::AgentSelection::Explicit(
            crate::instruction::InstructionSelector::global(
                crate::instruction::InstructionKind::Agent,
                "reviewer",
            )
            .expect("selector"),
        );
        agent.session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: "visible user turn".to_string(),
                cache_control: None,
            }],
        );
        let before_messages = agent.session.messages.len();
        let outcome = agent
            .change_primary_agent(
                selection.clone(),
                crate::agent::AgentProfileChangeMode::Ordinary,
            )
            .await
            .expect("append switch");
        let crate::agent::AgentProfileChangeOutcome::Appended { message_id, .. } = outcome else {
            panic!("expected appended profile")
        };
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.invalidations.load(Ordering::SeqCst),
            invalidations_before
        );
        assert_eq!(agent.session.messages.len(), before_messages + 1);
        assert_eq!(
            agent.active_transition_message_id(),
            Some(message_id.as_str())
        );
        assert_eq!(agent.session.model.as_deref(), Some("synthetic-model"));
        assert_eq!(agent.session.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            agent.provider_session_id.as_deref(),
            Some("live-continuation")
        );
        assert!(agent.locked_tools.is_some());

        agent.session.add_message(
            crate::message::Role::Assistant,
            vec![crate::message::ContentBlock::Text {
                text: "visible assistant turn".to_string(),
                cache_control: None,
            }],
        );
        agent.session.save().expect("persist assistant turn");
        assert_eq!(agent.rewind_to_message(1).expect("rewind"), 1);
        assert_eq!(
            agent
                .session
                .messages
                .iter()
                .filter(|message| message.id == message_id)
                .count(),
            1
        );
        assert_eq!(
            agent.active_transition_message_id(),
            Some(message_id.as_str())
        );
        assert_eq!(agent.undo_rewind().expect("undo rewind"), 1);
        assert_eq!(
            agent
                .session
                .messages
                .iter()
                .filter(|message| message.id == message_id)
                .count(),
            1
        );
        assert_eq!(
            agent.active_transition_message_id(),
            Some(message_id.as_str())
        );
        let _ = agent.tool_definitions().await;
        assert!(agent.locked_tools.is_some());

        let moved_store = home.path().join("instructions-away");
        std::fs::rename(&store, &moved_store).expect("move store");
        let same = agent
            .change_primary_agent(
                selection.clone(),
                crate::agent::AgentProfileChangeMode::Ordinary,
            )
            .await
            .expect("same-agent source-free no-op");
        assert!(matches!(
            same,
            crate::agent::AgentProfileChangeOutcome::NoChange { .. }
        ));
        std::fs::rename(&moved_store, &store).expect("restore store");

        std::fs::write(
            store.join("agents/failing.md"),
            "---\nid: failing\nkind: agent\nname: Failing\ndescription: Synthetic failing agent\navailability: both\n---\n\nSYNTHETIC_FAILING_PROFILE",
        )
        .expect("write failing agent");
        let before_failed_append =
            serde_json::to_vec(&agent.session).expect("serialize before failed append");
        let sessions_dir = home.path().join("sessions");
        std::fs::remove_dir_all(&sessions_dir).expect("remove sessions for append failure");
        std::fs::write(&sessions_dir, "block session persistence")
            .expect("block append persistence");
        let append_error = agent
            .change_primary_agent(
                crate::instruction::AgentSelection::Explicit(
                    crate::instruction::InstructionSelector::global(
                        crate::instruction::InstructionKind::Agent,
                        "failing",
                    )
                    .expect("failing selector"),
                ),
                crate::agent::AgentProfileChangeMode::Ordinary,
            )
            .await
            .expect_err("failed append persistence must reject");
        assert!(matches!(
            append_error,
            PrimaryInstructionActivationError::Persistence(_)
        ));
        assert_eq!(
            serde_json::to_vec(&agent.session).expect("serialize after failed append"),
            before_failed_append
        );
        std::fs::remove_file(&sessions_dir).expect("remove append blocker");
        std::fs::create_dir_all(&sessions_dir).expect("restore sessions directory");
        agent.session.save().expect("restore persisted session");

        std::fs::write(
            &agent_path,
            "---\nid: reviewer\nkind: agent\nname: Reviewer\ndescription: Synthetic reviewer\navailability: both\n---\n\nSYNTHETIC_REPLACED_PROFILE",
        )
        .expect("update reviewer");
        agent.session.provider_session_id = Some("stored-replacement-continuation".to_string());
        agent.provider_session_id = Some("live-replacement-continuation".to_string());
        agent.session.save().expect("persist replacement setup");
        let before_failed_replacement =
            serde_json::to_vec(&agent.session).expect("serialize before failed replacement");
        std::fs::remove_dir_all(&sessions_dir).expect("remove sessions for replacement failure");
        std::fs::write(&sessions_dir, "block session persistence")
            .expect("block replacement persistence");
        let replacement_error = agent
            .change_primary_agent(
                selection.clone(),
                crate::agent::AgentProfileChangeMode::ReplaceSystem,
            )
            .await
            .expect_err("failed replacement persistence must reject");
        assert!(matches!(
            replacement_error,
            PrimaryInstructionActivationError::Persistence(_)
        ));
        assert_eq!(
            serde_json::to_vec(&agent.session).expect("serialize after failed replacement"),
            before_failed_replacement
        );
        assert_eq!(
            agent.provider_session_id.as_deref(),
            Some("live-replacement-continuation")
        );
        std::fs::remove_file(&sessions_dir).expect("remove replacement blocker");
        std::fs::create_dir_all(&sessions_dir).expect("restore sessions directory");
        agent.session.save().expect("restore replacement setup");
        let invalidations_before_replacement = provider.invalidations.load(Ordering::SeqCst);
        let outcome = agent
            .change_primary_agent(
                selection,
                crate::agent::AgentProfileChangeMode::ReplaceSystem,
            )
            .await
            .expect("explicit replacement");
        assert!(matches!(
            outcome,
            crate::agent::AgentProfileChangeOutcome::Replaced {
                audit_message_id: Some(_),
                ..
            }
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.invalidations.load(Ordering::SeqCst),
            invalidations_before_replacement + 1
        );
        assert_eq!(agent.provider_session_id, None);
        assert_eq!(agent.session.provider_session_id, None);
        assert_eq!(agent.active_transition_message_id(), None);
        assert!(
            agent
                .system_prompt_text()
                .expect("replacement prompt")
                .contains("SYNTHETIC_REPLACED_PROFILE")
        );
        assert_eq!(agent.session.model.as_deref(), Some("synthetic-model"));
        assert_eq!(agent.session.reasoning_effort.as_deref(), Some("high"));
        assert!(agent.locked_tools.is_some());

        let saved = Session::load(&agent.session.id).expect("load persisted replacement");
        assert_eq!(saved.provider_session_id, None);
        assert_eq!(saved.active_transition_message_id(), None);
        assert!(
            saved
                .system_prompt_text()
                .expect("saved prompt")
                .contains("SYNTHETIC_REPLACED_PROFILE")
        );
        agent.session.active_skill = Some(crate::session::StoredActiveSkill {
            skill_id: "synthetic-skill".to_string(),
            rendered_text: "SYNTHETIC_ACTIVE_SKILL_EXPORT".to_string(),
        });
        let markdown = agent.export_conversation_markdown();
        assert!(markdown.contains("SYNTHETIC_REPLACED_PROFILE"));
        assert!(markdown.contains("SYNTHETIC_ACTIVE_SKILL_EXPORT"));
        assert!(markdown.contains("SYNTHETIC_APPEND_PROFILE"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_dispatch_explicit_replace_is_provisional_and_cache_neutral() {
        let home = TestHome::new();
        let project = tempfile::tempdir().expect("project");
        let provider = RecordingProvider::default();
        let provider_handle: std::sync::Arc<dyn Provider> = std::sync::Arc::new(provider.clone());
        let mut agent = Agent::new_with_disabled_startup_context(
            provider_handle,
            crate::tool::Registry::empty(),
            project.path().to_str(),
        );
        agent
            .activate_primary_instructions(crate::instruction::AgentSelection::Default)
            .expect("initial activation");
        std::fs::write(
            home.path().join("instructions/agents/provisional.md"),
            "---\nid: provisional\nkind: agent\nname: Provisional\ndescription: Synthetic provisional agent\navailability: both\n---\n\nSYNTHETIC_PROVISIONAL_PROFILE",
        )
        .expect("write provisional agent");
        let invalidations_before = provider.invalidations.load(Ordering::SeqCst);
        let messages_before =
            serde_json::to_vec(&agent.session.messages).expect("serialize prior messages");
        let outcome = agent
            .change_primary_agent(
                crate::instruction::AgentSelection::Explicit(
                    crate::instruction::InstructionSelector::global(
                        crate::instruction::InstructionKind::Agent,
                        "provisional",
                    )
                    .expect("selector"),
                ),
                crate::agent::AgentProfileChangeMode::ReplaceSystem,
            )
            .await
            .expect("pre-dispatch replace");
        assert!(matches!(
            outcome,
            crate::agent::AgentProfileChangeOutcome::Provisional { .. }
        ));
        assert!(agent.first_provider_dispatch_at().is_none());
        assert!(agent.active_transition_message_id().is_none());
        assert_eq!(
            serde_json::to_vec(&agent.session.messages).expect("serialize current messages"),
            messages_before
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.invalidations.load(Ordering::SeqCst),
            invalidations_before
        );
        assert!(
            agent
                .system_prompt_text()
                .expect("provisional prompt")
                .contains("SYNTHETIC_PROVISIONAL_PROFILE")
        );
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
