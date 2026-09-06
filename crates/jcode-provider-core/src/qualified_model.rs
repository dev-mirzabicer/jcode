//! Explicit route selectors for policy callers. Model IDs remain opaque data.

use crate::{AuthRoute, ModelRoute, ModelRouteApiMethod, RouteSelection, RuntimeKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QualifiedModel(String);

impl QualifiedModel {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let (route, model) = value.split_once(':').ok_or_else(|| {
            "model must include an explicit provider/auth route followed by ':'".to_string()
        })?;
        if route.is_empty()
            || model.is_empty()
            || value.trim() != value
            || value.chars().any(char::is_control)
            || route.chars().any(char::is_whitespace)
            || model.trim() != model
        {
            return Err("route and model must be nonempty, without surrounding whitespace or control characters".into());
        }
        // These aliases deliberately leave authentication implicit in ordinary
        // interactive selection. A policy must not inherit that ambiguity.
        if matches!(
            route.to_ascii_lowercase().as_str(),
            "claude" | "anthropic" | "openai" | "oauth" | "api-key" | "current" | "remote-catalog"
        ) {
            return Err("provider and authentication route must both be explicit".into());
        }
        if route == "openai-compatible" {
            let (profile, model) = model
                .split_once(':')
                .ok_or_else(|| "openai-compatible requires a profile and model".to_string())?;
            if profile.is_empty() || model.trim().is_empty() || profile.trim() != profile {
                return Err("openai-compatible requires a nonempty profile and model".into());
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parts(&self) -> (&str, &str) {
        let (route, model) = self.0.split_once(':').expect("validated qualification");
        if route == "openai-compatible" {
            let (profile, model) = model.split_once(':').expect("validated profile");
            (profile, model)
        } else {
            (route, model)
        }
    }

    pub fn matches_route(&self, route: &ModelRoute) -> bool {
        let (prefix, _) = self.parts();
        let selection = RouteSelection::from_model_route(route);
        if self.0.starts_with("openai-compatible:") {
            return matches!(&selection.runtime_key, RuntimeKey::OpenAiCompatible { profile_id: Some(profile) } if prefix.eq_ignore_ascii_case(profile));
        }
        if let Some(auth) = AuthRoute::parse_explicit_credential_prefix(prefix) {
            return route.api_method_kind() == ModelRouteApiMethod::from_auth_route(auth);
        }
        let key = &selection.runtime_key;
        if let RuntimeKey::OpenAiCompatible {
            profile_id: Some(profile),
        } = key
        {
            return prefix.eq_ignore_ascii_case(profile);
        }
        // The provider domain owns route aliases, including single-auth
        // runtimes. Do not infer an endpoint from the model's family.
        prefix.eq_ignore_ascii_case(&key.stable_id())
            || ModelRouteApiMethod::parse(prefix) == route.api_method_kind()
    }

    pub fn matches_model(&self, route: &ModelRoute) -> bool {
        let (_, requested) = self.parts();
        let selection = RouteSelection::from_model_route(route);
        if selection.runtime_key == RuntimeKey::OpenRouter {
            return requested == selection.routed_model_spec()
                || (route.provider.eq_ignore_ascii_case("auto") && requested == route.model);
        }
        // Catalog IDs are opaque. In particular, do not strip [1m], dates, or
        // endpoint-local namespaces while choosing an execution identity.
        requested == route.model
            || crate::normalize_copilot_model_name(requested).is_some_and(|id| id == route.model)
    }
}

impl TryFrom<String> for QualifiedModel {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<QualifiedModel> for String {
    fn from(value: QualifiedModel) -> Self {
        value.0
    }
}

impl std::fmt::Display for QualifiedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route(model: &str, api: &str, provider: &str) -> ModelRoute {
        ModelRoute {
            model: model.into(),
            api_method: api.into(),
            provider: provider.into(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }
    }
    #[test]
    fn qualification_and_serde_reject_implicit_or_damaged_routes() {
        for text in [
            "model",
            "claude:model",
            "openai:model",
            "oauth:model",
            ":model",
            "openai-oauth:",
            "openai-compatible:model",
            "openai-compatible::model",
            "openai-oauth: model",
        ] {
            assert!(QualifiedModel::parse(text).is_err(), "{text}");
            assert!(serde_json::from_value::<QualifiedModel>(serde_json::json!(text)).is_err());
        }
    }
    #[test]
    fn auth_profiles_pins_and_context_profiles_remain_distinct() {
        let spec = QualifiedModel::parse("openai-oauth:gpt-fixture[1m]").unwrap();
        assert!(spec.matches_route(&route("gpt-fixture[1m]", "openai-oauth", "OpenAI")));
        assert!(!spec.matches_route(&route("gpt-fixture[1m]", "openai-api", "OpenAI")));
        assert!(!spec.matches_model(&route("gpt-fixture", "openai-oauth", "OpenAI")));
        let profile = QualifiedModel::parse("openai-compatible:private:opaque/model:rev").unwrap();
        let target = route("opaque/model:rev", "openai-compatible:private", "Private");
        assert!(profile.matches_route(&target) && profile.matches_model(&target));
        assert!(!profile.matches_route(&route("opaque/model:rev", "openrouter", "auto")));
        let pinned = QualifiedModel::parse("openrouter:vendor/model@Pinned").unwrap();
        assert!(pinned.matches_model(&route("vendor/model", "openrouter", "Pinned")));
        assert!(!pinned.matches_model(&route("vendor/model", "openrouter", "auto")));
        let collision = QualifiedModel::parse("openai-compatible:openai-oauth:opaque").unwrap();
        assert!(!collision.matches_route(&route("opaque", "openai-oauth", "OpenAI")));
        assert!(collision.matches_route(&route(
            "opaque",
            "openai-compatible:openai-oauth",
            "fixture"
        )));
        let subscription = QualifiedModel::parse("jcode-subscription:opaque").unwrap();
        assert!(subscription.matches_route(&route(
            "opaque",
            "jcode-subscription",
            "Jcode Subscription"
        )));
        assert!(!subscription.matches_route(&route("opaque", "openai-oauth", "OpenAI")));
    }
}
