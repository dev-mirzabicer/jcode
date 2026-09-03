use super::git::{GitRepository, validate_operation_id};
use super::lease::acquire_mutation_lease;
use super::mutation::{atomic_write_path, validate_relative_path};
use super::service::InstructionRepositoryService;
use super::types::*;
use crate::startup_context::{ProjectKey, StartupContext};
use std::path::{Path, PathBuf};

const PROJECT_CONFIG_RELATIVE_PATH: &str = ".jcode/instructions.toml";
const CONVENTIONAL_SUBMODULE_PATH: &str = ".jcode/instructions";

impl InstructionRepositoryService {
    pub fn resolve_project_root(
        &self,
        launch_dir: impl AsRef<Path>,
    ) -> InstructionRepositoryResult<PathBuf> {
        let roots = self.roots()?;
        StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map(|project| project.active_root().to_path_buf())
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve instruction project root",
                    error.to_string(),
                )
            })
    }

    pub fn resolve_project_repository(
        &self,
        launch_dir: impl AsRef<Path>,
    ) -> InstructionRepositoryResult<Option<InstructionRepositoryRef>> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve instruction project",
                    error.to_string(),
                )
            })?;
        let project_root = project.active_root().to_path_buf();
        let config_path = project_root.join(PROJECT_CONFIG_RELATIVE_PATH);
        let project_id = format!("project-{}", project.key().digest());
        if project_config_present(&config_path)? {
            let config = load_project_config(&config_path)?;
            return configured_repository(
                roots,
                &project_root,
                project.key(),
                project_id,
                config_path,
                config,
            )
            .map(Some);
        }

        if project.key().is_git() {
            let relative = Path::new(CONVENTIONAL_SUBMODULE_PATH);
            if GitRepository::submodule_recorded_commit(&project_root, relative)?.is_some() {
                return Ok(Some(InstructionRepositoryRef {
                    id: project_id,
                    kind: InstructionRepositoryKind::ProjectSubmodule,
                    root: project_root.join(relative),
                    project_root: Some(project_root),
                    project_config_path: None,
                    configured_branch: None,
                    configured_remote: None,
                    owner_only: false,
                }));
            }
        }
        Ok(None)
    }

    pub fn load_project_configuration(
        &self,
        launch_dir: impl AsRef<Path>,
    ) -> InstructionRepositoryResult<Option<InstructionProjectConfig>> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve instruction project configuration",
                    error.to_string(),
                )
            })?;
        let path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        if !project_config_present(&path)? {
            return Ok(None);
        }
        load_project_config(&path).map(Some)
    }

    pub fn configure_submodule(
        &self,
        launch_dir: impl AsRef<Path>,
        operation_id: &str,
        url: &str,
        branch: &str,
        path: Option<PathBuf>,
    ) -> InstructionRepositoryResult<InstructionRepositoryRef> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve submodule project",
                    error.to_string(),
                )
            })?;
        if !project.key().is_git() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "configure instruction submodule",
                "a true instruction submodule requires a Git parent project",
            ));
        }
        let relative = path.unwrap_or_else(|| PathBuf::from(CONVENTIONAL_SUBMODULE_PATH));
        validate_relative_path(&relative)?;
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        let repository = InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectSubmodule,
            root: project.active_root().join(&relative),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path.clone()),
            configured_branch: Some(branch.to_string()),
            configured_remote: Some(url.to_string()),
            owner_only: false,
        };
        validate_operation_id(operation_id)?;
        let _lease = acquire_mutation_lease(&roots.durable_state, &repository, operation_id)?;
        if GitRepository::submodule_recorded_commit(project.active_root(), &relative)?.is_none() {
            GitRepository::add_submodule(project.active_root(), url, branch, &relative)
                .map_err(InstructionRepositoryError::may_have_working_changes)?;
        } else if !GitRepository::new(&repository.root).is_repository() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "configure instruction submodule",
                "parent records the submodule, but its checkout is missing or invalid",
            )
            .repository(&repository));
        } else {
            validate_checkout_identity(&repository, url, branch)?;
        }
        self.validate_complete_store(&repository)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        let config = InstructionProjectConfig::new(InstructionProjectRepositoryMode::Submodule {
            path: relative,
            url: Some(url.to_string()),
            branch: Some(branch.to_string()),
        });
        save_project_config(&config_path, &config)
            .map_err(|error| error.may_have_working_changes())?;
        Ok(repository)
    }

    pub fn configure_external_remote(
        &self,
        launch_dir: impl AsRef<Path>,
        operation_id: &str,
        url: &str,
        branch: &str,
    ) -> InstructionRepositoryResult<InstructionRepositoryRef> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve external instruction project",
                    error.to_string(),
                )
            })?;
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        let checkout = external_checkout_root(roots, project.key());
        let repository = InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectExternal,
            root: checkout.clone(),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path.clone()),
            configured_branch: Some(branch.to_string()),
            configured_remote: Some(url.to_string()),
            owner_only: true,
        };
        validate_operation_id(operation_id)?;
        let _lease = acquire_mutation_lease(&roots.durable_state, &repository, operation_id)?;
        if !checkout.exists() {
            GitRepository::clone_remote(url, branch, &checkout)
                .map_err(InstructionRepositoryError::may_have_working_changes)?;
        } else if !GitRepository::new(&checkout).is_repository() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "configure external instruction repository",
                "private external checkout exists but is not a Git worktree",
            )
            .path(&checkout)
            .may_have_working_changes());
        } else {
            validate_checkout_identity(&repository, url, branch)?;
        }
        self.validate_complete_store(&repository)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        harden_private_checkout(&checkout)?;
        let config =
            InstructionProjectConfig::new(InstructionProjectRepositoryMode::ExternalRemote {
                url: url.to_string(),
                branch: branch.to_string(),
            });
        save_project_config(&config_path, &config)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        Ok(repository)
    }

    pub fn configure_external_local(
        &self,
        launch_dir: impl AsRef<Path>,
        operation_id: &str,
        checkout: impl AsRef<Path>,
        branch: Option<String>,
    ) -> InstructionRepositoryResult<InstructionRepositoryRef> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve external instruction project",
                    error.to_string(),
                )
            })?;
        let checkout = absolute_project_path(project.active_root(), checkout.as_ref());
        let canonical = std::fs::canonicalize(&checkout).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryUnavailable,
                "attach external instruction repository",
                error.to_string(),
            )
            .path(&checkout)
        })?;
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        let repository = InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectExternal,
            root: canonical.clone(),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path.clone()),
            configured_branch: branch.clone(),
            configured_remote: None,
            owner_only: false,
        };
        validate_operation_id(operation_id)?;
        let _lease = acquire_mutation_lease(&roots.durable_state, &repository, operation_id)?;
        let git = GitRepository::new(&canonical);
        if !git.is_repository() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "attach external instruction repository",
                "selected checkout is not a Git worktree",
            )
            .path(&canonical));
        }
        if let Some(expected_branch) = branch.as_deref() {
            let actual_branch = git.branch()?;
            if actual_branch.as_deref() != Some(expected_branch) {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "attach external instruction repository",
                    format!(
                        "requested branch '{expected_branch}', but the checkout is on {}",
                        actual_branch.as_deref().unwrap_or("detached HEAD")
                    ),
                )
                .path(&canonical));
            }
        }
        self.validate_complete_store(&repository)?;
        let config =
            InstructionProjectConfig::new(InstructionProjectRepositoryMode::ExternalLocal {
                path: canonical,
                branch,
            });
        save_project_config(&config_path, &config)?;
        Ok(repository)
    }

    pub fn configure_non_git_project(
        &self,
        launch_dir: impl AsRef<Path>,
        operation_id: &str,
        path: Option<PathBuf>,
        seed: &InstructionStoreSeed,
        legacy: &[InstructionLegacyImportSpec],
    ) -> InstructionRepositoryResult<InstructionStoreInitialization> {
        let roots = self.roots()?;
        let project = StartupContext::from_durable_state_dir(&roots.durable_state)
            .resolve_project(launch_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "resolve non-Git instruction project",
                    error.to_string(),
                )
            })?;
        if project.key().is_git() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "configure non-Git instruction repository",
                "Git projects use a submodule or external instruction repository",
            ));
        }
        let relative = path.unwrap_or_else(|| PathBuf::from(CONVENTIONAL_SUBMODULE_PATH));
        validate_relative_path(&relative)?;
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        let repository = InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::NonGitProject,
            root: project.active_root().join(&relative),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path.clone()),
            configured_branch: Some("main".to_string()),
            configured_remote: None,
            owner_only: false,
        };
        validate_operation_id(operation_id)?;
        let _lease = acquire_mutation_lease(&roots.durable_state, &repository, operation_id)?;
        let initialization =
            self.initialize_repository_locked(&repository, seed, legacy, "main", operation_id)?;
        let config = InstructionProjectConfig::new(InstructionProjectRepositoryMode::Standalone {
            path: relative,
        });
        save_project_config(&config_path, &config)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        Ok(initialization)
    }
}

fn configured_repository(
    roots: &super::service::ServiceRoots,
    project_root: &Path,
    project_key: &ProjectKey,
    project_id: String,
    config_path: PathBuf,
    config: InstructionProjectConfig,
) -> InstructionRepositoryResult<InstructionRepositoryRef> {
    match config.repository {
        InstructionProjectRepositoryMode::Submodule { path, url, branch } => {
            if !project_key.is_git() {
                return Err(config_error(
                    &config_path,
                    "submodule mode requires a Git parent project",
                ));
            }
            validate_relative_path(&path)?;
            Ok(InstructionRepositoryRef {
                id: project_id,
                kind: InstructionRepositoryKind::ProjectSubmodule,
                root: project_root.join(path),
                project_root: Some(project_root.to_path_buf()),
                project_config_path: Some(config_path),
                configured_branch: branch,
                configured_remote: url,
                owner_only: false,
            })
        }
        InstructionProjectRepositoryMode::ExternalRemote { url, branch } => {
            Ok(InstructionRepositoryRef {
                id: project_id,
                kind: InstructionRepositoryKind::ProjectExternal,
                root: external_checkout_root(roots, project_key),
                project_root: Some(project_root.to_path_buf()),
                project_config_path: Some(config_path),
                configured_branch: Some(branch),
                configured_remote: Some(url),
                owner_only: true,
            })
        }
        InstructionProjectRepositoryMode::ExternalLocal { path, branch } => {
            Ok(InstructionRepositoryRef {
                id: project_id,
                kind: InstructionRepositoryKind::ProjectExternal,
                root: absolute_project_path(project_root, &path),
                project_root: Some(project_root.to_path_buf()),
                project_config_path: Some(config_path),
                configured_branch: branch,
                configured_remote: None,
                owner_only: false,
            })
        }
        InstructionProjectRepositoryMode::Standalone { path } => {
            if project_key.is_git() {
                return Err(config_error(
                    &config_path,
                    "standalone mode is reserved for non-Git projects",
                ));
            }
            validate_relative_path(&path)?;
            Ok(InstructionRepositoryRef {
                id: project_id,
                kind: InstructionRepositoryKind::NonGitProject,
                root: project_root.join(path),
                project_root: Some(project_root.to_path_buf()),
                project_config_path: Some(config_path),
                configured_branch: Some("main".to_string()),
                configured_remote: None,
                owner_only: false,
            })
        }
    }
}

fn load_project_config(path: &Path) -> InstructionRepositoryResult<InstructionProjectConfig> {
    reject_project_config_symlinks(path)?;
    let content = std::fs::read_to_string(path).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "read instruction project configuration",
            error.to_string(),
        )
        .path(path)
    })?;
    let config: InstructionProjectConfig = toml::from_str(&content).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "parse instruction project configuration",
            error.to_string(),
        )
        .path(path)
    })?;
    if config.schema_version != INSTRUCTION_PROJECT_CONFIG_SCHEMA_VERSION {
        return Err(config_error(
            path,
            &format!(
                "project configuration schema version {} is unsupported; expected {}",
                config.schema_version, INSTRUCTION_PROJECT_CONFIG_SCHEMA_VERSION
            ),
        ));
    }
    Ok(config)
}

fn project_config_present(path: &Path) -> InstructionRepositoryResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Io,
            "inspect instruction project configuration",
            error.to_string(),
        )
        .path(path)),
    }
}

fn save_project_config(
    path: &Path,
    config: &InstructionProjectConfig,
) -> InstructionRepositoryResult<()> {
    reject_project_config_symlinks(path)?;
    let content = toml::to_string_pretty(config).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "serialize instruction project configuration",
            error.to_string(),
        )
        .path(path)
    })?;
    atomic_write_path(path, content.as_bytes(), false).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Io,
            "write instruction project configuration",
            error.to_string(),
        )
        .path(path)
    })
}

fn reject_project_config_symlinks(path: &Path) -> InstructionRepositoryResult<()> {
    for candidate in path.parent().into_iter().chain(std::iter::once(path)) {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::SymlinkEscape,
                    "access instruction project configuration",
                    "project instruction configuration may not traverse a symlink",
                )
                .path(candidate));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "inspect instruction project configuration",
                    error.to_string(),
                )
                .path(candidate));
            }
        }
    }
    Ok(())
}

fn external_checkout_root(
    roots: &super::service::ServiceRoots,
    project_key: &ProjectKey,
) -> PathBuf {
    roots
        .jcode_home
        .join("instruction-repositories")
        .join("projects")
        .join(project_key.digest())
}

fn absolute_project_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

fn harden_private_checkout(root: &Path) -> InstructionRepositoryResult<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        crate::platform::set_directory_permissions_owner_only(&directory).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "secure private instruction checkout",
                error.to_string(),
            )
            .path(&directory)
        })?;
        for entry in std::fs::read_dir(&directory).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "walk private instruction checkout",
                error.to_string(),
            )
            .path(&directory)
        })? {
            let entry = entry.map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "walk private instruction checkout",
                    error.to_string(),
                )
                .path(&directory)
            })?;
            let file_type = entry.file_type().map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "inspect private instruction checkout",
                    error.to_string(),
                )
                .path(entry.path())
            })?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                crate::platform::set_permissions_owner_only(&entry.path()).map_err(|error| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Io,
                        "secure private instruction checkout file",
                        error.to_string(),
                    )
                    .path(entry.path())
                })?;
            }
        }
    }
    Ok(())
}

fn config_error(path: &Path, detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Configuration,
        "load instruction project configuration",
        detail,
    )
    .path(path)
}

fn validate_checkout_identity(
    repository: &InstructionRepositoryRef,
    url: &str,
    branch: &str,
) -> InstructionRepositoryResult<()> {
    let git = GitRepository::new(&repository.root);
    let actual_url = git.remote_url("origin")?;
    if actual_url.as_deref() != Some(url) {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "reuse instruction repository checkout",
            format!(
                "existing checkout origin is {}, not the requested URL '{url}'",
                actual_url.as_deref().unwrap_or("not configured")
            ),
        )
        .repository(repository));
    }
    let actual_branch = git.branch()?;
    if actual_branch.as_deref() != Some(branch) {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "reuse instruction repository checkout",
            format!(
                "existing checkout is on {}, not the requested branch '{branch}'",
                actual_branch.as_deref().unwrap_or("detached HEAD")
            ),
        )
        .repository(repository));
    }
    Ok(())
}
