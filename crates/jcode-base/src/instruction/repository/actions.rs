//! Reviewed repository operations and durable completion receipts.
use super::git::{GitRepository, validate_branch, validate_operation_id};
use super::lease::{acquire_mutation_lease, acquire_operation_lease};
use super::mutation::sha256;
use super::service::require_clean;
use super::*;
use jcode_instruction_types::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRecord {
    schema: u32,
    session: String,
    project: Option<PathBuf>,
    repository: Option<InstructionRepositoryRef>,
    repository_digest: Option<String>,
    configuration_digest: Option<String>,
    push_commit: Option<String>,
    plan: InstructionRepositoryPlan,
    started: bool,
    receipt: Option<InstructionRepositoryReceipt>,
}

impl InstructionRepositoryService {
    fn operation_repository(
        &self,
        project: Option<&Path>,
        scope: InstructionEditScope,
    ) -> InstructionRepositoryResult<Option<InstructionRepositoryRef>> {
        match scope {
            InstructionEditScope::Global => self.global_repository().map(Some),
            InstructionEditScope::Project => project
                .map(|path| self.resolve_project_repository(path))
                .transpose()
                .map(Option::flatten),
        }
    }

    pub fn repository_operation_choices(
        &self,
        project: Option<&Path>,
        scope: InstructionEditScope,
    ) -> InstructionRepositoryResult<InstructionRepositoryChoices> {
        let repository = self.operation_repository(project, scope);
        let mut choices = InstructionRepositoryChoices {
            scope,
            project_is_git: project
                .and_then(|path| self.resolve_project_root(path).ok())
                .is_some_and(|path| GitRepository::new(path).is_repository()),
            git_available: false,
            can_initialize: false,
            can_recreate: false,
            root: None,
            branches: Vec::new(),
            remote_branches: Vec::new(),
            remotes: Vec::new(),
            configured_branch: None,
            current_branch: None,
            health: "No configured project instruction repository. Choose a setup mode.".into(),
            warnings: Vec::new(),
        };
        let repository = match repository {
            Ok(Some(repository)) => repository,
            Ok(None) => return Ok(choices),
            Err(error) if scope == InstructionEditScope::Global => return Err(error),
            Err(error) => {
                choices.health = error.to_string();
                return Ok(choices);
            }
        };
        choices.root = Some(repository.root.to_string_lossy().into_owned());
        choices.configured_branch = repository.configured_branch.clone();
        let state = self.inspect(&repository)?;
        choices.can_initialize = scope == InstructionEditScope::Global
            && matches!(state.health, InstructionRepositoryHealth::Uninitialized);
        choices.can_recreate = scope == InstructionEditScope::Global && !choices.can_initialize;
        choices.current_branch = state.branch.clone();
        choices.health = format!("{:?}", state.health);
        choices.warnings = state.configuration_warnings;
        if state.detached {
            choices
                .warnings
                .push("Detached HEAD: select a branch before Save.".into());
        }
        if !state.conflicts.is_empty() {
            choices.warnings.push("Git has unresolved conflicts. Source is preserved. Resolve the merge with Git before synchronization or committing.".into());
        }
        let git = GitRepository::new(&repository.root);
        if git.is_repository() {
            choices.git_available = git.head()?.is_some();
            choices.branches = git.branch_names(false)?;
            choices.remote_branches = git.branch_names(true)?;
            choices.remotes = git
                .remotes()?
                .into_iter()
                .map(|(name, url)| InstructionRemoteChoice {
                    name,
                    url: display_url(&url),
                })
                .collect();
        }
        Ok(choices)
    }

    pub fn plan_repository_operation(
        &self,
        session: &str,
        project: Option<&Path>,
        scope: InstructionEditScope,
        mut action: InstructionRepositoryAction,
    ) -> InstructionRepositoryResult<InstructionRepositoryPlan> {
        let setup = is_setup(&action);
        let project = project
            .map(|path| self.resolve_project_root(path))
            .transpose()?;
        let mut repository = match self.operation_repository(project.as_deref(), scope) {
            Ok(value) => value,
            Err(_) if setup => None,
            Err(error) => return Err(error),
        };
        if let Some(url) = repository
            .as_ref()
            .and_then(|repository| repository.configured_remote.as_deref())
            && validate_url(url).is_err()
        {
            if setup {
                repository = None;
            } else {
                return Err(operation_error(
                    "Configured repository URL contains credentials. Replace it with a credential-free URL through explicit setup before proceeding.",
                ));
            }
        }
        if matches!(
            action,
            InstructionRepositoryAction::InitializeGlobal
                | InstructionRepositoryAction::RecreateGlobal
        ) && scope != InstructionEditScope::Global
        {
            return Err(operation_error(
                "Global recovery cannot target project scope",
            ));
        }
        if setup && scope != InstructionEditScope::Project {
            return Err(operation_error("Project setup requires project scope"));
        }
        if (setup || matches!(action, InstructionRepositoryAction::RepairCheckout))
            && project.is_none()
        {
            return Err(operation_error("This session has no project directory"));
        }
        validate_action(&action)?;
        let mut outgoing_commits = Vec::new();
        let mut push_commit = None;
        if !setup
            && !matches!(
                action,
                InstructionRepositoryAction::InitializeGlobal
                    | InstructionRepositoryAction::RecreateGlobal
                    | InstructionRepositoryAction::RepairCheckout
            )
        {
            let repository = repository
                .as_ref()
                .ok_or_else(|| operation_error("Configure an instruction repository first"))?;
            let git = GitRepository::new(&repository.root);
            if let InstructionRepositoryAction::Checkout {
                create: true,
                start,
                ..
            } = &mut action
            {
                *start = Some(git.resolve_reference(start.as_deref().unwrap_or("HEAD"))?);
            }
            match &action {
                InstructionRepositoryAction::Fetch { remote }
                | InstructionRepositoryAction::FetchCheckout { remote, .. }
                | InstructionRepositoryAction::Pull { remote, .. }
                | InstructionRepositoryAction::Push { remote, .. } => {
                    let url = git
                        .remote_url(remote)?
                        .ok_or_else(|| operation_error("The selected remote is not configured"))?;
                    validate_url(&url)?;
                }
                _ => {}
            }
            if let InstructionRepositoryAction::Push { remote, branch } = &action {
                outgoing_commits = git.outgoing(remote, branch)?;
                push_commit = Some(git.resolve_reference(&format!("refs/heads/{branch}"))?);
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let title = action_title(&action).to_string();
        let root = repository
            .as_ref()
            .map(|repository| repository.root.display().to_string())
            .unwrap_or_else(|| "New project instruction repository".into());
        let detail = format!(
            "{title}\n\nProject: {}\nRepository: {root}\nScope: {scope:?}\n\n{}\n\nNo parent-project commit, history rewrite, automatic push, or current-session instruction activation occurs. Network operations use Git's existing credential tooling.\n{}",
            project
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".into()),
            action_detail(&action),
            if matches!(action, InstructionRepositoryAction::Push { .. }) {
                "Outgoing commits are based on locally known tracking refs. This pushes the exact reviewed local commit without force."
            } else {
                ""
            }
        );
        let plan = InstructionRepositoryPlan {
            id,
            scope,
            network: is_network(&action),
            action,
            title,
            detail,
            outgoing_commits,
        };
        let record = OperationRecord {
            schema: 1,
            session: session.into(),
            project: project.clone(),
            repository_digest: repository
                .as_ref()
                .map(|repository| self.repository_operation_digest(repository))
                .transpose()?,
            configuration_digest: project
                .as_deref()
                .map(|path| self.project_operation_configuration(path))
                .transpose()?,
            repository,
            push_commit,
            plan: plan.clone(),
            started: false,
            receipt: None,
        };
        self.write_operation(&record)?;
        Ok(plan)
    }

    pub fn retained_repository_operations(
        &self,
        session: &str,
        project: Option<&Path>,
    ) -> InstructionRepositoryResult<(Vec<InstructionRetainedOperation>, Vec<String>)> {
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
            session: String,
            project: Option<PathBuf>,
            plan: PlanHeader,
            started: bool,
            receipt: Option<InstructionRepositoryReceipt>,
        }
        #[derive(Deserialize)]
        struct PlanHeader {
            id: String,
            title: String,
        }
        let directory = self
            .operation_path(session, &uuid::Uuid::nil().to_string())?
            .parent()
            .ok_or_else(|| operation_error("No operation directory"))?
            .to_path_buf();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(error) => return Err(operation_error(&error.to_string())),
        };
        let project = project
            .map(|path| self.resolve_project_root(path))
            .transpose()?;
        let mut operations = Vec::new();
        let mut errors = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            };
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let read = || -> InstructionRepositoryResult<Header> {
                let metadata = std::fs::symlink_metadata(entry.path())
                    .map_err(|error| operation_error(&error.to_string()))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(operation_error(
                        "Operation recovery record is not a regular file",
                    ));
                }
                let file = std::fs::File::open(entry.path())
                    .map_err(|error| operation_error(&error.to_string()))?;
                serde_json::from_reader(std::io::BufReader::new(file)).map_err(serialization_error)
            };
            match read() {
                Ok(header)
                    if header.schema == 1
                        && header.session == session
                        && header.project == project
                        && uuid::Uuid::parse_str(&header.plan.id).is_ok() =>
                {
                    operations.push(InstructionRetainedOperation {
                        id: header.plan.id,
                        title: header.plan.title,
                        started: header.started,
                        completed: header.receipt.is_some_and(|receipt| receipt.completed),
                    })
                }
                Ok(_) => errors.push(format!(
                    "{} has unsupported or inconsistent operation metadata; it was retained",
                    entry.path().display()
                )),
                Err(error) => errors.push(format!("{}: {error}", entry.path().display())),
            }
        }
        operations.sort_by(|left, right| left.title.cmp(&right.title).then(left.id.cmp(&right.id)));
        Ok((operations, errors))
    }

    pub fn repository_operation_receipt(
        &self,
        session: &str,
        project: Option<&Path>,
        id: &str,
    ) -> InstructionRepositoryResult<InstructionRepositoryReceipt> {
        let record = self.read_operation(session, project, id)?;
        if let Some(receipt) = record.receipt {
            return Ok(receipt);
        }
        let repository = record
            .repository
            .as_ref()
            .cloned()
            .unwrap_or(self.global_repository()?);
        let running =
            super::lease::active_operation_lease(&self.roots()?.durable_state, &repository, id)
                .is_some();
        Ok(InstructionRepositoryReceipt {
            id: record.plan.id,
            title: record.plan.title,
            detail: if running {
                "This repository operation is still running. No terminal receipt is available yet. Wait and read its receipt again; do not replay it.".into()
            } else if record.started {
                "The operation was interrupted before a terminal receipt was recorded. Inspect repository and remote state before preparing another explicit action. No operation was replayed.".into()
            } else {
                "The operation was reviewed but never applied. No source action was started.".into()
            },
            completed: false,
            running,
            outcome_uncertain: record.started && !running,
            source_unchanged: !record.started,
        })
    }

    pub fn apply_repository_operation(
        &self,
        session: &str,
        project: Option<&Path>,
        id: &str,
    ) -> InstructionRepositoryResult<InstructionRepositoryReceipt> {
        let mut record = self.read_operation(session, project, id)?;
        let lock_repository = record
            .repository
            .clone()
            .unwrap_or(self.global_repository()?);
        let _operation =
            acquire_operation_lease(&self.roots()?.durable_state, &lock_repository, id)?;
        record = self.read_operation(session, project, id)?;
        if record.receipt.is_some() || record.started {
            drop(_operation);
            return self.repository_operation_receipt(session, project, id);
        }
        if !is_setup(&record.plan.action) {
            let current =
                self.operation_repository(record.project.as_deref(), record.plan.scope)?;
            if current != record.repository {
                return Err(operation_error(
                    "Configured repository changed since review. Prepare the operation again.",
                ));
            }
        }
        // Persist intent before any potentially irreversible operation. A lost
        // response reads the durable receipt, never replays guessed new intent.
        record.started = true;
        self.write_operation(&record)?;
        let result = self.execute_repository_operation(&record);
        let receipt = match result {
            Ok(detail) => InstructionRepositoryReceipt {
                id: id.into(),
                title: record.plan.title.clone(),
                detail,
                completed: true,
                running: false,
                outcome_uncertain: false,
                source_unchanged: false,
            },
            Err(error) => InstructionRepositoryReceipt {
                id: id.into(),
                title: record.plan.title.clone(),
                detail: format!(
                    "{error}\nLocal work and this receipt are preserved. Correct the reported condition and prepare a new reviewed action; a failed action is not silently retried."
                ),
                completed: false,
                running: false,
                outcome_uncertain: !error.existing_state_unchanged,
                source_unchanged: error.existing_state_unchanged,
            },
        };
        record.receipt = Some(receipt.clone());
        self.write_operation(&record).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "record repository outcome",
                format!(
                    "Operation outcome: {}. Receipt persistence failed: {error}",
                    receipt.detail
                ),
            )
            .may_have_working_changes()
        })?;
        Ok(receipt)
    }

    fn execute_repository_operation(
        &self,
        record: &OperationRecord,
    ) -> InstructionRepositoryResult<String> {
        let id = record.plan.id.as_str();
        let project = || {
            record
                .project
                .as_deref()
                .ok_or_else(|| operation_error("Project identity is missing"))
        };
        let expected = record.configuration_digest.as_deref();
        match &record.plan.action {
            InstructionRepositoryAction::Submodule { path, url, branch } => {
                let repository = self.configure_submodule_checked(
                    project()?,
                    id,
                    url,
                    branch,
                    Some(path.into()),
                    expected,
                )?;
                return self.setup_receipt(&repository);
            }
            InstructionRepositoryAction::CloneExternal { url, branch } => {
                let repository =
                    self.configure_external_remote_checked(project()?, id, url, branch, expected)?;
                return self.setup_receipt(&repository);
            }
            InstructionRepositoryAction::AttachExternal { path, branch } => {
                let repository = self.configure_external_local_checked(
                    project()?,
                    id,
                    path,
                    branch.clone(),
                    expected,
                )?;
                return self.setup_receipt(&repository);
            }
            InstructionRepositoryAction::Standalone { path } => {
                let initialized = self.configure_non_git_project_checked(
                    project()?,
                    id,
                    Some(path.into()),
                    &InstructionStoreSeed::empty(),
                    &[],
                    expected,
                )?;
                return self.setup_receipt(&initialized.repository);
            }
            InstructionRepositoryAction::InitializeGlobal => {
                let repository = self.global_repository()?;
                if !matches!(
                    self.inspect(&repository)?.health,
                    InstructionRepositoryHealth::Uninitialized
                ) {
                    return Err(operation_error(
                        "The global store already exists or is damaged. Use inspection and explicit recovery rather than initialization.",
                    ));
                }
                let initialized =
                    crate::instruction::SystemPromptComposer::from_repository_service(self.clone())
                        .ensure_global_store()
                        .map_err(|error| operation_error(&error.to_string()))?;
                return Ok(format!(
                    "Global store initialized at {} with baseline {}. Imported {} legacy source(s). Nothing was pushed.",
                    initialized.repository.root.display(),
                    initialized.commit,
                    initialized.imported.len()
                ));
            }
            InstructionRepositoryAction::RecreateGlobal => {
                let repository = record
                    .repository
                    .as_ref()
                    .ok_or_else(|| operation_error("Global repository identity is missing"))?;
                let seed = crate::instruction::shipped_instruction_seed()
                    .map_err(|error| operation_error(&error.to_string()))?;
                let recreated = self.recreate_from_seed_checked(
                    repository,
                    &seed,
                    &[],
                    "main",
                    Some((
                        id,
                        record
                            .repository_digest
                            .as_deref()
                            .ok_or_else(|| operation_error("Review signature is missing"))?,
                    )),
                )?;
                return Ok(format!(
                    "Recreated from the shipped seed at {}. Existing store preserved at {}. Current sessions keep their exact stored instructions.",
                    recreated.initialization.commit,
                    recreated
                        .damaged_backup
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "no previous checkout existed".into())
                ));
            }
            InstructionRepositoryAction::RepairCheckout => {
                let repository = record
                    .repository
                    .as_ref()
                    .ok_or_else(|| operation_error("No configured checkout to repair"))?;
                return self
                    .repair_instruction_checkout(repository, id, expected)
                    .and_then(|repository| self.setup_receipt(&repository));
            }
            _ => {}
        }
        let repository = record
            .repository
            .as_ref()
            .ok_or_else(|| operation_error("No configured instruction repository"))?;
        let _lease = acquire_mutation_lease(&self.roots()?.durable_state, repository, id)?;
        if Some(self.repository_operation_digest(repository)?) != record.repository_digest {
            return Err(operation_error(
                "Repository files, index, branch or remote references changed since review. Nothing was applied; prepare the action again.",
            ));
        }
        let git = GitRepository::new(&repository.root);
        let before = git.head()?;
        let operation = (|| {
            match &record.plan.action {
                InstructionRepositoryAction::ConfigureRemote { name, url } => {
                    git.set_remote(name, url)?
                }
                InstructionRepositoryAction::Checkout {
                    branch,
                    create,
                    start,
                } => {
                    self.require_no_attached_drafts(repository)?;
                    let same_commit = *create && start.as_deref() == before.as_deref();
                    if !same_commit {
                        require_clean(repository, &git, "change branch")?;
                    }
                    git.branch_checkout(branch, *create, start.as_deref())?;
                }
                InstructionRepositoryAction::Fetch { remote } => git.fetch(remote)?,
                InstructionRepositoryAction::Pull { remote, branch } => {
                    self.require_no_attached_drafts(repository)?;
                    require_clean(repository, &git, "fast-forward pull")?;
                    git.pull(remote, branch, InstructionPullStrategy::FastForwardOnly)?;
                }
                InstructionRepositoryAction::Push { remote, branch } => git.push_snapshot(
                    remote,
                    branch,
                    record
                        .push_commit
                        .as_deref()
                        .ok_or_else(|| operation_error("Reviewed push commit is missing"))?,
                )?,
                InstructionRepositoryAction::FetchCheckout {
                    remote,
                    branch,
                    local_branch,
                } => {
                    self.require_no_attached_drafts(repository)?;
                    require_clean(repository, &git, "fetch and check out remote branch")?;
                    git.fetch_branch(remote, branch)?;
                    let commit =
                        git.resolve_reference(&format!("refs/remotes/{remote}/{branch}"))?;
                    git.branch_checkout(local_branch, true, Some(&commit))?;
                }
                _ => return Err(operation_error("Unsupported repository action")),
            }
            Ok(())
        })();
        operation.map_err(InstructionRepositoryError::may_have_working_changes)?;
        let state = self.inspect(repository)?;
        let mut detail = format!(
            "{} completed.\nRepository: {}\nBranch: {}\nHEAD before: {}\nHEAD after: {}\nNo parent commit or history rewrite occurred.",
            record.plan.title,
            repository.root.display(),
            state.branch.as_deref().unwrap_or("detached"),
            before.as_deref().unwrap_or("none"),
            state.head.as_deref().unwrap_or("none")
        );
        if let Some(parent) = state.parent_gitlink {
            detail.push_str(&format!(
                "\nParent gitlink changed: {}. The project owner decides when to commit it.",
                parent.gitlink_changed
            ));
        }
        if let Ok(validation) = self.validate_repository(repository)
            && !validation.is_valid()
        {
            detail.push_str("\nThe synchronized working source has validation issues. Inspect and repair the affected resources. No seed fallback or current-session replacement was performed.");
        }
        Ok(detail)
    }

    fn setup_receipt(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<String> {
        let state = self.inspect(repository)?;
        Ok(format!(
            "Repository configured: {}\nBranch: {}\nHEAD: {}\nParent changes were left uncommitted. Existing session instructions remain frozen.{}",
            repository.root.display(),
            state
                .branch
                .as_deref()
                .unwrap_or("detached: choose a branch before Save"),
            state.head.as_deref().unwrap_or("none"),
            if state.parent_gitlink.is_some() {
                "\nReview .gitmodules and the changed gitlink in the parent project."
            } else {
                ""
            }
        ))
    }

    pub(super) fn repository_operation_digest(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<String> {
        let mut data = Vec::new();
        capture_path(&repository.root, &repository.root, &mut data)?;
        let git = GitRepository::new(&repository.root);
        if git.is_repository() {
            data.push(format!(
                "HEAD:{:?};branch:{:?};refs:{};index:{:?};remotes:{}",
                git.head()?,
                git.branch()?,
                git.references()?,
                git.index_digest()?,
                sha256(&serde_json::to_vec(&git.remotes()?).map_err(serialization_error)?)
            ));
        }
        Ok(sha256(
            &serde_json::to_vec(&data).map_err(serialization_error)?,
        ))
    }

    fn project_operation_configuration(
        &self,
        project: &Path,
    ) -> InstructionRepositoryResult<String> {
        let mut values = Vec::new();
        for relative in [".jcode/instructions.toml", ".gitmodules"] {
            capture_path(project, &project.join(relative), &mut values)?;
        }
        let git = GitRepository::new(project);
        if git.is_repository() {
            values.push(format!(
                "{:?};{:?};{:?}",
                git.head()?,
                git.branch()?,
                git.index_digest()?
            ));
        }
        Ok(sha256(
            &serde_json::to_vec(&values).map_err(serialization_error)?,
        ))
    }
    pub(super) fn check_project_operation_configuration(
        &self,
        project: &Path,
        expected: Option<&str>,
    ) -> InstructionRepositoryResult<()> {
        if let Some(expected) = expected
            && self.project_operation_configuration(project)? != expected
        {
            return Err(operation_error(
                "Project configuration, branch or index changed since review. Nothing was published; review setup again.",
            ));
        }
        Ok(())
    }
    fn operation_path(&self, session: &str, id: &str) -> InstructionRepositoryResult<PathBuf> {
        uuid::Uuid::parse_str(id)
            .map_err(|_| operation_error("Invalid repository operation identity"))?;
        Ok(self
            .roots()?
            .durable_state
            .join("instruction-repositories/operations")
            .join(sha256(session.as_bytes()))
            .join(format!("{id}.json")))
    }
    fn write_operation(&self, record: &OperationRecord) -> InstructionRepositoryResult<()> {
        let path = self.operation_path(&record.session, &record.plan.id)?;
        crate::storage::ensure_dir(
            path.parent()
                .ok_or_else(|| operation_error("No operation directory"))?,
        )
        .map_err(|error| operation_error(&error.to_string()))?;
        crate::storage::write_json_secret(&path, record)
            .map_err(|error| operation_error(&error.to_string()))
    }
    fn read_operation(
        &self,
        session: &str,
        project: Option<&Path>,
        id: &str,
    ) -> InstructionRepositoryResult<OperationRecord> {
        let path = self.operation_path(session, id)?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| operation_error(&error.to_string()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(operation_error(
                "Repository operation record is not a regular file",
            ));
        }
        let record: OperationRecord = serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| operation_error(&error.to_string()))?,
        )
        .map_err(serialization_error)?;
        let project = project
            .map(|path| self.resolve_project_root(path))
            .transpose()?;
        if record.schema != 1
            || record.session != session
            || record.plan.id != id
            || record.project != project
        {
            return Err(operation_error(
                "Repository operation belongs to another session or project, or has an unsupported schema",
            ));
        }
        if let Some(receipt) = &record.receipt
            && (receipt.id != record.plan.id
                || !record.started
                || receipt.running
                || (receipt.completed && receipt.outcome_uncertain))
        {
            return Err(operation_error(
                "Stored repository receipt has inconsistent operation identity or lifecycle",
            ));
        }
        validate_action(&record.plan.action)?;
        if is_setup(&record.plan.action)
            && (record.project.is_none() || record.plan.scope != InstructionEditScope::Project)
        {
            return Err(operation_error("Invalid project setup record"));
        }
        if matches!(
            record.plan.action,
            InstructionRepositoryAction::InitializeGlobal
                | InstructionRepositoryAction::RecreateGlobal
        ) && record.plan.scope != InstructionEditScope::Global
        {
            return Err(operation_error("Invalid global recovery record"));
        }
        Ok(record)
    }
}

fn capture_path(
    root: &Path,
    path: &Path,
    values: &mut Vec<String>,
) -> InstructionRepositoryResult<()> {
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let name = hex::encode(relative.as_os_str().as_encoded_bytes());
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                values.push(format!("{name}:missing"));
                continue;
            }
            Err(error) => {
                return Err(operation_error(&format!(
                    "Cannot inspect {}: {error}",
                    path.display()
                )));
            }
        };
        if metadata.file_type().is_symlink() {
            let target =
                std::fs::read_link(&path).map_err(|error| operation_error(&error.to_string()))?;
            values.push(format!(
                "{name}:symlink:{}",
                hex::encode(target.as_os_str().as_encoded_bytes())
            ));
        } else if metadata.is_dir() {
            values.push(format!("{name}:directory"));
            let mut entries = std::fs::read_dir(&path)
                .map_err(|error| operation_error(&error.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| operation_error(&error.to_string()))?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries.into_iter().rev() {
                if entry.file_name() != ".git" {
                    pending.push(entry.path());
                }
            }
        } else if metadata.is_file() {
            values.push(format!(
                "{name}:{}",
                sha256(&std::fs::read(&path).map_err(|error| operation_error(&error.to_string()))?)
            ));
        } else {
            values.push(format!("{name}:nonregular"));
        }
    }
    Ok(())
}

fn validate_url(value: &str) -> InstructionRepositoryResult<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) || value.starts_with('-') {
        return Err(operation_error("Repository URL is empty or invalid"));
    }
    if let Ok(url) = url::Url::parse(value)
        && (url.password().is_some()
            || (matches!(url.scheme(), "http" | "https")
                && (!url.username().is_empty()
                    || url.query().is_some()
                    || url.fragment().is_some())))
    {
        return Err(operation_error(
            "Use a credential-free repository URL and Git's credential tooling. Credentials and tokens cannot enter operation records.",
        ));
    }
    Ok(())
}
fn display_url(value: &str) -> String {
    if validate_url(value).is_ok() {
        value.into()
    } else {
        "Credential-bearing or invalid URL hidden; configure a credential-free URL.".into()
    }
}
fn validate_action(action: &InstructionRepositoryAction) -> InstructionRepositoryResult<()> {
    match action {
        InstructionRepositoryAction::Submodule { path, url, branch } => {
            super::mutation::validate_relative_path(Path::new(path))?;
            validate_url(url)?;
            validate_branch(branch)?;
        }
        InstructionRepositoryAction::CloneExternal { url, branch } => {
            validate_url(url)?;
            validate_branch(branch)?;
        }
        InstructionRepositoryAction::ConfigureRemote { name, url } => {
            validate_operation_id(name)?;
            validate_url(url)?;
        }
        InstructionRepositoryAction::Standalone { path } => {
            super::mutation::validate_relative_path(Path::new(path))?
        }
        InstructionRepositoryAction::AttachExternal { path, branch } => {
            if !Path::new(path).is_absolute() {
                return Err(operation_error(
                    "Choose an absolute server-side checkout path",
                ));
            }
            if let Some(branch) = branch {
                validate_branch(branch)?;
            }
        }
        InstructionRepositoryAction::Checkout { branch, .. } => validate_branch(branch)?,
        InstructionRepositoryAction::Fetch { remote } => validate_operation_id(remote)?,
        InstructionRepositoryAction::Pull { remote, branch }
        | InstructionRepositoryAction::Push { remote, branch } => {
            validate_operation_id(remote)?;
            validate_branch(branch)?;
        }
        InstructionRepositoryAction::FetchCheckout {
            remote,
            branch,
            local_branch,
        } => {
            validate_operation_id(remote)?;
            validate_branch(branch)?;
            validate_branch(local_branch)?;
        }
        _ => {}
    }
    Ok(())
}
fn is_setup(action: &InstructionRepositoryAction) -> bool {
    matches!(
        action,
        InstructionRepositoryAction::Submodule { .. }
            | InstructionRepositoryAction::CloneExternal { .. }
            | InstructionRepositoryAction::AttachExternal { .. }
            | InstructionRepositoryAction::Standalone { .. }
    )
}
fn is_network(action: &InstructionRepositoryAction) -> bool {
    matches!(
        action,
        InstructionRepositoryAction::Submodule { .. }
            | InstructionRepositoryAction::CloneExternal { .. }
            | InstructionRepositoryAction::RepairCheckout
            | InstructionRepositoryAction::Fetch { .. }
            | InstructionRepositoryAction::Pull { .. }
            | InstructionRepositoryAction::Push { .. }
            | InstructionRepositoryAction::FetchCheckout { .. }
    )
}
fn action_title(action: &InstructionRepositoryAction) -> &'static str {
    match action {
        InstructionRepositoryAction::InitializeGlobal => "Initialize global instructions",
        InstructionRepositoryAction::RecreateGlobal => "Recreate global instructions from seed",
        InstructionRepositoryAction::Submodule { .. } => "Set up project submodule",
        InstructionRepositoryAction::CloneExternal { .. } => "Clone external project instructions",
        InstructionRepositoryAction::AttachExternal { .. } => {
            "Attach existing instruction checkout"
        }
        InstructionRepositoryAction::Standalone { .. } => "Set up non-Git project instructions",
        InstructionRepositoryAction::RepairCheckout => "Repair missing project checkout",
        InstructionRepositoryAction::ConfigureRemote { .. } => "Configure instruction remote",
        InstructionRepositoryAction::Checkout { .. } => "Select working branch",
        InstructionRepositoryAction::Fetch { .. } => "Fetch instruction remote",
        InstructionRepositoryAction::Pull { .. } => "Pull fast-forward only",
        InstructionRepositoryAction::Push { .. } => "Push reviewed instruction commits",
        InstructionRepositoryAction::FetchCheckout { .. } => {
            "Fetch and create a remote-tracking working branch"
        }
    }
}
fn action_detail(action: &InstructionRepositoryAction) -> String {
    match action {
    InstructionRepositoryAction::Submodule { path, url, branch } => format!("Add true submodule at {path}\nURL: {url}\nWorking branch: {branch}\nThe parent .gitmodules and gitlink will change, but remain uncommitted."),
    InstructionRepositoryAction::CloneExternal { url, branch } => format!("Clone {url} at branch {branch} into private Jcode state and publish project configuration."),
    InstructionRepositoryAction::AttachExternal { path, branch } => format!("Attach server-side checkout {path}\nExpected branch: {}\nNo source copy or parent commit.", branch.as_deref().unwrap_or("current")),
    InstructionRepositoryAction::Standalone { path } => format!("Create a standalone instruction Git repository at {path}, without making the whole project a Git repository."),
    InstructionRepositoryAction::ConfigureRemote { name, url } => format!("Set remote {name} to {url}. This does not fetch, pull or push."),
    InstructionRepositoryAction::Checkout { branch, create, start } => format!("{} branch {branch}\nStart: {}\nAn attached draft prevents branch changes.", if *create { "Create and select" } else { "Select existing" }, start.as_deref().unwrap_or("existing local branch")),
    InstructionRepositoryAction::Fetch { remote } => format!("Fetch {remote}. Update remote refs only; do not change the working branch."),
    InstructionRepositoryAction::Pull { remote, branch } => format!("Fetch and fast-forward from {remote}/{branch}. Requires safe clean working state. Divergence is rejected, never rebased or silently merged."),
    InstructionRepositoryAction::Push { remote, branch } => format!("Push the reviewed snapshot to {remote}/refs/heads/{branch}. No force, tag follow-up or submodule push."),
    InstructionRepositoryAction::FetchCheckout { remote, branch, local_branch } => format!("Fetch {remote}/{branch}, then create local branch {local_branch} from that fetched commit."),
    InstructionRepositoryAction::RecreateGlobal => "Consequential recovery: preserve the existing repository in an operation-specific backup, then create a new store from the shipped seed. Existing history is kept in that backup, not erased.".into(),
    InstructionRepositoryAction::InitializeGlobal => "Initialize only a genuinely new store, with exact eligible legacy imports and a baseline commit. An existing damaged store is never silently recreated.".into(),
    InstructionRepositoryAction::RepairCheckout => "Restore the configured missing checkout. This may contact its configured remote. The project repository is never committed automatically.".into(),
}
}
fn operation_error(detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Configuration,
        "instruction repository operation",
        detail,
    )
}
fn serialization_error(error: serde_json::Error) -> InstructionRepositoryError {
    operation_error(&format!("Repository operation record is invalid: {error}"))
}
