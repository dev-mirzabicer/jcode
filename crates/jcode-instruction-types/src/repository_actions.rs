//! Reviewed repository operations. Network and setup actions are always explicit.
use super::InstructionEditScope;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InstructionRepositoryAction {
    InitializeGlobal,
    RecreateGlobal,
    Submodule {
        path: String,
        url: String,
        branch: String,
    },
    CloneExternal {
        url: String,
        branch: String,
    },
    AttachExternal {
        path: String,
        branch: Option<String>,
    },
    Standalone {
        path: String,
    },
    RepairCheckout,
    ConfigureRemote {
        name: String,
        url: String,
    },
    Checkout {
        branch: String,
        create: bool,
        start: Option<String>,
    },
    Fetch {
        remote: String,
    },
    Pull {
        remote: String,
        branch: String,
    },
    Push {
        remote: String,
        branch: String,
    },
    FetchCheckout {
        remote: String,
        branch: String,
        local_branch: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRepositoryChoices {
    pub scope: InstructionEditScope,
    pub project_is_git: bool,
    pub git_available: bool,
    pub can_initialize: bool,
    pub can_recreate: bool,
    pub root: Option<String>,
    pub branches: Vec<String>,
    pub remote_branches: Vec<String>,
    pub remotes: Vec<InstructionRemoteChoice>,
    pub configured_branch: Option<String>,
    pub current_branch: Option<String>,
    pub health: String,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRemoteChoice {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRepositoryPlan {
    pub id: String,
    pub scope: InstructionEditScope,
    pub action: InstructionRepositoryAction,
    pub title: String,
    pub detail: String,
    pub network: bool,
    pub outgoing_commits: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRepositoryReceipt {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub completed: bool,
    pub running: bool,
    pub outcome_uncertain: bool,
    pub source_unchanged: bool,
}
