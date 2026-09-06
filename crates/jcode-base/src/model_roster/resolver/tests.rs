use super::*;
use std::sync::RwLock;

struct FixtureProvider {
    model: String,
    effort: RwLock<Option<String>>,
}

#[async_trait::async_trait]
impl Provider for FixtureProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        panic!("roster resolution must never make a model request")
    }
    fn name(&self) -> &str {
        "fixture"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: self.model.clone(),
            effort: RwLock::new(self.reasoning_effort()),
        })
    }
    fn model(&self) -> String {
        self.model.clone()
    }
    fn reasoning_effort(&self) -> Option<String> {
        self.effort.read().unwrap().clone()
    }
    fn available_efforts(&self) -> Vec<&'static str> {
        if self.model == "limited" {
            vec!["low"]
        } else {
            vec!["low", "high", "xhigh", "max"]
        }
    }
    fn set_reasoning_effort(&self, effort: &str) -> anyhow::Result<()> {
        *self.effort.write().unwrap() = Some(effort.into());
        Ok(())
    }
}

fn factory(selection: &RouteSelection) -> anyhow::Result<Arc<dyn Provider>> {
    if selection.model == "rejected" {
        return Err(
            crate::provider::route_execution::MissingRouteCredentials("fixture".into()).into(),
        );
    }
    Ok(Arc::new(FixtureProvider {
        model: selection.model.clone(),
        effort: RwLock::new(Some("low".into())),
    }))
}

fn route(model: &str, api: &str, available: bool) -> ModelRoute {
    ModelRoute {
        model: model.into(),
        api_method: api.into(),
        provider: "fixture".into(),
        available,
        detail: if available {
            "ready"
        } else {
            "credentials unavailable"
        }
        .into(),
        cheapness: None,
    }
}

fn roster(models: &[&str], effort: Option<&str>) -> ModelRoster {
    let models = models
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(",");
    let effort = effort
        .map(|e| format!("\ndefault_effort = \"{e}\""))
        .unwrap_or_default();
    ModelRoster::parse(&format!("[aliases.work]\ndescription = \"Synthetic fixture\"\nnotes = \"human only\"\nmodels = [{models}]{effort}\n")).unwrap()
}

fn catalog() -> RosterCatalog {
    RosterCatalog {
        routes: vec![
            route("first", "openai-oauth", true),
            route("second", "claude-oauth", true),
            route("limited", "openai-oauth", true),
            route("rejected", "claude-oauth", true),
        ],
        factory,
        auth: crate::auth::AuthStatus {
            openai_has_oauth: true,
            anthropic: crate::auth::ProviderAuth {
                has_oauth: true,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

#[test]
fn parse_roundtrip_and_discovery_keep_notes_human_only() {
    let original = roster(&["openai-oauth:first", "claude-oauth:second"], Some("high"));
    assert!(original.validate().is_empty());
    let copy = ModelRoster::parse(&original.to_toml().unwrap()).unwrap();
    assert_eq!(
        copy.inspect("work").unwrap(),
        original.inspect("work").unwrap()
    );
    let listing = serde_json::to_value(copy.list()).unwrap();
    assert_eq!(listing[0].as_object().unwrap().len(), 2);
    assert!(listing[0].get("notes").is_none());
    assert!(listing[0].get("models").is_none());
}

#[test]
fn validation_is_strict_and_isolates_unrelated_invalid_aliases() {
    for invalid in [
        "models=[]\ndescription='x'",
        "models=['unqualified']\ndescription='x'",
        "models=['openai-oauth:x']",
        "models=['openai-oauth:x']\ndescription=''",
        "models=['openai-oauth:x']\ndescription='x'\ntags=['task']",
        "models=['openai-oauth:x']\ndescription='x'\ndefault_effort='swarm'",
    ] {
        let content = format!(
            "[aliases.broken]\n{invalid}\n[aliases.valid]\ndescription='fixture'\nmodels=['openai-oauth:first']\n"
        );
        let parsed = ModelRoster::parse(&content).unwrap();
        assert_eq!(parsed.validate().len(), 1, "{invalid}");
        assert!(matches!(
            parsed.inspect("broken"),
            Err(ModelRosterError::InvalidAlias { .. })
        ));
        assert!(parsed.inspect("valid").is_ok());
        assert!(parsed.to_toml().is_err());
    }
    assert!(ModelRoster::parse("aliases={}\ntags=[]").is_err());
    assert!(ModelRoster::parse("").is_err());
    assert!(ModelRoster::parse("aliases={}").unwrap().list().is_empty());
}

#[test]
fn ordered_fallback_and_effort_precedence_are_launch_only() {
    let roster = roster(&["openai-oauth:first", "claude-oauth:second"], Some("high"));
    let mut catalog = catalog();
    let request = ModelRosterRequest::alias("work");
    let first = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(first.resolution.selected_model(), "first");
    assert_eq!(first.resolution.selected_effort(), Some("high"));
    catalog.routes[0].available = false;
    catalog.auth.openai_has_oauth = false;
    let next = roster.resolve(&request, &catalog).unwrap();
    assert_eq!(next.resolution.selected_model(), "second");
    assert_eq!(next.rejected_candidates.len(), 1);
    assert!(matches!(
        next.rejected_candidates[0].reason,
        CandidateFailure::CredentialsUnavailable(_)
    ));
    assert_eq!(first.resolution.selected_model(), "first");
    let mut override_request = request;
    override_request.effort_override = Some("low".into());
    let overridden = roster.resolve(&override_request, &catalog).unwrap();
    assert_eq!(overridden.resolution.selected_effort(), Some("low"));
    assert!(overridden.resolution.used_effort_override());
}

#[test]
fn unsupported_effort_continues_and_default_is_provider_owned() {
    let roster = roster(
        &["openai-oauth:limited", "claude-oauth:second"],
        Some("xhigh"),
    );
    let result = roster
        .resolve(&ModelRosterRequest::alias("work"), &catalog())
        .unwrap();
    assert_eq!(result.resolution.selected_model(), "second");
    assert!(matches!(
        result.rejected_candidates[0].reason,
        CandidateFailure::UnsupportedEffort(_)
    ));
    let defaults = super::tests::roster(&["openai-oauth:first"], None);
    assert_eq!(
        defaults
            .resolve(&ModelRosterRequest::alias("work"), &catalog())
            .unwrap()
            .resolution
            .selected_effort(),
        Some("low")
    );
}

#[test]
fn explicit_override_bypasses_candidates_not_alias_effort_or_validation() {
    let roster = roster(&["openai-oauth:missing"], Some("high"));
    let mut request = ModelRosterRequest::alias("work");
    request.model_override = Some(QualifiedModel::parse("claude-oauth:second").unwrap());
    let result = roster.resolve(&request, &catalog()).unwrap();
    assert_eq!(result.resolution.selected_model(), "second");
    assert_eq!(result.resolution.selected_effort(), Some("high"));
    assert!(result.resolution.used_model_override());
    assert!(result.rejected_candidates.is_empty());
    request.alias = None;
    assert_eq!(
        roster
            .resolve(&request, &catalog())
            .unwrap()
            .resolution
            .selected_effort(),
        Some("low")
    );
    request.model_override = Some(QualifiedModel::parse("claude-oauth:missing").unwrap());
    assert!(
        matches!(roster.resolve(&request, &catalog()), Err(ModelRosterError::AllCandidatesRejected { candidates, .. }) if candidates.len() == 1)
    );
}

#[test]
fn complete_failure_accounting_and_preview_share_resolution() {
    let roster = roster(
        &[
            "openai-api:first",
            "openai-oauth:missing",
            "claude-oauth:rejected",
            "openai-oauth:limited",
        ],
        Some("max"),
    );
    let error = roster
        .resolve(&ModelRosterRequest::alias("work"), &catalog())
        .err()
        .unwrap();
    let ModelRosterError::AllCandidatesRejected { candidates, .. } = &error else {
        panic!("{error}")
    };
    assert_eq!(candidates.len(), 4);
    assert!(matches!(
        candidates[0].reason,
        CandidateFailure::RouteUnavailable(_)
    ));
    assert!(matches!(
        candidates[1].reason,
        CandidateFailure::ModelUnavailable(_)
    ));
    assert!(matches!(
        candidates[2].reason,
        CandidateFailure::CredentialsUnavailable(_)
    ));
    assert!(matches!(
        candidates[3].reason,
        CandidateFailure::UnsupportedEffort(_)
    ));
    assert_eq!(
        serde_json::from_str::<ModelRosterError>(&serde_json::to_string(&error).unwrap()).unwrap(),
        error
    );
    assert_eq!(roster.availability("work", &catalog()).unwrap().len(), 4);
    assert_eq!(
        roster
            .preview(&ModelRosterRequest::alias("work"), &catalog())
            .err()
            .unwrap(),
        error
    );
}

#[test]
fn durable_resolution_roundtrips_without_any_roster_source() {
    let roster = roster(&["openai-oauth:first"], Some("high"));
    let result = roster
        .resolve(&ModelRosterRequest::alias("work"), &catalog())
        .unwrap();
    let path = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(path.path(), serde_json::to_vec(&result.resolution).unwrap()).unwrap();
    drop(roster);
    let restored: ModelRosterResolution =
        serde_json::from_slice(&std::fs::read(path.path()).unwrap()).unwrap();
    assert_eq!(restored, result.resolution);
    assert_eq!(restored.provider_key(), "openai-oauth");
    assert_eq!(restored.route_api_method(), "openai-oauth");
}

#[test]
fn working_edits_and_missing_file_head_recovery_do_not_reresolve_existing_executions() {
    use crate::instruction::*;
    let root = tempfile::tempdir().unwrap();
    let repositories = InstructionRepositoryService::from_paths(
        root.path().join("home"),
        root.path().join("state"),
    );
    let initial = roster(&["openai-oauth:first"], Some("high"))
        .to_toml()
        .unwrap();
    let initialized = repositories
        .initialize_global(
            &InstructionStoreSeed {
                manifest: InstructionStoreManifest::current(),
                files: vec![InstructionSeedFile {
                    relative_path: ROSTER_PATH.into(),
                    content: initial.as_bytes().to_vec(),
                }],
            },
            &[],
        )
        .unwrap();
    let service = ModelRosterService::new(repositories.clone());
    let request = ModelRosterRequest::alias("work");
    let path = initialized.repository.root.join(ROSTER_PATH);
    let first = service
        .resolve(&request, &catalog(), InstructionReadPolicy::WorkingTreeOnly)
        .unwrap()
        .resolution;
    let next = roster(&["claude-oauth:second"], Some("low"))
        .to_toml()
        .unwrap();
    std::fs::write(&path, next).unwrap();
    let second = service
        .resolve(&request, &catalog(), InstructionReadPolicy::WorkingTreeOnly)
        .unwrap()
        .resolution;
    assert_eq!(first.selected_model(), "first");
    assert_eq!(second.selected_model(), "second");
    let stored = serde_json::to_vec(&first).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        service
            .load(InstructionReadPolicy::WorkingTreeOnly)
            .is_err()
    );
    assert_eq!(
        service
            .resolve(
                &request,
                &catalog(),
                InstructionReadPolicy::AllowHeadFallback
            )
            .unwrap()
            .resolution,
        first
    );
    assert_eq!(
        serde_json::from_slice::<ModelRosterResolution>(&stored).unwrap(),
        first
    );
    for broken in [b"bad = [".as_slice(), &[0xff], b""] {
        std::fs::write(&path, broken).unwrap();
        assert!(
            service
                .load(InstructionReadPolicy::AllowHeadFallback)
                .is_err()
        );
    }
    // Concrete-only overrides remain usable even with broken roster source.
    let explicit = ModelRosterRequest {
        model_override: Some(QualifiedModel::parse("openai-oauth:first").unwrap()),
        ..Default::default()
    };
    assert!(
        service
            .resolve(
                &explicit,
                &catalog(),
                InstructionReadPolicy::WorkingTreeOnly
            )
            .is_ok()
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), &path).unwrap();
        assert!(
            service
                .load(InstructionReadPolicy::AllowHeadFallback)
                .is_err()
        );
    }
}
