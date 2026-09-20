//! Complete primary creation over existing provider, instruction, Startup Context,
//! Session and catalog owners. This service does not dispatch a model turn.
use super::{PrimaryHost, PrimaryLease};
use crate::workspace::*;
use crate::{
    agent::{Agent, StartupContextActivation, StartupContextCaller},
    instruction::{AgentSelection, InstructionRepositoryService},
    provider::{ModelRoute, Provider, RouteSelection},
    session::{Session, StoredPrimaryCreation},
    tool::Registry,
};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;

pub enum PrimaryRegistryMode {
    Shared(Arc<crate::mcp::SharedMcpPool>),
    Process,
}

pub struct PrimaryLauncher {
    pub workspace: WorkspaceService,
    pub repositories: InstructionRepositoryService,
    pub provider: Arc<dyn Provider>,
    pub registry: PrimaryRegistryMode,
}
impl PrimaryLauncher {
    pub async fn launch_hosted(
        &self,
        host: &Arc<PrimaryHost>,
        request: RequestId,
        expected: Revision,
        input: PrimaryLaunchInput,
        caller: StartupContextCaller,
    ) -> Result<PrimaryLaunchRecord> {
        let _lease = self.workspace.primary_launch_lease(request)?;
        if let Ok(record) = self.workspace.inspect_primary_launch(request) {
            ensure!(
                record.input == input,
                "Launch request already has different settings"
            );
            if record.state == PrimaryLaunchState::Complete
                && host.read().await.contains_key(&record.session)
            {
                return Ok(record);
            }
        }
        ensure!(
            host.accepting.load(std::sync::atomic::Ordering::Acquire),
            "Primary runtime is stopping"
        );
        let (agent, record) = self.prepare(request, expected, input, caller).await?;
        let mut agents = host.write().await;
        ensure!(
            host.accepting.load(std::sync::atomic::Ordering::Acquire),
            "Primary {} was persisted but the runtime is stopping; retry attachment after Start",
            record.session
        );
        ensure!(
            !agents.contains_key(&record.session),
            "Primary was already adopted; inspect the original launch result"
        );
        host.adopt_owner(&agent)?;
        host.resources.lock().expect("primary resources").insert(
            record.session.clone(),
            super::PrimaryResources::from_agent(&agent),
        );
        agents.insert(
            record.session.clone(),
            Arc::new(tokio::sync::Mutex::new(agent)),
        );
        Ok(record)
    }

    /// Process-owned inference retains exactly the same writer lease and launch
    /// transaction, without advertising detached execution.
    pub async fn launch_local(
        &self,
        request: RequestId,
        expected: Revision,
        input: PrimaryLaunchInput,
        caller: StartupContextCaller,
    ) -> Result<(Agent, PrimaryLaunchRecord)> {
        let _lease = self.workspace.primary_launch_lease(request)?;
        self.prepare(request, expected, input, caller).await
    }

    async fn prepare(
        &self,
        request: RequestId,
        expected: Revision,
        input: PrimaryLaunchInput,
        caller: StartupContextCaller,
    ) -> Result<(Agent, PrimaryLaunchRecord)> {
        let saved = match self.workspace.inspect_primary_launch(request) {
            Ok(record) => {
                ensure!(
                    record.input == input,
                    "Launch request already has different settings"
                );
                Some(record)
            }
            Err(error) if error.code == IssueCode::InvalidIdentity => None,
            Err(error) => return Err(error.into()),
        };
        let mut model = match &saved {
            Some(record) => record.concrete_model.clone(),
            None => select_model(self.provider.as_ref(), input.model.as_ref())?,
        };
        let provider = instantiate(self.provider.as_ref(), &model)?;
        if saved.is_some() {
            ensure!(
                provider.reasoning_effort() == model.effort,
                "The stored concrete effort is no longer available; choose launch settings explicitly"
            );
        } else {
            model.effort = provider.reasoning_effort();
        }
        ensure!(
            !provider.handles_tools_internally(),
            "Selected provider performs tools outside the native managed-primary boundary"
        );
        let selection = AgentSelection::parse(input.agent.as_deref())?;
        let record = self
            .workspace
            .reserve_primary_launch(request, expected, input, model)?;
        let result = self
            .prepare_record(&record, provider, selection, caller)
            .await;
        match result {
            Ok(agent) => Ok((agent, self.workspace.inspect_primary_launch(request)?)),
            Err(error) => {
                let receipt = self
                    .workspace
                    .record_primary_launch_failure(request, error.to_string());
                match receipt {
                    Ok(_) => Err(error),
                    Err(storage) => Err(error.context(format!(
                        "Launch failure receipt could not be saved: {storage}"
                    ))),
                }
            }
        }
    }

    async fn prepare_record(
        &self,
        record: &PrimaryLaunchRecord,
        provider: Arc<dyn Provider>,
        selection: AgentSelection,
        caller: StartupContextCaller,
    ) -> Result<Agent> {
        if crate::session::session_exists(&record.session) {
            let owner = Arc::new(PrimaryLease::acquire(&record.session)?);
            let stored = Session::load_startup_stub(&record.session)?;
            let creation = stored
                .primary_creation
                .as_ref()
                .context("Existing Session is not owned by this launch")?;
            ensure!(
                creation.request == record.request && creation.operation == record.operation,
                "Existing Session belongs to another launch"
            );
            if creation.ready {
                self.workspace.reconcile_primary_launch(record.request)?;
                let registry = self.registry(provider.clone()).await?;
                return Agent::restore_primary(
                    &record.session,
                    provider,
                    registry,
                    self.repositories.clone(),
                    owner,
                );
            }
            ensure!(
                record.state != PrimaryLaunchState::RecoveryRequired,
                "Incomplete Session retained for explicit recovery; review a fresh launch instead of replaying restored effects"
            );
            crate::session::remove_unpublished_session(&record.session)?;
            drop(owner);
        } else {
            ensure!(
                record.state != PrimaryLaunchState::Complete,
                "Published Session is missing; restore its data rather than recreate its history"
            );
        }
        let placement = self.workspace.prepare_launch_filesystem(record.request)?;
        let prepared = self.workspace.prepare_primary_location(
            placement,
            record.input.cwd.as_ref().map(PrimaryCwd::path),
            record.operation,
        )?;
        let registry = self.registry(provider.clone()).await?;
        let mut session = Session::create_with_id(record.session.clone(), None, None);
        session.working_dir = Some(
            prepared
                .location
                .cwd
                .observed_path()
                .to_str()
                .context("Cwd is not UTF-8")?
                .into(),
        );
        session.location = Some(prepared.location.clone());
        session.primary_creation = Some(StoredPrimaryCreation {
            request: record.request,
            operation: record.operation,
            ready: false,
        });
        let (mut agent, outcome) = Agent::prepare_primary_session(
            provider,
            registry,
            session,
            StartupContextActivation::primary(caller),
            selection,
            record.input.selfdev,
            self.repositories.clone(),
        )?;
        if outcome.is_blocked() {
            agent.mark_closed();
            crate::tool::clear_session_tool_policy(&record.session);
            crate::session::remove_unpublished_session(&record.session)?;
            anyhow::bail!(
                "Startup Context has unresolved required files; edit the selection and retry the retained launch request"
            );
        }
        let actual_model = agent.provider_model();
        let actual_effort = agent.provider_handle().reasoning_effort();
        let route = concrete_selection(&record.concrete_model);
        let session = agent.startup_context_session_mut();
        session.model = Some(actual_model);
        session.provider_key = Some(route.runtime_key.stable_id());
        session.route_api_method = Some(route.api_method);
        session.reasoning_effort = actual_effort;
        session
            .primary_creation
            .as_mut()
            .context("Preparation lost its launch identity")?
            .ready = true;
        session.save()?;
        drop(prepared);
        self.workspace.reconcile_primary_launch(record.request)?;
        Ok(agent)
    }

    async fn registry(&self, provider: Arc<dyn Provider>) -> Result<Registry> {
        Ok(match &self.registry {
            PrimaryRegistryMode::Shared(pool) => {
                Registry::new_for_shared_session(provider, pool.clone(), self.repositories.clone())
                    .await?
            }
            PrimaryRegistryMode::Process => Registry::new(provider).await,
        })
    }
}

fn select_model(provider: &dyn Provider, requested: Option<&PrimaryModel>) -> Result<PrimaryModel> {
    let routes = provider.model_routes();
    if let Some(requested) = requested {
        ensure!(
            routes.iter().any(|route| route.available
                && route.model == requested.model
                && route.provider == requested.provider
                && route.api_method == requested.api_method),
            "Selected concrete model route is unavailable"
        );
        return Ok(requested.clone());
    }
    let direct = provider.direct_openai_compatible_route_parts();
    let candidates = routes
        .iter()
        .filter(|route| route.available && route.model == provider.model())
        .filter(|route| {
            if let Some((label, method, _)) = &direct {
                route.provider == *label && route.api_method == *method
            } else {
                crate::provider::route_execution::validate_runtime_identity(
                    provider,
                    &RouteSelection::from_model_route(route),
                )
                .is_ok()
            }
        })
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "Current model has no unambiguous concrete route; select its provider and API method explicitly"
    );
    let route = candidates[0];
    Ok(PrimaryModel {
        model: route.model.clone(),
        provider: route.provider.clone(),
        api_method: route.api_method.clone(),
        effort: provider.reasoning_effort(),
    })
}
fn instantiate(template: &dyn Provider, model: &PrimaryModel) -> Result<Arc<dyn Provider>> {
    let selection = concrete_selection(model);
    instantiate_selection(template, model, selection)
}

fn concrete_selection(model: &PrimaryModel) -> RouteSelection {
    let route = ModelRoute {
        model: model.model.clone(),
        provider: model.provider.clone(),
        api_method: model.api_method.clone(),
        available: true,
        detail: String::new(),
        cheapness: None,
    };
    RouteSelection::from_model_route(&route)
}

fn instantiate_selection(
    template: &dyn Provider,
    model: &PrimaryModel,
    selection: RouteSelection,
) -> Result<Arc<dyn Provider>> {
    let provider = if template.model() == model.model
        && template.model_routes().iter().any(|r| {
            r.model == model.model
                && r.provider == model.provider
                && r.api_method == model.api_method
        })
        && crate::provider::route_execution::validate_runtime_identity(template, &selection).is_ok()
    {
        template.fork_for_new_session()
    } else {
        crate::provider::route_execution::instantiate_route(&selection)?
    };
    crate::provider::route_execution::validate_runtime_identity(provider.as_ref(), &selection)?;
    if let Some(effort) = &model.effort {
        ensure!(
            provider.accepts_reasoning_effort(effort),
            "Requested effort is not supported by the selected route"
        );
        provider.set_reasoning_effort(effort)?;
        ensure!(
            provider.reasoning_effort().is_some(),
            "Provider did not retain the requested effort"
        );
    }
    Ok(provider)
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
