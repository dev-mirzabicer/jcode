use super::super::{
    InstructionDiagnostic, InstructionId, InstructionKind, InstructionMetadata,
    InstructionResourceRef, InstructionResourceSummary, InstructionScope, TemplateMode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

pub const INSTRUCTION_STORE_SCHEMA_VERSION: u32 = 1;
pub const INSTRUCTION_STORE_SEED_VERSION: u32 = 8;

fn initial_instruction_store_seed_version() -> u32 {
    1
}
pub const INSTRUCTION_PROJECT_CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstructionRepositoryKind {
    Global,
    ProjectSubmodule,
    ProjectExternal,
    NonGitProject,
}

impl fmt::Display for InstructionRepositoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "global",
            Self::ProjectSubmodule => "project submodule",
            Self::ProjectExternal => "project external",
            Self::NonGitProject => "non-Git project",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionRepositoryRef {
    pub id: String,
    pub kind: InstructionRepositoryKind,
    pub root: PathBuf,
    pub project_root: Option<PathBuf>,
    pub project_config_path: Option<PathBuf>,
    pub configured_branch: Option<String>,
    pub configured_remote: Option<String>,
    pub owner_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstructionRepositoryDamageKind {
    MissingAfterInitialization,
    MissingCheckout,
    NotGitRepository,
    MissingManifest,
    InvalidManifest,
    UnsupportedSchema,
    ConfiguredPathMismatch,
    NotSubmodule,
    GitInspectionFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionRepositoryDamage {
    pub kind: InstructionRepositoryDamageKind,
    pub detail: String,
    pub git_head_recovery_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "damage")]
pub enum InstructionRepositoryHealth {
    Uninitialized,
    Ready,
    Damaged(InstructionRepositoryDamage),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitDelta {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionRepositoryChange {
    pub path: PathBuf,
    pub original_path: Option<PathBuf>,
    pub index: Option<GitDelta>,
    pub worktree: Option<GitDelta>,
    pub conflicted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionRepositoryUpstream {
    pub reference: String,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParentGitlinkState {
    pub path: PathBuf,
    pub gitmodules_changed: bool,
    pub gitlink_changed: bool,
    pub recorded_commit: Option<String>,
    pub checked_out_commit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionMutationLeaseInfo {
    pub operation_id: String,
    pub pid: u32,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionRepositoryState {
    pub health: InstructionRepositoryHealth,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub upstream: Option<InstructionRepositoryUpstream>,
    pub changes: Vec<InstructionRepositoryChange>,
    pub conflicts: Vec<PathBuf>,
    pub parent_gitlink: Option<ParentGitlinkState>,
    pub active_mutation: Option<InstructionMutationLeaseInfo>,
    pub configuration_warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionRepositoryValidation {
    pub manifest: InstructionStoreManifest,
    pub resources: Vec<InstructionResourceSummary>,
    pub diagnostics: Vec<InstructionDiagnostic>,
}

impl InstructionRepositoryValidation {
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
            && self.resources.iter().all(|resource| {
                matches!(resource.state, super::super::ResourceValidationState::Valid)
            })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionStoreManifest {
    pub schema_version: u32,
    #[serde(default = "initial_instruction_store_seed_version")]
    pub seed_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_imports: BTreeMap<String, LegacyImportReceipt>,
}

impl InstructionStoreManifest {
    pub fn current() -> Self {
        Self {
            schema_version: INSTRUCTION_STORE_SCHEMA_VERSION,
            seed_version: INSTRUCTION_STORE_SEED_VERSION,
            default_agent: None,
            legacy_imports: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyImportReceipt {
    pub source_kind: LegacyInstructionSourceKind,
    pub source_path: PathBuf,
    pub source_sha256: String,
    pub target_path: PathBuf,
    pub target: String,
    pub source_was_empty: bool,
    pub source_was_blank: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyInstructionSourceKind {
    SystemPrompt,
    PromptOverlay,
    PreferredTools,
    SwarmPrompt,
    InventoryApproved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionLegacySourceSnapshot {
    pub scope: InstructionScope,
    pub source_kind: LegacyInstructionSourceKind,
    pub path: PathBuf,
    pub content: Option<String>,
    pub source_sha256: Option<String>,
    pub source_was_empty: Option<bool>,
    pub source_was_blank: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionSeedFile {
    pub relative_path: PathBuf,
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionStoreSeed {
    pub manifest: InstructionStoreManifest,
    pub files: Vec<InstructionSeedFile>,
}

impl InstructionStoreSeed {
    pub fn empty() -> Self {
        Self {
            manifest: InstructionStoreManifest::current(),
            files: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionStoreInitialization {
    pub repository: InstructionRepositoryRef,
    pub commit: String,
    pub created: bool,
    pub imported: Vec<LegacyImportReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionStoreRecreation {
    pub initialization: InstructionStoreInitialization,
    pub damaged_backup: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionFileState {
    pub relative_path: PathBuf,
    pub fingerprint: InstructionTargetFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionTargetFingerprint {
    Missing,
    File { sha256: String, bytes: u64 },
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionReadPolicy {
    WorkingTreeOnly,
    AllowHeadFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionFileSource {
    WorkingTree,
    GitHead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionFileContent {
    pub relative_path: PathBuf,
    pub source: InstructionFileSource,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionDraft {
    pub draft_id: String,
    pub repository: InstructionRepositoryRef,
    pub relative_path: PathBuf,
    pub base: InstructionFileState,
    pub base_head: String,
    pub content: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionFileMutation {
    Write {
        relative_path: PathBuf,
        content: Vec<u8>,
    },
    Delete {
        relative_path: PathBuf,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
}

impl InstructionFileMutation {
    pub fn affected_paths(&self) -> Vec<&PathBuf> {
        match self {
            Self::Write { relative_path, .. } | Self::Delete { relative_path } => {
                vec![relative_path]
            }
            Self::Rename { from, to } => vec![from, to],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionCommitRequest {
    pub operation_id: String,
    pub message: String,
    pub expected_head: String,
    pub expected_files: Vec<InstructionFileState>,
    pub mutations: Vec<InstructionFileMutation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionCommitDisposition {
    Created,
    AlreadyCommitted,
    NoChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionCommitOutcome {
    pub disposition: InstructionCommitDisposition,
    pub commit: String,
    pub changed_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionHistoryEntry {
    pub commit: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub subject: String,
    pub changed_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionRevisionContent {
    pub commit: String,
    pub relative_path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionRevisionComparison {
    pub from: String,
    pub to: String,
    pub relative_path: Option<PathBuf>,
    pub patch: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionPullStrategy {
    FastForwardOnly,
    Merge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionGitOperationOutcome {
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub branch: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionProjectConfig {
    pub schema_version: u32,
    pub repository: InstructionProjectRepositoryMode,
}

impl InstructionProjectConfig {
    pub fn new(repository: InstructionProjectRepositoryMode) -> Self {
        Self {
            schema_version: INSTRUCTION_PROJECT_CONFIG_SCHEMA_VERSION,
            repository,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "mode")]
pub enum InstructionProjectRepositoryMode {
    Submodule {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    ExternalRemote {
        url: String,
        branch: String,
    },
    ExternalLocal {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    Standalone {
        path: PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionLegacyImportTarget {
    pub relative_path: PathBuf,
    pub id: InstructionId,
    pub kind: InstructionKind,
    pub scope: InstructionScope,
    pub template_mode: TemplateMode,
    pub metadata: InstructionMetadata,
}

impl InstructionLegacyImportTarget {
    pub fn resource(&self) -> InstructionResourceRef {
        InstructionResourceRef {
            scope: self.scope,
            kind: self.kind,
            id: self.id.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionLegacyImportSpec {
    pub import_id: String,
    pub source_kind: LegacyInstructionSourceKind,
    pub source_path: PathBuf,
    pub target: InstructionLegacyImportTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionLegacyImportPlan {
    pub spec: InstructionLegacyImportSpec,
    pub source_content: String,
    pub source_sha256: String,
    pub source_was_empty: bool,
    pub source_was_blank: bool,
    pub managed_content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionLegacyImportOutcome {
    Imported {
        receipt: LegacyImportReceipt,
        commit: String,
        working_changes_preserved: Vec<PathBuf>,
    },
    AlreadyImported {
        receipt: LegacyImportReceipt,
        commit: String,
        working_changes_preserved: Vec<PathBuf>,
    },
    SourceAbsent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionRepositoryErrorKind {
    Configuration,
    RepositoryUnavailable,
    RepositoryDamaged,
    DetachedHead,
    DirtyWorkingTree,
    Conflict,
    MutationBusy,
    StaleDraft,
    InvalidPath,
    SymlinkEscape,
    InvalidUtf8,
    InvalidManifest,
    GitCommand,
    Io,
    LegacyImport,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionRepositoryError {
    pub kind: InstructionRepositoryErrorKind,
    pub operation: String,
    pub repository_id: Option<String>,
    pub path: Option<PathBuf>,
    pub detail: String,
    pub existing_state_unchanged: bool,
}

impl InstructionRepositoryError {
    pub(crate) fn new(
        kind: InstructionRepositoryErrorKind,
        operation: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation: operation.into(),
            repository_id: None,
            path: None,
            detail: detail.into(),
            existing_state_unchanged: true,
        }
    }

    pub(crate) fn repository(mut self, repository: &InstructionRepositoryRef) -> Self {
        self.repository_id = Some(repository.id.clone());
        self
    }

    pub(crate) fn path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub(crate) fn may_have_working_changes(mut self) -> Self {
        self.existing_state_unchanged = false;
        self
    }
}

impl fmt::Display for InstructionRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "instruction repository {} failed",
            self.operation
        )?;
        if let Some(repository_id) = &self.repository_id {
            write!(formatter, " for {repository_id}")?;
        }
        if let Some(path) = &self.path {
            write!(formatter, " at {}", path.display())?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl std::error::Error for InstructionRepositoryError {}

pub type InstructionRepositoryResult<T> = Result<T, InstructionRepositoryError>;
