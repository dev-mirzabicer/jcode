use super::*;

impl Agent {
    pub fn set_premium_mode(&self, mode: crate::provider::copilot::PremiumMode) {
        self.provider.set_premium_mode(mode);
    }

    pub fn premium_mode(&self) -> crate::provider::copilot::PremiumMode {
        self.provider.premium_mode()
    }

    pub fn provider_fork(&self) -> Arc<dyn Provider> {
        self.provider.fork()
    }

    pub fn provider_handle(&self) -> Arc<dyn Provider> {
        Arc::clone(&self.provider)
    }

    pub fn available_models(&self) -> Vec<&'static str> {
        self.provider.available_models()
    }

    pub fn available_models_for_switching(&self) -> Vec<String> {
        self.provider.available_models_for_switching()
    }

    pub fn available_models_display(&self) -> Vec<String> {
        self.provider.available_models_display()
    }

    pub fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        self.provider.model_routes()
    }

    pub fn model_catalog_snapshot(&self) -> jcode_provider_core::ModelCatalogSnapshot {
        jcode_provider_core::ModelCatalogSnapshot::new(
            Some(self.provider_name()),
            Some(self.provider_model()),
            self.available_models_display(),
            self.model_routes(),
        )
    }

    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    pub fn provider_messages(&mut self) -> Result<Vec<Message>> {
        self.messages_for_provider()
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(
            model,
            crate::provider::ProviderModelSelectionSource::User,
        )
    }

    pub fn set_route_selection(
        &mut self,
        selection: &crate::provider::RouteSelection,
    ) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::User,
        )
    }

    pub(crate) fn set_route_selection_from_auth(
        &mut self,
        selection: &crate::provider::RouteSelection,
    ) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::Auth,
        )
    }

    fn set_route_selection_from_provider_state_event(
        &mut self,
        selection: &crate::provider::RouteSelection,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        let has_active_operations =
            !crate::context::projection_validation_operations(&self.session.context_view)
                .is_empty();
        let candidate_model = if has_active_operations {
            let candidate = self.provider.fork();
            candidate.set_route_selection(selection)?;
            self.validate_provider_switch_projection(candidate.as_ref())?;
            candidate.model()
        } else {
            selection.model.clone()
        };

        let mut previous_session = self.session.clone();
        let mut next_session = self.session.clone();
        next_session.provider_key =
            crate::provider::MultiProvider::default_model_selection_from_route(
                &selection.model,
                &selection.api_method,
                &selection.provider_label,
            )
            .provider_key
            .or_else(|| Some(selection.runtime_key.stable_id()));
        next_session.route_api_method = Some(selection.api_method.clone());
        next_session.model = Some(candidate_model);
        next_session.provider_session_id = None;
        next_session.save()?;
        if let Err(error) = self.provider.set_route_selection(selection) {
            if previous_session.save().is_err() {
                crate::logging::error(
                    "Provider route switch failed after staging and durable session rollback also failed",
                );
            }
            return Err(error);
        }
        self.session = next_session;
        let resolved_model = self.provider.model();
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.update_context_runtime_budget();
        self.after_provider_context_changed(
            "provider route switch",
            format!(
                "provider route changed to {} using {}",
                selection.provider_label, selection.api_method
            ),
            true,
        )?;
        self.log_env_snapshot("set_route_selection");
        Ok(())
    }

    pub(crate) fn set_model_from_auth(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(
            model,
            crate::provider::ProviderModelSelectionSource::Auth,
        )
    }

    fn set_model_from_provider_state_event(
        &mut self,
        model: &str,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        let candidate = self.provider.fork();
        crate::provider::set_model_with_auth_refresh(candidate.as_ref(), model)?;
        self.validate_provider_switch_projection(candidate.as_ref())?;

        let mut previous_session = self.session.clone();
        let mut next_session = self.session.clone();
        let candidate_model = candidate.model();
        if let Some(pin) = candidate.explicit_provider_pin_for_current_model() {
            next_session.provider_key = Some("openrouter".to_string());
            next_session.route_api_method = Some("openrouter".to_string());
            next_session.model = Some(format!("{candidate_model}@{pin}"));
        } else {
            next_session.provider_key =
                crate::provider::MultiProvider::session_provider_key_after_model_switch(
                    model,
                    candidate.name(),
                    self.session.provider_key.as_deref(),
                );
            next_session.model = Some(candidate_model);
        }
        next_session.provider_session_id = None;
        next_session.save()?;
        if let Err(error) =
            crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), model)
        {
            if previous_session.save().is_err() {
                crate::logging::error(
                    "Provider model switch failed after staging and durable session rollback also failed",
                );
            }
            return Err(error);
        }
        self.session = next_session;
        self.reconcile_explicit_provider_pin_route();
        let resolved_model = self.provider.model();
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.update_context_runtime_budget();
        self.after_provider_context_changed(
            "provider model switch",
            format!("provider model changed to {}", self.provider.model()),
            true,
        )?;
        self.log_env_snapshot("set_model");
        Ok(())
    }

    fn validate_provider_switch_projection(&mut self, candidate: &dyn Provider) -> Result<()> {
        let operations =
            crate::context::projection_validation_operations(&self.session.context_view);
        if operations.is_empty() {
            return Ok(());
        }
        let projected = self.session.projected_messages_for_provider()?;
        crate::context::provider_validation::require_supported_projected_messages(
            candidate,
            &projected,
            &operations,
        )?;
        Ok(())
    }

    pub(crate) fn provider_model_selection_generation(&self) -> u64 {
        self.provider_runtime_state.selection_generation()
    }

    pub(crate) fn user_selected_provider_model_after(&self, generation: u64) -> bool {
        self.provider_runtime_state.user_selected_after(generation)
    }

    pub fn restore_reasoning_effort_from_session(&mut self) {
        if let Some(effort) = self.session.reasoning_effort.clone() {
            if let Err(e) = self.provider.set_reasoning_effort(&effort) {
                crate::logging::error(&format!(
                    "Failed to restore session reasoning effort '{}': {}",
                    effort, e
                ));
            }
        } else {
            self.session.reasoning_effort = self.provider.reasoning_effort();
        }
        // Mirror the effort into the deadlock-free side-table so server handlers
        // (e.g. the swarm seed handler) can learn this session's effort without
        // taking the agent lock.
        crate::session_effort::record_session_effort(
            &self.session.id,
            self.session.reasoning_effort.as_deref(),
        );
    }

    pub fn set_reasoning_effort(&mut self, effort: &str) -> Result<Option<String>> {
        self.provider.set_reasoning_effort(effort)?;
        let current = self.provider.reasoning_effort();
        self.session.reasoning_effort = current.clone();
        // Keep the side-table in sync (see `restore_reasoning_effort_from_session`).
        crate::session_effort::record_session_effort(&self.session.id, current.as_deref());
        self.log_env_snapshot("set_reasoning_effort");
        self.session.save()?;
        Ok(current)
    }

    pub fn subagent_model(&self) -> Option<String> {
        self.session.subagent_model.clone()
    }

    pub fn set_subagent_model(&mut self, model: Option<String>) -> Result<()> {
        self.session.subagent_model = model;
        self.log_env_snapshot("set_subagent_model");
        self.session.save()?;
        Ok(())
    }

    pub fn session_provider_key(&self) -> Option<String> {
        self.session.provider_key.clone()
    }

    /// API method/runtime route used to select the active model (e.g.
    /// "openai-api", "claude-oauth", "openai-compatible:nvidia-nim"). Spawned
    /// swarm agents inherit this so they reconstruct the coordinator's exact
    /// auth route instead of falling back to the config default.
    pub fn session_route_api_method(&self) -> Option<String> {
        self.session.route_api_method.clone()
    }

    /// The credential the active provider will use for the next request, when
    /// the provider distinguishes OAuth (subscription) from API key (cost).
    /// Resolved authoritatively here so remote clients can render billing/usage
    /// without re-deriving it from the provider name.
    pub fn active_resolved_credential(&self) -> Option<jcode_provider_core::ResolvedCredential> {
        self.provider.active_resolved_credential()
    }

    pub fn set_session_provider_key(&mut self, provider_key: Option<String>) {
        self.session.provider_key = provider_key;
    }

    pub fn rename_session_title(&mut self, title: Option<String>) -> Result<String> {
        self.session.rename_title(title);
        self.log_env_snapshot("rename_session");
        self.session.save()?;
        Ok(self.session.display_title_or_name().to_string())
    }

    pub fn autoreview_enabled(&self) -> Option<bool> {
        self.session.autoreview_enabled
    }

    pub fn set_autoreview_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autoreview_enabled = Some(enabled);
        self.log_env_snapshot("set_autoreview_enabled");
        self.session.save()?;
        Ok(())
    }

    pub fn autojudge_enabled(&self) -> Option<bool> {
        self.session.autojudge_enabled
    }

    pub fn set_autojudge_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autojudge_enabled = Some(enabled);
        self.log_env_snapshot("set_autojudge_enabled");
        self.session.save()?;
        Ok(())
    }

    /// Set the working directory for this session
    pub fn set_working_dir(&mut self, dir: &str) {
        if self.session.working_dir.as_deref() == Some(dir) {
            return;
        }
        self.session.working_dir = Some(dir.to_string());
        self.session.refresh_initial_session_context_message();
        self.log_env_snapshot("working_dir");
    }

    /// Get the working directory for this session
    pub fn working_dir(&self) -> Option<&str> {
        self.session.working_dir.as_deref()
    }

    pub fn active_agent(&self) -> Option<&crate::session::StoredAgentReference> {
        self.session.active_agent()
    }

    pub fn first_provider_dispatch_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.session.first_provider_dispatch_at()
    }

    pub fn system_prompt_text(&self) -> Option<&str> {
        self.session.system_prompt_text()
    }

    pub fn set_instruction_repositories(
        &mut self,
        repositories: crate::instruction::InstructionRepositoryService,
    ) {
        self.instruction_repositories = repositories;
    }

    /// Get the stored messages (for transcript export)
    pub fn messages(&self) -> &[StoredMessage] {
        &self.session.messages
    }
}
