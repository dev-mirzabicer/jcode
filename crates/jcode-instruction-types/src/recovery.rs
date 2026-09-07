use crate::InstructionEditScope;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRecoveryList {
    pub drafts: Vec<InstructionRetainedDraft>,
    pub operations: Vec<InstructionRetainedOperation>,
    pub errors: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRetainedDraft {
    pub id: String,
    pub scope: InstructionEditScope,
    pub subject: String,
    pub generation: u64,
    pub save_started: bool,
    pub committed: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRetainedOperation {
    pub id: String,
    pub title: String,
    pub started: bool,
    pub completed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionDraftConflict {
    pub draft: String,
    pub generation: u64,
    pub comparison: String,
    pub head: String,
    pub branch: Option<String>,
    pub files: Vec<InstructionConflictFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionConflictFile {
    pub path: String,
    pub base: Option<String>,
    pub working: Option<String>,
    pub proposed: Option<String>,
}
