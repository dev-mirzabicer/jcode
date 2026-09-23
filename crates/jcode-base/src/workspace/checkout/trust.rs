//! Stage-bound review of newly discovered Git and LFS sources.
//!
//! One clone operation retains the stage and all observations. Human trust
//! applies only to the exact typed sources of that operation, never to all
//! repositories or future clones.
use super::*;
use rusqlite::{TransactionBehavior, params};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedTrust {
    review: CloneTrustReview,
    stage: PhysicalBinding,
}

pub(super) fn validate_submodule_url(url: &str) -> Result<()> {
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
        return Err(issue(
            IssueCode::InvalidInput,
            "Invalid submodule transport URL",
        ));
    }
    if url.starts_with("../") || url.starts_with("./") || Path::new(url).is_absolute() {
        return Ok(());
    }
    checked_git_url(url).map(|_| ())
}

pub(super) fn approved(operation: &CloneOperation, source: &CloneTrustSource) -> bool {
    let original = match source.kind {
        CloneTrustKind::Submodule => &operation.public.review.spec.trusted_submodule_urls,
        CloneTrustKind::Lfs => &operation.public.review.spec.trusted_lfs_urls,
    };
    original.iter().any(|url| url == &source.url)
        || operation
            .public
            .trust_approvals
            .iter()
            .any(|approval| approval.sources.contains(source))
}

impl WorkspaceService {
    pub(super) fn record_clone_sources(
        &self,
        request: RequestId,
        sources: &[CloneTrustSource],
    ) -> Result<()> {
        let observed = sources.to_vec();
        let record = self.update_clone(request, |operation| {
            if matches!(operation.public.state, CloneState::Ready | CloneState::Cancelled) {
                return Err(issue(IssueCode::Conflict, "Cannot discover sources after clone completion"));
            }
            for source in &observed {
                if !operation.public.discovered_sources.contains(source) {
                    operation.public.discovered_sources.push(source.clone());
                }
            }
            operation.public.discovered_sources.sort();
            let pending = observed.iter().filter(|source| !approved(operation, source))
                .cloned().collect::<Vec<_>>();
            if !pending.is_empty() {
                operation.public.pending_trust = pending;
                operation.public.state = CloneState::AwaitingTrust;
                operation.public.issue = Some(issue(IssueCode::PermissionRequired,
                    "Review discovered submodule/LFS sources before further materialization; the exact owned stage is retained"));
            }
            Ok(())
        })?;
        if record.state == CloneState::AwaitingTrust {
            return Err(record.issue.unwrap_or_else(|| {
                issue(
                    IssueCode::PermissionRequired,
                    "Clone source trust review is required",
                )
            }));
        }
        Ok(())
    }

    pub fn review_clone_trust(
        &self,
        clone: RequestId,
        expected_revision: Revision,
    ) -> Result<CloneTrustReview> {
        let _clone = self.clone_lease(clone)?;
        let mut connection = self.connection()?;
        let operation = read_clone(&connection, clone)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone"))?;
        if operation.public.revision != expected_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Checkout changed before source trust review",
            ));
        }
        let stage = require_reviewable_stage(self, &operation)?;
        let _root = self.acquire_binding(&stage)?;
        verify_source_observations(self, &operation, &stage)?;
        let review = CloneTrustReview {
            id: ReviewId::new(),
            clone,
            catalog_revision: storage::status(&connection)?.revision,
            clone_revision: operation.public.revision,
            stage: stage.observed_path().to_path_buf(),
            source_commit: operation.public.review.source_commit.clone(),
            sources: operation.public.pending_trust.clone(),
        };
        let prepared = PreparedTrust {
            review: review.clone(),
            stage,
        };
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        organization::require_revision(&tx, review.catalog_revision)?;
        let current =
            read_clone(&tx, clone)?.ok_or_else(|| corrupt("Checkout disappeared during review"))?;
        require_same_sources(&current, &prepared)?;
        tx.execute(
            "INSERT INTO reviews VALUES(?1,'clone_trust',?2)",
            params![review.id.to_string(), encode(&prepared)?],
        )
        .map_err(io)?;
        tx.commit().map_err(io)?;
        Ok(review)
    }

    pub fn apply_clone_trust(
        &self,
        request: RequestId,
        review: ReviewId,
        issued_by: &WorkspaceClientAuthority,
    ) -> Result<Receipt> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let prepared: PreparedTrust =
            organization::read_review(&connection, review, "clone_trust")?;
        let input = digest(encode(&(&prepared.review, &issued_by.0))?.as_bytes());
        if let Some(receipt) = organization::replay(&connection, request, &input)? {
            return Ok(receipt);
        }
        let _clone = self.clone_lease(prepared.review.clone)?;
        let operation = read_clone(&connection, prepared.review.clone)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone"))?;
        require_same_sources(&operation, &prepared)?;
        organization::require_revision(&connection, prepared.review.catalog_revision)?;
        let stage = require_reviewable_stage(self, &operation)?;
        if stage != prepared.stage {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Clone stage differs from the approved source review",
            ));
        }
        let _root = self.acquire_binding(&stage)?;
        verify_source_observations(self, &operation, &stage)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(receipt) = organization::replay(&tx, request, &input)? {
            return Ok(receipt);
        }
        organization::require_revision(&tx, prepared.review.catalog_revision)?;
        let mut current = read_clone(&tx, prepared.review.clone)?
            .ok_or_else(|| corrupt("Clone changed during source trust approval"))?;
        require_same_sources(&current, &prepared)?;
        let receipt = organization::commit_receipt(
            &tx,
            request,
            &input,
            vec![EntityId::Location(current.public.location)],
        )?;
        current.public.trust_approvals.push(CloneTrustApproval {
            request,
            review,
            issued_by: issued_by.0.clone(),
            sources: prepared.review.sources.clone(),
        });
        current.public.pending_trust.clear();
        current.public.state = CloneState::Materializing;
        current.public.issue = None;
        current.public.revision = receipt.revision;
        tx.execute(
            "UPDATE operations SET state='pending',body=?2 WHERE id=?1",
            params![current.public.operation.to_string(), encode(&current)?],
        )
        .map_err(io)?;
        tx.commit().map_err(io)?;
        self.checkpoint("clone_trust_approved")?;
        self.after_mutation(receipt)
    }
}

fn require_reviewable_stage(
    service: &WorkspaceService,
    operation: &CloneOperation,
) -> Result<PhysicalBinding> {
    if operation.public.state != CloneState::AwaitingTrust
        || operation.public.cancel_requested
        || operation.public.pending_trust.is_empty()
        || !operation.acquired
        || operation.materialized
        || operation.published_binding.is_some()
    {
        return Err(issue(
            IssueCode::Conflict,
            "Checkout is not waiting for a source trust review over an acquired stage",
        ));
    }
    let stage = operation
        .stage_binding
        .clone()
        .ok_or_else(|| corrupt("Paused checkout has no acquired stage witness"))?;
    if operation.public.stage.as_deref() != Some(stage.observed_path()) {
        return Err(corrupt(
            "Paused checkout stage path disagrees with its binding",
        ));
    }
    let current = service.resolver.resolve_directory(&stage).map_err(io)?;
    if current.relocated || current.path != stage.observed_path() {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Paused checkout stage moved or was replaced",
        ));
    }
    Ok(stage)
}

fn require_same_sources(current: &CloneOperation, prepared: &PreparedTrust) -> Result<()> {
    if current.public.request != prepared.review.clone
        || current.public.revision != prepared.review.clone_revision
        || current.public.state != CloneState::AwaitingTrust
        || current.public.pending_trust != prepared.review.sources
        || current.public.review.source_commit != prepared.review.source_commit
        || current.stage_binding.as_ref() != Some(&prepared.stage)
    {
        return Err(issue(
            IssueCode::Conflict,
            "Checkout or discovered source list changed since trust review",
        ));
    }
    Ok(())
}

pub(super) fn verify_source_observations(
    service: &WorkspaceService,
    operation: &CloneOperation,
    stage: &PhysicalBinding,
) -> Result<()> {
    service.verify_acquired(operation, stage)?;
    let root = stage.observed_path();
    let expected_origin = match &operation.public.review.spec.source {
        CloneSource::Local { path } => path.to_string_lossy().to_string(),
        CloneSource::Remote { url } => url.clone(),
    };
    let origin = git::git(Some(root), ["remote", "get-url", "origin"])
        .output()
        .map_err(io)?;
    if !origin.status.success()
        || std::str::from_utf8(&origin.stdout).map_err(io)?.trim_end() != expected_origin
    {
        return Err(issue(
            IssueCode::Conflict,
            "Paused checkout origin differs from the reviewed acquisition source",
        ));
    }
    materialize::verify_clean_tree(root, &operation.public.review.spec)?;
    for source in operation.public.pending_trust.iter().chain(
        operation
            .public
            .trust_approvals
            .iter()
            .flat_map(|approval| approval.sources.iter()),
    ) {
        let declared = root.join(&source.repository).canonicalize().map_err(io)?;
        if !declared.starts_with(root) {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Trust source repository left the owned stage",
            ));
        }
        let present = match source.kind {
            CloneTrustKind::Submodule => {
                materialize::submodule_sources(root, &declared)?.contains(source)
            }
            CloneTrustKind::Lfs => {
                materialize::lfs_source(root, &declared)?.as_ref() == Some(source)
            }
        };
        if !present {
            return Err(issue(
                IssueCode::Conflict,
                "Git/LFS source changed since the paused stage was inspected",
            ));
        }
    }
    Ok(())
}
