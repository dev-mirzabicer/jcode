use super::git::{GitRepository, validate_branch, validate_operation_id};
use super::lease::{acquire_mutation_lease, active_mutation_lease};
use super::mutation::{
    atomic_write, commit_request, fingerprint, read_working_utf8, safe_target, sha256,
    validate_relative_path,
};
use super::types::*;
use crate::instruction::{InstructionDocument, InstructionScope, InstructionSources};
use crate::startup_context::StartupContext;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const STORE_MANIFEST: &str = "instruction-store.toml";
const DEFAULT_BRANCH: &str = "main";

#[derive(Clone, Debug)]
pub struct InstructionRepositoryService {
    pub(super) roots: Result<ServiceRoots, String>,
}

#[derive(Clone, Debug)]
pub(super) struct ServiceRoots {
    pub(super) jcode_home: PathBuf,
    pub(super) durable_state: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct InitializationReceipt {
    repository_id: String,
    root: PathBuf,
    initial_commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct InitializationAttempt {
    repository_id: String,
    root: PathBuf,
    branch: String,
}

impl Default for InstructionRepositoryService {
    fn default() -> Self {
        Self::new()
    }
}

impl InstructionRepositoryService {
    /// Construct the server-owned service without performing repository I/O.
    /// An unavailable home directory remains a typed service error when an
    /// operation is attempted; server startup itself is not made dependent on
    /// instruction-store availability before production activation adopts it.
    pub fn new() -> Self {
        let roots = crate::storage::jcode_dir()
            .map(|jcode_home| ServiceRoots {
                jcode_home,
                durable_state: crate::storage::durable_state_dir(),
            })
            .map_err(|error| error.to_string());
        Self { roots }
    }

    pub fn from_paths(jcode_home: impl Into<PathBuf>, durable_state: impl Into<PathBuf>) -> Self {
        Self {
            roots: Ok(ServiceRoots {
                jcode_home: jcode_home.into(),
                durable_state: durable_state.into(),
            }),
        }
    }

    pub fn global_repository(&self) -> InstructionRepositoryResult<InstructionRepositoryRef> {
        let roots = self.roots()?;
        Ok(InstructionRepositoryRef {
            id: "global".to_string(),
            kind: InstructionRepositoryKind::Global,
            root: roots.jcode_home.join("instructions"),
            project_root: None,
            project_config_path: None,
            configured_branch: Some(DEFAULT_BRANCH.to_string()),
            configured_remote: None,
            owner_only: true,
        })
    }

    pub fn instruction_sources(
        &self,
        project: Option<&InstructionRepositoryRef>,
    ) -> InstructionRepositoryResult<InstructionSources> {
        let global = self.global_repository()?;
        let mut sources = InstructionSources::new(global.root);
        if let Some(project) = project {
            sources = sources.with_project_root(project.root.clone());
        }
        Ok(sources)
    }

    pub fn inspect(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<InstructionRepositoryState> {
        let roots = self.roots()?;
        if !repository.root.exists() {
            let initialized = self.initialization_receipt(repository).is_some();
            let configured = repository.project_config_path.is_some();
            let health = if initialized || configured {
                InstructionRepositoryHealth::Damaged(InstructionRepositoryDamage {
                    kind: if initialized {
                        InstructionRepositoryDamageKind::MissingAfterInitialization
                    } else {
                        InstructionRepositoryDamageKind::MissingCheckout
                    },
                    detail: format!(
                        "configured instruction repository is missing at {}",
                        repository.root.display()
                    ),
                    git_head_recovery_available: false,
                })
            } else {
                InstructionRepositoryHealth::Uninitialized
            };
            return Ok(empty_state(
                health,
                active_mutation_lease(&roots.durable_state, repository),
            ));
        }

        let metadata = std::fs::symlink_metadata(&repository.root).map_err(|error| {
            repository_io_error(
                repository,
                "inspect repository root",
                &repository.root,
                error,
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(empty_state(
                InstructionRepositoryHealth::Damaged(InstructionRepositoryDamage {
                    kind: InstructionRepositoryDamageKind::ConfiguredPathMismatch,
                    detail: "instruction repository root is not a real directory".to_string(),
                    git_head_recovery_available: false,
                }),
                active_mutation_lease(&roots.durable_state, repository),
            ));
        }

        let git = GitRepository::new(&repository.root);
        if !git.is_repository() {
            return Ok(empty_state(
                InstructionRepositoryHealth::Damaged(InstructionRepositoryDamage {
                    kind: InstructionRepositoryDamageKind::NotGitRepository,
                    detail: "configured instruction store is not a Git worktree".to_string(),
                    git_head_recovery_available: false,
                }),
                active_mutation_lease(&roots.durable_state, repository),
            ));
        }

        let head = match git.head() {
            Ok(head) => head,
            Err(error) => {
                return Ok(empty_state(
                    InstructionRepositoryHealth::Damaged(InstructionRepositoryDamage {
                        kind: InstructionRepositoryDamageKind::GitInspectionFailed,
                        detail: error.to_string(),
                        git_head_recovery_available: false,
                    }),
                    active_mutation_lease(&roots.durable_state, repository),
                ));
            }
        };
        let health = self.store_health(repository, &git, head.as_deref())?;
        let branch = git.branch()?;
        let detached = head.is_some() && branch.is_none();
        let changes = git.changes()?;
        let conflicts = changes
            .iter()
            .filter(|change| change.conflicted)
            .map(|change| change.path.clone())
            .collect();
        let upstream = git.upstream()?;
        let parent_gitlink = self.parent_gitlink(repository, &git)?;
        let mut configuration_warnings = Vec::new();
        if let (Some(expected), Some(actual)) = (&repository.configured_branch, &branch)
            && expected != actual
        {
            configuration_warnings.push(format!(
                "configured branch is '{expected}', but the checkout is on '{actual}'"
            ));
        }
        Ok(InstructionRepositoryState {
            health,
            head,
            branch,
            detached,
            upstream,
            changes,
            conflicts,
            parent_gitlink,
            active_mutation: active_mutation_lease(&roots.durable_state, repository),
            configuration_warnings,
        })
    }

    pub fn initialize_global(
        &self,
        seed: &InstructionStoreSeed,
        legacy: &[InstructionLegacyImportSpec],
    ) -> InstructionRepositoryResult<InstructionStoreInitialization> {
        let repository = self.global_repository()?;
        self.initialize_repository(&repository, seed, legacy, DEFAULT_BRANCH)
    }

    pub fn initialize_repository(
        &self,
        repository: &InstructionRepositoryRef,
        seed: &InstructionStoreSeed,
        legacy: &[InstructionLegacyImportSpec],
        branch: &str,
    ) -> InstructionRepositoryResult<InstructionStoreInitialization> {
        validate_branch(branch)?;
        let roots = self.roots()?;
        let operation_id = format!("initialize-{}", repository.id);
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, &operation_id)?;
        let existing_attempt = self.initialization_attempt(repository);

        if repository.root.exists() {
            let git = GitRepository::new(&repository.root);
            if git.is_repository()
                && let Some(head) = git.head()?
            {
                if matches!(
                    self.store_health(repository, &git, Some(&head))?,
                    InstructionRepositoryHealth::Ready
                ) {
                    if repository.owner_only {
                        harden_repository_tree(&repository.root)?;
                    }
                    self.write_initialization_receipt(repository, &head)?;
                    self.clear_initialization_attempt(repository);
                    let manifest = self.load_manifest(repository)?;
                    return Ok(InstructionStoreInitialization {
                        repository: repository.clone(),
                        commit: head,
                        created: false,
                        imported: manifest.legacy_imports.into_values().collect(),
                    });
                }
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::RepositoryDamaged,
                    "initialize instruction store",
                    "an existing Git store is invalid; use Git repair or explicit seed recreation",
                )
                .repository(repository));
            }
            if self.initialization_receipt(repository).is_some() {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::RepositoryDamaged,
                    "initialize instruction store",
                    "an initialized store is damaged; use Git repair or explicit seed recreation",
                )
                .repository(repository));
            }
            if repository
                .root
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_some())
                && !git.is_repository()
                && !existing_attempt.is_some_and(|attempt| {
                    attempt.repository_id == repository.id
                        && attempt.root == repository.root
                        && attempt.branch == branch
                })
            {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::RepositoryDamaged,
                    "initialize instruction store",
                    "refusing to initialize Git over a nonempty unmanaged directory",
                )
                .repository(repository)
                .path(&repository.root));
            }
        }

        for spec in legacy {
            validate_import_scope(repository, spec)?;
        }
        let plans = legacy
            .iter()
            .map(|spec| self.plan_legacy_import(spec))
            .collect::<InstructionRepositoryResult<Vec<_>>>()?;
        let plans = plans.into_iter().flatten().collect::<Vec<_>>();
        let (manifest, files, receipts) = prepare_seed(seed, &plans)?;
        let created_root = !repository.root.exists();
        self.write_initialization_attempt(repository, branch)?;
        std::fs::create_dir_all(&repository.root).map_err(|error| {
            repository_io_error(
                repository,
                "create instruction repository",
                &repository.root,
                error,
            )
        })?;
        if repository.owner_only {
            crate::platform::set_directory_permissions_owner_only(&repository.root).map_err(
                |error| {
                    repository_io_error(
                        repository,
                        "secure instruction repository",
                        &repository.root,
                        error,
                    )
                },
            )?;
        }

        let result = (|| {
            let manifest_content = toml::to_string_pretty(&manifest).map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::InvalidManifest,
                    "serialize instruction store manifest",
                    error.to_string(),
                )
                .repository(repository)
            })?;
            atomic_write(
                repository,
                Path::new(STORE_MANIFEST),
                manifest_content.as_bytes(),
            )?;
            for file in files {
                atomic_write(repository, &file.relative_path, &file.content)?;
            }
            self.validate_complete_store(repository)?;
            let git = GitRepository::init(&repository.root, branch)?;
            let index_path = self.isolated_index_path(repository, &operation_id)?;
            let commit = git.initial_commit(
                &index_path,
                "instruction: initialize managed store",
                &operation_id,
            );
            cleanup_index(&index_path);
            let commit = commit?;
            if repository.owner_only {
                harden_repository_tree(&repository.root)?;
            }
            self.write_initialization_receipt(repository, &commit)?;
            self.clear_initialization_attempt(repository);
            Ok(InstructionStoreInitialization {
                repository: repository.clone(),
                commit,
                created: true,
                imported: receipts,
            })
        })();
        if result.is_err() && created_root {
            let git = GitRepository::new(&repository.root);
            if git.head().ok().flatten().is_none() {
                let _ = std::fs::remove_dir_all(&repository.root);
            }
        }
        result
    }

    pub fn recreate_from_seed(
        &self,
        repository: &InstructionRepositoryRef,
        seed: &InstructionStoreSeed,
        legacy: &[InstructionLegacyImportSpec],
        branch: &str,
    ) -> InstructionRepositoryResult<InstructionStoreRecreation> {
        let roots = self.roots()?;
        let operation_id = format!("recreate-{}-{}", repository.id, crate::id::new_id("store"));
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, &operation_id)?;
        let backup = if repository.root.exists() {
            let parent = repository.root.parent().ok_or_else(|| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Configuration,
                    "recreate instruction store",
                    "repository root has no parent directory",
                )
            })?;
            let name = repository
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("instructions");
            let backup = parent.join(format!(
                "{name}.damaged-{}-{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                std::process::id()
            ));
            std::fs::rename(&repository.root, &backup).map_err(|error| {
                repository_io_error(
                    repository,
                    "preserve damaged instruction store",
                    &repository.root,
                    error,
                )
            })?;
            Some(backup)
        } else {
            None
        };
        let receipt = self.initialization_receipt_path(repository)?;
        let _ = std::fs::remove_file(receipt);
        drop(_lease);
        let initialization = self.initialize_repository(repository, seed, legacy, branch)?;
        Ok(InstructionStoreRecreation {
            initialization,
            damaged_backup: backup,
        })
    }

    pub fn load_manifest(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<InstructionStoreManifest> {
        let content =
            read_working_utf8(repository, Path::new(STORE_MANIFEST))?.ok_or_else(|| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::InvalidManifest,
                    "load instruction store manifest",
                    "instruction-store.toml is missing",
                )
                .repository(repository)
            })?;
        parse_manifest(repository, &content)
    }

    pub fn validate_repository(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<InstructionRepositoryValidation> {
        let manifest = self.load_manifest(repository)?;
        let runtime =
            crate::instruction::InstructionRuntime::discover(self.validation_sources(repository)?);
        Ok(InstructionRepositoryValidation {
            manifest,
            resources: runtime.resources(),
            diagnostics: runtime.diagnostics().to_vec(),
        })
    }

    pub fn load_committed_manifest(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<InstructionStoreManifest> {
        let git = GitRepository::new(&repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "load committed manifest",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let bytes = git
            .show_file(&head, Path::new(STORE_MANIFEST))?
            .ok_or_else(|| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::InvalidManifest,
                    "load committed manifest",
                    "current Git HEAD does not contain instruction-store.toml",
                )
                .repository(repository)
            })?;
        let content = String::from_utf8(bytes).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidUtf8,
                "load committed manifest",
                error.to_string(),
            )
            .repository(repository)
        })?;
        parse_manifest(repository, &content)
    }

    pub fn read_file(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: impl AsRef<Path>,
        policy: InstructionReadPolicy,
    ) -> InstructionRepositoryResult<InstructionFileContent> {
        let relative_path = relative_path.as_ref();
        if let Some(content) = read_working_utf8(repository, relative_path)? {
            return Ok(InstructionFileContent {
                relative_path: relative_path.to_path_buf(),
                source: InstructionFileSource::WorkingTree,
                content,
            });
        }
        if policy == InstructionReadPolicy::AllowHeadFallback {
            let git = GitRepository::new(&repository.root);
            if let Some(head) = git.head()?
                && let Some(bytes) = git.show_file(&head, relative_path)?
            {
                let content = String::from_utf8(bytes).map_err(|error| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::InvalidUtf8,
                        "read instruction file from Git HEAD",
                        error.to_string(),
                    )
                    .repository(repository)
                    .path(relative_path)
                })?;
                return Ok(InstructionFileContent {
                    relative_path: relative_path.to_path_buf(),
                    source: InstructionFileSource::GitHead,
                    content,
                });
            }
        }
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::RepositoryDamaged,
            "read instruction file",
            "managed instruction file is missing",
        )
        .repository(repository)
        .path(relative_path))
    }

    pub fn open_draft(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: impl AsRef<Path>,
    ) -> InstructionRepositoryResult<InstructionDraft> {
        let relative_path = relative_path.as_ref();
        validate_relative_path(relative_path)?;
        let git = GitRepository::new(&repository.root);
        let base_head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "open instruction draft",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        Ok(InstructionDraft {
            draft_id: crate::id::new_id("instruction_draft"),
            repository: repository.clone(),
            relative_path: relative_path.to_path_buf(),
            base: fingerprint(repository, relative_path)?,
            base_head,
            content: read_working_utf8(repository, relative_path)?,
        })
    }

    pub fn validate_draft(&self, draft: &InstructionDraft) -> InstructionRepositoryResult<()> {
        let git = GitRepository::new(&draft.repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "validate instruction draft",
                "instruction repository has no current HEAD",
            )
            .repository(&draft.repository)
        })?;
        if head != draft.base_head {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::StaleDraft,
                "validate instruction draft",
                "repository HEAD changed after the draft was opened",
            )
            .repository(&draft.repository));
        }
        let actual = fingerprint(&draft.repository, &draft.relative_path)?;
        if actual != draft.base {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::StaleDraft,
                "validate instruction draft",
                "target file changed after the draft was opened",
            )
            .repository(&draft.repository)
            .path(&draft.relative_path));
        }
        Ok(())
    }

    pub fn commit(
        &self,
        repository: &InstructionRepositoryRef,
        request: &InstructionCommitRequest,
    ) -> InstructionRepositoryResult<InstructionCommitOutcome> {
        let roots = self.roots()?;
        let before = self.validation_issue_set_at_head(repository, &request.mutations)?;
        commit_request(&roots.durable_state, repository, request, || {
            let after = self.validation_issue_set(repository)?;
            let introduced = after.difference(&before).cloned().collect::<Vec<_>>();
            if introduced.is_empty() {
                Ok(())
            } else {
                Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::RepositoryDamaged,
                    "validate instruction mutation",
                    format!(
                        "mutation introduced {} new resource or dependency error(s): {}",
                        introduced.len(),
                        introduced.join(" | ")
                    ),
                )
                .repository(repository))
            }
        })
    }

    pub fn commit_external_version(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: impl AsRef<Path>,
        operation_id: impl Into<String>,
        message: impl Into<String>,
    ) -> InstructionRepositoryResult<InstructionCommitOutcome> {
        let path = relative_path.as_ref();
        let git = GitRepository::new(&repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "commit external instruction edit",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let state = fingerprint(repository, path)?;
        let content = read_working_utf8(repository, path)?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "commit external instruction edit",
                "external target file is missing",
            )
            .repository(repository)
            .path(path)
        })?;
        self.commit(
            repository,
            &InstructionCommitRequest {
                operation_id: operation_id.into(),
                message: message.into(),
                expected_head: head,
                expected_files: vec![state],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: path.to_path_buf(),
                    content: content.into_bytes(),
                }],
            },
        )
    }

    pub fn history(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: Option<&Path>,
    ) -> InstructionRepositoryResult<Vec<InstructionHistoryEntry>> {
        GitRepository::new(&repository.root).history(relative_path)
    }

    pub fn content_at_revision(
        &self,
        repository: &InstructionRepositoryRef,
        commit: &str,
        relative_path: impl AsRef<Path>,
    ) -> InstructionRepositoryResult<InstructionRevisionContent> {
        let relative_path = relative_path.as_ref();
        validate_relative_path(relative_path)?;
        let bytes = GitRepository::new(&repository.root)
            .show_file(commit, relative_path)?
            .ok_or_else(|| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::RepositoryUnavailable,
                    "read instruction revision",
                    "file is absent at the selected revision",
                )
                .repository(repository)
                .path(relative_path)
            })?;
        let content = String::from_utf8(bytes).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidUtf8,
                "read instruction revision",
                error.to_string(),
            )
            .repository(repository)
            .path(relative_path)
        })?;
        Ok(InstructionRevisionContent {
            commit: commit.to_string(),
            relative_path: relative_path.to_path_buf(),
            content,
        })
    }

    pub fn compare_revisions(
        &self,
        repository: &InstructionRepositoryRef,
        from: &str,
        to: &str,
        relative_path: Option<&Path>,
    ) -> InstructionRepositoryResult<InstructionRevisionComparison> {
        if let Some(path) = relative_path {
            validate_relative_path(path)?;
        }
        let patch = GitRepository::new(&repository.root).compare(from, to, relative_path)?;
        Ok(InstructionRevisionComparison {
            from: from.to_string(),
            to: to.to_string(),
            relative_path: relative_path.map(Path::to_path_buf),
            patch,
        })
    }

    pub fn restore_revision(
        &self,
        repository: &InstructionRepositoryRef,
        commit: &str,
        relative_path: impl AsRef<Path>,
        operation_id: impl Into<String>,
        message: impl Into<String>,
    ) -> InstructionRepositoryResult<InstructionCommitOutcome> {
        let relative_path = relative_path.as_ref();
        let git = GitRepository::new(&repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "restore instruction revision",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let base = fingerprint(repository, relative_path)?;
        let mutation = match git.show_file(commit, relative_path)? {
            Some(content) => InstructionFileMutation::Write {
                relative_path: relative_path.to_path_buf(),
                content,
            },
            None => InstructionFileMutation::Delete {
                relative_path: relative_path.to_path_buf(),
            },
        };
        self.commit(
            repository,
            &InstructionCommitRequest {
                operation_id: operation_id.into(),
                message: message.into(),
                expected_head: head,
                expected_files: vec![base],
                mutations: vec![mutation],
            },
        )
    }

    pub fn restore_working_file_from_head(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: impl AsRef<Path>,
        operation_id: &str,
    ) -> InstructionRepositoryResult<InstructionFileContent> {
        validate_operation_id(operation_id)?;
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let relative_path = relative_path.as_ref();
        let git = GitRepository::new(&repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "restore working file from HEAD",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let bytes = git.show_file(&head, relative_path)?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "restore working file from HEAD",
                "current Git HEAD does not contain the requested file",
            )
            .repository(repository)
            .path(relative_path)
        })?;
        atomic_write(repository, relative_path, &bytes)?;
        let content = String::from_utf8(bytes).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidUtf8,
                "restore working file from HEAD",
                error.to_string(),
            )
            .repository(repository)
            .path(relative_path)
        })?;
        Ok(InstructionFileContent {
            relative_path: relative_path.to_path_buf(),
            source: InstructionFileSource::WorkingTree,
            content,
        })
    }

    pub fn clear_resource_body(
        &self,
        repository: &InstructionRepositoryRef,
        relative_path: impl AsRef<Path>,
        operation_id: impl Into<String>,
        message: impl Into<String>,
    ) -> InstructionRepositoryResult<InstructionCommitOutcome> {
        let relative_path = relative_path.as_ref();
        let kind = kind_for_path(relative_path)?;
        let scope = match repository.kind {
            InstructionRepositoryKind::Global => crate::instruction::InstructionScope::Global,
            _ => crate::instruction::InstructionScope::Project,
        };
        let source = self
            .read_file(
                repository,
                relative_path,
                InstructionReadPolicy::WorkingTreeOnly,
            )?
            .content;
        let absolute = safe_target(repository, relative_path, false)?;
        let mut document = crate::instruction::runtime::parse_document(
            scope, kind, &absolute, &source,
        )
        .map_err(|detail| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "clear instruction resource",
                detail,
            )
            .repository(repository)
            .path(relative_path)
        })?;
        document.body.clear();
        let content = document.to_markdown().map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "serialize cleared instruction resource",
                error.to_string(),
            )
            .repository(repository)
            .path(relative_path)
        })?;
        let draft = self.open_draft(repository, relative_path)?;
        self.commit(
            repository,
            &InstructionCommitRequest {
                operation_id: operation_id.into(),
                message: message.into(),
                expected_head: draft.base_head,
                expected_files: vec![draft.base],
                mutations: vec![InstructionFileMutation::Write {
                    relative_path: relative_path.to_path_buf(),
                    content: content.into_bytes(),
                }],
            },
        )
    }

    pub fn checkout_branch(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
        branch: &str,
        create: bool,
        start: Option<&str>,
    ) -> InstructionRepositoryResult<InstructionGitOperationOutcome> {
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        require_clean(repository, &git, "change branch")?;
        let before = git.head()?;
        git.branch_checkout(branch, create, start)?;
        Ok(InstructionGitOperationOutcome {
            head_before: before,
            head_after: git.head()?,
            branch: git.branch()?,
            detail: format!("checked out branch {branch}"),
        })
    }

    pub fn configure_remote(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
        name: &str,
        url: &str,
    ) -> InstructionRepositoryResult<InstructionGitOperationOutcome> {
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        let before = git.head()?;
        git.set_remote(name, url)?;
        Ok(InstructionGitOperationOutcome {
            head_before: before.clone(),
            head_after: before,
            branch: git.branch()?,
            detail: format!("configured remote {name}"),
        })
    }

    pub fn fetch(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
        remote: &str,
    ) -> InstructionRepositoryResult<InstructionGitOperationOutcome> {
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        let before = git.head()?;
        git.fetch(remote)?;
        Ok(InstructionGitOperationOutcome {
            head_before: before.clone(),
            head_after: before,
            branch: git.branch()?,
            detail: format!("fetched remote {remote}"),
        })
    }

    pub fn pull(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
        remote: &str,
        branch: &str,
        strategy: InstructionPullStrategy,
    ) -> InstructionRepositoryResult<InstructionGitOperationOutcome> {
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        require_clean(repository, &git, "pull")?;
        let before = git.head()?;
        git.pull(remote, branch, strategy).map_err(|error| {
            if GitRepository::new(&repository.root)
                .changes()
                .is_ok_and(|changes| !changes.is_empty())
            {
                error.may_have_working_changes()
            } else {
                error
            }
        })?;
        Ok(InstructionGitOperationOutcome {
            head_before: before,
            head_after: git.head()?,
            branch: git.branch()?,
            detail: format!("pulled {remote}/{branch}"),
        })
    }

    pub fn push(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
        remote: &str,
        branch: &str,
        set_upstream: bool,
    ) -> InstructionRepositoryResult<InstructionGitOperationOutcome> {
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        let before = git.head()?;
        git.push(remote, branch, set_upstream)?;
        Ok(InstructionGitOperationOutcome {
            head_before: before.clone(),
            head_after: before,
            branch: git.branch()?,
            detail: format!("pushed {branch} to {remote}"),
        })
    }

    pub fn plan_legacy_import(
        &self,
        spec: &InstructionLegacyImportSpec,
    ) -> InstructionRepositoryResult<Option<InstructionLegacyImportPlan>> {
        if spec.import_id.is_empty()
            || spec.import_id.len() > 128
            || !spec
                .import_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::LegacyImport,
                "plan legacy import",
                "legacy import ID is invalid",
            ));
        }
        validate_relative_path(&spec.target.relative_path)?;
        let bytes = match std::fs::read(&spec.source_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "read legacy instruction source",
                    error.to_string(),
                )
                .path(&spec.source_path));
            }
        };
        let source_content = String::from_utf8(bytes.clone()).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidUtf8,
                "read legacy instruction source",
                error.to_string(),
            )
            .path(&spec.source_path)
        })?;
        let document = InstructionDocument {
            id: spec.target.id.clone(),
            kind: spec.target.kind,
            scope: spec.target.scope,
            template_mode: spec.target.template_mode,
            metadata: spec.target.metadata.clone(),
            body: source_content.clone(),
            path: spec.target.relative_path.clone(),
        };
        let managed_content = document.to_markdown().map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::LegacyImport,
                "serialize legacy instruction import",
                error.to_string(),
            )
            .path(&spec.target.relative_path)
        })?;
        crate::instruction::runtime::parse_document(
            spec.target.scope,
            spec.target.kind,
            &spec.target.relative_path,
            &managed_content,
        )
        .map_err(|detail| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::LegacyImport,
                "validate legacy instruction import",
                detail,
            )
            .path(&spec.target.relative_path)
        })?;
        Ok(Some(InstructionLegacyImportPlan {
            spec: spec.clone(),
            source_sha256: sha256(&bytes),
            source_was_empty: source_content.is_empty(),
            source_was_blank: source_content.trim().is_empty(),
            source_content,
            managed_content,
        }))
    }

    /// Inspect the complete current compatibility-source set without importing
    /// it. Dedicated `AGENTS.md` and external skills are intentionally absent:
    /// they remain live ecosystem/read-only inputs under the accepted design.
    pub fn discover_known_legacy_sources(
        &self,
        launch_dir: Option<&Path>,
    ) -> InstructionRepositoryResult<Vec<InstructionLegacySourceSnapshot>> {
        let roots = self.roots()?;
        let global = [
            (
                LegacyInstructionSourceKind::SystemPrompt,
                roots.jcode_home.join("system-prompt.md"),
            ),
            (
                LegacyInstructionSourceKind::PromptOverlay,
                roots.jcode_home.join("prompt-overlay.md"),
            ),
            (
                LegacyInstructionSourceKind::PreferredTools,
                roots.jcode_home.join("preferred-tools.md"),
            ),
            (
                LegacyInstructionSourceKind::SwarmPrompt,
                roots.jcode_home.join("swarm-prompt.md"),
            ),
        ];
        let mut snapshots = global
            .into_iter()
            .map(|(kind, path)| legacy_source_snapshot(InstructionScope::Global, kind, path))
            .collect::<InstructionRepositoryResult<Vec<_>>>()?;
        if let Some(launch_dir) = launch_dir {
            let project = StartupContext::from_durable_state_dir(&roots.durable_state)
                .resolve_project(launch_dir)
                .map_err(|error| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Configuration,
                        "resolve project legacy instruction sources",
                        error.to_string(),
                    )
                })?;
            let project_dir = project.active_root().join(".jcode");
            for (kind, name) in [
                (
                    LegacyInstructionSourceKind::SystemPrompt,
                    "system-prompt.md",
                ),
                (
                    LegacyInstructionSourceKind::PromptOverlay,
                    "prompt-overlay.md",
                ),
                (
                    LegacyInstructionSourceKind::PreferredTools,
                    "preferred-tools.md",
                ),
                (LegacyInstructionSourceKind::SwarmPrompt, "swarm-prompt.md"),
            ] {
                snapshots.push(legacy_source_snapshot(
                    InstructionScope::Project,
                    kind,
                    project_dir.join(name),
                )?);
            }
        }
        Ok(snapshots)
    }

    pub fn import_legacy(
        &self,
        repository: &InstructionRepositoryRef,
        spec: &InstructionLegacyImportSpec,
        operation_id: &str,
    ) -> InstructionRepositoryResult<InstructionLegacyImportOutcome> {
        let Some(plan) = self.plan_legacy_import(spec)? else {
            return Ok(InstructionLegacyImportOutcome::SourceAbsent);
        };
        validate_import_scope(repository, spec)?;
        validate_operation_id(operation_id)?;
        let roots = self.roots()?;
        let _lease = acquire_mutation_lease(&roots.durable_state, repository, operation_id)?;
        let git = GitRepository::new(&repository.root);
        if let Some(commit) = git.find_operation_commit(operation_id)? {
            self.materialize_head_file(repository, &git, &commit, Path::new(STORE_MANIFEST))?;
            self.materialize_head_file(repository, &git, &commit, &plan.spec.target.relative_path)?;
            let receipt = self
                .load_committed_manifest(repository)?
                .legacy_imports
                .get(&plan.spec.import_id)
                .cloned()
                .ok_or_else(|| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::RepositoryDamaged,
                        "recover completed legacy import",
                        "operation commit does not contain its import receipt",
                    )
                    .repository(repository)
                })?;
            return Ok(InstructionLegacyImportOutcome::AlreadyImported { receipt, commit });
        }
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "import legacy instruction",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let mut manifest = self.load_committed_manifest(repository)?;
        if let Some(receipt) = manifest.legacy_imports.get(&plan.spec.import_id).cloned() {
            self.materialize_head_file(repository, &git, &head, Path::new(STORE_MANIFEST))?;
            self.materialize_head_file(repository, &git, &head, &plan.spec.target.relative_path)?;
            return Ok(InstructionLegacyImportOutcome::AlreadyImported {
                receipt,
                commit: head,
            });
        }
        if git
            .show_file(&head, &plan.spec.target.relative_path)?
            .is_some()
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Conflict,
                "import legacy instruction",
                "legacy import target already exists without a matching durable receipt",
            )
            .repository(repository)
            .path(&plan.spec.target.relative_path));
        }
        let receipt = receipt_for_plan(&plan);
        manifest
            .legacy_imports
            .insert(plan.spec.import_id.clone(), receipt.clone());
        let manifest_content = toml::to_string_pretty(&manifest).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidManifest,
                "serialize legacy import receipt",
                error.to_string(),
            )
            .repository(repository)
        })?;
        let index_path = self.isolated_index_path(repository, operation_id)?;
        let writes = vec![
            (
                PathBuf::from(STORE_MANIFEST),
                manifest_content.as_bytes().to_vec(),
            ),
            (
                plan.spec.target.relative_path.clone(),
                plan.managed_content.as_bytes().to_vec(),
            ),
        ];
        let committed = git.commit_virtual_files(
            &index_path,
            &writes,
            &[],
            &format!("instruction: import {}", plan.spec.import_id),
            operation_id,
            &head,
        );
        cleanup_index(&index_path);
        let commit = committed?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::LegacyImport,
                "import legacy instruction",
                "import produced no committed change without an existing receipt",
            )
            .repository(repository)
        })?;
        self.materialize_head_file(repository, &git, &commit, Path::new(STORE_MANIFEST))
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        self.materialize_head_file(repository, &git, &commit, &plan.spec.target.relative_path)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        Ok(InstructionLegacyImportOutcome::Imported { receipt, commit })
    }

    pub(super) fn roots(&self) -> InstructionRepositoryResult<&ServiceRoots> {
        self.roots.as_ref().map_err(|detail| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryUnavailable,
                "resolve instruction repository paths",
                detail.clone(),
            )
        })
    }

    fn validate_complete_store(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<()> {
        let validation = self.validate_repository(repository)?;
        if let Some(diagnostic) = validation.diagnostics.first() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "validate complete instruction store",
                diagnostic.detail.clone(),
            )
            .repository(repository)
            .path(&diagnostic.path));
        }
        Ok(())
    }

    fn validation_sources(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<InstructionSources> {
        match repository.kind {
            InstructionRepositoryKind::Global => Ok(InstructionSources::new(&repository.root)),
            _ => {
                let global = self.global_repository()?;
                let global_root = if global.root.exists() {
                    global.root
                } else {
                    let empty_global = self
                        .roots()?
                        .durable_state
                        .join("instruction-repositories")
                        .join("validation-empty-global");
                    std::fs::create_dir_all(&empty_global).map_err(|error| {
                        repository_io_error(
                            repository,
                            "create validation root",
                            &empty_global,
                            error,
                        )
                    })?;
                    empty_global
                };
                Ok(InstructionSources::new(global_root).with_project_root(&repository.root))
            }
        }
    }

    fn validation_issue_set(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<BTreeSet<String>> {
        self.validation_issue_set_for_root(repository, &repository.root, BTreeSet::new())
    }

    fn validation_issue_set_at_head(
        &self,
        repository: &InstructionRepositoryRef,
        mutations: &[InstructionFileMutation],
    ) -> InstructionRepositoryResult<BTreeSet<String>> {
        let mut allowed = self.validation_issue_set(repository)?;
        let git = GitRepository::new(&repository.root);
        let head = git.head()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "validate committed instruction repository",
                "instruction repository has no current HEAD",
            )
            .repository(repository)
        })?;
        let validation_dir = self
            .roots()?
            .durable_state
            .join("instruction-repositories")
            .join("validation-snapshots");
        crate::storage::ensure_dir(&validation_dir).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "create instruction validation directory",
                error.to_string(),
            )
            .repository(repository)
            .path(&validation_dir)
        })?;
        let snapshot = tempfile::Builder::new()
            .prefix(&format!("{}-", repository.id))
            .tempdir_in(&validation_dir)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "create committed instruction snapshot",
                    error.to_string(),
                )
                .repository(repository)
                .path(&validation_dir)
            })?;
        let mut issues = BTreeSet::new();
        for entry in git.tree_entries(&head)? {
            validate_relative_path(&entry.path)?;
            match entry.mode.as_str() {
                "100644" | "100755" => {
                    let bytes = git.show_file(&head, &entry.path)?.ok_or_else(|| {
                        InstructionRepositoryError::new(
                            InstructionRepositoryErrorKind::RepositoryDamaged,
                            "materialize committed instruction snapshot",
                            "Git tree entry disappeared while reading the same commit",
                        )
                        .repository(repository)
                        .path(&entry.path)
                    })?;
                    let target = snapshot.path().join(&entry.path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| {
                            repository_io_error(
                                repository,
                                "create committed snapshot directory",
                                parent,
                                error,
                            )
                        })?;
                    }
                    std::fs::write(&target, bytes).map_err(|error| {
                        repository_io_error(
                            repository,
                            "write committed instruction snapshot",
                            &target,
                            error,
                        )
                    })?;
                }
                "120000" => {
                    issues.insert(format!("path:{}:symlink", entry.path.display()));
                }
                mode => {
                    issues.insert(format!(
                        "path:{}:unsupported-mode:{mode}",
                        entry.path.display()
                    ));
                }
            }
        }
        let filters = self.impacted_issue_filters(repository, snapshot.path(), mutations)?;
        allowed.retain(|issue| !filters.iter().any(|filter| issue.contains(filter)));
        allowed.extend(self.validation_issue_set_for_root(repository, snapshot.path(), issues)?);
        Ok(allowed)
    }

    fn validation_issue_set_for_root(
        &self,
        repository: &InstructionRepositoryRef,
        repository_root: &Path,
        mut issues: BTreeSet<String>,
    ) -> InstructionRepositoryResult<BTreeSet<String>> {
        let runtime = crate::instruction::InstructionRuntime::discover(
            self.validation_sources_for_root(repository, repository_root)?,
        );
        collect_managed_path_issues(repository, repository_root, repository_root, &mut issues)?;
        for diagnostic in runtime.diagnostics() {
            let path = diagnostic
                .path
                .strip_prefix(repository_root)
                .unwrap_or(&diagnostic.path);
            issues.insert(format!(
                "diagnostic:{}:{}",
                path.display(),
                diagnostic.detail
            ));
        }
        for summary in runtime.resources() {
            match &summary.state {
                crate::instruction::ResourceValidationState::Valid => {
                    let selector = match summary.resource.scope {
                        InstructionScope::Global => {
                            crate::instruction::InstructionSelector::global(
                                summary.resource.kind,
                                summary.resource.id.as_str(),
                            )
                        }
                        InstructionScope::Project => {
                            crate::instruction::InstructionSelector::project(
                                summary.resource.kind,
                                summary.resource.id.as_str(),
                            )
                        }
                    }
                    .map_err(|error| {
                        InstructionRepositoryError::new(
                            InstructionRepositoryErrorKind::RepositoryDamaged,
                            "validate instruction repository graph",
                            error.to_string(),
                        )
                        .repository(repository)
                    })?;
                    if let Err(error) = runtime.validate_graph(&selector) {
                        issues.insert(format!("graph:{}:{error}", summary.resource));
                    }
                }
                crate::instruction::ResourceValidationState::Invalid(detail) => {
                    issues.insert(format!(
                        "resource:{}:invalid:{}:{}",
                        summary.resource,
                        summary
                            .paths
                            .iter()
                            .map(|path| {
                                path.strip_prefix(repository_root)
                                    .unwrap_or(path)
                                    .display()
                                    .to_string()
                            })
                            .collect::<Vec<_>>()
                            .join(","),
                        detail
                    ));
                }
                crate::instruction::ResourceValidationState::Ambiguous => {
                    issues.insert(format!(
                        "resource:{}:ambiguous:{}",
                        summary.resource,
                        summary
                            .paths
                            .iter()
                            .map(|path| {
                                path.strip_prefix(repository_root)
                                    .unwrap_or(path)
                                    .display()
                                    .to_string()
                            })
                            .collect::<Vec<_>>()
                            .join(",")
                    ));
                }
            }
        }
        Ok(issues)
    }

    fn validation_sources_for_root(
        &self,
        repository: &InstructionRepositoryRef,
        repository_root: &Path,
    ) -> InstructionRepositoryResult<InstructionSources> {
        match repository.kind {
            InstructionRepositoryKind::Global => Ok(InstructionSources::new(repository_root)),
            _ => {
                let global = self.global_repository()?;
                let global_root = if global.root.exists() {
                    global.root
                } else {
                    let empty_global = self
                        .roots()?
                        .durable_state
                        .join("instruction-repositories")
                        .join("validation-empty-global");
                    std::fs::create_dir_all(&empty_global).map_err(|error| {
                        repository_io_error(
                            repository,
                            "create validation root",
                            &empty_global,
                            error,
                        )
                    })?;
                    empty_global
                };
                Ok(InstructionSources::new(global_root).with_project_root(repository_root))
            }
        }
    }

    fn impacted_issue_filters(
        &self,
        repository: &InstructionRepositoryRef,
        repository_root: &Path,
        mutations: &[InstructionFileMutation],
    ) -> InstructionRepositoryResult<BTreeSet<String>> {
        let affected_paths = mutations
            .iter()
            .flat_map(InstructionFileMutation::affected_paths)
            .cloned()
            .collect::<BTreeSet<_>>();
        let runtime = crate::instruction::InstructionRuntime::discover(
            self.validation_sources_for_root(repository, repository_root)?,
        );
        let resources = runtime.resources();
        let mut impacted = resources
            .iter()
            .filter(|summary| {
                summary.paths.iter().any(|path| {
                    path.strip_prefix(repository_root)
                        .is_ok_and(|relative| affected_paths.contains(relative))
                })
            })
            .map(|summary| summary.resource.clone())
            .collect::<BTreeSet<_>>();

        loop {
            let mut changed = false;
            for summary in &resources {
                if impacted.contains(&summary.resource)
                    || !matches!(
                        summary.state,
                        crate::instruction::ResourceValidationState::Valid
                    )
                {
                    continue;
                }
                let selector = explicit_selector(&summary.resource)?;
                let Ok(graph) = runtime.validate_graph(&selector) else {
                    continue;
                };
                let references_impacted = graph
                    .render_dependencies
                    .iter()
                    .chain(&graph.validation_dependencies)
                    .any(|(consumer, dependencies)| {
                        impacted.contains(consumer)
                            || dependencies
                                .iter()
                                .any(|dependency| impacted.contains(dependency))
                    });
                if references_impacted {
                    changed |= impacted.insert(summary.resource.clone());
                }
            }
            if !changed {
                break;
            }
        }

        let mut filters = affected_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<BTreeSet<_>>();
        filters.extend(impacted.into_iter().map(|resource| resource.to_string()));
        Ok(filters)
    }

    fn store_health(
        &self,
        repository: &InstructionRepositoryRef,
        git: &GitRepository,
        head: Option<&str>,
    ) -> InstructionRepositoryResult<InstructionRepositoryHealth> {
        let manifest_path = repository.root.join(STORE_MANIFEST);
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(InstructionRepositoryHealth::Damaged(
                    InstructionRepositoryDamage {
                        kind: InstructionRepositoryDamageKind::MissingManifest,
                        detail: "instruction-store.toml is missing".to_string(),
                        git_head_recovery_available: head.is_some_and(|_| {
                            git.file_exists_at_head(Path::new(STORE_MANIFEST))
                                .unwrap_or(false)
                        }),
                    },
                ));
            }
            Err(error) => {
                return Ok(InstructionRepositoryHealth::Damaged(
                    InstructionRepositoryDamage {
                        kind: InstructionRepositoryDamageKind::InvalidManifest,
                        detail: error.to_string(),
                        git_head_recovery_available: false,
                    },
                ));
            }
        };
        match parse_manifest(repository, &content) {
            Ok(_) => Ok(InstructionRepositoryHealth::Ready),
            Err(error) => Ok(InstructionRepositoryHealth::Damaged(
                InstructionRepositoryDamage {
                    kind: if error.detail.contains("schema version") {
                        InstructionRepositoryDamageKind::UnsupportedSchema
                    } else {
                        InstructionRepositoryDamageKind::InvalidManifest
                    },
                    detail: error.detail,
                    git_head_recovery_available: head.is_some_and(|_| {
                        git.file_exists_at_head(Path::new(STORE_MANIFEST))
                            .unwrap_or(false)
                    }),
                },
            )),
        }
    }

    fn parent_gitlink(
        &self,
        repository: &InstructionRepositoryRef,
        git: &GitRepository,
    ) -> InstructionRepositoryResult<Option<ParentGitlinkState>> {
        if repository.kind != InstructionRepositoryKind::ProjectSubmodule {
            return Ok(None);
        }
        let parent = repository.project_root.as_ref().ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "inspect parent gitlink",
                "submodule repository is missing its parent project root",
            )
            .repository(repository)
        })?;
        let relative = repository.root.strip_prefix(parent).map_err(|_| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "inspect parent gitlink",
                "submodule root is outside its parent project",
            )
            .repository(repository)
        })?;
        let recorded_commit = GitRepository::submodule_recorded_commit(parent, relative)?;
        let parent_changes = GitRepository::new(parent).changes()?;
        let gitmodules_changed = parent_changes
            .iter()
            .any(|change| change.path == Path::new(".gitmodules"));
        let gitlink_changed = parent_changes.iter().any(|change| change.path == relative);
        Ok(Some(ParentGitlinkState {
            path: relative.to_path_buf(),
            gitmodules_changed,
            gitlink_changed,
            recorded_commit,
            checked_out_commit: git.head()?,
        }))
    }

    fn materialize_head_file(
        &self,
        repository: &InstructionRepositoryRef,
        git: &GitRepository,
        commit: &str,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<()> {
        let bytes = git.show_file(commit, relative_path)?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "materialize committed instruction file",
                "committed operation is missing an expected file",
            )
            .repository(repository)
            .path(relative_path)
        })?;
        atomic_write(repository, relative_path, &bytes)
    }

    fn isolated_index_path(
        &self,
        repository: &InstructionRepositoryRef,
        operation_id: &str,
    ) -> InstructionRepositoryResult<PathBuf> {
        validate_operation_id(operation_id)?;
        let directory = self
            .roots()?
            .durable_state
            .join("instruction-repositories")
            .join("isolated-indexes")
            .join(&repository.id);
        crate::storage::ensure_dir(&directory).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "create isolated index directory",
                error.to_string(),
            )
            .repository(repository)
        })?;
        Ok(directory.join(format!("{operation_id}.index")))
    }

    fn initialization_receipt(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> Option<InitializationReceipt> {
        let path = self.initialization_receipt_path(repository).ok()?;
        crate::storage::read_json(&path).ok()
    }

    fn initialization_attempt(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> Option<InitializationAttempt> {
        let path = self.initialization_attempt_path(repository).ok()?;
        crate::storage::read_json(&path).ok()
    }

    fn initialization_attempt_path(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<PathBuf> {
        Ok(self
            .roots()?
            .durable_state
            .join("instruction-repositories")
            .join("initializing")
            .join(format!("{}.json", repository.id)))
    }

    fn write_initialization_attempt(
        &self,
        repository: &InstructionRepositoryRef,
        branch: &str,
    ) -> InstructionRepositoryResult<()> {
        let path = self.initialization_attempt_path(repository)?;
        crate::storage::write_json_secret(
            &path,
            &InitializationAttempt {
                repository_id: repository.id.clone(),
                root: repository.root.clone(),
                branch: branch.to_string(),
            },
        )
        .map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "persist instruction-store initialization attempt",
                error.to_string(),
            )
            .repository(repository)
            .path(path)
        })
    }

    fn clear_initialization_attempt(&self, repository: &InstructionRepositoryRef) {
        if let Ok(path) = self.initialization_attempt_path(repository) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn initialization_receipt_path(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<PathBuf> {
        Ok(self
            .roots()?
            .durable_state
            .join("instruction-repositories")
            .join("initialized")
            .join(format!("{}.json", repository.id)))
    }

    fn write_initialization_receipt(
        &self,
        repository: &InstructionRepositoryRef,
        commit: &str,
    ) -> InstructionRepositoryResult<()> {
        let path = self.initialization_receipt_path(repository)?;
        crate::storage::write_json_secret(
            &path,
            &InitializationReceipt {
                repository_id: repository.id.clone(),
                root: repository.root.clone(),
                initial_commit: commit.to_string(),
            },
        )
        .map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "persist instruction-store initialization receipt",
                error.to_string(),
            )
            .repository(repository)
            .path(path)
        })
    }
}

fn prepare_seed(
    seed: &InstructionStoreSeed,
    imports: &[InstructionLegacyImportPlan],
) -> InstructionRepositoryResult<(
    InstructionStoreManifest,
    Vec<InstructionSeedFile>,
    Vec<LegacyImportReceipt>,
)> {
    if seed.manifest.schema_version != INSTRUCTION_STORE_SCHEMA_VERSION {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidManifest,
            "prepare instruction-store seed",
            format!(
                "seed schema version {} is unsupported; expected {}",
                seed.manifest.schema_version, INSTRUCTION_STORE_SCHEMA_VERSION
            ),
        ));
    }
    let mut manifest = seed.manifest.clone();
    let mut files = BTreeMap::<PathBuf, Vec<u8>>::new();
    for file in &seed.files {
        validate_relative_path(&file.relative_path)?;
        if file.relative_path == Path::new(STORE_MANIFEST) {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidManifest,
                "prepare instruction-store seed",
                "seed files must not replace instruction-store.toml",
            ));
        }
        if files
            .insert(file.relative_path.clone(), file.content.clone())
            .is_some()
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Conflict,
                "prepare instruction-store seed",
                format!("duplicate seed path {}", file.relative_path.display()),
            ));
        }
    }
    let mut receipts = Vec::new();
    for plan in imports {
        let receipt = receipt_for_plan(plan);
        if manifest
            .legacy_imports
            .insert(plan.spec.import_id.clone(), receipt.clone())
            .is_some()
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Conflict,
                "prepare instruction-store seed",
                format!("duplicate legacy import ID {}", plan.spec.import_id),
            ));
        }
        if files
            .insert(
                plan.spec.target.relative_path.clone(),
                plan.managed_content.as_bytes().to_vec(),
            )
            .is_some()
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Conflict,
                "prepare instruction-store seed",
                format!(
                    "legacy import target {} collides with another seed file",
                    plan.spec.target.relative_path.display()
                ),
            ));
        }
        receipts.push(receipt);
    }
    Ok((
        manifest,
        files
            .into_iter()
            .map(|(relative_path, content)| InstructionSeedFile {
                relative_path,
                content,
            })
            .collect(),
        receipts,
    ))
}

fn receipt_for_plan(plan: &InstructionLegacyImportPlan) -> LegacyImportReceipt {
    LegacyImportReceipt {
        source_kind: plan.spec.source_kind,
        source_path: plan.spec.source_path.clone(),
        source_sha256: plan.source_sha256.clone(),
        target_path: plan.spec.target.relative_path.clone(),
        target: plan.spec.target.resource().to_string(),
        source_was_empty: plan.source_was_empty,
        source_was_blank: plan.source_was_blank,
    }
}

fn parse_manifest(
    repository: &InstructionRepositoryRef,
    content: &str,
) -> InstructionRepositoryResult<InstructionStoreManifest> {
    let manifest: InstructionStoreManifest = toml::from_str(content).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidManifest,
            "parse instruction store manifest",
            error.to_string(),
        )
        .repository(repository)
        .path(Path::new(STORE_MANIFEST))
    })?;
    if manifest.schema_version != INSTRUCTION_STORE_SCHEMA_VERSION {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidManifest,
            "parse instruction store manifest",
            format!(
                "instruction-store schema version {} is unsupported; expected {}",
                manifest.schema_version, INSTRUCTION_STORE_SCHEMA_VERSION
            ),
        )
        .repository(repository)
        .path(Path::new(STORE_MANIFEST)));
    }
    Ok(manifest)
}

fn empty_state(
    health: InstructionRepositoryHealth,
    active_mutation: Option<InstructionMutationLeaseInfo>,
) -> InstructionRepositoryState {
    InstructionRepositoryState {
        health,
        head: None,
        branch: None,
        detached: false,
        upstream: None,
        changes: Vec::new(),
        conflicts: Vec::new(),
        parent_gitlink: None,
        active_mutation,
        configuration_warnings: Vec::new(),
    }
}

fn kind_for_path(path: &Path) -> InstructionRepositoryResult<crate::instruction::InstructionKind> {
    let directory = path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::InvalidPath,
                "identify instruction resource kind",
                "resource path has no UTF-8 category directory",
            )
            .path(path)
        })?;
    match directory {
        "system" => Ok(crate::instruction::InstructionKind::System),
        "agents" => Ok(crate::instruction::InstructionKind::Agent),
        "addenda" => Ok(crate::instruction::InstructionKind::AgentAddendum),
        "modules" => Ok(crate::instruction::InstructionKind::Module),
        "notifications" => Ok(crate::instruction::InstructionKind::Notification),
        "tools" => Ok(crate::instruction::InstructionKind::ToolGuidance),
        "skills" => Ok(crate::instruction::InstructionKind::Skill),
        _ => Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidPath,
            "identify instruction resource kind",
            format!("unknown instruction category '{directory}'"),
        )
        .path(path)),
    }
}

fn require_clean(
    repository: &InstructionRepositoryRef,
    git: &GitRepository,
    operation: &str,
) -> InstructionRepositoryResult<()> {
    let changes = git.changes()?;
    if changes.is_empty() {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::DirtyWorkingTree,
            operation,
            format!(
                "instruction repository has {} working-tree or index change(s)",
                changes.len()
            ),
        )
        .repository(repository))
    }
}

fn cleanup_index(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    let _ = std::fs::remove_file(PathBuf::from(lock));
}

fn harden_repository_tree(root: &Path) -> InstructionRepositoryResult<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        crate::platform::set_directory_permissions_owner_only(&directory).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "secure instruction repository directory",
                error.to_string(),
            )
            .path(&directory)
        })?;
        for entry in std::fs::read_dir(&directory).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "walk instruction repository permissions",
                error.to_string(),
            )
            .path(&directory)
        })? {
            let entry = entry.map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "walk instruction repository permissions",
                    error.to_string(),
                )
                .path(&directory)
            })?;
            let metadata = entry.file_type().map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "inspect instruction repository permissions",
                    error.to_string(),
                )
                .path(entry.path())
            })?;
            if metadata.is_dir() {
                directories.push(entry.path());
            } else if metadata.is_file() {
                crate::platform::set_permissions_owner_only(&entry.path()).map_err(|error| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Io,
                        "secure instruction repository file",
                        error.to_string(),
                    )
                    .path(entry.path())
                })?;
            }
        }
    }
    Ok(())
}

fn repository_io_error(
    repository: &InstructionRepositoryRef,
    operation: &str,
    path: &Path,
    error: std::io::Error,
) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Io,
        operation,
        error.to_string(),
    )
    .repository(repository)
    .path(path)
}

fn explicit_selector(
    resource: &crate::instruction::InstructionResourceRef,
) -> InstructionRepositoryResult<crate::instruction::InstructionSelector> {
    let selector = match resource.scope {
        InstructionScope::Global => {
            crate::instruction::InstructionSelector::global(resource.kind, resource.id.as_str())
        }
        InstructionScope::Project => {
            crate::instruction::InstructionSelector::project(resource.kind, resource.id.as_str())
        }
    };
    selector.map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::RepositoryDamaged,
            "construct instruction validation selector",
            error.to_string(),
        )
    })
}

fn collect_managed_path_issues(
    repository: &InstructionRepositoryRef,
    root: &Path,
    directory: &Path,
    issues: &mut BTreeSet<String>,
) -> InstructionRepositoryResult<()> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        repository_io_error(
            repository,
            "inspect managed repository paths",
            directory,
            error,
        )
    })? {
        let entry = entry.map_err(|error| {
            repository_io_error(
                repository,
                "inspect managed repository path",
                directory,
                error,
            )
        })?;
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::SymlinkEscape,
                "inspect managed repository path",
                "repository entry escaped its configured root",
            )
            .repository(repository)
            .path(&path)
        })?;
        if relative
            .components()
            .next()
            .is_some_and(|component| component.as_os_str() == std::ffi::OsStr::new(".git"))
        {
            continue;
        }
        if relative.to_str().is_none() {
            issues.insert(format!("path:{}:non-utf8", relative.display()));
        }
        let file_type = entry.file_type().map_err(|error| {
            repository_io_error(repository, "inspect managed repository path", &path, error)
        })?;
        if file_type.is_symlink() {
            issues.insert(format!("path:{}:symlink", relative.display()));
        } else if file_type.is_dir() {
            collect_managed_path_issues(repository, root, &path, issues)?;
        } else if !file_type.is_file() {
            issues.insert(format!("path:{}:unsupported-file-type", relative.display()));
        }
    }
    Ok(())
}

fn legacy_source_snapshot(
    scope: InstructionScope,
    source_kind: LegacyInstructionSourceKind,
    path: PathBuf,
) -> InstructionRepositoryResult<InstructionLegacySourceSnapshot> {
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "inspect legacy instruction source",
                error.to_string(),
            )
            .path(&path));
        }
    };
    let Some(bytes) = bytes else {
        return Ok(InstructionLegacySourceSnapshot {
            scope,
            source_kind,
            path,
            content: None,
            source_sha256: None,
            source_was_empty: None,
            source_was_blank: None,
        });
    };
    let content = String::from_utf8(bytes.clone()).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidUtf8,
            "inspect legacy instruction source",
            error.to_string(),
        )
        .path(&path)
    })?;
    Ok(InstructionLegacySourceSnapshot {
        scope,
        source_kind,
        path,
        source_sha256: Some(sha256(&bytes)),
        source_was_empty: Some(content.is_empty()),
        source_was_blank: Some(content.trim().is_empty()),
        content: Some(content),
    })
}

fn validate_import_scope(
    repository: &InstructionRepositoryRef,
    spec: &InstructionLegacyImportSpec,
) -> InstructionRepositoryResult<()> {
    let expected = match repository.kind {
        InstructionRepositoryKind::Global => InstructionScope::Global,
        _ => InstructionScope::Project,
    };
    if spec.target.scope == expected {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::LegacyImport,
            "validate legacy import scope",
            format!(
                "target scope {} does not match {} repository",
                spec.target.scope, repository.kind
            ),
        )
        .repository(repository)
        .path(&spec.target.relative_path))
    }
}
