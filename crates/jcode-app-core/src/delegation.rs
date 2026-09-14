//! Isolated conversations hosted through ordinary Agent, Session and execution owners.
use crate::agent::Agent;
use crate::execution::{ChildTurnClaim, ExecutionStore};
use crate::instruction::{
    AgentSelection, InstructionReadPolicy, InstructionRepositoryService,
    SystemPromptActivationRequest, SystemPromptComposer,
};
use crate::model_roster::{ModelRosterRequest, ModelRosterService, RosterCatalog};
use crate::provider::Provider;
use crate::session::{IsolatedChildIdentity, Session};
use crate::tool::Registry;
use anyhow::{Context, Result, ensure};
use jcode_agent_runtime::InterruptSignal;
use jcode_tool_core::{OutputStream, Tool, ToolContext, ToolExecutionMode};
use jcode_tool_types::delegation::*;
use jcode_tool_types::{OutputSource, StopCause, ToolOutput};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

mod idle;
mod relay;
#[cfg(test)]
mod tests;
pub(crate) use relay::{forward, serve_request};

pub(crate) fn release_idle_runtimes() -> Result<()> {
    idle::clear(&crate::storage::jcode_dir()?);
    Ok(())
}

pub(crate) struct Host {
    provider: Arc<dyn Provider>,
    pool: Arc<crate::mcp::SharedMcpPool>,
    repositories: InstructionRepositoryService,
    root: PathBuf,
}
impl Host {
    pub(crate) fn new(
        provider: Arc<dyn Provider>,
        pool: Arc<crate::mcp::SharedMcpPool>,
        repositories: InstructionRepositoryService,
    ) -> Result<Self> {
        Ok(Self {
            provider,
            pool,
            repositories,
            root: crate::storage::jcode_dir()?,
        })
    }

    fn authorize_parent(&self, parent: &str) -> Result<()> {
        ensure!(
            !crate::server::server_reload_starting(),
            "Delegation host is reloading; no new child work was admitted"
        );
        let (_, original_parent) = Session::inspection_relationships(&self.root, parent, None)?;
        ensure!(
            original_parent.is_none(),
            "Isolated children cannot delegate or administer other child conversations"
        );
        Ok(())
    }

    pub(crate) async fn catalog(&self, mut input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        self.authorize_parent(&ctx.session_id)?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CatalogInput {
            working_dir: Option<PathBuf>,
        }
        let object = input
            .as_object_mut()
            .context("Catalog input must be an object")?;
        object.remove("intent");
        let input: CatalogInput = serde_json::from_value(input)?;
        let cwd = resolve_working_dir(input.working_dir, ctx.working_dir.as_deref())?;
        check_stop(&ctx)?;
        let composer = SystemPromptComposer::from_repository_service(self.repositories.clone());
        let instructions = composer.delegation_catalog(Some(&cwd))?;
        let roster = ModelRosterService::new(self.repositories.clone())
            .load(InstructionReadPolicy::WorkingTreeOnly)?;
        let output = json!({"profiles":instructions.profiles,"model_aliases":roster.list(),"task_presets":instructions.task_presets,"diagnostics":{"instructions":instructions.diagnostics,"models":roster.validate()}});
        check_stop(&ctx)?;
        Ok(ToolOutput::new(serde_json::to_string_pretty(&output)?))
    }

    pub(crate) async fn submit(
        &self,
        request: SubagentRequest,
        ctx: ToolContext,
    ) -> Result<ToolOutput> {
        self.authorize_parent(&ctx.session_id)?;
        let identity = ctx
            .invocation
            .identity
            .as_ref()
            .context("Child submission has no execution owner")?;
        let store = ExecutionStore::open(&self.root)?;
        let recovery_issues = store.recover_abandoned_children().await?;
        let child_id = match &request.request {
            ChildRequest::Create(_) => ChildSessionId::parse(format!(
                "session_child_{}",
                identity
                    .id
                    .strip_prefix("run-")
                    .context("Invalid child invocation ID")?
            ))
            .map_err(anyhow::Error::msg)?,
            ChildRequest::Send(message) => message.child_id.clone(),
        };
        let initial = matches!(request.request, ChildRequest::Create(_));
        let existing = if initial {
            None
        } else {
            // Fixed ownership and artifact metadata do not need an Agent lock or prompt bodies.
            let child = Session::load_startup_stub(child_id.as_str())?
                .isolated_child
                .context("Target is not an isolated child")?;
            ensure!(
                child.identity.original_parent == ctx.session_id,
                "Only the original parent may send to or control this child"
            );
            // Proven dead owners are recovered, never resent. Live predecessors stay queued.
            for run in store.unfinished_child_turns(child_id.as_str())? {
                let _ = store.recover_lost_owner(&run).await?;
            }
            Some(child)
        };
        check_stop(&ctx)?;
        let queue =
            matches!(&request.request, ChildRequest::Send(message) if message.queue_if_busy);
        let admission = store
            .admit_child_turn(
                &identity.id,
                &identity.owner,
                child_id.as_str(),
                initial,
                queue,
                crate::config::config()
                    .delegation
                    .max_running_children
                    .get(),
            )
            .with_context(|| {
                if recovery_issues.is_empty() {
                    "Child admission failed".to_string()
                } else {
                    format!(
                        "Child admission failed; unresolved prior work: {}",
                        recovery_issues.join(" | ")
                    )
                }
            })?;
        if admission.queued {
            let child = existing
                .as_ref()
                .context("Queued child has no original identity")?;
            mark_ready(
                &ctx,
                child_id.clone(),
                child.identity.artifact_dir.clone(),
                true,
            )?;
            let stop = ctx
                .graceful_shutdown_signal
                .as_ref()
                .context("Child queue has no Stop signal")?;
            loop {
                check_stop(&ctx)?;
                match store.claim_child_turn(&identity.id, &identity.owner)? {
                    ChildTurnClaim::Claimed => break,
                    ChildTurnClaim::CancelledBy(predecessor) => {
                        stop.fire_with_cause(StopCause::ChildPredecessorFailure);
                        anyhow::bail!(
                            "Queued child input cancelled after {predecessor}; its original invocation input remains retained"
                        );
                    }
                    ChildTurnClaim::Waiting => {
                        let pending = store.unfinished_child_turns(child_id.as_str())?;
                        if let Some(predecessor) = pending.first()
                            && predecessor != &identity.id
                        {
                            crate::execution::await_terminal(&store, predecessor, stop).await?;
                        }
                    }
                }
            }
        }
        let mut agent = match request.request {
            ChildRequest::Create(request) => self.create(&ctx, child_id.clone(), request).await?,
            ChildRequest::Send(message) => self.restore_turn(&ctx, &message).await?,
        };
        let result = self.run(&ctx, &mut agent).await;
        idle::put(&self.root, agent);
        result
    }

    async fn create(
        &self,
        ctx: &ToolContext,
        child_id: ChildSessionId,
        request: CreateChild,
    ) -> Result<Agent> {
        ensure!(
            !crate::session::session_exists(child_id.as_str()),
            "Child identity already exists; no replacement conversation was created"
        );
        let mut unpublished = UnpublishedChild {
            session_id: child_id.as_str().into(),
            owns_session: false,
            artifact: None,
            published: false,
        };
        let result: Result<Agent> = async {
            check_stop(ctx)?;
            let cwd = resolve_working_dir(request.working_dir.clone(), ctx.working_dir.as_deref())?;
            let registry = Registry::new(self.provider.clone()).await;
            let global = registry.skills().read().await.clone();
            let skills = crate::skill::SkillRegistry::effective_for_working_dir_with_repositories(
                &global,
                Some(&cwd),
                &self.repositories,
            );
            let available = skills
                .list()
                .iter()
                .map(|skill| crate::prompt::SkillInfo {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                })
                .collect::<Vec<_>>();
            let composer = SystemPromptComposer::from_repository_service(self.repositories.clone());
            let activation = composer.activate_isolated(SystemPromptActivationRequest {
                working_dir: Some(&cwd),
                selection: AgentSelection::parse(Some(&request.agent))?,
                is_selfdev: false,
                capabilities: crate::prompt::PromptCapabilities::current(),
                available_skills: &available,
            })?;
            let profile = activation.state.active_agent.clone();
            let preset = composer
                .activate_task_preset(Some(&cwd), request.preset.as_deref(), None)?
                .context("Initial task preset was not activated")?;
            let catalog = RosterCatalog::from_provider(self.provider.as_ref())?;
            let selection = ModelRosterRequest {
                alias: Some(request.model_alias.clone()),
                effort_override: request.effort.clone(),
                model_override: None,
            };
            let prepared = ModelRosterService::new(self.repositories.clone()).resolve(
                &selection,
                &catalog,
                InstructionReadPolicy::WorkingTreeOnly,
            )?;
            ensure!(
                !prepared.provider.handles_tools_internally(),
                "Selected model route cannot enforce the child's native-tool policy"
            );
            let mut session = Session::create_with_id(child_id.as_str().into(), None, None);
            session.working_dir = Some(
                cwd.to_str()
                    .context("Child working directory is not UTF-8")?
                    .into(),
            );
            session.install_system_prompt(activation.state);
            prepare_startup(&mut session, &request.startup_context, &cwd)?;
            check_stop(ctx)?;
            let artifact_parent = self.root.join("artifacts");
            crate::storage::ensure_dir(&artifact_parent)?;
            ensure!(
                !std::fs::symlink_metadata(&artifact_parent)?
                    .file_type()
                    .is_symlink(),
                "Child artifact root cannot be a symlink"
            );
            let artifact = artifact_parent.join(child_id.as_str());
            std::fs::create_dir(&artifact)
                .context("Allocate a new child-owned artifact directory")?;
            unpublished.artifact = Some(artifact.clone());
            jcode_core::fs::set_directory_permissions_owner_only(&artifact)?;
            let artifact = artifact.canonicalize()?;
            session.install_isolated_child(
                IsolatedChildIdentity {
                    profile,
                    original_parent: ctx.session_id.clone(),
                    creation_run: crate::execution::invocation_id(ctx),
                    working_dir: cwd.clone(),
                    artifact_dir: artifact.clone(),
                    resolution: prepared.resolution,
                    blocked_mcps: request.blocked_mcps.clone(),
                },
                request.permission,
                preset,
            )?;
            let policy = crate::mcp::McpAccessPolicy {
                permission: request.permission,
                blocked: request.blocked_mcps,
            };
            registry
                .register_isolated_mcp_tools(
                    self.pool.clone(),
                    child_id.as_str().into(),
                    cwd,
                    policy,
                )
                .await?;
            check_stop(ctx)?;
            let mut agent = Agent::from_isolated_session(
                prepared.provider,
                registry,
                session,
                self.repositories.clone(),
            )?;
            unpublished.owns_session = true;
            agent
                .prepare_isolated_turn(
                    &crate::execution::invocation_id(ctx),
                    &request.prompt,
                    None,
                    None,
                )
                .await?;
            mark_ready(ctx, child_id, artifact, false)?;
            unpublished.published = true;
            Ok(agent)
        }
        .await;
        if let Err(error) = result {
            if let Err(cleanup) = unpublished.cleanup() {
                return Err(error.context(format!(
                    "Unpublished child cleanup also failed: {cleanup:#}"
                )));
            }
            return Err(error);
        }
        result
    }

    async fn restore_turn(&self, ctx: &ToolContext, message: &ChildMessage) -> Result<Agent> {
        check_stop(ctx)?;
        if let Some(mut agent) = idle::take(&self.root, message.child_id.as_str()) {
            let saved = Session::load_startup_stub(message.child_id.as_str())?;
            let current = agent.startup_context_session();
            let child = current
                .isolated_child
                .as_ref()
                .context("Cached child lost its origin")?;
            ensure!(
                child.identity.original_parent == ctx.session_id,
                "Child belongs to another parent"
            );
            if saved.updated_at == current.updated_at
                && message
                    .permission
                    .is_none_or(|permission| permission == child.permission)
            {
                let preset =
                    SystemPromptComposer::from_repository_service(self.repositories.clone())
                        .activate_task_preset(
                            Some(&child.identity.working_dir),
                            message.preset.as_deref(),
                            Some(&child.preset),
                        )?;
                let artifact = child.identity.artifact_dir.clone();
                agent.observe_startup_context_before_user_turn()?;
                if let Err(error) = agent
                    .prepare_isolated_turn(
                        &crate::execution::invocation_id(ctx),
                        &message.prompt,
                        message.permission,
                        preset,
                    )
                    .await
                {
                    idle::put(&self.root, agent);
                    return Err(error);
                }
                mark_ready(ctx, message.child_id.clone(), artifact, false)?;
                return Ok(agent);
            }
            // Explicit permission changes or external context transactions need
            // fresh tool/context preparation. Durable Session state remains exact.
        }
        let mut session = Session::capture_readonly(&self.root, message.child_id.as_str())?
            .session()
            .clone();
        session.validate_active_agent_profile()?;
        let child = session
            .isolated_child
            .as_ref()
            .context("Target is not a child")?
            .clone();
        ensure!(
            child.identity.original_parent == ctx.session_id,
            "Child belongs to a different original parent"
        );
        let permission = message.permission.unwrap_or(child.permission);
        let preset = SystemPromptComposer::from_repository_service(self.repositories.clone())
            .activate_task_preset(
                Some(&child.identity.working_dir),
                message.preset.as_deref(),
                Some(&child.preset),
            )?;
        let provider = child
            .identity
            .resolution
            .restore_provider()
            .map_err(|error| {
                anyhow::anyhow!("Stored child execution route is unavailable: {error:?}")
            })?;
        let registry = Registry::new(provider.clone()).await;
        registry
            .register_isolated_mcp_tools(
                self.pool.clone(),
                session.id.clone(),
                child.identity.working_dir.clone(),
                crate::mcp::McpAccessPolicy {
                    permission,
                    blocked: child.identity.blocked_mcps.clone(),
                },
            )
            .await?;
        // Observation reuses Startup Context's existing bounded staleness policy.
        session.observe_startup_context_before_user_turn(
            &crate::startup_context::StartupContext::new(),
        )?;
        let mut agent =
            Agent::from_isolated_session(provider, registry, session, self.repositories.clone())?;
        agent
            .prepare_isolated_turn(
                &crate::execution::invocation_id(ctx),
                &message.prompt,
                message.permission,
                preset,
            )
            .await?;
        mark_ready(
            ctx,
            message.child_id.clone(),
            child.identity.artifact_dir,
            false,
        )?;
        Ok(agent)
    }

    async fn run(&self, ctx: &ToolContext, agent: &mut Agent) -> Result<ToolOutput> {
        let capture = ctx
            .invocation
            .capture
            .as_ref()
            .context("Child output has no capture owner")?;
        let ready = ctx
            .invocation
            .ready
            .as_ref()
            .context("Child has no acceptance owner")?;
        let receipt = ready
            .child_receipt()
            .context("Child identity was not published")?;
        capture.write(
            OutputStream::Text,
            format!(
                "Child: {}\nTurn: {}\nArtifacts: {}\n\n",
                receipt.child_id.as_str(),
                receipt.turn_id,
                receipt.artifact_dir.display()
            )
            .as_bytes(),
        )?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let stop = ctx
            .graceful_shutdown_signal
            .clone()
            .context("Child run has no Stop owner")?;
        let mut pending = Vec::with_capacity(64 * 1024);
        let mut events_open = true;
        let mut flush = tokio::time::interval(std::time::Duration::from_millis(250));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result = {
            let run = agent.run_prepared_isolated_turn(&receipt.turn_id, stop, tx);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    result = &mut run => break result,
                    _ = flush.tick(), if !pending.is_empty() => { capture.write(OutputStream::Text, &pending)?; pending.clear(); },
                    event = rx.recv(), if events_open => {
                        if event.is_none() { events_open = false; }
                        if let Some(crate::protocol::ServerEvent::TextDelta { text }) = event {
                            pending.extend_from_slice(text.as_bytes());
                            if pending.len() >= 64 * 1024 { capture.write(OutputStream::Text, &pending)?; pending.clear(); }
                        }
                    }
                }
            }
        };
        while let Ok(event) = rx.try_recv() {
            if let crate::protocol::ServerEvent::TextDelta { text } = event {
                pending.extend_from_slice(text.as_bytes());
            }
            if pending.len() >= 64 * 1024 {
                capture.write(OutputStream::Text, &pending)?;
                pending.clear();
            }
        }
        capture.write(OutputStream::Text, &pending)?;
        result?;
        let mut output = ToolOutput::new("").with_metadata(json!({"child_id":receipt.child_id,"run_id":receipt.turn_id,"artifact_dir":receipt.artifact_dir}));
        output.source = OutputSource::Retained(capture.reference()?);
        Ok(output)
    }
}

fn check_stop(ctx: &ToolContext) -> Result<()> {
    ensure!(
        !ctx.graceful_shutdown_signal
            .as_ref()
            .is_some_and(InterruptSignal::is_set),
        "Child operation stopped before the next execution boundary"
    );
    Ok(())
}
fn mark_ready(
    ctx: &ToolContext,
    child_id: ChildSessionId,
    artifact_dir: PathBuf,
    queued: bool,
) -> Result<()> {
    let receipt = ChildExecutionReceipt {
        child_id,
        turn_id: crate::execution::invocation_id(ctx),
        artifact_dir,
        queued,
    };
    let mut bytes = serde_json::to_vec(&receipt)?;
    bytes.push(b'\n');
    ctx.invocation
        .capture
        .as_ref()
        .context("Missing child capture")?
        .append_part("child-identity", &bytes)?;
    ctx.invocation
        .ready
        .as_ref()
        .context("Missing child readiness")?
        .mark_child(receipt);
    Ok(())
}
fn resolve_working_dir(requested: Option<PathBuf>, caller: Option<&Path>) -> Result<PathBuf> {
    let path = match requested {
        Some(path) if path.is_absolute() => path,
        Some(path) => caller.context("No caller working directory")?.join(path),
        None => caller
            .context("Child creation requires a bound working directory")?
            .to_path_buf(),
    };
    let path = path
        .canonicalize()
        .context("Resolve child working directory")?;
    ensure!(path.is_dir(), "Child working directory is not a directory");
    Ok(path)
}
fn prepare_startup(
    session: &mut Session,
    selection: &ChildStartupContext,
    cwd: &Path,
) -> Result<()> {
    use crate::startup_context::{
        StartupContext, StartupFailurePolicy, StartupPreparationOutcome, StartupSelectionInput,
    };
    if matches!(selection, ChildStartupContext::Disabled) {
        session.ensure_initial_session_context_message();
        return Ok(());
    }
    let engine = StartupContext::new();
    let project = engine.resolve_project(cwd)?;
    let prepared = match selection {
        ChildStartupContext::ProjectDefault => {
            let plan = engine.load_project_plan(&project)?;
            engine.prepare_project_plan(&project, plan.plan(), StartupFailurePolicy::Block)?
        }
        ChildStartupContext::Custom(files) => {
            let inputs = files
                .iter()
                .map(|file| {
                    let path = if Path::new(file).is_absolute() {
                        PathBuf::from(file)
                    } else {
                        cwd.join(file)
                    };
                    let resolved = path.canonicalize().with_context(|| {
                        format!("Resolve custom Startup Context file {}", path.display())
                    })?;
                    Ok(StartupSelectionInput::new(path).with_external_approval(resolved))
                })
                .collect::<Result<Vec<_>>>()?;
            let preview = engine.preview_selection(&project, inputs);
            engine.prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)?
        }
        ChildStartupContext::Disabled => unreachable!(),
    };
    ensure!(
        !matches!(prepared, StartupPreparationOutcome::Blocked(_)),
        "Child Startup Context is blocked: {:?}",
        prepared.preparation().issues().collect::<Vec<_>>()
    );
    session.stage_isolated_startup_context(prepared)?;
    Ok(())
}

struct UnpublishedChild {
    session_id: String,
    owns_session: bool,
    artifact: Option<PathBuf>,
    published: bool,
}
impl UnpublishedChild {
    fn cleanup(&mut self) -> Result<()> {
        if self.published {
            return Ok(());
        }
        self.published = true;
        let mut failures = Vec::new();
        crate::tool::clear_session_tool_policy(&self.session_id);
        if self.owns_session
            && let Err(error) = crate::session::remove_unpublished_session(&self.session_id)
        {
            failures.push(format!("Session: {error:#}"));
        }
        if let Some(path) = &self.artifact
            && let Err(error) = std::fs::remove_dir(path)
        {
            failures.push(format!("Artifact directory {}: {error}", path.display()));
        }
        ensure!(failures.is_empty(), "{}", failures.join("\n"));
        Ok(())
    }
}
impl Drop for UnpublishedChild {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            crate::logging::warn(&format!("Unpublished child cleanup failed: {error:#}"));
        }
    }
}
