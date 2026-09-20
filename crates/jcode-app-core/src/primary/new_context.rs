//! A new context has its own Agent identity. The source stays owned and usable.
use super::*;
use crate::agent::{StartupContextActivation, StartupContextCaller};
use crate::instruction::{AgentSelection, InstructionRepositoryService};
use crate::workspace::*;
use anyhow::Result;

impl PrimaryHost {
    pub(crate) async fn clear_context(
        self: &Arc<Self>,
        source: &Arc<Mutex<Agent>>,
        source_id: &str,
        repositories: &InstructionRepositoryService,
        pool: &Arc<crate::mcp::SharedMcpPool>,
    ) -> Result<Arc<Mutex<Agent>>> {
        let (send, receive) = tokio::sync::oneshot::channel();
        let host = Arc::downgrade(self);
        let source = source.clone();
        let source_id = source_id.to_owned();
        let repositories = repositories.clone();
        let pool = pool.clone();
        self.retain_delivery(async move {
            let result = match host.upgrade() {
                Some(host) => {
                    Box::pin(host.clear_context_owned(&source, &source_id, &repositories, &pool))
                        .await
                }
                None => Err(anyhow::anyhow!(
                    "Primary runtime ended before Clear preparation"
                )),
            };
            let _ = send.send(result);
        });
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
            self.processing(source_id).is_none() && self.accepts_input(),
            "Primary is busy or stopping"
        );
        source.primary_owner = Some(self.adopt_owner(&source)?);
        source
            .startup_context_session()
            .require_published_primary()?;
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
                .launch_hosted(
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
                    StartupContextCaller::Clear,
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
        if !self.accepts_input() {
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
            assert!(
                host.admit(&id, 71, source.clone()).is_err(),
                "a peer cannot enter after Clear's initial check"
            );
            release.send(())?;
            let fresh = clear.await??;
            assert!(!Arc::ptr_eq(&source, &fresh));
            assert_eq!(
                serde_json::to_vec(source.lock().await.startup_context_session())?,
                before
            );
            assert!(host.read().await.contains_key(&id));
            drop(host.admit(&id, 72, source.clone())?);
            host.shutdown().await
        })
    }
}
