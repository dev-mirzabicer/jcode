//! Trusted recovery decisions never infer old destructive receipts. Retaining
//! files ends registration without claiming physical removal or preservation.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
struct Observation {
    view: CloseoutRecoveryPath,
    witness: Option<Witness>,
    link: Option<PathBuf>,
    binding: Option<PhysicalBinding>,
}
#[derive(Clone, Serialize, Deserialize)]
struct RecoveryFacts {
    observations: Vec<Observation>,
    references: String,
    work: CloseoutWorkReport,
    issues: Vec<Issue>,
    adopt: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PreparedRecovery {
    review: CloseoutRecoveryReview,
    state: String,
    observations: Vec<Observation>,
    references: String,
    // Retained prior authority/evidence is inspectable even after restart.
    prior: StoredCloseout,
    reviewed_by: String,
}

impl WorkspaceService {
    #[cfg(target_os = "macos")]
    pub async fn review_closeout_recovery(
        &self,
        client: &WorkspaceClientAuthority,
        operation: OperationId,
        expected: Revision,
        action: CloseoutRecoveryAction,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecoveryReview> {
        let _operation = self.closeout_lease(operation)?;
        runtime.check_stop()?;
        let mut stored = load(&self.connection()?, operation)?;
        require_current(&self.connection()?, &stored, expected)?;
        if matches!(
            stored.record.stage,
            CloseoutStage::Closed | CloseoutStage::Retained | CloseoutStage::Revoked
        ) {
            return Err(issue(
                IssueCode::Conflict,
                "Terminal closeout cannot be recovered as an active operation",
            ));
        }
        let prior = stored.clone();
        // The explicit trusted recovery review revokes old authority and keeps
        // admission fenced. It never restores an earlier conditional grant.
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&tx, &load(&tx, operation)?, expected)?;
        let Entity::Location(mut location) =
            entity(&tx, EntityId::Location(stored.record.spec.location))?
        else {
            return Err(corrupt("Recovery location missing"));
        };
        if location.retired || location.lifecycle.is_historical() {
            return Err(issue(
                IssueCode::Conflict,
                "Location registration already ended",
            ));
        }
        tx.execute("UPDATE catalog SET revision=revision+1", [])
            .map_err(io)?;
        stored.record.revision = storage::status(&tx)?.revision;
        stored.record.stage = CloseoutStage::RecoveryRequired;
        stored.record.authorization = None;
        stored.record.spec.conditional_no_loss = false;
        stored.review = None;
        location.lifecycle = LocationLifecycle::Closing;
        location.revision = stored.record.revision;
        organization::save_entity(&tx, &Entity::Location(location))?;
        save(&tx, &stored)?;
        tx.commit().map_err(io)?;
        drop(connection);
        let after_fence = |error: Issue| {
            issue(
                error.code,
                format!(
                    "Closeout {operation} is fenced at revision {}; no recovery decision was applied: {}",
                    stored.record.revision, error.detail
                ),
            )
        };
        let _root = self
            .acquire_recorded_binding(&stored.binding)
            .map_err(&after_fence)?;
        let RecoveryFacts {
            observations,
            references,
            work,
            mut issues,
            adopt,
        } = self
            .recovery_facts(&stored, action, runtime)
            .await
            .map_err(&after_fence)?;
        issues.extend(
            work.findings
                .iter()
                .map(|f| issue(IssueCode::LiveWork, format!("{}: {}", f.identity, f.detail))),
        );
        let review = CloseoutRecoveryReview {
            id: ReviewId::new(),
            operation,
            revision: stored.record.revision,
            action,
            paths: observations.iter().map(|p| p.view.clone()).collect(),
            adopt_empty_holding: adopt,
            work,
            issues,
        };
        let prepared = PreparedRecovery {
            review: review.clone(),
            state: verification::hash_value(&stored)?,
            observations,
            references,
            prior,
            reviewed_by: client.0.clone(),
        };
        runtime.check_stop().map_err(after_fence)?;
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&tx, &load(&tx, operation)?, stored.record.revision)?;
        tx.execute(
            "INSERT INTO reviews(id,kind,body) VALUES(?1,'closeout_recovery',?2)",
            params![review.id.to_string(), encode(&prepared)?],
        )
        .map_err(io)?;
        tx.commit().map_err(io)?;
        if let Some(error) = self
            .closeout_backup(stored.record)?
            .issues
            .into_iter()
            .find(|i| i.code == IssueCode::BackupFailed)
        {
            return Err(issue(
                IssueCode::BackupFailed,
                format!(
                    "Recovery review {} was committed; inspect before retrying: {}",
                    review.id, error.detail
                ),
            ));
        }
        Ok(review)
    }

    pub fn inspect_closeout_recovery(&self, review: ReviewId) -> Result<CloseoutRecoveryReview> {
        let prepared: PreparedRecovery =
            organization::read_review(&self.connection()?, review, "closeout_recovery")?;
        Ok(prepared.review)
    }

    pub fn pending_closeout_recovery(
        &self,
        operation: OperationId,
    ) -> Result<Option<CloseoutRecoveryReview>> {
        let _catalog = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let current = load(&transaction, operation)?;
        let body: Option<String> = transaction.query_row(
            "SELECT body FROM reviews WHERE kind='closeout_recovery' AND json_extract(body,'$.review.operation')=?1 AND json_extract(body,'$.review.revision')=?2 ORDER BY id DESC LIMIT 1",
            params![operation.to_string(), i64::try_from(current.record.revision).map_err(corrupt)?], |row| row.get(0)).optional().map_err(io)?;
        body.map(|body| decode::<PreparedRecovery>(&body).map(|prepared| prepared.review))
            .transpose()
    }

    #[cfg(target_os = "macos")]
    pub async fn apply_closeout_recovery(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        target: CloseoutReviewTarget,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecord> {
        let _operation = self.closeout_lease(target.operation)?;
        let prepared: PreparedRecovery =
            organization::read_review(&self.connection()?, target.review, "closeout_recovery")?;
        if prepared.review.operation != target.operation {
            return Err(issue(
                IssueCode::Conflict,
                "Recovery review belongs to a different closeout",
            ));
        }
        let input = verification::hash_value(&(target, &client.0))?;
        if organization::replay(&self.connection()?, request, &input)?.is_some() {
            return Ok(load(&self.connection()?, target.operation)?.record);
        }
        runtime.check_stop()?;
        let mut stored = load(&self.connection()?, target.operation)?;
        require_current(&self.connection()?, &stored, prepared.review.revision)?;
        if verification::hash_value(&stored)? != prepared.state
            || !prepared.review.issues.is_empty()
        {
            return Err(issue(
                IssueCode::Conflict,
                "Recovery findings are unresolved or changed; prepare a fresh review",
            ));
        }
        let _root = self.acquire_recorded_binding(&stored.binding)?;
        let RecoveryFacts {
            observations,
            references,
            work,
            issues,
            adopt,
        } = self
            .recovery_facts(&stored, prepared.review.action, runtime)
            .await?;
        if !issues.is_empty() || !work.findings.is_empty() {
            return Err(issue(
                IssueCode::RecoveryRequired,
                format!("Recovery has new blockers: {issues:?} {:?}", work.findings),
            ));
        }
        if verification::hash_value(&observations)?
            != verification::hash_value(&prepared.observations)?
            || references != prepared.references
            || adopt != prepared.review.adopt_empty_holding
        {
            return Err(issue(
                IssueCode::Conflict,
                "Recovery target, references or physical observations changed",
            ));
        }
        let ownership = if prepared.review.action == CloseoutRecoveryAction::ResumeRemoval {
            self.closeout_destination_ownership(&stored)?
        } else {
            stored.record.preservation_volume_ownership
        };
        let retained_report = if prepared.review.action
            == CloseoutRecoveryAction::UnregisterRetainFiles
        {
            let report = self
                .root
                .join("closeout-reports")
                .join(format!("{}-retained-{}.json", target.operation, request));
            storage::private_dir(report.parent().unwrap())?;
            storage::atomic_json(
                &report,
                &serde_json::json!({"kind":"checkout_retention_decision_evidence", "review":prepared.review,
                "issued_by":client.0, "observations":observations, "prior":prepared.prior, "current_before_commit":stored,
                "no_filesystem_removal_performed":true}),
            )?;
            Some(report)
        } else {
            None
        };
        runtime.check_stop()?;
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&tx, &load(&tx, target.operation)?, prepared.review.revision)?;
        let receipt = organization::commit_receipt(
            &tx,
            request,
            &input,
            vec![EntityId::Location(stored.record.spec.location)],
        )?;
        stored.record.revision = receipt.revision;
        stored.record.issues.clear();
        match prepared.review.action {
            CloseoutRecoveryAction::RestartPreparation => {
                stored.removal = None;
                stored.record.quarantine = None;
                stored.record.removed_entries = 0;
                stored.record.inventory_digest = None;
                stored.record.inventory_entries = 0;
                stored.record.preservation_digest = None;
                stored.inventory = None;
                stored.history = None;
                stored.history_digest = None;
                stored.references = None;
                stored.preservation = None;
                stored.decisions.clear();
                stored.review = None;
                stored.record.authorization = None;
                stored.record.stage = CloseoutStage::Preparing;
                let Entity::Location(mut location) =
                    entity(&tx, EntityId::Location(stored.record.spec.location))?
                else {
                    return Err(corrupt("Recovery location missing"));
                };
                location.lifecycle = LocationLifecycle::Ready;
                location.revision = receipt.revision;
                organization::save_entity(&tx, &Entity::Location(location))?;
            }
            CloseoutRecoveryAction::ResumeRemoval => {
                if adopt.is_some() {
                    stored
                        .removal
                        .as_mut()
                        .ok_or_else(|| corrupt("Removal journal missing"))?
                        .holding_binding = observations
                        .iter()
                        .find(|o| Some(&o.view.path) == adopt.as_ref())
                        .and_then(|o| o.binding.clone());
                }
                let review = CloseoutReview {
                    id: target.review,
                    operation: target.operation,
                    revision: receipt.revision,
                    inventory_digest: stored.record.inventory_digest.clone(),
                    preservation_digest: stored.record.preservation_digest.clone(),
                    references_digest: references,
                    work,
                    issues: vec![],
                    preservation_volume_ownership: ownership,
                };
                let seal = verification::seal(&stored, &review)?;
                stored.review = Some(review);
                stored.record.authorization = Some(CloseoutAuthorization {
                    review: target.review,
                    seal,
                    source: CloseoutAuthorizationSource::Human {
                        client: client.0.clone(),
                    },
                });
                stored.record.stage = CloseoutStage::Authorized;
            }
            CloseoutRecoveryAction::UnregisterRetainFiles => {
                let Entity::Location(mut location) =
                    entity(&tx, EntityId::Location(stored.record.spec.location))?
                else {
                    return Err(corrupt("Recovery location missing"));
                };
                location.lifecycle = LocationLifecycle::Unregistered;
                location.retired = true;
                location.revision = receipt.revision;
                organization::save_entity(&tx, &Entity::Location(location))?;
                tx.execute(
                    "UPDATE bindings SET live_key=NULL WHERE location=?1",
                    [stored.record.spec.location.to_string()],
                )
                .map_err(io)?;
                stored.record.stage = CloseoutStage::Retained;
                stored.record.authorization = None;
                stored.review = None;
                let mut paths: Vec<_> = observations
                    .iter()
                    .filter(|o| o.view.present)
                    .map(|o| o.view.path.clone())
                    .collect();
                if stored.preservation.is_some()
                    || stored
                        .record
                        .preservation_directory
                        .try_exists()
                        .map_err(io)?
                {
                    paths.push(stored.record.preservation_directory.clone());
                }
                let history = portable::ClosedReference {
                    location: stored.record.spec.location,
                    operation: target.operation,
                    preservation_paths: paths,
                    report: retained_report,
                };
                tx.execute(
                    "INSERT INTO closed_history(location,body) VALUES(?1,?2)",
                    params![history.location.to_string(), encode(&history)?],
                )
                .map_err(io)?;
            }
        }
        save(&tx, &stored)?;
        tx.execute("INSERT INTO operations(id,kind,state,body) VALUES(?1,'closeout_recovery','complete',?2)", params![receipt.operation.to_string(), encode(&(receipt.clone(), target, prepared.review.action))?]).map_err(io)?;
        tx.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }

    #[cfg(target_os = "macos")]
    async fn recovery_facts(
        &self,
        stored: &StoredCloseout,
        action: CloseoutRecoveryAction,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<RecoveryFacts> {
        let root = stored.binding.observed_path();
        let parent = root
            .parent()
            .ok_or_else(|| corrupt("Recovery root has no parent"))?;
        let operation = stored.record.operation;
        let paths = [
            root.to_path_buf(),
            parent.join(format!(".jcode-closeout-{operation}")),
            parent.join(format!(".jcode-closeout-holding-{operation}")),
        ];
        let mut observations = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let expected = if index < 2 {
                Some(&stored.binding)
            } else {
                stored
                    .removal
                    .as_ref()
                    .and_then(|r| r.holding_binding.as_ref())
            };
            observations.push(self.recovery_observation(path, expected)?);
        }
        let mut findings = Vec::new();
        runtime.observe_internal(self, stored, &mut findings)?;
        for observed in &observations {
            if observed.binding.is_some() {
                match work::external_work(self, operation, &observed.view.path, runtime.capture)
                    .await
                {
                    Ok(current) => findings.extend(current),
                    Err(error) => findings.push(CloseoutWorkFinding {
                        kind: CloseoutWorkKind::Unknown,
                        identity: observed.view.path.display().to_string(),
                        detail: error.detail,
                    }),
                }
            }
        }
        let work = CloseoutWorkReport {
            operation,
            observed_at: chrono::Utc::now().to_rfc3339(),
            findings,
        };
        let references = self.closeout_references(stored, runtime.session_root)?;
        let reference_digest = verification::references_digest(&references)?;
        let mut issues = Vec::new();
        let mut adopt = None;
        match action {
            CloseoutRecoveryAction::RestartPreparation => {
                if !observations[0].view.matches_recorded_root
                    || observations[1].view.present
                    || observations[2].view.present
                    || stored.record.removed_entries != 0
                    || stored.removal.as_ref().is_some_and(|r| {
                        r.pending.is_some()
                            || r.quarantined.is_some()
                            || r.root_removed
                            || r.worktree.as_ref().is_some_and(|w| w.started)
                    })
                {
                    issues.push(issue(IssueCode::RecoveryRequired, "Restart requires the intact original root and proof that removal has not begun"));
                }
            }
            CloseoutRecoveryAction::ResumeRemoval => {
                if let Err(error) = verification::preservation(stored) {
                    issues.push(error);
                }
                if let Err(error) = verification::linked_content(stored, &references) {
                    issues.push(error);
                }
                issues.extend(references.issues);
                if let Err(error) = self.verify_recovery_remainder(stored) {
                    issues.push(error);
                }
                if let Some(removal) = &stored.removal
                    && removal.holding_binding.is_none()
                    && observations[2].view.present
                {
                    if observations[2].binding.is_some()
                        && std::fs::read_dir(&paths[2]).map_err(io)?.next().is_none()
                    {
                        adopt = Some(paths[2].clone());
                    } else {
                        issues.push(issue(IssueCode::RecoveryRequired, "Unjournaled holding path is not an empty directory; retain it for explicit repair"));
                    }
                }
            }
            CloseoutRecoveryAction::UnregisterRetainFiles => (),
        }
        Ok(RecoveryFacts {
            observations,
            references: reference_digest,
            work,
            issues,
            adopt,
        })
    }

    fn recovery_observation(
        &self,
        path: &Path,
        expected: Option<&PhysicalBinding>,
    ) -> Result<Observation> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io(error)),
        };
        let binding = metadata
            .as_ref()
            .filter(|m| m.is_dir())
            .map(|_| self.resolver.bind_directory(path).map_err(io))
            .transpose()?;
        let matches = binding
            .as_ref()
            .zip(expected)
            .is_some_and(|(actual, expected)| {
                actual.volume() == expected.volume()
                    && actual.root_witness() == expected.root_witness()
            });
        let link = metadata
            .as_ref()
            .filter(|m| m.file_type().is_symlink())
            .map(|_| std::fs::read_link(path).map_err(io))
            .transpose()?;
        Ok(Observation {
            view: CloseoutRecoveryPath {
                path: path.into(),
                present: metadata.is_some(),
                matches_recorded_root: matches,
            },
            witness: metadata.as_ref().map(Witness::of).transpose()?,
            link,
            binding,
        })
    }
}
