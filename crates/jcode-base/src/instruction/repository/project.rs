use super::git::GitRepository;
use super::mutation::{atomic_write_path, validate_relative_path};
use super::service::InstructionRepositoryService;
use super::types::*;
use crate::startup_context::{ProjectKey, StartupContext};
use std::path::{Path, PathBuf};

const PROJECT_CONFIG_RELATIVE_PATH: &str = ".jcode/instructions.toml";
const CONVENTIONAL_SUBMODULE_PATH: &str = ".jcode/instructions";

impl InstructionRepositoryService {
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
        if config_path.exists() {
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
        if !path.exists() {
            return Ok(None);
        }
        load_project_config(&path).map(Some)
    }

    pub fn configure_submodule(
        &self,
        launch_dir: impl AsRef<Path>,
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
        GitRepository::add_submodule(project.active_root(), url, branch, &relative)?;
        let config = InstructionProjectConfig::new(InstructionProjectRepositoryMode::Submodule {
            path: relative.clone(),
            url: Some(url.to_string()),
            branch: Some(branch.to_string()),
        });
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        save_project_config(&config_path, &config)
            .map_err(|error| error.may_have_working_changes())?;
        Ok(InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectSubmodule,
            root: project.active_root().join(relative),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path),
            configured_branch: Some(branch.to_string()),
            configured_remote: Some(url.to_string()),
            owner_only: false,
        })
    }

    pub fn configure_external_remote(
        &self,
        launch_dir: impl AsRef<Path>,
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
        let config =
            InstructionProjectConfig::new(InstructionProjectRepositoryMode::ExternalRemote {
                url: url.to_string(),
                branch: branch.to_string(),
            });
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        save_project_config(&config_path, &config)?;
        let checkout = external_checkout_root(roots, project.key());
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
        }
        harden_private_checkout(&checkout)?;
        Ok(InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectExternal,
            root: checkout,
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path),
            configured_branch: Some(branch.to_string()),
            configured_remote: Some(url.to_string()),
            owner_only: true,
        })
    }

    pub fn configure_external_local(
        &self,
        launch_dir: impl AsRef<Path>,
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
        if !GitRepository::new(&canonical).is_repository() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "attach external instruction repository",
                "selected checkout is not a Git worktree",
            )
            .path(&canonical));
        }
        let config =
            InstructionProjectConfig::new(InstructionProjectRepositoryMode::ExternalLocal {
                path: canonical.clone(),
                branch: branch.clone(),
            });
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        save_project_config(&config_path, &config)?;
        Ok(InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::ProjectExternal,
            root: canonical,
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path),
            configured_branch: branch,
            configured_remote: None,
            owner_only: false,
        })
    }

    pub fn configure_non_git_project(
        &self,
        launch_dir: impl AsRef<Path>,
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
        let config = InstructionProjectConfig::new(InstructionProjectRepositoryMode::Standalone {
            path: relative.clone(),
        });
        let config_path = project.active_root().join(PROJECT_CONFIG_RELATIVE_PATH);
        save_project_config(&config_path, &config)?;
        let repository = InstructionRepositoryRef {
            id: format!("project-{}", project.key().digest()),
            kind: InstructionRepositoryKind::NonGitProject,
            root: project.active_root().join(relative),
            project_root: Some(project.active_root().to_path_buf()),
            project_config_path: Some(config_path),
            configured_branch: Some("main".to_string()),
            configured_remote: None,
            owner_only: false,
        };
        self.initialize_repository(&repository, seed, legacy, "main")
            .map_err(InstructionRepositoryError::may_have_working_changes)
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

fn save_project_config(
    path: &Path,
    config: &InstructionProjectConfig,
) -> InstructionRepositoryResult<()> {
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
