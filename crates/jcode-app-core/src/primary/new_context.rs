//! A new context has its own Agent identity. The source stays owned and usable.
use super::*;
use crate::agent::{StartupContextActivation, StartupContextCaller};
use crate::instruction::{AgentSelection, InstructionRepositoryService};
use crate::workspace::*;
use anyhow::Result;

impl PrimaryHost {
    #[cfg(test)]
    pub(crate) async fn clear_context(
        self: &Arc<Self>,
        source: &Arc<Mutex<Agent>>,
        source_id: &str,
        repositories: &InstructionRepositoryService,
        pool: &Arc<crate::mcp::SharedMcpPool>,
    ) -> Result<Arc<Mutex<Agent>>> {
        self.clear_context_with_grants(source, source_id, repositories, pool, None)
            .await
    }

    pub(crate) async fn clear_context_with_grants(
        self: &Arc<Self>,
        source: &Arc<Mutex<Agent>>,
        source_id: &str,
        repositories: &InstructionRepositoryService,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        choice: Option<GrantCarryChoice>,
    ) -> Result<Arc<Mutex<Agent>>> {
        let permit =
            crate::runtime_lifecycle::admission::preparation("clear", Some(source_id.into()))?;
        let (send, receive) = tokio::sync::oneshot::channel();
        let host = Arc::downgrade(self);
        let source = source.clone();
        let source_id = source_id.to_owned();
        let repositories = repositories.clone();
        let pool = pool.clone();
        self.retain_delivery(crate::runtime_lifecycle::admission::scope(
            permit,
            async move {
                let result = match host.upgrade() {
                    Some(host) => {
                        Box::pin(host.clear_context_owned(
                            &source,
                            &source_id,
                            &repositories,
                            &pool,
                            choice,
                        ))
                        .await
                    }
                    None => Err(anyhow::anyhow!(
                        "Primary runtime ended before Clear preparation"
                    )),
                };
                let _ = send.send(result);
            },
        ));
        receive
            .await
            .context("Clear outcome requires runtime reconciliation")?
    }

    async fn clear_context_owned(
        self: &Arc<Self>,
        source: &Arc<Mutex<Agent>>,
        source_id: &str,
        repositories: &InstructionRepositoryService,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        choice: Option<GrantCarryChoice>,
    ) -> Result<Arc<Mutex<Agent>>> {
        // Retain this guard through preparation/publication. A busy check followed
        // by a later lock would let a peer admit work in the middle of Clear.
        let mut source = source
            .clone()
            .try_lock_owned()
            .context("Primary is busy; Clear was not applied")?;
        ensure!(
            source.session_id() == source_id,
            "Clear source identity changed"
        );
        ensure!(
            self.processing(source_id).is_none() && self.accepts_prepared_work(),
            "Primary is busy or stopping"
        );
        source.primary_owner = Some(self.adopt_owner(&source)?);
        source
            .startup_context_session()
            .require_published_primary()?;
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        let scope = workspace.prepare_context_scope(
            source.startup_context_session(),
            choice,
            &WorkspaceClientAuthority::authenticated("shared-primary-context")?,
        )?;
        let selection = source
            .active_agent()
            .map(AgentSelection::from_stored)
            .transpose()?
            .unwrap_or(AgentSelection::Default);
        let provider = source.provider_handle();
        if let Some(location) = &source.startup_context_session().location {
            let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
            let expected = workspace.status()?.revision;
            let agent = source.active_agent().map(|agent| {
                format!(
                    "{}:{}",
                    if agent.scope == crate::instruction::InstructionScope::Global {
                        "global"
                    } else {
                        "project"
                    },
                    agent.id
                )
            });
            let launcher = PrimaryLauncher {
                workspace,
                repositories: repositories.clone(),
                provider,
                registry: PrimaryRegistryMode::Shared(pool.clone()),
            };
            let record = launcher
                .launch_hosted_scoped(
                    self,
                    RequestId::new(),
                    expected,
                    PrimaryLaunchInput {
                        placement: PrimaryPlacement::Existing {
                            placement: location.placement,
                        },
                        cwd: Some(PrimaryCwd::Existing {
                            path: source
                                .working_dir()
                                .context("Managed primary has no cwd")?
                                .into(),
                        }),
                        agent,
                        model: None,
                        selfdev: source.is_canary(),
                    },
                    super::launch::PrimaryPreparation {
                        caller: StartupContextCaller::Clear,
                        scope: scope.as_ref(),
                    },
                )
                .await?;
            return self
                .read()
                .await
                .get(&record.session)
                .cloned()
                .context("Cleared primary was not published");
        }
        let fresh_provider = provider.fork();
        let registry = crate::tool::Registry::new_for_shared_session(
            fresh_provider.clone(),
            pool.clone(),
            repositories.clone(),
        )
        .await?;
        let mut session = crate::session::Session::create(None, None);
        session.working_dir = source.working_dir().map(str::to_owned);
        session.model = Some(source.provider_model());
        session.provider_key = source.startup_context_session().provider_key.clone();
        session.route_api_method = source.startup_context_session().route_api_method.clone();
        session.reasoning_effort = fresh_provider.reasoning_effort();
        let activation = if source.is_debug() {
            StartupContextActivation::Disabled
        } else {
            StartupContextActivation::primary(StartupContextCaller::Clear)
        };
        let (mut fresh, _) = Agent::prepare_primary_session(
            fresh_provider,
            registry,
            session,
            activation,
            selection,
            source.is_canary(),
            repositories.clone(),
        )?;
        fresh.set_debug(source.is_debug());
        let identity = fresh.session_id().to_string();
        fresh
            .registry()
            .register_mcp_tools_for_dir(
                None,
                Some(pool.clone()),
                Some(identity.clone()),
                fresh.working_dir().map(std::path::PathBuf::from),
            )
            .await;
        if !self.accepts_prepared_work() {
            crate::tool::clear_session_tool_policy(&identity);
            crate::session::remove_unpublished_session(&identity)?;
            anyhow::bail!("Primary runtime stopped during Clear preparation; source is unchanged");
        }
        fresh.primary_owner = Some(self.adopt_owner(&fresh)?);
        self.resources
            .lock()
            .expect("primary resources")
            .insert(identity.clone(), PrimaryResources::from_agent(&fresh));
        let fresh = Arc::new(Mutex::new(fresh));
        self.write().await.insert(identity, fresh.clone());
        Ok(fresh)
    }
}

/// Shared primary transfer preparation for hosted and process-owned clients.
/// The caller owns summary generation, this owner owns complete new context publication.
pub fn prepare_transfer_session(
    parent: &crate::session::Session,
    instruction_repositories: &InstructionRepositoryService,
    summary: Option<String>,
    choice: Option<GrantCarryChoice>,
) -> Result<(String, String)> {
    let child = prepare_fresh_context(
        parent,
        instruction_repositories,
        summary,
        choice,
        NewContextKind::Transfer,
    )?;
    Ok((child.id.clone(), child.display_name().to_string()))
}

pub fn prepare_local_clear_session(
    parent: &crate::session::Session,
    repositories: &InstructionRepositoryService,
    choice: Option<GrantCarryChoice>,
) -> Result<crate::session::Session> {
    prepare_fresh_context(parent, repositories, None, choice, NewContextKind::Clear)
}

fn prepare_fresh_context(
    parent: &crate::session::Session,
    instruction_repositories: &InstructionRepositoryService,
    summary: Option<String>,
    choice: Option<GrantCarryChoice>,
    kind: NewContextKind,
) -> Result<crate::session::Session> {
    use crate::session::Session;
    let _permit =
        crate::runtime_lifecycle::admission::preparation("new-context", Some(parent.id.clone()))?;
    let parent_session_id = parent.id.as_str();
    let workspace = crate::workspace::WorkspaceService::new(&crate::storage::durable_state_dir());
    let scope = workspace.prepare_context_scope(
        parent,
        choice,
        &crate::workspace::WorkspaceClientAuthority::authenticated("primary-context-control")?,
    )?;
    let todos = if kind == NewContextKind::Transfer {
        crate::todo::load_todos(parent_session_id).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut child = Session::create(
        (kind == NewContextKind::Transfer).then(|| parent_session_id.to_string()),
        None,
    );
    child.messages.clear();
    child.compaction = None;
    child.context_view = Default::default();
    child.working_dir = parent.working_dir.clone();
    if let Some(scope) = &scope {
        workspace.stage_context_scope(scope, &mut child, kind)?;
    }
    child.model = parent.model.clone();
    child.provider_key = parent.provider_key.clone();
    child.route_api_method = parent.route_api_method.clone();
    child.reasoning_effort = parent.reasoning_effort.clone();
    child.reasoning_effort_intent = parent.reasoning_effort_intent.clone();
    child.subagent_model = parent.subagent_model.clone();
    child.improve_mode = parent.improve_mode;
    child.autoreview_enabled = parent.autoreview_enabled;
    child.autojudge_enabled = parent.autojudge_enabled;
    child.is_canary = parent.is_canary;
    child.is_debug = parent.is_debug;
    child.testing_build = parent.testing_build.clone();
    child.provider_session_id = None;
    child.clear_active_skill();
    child.status = if kind == NewContextKind::Clear {
        crate::session::SessionStatus::Active
    } else {
        crate::session::SessionStatus::Closed
    };

    let child_id = child.id.clone();
    let instruction_selection = parent
        .active_agent()
        .map(crate::instruction::AgentSelection::from_stored)
        .transpose()
        .map_err(anyhow::Error::new)
        .and_then(|selection| match selection {
            Some(selection) => Ok(selection),
            None => Ok(crate::instruction::AgentSelection::Explicit(
                crate::instruction::InstructionSelector::global(
                    crate::instruction::InstructionKind::Agent,
                    "jcode",
                )?,
            )),
        });
    let instruction_activation = instruction_selection.and_then(|selection| {
        let global_skills = crate::skill::SkillRegistry::shared_snapshot();
        let effective_skills = crate::skill::SkillRegistry::effective_for_working_dir(
            &global_skills,
            child.working_dir.as_deref().map(std::path::Path::new),
        );
        let available_skills = effective_skills
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect::<Vec<_>>();
        crate::instruction::SystemPromptComposer::from_repository_service(
            instruction_repositories.clone(),
        )
        .activate(crate::instruction::SystemPromptActivationRequest {
            working_dir: child.working_dir.as_deref().map(std::path::Path::new),
            selection,
            is_selfdev: child.is_canary,
            capabilities: crate::prompt::PromptCapabilities::current(),
            available_skills: &available_skills,
        })
        .map_err(anyhow::Error::new)
    });
    match instruction_activation {
        Ok(activation) => child.install_system_prompt(activation.state),
        Err(error) => {
            if let Err(cleanup) = crate::session::remove_unpublished_session(&child_id) {
                anyhow::bail!(
                    "new-context instruction activation failed ({error}); unpublished child cleanup also failed: {cleanup}"
                );
            }
            return Err(error.context("new-context instruction activation failed"));
        }
    }
    let transfer_activation = if parent.is_debug {
        crate::agent::StartupContextActivation::Disabled
    } else {
        crate::agent::StartupContextActivation::primary(if kind == NewContextKind::Clear {
            crate::agent::StartupContextCaller::Clear
        } else {
            crate::agent::StartupContextCaller::Transfer
        })
    };
    if let Err(error) =
        crate::agent::activate_session_startup_context(&mut child, transfer_activation)
    {
        if let Err(cleanup) = crate::session::remove_unpublished_session(&child_id) {
            anyhow::bail!(
                "new-context Startup Context failed ({error}); unpublished child cleanup also failed: {cleanup}"
            );
        }
        return Err(error.into());
    }

    let handoff_result = match summary {
        Some(summary) => child
            .append_transfer_handoff(parent_session_id, &summary)
            .and_then(|appended| {
                if appended {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "transfer summary was empty; refusing to create a contextless child"
                    ))
                }
            }),
        None if kind == NewContextKind::Transfer && !parent.messages.is_empty() => Err(
            anyhow::anyhow!("transfer produced no readable summary for a non-empty parent session"),
        ),
        None => Ok(()),
    };
    if let Err(error) = handoff_result
        .and_then(|()| crate::todo::save_todos(&child.id, &todos))
        .and_then(|()| {
            child.seal_context_scope();
            child.save()
        })
    {
        if let Err(cleanup) = crate::session::remove_unpublished_session(&child_id) {
            anyhow::bail!(
                "new-context creation failed ({error}); unpublished child cleanup also failed: {cleanup}"
            );
        }
        return Err(error);
    }
    if child.scope_copy.is_some() {
        workspace.reconcile_context_scope(&child.id)?;
    }
    Ok(child)
}

pub fn prepare_split_session(
    parent: &crate::session::Session,
    choice: Option<GrantCarryChoice>,
) -> Result<crate::session::Session> {
    use crate::session::Session;
    let _permit =
        crate::runtime_lifecycle::admission::preparation("split", Some(parent.id.clone()))?;
    let workspace = crate::workspace::WorkspaceService::new(&crate::storage::durable_state_dir());
    let scope = workspace.prepare_context_scope(
        parent,
        choice,
        &crate::workspace::WorkspaceClientAuthority::authenticated("shared-primary-context")?,
    )?;
    let parent_session_id = &parent.id;
    let mut child = Session::create(Some(parent_session_id.to_string()), None);
    child.inherit_continuation_state_from(parent);
    if let Some(scope) = &scope {
        workspace.stage_context_scope(
            scope,
            &mut child,
            crate::workspace::NewContextKind::Split,
        )?;
    }
    child.status = crate::session::SessionStatus::Closed;
    // The parent agent keeps ownership of any in-flight request; tell the
    // forked agent so it treats the next prompt as fresh work instead of
    // continuing (and duplicating) the parent's current turn.
    if let Err(error) = child
        .append_fork_notice(parent_session_id, parent.display_name())
        .map(|_| ())
        .and_then(|()| {
            child.seal_context_scope();
            child.save()
        })
    {
        if let Err(cleanup) = crate::session::remove_unpublished_session(&child.id) {
            anyhow::bail!(
                "split child preparation failed ({error}); cleanup also failed: {cleanup}"
            );
        }
        return Err(error);
    }

    if child.scope_copy.is_some() {
        workspace.reconcile_context_scope(&child.id)?;
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    type ForkWait = (
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    #[derive(Clone, Default)]
    struct ForkGate(Arc<StdMutex<Option<ForkWait>>>);
    #[async_trait::async_trait]
    impl crate::provider::Provider for ForkGate {
        async fn complete(
            &self,
            _: &[crate::message::Message],
            _: &[crate::message::ToolDefinition],
            _: &str,
            _: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            anyhow::bail!("Clear must not dispatch")
        }
        fn name(&self) -> &str {
            "clear-fixture"
        }
        fn fork(&self) -> Arc<dyn crate::provider::Provider> {
            let gate = self.0.lock().unwrap().take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                release
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .unwrap();
            }
            Arc::new(self.clone())
        }
    }
    #[test]
    fn clear_failure_and_concurrent_peer_admission_preserve_the_source() -> Result<()> {
        clear_with_shutdown(crate::workspace::runtime::StopStrategy::FinishCurrent)
    }

    #[test]
    fn interrupted_clear_cleans_the_unpublished_context_and_retains_its_source() -> Result<()> {
        clear_with_shutdown(crate::workspace::runtime::StopStrategy::Interrupt)
    }

    fn clear_with_shutdown(strategy: crate::workspace::runtime::StopStrategy) -> Result<()> {
        use crate::runtime_lifecycle::{RuntimeStopStore, admission::RuntimeAdmission};
        use crate::workspace::runtime::*;
        let _environment = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let provider = ForkGate::default();
            let concrete: Arc<dyn crate::provider::Provider> = Arc::new(provider.clone());
            let repositories = InstructionRepositoryService::new();
            let registry = crate::tool::Registry::new(concrete.clone()).await;
            let (agent, _) = Agent::new_with_startup_context_and_agent_with_repositories(
                concrete,
                registry,
                None,
                StartupContextActivation::primary(StartupContextCaller::HarnessApi),
                AgentSelection::Default,
                false,
                repositories.clone(),
            )?;
            let id = agent.session_id().to_string();
            let source = Arc::new(Mutex::new(agent));
            let host = Arc::new(PrimaryHost::new(HashMap::from([(
                id.clone(),
                source.clone(),
            )])));
            let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
            let root = crate::storage::jcode_dir()?;
            let owner = RuntimeStopStore::new(
                &crate::storage::durable_state_dir(),
                &root.join("clear.sock"),
            )?
            .claim()?;
            let registration = RuntimeAdmission::register(&root, owner.identity())?;
            let before = serde_json::to_vec(source.lock().await.startup_context_session())?;
            let profile = crate::storage::jcode_dir()?.join("instructions/agents/jcode.md");
            let original = std::fs::read(&profile)?;
            std::fs::write(&profile, "---\ninvalid: [\n")?;
            assert!(
                host.clear_context(&source, &id, &repositories, &pool)
                    .await
                    .is_err()
            );
            assert_eq!(host.read().await.len(), 1);
            assert_eq!(
                serde_json::to_vec(source.lock().await.startup_context_session())?,
                before
            );
            std::fs::write(profile, original)?;
            let (signal, entered) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            *provider.0.lock().unwrap() = Some((signal, wait));
            let worker_host = host.clone();
            let worker_source = source.clone();
            let worker_id = id.clone();
            let clear = tokio::spawn(async move {
                worker_host
                    .clear_context(&worker_source, &worker_id, &repositories, &pool)
                    .await
            });
            entered.await?;
            let review = registration.admission().review(
                &owner,
                ShutdownOptions {
                    strategy,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 5,
                    destination: Default::default(),
                },
                Vec::new(),
            )?;
            let operation =
                registration
                    .admission()
                    .begin(&owner, RequestId::new(), review.id, Vec::new())?;
            assert!(
                host.admit(&id, 71, source.clone()).is_err(),
                "a peer cannot enter after Clear's initial check"
            );
            release.send(())?;
            let outcome = clear.await?;
            if strategy == StopStrategy::FinishCurrent {
                let fresh = outcome?;
                assert!(!Arc::ptr_eq(&source, &fresh));
                registration
                    .admission()
                    .cancel_wait(&owner, operation.id, operation.revision)?;
            } else {
                assert!(outcome.is_err());
                assert_eq!(host.read().await.len(), 1);
            }
            assert_eq!(
                serde_json::to_vec(source.lock().await.startup_context_session())?,
                before
            );
            assert!(host.read().await.contains_key(&id));
            if strategy == StopStrategy::FinishCurrent {
                drop(host.admit(&id, 72, source.clone())?);
            }
            assert!(registration.admission().work()?.is_empty());
            host.shutdown().await
        })
    }
}
