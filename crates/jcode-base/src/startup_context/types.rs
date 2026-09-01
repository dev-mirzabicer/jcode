use chrono::{DateTime, Utc};
use jcode_session_types::{
    StoredStartupExternalApproval, StoredStartupFileIssue, StoredStartupFileIssueKind,
    StoredStartupFileSpec, StoredStartupPathClassification, StoredStartupProjectIdentity,
    StoredStartupSelectedPath, StoredStartupTargetType, StoredStartupUnsupportedContent,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

pub const STARTUP_PROJECT_PLAN_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_STARTUP_PLAN_ENTRIES: usize = 1_024;
pub const DEFAULT_MAX_STARTUP_BATCH_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const DEFAULT_MAX_CAPTURE_ATTEMPTS: usize = 2;

/// Current supported disk state for one immutable startup snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupObservedState {
    Current,
    Changed { sha256: String, bytes: u64 },
    Missing,
    Unreadable,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProjectKey {
    Git { canonical_common_dir: PathBuf },
    Directory { canonical_root: PathBuf },
}

impl ProjectKey {
    pub fn canonical_identity_path(&self) -> &Path {
        match self {
            Self::Git {
                canonical_common_dir,
            } => canonical_common_dir,
            Self::Directory { canonical_root } => canonical_root,
        }
    }

    pub fn is_git(&self) -> bool {
        matches!(self, Self::Git { .. })
    }

    pub(super) fn stable_bytes(&self) -> Vec<u8> {
        let (kind, path) = match self {
            Self::Git {
                canonical_common_dir,
            } => (b"git".as_slice(), canonical_common_dir),
            Self::Directory { canonical_root } => (b"directory".as_slice(), canonical_root),
        };
        let mut bytes = Vec::with_capacity(kind.len() + 1 + path.as_os_str().len());
        bytes.extend_from_slice(kind);
        bytes.push(0);
        bytes.extend_from_slice(path.to_string_lossy().as_bytes());
        bytes
    }

    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(self.stable_bytes()))
    }

    pub(crate) fn to_stored(&self) -> Result<StoredStartupProjectIdentity, StartupContextError> {
        match self {
            Self::Git {
                canonical_common_dir,
            } => Ok(StoredStartupProjectIdentity::Git {
                canonical_common_dir: utf8_path(canonical_common_dir, "Git common directory")?,
            }),
            Self::Directory { canonical_root } => Ok(StoredStartupProjectIdentity::Directory {
                canonical_root: utf8_path(canonical_root, "project root")?,
            }),
        }
    }

    pub(super) fn from_stored(
        stored: StoredStartupProjectIdentity,
    ) -> Result<Self, StartupContextError> {
        let key = match stored {
            StoredStartupProjectIdentity::Git {
                canonical_common_dir,
            } => Self::Git {
                canonical_common_dir: PathBuf::from(canonical_common_dir),
            },
            StoredStartupProjectIdentity::Directory { canonical_root } => Self::Directory {
                canonical_root: PathBuf::from(canonical_root),
            },
        };
        validate_absolute_utf8_path(key.canonical_identity_path(), "stored project identity")?;
        Ok(key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveProject {
    key: ProjectKey,
    active_root: PathBuf,
}

impl ActiveProject {
    pub(super) fn new(key: ProjectKey, active_root: PathBuf) -> Self {
        Self { key, active_root }
    }

    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    pub fn active_root(&self) -> &Path {
        &self.active_root
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StartupFileSpecId(String);

impl StartupFileSpecId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, StartupContextError> {
        Self::from_stored(value.into())
    }

    pub(super) fn from_stored(value: String) -> Result<Self, StartupContextError> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StartupContextError::InvalidStoredPlan {
                detail: "startup file spec id must be exactly 64 hexadecimal characters"
                    .to_string(),
            });
        }
        Ok(Self(value))
    }

    pub(super) fn from_digest(value: String) -> Self {
        debug_assert!(!value.is_empty());
        Self(value)
    }
}

impl fmt::Display for StartupFileSpecId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SelectedStartupPath {
    ProjectRelative(PathBuf),
    ExternalAbsolute(PathBuf),
}

impl SelectedStartupPath {
    pub fn as_path(&self) -> &Path {
        match self {
            Self::ProjectRelative(path) | Self::ExternalAbsolute(path) => path,
        }
    }

    pub fn is_project_relative(&self) -> bool {
        matches!(self, Self::ProjectRelative(_))
    }

    pub(super) fn to_stored(&self) -> Result<StoredStartupSelectedPath, StartupContextError> {
        match self {
            Self::ProjectRelative(path) => Ok(StoredStartupSelectedPath::ProjectRelative {
                path: utf8_path(path, "project-relative startup path")?,
            }),
            Self::ExternalAbsolute(path) => Ok(StoredStartupSelectedPath::ExternalAbsolute {
                path: utf8_path(path, "external startup path")?,
            }),
        }
    }

    pub(super) fn from_stored(
        stored: StoredStartupSelectedPath,
    ) -> Result<Self, StartupContextError> {
        match stored {
            StoredStartupSelectedPath::ProjectRelative { path } => {
                let path = PathBuf::from(path);
                validate_relative_selected_path(&path).map_err(|detail| {
                    StartupContextError::InvalidStoredPlan {
                        detail: format!("invalid project-relative startup path: {detail}"),
                    }
                })?;
                Ok(Self::ProjectRelative(path))
            }
            StoredStartupSelectedPath::ExternalAbsolute { path } => {
                let path = PathBuf::from(path);
                validate_absolute_utf8_path(&path, "stored external startup path")?;
                reject_parent_components(&path).map_err(|detail| {
                    StartupContextError::InvalidStoredPlan {
                        detail: format!("invalid external startup path: {detail}"),
                    }
                })?;
                Ok(Self::ExternalAbsolute(path))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExternalTargetApproval {
    approved_resolved_target: PathBuf,
}

impl ExternalTargetApproval {
    pub fn approved_resolved_target(&self) -> &Path {
        &self.approved_resolved_target
    }

    pub(super) fn new(approved_resolved_target: PathBuf) -> Self {
        Self {
            approved_resolved_target,
        }
    }

    pub(super) fn to_stored(&self) -> Result<StoredStartupExternalApproval, StartupContextError> {
        Ok(StoredStartupExternalApproval {
            approved_resolved_target: utf8_path(
                &self.approved_resolved_target,
                "approved external startup target",
            )?,
        })
    }

    pub(super) fn from_stored(
        stored: StoredStartupExternalApproval,
    ) -> Result<Self, StartupContextError> {
        let path = PathBuf::from(stored.approved_resolved_target);
        validate_absolute_utf8_path(&path, "stored external approval target")?;
        reject_parent_components(&path).map_err(|detail| {
            StartupContextError::InvalidStoredPlan {
                detail: format!("invalid stored external approval target: {detail}"),
            }
        })?;
        Ok(Self::new(path))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StartupFileSpec {
    id: StartupFileSpecId,
    path: SelectedStartupPath,
    external_approval: Option<ExternalTargetApproval>,
}

impl StartupFileSpec {
    pub fn id(&self) -> &StartupFileSpecId {
        &self.id
    }

    pub fn path(&self) -> &SelectedStartupPath {
        &self.path
    }

    pub fn external_approval(&self) -> Option<&ExternalTargetApproval> {
        self.external_approval.as_ref()
    }

    pub(super) fn new(
        id: StartupFileSpecId,
        path: SelectedStartupPath,
        external_approval: Option<ExternalTargetApproval>,
    ) -> Self {
        Self {
            id,
            path,
            external_approval,
        }
    }

    pub(super) fn to_stored(&self) -> Result<StoredStartupFileSpec, StartupContextError> {
        Ok(StoredStartupFileSpec {
            id: self.id.0.clone(),
            path: self.path.to_stored()?,
            external_approval: self
                .external_approval
                .as_ref()
                .map(ExternalTargetApproval::to_stored)
                .transpose()?,
        })
    }

    pub(super) fn from_stored(stored: StoredStartupFileSpec) -> Result<Self, StartupContextError> {
        let path = SelectedStartupPath::from_stored(stored.path)?;
        let external_approval = stored
            .external_approval
            .map(ExternalTargetApproval::from_stored)
            .transpose()?;
        if matches!(path, SelectedStartupPath::ExternalAbsolute(_)) && external_approval.is_none() {
            return Err(StartupContextError::InvalidStoredPlan {
                detail: "external startup path is missing its approved resolved target".to_string(),
            });
        }
        Ok(Self::new(
            StartupFileSpecId::from_stored(stored.id)?,
            path,
            external_approval,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupSelectionInput {
    existing_id: Option<StartupFileSpecId>,
    path: PathBuf,
    approved_external_target: Option<PathBuf>,
}

impl StartupSelectionInput {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            existing_id: None,
            path: path.into(),
            approved_external_target: None,
        }
    }

    pub fn existing(id: StartupFileSpecId, path: impl Into<PathBuf>) -> Self {
        Self {
            existing_id: Some(id),
            path: path.into(),
            approved_external_target: None,
        }
    }

    pub fn with_external_approval(mut self, target: impl Into<PathBuf>) -> Self {
        self.approved_external_target = Some(target.into());
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn existing_id(&self) -> Option<&StartupFileSpecId> {
        self.existing_id.as_ref()
    }

    pub fn approved_external_target(&self) -> Option<&Path> {
        self.approved_external_target.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartupPathClassification {
    Project,
    External,
}

impl StartupPathClassification {
    pub(crate) fn to_stored(self) -> StoredStartupPathClassification {
        match self {
            Self::Project => StoredStartupPathClassification::Project,
            Self::External => StoredStartupPathClassification::External,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedStartupSelection {
    input_index: usize,
    spec: StartupFileSpec,
    resolved_path: PathBuf,
    classification: StartupPathClassification,
    current_bytes: u64,
}

impl ValidatedStartupSelection {
    pub(super) fn new(
        input_index: usize,
        spec: StartupFileSpec,
        resolved_path: PathBuf,
        classification: StartupPathClassification,
        current_bytes: u64,
    ) -> Self {
        Self {
            input_index,
            spec,
            resolved_path,
            classification,
            current_bytes,
        }
    }

    pub fn input_index(&self) -> usize {
        self.input_index
    }

    pub fn spec(&self) -> &StartupFileSpec {
        &self.spec
    }

    pub fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    pub fn classification(&self) -> StartupPathClassification {
        self.classification
    }

    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupSelectionEntry {
    Selected(ValidatedStartupSelection),
    Issue(StartupFileIssue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupSelectionPreview {
    project_key: ProjectKey,
    entries: Vec<StartupSelectionEntry>,
    batch_issues: Vec<StartupFileIssue>,
}

impl StartupSelectionPreview {
    pub(super) fn new(
        project_key: ProjectKey,
        entries: Vec<StartupSelectionEntry>,
        batch_issues: Vec<StartupFileIssue>,
    ) -> Self {
        Self {
            project_key,
            entries,
            batch_issues,
        }
    }

    pub fn project_key(&self) -> &ProjectKey {
        &self.project_key
    }

    pub fn entries(&self) -> &[StartupSelectionEntry] {
        &self.entries
    }

    pub fn batch_issues(&self) -> &[StartupFileIssue] {
        &self.batch_issues
    }

    pub fn issue_count(&self) -> usize {
        self.batch_issues.len()
            + self
                .entries
                .iter()
                .filter(|entry| matches!(entry, StartupSelectionEntry::Issue(_)))
                .count()
    }

    pub fn is_valid(&self) -> bool {
        self.issue_count() == 0
    }

    pub fn selected(&self) -> impl Iterator<Item = &ValidatedStartupSelection> {
        self.entries.iter().filter_map(|entry| match entry {
            StartupSelectionEntry::Selected(selected) => Some(selected),
            StartupSelectionEntry::Issue(_) => None,
        })
    }

    pub fn issues(&self) -> impl Iterator<Item = &StartupFileIssue> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                StartupSelectionEntry::Selected(_) => None,
                StartupSelectionEntry::Issue(issue) => Some(issue),
            })
            .chain(self.batch_issues.iter())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupProjectPlan {
    project_key: ProjectKey,
    revision: u64,
    entries: Vec<StartupFileSpec>,
    updated_at: Option<DateTime<Utc>>,
}

impl StartupProjectPlan {
    pub(super) fn empty(project_key: ProjectKey) -> Self {
        Self {
            project_key,
            revision: 0,
            entries: Vec::new(),
            updated_at: None,
        }
    }

    pub(super) fn stored(
        project_key: ProjectKey,
        revision: u64,
        entries: Vec<StartupFileSpec>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            project_key,
            revision,
            entries,
            updated_at: Some(updated_at),
        }
    }

    pub fn project_key(&self) -> &ProjectKey {
        &self.project_key
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn entries(&self) -> &[StartupFileSpec] {
        &self.entries
    }

    pub fn updated_at(&self) -> Option<DateTime<Utc>> {
        self.updated_at
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPlanLoadSource {
    Missing,
    Primary,
    RecoveredBackup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedStartupProjectPlan {
    plan: StartupProjectPlan,
    source: StartupPlanLoadSource,
}

impl LoadedStartupProjectPlan {
    pub(super) fn new(plan: StartupProjectPlan, source: StartupPlanLoadSource) -> Self {
        Self { plan, source }
    }

    pub fn plan(&self) -> &StartupProjectPlan {
        &self.plan
    }

    pub fn into_plan(self) -> StartupProjectPlan {
        self.plan
    }

    pub fn source(&self) -> StartupPlanLoadSource {
        self.source
    }
}

/// Opaque, durable description of one validated project-plan transition.
///
/// The transition contains path specifications and revisions only. It never
/// contains captured file content, so app-core can persist it in an apply
/// recovery record without duplicating source text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupProjectPlanTransition {
    project: StoredStartupProjectIdentity,
    previous_revision: u64,
    proposed_revision: u64,
    previous_entries: Vec<StoredStartupFileSpec>,
    proposed_entries: Vec<StoredStartupFileSpec>,
    updated_at: DateTime<Utc>,
}

impl StartupProjectPlanTransition {
    pub(super) fn new(
        project: StoredStartupProjectIdentity,
        previous_revision: u64,
        proposed_revision: u64,
        previous_entries: Vec<StoredStartupFileSpec>,
        proposed_entries: Vec<StoredStartupFileSpec>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            project,
            previous_revision,
            proposed_revision,
            previous_entries,
            proposed_entries,
            updated_at,
        }
    }

    pub fn previous_revision(&self) -> u64 {
        self.previous_revision
    }

    pub fn proposed_revision(&self) -> u64 {
        self.proposed_revision
    }

    pub fn changes_plan(&self) -> bool {
        self.previous_revision != self.proposed_revision
            || self.previous_entries != self.proposed_entries
    }

    pub(super) fn project(&self) -> &StoredStartupProjectIdentity {
        &self.project
    }

    pub(super) fn previous_entries(&self) -> &[StoredStartupFileSpec] {
        &self.previous_entries
    }

    pub(super) fn proposed_entries(&self) -> &[StoredStartupFileSpec] {
        &self.proposed_entries
    }

    pub(super) fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupProjectPlanCommitOutcome {
    Unchanged,
    Applied,
    AlreadyApplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupFailurePolicy {
    Block,
    InjectDiagnostic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupTargetType {
    Directory,
    SymlinkToDirectory,
    DeviceOrSpecial,
}

impl StartupTargetType {
    fn to_stored(self) -> StoredStartupTargetType {
        match self {
            Self::Directory => StoredStartupTargetType::Directory,
            Self::SymlinkToDirectory => StoredStartupTargetType::SymlinkToDirectory,
            Self::DeviceOrSpecial => StoredStartupTargetType::DeviceOrSpecial,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupUnsupportedContent {
    Binary,
    Pdf,
    Image,
}

impl StartupUnsupportedContent {
    fn to_stored(self) -> StoredStartupUnsupportedContent {
        match self {
            Self::Binary => StoredStartupUnsupportedContent::Binary,
            Self::Pdf => StoredStartupUnsupportedContent::Pdf,
            Self::Image => StoredStartupUnsupportedContent::Image,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupFileIssueKind {
    EmptyPath,
    InvalidPathEncoding,
    PathTraversal,
    Missing,
    BrokenSymlink,
    Unreadable {
        detail: String,
    },
    UnsupportedTarget {
        target_type: StartupTargetType,
    },
    UnsupportedContent {
        content: StartupUnsupportedContent,
    },
    NonUtf8,
    ExternalApprovalRequired {
        resolved_target: PathBuf,
    },
    ExternalTargetChanged {
        approved_target: PathBuf,
        resolved_target: PathBuf,
    },
    InvalidExternalApproval {
        detail: String,
    },
    DuplicateSelection {
        first_input_index: usize,
    },
    TooManyEntries {
        count: usize,
        limit: usize,
    },
    FileTooLarge {
        bytes: u64,
        limit: u64,
    },
    BatchTooLarge {
        bytes: u64,
        limit: u64,
    },
    ChangedDuringCapture,
    DirectoryOutsideProject,
    DirectoryReadFailed {
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupFileIssue {
    input_index: Option<usize>,
    spec_id: Option<StartupFileSpecId>,
    logical_path: Option<PathBuf>,
    kind: StartupFileIssueKind,
}

impl StartupFileIssue {
    pub(super) fn for_input(
        input_index: usize,
        path: impl Into<PathBuf>,
        kind: StartupFileIssueKind,
    ) -> Self {
        Self {
            input_index: Some(input_index),
            spec_id: None,
            logical_path: Some(path.into()),
            kind,
        }
    }

    pub(super) fn for_spec(spec: &StartupFileSpec, kind: StartupFileIssueKind) -> Self {
        Self {
            input_index: None,
            spec_id: Some(spec.id.clone()),
            logical_path: Some(spec.path.as_path().to_path_buf()),
            kind,
        }
    }

    pub(super) fn batch(kind: StartupFileIssueKind) -> Self {
        Self {
            input_index: None,
            spec_id: None,
            logical_path: None,
            kind,
        }
    }

    pub(super) fn with_spec_id(mut self, spec_id: StartupFileSpecId) -> Self {
        self.spec_id = Some(spec_id);
        self
    }

    pub fn input_index(&self) -> Option<usize> {
        self.input_index
    }

    pub fn spec_id(&self) -> Option<&StartupFileSpecId> {
        self.spec_id.as_ref()
    }

    pub fn logical_path(&self) -> Option<&Path> {
        self.logical_path.as_deref()
    }

    pub fn kind(&self) -> &StartupFileIssueKind {
        &self.kind
    }

    pub(crate) fn to_stored(&self) -> Result<StoredStartupFileIssue, StartupContextError> {
        let input_index = self
            .input_index
            .map(|value| stored_u32(value, "startup issue input index"))
            .transpose()?;
        let logical_path = self
            .logical_path
            .as_deref()
            .map(|path| utf8_path(path, "startup issue logical path"))
            .transpose()?;
        let kind = match &self.kind {
            StartupFileIssueKind::EmptyPath => StoredStartupFileIssueKind::EmptyPath,
            StartupFileIssueKind::InvalidPathEncoding => {
                StoredStartupFileIssueKind::InvalidPathEncoding
            }
            StartupFileIssueKind::PathTraversal => StoredStartupFileIssueKind::PathTraversal,
            StartupFileIssueKind::Missing => StoredStartupFileIssueKind::Missing,
            StartupFileIssueKind::BrokenSymlink => StoredStartupFileIssueKind::BrokenSymlink,
            StartupFileIssueKind::Unreadable { detail } => StoredStartupFileIssueKind::Unreadable {
                detail: detail.clone(),
            },
            StartupFileIssueKind::UnsupportedTarget { target_type } => {
                StoredStartupFileIssueKind::UnsupportedTarget {
                    target_type: target_type.to_stored(),
                }
            }
            StartupFileIssueKind::UnsupportedContent { content } => {
                StoredStartupFileIssueKind::UnsupportedContent {
                    content: content.to_stored(),
                }
            }
            StartupFileIssueKind::NonUtf8 => StoredStartupFileIssueKind::NonUtf8,
            StartupFileIssueKind::ExternalApprovalRequired { resolved_target } => {
                StoredStartupFileIssueKind::ExternalApprovalRequired {
                    resolved_target: utf8_path(
                        resolved_target,
                        "startup issue external resolved target",
                    )?,
                }
            }
            StartupFileIssueKind::ExternalTargetChanged {
                approved_target,
                resolved_target,
            } => StoredStartupFileIssueKind::ExternalTargetChanged {
                approved_target: utf8_path(
                    approved_target,
                    "startup issue approved external target",
                )?,
                resolved_target: utf8_path(
                    resolved_target,
                    "startup issue changed external target",
                )?,
            },
            StartupFileIssueKind::InvalidExternalApproval { detail } => {
                StoredStartupFileIssueKind::InvalidExternalApproval {
                    detail: detail.clone(),
                }
            }
            StartupFileIssueKind::DuplicateSelection { first_input_index } => {
                StoredStartupFileIssueKind::DuplicateSelection {
                    first_input_index: stored_u32(
                        *first_input_index,
                        "duplicate startup selection index",
                    )?,
                }
            }
            StartupFileIssueKind::TooManyEntries { count, limit } => {
                StoredStartupFileIssueKind::TooManyEntries {
                    count: stored_u32(*count, "startup selection count")?,
                    limit: stored_u32(*limit, "startup selection limit")?,
                }
            }
            StartupFileIssueKind::FileTooLarge { bytes, limit } => {
                StoredStartupFileIssueKind::FileTooLarge {
                    bytes: *bytes,
                    limit: *limit,
                }
            }
            StartupFileIssueKind::BatchTooLarge { bytes, limit } => {
                StoredStartupFileIssueKind::BatchTooLarge {
                    bytes: *bytes,
                    limit: *limit,
                }
            }
            StartupFileIssueKind::ChangedDuringCapture => {
                StoredStartupFileIssueKind::ChangedDuringCapture
            }
            StartupFileIssueKind::DirectoryOutsideProject => {
                StoredStartupFileIssueKind::DirectoryOutsideProject
            }
            StartupFileIssueKind::DirectoryReadFailed { detail } => {
                StoredStartupFileIssueKind::DirectoryReadFailed {
                    detail: detail.clone(),
                }
            }
        };

        Ok(StoredStartupFileIssue {
            input_index,
            spec_id: self.spec_id.as_ref().map(ToString::to_string),
            logical_path,
            kind,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedStartupFile {
    spec_id: StartupFileSpecId,
    logical_path: PathBuf,
    resolved_path: PathBuf,
    classification: StartupPathClassification,
    sha256: String,
    bytes: u64,
    estimated_tokens: u64,
    text: String,
}

impl CapturedStartupFile {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        spec_id: StartupFileSpecId,
        logical_path: PathBuf,
        resolved_path: PathBuf,
        classification: StartupPathClassification,
        sha256: String,
        bytes: u64,
        estimated_tokens: u64,
        text: String,
    ) -> Self {
        Self {
            spec_id,
            logical_path,
            resolved_path,
            classification,
            sha256,
            bytes,
            estimated_tokens,
            text,
        }
    }

    pub fn spec_id(&self) -> &StartupFileSpecId {
        &self.spec_id
    }

    pub fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    pub fn classification(&self) -> StartupPathClassification {
        self.classification
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedStartupEntry {
    Captured(CapturedStartupFile),
    Issue(StartupFileIssue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupPreparation {
    project: ActiveProject,
    plan_revision: u64,
    entries: Vec<PreparedStartupEntry>,
    batch_issues: Vec<StartupFileIssue>,
    captured_bytes: u64,
    estimated_tokens: u64,
}

impl StartupPreparation {
    pub(super) fn new(
        project: ActiveProject,
        plan_revision: u64,
        entries: Vec<PreparedStartupEntry>,
        batch_issues: Vec<StartupFileIssue>,
    ) -> Self {
        let (captured_bytes, estimated_tokens) =
            entries
                .iter()
                .fold((0u64, 0u64), |(bytes, tokens), entry| match entry {
                    PreparedStartupEntry::Captured(file) => (
                        bytes.saturating_add(file.bytes),
                        tokens.saturating_add(file.estimated_tokens),
                    ),
                    PreparedStartupEntry::Issue(_) => (bytes, tokens),
                });
        Self {
            project,
            plan_revision,
            entries,
            batch_issues,
            captured_bytes,
            estimated_tokens,
        }
    }

    pub fn project(&self) -> &ActiveProject {
        &self.project
    }

    pub fn plan_revision(&self) -> u64 {
        self.plan_revision
    }

    pub fn entries(&self) -> &[PreparedStartupEntry] {
        &self.entries
    }

    pub fn batch_issues(&self) -> &[StartupFileIssue] {
        &self.batch_issues
    }

    pub fn captured_files(&self) -> impl Iterator<Item = &CapturedStartupFile> {
        self.entries.iter().filter_map(|entry| match entry {
            PreparedStartupEntry::Captured(file) => Some(file),
            PreparedStartupEntry::Issue(_) => None,
        })
    }

    pub fn issues(&self) -> impl Iterator<Item = &StartupFileIssue> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                PreparedStartupEntry::Captured(_) => None,
                PreparedStartupEntry::Issue(issue) => Some(issue),
            })
            .chain(self.batch_issues.iter())
    }

    pub fn issue_count(&self) -> usize {
        self.issues().count()
    }

    pub fn captured_bytes(&self) -> u64 {
        self.captured_bytes
    }

    pub fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupPreparationOutcome {
    Ready(StartupPreparation),
    Blocked(StartupPreparation),
    Diagnostic(StartupPreparation),
}

impl StartupPreparationOutcome {
    pub(super) fn from_policy(
        preparation: StartupPreparation,
        policy: StartupFailurePolicy,
    ) -> Self {
        if preparation.issue_count() == 0 {
            Self::Ready(preparation)
        } else {
            match policy {
                StartupFailurePolicy::Block => Self::Blocked(preparation),
                StartupFailurePolicy::InjectDiagnostic => Self::Diagnostic(preparation),
            }
        }
    }

    pub fn preparation(&self) -> &StartupPreparation {
        match self {
            Self::Ready(preparation)
            | Self::Blocked(preparation)
            | Self::Diagnostic(preparation) => preparation,
        }
    }

    pub fn into_preparation(self) -> StartupPreparation {
        match self {
            Self::Ready(preparation)
            | Self::Blocked(preparation)
            | Self::Diagnostic(preparation) => preparation,
        }
    }
}

#[derive(Debug)]
pub enum StartupContextError {
    ProjectIdentity { path: PathBuf, detail: String },
    PlanStorage { path: PathBuf, detail: String },
    UnsupportedPlanSchema { path: PathBuf, schema_version: u32 },
    InvalidStoredPlan { detail: String },
    PlanProjectMismatch,
    SelectionProjectMismatch,
    InvalidSelection { issue_count: usize },
    InvalidPlanTransition { detail: String },
    StalePlanRevision { expected: u64, actual: u64 },
    PlanRevisionOverflow,
}

impl fmt::Display for StartupContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectIdentity { path, detail } => {
                write!(
                    formatter,
                    "could not resolve project identity for {}: {detail}",
                    path.display()
                )
            }
            Self::PlanStorage { path, detail } => {
                write!(
                    formatter,
                    "startup-context plan storage failed at {}: {detail}",
                    path.display()
                )
            }
            Self::UnsupportedPlanSchema {
                path,
                schema_version,
            } => write!(
                formatter,
                "startup-context plan at {} uses unsupported schema version {schema_version}",
                path.display()
            ),
            Self::InvalidStoredPlan { detail } => {
                write!(formatter, "invalid stored startup-context plan: {detail}")
            }
            Self::PlanProjectMismatch => {
                formatter.write_str("startup-context plan belongs to a different project")
            }
            Self::SelectionProjectMismatch => {
                formatter.write_str("startup-context selection belongs to a different project")
            }
            Self::InvalidSelection { issue_count } => write!(
                formatter,
                "startup-context selection has {issue_count} unresolved issue(s)"
            ),
            Self::InvalidPlanTransition { detail } => {
                write!(
                    formatter,
                    "invalid startup-context plan transition: {detail}"
                )
            }
            Self::StalePlanRevision { expected, actual } => write!(
                formatter,
                "startup-context plan revision is stale: expected {expected}, current revision is {actual}"
            ),
            Self::PlanRevisionOverflow => {
                formatter.write_str("startup-context plan revision overflowed")
            }
        }
    }
}

impl Error for StartupContextError {}

pub(super) fn utf8_path(path: &Path, label: &str) -> Result<String, StartupContextError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StartupContextError::InvalidStoredPlan {
            detail: format!("{label} is not valid UTF-8: {}", path.display()),
        })
}

fn stored_u32(value: usize, label: &str) -> Result<u32, StartupContextError> {
    u32::try_from(value).map_err(|_| StartupContextError::InvalidStoredPlan {
        detail: format!("{label} exceeds the durable u32 range: {value}"),
    })
}

pub(super) fn validate_absolute_utf8_path(
    path: &Path,
    label: &str,
) -> Result<(), StartupContextError> {
    if !path.is_absolute() {
        return Err(StartupContextError::InvalidStoredPlan {
            detail: format!("{label} is not absolute: {}", path.display()),
        });
    }
    let _ = utf8_path(path, label)?;
    Ok(())
}

pub(super) fn validate_relative_selected_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("path is empty".to_string());
    }
    if path.is_absolute() {
        return Err("path is absolute".to_string());
    }
    reject_parent_components(path)?;
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        return Err("path contains a root or platform prefix".to_string());
    }
    if path.to_str().is_none() {
        return Err("path is not valid UTF-8".to_string());
    }
    Ok(())
}

pub(super) fn reject_parent_components(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("path contains parent traversal".to_string());
    }
    Ok(())
}
