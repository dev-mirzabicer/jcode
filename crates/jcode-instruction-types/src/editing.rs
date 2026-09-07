//! Explicit instruction editing intent. Inspection remains a separate protocol.
use crate::InstructionInspectionTarget;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionEditScope {
    Global,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstructionEditKind {
    System,
    Agent,
    AgentAddendum,
    Module,
    Notification,
    ToolGuidance,
    Skill,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionEditTemplate {
    #[default]
    Plain,
    Handlebars,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionEditAvailability {
    Primary,
    Isolated,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionResourceFields {
    pub id: String,
    pub kind: InstructionEditKind,
    pub name: Option<String>,
    pub description: Option<String>,
    pub template: InstructionEditTemplate,
    pub availability: Option<InstructionEditAvailability>,
    pub target: Option<String>,
    pub includes: Vec<String>,
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRosterFields {
    pub alias: String,
    pub description: String,
    pub candidates: Vec<String>,
    pub effort: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "fields", rename_all = "snake_case")]
pub enum InstructionEditMetadata {
    Resource(InstructionResourceFields),
    Roster(Vec<InstructionRosterFields>),
    StoreSettings {
        default_agent: Option<String>,
    },
    /// Explicit recovery for invalid frontmatter/configuration. Normal prose
    /// editing uses Body and typed metadata, not an opaque configuration form.
    Damaged {
        detail: String,
    },
    Ecosystem,
    Binary {
        bytes: usize,
        sha256: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditFile {
    pub executable: bool,
    pub key: String,
    pub path: String,
    pub body: String,
    pub metadata: InstructionEditMetadata,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditDraft {
    pub id: String,
    pub generation: u64,
    pub title: String,
    pub scope: InstructionEditScope,
    pub repository: String,
    pub branch: Option<String>,
    pub files: Vec<InstructionEditFile>,
    pub subject: String,
    pub warnings: Vec<String>,
    pub reviewed: bool,
    pub save_started: bool,
    pub committed: Option<String>,
    pub choices: InstructionEditChoices,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditChoices {
    pub agents: Vec<String>,
    pub modules: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InstructionEditAction {
    Edit,
    Create {
        scope: InstructionEditScope,
        fields: InstructionResourceFields,
    },
    RedefineInProject,
    Addendum {
        id: String,
    },
    Clear,
    Delete,
    Rename {
        id: String,
    },
    Restore {
        revision: String,
    },
    CommitExternal,
    CopySkill {
        scope: InstructionEditScope,
        #[serde(default)]
        destination_id: Option<String>,
    },
    Settings {
        scope: InstructionEditScope,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum InstructionDraftChange {
    Body {
        file: String,
        body: String,
    },
    Metadata {
        file: String,
        metadata: InstructionEditMetadata,
    },
    RepairSource {
        file: String,
        source: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum InstructionManagementRequest {
    Recoveries,
    CompareDraft {
        draft: String,
        generation: u64,
    },
    ReconcileDraft {
        draft: String,
        generation: u64,
        comparison: String,
        use_working_content: bool,
    },
    RepositoryChoices {
        scope: InstructionEditScope,
    },
    PlanRepository {
        scope: InstructionEditScope,
        action: super::InstructionRepositoryAction,
    },
    ApplyRepository {
        operation_id: String,
    },
    RepositoryReceipt {
        operation_id: String,
    },
    Begin {
        snapshot: String,
        target: InstructionInspectionTarget,
        action: InstructionEditAction,
    },
    Update {
        draft: String,
        generation: u64,
        change: InstructionDraftChange,
    },
    Review {
        draft: String,
        generation: u64,
    },
    Save {
        draft: String,
        generation: u64,
    },
    Resume {
        scope: InstructionEditScope,
        draft: String,
    },
    Discard {
        draft: String,
        generation: u64,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditReview {
    pub draft: InstructionEditDraft,
    /// Complete exact file versions let the client render a unified diff using
    /// its existing diff library without replacing source with a preview.
    pub files: Vec<InstructionEditComparison>,
    pub errors: Vec<String>,
    pub previews: Vec<InstructionEditPreview>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditComparison {
    pub working_executable: bool,
    pub committed_executable: bool,
    pub proposed_executable: bool,
    pub path: String,
    pub working: Option<String>,
    pub committed: Option<String>,
    pub proposed: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionEditPreview {
    pub title: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionManagementFailure {
    pub operation: String,
    pub detail: String,
    pub draft: Option<String>,
    pub source_unchanged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum InstructionManagementResult {
    Recoveries(super::InstructionRecoveryList),
    DraftConflict(super::InstructionDraftConflict),
    RepositoryChoices(super::InstructionRepositoryChoices),
    RepositoryPlan(super::InstructionRepositoryPlan),
    RepositoryReceipt(super::InstructionRepositoryReceipt),
    Draft(InstructionEditDraft),
    Reviewed(InstructionEditReview),
    Saved {
        draft: String,
        commit: String,
        no_change: bool,
        recovered: bool,
        paths: Vec<String>,
    },
    Closed,
    Discarded,
    Failed(InstructionManagementFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionManagementReply {
    pub session_id: String,
    pub result: InstructionManagementResult,
}
