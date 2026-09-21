use super::*;

/// Audit provenance from the authenticated client adapter, never model input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantAuthorization {
    pub client: String,
    pub request: RequestId,
    pub installation: InstallationId,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessProposalState {
    #[default]
    Pending,
    Approved,
    Declined,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantChange {
    Issue {
        audience: Audience,
        target: WriteTarget,
        proposal: Option<ProposalId>,
    },
    ActivateImported {
        grant: GrantId,
    },
    Revoke {
        grant: GrantId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrantReview {
    pub id: ReviewId,
    pub revision: Revision,
    pub change: GrantChange,
    pub grant: GrantDefinition,
    /// Current roots observed in this review. Member targets include future members too.
    pub roots: Vec<Location>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionMutation {
    pub receipt: Receipt,
    pub grant: Option<GrantDefinition>,
    pub proposal: Option<AccessProposal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WritableRoot {
    pub location: Location,
    pub ordinary: bool,
    pub grants: Vec<GrantId>,
    /// An otherwise included root may be offline, closing, retired or replaced.
    pub issue: Option<Issue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionWriteScope {
    pub session: String,
    pub placement: Placement,
    pub session_revision: Revision,
    pub catalog_revision: Revision,
    pub roots: Vec<WritableRoot>,
    pub grants: Vec<GrantDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionRequest {
    Scope {
        session: String,
    },
    Review {
        expected_revision: Revision,
        change: GrantChange,
    },
    Apply {
        request: RequestId,
        review: ReviewId,
    },
    Grant {
        grant: GrantId,
    },
    Propose {
        session: String,
        request: RequestId,
        target: WriteTarget,
        reason: String,
    },
    Proposal {
        proposal: ProposalId,
    },
    List {
        query: PermissionQuery,
        after: Option<Cursor>,
        limit: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionQuery {
    Grants {
        audience: Option<Audience>,
    },
    Proposals {
        session: Option<String>,
        state: Option<AccessProposalState>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PermissionItem {
    Grant(GrantDefinition),
    Proposal(AccessProposal),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionPage {
    pub revision: Revision,
    pub total: u64,
    pub items: Vec<PermissionItem>,
    pub next: Option<Cursor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PermissionResponse {
    Scope(SessionWriteScope),
    Review(GrantReview),
    Mutation(PermissionMutation),
    Grant(GrantDefinition),
    Proposal(AccessProposal),
    Page(PermissionPage),
}
