use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::EventStream;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Default)]
struct FixtureProvider(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Provider for FixtureProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("Launch must not dispatch inference")
    }
    fn name(&self) -> &str {
        "fixture"
    }
    fn model(&self) -> String {
        "fixture-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        vec![ModelRoute {
            model: self.model(),
            provider: self.name().into(),
            api_method: "fixture".into(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }
}
struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Env {
    fn new(root: &std::path::Path) -> Self {
        let values = ["JCODE_HOME", "JCODE_RUNTIME_DIR"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        crate::env::set_var("JCODE_HOME", root.join("state"));
        crate::env::set_var("JCODE_RUNTIME_DIR", root.join("runtime"));
        Self(values)
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::Config::invalidate_cache();
    }
}

#[test]
#[cfg(target_os = "macos")]
fn primary_location_idle_notice_prefix_and_missing_cwd_repair() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    std::fs::write(
        crate::config::Config::path().unwrap(),
        "[features]\nmanaged_primary_launch = true\n",
    )?;
    crate::config::Config::invalidate_cache();
    tokio::runtime::Runtime::new()?.block_on(async {
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new())?;
        let mut roots = Vec::new();
        for name in ["initial", "destination", "repair"] {
            let path = temp.path().join(name);
            std::fs::create_dir(&path)?;
            std::fs::write(path.join("sentinel.txt"), name)?;
            let path = path.canonicalize()?;
            let EntityId::Location(id) = change(&service,OrganizationChange::RegisterLocation { name: name.into(), path:path.clone(), registration:Registration::Standalone })?.targets[0] else { panic!("location"); };
            roots.push((id,path));
        }
        let provider = FixtureProvider::default();
        let launcher = PrimaryLauncher { workspace:service.clone(), repositories:InstructionRepositoryService::new(), provider:Arc::new(provider.clone()), registry:PrimaryRegistryMode::Process };
        let host = Arc::new(PrimaryHost::default());
        let launch = launcher.launch_hosted(&host,RequestId::new(),service.status()?.revision,PrimaryLaunchInput { placement:PrimaryPlacement::Existing { placement:Placement::Standalone(roots[0].0) }, cwd:Some(PrimaryCwd::Existing { path:roots[0].1.clone() }), agent:None,model:None,selfdev:false },StartupContextCaller::HarnessApi).await?;
        let before = Session::load(&launch.session)?;
        let request = LocationChangeRequest { request:RequestId::new(), session:launch.session.clone(), expected_session_revision:1, expected_catalog_revision:service.status()?.revision, placement:Placement::Standalone(roots[1].0), cwd:roots[1].1.clone() };
        use crate::runtime_lifecycle::{RuntimeStopStore, admission::RuntimeAdmission};
        use crate::workspace::runtime::*;
        let owner = RuntimeStopStore::new(&crate::storage::durable_state_dir(), &temp.path().join("location-runtime.sock"))?.claim()?;
        let registration = RuntimeAdmission::register(&crate::storage::jcode_dir()?, owner.identity())?;
        let pending = service.request_location_change(request.clone())?;
        let review = registration.admission().review(&owner, ShutdownOptions { strategy: StopStrategy::FinishCurrent, independent: IndependentTasks::Stop, quiescence_timeout_seconds: 5 }, Vec::new())?;
        let stop = registration.admission().begin(&owner, RequestId::new(), review.id, Vec::new())?;
        assert!(matches!(host.request_location(PrimaryLocationCommand::Change { request: request.clone() }).await, PrimaryLocationResponse::Rejected { .. }));
        assert!(matches!(host.request_location(PrimaryLocationCommand::Inspect { operation: pending.operation }).await, PrimaryLocationResponse::State { record } if record.state == LocationChangeState::Pending));
        assert!(host.read().await[&launch.session].lock().await.apply_primary_location_changes().await.is_err());
        assert_eq!(serde_json::to_vec(&Session::load(&launch.session)?.messages)?, serde_json::to_vec(&before.messages)?);
        registration.admission().cancel_wait(&owner, stop.id, stop.revision)?;
        let result = host.request_location(PrimaryLocationCommand::Change { request:request.clone() }).await;
        let PrimaryLocationResponse::State { record } = result else { anyhow::bail!("move failed: {result:?}"); };
        assert_eq!(record.state,LocationChangeState::Complete);
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        assert!(host.processing(&launch.session).is_none());
        let after = Session::load(&launch.session)?;
        assert_eq!(after.working_dir.as_deref(),roots[1].1.to_str());
        assert_eq!(after.location.as_ref().unwrap().initial_cwd,before.location.as_ref().unwrap().initial_cwd);
        assert_eq!(after.system_prompt,before.system_prompt);
        assert_eq!(after.active_skill,before.active_skill);
        assert_eq!(serde_json::to_value(&after.messages[..before.messages.len()])?,serde_json::to_value(&before.messages)?);
        assert_eq!(after.messages.len(),before.messages.len()+1);
        let again = host.request_location(PrimaryLocationCommand::Change { request }).await;
        assert!(matches!(again,PrimaryLocationResponse::State{record:ref replay} if **replay == *record));
        assert!(matches!(host.request_location(PrimaryLocationCommand::Cancel { operation:record.operation }).await,PrimaryLocationResponse::Rejected{..}));
        assert_eq!(Session::load(&launch.session)?.messages.len(),after.messages.len());
        std::fs::write(crate::config::Config::path().unwrap(), "[features]\nmanaged_primary_launch = false\n")?;
        crate::config::Config::invalidate_cache();
        assert!(matches!(host.request_location(PrimaryLocationCommand::Inspect { operation:record.operation }).await,PrimaryLocationResponse::State{..}));
        assert!(matches!(host.request_location(PrimaryLocationCommand::Cancel { operation:record.operation }).await,PrimaryLocationResponse::Rejected{issue} if issue.code == IssueCode::Conflict));
        std::fs::write(crate::config::Config::path().unwrap(), "[features]\nmanaged_primary_launch = true\n")?;
        crate::config::Config::invalidate_cache();
        // Actual invocation reads from the new cwd. There is no model call.
        let agent = host.read().await.get(&launch.session).cloned().unwrap();
        let output = agent.lock().await.execute_tool("read",serde_json::json!({"file_path":"sentinel.txt","intent":"Check bound cwd"})).await?;
        assert!(output.output.contains("destination"));
        drop(agent);
        host.shutdown().await?;
        drop(host);
        std::fs::rename(&roots[1].1,temp.path().join("unavailable-old-cwd"))?;
        let host = Arc::new(PrimaryHost::default());
        let pool = Arc::new(crate::mcp::SharedMcpPool::new(Default::default()));
        let source: Arc<dyn Provider> = Arc::new(provider.clone());
        assert!(host.restore(&launch.session,&source,&pool,&InstructionRepositoryService::new()).await.is_err());
        let repair = LocationChangeRequest { request:RequestId::new(),session:launch.session.clone(),expected_session_revision:2,expected_catalog_revision:service.status()?.revision,placement:Placement::Standalone(roots[2].0),cwd:roots[2].1.clone() };
        let repaired = host.request_location_restoring(PrimaryLocationCommand::Change{request:repair},&source,&pool,&InstructionRepositoryService::new()).await;
        assert!(matches!(repaired,PrimaryLocationResponse::State{ref record} if record.state==LocationChangeState::Complete),"{repaired:?}");
        let repaired = Session::load(&launch.session)?;
        assert_eq!(repaired.working_dir.as_deref(),roots[2].1.to_str());
        assert_eq!(repaired.system_prompt,before.system_prompt);
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        host.shutdown().await
    })
}
fn change(service: &WorkspaceService, change: OrganizationChange) -> Result<Receipt> {
    let review = service.review_organization_change(service.status()?.revision, change)?;
    Ok(service.apply_organization_change(RequestId::new(), review.id)?)
}

#[test]
#[cfg(target_os = "macos")]
fn launch_service_prepares_all_placements_replays_once_and_retains_exclusive_ownership()
-> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new())?;
        let EntityId::Project(project) = change(
            &service,
            OrganizationChange::CreateProject {
                name: "fixture".into(),
            },
        )?
        .targets[0] else {
            panic!("project")
        };
        let EntityId::WorkArea(area) = change(
            &service,
            OrganizationChange::CreateWorkArea {
                project,
                name: "area".into(),
            },
        )?
        .targets[0] else {
            panic!("area")
        };
        let EntityId::Repository(repository) = change(
            &service,
            OrganizationChange::CreateRepository {
                name: "repo".into(),
                remotes: vec![],
            },
        )?
        .targets[0] else {
            panic!("repository")
        };
        change(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        )?;
        let mut roots = Vec::new();
        for (name, registration, git) in [
            (
                "checkout",
                Registration::Checkout {
                    home: Home::WorkArea(area),
                    repository,
                },
                true,
            ),
            (
                "directory",
                Registration::Directory {
                    home: Home::Project(project),
                },
                false,
            ),
            ("standalone", Registration::Standalone, false),
        ] {
            let path = temp.path().join(name);
            std::fs::create_dir(&path)?;
            if git {
                ensure!(
                    std::process::Command::new("git")
                        .args(["init", "-q"])
                        .arg(&path)
                        .status()?
                        .success(),
                    "fixture git init"
                );
            }
            let EntityId::Location(id) = change(
                &service,
                OrganizationChange::RegisterLocation {
                    name: name.into(),
                    path: path.clone(),
                    registration,
                },
            )?
            .targets[0] else {
                panic!("location")
            };
            roots.push((id, path));
        }
        let provider = FixtureProvider::default();
        let launcher = PrimaryLauncher {
            workspace: service.clone(),
            repositories: InstructionRepositoryService::new(),
            provider: Arc::new(provider.clone()),
            registry: PrimaryRegistryMode::Process,
        };
        let host = Arc::new(PrimaryHost::default());
        for (placement, path) in [
            (Placement::Project(project), &roots[0].1),
            (Placement::WorkArea(area), &roots[0].1),
            (Placement::Checkout(roots[0].0), &roots[0].1),
            (Placement::Directory(roots[1].0), &roots[1].1),
            (Placement::Standalone(roots[2].0), &roots[2].1),
        ] {
            let request = RequestId::new();
            let expected = service.status()?.revision;
            let input = PrimaryLaunchInput {
                placement: PrimaryPlacement::Existing { placement },
                cwd: Some(PrimaryCwd::Existing { path: path.clone() }),
                agent: None,
                model: None,
                selfdev: false,
            };
            let record = launcher
                .launch_hosted(
                    &host,
                    request,
                    expected,
                    input.clone(),
                    StartupContextCaller::HarnessApi,
                )
                .await?;
            assert_eq!(record.state, PrimaryLaunchState::Complete);
            assert_eq!(
                launcher
                    .launch_hosted(
                        &host,
                        request,
                        expected,
                        input,
                        StartupContextCaller::HarnessApi
                    )
                    .await?,
                record
            );
            let saved = Session::load(&record.session)?;
            assert_eq!(saved.location.as_ref().unwrap().placement, placement);
            assert_eq!(
                std::path::Path::new(saved.working_dir.as_deref().unwrap()),
                path.canonicalize()?
            );
            assert!(saved.primary_creation.as_ref().unwrap().ready);
            assert!(PrimaryLease::acquire(&record.session).is_err());
            let source = host.read().await[&record.session].clone();
            let source_bytes = serde_json::to_vec(source.lock().await.startup_context_session())?;
            let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
            let cleared = host
                .clear_context(&source, &record.session, &launcher.repositories, &pool)
                .await?;
            let guard = cleared.lock().await;
            let replacement = Session::load(guard.session_id())?;
            assert_eq!(replacement.location.as_ref().unwrap().placement, placement);
            assert_eq!(
                replacement.location.as_ref().unwrap().cwd,
                saved.location.as_ref().unwrap().cwd
            );
            assert_eq!(replacement.active_agent(), saved.active_agent());
            assert_eq!(replacement.model, saved.model);
            assert_eq!(replacement.reasoning_effort, saved.reasoning_effort);
            assert_ne!(
                replacement.primary_creation.as_ref().unwrap().request,
                record.request
            );
            replacement.require_published_primary()?;
            assert!(!Arc::ptr_eq(
                &source.lock().await.provider_handle(),
                &guard.provider_handle()
            ));
            drop(guard);
            assert_eq!(
                serde_json::to_vec(source.lock().await.startup_context_session())?,
                source_bytes
            );
            assert!(host.read().await.contains_key(&record.session));
        }
        assert_eq!(host.read().await.len(), 10);
        let path = temp.path().join("new-local");
        let request = RequestId::new();
        let revision = service.status()?.revision;
        let input = PrimaryLaunchInput {
            placement: PrimaryPlacement::Standalone { root: path.clone() },
            cwd: Some(PrimaryCwd::CreateEmpty {
                path: path.clone(),
                home: None,
            }),
            agent: None,
            model: None,
            selfdev: false,
        };
        let (mut local, record) = launcher
            .launch_local(
                request,
                revision,
                input.clone(),
                StartupContextCaller::RunCommand,
            )
            .await?;
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&path)?.count(), 0);
        let before = serde_json::to_vec(local.messages())?;
        assert!(
            launcher
                .launch_hosted(
                    &host,
                    request,
                    revision,
                    input.clone(),
                    StartupContextCaller::HarnessApi
                )
                .await
                .is_err()
        );
        let placement = local
            .startup_context_session()
            .location
            .as_ref()
            .unwrap()
            .placement;
        local.clear()?;
        assert_ne!(local.session_id(), record.session);
        assert_eq!(
            local
                .startup_context_session()
                .location
                .as_ref()
                .unwrap()
                .placement,
            placement
        );
        assert!(local.startup_context_session().primary_creation.is_none());
        assert!(PrimaryLease::acquire(local.session_id()).is_err());
        drop(local);
        let restored = launcher
            .launch_hosted(
                &host,
                request,
                revision,
                input,
                StartupContextCaller::HarnessApi,
            )
            .await?;
        assert_eq!(restored.session, record.session);
        assert_eq!(
            serde_json::to_vec(&Session::load(&record.session)?.messages)?,
            before
        );
        assert_eq!(provider.0.load(Ordering::SeqCst), 0);
        host.shutdown().await?;
        Ok(())
    })
}

#[test]
#[cfg(target_os = "macos")]
fn launch_service_rejects_invalid_preparation_without_dispatch_or_session_and_retries_same_identity()
-> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let service = WorkspaceService::new(&crate::storage::durable_state_dir());
        service.initialize(RequestId::new())?;
        let path = temp.path().join("work");
        std::fs::create_dir(&path)?;
        let control = FixtureProvider::default();
        let launcher = PrimaryLauncher {
            workspace: service.clone(),
            repositories: InstructionRepositoryService::new(),
            provider: Arc::new(control.clone()),
            registry: PrimaryRegistryMode::Process,
        };
        let valid = PrimaryLaunchInput {
            placement: PrimaryPlacement::Standalone { root: path.clone() },
            cwd: Some(PrimaryCwd::Existing { path: path.clone() }),
            agent: None,
            model: None,
            selfdev: false,
        };
        let mut missing = valid.clone();
        missing.cwd = None;
        let request = RequestId::new();
        assert!(
            launcher
                .launch_local(
                    request,
                    service.status()?.revision,
                    missing,
                    StartupContextCaller::RunCommand
                )
                .await
                .is_err()
        );
        assert!(service.inspect_primary_launch(request).is_err());
        let mut bad_model = valid.clone();
        bad_model.model = Some(PrimaryModel {
            model: "unavailable".into(),
            provider: "fixture".into(),
            api_method: "fixture".into(),
            effort: None,
        });
        let request = RequestId::new();
        assert!(
            launcher
                .launch_local(
                    request,
                    service.status()?.revision,
                    bad_model,
                    StartupContextCaller::RunCommand
                )
                .await
                .is_err()
        );
        assert!(service.inspect_primary_launch(request).is_err());
        let mut bad_profile = valid.clone();
        bad_profile.agent = Some("missing-fixture-profile".into());
        let request = RequestId::new();
        assert!(
            launcher
                .launch_local(
                    request,
                    service.status()?.revision,
                    bad_profile,
                    StartupContextCaller::HarnessApi
                )
                .await
                .is_err()
        );
        let rejected = service.inspect_primary_launch(request)?;
        assert_eq!(rejected.state, PrimaryLaunchState::Failed);
        assert!(!crate::session::session_exists(&rejected.session));
        let engine = crate::startup_context::StartupContext::from_durable_state_dir(
            crate::storage::durable_state_dir(),
        );
        let file = path.join("required.txt");
        std::fs::write(&file, "synthetic required context")?;
        let active = engine.resolve_project(&path)?;
        let preview = engine.preview_selection(
            &active,
            [crate::startup_context::StartupSelectionInput::new(
                "required.txt",
            )],
        );
        engine.save_project_plan(&active, 0, &preview)?;
        std::fs::remove_file(&file)?;
        let request = RequestId::new();
        let expected = service.status()?.revision;
        assert!(
            launcher
                .launch_local(
                    request,
                    expected,
                    valid.clone(),
                    StartupContextCaller::HarnessApi
                )
                .await
                .is_err()
        );
        let rejected = service.inspect_primary_launch(request)?;
        assert_eq!(rejected.state, PrimaryLaunchState::Failed);
        assert!(!crate::session::session_exists(&rejected.session));
        std::fs::write(&file, "synthetic repaired context")?;
        let (agent, receipt) = launcher
            .launch_local(request, expected, valid, StartupContextCaller::HarnessApi)
            .await?;
        assert_eq!(receipt.session, rejected.session);
        assert_eq!(receipt.state, PrimaryLaunchState::Complete);
        assert_eq!(
            agent
                .startup_context_session()
                .startup_context
                .as_ref()
                .unwrap()
                .batches[0]
                .files
                .len(),
            1
        );
        assert_eq!(control.0.load(Ordering::SeqCst), 0);
        drop(agent);
        Ok(())
    })
}

#[test]
#[cfg(target_os = "macos")]
fn legacy_adoption_uses_the_real_idle_primary_control_without_inference() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    std::fs::write(
        crate::config::Config::path().unwrap(),
        "[features]\nmanaged_primary_launch = false\n",
    )?;
    crate::config::Config::invalidate_cache();
    tokio::runtime::Runtime::new()?.block_on(async {
        let old = temp.path().join("old"); let next = temp.path().join("next");
        std::fs::create_dir(&old)?;std::fs::create_dir(&next)?;
        let old = old.canonicalize()?; let next = next.canonicalize()?;
        let service = WorkspaceService::new(&crate::storage::durable_state_dir()); service.initialize(RequestId::new())?;
        let EntityId::Location(target) = change(&service,OrganizationChange::RegisterLocation {name:"next".into(),path:next.clone(),registration:Registration::Standalone})?.targets[0] else { panic!() };
        let provider = FixtureProvider::default();
        let registry = crate::tool::Registry::new(Arc::new(provider.clone())).await;
        let mut session = Session::create(None,None); session.working_dir = Some(old.to_string_lossy().into());
        let (mut agent,_) = Agent::prepare_primary_session(Arc::new(provider.clone()),registry,session,StartupContextActivation::primary(StartupContextCaller::HarnessApi),AgentSelection::Default,false,InstructionRepositoryService::new())?;
        agent.startup_context_session_mut().save()?;
        let id = agent.session_id().to_string();
        let input = LegacyLocationAdoptionRequest {request:RequestId::new(),session:id.clone(),expected_working_dir:Some(old.clone()),expected_catalog_revision:service.status()?.revision,placement:Placement::Standalone(target),cwd:next.clone()};
        let host = Arc::new(PrimaryHost::default());
        agent.primary_owner = Some(host.adopt_owner(&agent)?);
        let agent = Arc::new(tokio::sync::Mutex::new(agent));
        host.write().await.insert(id.clone(),agent.clone());
        assert!(matches!(host.request_location(PrimaryLocationCommand::AdoptLegacy{request:input.clone()}).await,PrimaryLocationResponse::Rejected {issue} if issue.code == IssueCode::UnsupportedCapability));
        std::fs::write(crate::config::Config::path().unwrap(),"[features]\nmanaged_primary_launch = true\n")?;
        crate::config::Config::invalidate_cache();
        // Inspection remains available. An unreviewed primary cannot dispatch.
        let failure = agent.lock().await.run_once_capture("SYNTHETIC RETAINED LEGACY INPUT").await.unwrap_err();
        assert!(failure.to_string().contains("Legacy primary requires explicit placement"));
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        let before = Session::load(&id)?;
        let result = host.request_location(PrimaryLocationCommand::AdoptLegacy{request:input.clone()}).await;
        let PrimaryLocationResponse::State {record} = result else { anyhow::bail!("adoption failed: {result:?}") };
        assert_eq!(record.state,LocationChangeState::Complete);
        assert!(host.processing(&id).is_none());
        let after = Session::load(&id)?;
        assert_eq!(after.working_dir.as_deref(),next.to_str());
        assert_eq!(after.location.as_ref().unwrap().initial_cwd,old);
        assert_eq!(after.system_prompt,before.system_prompt);
        assert_eq!(after.startup_context,before.startup_context);
        assert_eq!(after.messages.len(),before.messages.len()+1);
        assert_eq!(serde_json::to_value(&after.messages[..before.messages.len()])?,serde_json::to_value(&before.messages)?);
        assert_eq!(host.request_location(PrimaryLocationCommand::AdoptLegacy{request:input}).await,PrimaryLocationResponse::State {record});
        assert_eq!(Session::load(&id)?.messages.len(),after.messages.len());
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        host.shutdown().await?;
        Ok(())
    })
}

#[test]
#[cfg(target_os = "macos")]
fn shared_clear_carries_only_reviewed_direct_grants_as_independent_authority() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    std::fs::write(
        crate::config::Config::path().unwrap(),
        "[features]\nmanaged_primary_launch = true\n",
    )?;
    crate::config::Config::invalidate_cache();
    tokio::runtime::Runtime::new()?.block_on(async {
        let service=WorkspaceService::new(&crate::storage::durable_state_dir());service.initialize(RequestId::new())?;
        let mut roots=Vec::new();
        for name in ["source","other"] {let path=temp.path().join(name);std::fs::create_dir(&path)?;let path=path.canonicalize()?;let EntityId::Location(id)=change(&service,OrganizationChange::RegisterLocation{name:name.into(),path:path.clone(),registration:Registration::Standalone})?.targets[0] else {panic!()};roots.push((id,path));}
        let provider=FixtureProvider::default();let pool=Arc::new(crate::mcp::SharedMcpPool::new(Default::default()));
        let launcher=PrimaryLauncher{workspace:service.clone(),repositories:InstructionRepositoryService::new(),provider:Arc::new(provider.clone()),registry:PrimaryRegistryMode::Shared(pool.clone())};
        let host=Arc::new(PrimaryHost::default());
        let created=launcher.launch_hosted(&host,RequestId::new(),service.status()?.revision,PrimaryLaunchInput{placement:PrimaryPlacement::Existing{placement:Placement::Standalone(roots[0].0)},cwd:Some(PrimaryCwd::Existing{path:roots[0].1.clone()}),agent:None,model:None,selfdev:false},StartupContextCaller::HarnessApi).await?;
        let source=host.read().await.get(&created.session).cloned().unwrap();
        let auth=WorkspaceClientAuthority::authenticated("human-fixture")?;
        let review=service.review_grant_change(service.status()?.revision,GrantChange::Issue{audience:Audience::Session(created.session.clone()),target:WriteTarget::Root(roots[1].0),proposal:None})?;
        let original=service.apply_grant_change(&auth,RequestId::new(),review.id)?.grant.unwrap();
        let before=Session::load(&created.session)?;
        assert!(host.clear_context(&source,&created.session,&launcher.repositories,&pool).await.is_err());
        assert_eq!(host.read().await.len(),1);
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        let review=service.review_grant_carry(&created.session)?;
        let child=host.clear_context_with_grants(&source,&created.session,&launcher.repositories,&pool,Some(GrantCarryChoice{review:review.id,carry:true})).await?;
        let child_id=child.lock().await.session_id().to_owned();
        let copied=service.session_write_scope(&Session::load(&child_id)?)?.grants.into_iter().find(|g|g.copied_from==Some(original.id)).unwrap();
        assert_ne!(copied.id,original.id);
        let revoke=service.review_grant_change(service.status()?.revision,GrantChange::Revoke{grant:original.id})?;service.apply_grant_change(&auth,RequestId::new(),revoke.id)?;
        let target=roots[1].1.join("copied-authority.txt");
        child.lock().await.execute_tool("write",serde_json::json!({"file_path":target,"content":"independent copy","intent":"Exercise copied scope"})).await?;
        assert_eq!(std::fs::read_to_string(&target)?,"independent copy");
        let review=service.review_grant_carry(&child_id)?;
        let dropped=host.clear_context_with_grants(&child,&child_id,&launcher.repositories,&pool,Some(GrantCarryChoice{review:review.id,carry:false})).await?;
        assert!(dropped.lock().await.execute_tool("write",serde_json::json!({"file_path":target,"content":"not allowed","intent":"Exercise drop"})).await.is_err());
        assert_eq!(std::fs::read_to_string(&target)?,"independent copy");
        let after=Session::load(&created.session)?;
        assert_eq!(serde_json::to_value(after.messages)?,serde_json::to_value(before.messages)?);assert_eq!(after.system_prompt,before.system_prompt);
        assert_eq!(provider.0.load(Ordering::SeqCst),0);
        host.shutdown().await?;
        Ok(())
    })
}

#[test]
#[cfg(target_os = "macos")]
fn scope_notices_are_durable_non_waking_and_do_not_delay_revocation() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir()?;
    let _env = Env::new(temp.path());
    crate::config::Config::invalidate_cache();
    crate::instruction::SystemPromptComposer::new().ensure_global_store()?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let service=WorkspaceService::new(&crate::storage::durable_state_dir());service.initialize(RequestId::new())?;
        let mut roots=Vec::new();for name in ["source","other"] {let path=temp.path().join(name);std::fs::create_dir(&path)?;let path=path.canonicalize()?;let EntityId::Location(id)=change(&service,OrganizationChange::RegisterLocation{name:name.into(),path:path.clone(),registration:Registration::Standalone})?.targets[0] else {panic!()};roots.push((id,path));}
        let provider=FixtureProvider::default();let host=Arc::new(PrimaryHost::default());
        let launcher=PrimaryLauncher{workspace:service.clone(),repositories:InstructionRepositoryService::new(),provider:Arc::new(provider.clone()),registry:PrimaryRegistryMode::Process};
        let created=launcher.launch_hosted(&host,RequestId::new(),service.status()?.revision,PrimaryLaunchInput{placement:PrimaryPlacement::Existing{placement:Placement::Standalone(roots[0].0)},cwd:Some(PrimaryCwd::Existing{path:roots[0].1.clone()}),agent:None,model:None,selfdev:false},StartupContextCaller::HarnessApi).await?;
        let agent=host.read().await.get(&created.session).cloned().unwrap();
        host.reconcile_idle_scope_notices().await;
        let initial=Session::load(&created.session)?;assert!(initial.scope_notice.is_some());
        let auth=WorkspaceClientAuthority::authenticated("notice-fixture")?;
        let review=service.review_grant_change(service.status()?.revision,GrantChange::Issue{audience:Audience::Session(created.session.clone()),target:WriteTarget::Root(roots[1].0),proposal:None})?;
        let grant=service.apply_grant_change(&auth,RequestId::new(),review.id)?.grant.unwrap();
        // An exclusively owned active batch cannot be interrupted by idle polling.
        let busy=agent.lock().await;
        host.reconcile_idle_scope_notices().await;
        assert_eq!(Session::load(&created.session)?.scope_notice,initial.scope_notice);
        drop(busy);host.reconcile_idle_scope_notices().await;
        let granted=Session::load(&created.session)?;assert_ne!(granted.scope_notice,initial.scope_notice);
        assert_eq!(granted.system_prompt,initial.system_prompt);assert_eq!(granted.active_skill,initial.active_skill);
        assert_eq!(serde_json::to_value(&granted.messages[..initial.messages.len()])?,serde_json::to_value(&initial.messages)?);
        let review=service.review_grant_change(service.status()?.revision,GrantChange::Revoke{grant:grant.id})?;
        service.apply_grant_change(&auth,RequestId::new(),review.id)?;
        // Broken occurrence text prevents explanation, never current enforcement.
        let path=crate::storage::jcode_dir()?.join("instructions/notifications/session-write-access-changed.md");
        let old=std::fs::read(&path)?;std::fs::write(&path,[0xff])?;
        host.reconcile_idle_scope_notices().await;
        assert_eq!(Session::load(&created.session)?.scope_notice,granted.scope_notice);
        assert!(agent.lock().await.execute_tool("write",serde_json::json!({"file_path":roots[1].1.join("denied"),"content":"no","intent":"Verify revocation"})).await.is_err());
        std::fs::write(&path,old)?;
        host.reconcile_idle_scope_notices().await;
        let revoked=Session::load(&created.session)?;assert_ne!(revoked.scope_notice,granted.scope_notice);
        host.reconcile_idle_scope_notices().await;
        assert_eq!(Session::load(&created.session)?.messages.len(),revoked.messages.len());
        assert_eq!(provider.0.load(Ordering::SeqCst),0);host.shutdown().await?;Ok(())
    })
}
