use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantCarryChoice {
    pub review: ReviewId,
    pub carry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewContextKind {
    Split,
    Clear,
    Transfer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrantCarryReview {
    pub id: ReviewId,
    pub source: String,
    pub session_revision: Revision,
    pub catalog_revision: Revision,
    pub direct_grants: Vec<GrantDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextScopeStatus {
    pub review: Option<ReviewId>,
    pub operation: OperationId,
    pub source: String,
    pub target: String,
    pub kind: NewContextKind,
    pub state: ContextScopeState,
    pub revision: Option<Revision>,
    pub backup_pending: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScopeState {
    Pending,
    Complete,
    Failed,
    RecoveryRequired,
}

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
    BindImported {
        reference: OperationId,
        installation: InstallationId,
        grant: GrantId,
        audience: Audience,
        target: WriteTarget,
    },
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
    #[serde(default)]
    pub excluded_roots: Vec<Location>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDecision {
    Decline,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportedGrantReference {
    pub reference: OperationId,
    pub installation: InstallationId,
    pub grant: GrantDefinition,
    pub bound_grant: Option<GrantId>,
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
pub struct ScopeObservation {
    pub location_revision: Revision,
    pub installation: InstallationId,
    pub catalog_revision: Revision,
    pub fingerprint: String,
    pub ordinary_roots: usize,
    pub additional_roots: usize,
    pub inactive_roots: usize,
    pub explicit_grants: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionRequest {
    DecideProposal {
        request: RequestId,
        proposal: ProposalId,
        expected_revision: Revision,
        decision: ProposalDecision,
    },
    ImportedGrants {
        after: Option<Cursor>,
        limit: u32,
    },
    AbandonContextScope {
        session: String,
    },
    ContextScopeStatus {
        review: ReviewId,
    },
    ReconcileContextScope {
        session: String,
    },
    ReviewCarry {
        session: String,
    },
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
    ImportedGrants {
        revision: Revision,
        total: u64,
        items: Vec<ImportedGrantReference>,
        next: Option<Cursor>,
    },
    ContextScopes(Vec<ContextScopeStatus>),
    CarryReview(GrantCarryReview),
    Scope(SessionWriteScope),
    Review(GrantReview),
    Mutation(PermissionMutation),
    Grant(GrantDefinition),
    Proposal(AccessProposal),
    Page(PermissionPage),
}
