//! Reviewed new-context scope: Session checkpoint first, copied grants and index
//! in one catalog transaction. No transcript or inference is owned here.
use super::*;
use crate::session::{Session, StoredSessionLocation};
use rusqlite::{TransactionBehavior, params};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Selection {
    installation: InstallationId,
    source: String,
    location: StoredSessionLocation,
    revision: Revision,
    direct: Vec<GrantDefinition>,
    choice: Option<GrantCarryChoice>,
    client: String,
}

/// Code-owned preparation, not serializable client authority. Retains catalog
/// replacement exclusion, but never postpones ordinary grant revocation.
pub struct ContextScopePlan {
    selection: Selection,
    _lease: CatalogLease,
}
impl ContextScopePlan {
    pub(super) fn source_location(&self) -> &StoredSessionLocation {
        &self.selection.location
    }
    pub(super) fn grants_for_copy(&self, connection: &Connection) -> Result<&[GrantDefinition]> {
        validate_selection(connection, &self.selection)?;
        Ok(if self.selection.choice.is_some_and(|c| c.carry) {
            &self.selection.direct
        } else {
            &[]
        })
    }
}
#[derive(Serialize, Deserialize)]
struct ScopeCopy {
    operation: OperationId,
    target: String,
    kind: NewContextKind,
    selection: Selection,
    location: StoredSessionLocation,
    copies: Vec<GrantDefinition>,
    receipt: Option<Receipt>,
    backup_pending: bool,
}

impl WorkspaceService {
    pub fn context_scope_status(&self, review: ReviewId) -> Result<Vec<ContextScopeStatus>> {
        self.scope_status(Some(review), None)
    }
    pub fn context_scope_status_for_session(
        &self,
        session: &str,
    ) -> Result<Vec<ContextScopeStatus>> {
        self.scope_status(None, Some(session))
    }
    fn scope_status(
        &self,
        review: Option<ReviewId>,
        session: Option<&str>,
    ) -> Result<Vec<ContextScopeStatus>> {
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let mut query=connection.prepare("SELECT state,body FROM operations WHERE kind='context_scope' AND (?1 IS NULL OR json_extract(body,'$.selection.choice.review')=?1) AND (?2 IS NULL OR json_extract(body,'$.target')=?2) ORDER BY rowid").map_err(io)?;
        query
            .query_map(params![review.map(|id| id.to_string()), session], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(io)?
            .map(|row| {
                let (state, body) = row.map_err(io)?;
                let copy: ScopeCopy = decode(&body)?;
                Ok(ContextScopeStatus {
                    review: copy.selection.choice.map(|choice| choice.review),
                    operation: copy.operation,
                    source: copy.selection.source,
                    target: copy.target,
                    kind: copy.kind,
                    state: serde_json::from_value(serde_json::Value::String(state))
                        .map_err(corrupt)?,
                    revision: copy.receipt.map(|r| r.revision),
                    backup_pending: copy.backup_pending,
                })
            })
            .collect()
    }
    pub fn abandon_unpublished_context_scope(&self, session: &str) -> Result<()> {
        if !self.catalog_present()? {
            return Ok(());
        }
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut statement = transaction.prepare("SELECT body FROM operations WHERE kind='context_scope' AND json_extract(body,'$.target')=?1 AND state!='failed'").map_err(io)?;
        let copies = statement
            .query_map([session], |row| row.get::<_, String>(0))
            .map_err(io)?
            .map(|row| decode::<ScopeCopy>(&row.map_err(io)?))
            .collect::<Result<Vec<_>>>()?;
        drop(statement);
        if copies.iter().any(|copy| copy.receipt.is_some()) {
            return Err(issue(
                IssueCode::Referenced,
                format!(
                    "Context scope for {session} was published; retain the Session and reconcile its receipt instead of deleting it"
                ),
            ));
        }
        for copy in copies {
            transaction
                .execute(
                    "UPDATE operations SET state='failed' WHERE id=?1",
                    [copy.operation.to_string()],
                )
                .map_err(io)?;
        }
        transaction.commit().map_err(io)
    }
    pub fn review_grant_carry(&self, source: &str) -> Result<GrantCarryReview> {
        let source = Session::load_startup_stub(source).map_err(io)?;
        let location = source.location.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Review legacy placement before managed context creation",
            )
        })?;
        if source.isolated_child.is_some() {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Isolated children cannot become primary continuations",
            ));
        }
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let review = GrantCarryReview {
            id: ReviewId::new(),
            source: source.id.clone(),
            session_revision: location.revision,
            catalog_revision: storage::status(&transaction)?.revision,
            direct_grants: direct_grants(&transaction, &source.id)?,
        };
        transaction
            .execute(
                "INSERT INTO reviews VALUES(?1,'grant_carry',?2)",
                params![review.id.to_string(), encode(&(review.clone(), location))?],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(review)
    }

    /// Run before Session allocation, transfer inference or any new-context effects.
    pub fn prepare_context_scope(
        &self,
        source: &Session,
        choice: Option<GrantCarryChoice>,
        client: &WorkspaceClientAuthority,
    ) -> Result<Option<ContextScopePlan>> {
        source
            .validate_primary_publication(self)
            .map_err(|error| issue(IssueCode::RecoveryRequired, error.to_string()))?;
        if source.isolated_child.is_some() {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Isolated children cannot create primary contexts",
            ));
        }
        let Some(location) = &source.location else {
            if source.scope_copy.is_some() {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Context scope binding is missing",
                ));
            }
            self.require_legacy_scope_absent(&source.id)?;
            if crate::config::config().features.managed_primary_launch {
                return Err(issue(
                    IssueCode::NeedsCwd,
                    "Adopt this legacy Session's placement and cwd before creating a managed continuation",
                ));
            }
            if choice.is_some() {
                return Err(issue(
                    IssueCode::Conflict,
                    "An unbound legacy context cannot use a grant-carry review",
                ));
            }
            return Ok(None);
        };
        let lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let current = Session::load_startup_stub(&source.id).map_err(io)?;
        if current.location.as_ref() != Some(location) {
            return Err(issue(
                IssueCode::Conflict,
                "Source placement changed before context preparation",
            ));
        }
        let revision = storage::status(&transaction)?.revision;
        validate_source_location(self, source)?;
        let direct = direct_grants(&transaction, &source.id)?;
        if let Some(choice) = choice {
            let (review, binding): (GrantCarryReview, StoredSessionLocation) =
                organization::read_review(&transaction, choice.review, "grant_carry")?;
            if review.source != source.id
                || binding != *location
                || review.catalog_revision != revision
                || review.direct_grants != direct
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Grant-carry review is stale; inspect the current grants and choose again",
                ));
            }
        } else if !direct.is_empty() {
            return Err(issue(
                IssueCode::NeedsGrantChoice,
                "Review direct grants and explicitly choose carry or do not carry before creating the new context",
            ));
        }
        let plan = ContextScopePlan {
            selection: Selection {
                installation: storage::status(&transaction)?.installation,
                source: source.id.clone(),
                location: location.clone(),
                revision,
                direct,
                choice,
                client: client.0.clone(),
            },
            _lease: lease,
        };
        self.prepare_context_location(&plan, OperationId::new())?;
        Ok(Some(plan))
    }

    /// Install before the destination's first checkpoint. Failed preparation may
    /// remove its unpublished Session. An interrupted committed candidate stays
    /// non-runnable until reconciliation proves the exact scope publication.
    pub fn stage_context_scope(
        &self,
        plan: &ContextScopePlan,
        target: &mut Session,
        kind: NewContextKind,
    ) -> Result<()> {
        self.resolver
            .resolve_directory(&plan.selection.location.cwd)
            .map_err(primary_location::location_issue)?;
        if target.id == plan.selection.source || target.isolated_child.is_some() {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "New context needs an independent primary identity",
            ));
        }
        if let Some(checkpoint) = target.scope_copy {
            let copy = self.read_scope_copy(checkpoint.operation)?;
            if copy.target != target.id || copy.selection != plan.selection || copy.kind != kind {
                return Err(issue(
                    IssueCode::Conflict,
                    "Context scope already has different intent",
                ));
            }
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        validate_selection(&transaction, &plan.selection)?;
        if let Some(choice) = plan.selection.choice {
            let used:Option<String>=transaction.query_row("SELECT json_extract(body,'$.target') FROM operations WHERE kind='context_scope' AND json_extract(body,'$.selection.choice.review')=?1",[choice.review.to_string()],|row|row.get(0)).optional().map_err(io)?;
            if let Some(target) = used {
                return Err(issue(
                    IssueCode::Conflict,
                    format!(
                        "Carry review is already bound to {target}; inspect its context-scope status rather than creating another Session"
                    ),
                ));
            }
        }
        let operation = OperationId::new();
        let mut location = plan.selection.location.clone();
        location.revision = 1;
        if kind != NewContextKind::Split {
            location.initial_cwd = location.cwd.observed_path().into();
        }
        location.last_operation = Some(
            target
                .primary_creation
                .as_ref()
                .map(|c| c.operation)
                .unwrap_or(operation),
        );
        if let Some(existing) = &target.location
            && (existing.placement != location.placement || existing.cwd != location.cwd)
        {
            return Err(issue(
                IssueCode::Conflict,
                "New context location differs from reviewed source",
            ));
        }
        let mut copies = Vec::new();
        if plan.selection.choice.is_some_and(|choice| choice.carry) {
            for source in &plan.selection.direct {
                let mut copy = source.clone();
                copy.id = GrantId::new();
                copy.audience = Audience::Session(target.id.clone());
                copy.copied_from = Some(source.id);
                copy.authorization = Some(GrantAuthorization {
                    client: plan.selection.client.clone(),
                    request: operation.to_string().parse().map_err(corrupt)?,
                    installation: storage::status(&transaction)?.installation,
                });
                copies.push(copy);
            }
        }
        let record = ScopeCopy {
            operation,
            target: target.id.clone(),
            kind,
            selection: plan.selection.clone(),
            location: location.clone(),
            copies,
            receipt: None,
            backup_pending: false,
        };
        transaction
            .execute(
                "INSERT INTO operations VALUES(?1,'context_scope','pending',?2)",
                params![operation.to_string(), encode(&record)?],
            )
            .map_err(io)?;
        transaction
            .execute(
                "INSERT INTO operation_targets VALUES(?1,?2)",
                params![
                    operation.to_string(),
                    query::placement_target(location.placement).to_string()
                ],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        target.scope_copy = Some(crate::session::StoredContextScope {
            operation,
            ready: false,
        });
        target.location = Some(location);
        target.working_dir = Some(record.location.cwd.observed_path().to_string_lossy().into());
        self.checkpoint("context_scope_intent")?;
        Ok(())
    }

    pub fn reconcile_context_scope(&self, session: &str) -> Result<()> {
        self.reconcile_scope_checkpoint(session)
            .map_err(|mut problem| {
                problem.detail = format!(
                    "New context {session} is retained for scope reconciliation: {}",
                    problem.detail
                );
                problem
            })
    }
    fn reconcile_scope_checkpoint(&self, session: &str) -> Result<()> {
        let _control = self.primary_control_lease(session)?;
        let source = Session::load_startup_stub(session).map_err(io)?;
        let Some(checkpoint) = source.scope_copy else {
            return Ok(());
        };
        if !checkpoint.ready
            || source
                .primary_creation
                .as_ref()
                .is_some_and(|creation| !creation.ready)
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Context preparation has not committed its ready checkpoint",
            ));
        }
        let operation = checkpoint.operation;
        if self.read_scope_copy(operation)?.receipt.is_none()
            && let Some(location) = &source.location
        {
            let resolved = self
                .resolver
                .resolve_directory(&location.cwd)
                .map_err(primary_location::location_issue)?;
            if resolved.relocated {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "New context cwd moved before publication",
                ));
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut copy = read_copy(&transaction, operation)?;
        if copy.target != session {
            return Err(corrupt("Context copy belongs to another Session"));
        }
        if copy.receipt.is_none() {
            if source.location.as_ref() != Some(&copy.location) {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Context scope has no matching Session checkpoint",
                ));
            }
            validate_selection(&transaction, &copy.selection)?;
            let request: RequestId = operation.to_string().parse().map_err(corrupt)?;
            let digest = digest(encode(&(session, &copy.selection, copy.kind))?.as_bytes());
            let mut receipt = organization::commit_receipt(
                &transaction,
                request,
                &digest,
                vec![query::placement_target(copy.location.placement)],
            )?;
            receipt.operation = operation;
            transaction
                .execute(
                    "UPDATE receipts SET body=?1 WHERE request=?2",
                    params![encode(&receipt)?, request.to_string()],
                )
                .map_err(io)?;
            for grant in &mut copy.copies {
                grant.revision = receipt.revision;
                portable::save_grant(&transaction, grant)?;
            }
            let index = SessionIndex {
                session: session.into(),
                placement: copy.location.placement,
                session_revision: copy.location.revision,
                operation: copy
                    .location
                    .last_operation
                    .ok_or_else(|| corrupt("Context location lacks operation"))?,
                active: true,
                reconciled: true,
            };
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT body FROM session_index WHERE session=?1",
                    [session],
                    |row| row.get(0),
                )
                .optional()
                .map_err(io)?;
            if existing
                .as_ref()
                .map(|value| decode::<SessionIndex>(value))
                .transpose()?
                .is_some_and(|known| known != index)
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Destination scope index already has different authority",
                ));
            }
            transaction.execute("INSERT INTO session_index VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET target=excluded.target,body=excluded.body",params![session,query::placement_target(index.placement).to_string(),encode(&index)?]).map_err(io)?;
            copy.receipt = Some(receipt);
            copy.backup_pending = true;
            write_copy(&transaction, &copy)?;
            self.checkpoint("context_scope_before_commit")?;
        }
        transaction.commit().map_err(io)?;
        self.checkpoint("context_scope_committed")?;
        self.finish_scope_backup(copy)
    }

    pub fn validate_context_scope(&self, session: &Session) -> Result<()> {
        let Some(checkpoint) = session.scope_copy else {
            return Ok(());
        };
        let copy = self.read_scope_copy(checkpoint.operation)?;
        if !checkpoint.ready
            || copy.target != session.id
            || copy.receipt.is_none()
            || session.location.is_none()
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "New-context scope publication is incomplete; reconcile its retained operation",
            ));
        }
        Ok(())
    }
    pub fn resume_context_scope(&self, session: &Session) -> Result<()> {
        let Some(checkpoint) = session.scope_copy else {
            return Ok(());
        };
        let copy = self.read_scope_copy(checkpoint.operation)?;
        if copy.target != session.id || !checkpoint.ready {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "New context preparation is incomplete",
            ));
        }
        if copy.receipt.is_none() || copy.backup_pending {
            self.reconcile_context_scope(&session.id)?;
        }
        if let Some(creation) = &session.primary_creation
            && self.inspect_primary_launch(creation.request)?.state != PrimaryLaunchState::Complete
        {
            self.reconcile_primary_launch(creation.request)?;
        }
        Ok(())
    }
    fn read_scope_copy(&self, operation: OperationId) -> Result<ScopeCopy> {
        let _lease = self.lease(false)?;
        read_copy(&self.connection()?, operation)
    }
    fn finish_scope_backup(&self, mut copy: ScopeCopy) -> Result<()> {
        if copy.backup_pending {
            let receipt = copy
                .receipt
                .as_mut()
                .ok_or_else(|| corrupt("Context scope receipt missing"))?;
            receipt.issues.retain(|i| i.code != IssueCode::BackupFailed);
            *receipt = self.after_mutation(receipt.clone())?;
            copy.backup_pending = receipt
                .issues
                .iter()
                .any(|i| i.code == IssueCode::BackupFailed);
            let connection = self.connection()?;
            let transaction = connection.unchecked_transaction().map_err(io)?;
            write_copy(&transaction, &copy)?;
            transaction
                .execute(
                    "UPDATE receipts SET body=?1 WHERE request=?2",
                    params![
                        encode(copy.receipt.as_ref().unwrap())?,
                        copy.operation.to_string()
                    ],
                )
                .map_err(io)?;
            transaction.commit().map_err(io)?;
            if copy.backup_pending {
                return Err(issue(
                    IssueCode::BackupFailed,
                    "Context scope committed, but its backup needs reconciliation",
                ));
            }
        }
        Ok(())
    }
}
fn direct_grants(connection: &Connection, session: &str) -> Result<Vec<GrantDefinition>> {
    let installation = storage::status(connection)?.installation;
    Ok(portable::grants(connection)?
        .into_iter()
        .filter(|g| {
            g.audience == Audience::Session(session.into())
                && g.state == GrantState::Active
                && g.authorization
                    .as_ref()
                    .is_some_and(|a| a.installation == installation)
        })
        .collect())
}
fn validate_selection(connection: &Connection, selected: &Selection) -> Result<()> {
    if storage::status(connection)?.installation != selected.installation {
        return Err(issue(
            IssueCode::Conflict,
            "Context scope belongs to another installation",
        ));
    }
    organization::require_revision(connection, selected.revision)?;
    let current = Session::load_startup_stub(&selected.source).map_err(io)?;
    if current.location.as_ref() != Some(&selected.location)
        || current.isolated_child.is_some()
        || direct_grants(connection, &selected.source)? != selected.direct
    {
        return Err(issue(
            IssueCode::Conflict,
            "Source scope changed during context preparation; review before retrying",
        ));
    }
    Ok(())
}

fn validate_source_location(service: &WorkspaceService, source: &Session) -> Result<()> {
    let location = source
        .location
        .as_ref()
        .ok_or_else(|| corrupt("Managed source lost its location"))?;
    if source.working_dir.as_deref().map(Path::new) != Some(location.cwd.observed_path()) {
        return Err(issue(
            IssueCode::Conflict,
            "Source cwd differs from its binding",
        ));
    }
    let resolved = service
        .resolver
        .resolve_directory(&location.cwd)
        .map_err(primary_location::location_issue)?;
    if resolved.relocated {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Source cwd moved before context preparation",
        ));
    }
    primary_location::validate_primary_placement(&service.connection()?, location.placement)
}
fn read_copy(connection: &Connection, id: OperationId) -> Result<ScopeCopy> {
    let (state, body): (String, String) = connection
        .query_row(
            "SELECT state,body FROM operations WHERE id=?1 AND kind='context_scope'",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(io)?;
    let copy: ScopeCopy = decode(&body)?;
    if copy.operation != id
        || !matches!(state.as_str(), "pending" | "complete")
        || (state == "complete") != copy.receipt.is_some()
    {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Context scope intent is unavailable or invalidated by restore",
        ));
    }
    Ok(copy)
}
fn write_copy(connection: &Connection, copy: &ScopeCopy) -> Result<()> {
    connection
        .execute(
            "UPDATE operations SET state=?1,body=?2 WHERE id=?3 AND kind='context_scope'",
            params![
                if copy.receipt.is_some() {
                    "complete"
                } else {
                    "pending"
                },
                encode(copy)?,
                copy.operation.to_string()
            ],
        )
        .map_err(io)?;
    Ok(())
}
