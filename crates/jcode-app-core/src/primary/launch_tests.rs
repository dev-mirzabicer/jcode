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
    }
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
