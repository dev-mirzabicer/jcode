use super::*;
use crate::instruction::{InstructionReadPolicy, InstructionRepositoryService};

#[derive(Clone, Default)]
pub struct ModelRosterService {
    repositories: InstructionRepositoryService,
}

impl ModelRosterService {
    pub fn new(repositories: InstructionRepositoryService) -> Self {
        Self { repositories }
    }

    /// Reads only the global store, never project configuration. Initialization
    /// and seed adoption remain the existing instruction store owner's work.
    pub fn load(&self, policy: InstructionReadPolicy) -> Result<ModelRoster, ModelRosterError> {
        let (repository, _) = crate::instruction::SystemPromptComposer::from_repository_service(
            self.repositories.clone(),
        )
        .prepare_global_store_for_read()
        .map_err(|error| ModelRosterError::Source {
            detail: error.to_string(),
        })?;
        let file = self
            .repositories
            .read_file(&repository, ROSTER_PATH, policy)
            .map_err(|error| ModelRosterError::Source {
                detail: error.to_string(),
            })?;
        ModelRoster::parse(&file.content)
    }

    pub fn resolve(
        &self,
        request: &ModelRosterRequest,
        catalog: &RosterCatalog,
        policy: InstructionReadPolicy,
    ) -> Result<PreparedRosterExecution, ModelRosterError> {
        // Concrete-only requests need no roster source. Named requests retain
        // alias effort precedence even when their model is explicitly replaced.
        if request.alias.is_none() {
            return ModelRoster {
                entries: BTreeMap::new(),
                diagnostics: Vec::new(),
            }
            .resolve(request, catalog);
        }
        self.load(policy)?.resolve(request, catalog)
    }

    pub fn list(
        &self,
        policy: InstructionReadPolicy,
    ) -> Result<Vec<ModelAliasDescription>, ModelRosterError> {
        Ok(self.load(policy)?.list())
    }

    pub fn inspect(
        &self,
        alias: &str,
        policy: InstructionReadPolicy,
    ) -> Result<ModelRosterEntry, ModelRosterError> {
        self.load(policy)?.inspect(alias).cloned()
    }

    pub fn validate(
        &self,
        policy: InstructionReadPolicy,
    ) -> Result<Vec<RosterDiagnostic>, ModelRosterError> {
        Ok(self.load(policy)?.validate().to_vec())
    }

    pub fn availability(
        &self,
        alias: &str,
        catalog: &RosterCatalog,
        policy: InstructionReadPolicy,
    ) -> Result<Vec<CandidateAvailability>, ModelRosterError> {
        self.load(policy)?.availability(alias, catalog)
    }

    pub fn preview(
        &self,
        request: &ModelRosterRequest,
        catalog: &RosterCatalog,
        policy: InstructionReadPolicy,
    ) -> Result<ModelRosterPreview, ModelRosterError> {
        let prepared = self.resolve(request, catalog, policy)?;
        Ok(ModelRosterPreview {
            resolution: prepared.resolution,
            rejected_candidates: prepared.rejected_candidates,
        })
    }
}
