//! Independent route construction for new execution policies. No live provider
//! is mutated and no process-wide provider-profile environment is installed.

use super::{CredentialMode, ModelRouteApiMethod, Provider, RouteSelection, RuntimeKey, external};
use anyhow::{Result, bail};
use std::sync::Arc;

#[derive(Debug)]
pub struct MissingRouteCredentials(pub String);
impl std::fmt::Display for MissingRouteCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MissingRouteCredentials {}

/// Presence comes from the auth owner's cached local probe. Expiry/refresh and
/// account failover remain provider-owned, not a second roster auth policy.
pub fn credentials_present(status: &crate::auth::AuthStatus, key: &RuntimeKey) -> Option<bool> {
    use crate::auth::AuthState;
    Some(match key {
        RuntimeKey::ClaudeOAuth => status.anthropic.has_oauth,
        RuntimeKey::AnthropicApiKey => status.anthropic.has_api_key,
        RuntimeKey::OpenAIOAuth => status.openai_has_oauth,
        RuntimeKey::OpenAIApiKey => status.openai_has_api_key,
        RuntimeKey::JcodeSubscription => status.jcode != AuthState::NotConfigured,
        RuntimeKey::OpenRouter => status.openrouter != AuthState::NotConfigured,
        RuntimeKey::Copilot => status.copilot_has_api_token,
        RuntimeKey::Bedrock => status.bedrock != AuthState::NotConfigured,
        RuntimeKey::Cursor => status.cursor != AuthState::NotConfigured,
        RuntimeKey::Antigravity => status.antigravity != AuthState::NotConfigured,
        // Named profiles may deliberately need no credentials. Their own
        // constructor validates the declared auth method and key source.
        _ => return None,
    })
}

pub fn instantiate_route(selection: &RouteSelection) -> Result<Arc<dyn Provider>> {
    if selection.model.trim().is_empty()
        || selection.runtime_key
            != RuntimeKey::from_api_method(
                &ModelRouteApiMethod::parse(&selection.api_method),
                &selection.provider_label,
            )
    {
        bail!("invalid or inconsistent concrete execution route");
    }
    if credentials_present(
        &crate::auth::AuthStatus::check_fast(),
        &selection.runtime_key,
    ) == Some(false)
    {
        return Err(MissingRouteCredentials(format!(
            "credentials unavailable for {}",
            selection.runtime_key.stable_id()
        ))
        .into());
    }
    use external::OpenRouterRuntimeSpec;
    let runtime: Arc<dyn Provider> = match &selection.runtime_key {
        RuntimeKey::ClaudeOAuth | RuntimeKey::AnthropicApiKey => {
            registered(external::ANTHROPIC_RUNTIME)?
        }
        RuntimeKey::OpenAIOAuth | RuntimeKey::OpenAIApiKey => registered(external::OPENAI_RUNTIME)?,
        RuntimeKey::Copilot => registered(external::COPILOT_RUNTIME)?,
        RuntimeKey::Cursor => registered(external::CURSOR_RUNTIME)?,
        RuntimeKey::Antigravity => registered(external::ANTIGRAVITY_RUNTIME)?,
        RuntimeKey::Bedrock => Arc::new(super::bedrock::BedrockProvider::new()),
        RuntimeKey::OpenRouter => {
            external::instantiate_openrouter_runtime(OpenRouterRuntimeSpec::OpenRouterApiKey)?
        }
        RuntimeKey::OpenAiCompatible {
            profile_id: Some(id),
        } => {
            // named_provider_profile_routes stores the exact configured name
            // in provider_label. Honor that structural identity even when a
            // built-in profile has the same ID. Never silently rebind it.
            let spec = if selection.provider_label == *id {
                let config = crate::config::config()
                    .providers
                    .get(id)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("named provider profile '{id}' is not configured")
                    })?;
                OpenRouterRuntimeSpec::NamedProfileForExecution {
                    name: id.clone(),
                    config,
                }
            } else if let Some(profile) =
                crate::provider_catalog::resolve_openai_compatible_profile_selection(id)
            {
                OpenRouterRuntimeSpec::CompatibleProfile(profile)
            } else {
                bail!("provider profile '{id}' is not configured")
            };
            external::instantiate_openrouter_runtime(spec)?
        }
        RuntimeKey::JcodeSubscription => {
            if !crate::subscription_catalog::is_model_allowed_for_current_tier(&selection.model) {
                bail!("model is unavailable for the current Jcode subscription tier");
            }
            external::instantiate_openrouter_runtime(OpenRouterRuntimeSpec::JcodeSubscription)?
        }
        _ => bail!(
            "route '{}' has no independent execution constructor",
            selection.runtime_key.stable_id()
        ),
    };
    let mode = match selection.runtime_key {
        RuntimeKey::ClaudeOAuth | RuntimeKey::OpenAIOAuth => Some(CredentialMode::OAuth),
        RuntimeKey::AnthropicApiKey | RuntimeKey::OpenAIApiKey => Some(CredentialMode::ApiKey),
        _ => None,
    };
    if let Some(mode) = mode {
        runtime.set_credential_mode(mode)?;
    }
    let model = if selection.runtime_key == RuntimeKey::OpenRouter {
        selection.routed_model_spec()
    } else {
        selection.model.clone()
    };
    runtime.set_model(&model)?;
    validate_runtime_identity(runtime.as_ref(), selection)?;
    Ok(runtime)
}

fn registered(key: &str) -> Result<Arc<dyn Provider>> {
    if !external::external_provider_registered(key) {
        bail!("runtime '{key}' is not registered");
    }
    external::instantiate_external_provider(key).ok_or_else(|| {
        MissingRouteCredentials(format!(
            "runtime '{key}' is unavailable or its credentials could not be loaded"
        ))
        .into()
    })
}

pub fn validate_runtime_identity(runtime: &dyn Provider, selection: &RouteSelection) -> Result<()> {
    let actual_model = runtime.model();
    let expected_model = if selection.runtime_key == RuntimeKey::OpenRouter {
        selection.routed_model_spec()
    } else {
        selection.model.clone()
    };
    if actual_model != expected_model && actual_model != selection.model {
        bail!("runtime selected model '{actual_model}' instead of '{expected_model}'");
    }
    match &selection.runtime_key {
        RuntimeKey::ClaudeOAuth | RuntimeKey::OpenAIOAuth
            if runtime.credential_mode() != CredentialMode::OAuth =>
        {
            bail!("runtime did not retain the OAuth credential route")
        }
        RuntimeKey::AnthropicApiKey | RuntimeKey::OpenAIApiKey
            if runtime.credential_mode() != CredentialMode::ApiKey =>
        {
            bail!("runtime did not retain the API-key credential route")
        }
        RuntimeKey::OpenRouter => {
            if !runtime.supports_provider_routing_features() {
                bail!("runtime is not the OpenRouter aggregator");
            }
            if !selection.provider_label.eq_ignore_ascii_case("auto")
                && runtime.preferred_provider().as_deref()
                    != Some(selection.provider_label.as_str())
            {
                bail!("runtime did not retain the requested OpenRouter provider pin");
            }
        }
        RuntimeKey::OpenAiCompatible { .. } | RuntimeKey::JcodeSubscription => {
            let Some((label, method, _)) = runtime.direct_openai_compatible_route_parts() else {
                bail!("runtime has no explicit endpoint identity")
            };
            let actual = RuntimeKey::from_api_method(&ModelRouteApiMethod::parse(&method), &label);
            if actual != selection.runtime_key {
                bail!("runtime endpoint differs from the selected route");
            }
        }
        _ => {}
    }
    Ok(())
}
