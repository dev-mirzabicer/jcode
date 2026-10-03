//! Structural reply correlation and capability negotiation for public clients.
//!
//! These rules belong to the contract owner so the Harness bridge, both SDKs
//! and later clients apply the same identity checks. A matching reply is not
//! authority; it only proves the reply describes the request that was sent.
use crate::*;
use jcode_tool_types::execution::{ExecutionRequest, ExecutionResponse};

/// Independently negotiated workspace contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCapability {
    /// Organization, receipts, sessions, backup/export/import/restore.
    Catalog,
    /// Grants, access proposals, scope and grant carry.
    Permissions,
    /// Volumes, clones, source trust, rebind and Startup Context copy.
    Checkout,
    /// Checkout closeout. Public clients use its dedicated closeout route.
    Closeout,
    /// Read-only operation discovery and copy-plan revisions.
    Management,
}

/// The versions a runtime advertised in `workspace_capabilities`. Absent
/// fields come from runtimes that predate that contract.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceVersions {
    pub catalog_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closeout_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_version: Option<u32>,
    /// Ordinary managed launch rollout. Staged runtimes report false.
    pub managed_rollout: bool,
}

impl WorkspaceVersions {
    /// Only the exact versions this contract describes are supported.
    pub fn supports(&self, capability: WorkspaceCapability) -> bool {
        match capability {
            WorkspaceCapability::Catalog => self.catalog_version == 1,
            WorkspaceCapability::Permissions => self.permissions_version == Some(1),
            WorkspaceCapability::Checkout => self.checkout_version == Some(1),
            WorkspaceCapability::Closeout => self.closeout_version == Some(2),
            WorkspaceCapability::Management => self.management_version == Some(1),
        }
    }
}

impl WorkspaceRequest {
    /// The contract a client must negotiate before sending this request.
    pub fn required_capability(&self) -> WorkspaceCapability {
        use WorkspaceCapability as C;
        match self {
            Self::Closeout { .. } => C::Closeout,
            Self::Permissions { .. } => C::Permissions,
            Self::Volumes {}
            | Self::CloneOutput { .. }
            | Self::ReviewClone { .. }
            | Self::BeginClone { .. }
            | Self::InspectClone { .. }
            | Self::CancelClone { .. }
            | Self::ResumeClone { .. }
            | Self::ReviewCloneTrust { .. }
            | Self::ApplyCloneTrust { .. }
            | Self::InspectRebind { .. }
            | Self::ReviewStartupCopy { .. }
            | Self::ApplyStartupCopy { .. }
            | Self::InspectStartupCopy { .. } => C::Checkout,
            Self::StartupCopyPlans { .. } | Self::Operations { .. } => C::Management,
            Self::Status {}
            | Self::Initialize { .. }
            | Self::List { .. }
            | Self::Inspect { .. }
            | Self::Review { .. }
            | Self::Apply { .. }
            | Self::InspectReceipt { .. }
            | Self::Sessions { .. }
            | Self::Backup { .. }
            | Self::Snapshots {}
            | Self::Export { .. }
            | Self::ReviewImport { .. }
            | Self::ApplyImport { .. }
            | Self::ReviewRestore { .. }
            | Self::ApplyRestore { .. } => C::Catalog,
        }
    }

    /// True when `response` is a domain rejection or describes this request's
    /// target, request identity or reviewed intent.
    pub fn matches_response(&self, response: &WorkspaceResponse) -> bool {
        use WorkspaceResponse as R;
        if let R::Error(_) = response {
            return true;
        }
        match (self, response) {
            (Self::Closeout { request }, R::Closeout(reply)) => {
                request.matches_reply(&CloseoutReply::State {
                    response: reply.clone(),
                })
            }
            (Self::Volumes {}, R::Volumes(_)) => true,
            (Self::CloneOutput { request, .. }, R::CloneOutput(reply)) => {
                execution_reply_matches(request, reply)
            }
            (
                Self::ReviewClone {
                    expected_revision,
                    spec,
                },
                R::CloneReview(review),
                // The service normalizes names, sources and remotes; bind the
                // stable identities and the revision the review was taken at.
            ) => {
                review.spec.home == spec.home
                    && review.spec.repository == spec.repository
                    && review.revision >= *expected_revision
            }
            (
                Self::BeginClone { request, .. }
                | Self::InspectClone { request }
                | Self::CancelClone { request }
                | Self::ResumeClone { request },
                R::Clone(record),
            ) => record.request == *request,
            (Self::ReviewCloneTrust { clone, .. }, R::CloneTrustReview(review)) => {
                review.clone == *clone
            }
            (
                Self::ApplyCloneTrust { request, .. }
                | Self::Apply { request, .. }
                | Self::InspectReceipt { request }
                | Self::ApplyImport { request, .. }
                | Self::ApplyRestore { request, .. },
                R::Receipt(receipt),
            ) => receipt.request == *request,
            (Self::InspectRebind { operation }, R::Rebind(record)) => {
                record.operation == *operation
            }
            // The source is canonicalized through its physical binding.
            (Self::ReviewStartupCopy { target, .. }, R::StartupCopyReview(review)) => {
                review.target == *target
            }
            (
                Self::ApplyStartupCopy { request, .. } | Self::InspectStartupCopy { request },
                R::StartupCopy(record),
            ) => record.request == *request,
            (Self::StartupCopyPlans { .. }, R::StartupCopyPlans(_)) => true,
            (Self::Operations { limit, .. }, R::Operations(page)) => {
                page.items.len() <= *limit as usize
            }
            (Self::Permissions { request }, R::Permissions(reply)) => {
                request.matches_response(reply)
            }
            (Self::Status {} | Self::Initialize { .. }, R::Status(_)) => true,
            (Self::List { limit, .. }, R::Page(page)) => page.items.len() <= *limit as usize,
            (Self::Inspect { target }, R::Entity(entity)) => entity.id() == *target,
            // The service normalizes intent (names, volume identities); bind the
            // operation kind and the exact revision the review was taken at.
            (
                Self::Review {
                    change,
                    expected_revision,
                },
                R::Review(review),
            ) => {
                std::mem::discriminant(&review.change) == std::mem::discriminant(change)
                    && review.revision == *expected_revision
            }
            (Self::Sessions { limit, .. }, R::Sessions(rows)) => rows.len() <= *limit as usize,
            (Self::Backup { name, .. }, R::Snapshot(snapshot)) => snapshot.name == *name,
            (Self::Snapshots {}, R::Snapshots(_)) => true,
            (Self::Export { .. }, R::Export(_)) => true,
            (Self::ReviewImport { .. }, R::ImportReview(_)) => true,
            (Self::ReviewRestore { snapshot }, R::RestoreReview(review)) => {
                review.snapshot.id == *snapshot
            }
            _ => false,
        }
    }
}

impl PermissionRequest {
    pub fn matches_response(&self, response: &PermissionResponse) -> bool {
        use PermissionResponse as R;
        match (self, response) {
            (
                Self::DecideProposal {
                    request, proposal, ..
                },
                R::Mutation(mutation),
            ) => {
                mutation.receipt.request == *request
                    && mutation
                        .proposal
                        .as_ref()
                        .is_some_and(|value| value.id == *proposal)
            }
            (Self::ImportedGrants { limit, .. }, R::ImportedGrants { items, .. }) => {
                items.len() <= *limit as usize
            }
            (
                Self::AbandonContextScope { session } | Self::ReconcileContextScope { session },
                R::ContextScopes(scopes),
            ) => scopes
                .iter()
                .all(|scope| scope.source == *session || scope.target == *session),
            (Self::ContextScopeStatus { review }, R::ContextScopes(scopes)) => scopes
                .iter()
                .all(|scope| scope.review.is_none_or(|value| value == *review)),
            (Self::ReviewCarry { session }, R::CarryReview(review)) => review.source == *session,
            (Self::Scope { session }, R::Scope(scope)) => scope.session == *session,
            (Self::Review { change, .. }, R::Review(review)) => review.change == *change,
            (Self::Apply { request, .. }, R::Mutation(mutation)) => {
                mutation.receipt.request == *request
            }
            (Self::Grant { grant }, R::Grant(value)) => value.id == *grant,
            (
                Self::Propose {
                    session,
                    request,
                    target,
                    ..
                },
                R::Mutation(mutation),
            ) => {
                mutation.receipt.request == *request
                    && mutation
                        .proposal
                        .as_ref()
                        .is_some_and(|value| value.session == *session && value.target == *target)
            }
            (Self::Proposal { proposal }, R::Proposal(value)) => value.id == *proposal,
            (Self::List { limit, .. }, R::Page(page)) => page.items.len() <= *limit as usize,
            _ => false,
        }
    }
}

/// Exact run correlation for execution controls nested in workspace routes.
pub fn execution_reply_matches(control: &ExecutionRequest, reply: &ExecutionResponse) -> bool {
    use ExecutionRequest as Request;
    use ExecutionResponse as Response;
    match (control, reply) {
        (Request::Inspect { run_id }, Response::Status { run }) => run.id == *run_id,
        (Request::Stop { run_id }, Response::Control { run_id: actual, .. }) => actual == run_id,
        (Request::Read { run_id, .. }, Response::Content { run_id: actual, .. }) => {
            actual == run_id
        }
        (
            Request::ReadPart {
                run_id,
                part,
                offset,
                expected_sha256,
                ..
            },
            Response::Part {
                run_id: actual,
                page,
            },
        ) => {
            actual == run_id
                && page.part == *part
                && page.offset == offset.unwrap_or(0)
                && expected_sha256
                    .as_ref()
                    .is_none_or(|digest| page.sha256 == *digest)
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "correlation_tests.rs"]
mod tests;
