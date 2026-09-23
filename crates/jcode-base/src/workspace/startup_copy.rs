//! Explicit path-selection copy. StartupContext owns validation and plan bytes;
//! the catalog owns target identity, review and cross-store operation receipts.
use super::*;
use crate::location::volume::PhysicalBinding;
use crate::startup_context::{
    StartupContext, StartupFileIssueKind, StartupProjectPlanTransition, StartupSelectionInput,
};
use rusqlite::{TransactionBehavior, params};
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedCopy {
    review: StartupCopyReview,
    approvals: Vec<StartupCopyApproval>,
    source_binding: PhysicalBinding,
    target_binding: PhysicalBinding,
    target_location_revision: Revision,
    transition: StartupProjectPlanTransition,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyOperation {
    public: StartupCopyRecord,
    prepared: PreparedCopy,
    backup_pending: bool,
}

pub struct StartupCopyIntent {
    pub record: StartupCopyRecord,
    pub target_path: PathBuf,
    pub transition: StartupProjectPlanTransition,
}

impl WorkspaceService {
    pub fn review_startup_copy(
        &self,
        expected_catalog_revision: Revision,
        source: PathBuf,
        target: LocationId,
        expected_source_plan_revision: u64,
        expected_target_plan_revision: u64,
        external_approvals: Vec<StartupCopyApproval>,
    ) -> Result<StartupCopyReview> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        organization::require_revision(&connection, expected_catalog_revision)?;
        let (location, target_binding) = bound_target(&connection, target)?;
        validate_target(&self.resolver, &location, &target_binding)?;
        let source_binding = self.resolver.bind_directory(&source).map_err(io)?;
        let engine = self.startup_engine()?;
        let source_project = engine
            .resolve_project(source_binding.observed_path())
            .map_err(io)?;
        if source_project.active_root() != source_binding.observed_path() {
            return Err(issue(
                IssueCode::InvalidInput,
                "Select the source's physical Git or directory root",
            ));
        }
        let target_project = engine
            .resolve_project(target_binding.observed_path())
            .map_err(io)?;
        if target_project.active_root() != target_binding.observed_path() {
            return Err(issue(
                IssueCode::InvalidInput,
                "Startup Context copy target must be a physical root",
            ));
        }
        if source_project.key() == target_project.key() {
            return Err(issue(
                IssueCode::InvalidInput,
                "Source and destination share one Startup Context plan; no copy is needed",
            ));
        }
        let source_plan = engine.load_project_plan(&source_project).map_err(io)?;
        if source_plan.plan().revision() != expected_source_plan_revision {
            return Err(issue(
                IssueCode::Conflict,
                "Source Startup Context plan changed before review",
            ));
        }
        let mut approvals = HashMap::new();
        for approval in &external_approvals {
            crate::startup_context::StartupFileSpecId::parse(&approval.source_spec_id)
                .map_err(io)?;
            if approvals
                .insert(
                    approval.source_spec_id.clone(),
                    approval.approved_resolved_target.clone(),
                )
                .is_some()
            {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Duplicate Startup Context approval identity",
                ));
            }
        }
        let inputs = source_plan
            .plan()
            .entries()
            .iter()
            .map(|file| {
                let mut input = StartupSelectionInput::new(file.path().as_path());
                if let Some(approved) = approvals.remove(file.id().as_str()) {
                    input = input.with_external_approval(approved);
                }
                input
            })
            .collect::<Vec<_>>();
        if !approvals.is_empty() {
            return Err(issue(
                IssueCode::InvalidInput,
                "Startup Context approval refers to a source file not in the reviewed plan",
            ));
        }
        let preview = engine.preview_selection(&target_project, inputs);
        if !preview.is_valid() {
            let needs_approval = preview.issues().any(|file| {
                matches!(
                    file.kind(),
                    StartupFileIssueKind::ExternalApprovalRequired { .. }
                        | StartupFileIssueKind::ExternalTargetChanged { .. }
                        | StartupFileIssueKind::InvalidExternalApproval { .. }
                )
            });
            let findings = preview
                .issues()
                .take(4)
                .map(|file| {
                    format!(
                        "{}: {:?}",
                        file.logical_path()
                            .map_or_else(|| "selection".into(), |path| path.display().to_string()),
                        file.kind()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(issue(
                if needs_approval {
                    IssueCode::PermissionRequired
                } else {
                    IssueCode::InvalidInput
                },
                format!(
                    "Target Startup Context selection has {} issue(s): {findings}",
                    preview.issue_count()
                ),
            ));
        }
        let transition = engine
            .prepare_project_plan_transition(
                &target_project,
                expected_target_plan_revision,
                &preview,
            )
            .map_err(io)?;
        let entries = source_plan
            .plan()
            .entries()
            .iter()
            .zip(preview.selected())
            .map(|(source, selected)| StartupCopyEntry {
                source_spec_id: source.id().to_string(),
                selected_path: selected.spec().path().as_path().to_path_buf(),
                resolved_target: selected.resolved_path().to_path_buf(),
                external: selected.classification()
                    == crate::startup_context::StartupPathClassification::External,
            })
            .collect();
        let review = StartupCopyReview {
            id: ReviewId::new(),
            catalog_revision: expected_catalog_revision,
            source: source_binding.observed_path().to_path_buf(),
            target,
            target_path: target_binding.observed_path().to_path_buf(),
            target_binding_generation: target_binding.generation(),
            source_plan_revision: expected_source_plan_revision,
            target_plan_revision: expected_target_plan_revision,
            proposed_plan_revision: transition.proposed_revision(),
            entries,
        };
        let prepared = PreparedCopy {
            review: review.clone(),
            approvals: external_approvals,
            source_binding,
            target_binding,
            target_location_revision: location.revision,
            transition,
        };
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        organization::require_revision(&transaction, expected_catalog_revision)?;
        transaction
            .execute(
                "INSERT INTO reviews VALUES(?1,'startup_copy',?2)",
                params![review.id.to_string(), encode(&prepared)?],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(review)
    }

    fn startup_engine(&self) -> Result<StartupContext> {
        let state = self
            .root
            .parent()
            .ok_or_else(|| corrupt("Workspace has no durable state root"))?;
        Ok(StartupContext::from_durable_state_dir(state.to_path_buf()))
    }

    pub fn inspect_startup_copy(&self, request: RequestId) -> Result<StartupCopyRecord> {
        let _lease = self.lease(false)?;
        Ok(read_operation(&self.connection()?, request)?
            .ok_or_else(|| {
                issue(
                    IssueCode::InvalidIdentity,
                    "Unknown Startup Context copy request",
                )
            })?
            .public)
    }

    pub fn reserve_startup_copy(
        &self,
        request: RequestId,
        review: ReviewId,
    ) -> Result<StartupCopyIntent> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        if let Some(operation) = read_operation(&connection, request)? {
            if operation.public.review.id != review {
                return Err(issue(
                    IssueCode::Conflict,
                    "Copy request ID belongs to another review",
                ));
            }
            if operation.public.state != StartupCopyState::Complete {
                self.validate_copy_target(&connection, &operation.prepared)?;
            }
            return Ok(operation.intent());
        }
        let prepared: PreparedCopy =
            organization::read_review(&connection, review, "startup_copy")?;
        organization::require_revision(&connection, prepared.review.catalog_revision)?;
        self.validate_copy_target(&connection, &prepared)?;
        let engine = self.startup_engine()?;
        let resolved_source = self
            .resolver
            .resolve_directory(&prepared.source_binding)
            .map_err(io)?;
        if resolved_source.relocated || resolved_source.path != prepared.review.source {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Startup Context source changed after review",
            ));
        }
        let source = engine
            .resolve_project(&prepared.review.source)
            .map_err(io)?;
        if engine
            .load_project_plan(&source)
            .map_err(io)?
            .plan()
            .revision()
            != prepared.review.source_plan_revision
        {
            return Err(issue(
                IssueCode::Conflict,
                "Source Startup Context plan changed after review",
            ));
        }
        validate_transition(&engine, &prepared)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(operation) = read_operation(&transaction, request)? {
            if operation.public.review.id != review {
                return Err(issue(
                    IssueCode::Conflict,
                    "Copy request ID belongs to another review",
                ));
            }
            return Ok(operation.intent());
        }
        organization::require_revision(&transaction, prepared.review.catalog_revision)?;
        let public = StartupCopyRecord {
            request,
            operation: request.to_string().parse().map_err(corrupt)?,
            review: prepared.review.clone(),
            state: StartupCopyState::Pending,
            issue: None,
        };
        let operation = CopyOperation {
            public: public.clone(),
            prepared,
            backup_pending: false,
        };
        transaction
            .execute(
                "INSERT INTO operations VALUES(?1,'startup_copy','pending',?2)",
                params![public.operation.to_string(), encode(&operation)?],
            )
            .map_err(io)?;
        transaction
            .execute(
                "INSERT INTO operation_targets VALUES(?1,?2)",
                params![
                    public.operation.to_string(),
                    public.review.target.to_string()
                ],
            )
            .map_err(io)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("startup_copy_intent")?;
        Ok(operation.intent())
    }

    pub fn validate_startup_copy_intent(&self, request: RequestId) -> Result<StartupCopyIntent> {
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let operation = read_operation(&connection, request)?.ok_or_else(|| {
            issue(
                IssueCode::InvalidIdentity,
                "Unknown Startup Context copy request",
            )
        })?;
        if operation.public.state == StartupCopyState::Complete {
            return Ok(operation.intent());
        }
        self.validate_copy_target(&connection, &operation.prepared)?;
        if operation.public.state != StartupCopyState::Complete {
            let engine = self.startup_engine()?;
            let target = engine
                .resolve_project(&operation.prepared.review.target_path)
                .map_err(io)?;
            let current = engine.load_project_plan(&target).map_err(io)?;
            if !operation
                .prepared
                .transition
                .matches_applied_plan(current.plan())
                .map_err(io)?
            {
                validate_transition(&engine, &operation.prepared)?;
            }
        }
        Ok(operation.intent())
    }

    pub fn finish_startup_copy(&self, request: RequestId) -> Result<StartupCopyRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let operation = read_operation(&connection, request)?.ok_or_else(|| {
            issue(
                IssueCode::InvalidIdentity,
                "Unknown Startup Context copy request",
            )
        })?;
        if operation.public.state == StartupCopyState::Complete {
            return self.finish_startup_copy_backup(request);
        }
        self.validate_copy_target(&connection, &operation.prepared)?;
        let engine = self.startup_engine()?;
        let target = engine
            .resolve_project(&operation.prepared.review.target_path)
            .map_err(io)?;
        let applied = engine.load_project_plan(&target).map_err(io)?;
        if !operation
            .prepared
            .transition
            .matches_applied_plan(applied.plan())
            .map_err(io)?
        {
            return Err(issue(
                IssueCode::Conflict,
                "Startup Context plan has not committed this copy (or changed afterward); no success was inferred",
            ));
        }
        if operation.public.state != StartupCopyState::Complete {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(io)?;
            let mut current = read_operation(&transaction, request)?
                .ok_or_else(|| corrupt("Copy operation disappeared"))?;
            self.validate_copy_target(&transaction, &current.prepared)?;
            current.public.state = StartupCopyState::Complete;
            current.public.issue = None;
            current.backup_pending = true;
            transaction
                .execute(
                    "UPDATE operations SET state='complete',body=?2 WHERE id=?1",
                    params![current.public.operation.to_string(), encode(&current)?],
                )
                .map_err(io)?;
            transaction
                .execute(
                    "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                    [],
                )
                .map_err(io)?;
            transaction.commit().map_err(io)?;
            self.checkpoint("startup_copy_catalog_committed")?;
        }
        self.finish_startup_copy_backup(request)
    }

    pub fn record_startup_copy_issue(
        &self,
        request: RequestId,
        problem: Issue,
    ) -> Result<StartupCopyRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut operation = read_operation(&transaction, request)?.ok_or_else(|| {
            issue(
                IssueCode::InvalidIdentity,
                "Unknown Startup Context copy request",
            )
        })?;
        if operation.public.state == StartupCopyState::Complete {
            return Ok(operation.public);
        }
        operation.public.state = StartupCopyState::RecoveryRequired;
        operation.public.issue = Some(problem);
        transaction
            .execute(
                "UPDATE operations SET state='recovery_required',body=?2 WHERE id=?1",
                params![operation.public.operation.to_string(), encode(&operation)?],
            )
            .map_err(io)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(operation.public)
    }

    fn finish_startup_copy_backup(&self, request: RequestId) -> Result<StartupCopyRecord> {
        let current = self.inspect_startup_copy(request)?;
        let operation = read_operation(&self.connection()?, request)?
            .ok_or_else(|| corrupt("Copy receipt disappeared"))?;
        if !operation.backup_pending {
            return Ok(current);
        }
        let outcome = self.automatic_backup();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut operation = read_operation(&transaction, request)?
            .ok_or_else(|| corrupt("Copy receipt disappeared"))?;
        if operation.public.state != StartupCopyState::Complete {
            return Err(issue(IssueCode::Conflict, "Copy changed before backup"));
        }
        match outcome {
            Ok(()) => {
                operation.backup_pending = false;
                operation.public.issue = None;
            }
            Err(error) => {
                operation.public.issue = Some(issue(
                    IssueCode::BackupFailed,
                    format!(
                        "Startup Context copy is committed, but catalog backup remains pending: {error}"
                    ),
                ))
            }
        }
        transaction
            .execute(
                "UPDATE operations SET body=?2 WHERE id=?1",
                params![operation.public.operation.to_string(), encode(&operation)?],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(operation.public)
    }

    fn validate_copy_target(&self, connection: &Connection, prepared: &PreparedCopy) -> Result<()> {
        let (location, binding) = bound_target(connection, prepared.review.target)?;
        if location.revision != prepared.target_location_revision
            || binding != prepared.target_binding
        {
            return Err(issue(
                IssueCode::Conflict,
                "Copy target's catalog identity changed since review",
            ));
        }
        validate_target(&self.resolver, &location, &binding)
    }
}

impl CopyOperation {
    fn intent(&self) -> StartupCopyIntent {
        StartupCopyIntent {
            record: self.public.clone(),
            target_path: self.prepared.review.target_path.clone(),
            transition: self.prepared.transition.clone(),
        }
    }
}

fn read_operation(connection: &Connection, request: RequestId) -> Result<Option<CopyOperation>> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT body,state FROM operations WHERE id=?1 AND kind='startup_copy'",
            [request.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    row.map(|(body, state)| {
        let operation: CopyOperation = decode(&body)?;
        let expected = match operation.public.state {
            StartupCopyState::Pending => "pending",
            StartupCopyState::Complete => "complete",
            StartupCopyState::RecoveryRequired => "recovery_required",
        };
        if operation.public.request != request
            || operation.public.operation.to_string() != request.to_string()
            || state != expected
            || operation.public.review != operation.prepared.review
        {
            return Err(corrupt(
                "Startup Context copy operation identity or status differs from its journal",
            ));
        }
        Ok(operation)
    })
    .transpose()
}

fn validate_transition(engine: &StartupContext, prepared: &PreparedCopy) -> Result<()> {
    let target = engine
        .resolve_project(&prepared.review.target_path)
        .map_err(io)?;
    let mut approvals = prepared
        .approvals
        .iter()
        .map(|approval| {
            (
                approval.source_spec_id.as_str(),
                approval.approved_resolved_target.as_path(),
            )
        })
        .collect::<HashMap<_, _>>();
    let inputs = prepared
        .review
        .entries
        .iter()
        .map(|entry| {
            let mut input = StartupSelectionInput::new(&entry.selected_path);
            if let Some(approved) = approvals.remove(entry.source_spec_id.as_str()) {
                input = input.with_external_approval(approved);
            }
            input
        })
        .collect::<Vec<_>>();
    if !approvals.is_empty() {
        return Err(corrupt("Copy review has an unused external approval"));
    }
    let preview = engine.preview_selection(&target, inputs);
    if !preview.is_valid() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            format!(
                "Target Startup Context changed after review: {} invalid selection(s); review again",
                preview.issue_count()
            ),
        ));
    }
    let fresh = engine
        .prepare_project_plan_transition(&target, prepared.review.target_plan_revision, &preview)
        .map_err(io)?;
    if !prepared.transition.same_change(&fresh) {
        return Err(issue(
            IssueCode::Conflict,
            "Startup Context targets differ from reviewed selection; no copy was applied",
        ));
    }
    Ok(())
}

fn bound_target(
    connection: &Connection,
    target: LocationId,
) -> Result<(Location, PhysicalBinding)> {
    let Entity::Location(location) = entity(connection, EntityId::Location(target))? else {
        unreachable!()
    };
    let stored: String = connection
        .query_row(
            "SELECT body FROM bindings WHERE location=?1",
            [target.to_string()],
            |row| row.get(0),
        )
        .map_err(corrupt)?;
    let bound: BoundLocation = decode(&stored)?;
    Ok((location, bound.binding))
}

fn validate_target(
    resolver: &crate::location::volume::LocationResolver,
    location: &Location,
    binding: &PhysicalBinding,
) -> Result<()> {
    if location.lifecycle != LocationLifecycle::Ready
        || location.retired
        || location.observed_path != binding.observed_path()
        || location.volume_uuid != binding.volume().as_str()
        || location.binding_generation != binding.generation()
    {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Copy target is not a ready verified location",
        ));
    }
    let resolved = resolver.resolve_directory(binding).map_err(io)?;
    if resolved.relocated || resolved.path != binding.observed_path() {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Copy target physically moved; rebind it first",
        ));
    }
    Ok(())
}
