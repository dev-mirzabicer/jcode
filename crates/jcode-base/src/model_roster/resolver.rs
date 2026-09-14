use super::*;
use crate::provider::{ModelRoute, Provider, RouteSelection};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRosterRequest {
    pub alias: Option<String>,
    pub model_override: Option<QualifiedModel>,
    pub effort_override: Option<String>,
}

impl ModelRosterRequest {
    pub fn alias(alias: impl Into<String>) -> Self {
        Self {
            alias: Some(alias.into()),
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), ModelRosterError> {
        if self.alias.is_none() && self.model_override.is_none() {
            return Err(ModelRosterError::InvalidRequest(
                "an alias or explicit model is required".into(),
            ));
        }
        if let Some(alias) = &self.alias {
            validate_alias(alias).map_err(ModelRosterError::InvalidRequest)?;
        }
        if let Some(effort) = &self.effort_override {
            validate_effort(effort).map_err(ModelRosterError::InvalidRequest)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum CandidateFailure {
    CredentialsUnavailable(String),
    RouteUnavailable(String),
    ModelUnavailable(String),
    AmbiguousRoute(String),
    UnsupportedEffort(String),
    RuntimeRejected(String),
}

impl std::fmt::Display for CandidateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, detail) = match self {
            Self::CredentialsUnavailable(d) => ("credentials unavailable", d),
            Self::RouteUnavailable(d) => ("route unavailable", d),
            Self::ModelUnavailable(d) => ("model unavailable", d),
            Self::AmbiguousRoute(d) => ("ambiguous route", d),
            Self::UnsupportedEffort(d) => ("unsupported effort", d),
            Self::RuntimeRejected(d) => ("runtime rejected", d),
        };
        write!(f, "{kind}: {detail}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CandidateRejection {
    pub model: QualifiedModel,
    pub reason: CandidateFailure,
}

/// Persist this value in the execution owner's durable state. Restoring it
/// requires neither the alias nor its source repository. Provider pin/display
/// identity is retained because API method alone cannot encode OpenRouter pins.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRosterResolution {
    requested_alias: Option<String>,
    selection: RouteSelection,
    selected_effort: Option<String>,
    used_model_override: bool,
    used_effort_override: bool,
}

impl ModelRosterResolution {
    pub fn validate_provider(&self, provider: &dyn Provider) -> Result<(), CandidateFailure> {
        crate::provider::route_execution::validate_runtime_identity(provider, &self.selection)
            .map_err(runtime_error)?;
        if provider.reasoning_effort() != self.selected_effort {
            return Err(CandidateFailure::UnsupportedEffort(
                "runtime effort differs from its stored concrete resolution".into(),
            ));
        }
        Ok(())
    }
    pub fn requested_alias(&self) -> Option<&str> {
        self.requested_alias.as_deref()
    }
    pub fn selected_model(&self) -> &str {
        &self.selection.model
    }
    pub fn provider_key(&self) -> String {
        self.selection.runtime_key.stable_id()
    }
    pub fn route_api_method(&self) -> &str {
        &self.selection.api_method
    }
    pub fn selected_effort(&self) -> Option<&str> {
        self.selected_effort.as_deref()
    }
    pub fn used_model_override(&self) -> bool {
        self.used_model_override
    }
    pub fn used_effort_override(&self) -> bool {
        self.used_effort_override
    }

    /// Restore this exact concrete identity, never an edited alias. A changed
    /// provider default or unavailable route fails rather than selecting another.
    /// The caller still owns context preflight, persistence and actual dispatch.
    pub fn restore_provider(&self) -> Result<Arc<dyn Provider>, CandidateFailure> {
        let provider = crate::provider::route_execution::instantiate_route(&self.selection)
            .map_err(runtime_error)?;
        if self.selected_effort.is_none() && provider.reasoning_effort().is_some() {
            provider
                .set_reasoning_effort("")
                .map_err(|error| CandidateFailure::UnsupportedEffort(error.to_string()))?;
        }
        apply_effort(provider.as_ref(), self.selected_effort.as_deref())?;
        if provider.reasoning_effort() != self.selected_effort {
            return Err(CandidateFailure::UnsupportedEffort(
                "stored effective effort cannot be restored exactly".into(),
            ));
        }
        Ok(provider)
    }
}

pub struct PreparedRosterExecution {
    pub resolution: ModelRosterResolution,
    pub provider: Arc<dyn Provider>,
    pub rejected_candidates: Vec<CandidateRejection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRosterPreview {
    pub resolution: ModelRosterResolution,
    pub rejected_candidates: Vec<CandidateRejection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateAvailability {
    pub model: QualifiedModel,
    pub result: Result<ModelRosterResolution, CandidateFailure>,
}

type RouteFactory = fn(&RouteSelection) -> anyhow::Result<Arc<dyn Provider>>;

/// One current production catalog snapshot plus the provider-owned independent
/// construction boundary. No model request, fallback resend or auth refresh is
/// initiated by roster policy. Provider constructors retain their own behavior.
pub struct RosterCatalog {
    routes: Vec<ModelRoute>,
    factory: RouteFactory,
    auth: crate::auth::AuthStatus,
}

impl RosterCatalog {
    /// Human editing choices use exact provider/auth/pin identity, including
    /// currently unavailable routes. This performs no construction or inference.
    pub fn qualified_models(&self) -> Vec<String> {
        self.routes
            .iter()
            .filter_map(|route| {
                let selection = RouteSelection::from_model_route(route);
                QualifiedModel::parse(format!(
                    "{}:{}",
                    selection.runtime_key.stable_id(),
                    selection.routed_model_spec()
                ))
                .ok()
                .filter(|model| model.matches_route(route) && model.matches_model(route))
                .map(String::from)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn from_provider(provider: &dyn Provider) -> Result<Self, ModelRosterError> {
        let routes = provider.model_routes();
        if routes.is_empty() {
            return Err(ModelRosterError::CatalogUnavailable(
                "provider exposed no route catalog".into(),
            ));
        }
        Ok(Self {
            routes,
            factory: crate::provider::route_execution::instantiate_route,
            auth: crate::auth::AuthStatus::check_fast(),
        })
    }

    fn candidate(
        &self,
        model: &QualifiedModel,
        effort: Option<&str>,
    ) -> Result<(RouteSelection, Arc<dyn Provider>), CandidateFailure> {
        let routes = self
            .routes
            .iter()
            .filter(|route| model.matches_route(route))
            .collect::<Vec<_>>();
        if routes.is_empty() {
            return Err(CandidateFailure::RouteUnavailable(
                "no matching provider/auth route in the current catalog".into(),
            ));
        }
        let key = RouteSelection::from_model_route(routes[0]).runtime_key;
        if crate::provider::route_execution::credentials_present(&self.auth, &key) == Some(false) {
            return Err(CandidateFailure::CredentialsUnavailable(key.stable_id()));
        }
        let matches = routes
            .iter()
            .copied()
            .filter(|route| model.matches_model(route))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(CandidateFailure::ModelUnavailable(
                "model is absent under the requested route".into(),
            ));
        }
        let available = matches
            .iter()
            .copied()
            .filter(|route| route.available)
            .collect::<Vec<_>>();
        let route = match available.as_slice() {
            [] => {
                let detail = matches
                    .iter()
                    .map(|r| r.detail.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                return Err(CandidateFailure::RouteUnavailable(detail));
            }
            [route] => *route,
            _ => {
                return Err(CandidateFailure::AmbiguousRoute(
                    "multiple exact catalog routes match, select an explicit endpoint or pin"
                        .into(),
                ));
            }
        };
        let mut selection = RouteSelection::from_model_route(route);
        // Detail is display-only catalog commentary, not durable route policy.
        selection.detail.clear();
        let provider = (self.factory)(&selection).map_err(runtime_error)?;
        apply_effort(provider.as_ref(), effort)?;
        Ok((selection, provider))
    }
}

fn runtime_error(error: anyhow::Error) -> CandidateFailure {
    let detail = error.to_string();
    if error.is::<crate::provider::route_execution::MissingRouteCredentials>()
        || crate::provider::error_looks_like_credential_failure(&detail)
    {
        CandidateFailure::CredentialsUnavailable(detail)
    } else {
        CandidateFailure::RuntimeRejected(detail)
    }
}

fn apply_effort(provider: &dyn Provider, effort: Option<&str>) -> Result<(), CandidateFailure> {
    if let Some(effort) = effort {
        if !provider.accepts_reasoning_effort(effort) {
            return Err(CandidateFailure::UnsupportedEffort(format!(
                "'{effort}' is not advertised for '{}'",
                provider.model()
            )));
        }
        provider
            .set_reasoning_effort(effort)
            .map_err(|error| CandidateFailure::UnsupportedEffort(error.to_string()))?;
        if provider.reasoning_effort().is_none() {
            return Err(CandidateFailure::UnsupportedEffort(
                "provider did not retain the explicit effort".into(),
            ));
        }
    }
    // Fresh constructors, not the coordinator's mutable effort, supply defaults.
    if let Some(effective) = provider.reasoning_effort() {
        validate_effort(&effective).map_err(CandidateFailure::UnsupportedEffort)?;
        if !provider.available_efforts().contains(&effective.as_str()) {
            return Err(CandidateFailure::UnsupportedEffort(format!(
                "provider default '{effective}' is not supported"
            )));
        }
    }
    Ok(())
}

impl ModelRoster {
    pub fn resolve(
        &self,
        request: &ModelRosterRequest,
        catalog: &RosterCatalog,
    ) -> Result<PreparedRosterExecution, ModelRosterError> {
        request.validate()?;
        let entry = request
            .alias
            .as_deref()
            .map(|alias| self.inspect(alias))
            .transpose()?;
        let models = if let Some(model) = &request.model_override {
            std::slice::from_ref(model)
        } else {
            &entry.expect("validated alias request").models
        };
        let effort = request
            .effort_override
            .as_deref()
            .or_else(|| entry.and_then(|e| e.default_effort.as_deref()));
        let mut rejected = Vec::new();
        for model in models {
            match catalog.candidate(model, effort) {
                Ok((selection, provider)) => {
                    return Ok(PreparedRosterExecution {
                        resolution: ModelRosterResolution {
                            requested_alias: request.alias.clone(),
                            selection,
                            selected_effort: provider.reasoning_effort(),
                            used_model_override: request.model_override.is_some(),
                            used_effort_override: request.effort_override.is_some(),
                        },
                        provider,
                        rejected_candidates: rejected,
                    });
                }
                Err(reason) => rejected.push(CandidateRejection {
                    model: model.clone(),
                    reason,
                }),
            }
        }
        Err(ModelRosterError::AllCandidatesRejected {
            alias: request.alias.clone(),
            candidates: rejected,
        })
    }

    pub fn preview(
        &self,
        request: &ModelRosterRequest,
        catalog: &RosterCatalog,
    ) -> Result<ModelRosterPreview, ModelRosterError> {
        let prepared = self.resolve(request, catalog)?;
        Ok(ModelRosterPreview {
            resolution: prepared.resolution,
            rejected_candidates: prepared.rejected_candidates,
        })
    }

    pub fn availability(
        &self,
        alias: &str,
        catalog: &RosterCatalog,
    ) -> Result<Vec<CandidateAvailability>, ModelRosterError> {
        let entry = self.inspect(alias)?;
        Ok(entry
            .models
            .iter()
            .map(|model| CandidateAvailability {
                model: model.clone(),
                result: catalog
                    .candidate(model, entry.default_effort.as_deref())
                    .map(|(selection, provider)| ModelRosterResolution {
                        requested_alias: Some(alias.into()),
                        selection,
                        selected_effort: provider.reasoning_effort(),
                        used_model_override: false,
                        used_effort_override: false,
                    }),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
