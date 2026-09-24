use super::*;

impl WorkspaceService {
    /// The review installs the closing fence but is never removal authority.
    pub async fn review_closeout_removal(
        &self,
        operation: OperationId,
        expected: Revision,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutReview> {
        let _operation = self.closeout_lease(operation)?;
        let fenced = self.fence_closeout(operation, expected)?;
        let mut stored = load(&self.connection()?, operation)?;
        let mut issues = Vec::new();
        let _root = match self.acquire_binding(&stored.binding) {
            Ok(lease) => Some(lease),
            Err(error) => {
                issues.push(error);
                None
            }
        };
        let work = runtime.observe(self, &stored).await?;
        issues.extend(work.findings.iter().map(|finding| {
            issue(
                IssueCode::LiveWork,
                format!("{}: {}", finding.identity, finding.detail),
            )
        }));
        let references = self.closeout_references(&stored, runtime.session_root)?;
        issues.extend(references.issues.clone());
        if let Err(error) = self.verify_closeout_evidence(&stored) {
            issues.push(error);
        }
        if let Err(error) = verification::linked_content(&stored, &references) {
            issues.push(error);
        }
        match git::dependent_worktrees(&stored) {
            Ok(dependencies) => issues.extend(dependencies),
            Err(error) => issues.push(error),
        }
        let ownership = self.closeout_destination_ownership(&stored)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let current = load(&transaction, operation)?;
        require_current(&transaction, &current, fenced.revision)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        let revision = storage::status(&transaction)?.revision;
        let review = CloseoutReview {
            id: ReviewId::new(),
            operation,
            revision,
            inventory_digest: stored.record.inventory_digest.clone(),
            preservation_digest: stored.record.preservation_digest.clone(),
            references_digest: verification::references_digest(&references)?,
            work,
            issues,
            preservation_volume_ownership: ownership,
        };
        stored.record.revision = revision;
        stored.record.stage = if review.issues.is_empty() {
            CloseoutStage::ReadyForApproval
        } else {
            CloseoutStage::NeedsDecision
        };
        stored.record.authorization = None;
        stored.record.issues = review.issues.clone();
        stored.record.preservation_volume_ownership = ownership;
        stored.review = Some(review.clone());
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        let backed = self.closeout_backup(stored.record)?;
        if let Some(error) = backed
            .issues
            .iter()
            .find(|issue| issue.code == IssueCode::BackupFailed)
        {
            return Err(issue(
                IssueCode::BackupFailed,
                format!(
                    "Closeout {operation} review {} was committed, but its automatic backup failed: {}. Inspect the retained review before retrying.",
                    review.id, error.detail
                ),
            ));
        }
        Ok(review)
    }

    pub fn inspect_closeout_review(
        &self,
        operation: OperationId,
    ) -> Result<Option<CloseoutReview>> {
        Ok(load(&self.connection()?, operation)?.review)
    }

    pub async fn approve_closeout_removal(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        target: CloseoutReviewTarget,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecord> {
        let CloseoutReviewTarget { operation, review } = target;
        self.authorize_closeout(
            request,
            operation,
            review,
            CloseoutAuthorizationSource::Human {
                client: client.0.clone(),
            },
            runtime,
        )
        .await
    }

    /// An agent may use only the initial human-issued conditional grant. Its
    /// assessment cannot waive preservation, current-work or physical checks.
    pub async fn declare_closeout_no_loss(
        &self,
        session: &str,
        request: RequestId,
        target: CloseoutReviewTarget,
        assessment: &str,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecord> {
        let CloseoutReviewTarget { operation, review } = target;
        if session.trim().is_empty() || assessment.trim().is_empty() {
            return Err(issue(
                IssueCode::InvalidInput,
                "A no-loss declaration requires its originating session and assessment",
            ));
        }
        self.authorize_closeout(
            request,
            operation,
            review,
            CloseoutAuthorizationSource::Conditional {
                session: session.into(),
                assessment: assessment.into(),
            },
            runtime,
        )
        .await
    }

    async fn authorize_closeout(
        &self,
        request: RequestId,
        operation: OperationId,
        review_id: ReviewId,
        source: CloseoutAuthorizationSource,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecord> {
        let _operation = self.closeout_lease(operation)?;
        let input =
            verification::hash_value(&("closeout_authorize", operation, review_id, &source))?;
        if organization::replay(&self.connection()?, request, &input)?.is_some() {
            return self.inspect_closeout(operation);
        }
        let mut stored = load(&self.connection()?, operation)?;
        let review = stored
            .review
            .as_ref()
            .ok_or_else(|| {
                issue(
                    IssueCode::PermissionRequired,
                    "Prepare a current closeout review first",
                )
            })?
            .clone();
        if review.id != review_id
            || review.operation != operation
            || review.revision != stored.record.revision
        {
            return Err(issue(
                IssueCode::Conflict,
                "Closeout review target or revision changed",
            ));
        }
        if stored.record.stage != CloseoutStage::ReadyForApproval || !review.issues.is_empty() {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Unresolved findings cannot be overridden by approval",
            ));
        }
        if matches!(source, CloseoutAuthorizationSource::Conditional { .. })
            && !stored.record.spec.conditional_no_loss
        {
            return Err(issue(
                IssueCode::PermissionRequired,
                "This closeout requires final trusted-client approval",
            ));
        }
        let _root = self.acquire_binding(&stored.binding)?;
        self.validate_closeout_review(&stored, &review, runtime)
            .await?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if organization::replay(&transaction, request, &input)?.is_some() {
            return Ok(load(&transaction, operation)?.record);
        }
        require_current(
            &transaction,
            &load(&transaction, operation)?,
            review.revision,
        )?;
        let receipt = organization::commit_receipt(
            &transaction,
            request,
            &input,
            vec![EntityId::Location(stored.record.spec.location)],
        )?;
        stored.record.authorization = Some(CloseoutAuthorization {
            review: review_id,
            seal: verification::seal(&stored, &review)?,
            source,
        });
        stored.record.revision = receipt.revision;
        stored.record.stage = CloseoutStage::Authorized;
        transaction
            .execute(
                "INSERT INTO operations VALUES(?1,'closeout_authorization','complete',?2)",
                params![
                    receipt.operation.to_string(),
                    encode(&(operation, &stored.record.authorization))?
                ],
            )
            .map_err(io)?;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }

    pub(super) async fn validate_closeout_review(
        &self,
        stored: &StoredCloseout,
        review: &CloseoutReview,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<()> {
        require_current(&self.connection()?, stored, stored.record.revision)?;
        self.verify_closeout_evidence(stored)?;
        let references = self.closeout_references(stored, runtime.session_root)?;
        verification::linked_content(stored, &references)?;
        if !references.issues.is_empty() {
            return Err(issue(
                IssueCode::IncompleteCapture,
                "Checkout references contain unresolved findings",
            ));
        }
        if verification::references_digest(&references)? != review.references_digest {
            return Err(issue(
                IssueCode::Conflict,
                "Checkout references changed after review",
            ));
        }
        if !git::dependent_worktrees(stored)?.is_empty() {
            return Err(issue(
                IssueCode::Referenced,
                "Other worktrees still depend on Git metadata in this checkout",
            ));
        }
        let work = runtime.observe(self, stored).await?;
        if !work.findings.is_empty() {
            return Err(issue(
                IssueCode::LiveWork,
                format!(
                    "Closeout is blocked by {} current work findings",
                    work.findings.len()
                ),
            ));
        }
        Ok(())
    }

    fn verify_closeout_evidence(&self, stored: &StoredCloseout) -> Result<()> {
        let source = self
            .resolver
            .resolve_directory(&stored.binding)
            .map_err(io)?;
        let destination = self
            .resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        if source.relocated || destination.relocated {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "A reviewed closeout location moved",
            ));
        }
        inventory::verify_source(stored)?;
        verification::preservation(stored)
    }

    fn closeout_destination_ownership(&self, stored: &StoredCloseout) -> Result<Option<bool>> {
        let destination = self
            .resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        if destination.relocated {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Preservation destination moved",
            ));
        }
        crate::location::native_files::VerifiedDirectory::open(destination.path)
            .and_then(|directory| directory.volume_ownership_enforced())
            .map_err(io)
    }
}
