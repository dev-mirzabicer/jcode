//! Production registry and provider constructors, synthetic credentials only.
use super::register_external_provider_runtimes;
use jcode_base::model_roster::*;
use jcode_provider_core::{CredentialMode, ModelRoute, Provider};
use std::sync::Arc;

struct CatalogOnly(Vec<ModelRoute>);
#[async_trait::async_trait]
impl Provider for CatalogOnly {
    async fn complete(
        &self,
        _: &[jcode_base::message::Message],
        _: &[jcode_base::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<jcode_provider_core::EventStream> {
        panic!("no model calls in roster tests")
    }
    fn name(&self) -> &str {
        "fixture-catalog"
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        self.0.clone()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(self.0.clone()))
    }
}

fn native_route(model: &str, method: &str, provider: &str) -> ModelRoute {
    ModelRoute {
        model: model.into(),
        api_method: method.into(),
        provider: provider.into(),
        available: true,
        detail: String::new(),
        cheapness: None,
    }
}

#[test]
fn roster_native_routes_use_production_auth_effort_and_source_free_restore() {
    let sandbox = jcode_base::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let expires = chrono::Utc::now().timestamp_millis() + 3_600_000;
    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "fixture".into(),
        access: "synthetic-access".into(),
        refresh: "synthetic-refresh".into(),
        expires,
        email: None,
        subscription_type: Some("max".into()),
        scopes: vec!["user:inference".into()],
    })
    .unwrap();
    jcode_base::auth::codex::upsert_account(jcode_base::auth::codex::OpenAiAccount {
        label: "fixture".into(),
        access_token: "synthetic-access".into(),
        refresh_token: "synthetic-refresh".into(),
        expires_at: Some(expires),
        id_token: None,
        account_id: Some("fixture-account".into()),
        email: None,
    })
    .unwrap();
    sandbox
        .write_env_file("openai.env", "OPENAI_API_KEY", "synthetic-api-key")
        .unwrap();
    sandbox
        .write_env_file("anthropic.env", "ANTHROPIC_API_KEY", "synthetic-api-key")
        .unwrap();
    register_external_provider_runtimes();
    let routes = CatalogOnly(vec![
        native_route("gpt-6-astra", "openai-oauth", "OpenAI"),
        native_route("gpt-6-astra", "openai-api", "OpenAI"),
        native_route("claude-fable-5", "claude-oauth", "Anthropic"),
        native_route("claude-fable-5", "claude-api", "Anthropic"),
    ]);
    let catalog = RosterCatalog::from_provider(&routes).unwrap();
    let roster = ModelRoster::parse("[aliases.fixture]\ndescription='synthetic'\nmodels=['openai-oauth:gpt-6-astra','claude-oauth:claude-fable-5']\ndefault_effort='xhigh'").unwrap();
    let request = ModelRosterRequest::alias("fixture");
    let prepared = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(prepared.provider.credential_mode(), CredentialMode::OAuth);
    assert_eq!(prepared.provider.model(), "gpt-6-astra");
    assert_eq!(
        prepared.provider.reasoning_effort().as_deref(),
        Some("xhigh")
    );
    let stored = serde_json::to_vec(&prepared.resolution).unwrap();
    let mut request = request;
    request.model_override = Some(QualifiedModel::parse("claude-api:claude-fable-5").unwrap());
    request.effort_override = Some("max".into());
    let api = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(api.provider.credential_mode(), CredentialMode::ApiKey);
    assert_eq!(api.provider.reasoning_effort().as_deref(), Some("max"));
    assert_eq!(prepared.provider.model(), "gpt-6-astra");
    assert_eq!(
        prepared.provider.reasoning_effort().as_deref(),
        Some("xhigh")
    );
    drop(roster);
    let restored: ModelRosterResolution = serde_json::from_slice(&stored).unwrap();
    let runtime = restored.restore_provider().unwrap();
    assert_eq!(runtime.model(), "gpt-6-astra");
    assert_eq!(runtime.credential_mode(), CredentialMode::OAuth);
    assert_eq!(runtime.reasoning_effort().as_deref(), Some("xhigh"));
}

#[test]
fn roster_named_and_pinned_routes_preserve_endpoints_and_provider_effort_aliases() {
    let sandbox = jcode_base::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    std::fs::write(
        sandbox.root().join("config.toml"),
        r#"
[providers.roster-gateway]
base_url = "http://127.0.0.1:9/v1"
auth = "none"
default_model = "opaque/model"
[[providers.roster-gateway.models]]
id = "opaque/model"
[providers.groq]
base_url = "http://127.0.0.1:9/v1"
auth = "none"
default_model = "private-model"
[[providers.groq.models]]
id = "private-model"
"#,
    )
    .unwrap();
    jcode_base::config::invalidate_config_cache();
    sandbox
        .write_env_file("openrouter.env", "OPENROUTER_API_KEY", "synthetic-api-key")
        .unwrap();
    jcode_base::env::set_var("JCODE_OPENROUTER_CACHE_NAMESPACE", "primary-fixture");
    register_external_provider_runtimes();
    let catalog = RosterCatalog::from_provider(&CatalogOnly(vec![
        native_route(
            "opaque/model",
            "openai-compatible:roster-gateway",
            "roster-gateway",
        ),
        native_route("vendor/model", "openrouter", "Pinned"),
        native_route("private-model", "openai-compatible:groq", "groq"),
        native_route("provided-model", "openai-compatible:groq", "Groq"),
    ]))
    .unwrap();
    let roster = ModelRoster::parse("aliases={}").unwrap();
    let mut request = ModelRosterRequest {
        model_override: Some(QualifiedModel::parse("roster-gateway:opaque/model").unwrap()),
        ..Default::default()
    };
    let named = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(
        named.resolution.provider_key(),
        "openai-compatible:roster-gateway"
    );
    assert_eq!(
        named
            .provider
            .direct_openai_compatible_route_parts()
            .unwrap()
            .1,
        "openai-compatible:roster-gateway"
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE").unwrap(),
        "primary-fixture"
    );
    request.model_override = Some(QualifiedModel::parse("openrouter:vendor/model@Pinned").unwrap());
    request.effort_override = Some("max".into());
    let pinned = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(
        pinned.provider.preferred_provider().as_deref(),
        Some("Pinned")
    );
    assert_eq!(pinned.resolution.selected_effort(), Some("xhigh"));
    let restored = pinned.resolution.restore_provider().unwrap();
    assert_eq!(restored.preferred_provider().as_deref(), Some("Pinned"));
    assert_eq!(restored.reasoning_effort().as_deref(), Some("xhigh"));
    assert_eq!(named.provider.model(), "opaque/model");
    request.model_override =
        Some(QualifiedModel::parse("openai-compatible:groq:private-model").unwrap());
    request.effort_override = None;
    let collision = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(
        collision
            .provider
            .direct_openai_compatible_route_parts()
            .unwrap()
            .2,
        "http://127.0.0.1:9/v1"
    );
    request.model_override = Some(QualifiedModel::parse("groq:provided-model").unwrap());
    assert!(
        matches!(roster.resolve(&request, &catalog), Err(ModelRosterError::AllCandidatesRejected { candidates, .. }) if matches!(candidates[0].reason, CandidateFailure::CredentialsUnavailable(_)))
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE").unwrap(),
        "primary-fixture"
    );
    jcode_base::config::invalidate_config_cache();
}
