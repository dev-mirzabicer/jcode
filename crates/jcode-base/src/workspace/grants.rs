//! Reviewed grants and durable proposals share catalog transactions and receipts.
use super::*;
use organization::{commit_receipt, read_review, replay, require_revision};
use rusqlite::{TransactionBehavior, params};

/// Only trusted application adapters construct this. It is deliberately not a wire type.
/// Same-user IPC is trusted, not physical-human attestation or shell containment.
pub struct WorkspaceClientAuthority(String);
impl WorkspaceClientAuthority {
    pub fn authenticated(client: impl Into<String>) -> Result<Self> {
        let client = client.into();
        if client.trim().is_empty() || client.chars().any(char::is_control) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Invalid authenticated client identity",
            ));
        }
        Ok(Self(client))
    }
}

#[derive(Serialize, Deserialize)]
struct PreparedGrant {
    review: GrantReview,
    bindings: Vec<PhysicalBinding>,
}

#[derive(Serialize, Deserialize)]
struct PermissionOperation {
    result: PermissionMutation,
    backup_pending: bool,
}

impl WorkspaceService {
    pub fn list_permissions(
        &self,
        query: PermissionQuery,
        after: Option<Cursor>,
        limit: u32,
    ) -> Result<PermissionPage> {
        if !(1..=200).contains(&limit) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Page size must be 1 through 200; continue for more records",
            ));
        }
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let revision = storage::status(&transaction)?.revision;
        let query_digest = digest(encode(&query)?.as_bytes());
        if after
            .as_ref()
            .is_some_and(|c| c.revision != revision || c.query_digest != query_digest)
        {
            return Err(issue(
                IssueCode::Conflict,
                "Permission page changed or belongs to another query",
            ));
        }
        let (table, filter, first, second) = match &query {
            PermissionQuery::Grants { audience } => (
                "grants",
                "(?1 IS NULL OR json_extract(body,'$.audience')=json(?1)) AND ?2 IS NULL",
                audience.as_ref().map(encode).transpose()?,
                None,
            ),
            PermissionQuery::Proposals { session, state } => (
                "proposals",
                "(?1 IS NULL OR json_extract(body,'$.session')=?1) AND (?2 IS NULL OR coalesce(json_extract(body,'$.state'),'pending')=json_extract(?2,'$'))",
                session.clone(),
                state.as_ref().map(encode).transpose()?,
            ),
        };
        let total: i64 = transaction
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {filter}"),
                params![first, second],
                |r| r.get(0),
            )
            .map_err(io)?;
        let mut statement = transaction
            .prepare(&format!(
                "SELECT id,body FROM {table} WHERE {filter} AND id>?3 ORDER BY id LIMIT ?4"
            ))
            .map_err(io)?;
        let mut rows = statement
            .query_map(
                params![
                    first,
                    second,
                    after.as_ref().map(|c| c.after.as_str()).unwrap_or(""),
                    limit + 1
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(io)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io)?;
        let next = if rows.len() > limit as usize {
            rows.pop();
            rows.last().map(|(id, _)| Cursor {
                revision,
                after: id.clone(),
                query_digest,
            })
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(|(_, body)| match query {
                PermissionQuery::Grants { .. } => decode(&body).map(PermissionItem::Grant),
                PermissionQuery::Proposals { .. } => decode(&body).map(PermissionItem::Proposal),
            })
            .collect::<Result<_>>()?;
        Ok(PermissionPage {
            revision,
            total: total.try_into().map_err(corrupt)?,
            items,
            next,
        })
    }
    pub fn inspect_grant(&self, id: GrantId) -> Result<GrantDefinition> {
        let _lease = self.lease(false)?;
        grant(&self.connection()?, id)
    }

    pub fn inspect_access_proposal(&self, id: ProposalId) -> Result<AccessProposal> {
        let _lease = self.lease(false)?;
        proposal(&self.connection()?, id)
    }

    pub fn review_grant_change(
        &self,
        expected: Revision,
        change: GrantChange,
    ) -> Result<GrantReview> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(io)?;
        require_revision(&transaction, expected)?;
        let mut value = match &change {
            GrantChange::Issue {
                audience,
                target,
                proposal: pending,
            } => {
                if let Some(id) = pending {
                    let pending = proposal(&transaction, *id)?;
                    if pending.state != AccessProposalState::Pending
                        || pending.target != *target
                        || *audience != Audience::Session(pending.session)
                    {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Access proposal does not match the exact pending audience and target",
                        ));
                    }
                }
                GrantDefinition {
                    id: GrantId::new(),
                    audience: audience.clone(),
                    target: target.clone(),
                    state: GrantState::Disabled,
                    revision: expected,
                    copied_from: None,
                    authorization: None,
                }
            }
            GrantChange::ActivateImported { grant: id } => {
                let value = grant(&transaction, *id)?;
                if value.state == GrantState::Revoked
                    || (value.state == GrantState::Active && value.authorization.is_some())
                {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Only an inactive imported or legacy definition can be activated",
                    ));
                }
                value
            }
            GrantChange::Revoke { grant: id } => grant(&transaction, *id)?,
        };
        let mut roots = Vec::new();
        let mut bindings = Vec::new();
        if !matches!(change, GrantChange::Revoke { .. }) {
            validate_audience(&transaction, &value.audience)?;
            validate_target(&transaction, &value.target)?;
            for root in scope::locations(&transaction)? {
                if scope::target_contains(&transaction, &value.target, &root)? {
                    bindings.push(self.verify_writable_root(&transaction, &root)?);
                    roots.push(root);
                }
            }
            value.state = GrantState::Active;
        } else {
            value.state = GrantState::Revoked;
        }
        let review = GrantReview {
            id: ReviewId::new(),
            revision: expected,
            change,
            grant: value,
            roots,
        };
        transaction.commit().map_err(io)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_revision(&transaction, expected)?;
        transaction
            .execute(
                "INSERT INTO reviews VALUES(?1,'grant',?2)",
                params![
                    review.id.to_string(),
                    encode(&PreparedGrant {
                        review: review.clone(),
                        bindings
                    })?
                ],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(review)
    }

    pub fn apply_grant_change(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        review: ReviewId,
    ) -> Result<PermissionMutation> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let input = digest(encode(&("grant", review))?.as_bytes());
        if let Some(receipt) = replay(&connection, request, &input)? {
            return self.finish_permission_mutation(permission_result(&connection, receipt)?);
        }
        let prepared: PreparedGrant = read_review(&connection, review, "grant")?;
        // Physical observation outside the short writer transaction. A revision
        // race invalidates this review instead of broadening its target set.
        for binding in &prepared.bindings {
            let resolved = self
                .resolver
                .resolve_directory(binding)
                .map_err(primary_location::location_issue)?;
            if resolved.relocated {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Grant target moved after review",
                ));
            }
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(receipt) = replay(&transaction, request, &input)? {
            let result = permission_result(&transaction, receipt)?;
            drop(transaction);
            return self.finish_permission_mutation(result);
        }
        require_revision(&transaction, prepared.review.revision)?;
        let mut value = prepared.review.grant;
        let mut pending = None;
        match &prepared.review.change {
            GrantChange::Issue { proposal: id, .. } => {
                if let Some(id) = id {
                    let mut p = proposal(&transaction, *id)?;
                    if p.state != AccessProposalState::Pending {
                        return Err(issue(IssueCode::Conflict, "Proposal is no longer pending"));
                    }
                    p.state = AccessProposalState::Approved;
                    p.grant = Some(value.id);
                    pending = Some(p);
                }
            }
            GrantChange::ActivateImported { .. } | GrantChange::Revoke { .. } => {
                let current = grant(&transaction, value.id)?;
                if current.revision != value.revision {
                    return Err(issue(IssueCode::Conflict, "Grant changed since review"));
                }
            }
        }
        if value.state == GrantState::Active {
            validate_audience(&transaction, &value.audience)?;
            value.authorization = Some(GrantAuthorization {
                client: client.0.clone(),
                request,
                installation: storage::status(&transaction)?.installation,
            });
        }
        let targets = portable::grant_targets(&value);
        let receipt = commit_receipt(&transaction, request, &input, targets)?;
        value.revision = receipt.revision;
        if matches!(prepared.review.change, GrantChange::Issue { .. }) {
            portable::save_grant(&transaction, &value)?;
        } else {
            transaction
                .execute(
                    "UPDATE grants SET body=?1 WHERE id=?2",
                    params![encode(&value)?, value.id.to_string()],
                )
                .map_err(io)?;
        }
        if let Some(p) = &mut pending {
            p.revision = receipt.revision;
            transaction
                .execute(
                    "UPDATE proposals SET body=?1 WHERE id=?2",
                    params![encode(p)?, p.id.to_string()],
                )
                .map_err(io)?;
        }
        let result = PermissionMutation {
            receipt,
            grant: Some(value),
            proposal: pending,
        };
        save_permission_result(&transaction, &result)?;
        self.checkpoint("grant_before_commit")?;
        transaction.commit().map_err(io)?;
        self.checkpoint("grant_committed")?;
        self.finish_permission_mutation(result)
    }

    /// Agent-facing proposal creation cannot select an audience or authorize it.
    /// Its caller supplies the actual Session, not model-authored Session metadata.
    pub fn request_access(
        &self,
        session: &crate::session::Session,
        request: RequestId,
        target: WriteTarget,
        reason: String,
    ) -> Result<PermissionMutation> {
        if session.isolated_child.is_some() {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Workspace access proposals belong to the original primary parent",
            ));
        }
        let location = session.location.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Adopt the legacy Session before proposing workspace access",
            )
        })?;
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let input = digest(encode(&("access", &session.id, &target, &reason))?.as_bytes());
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(receipt) = replay(&transaction, request, &input)? {
            let result = permission_result(&transaction, receipt)?;
            drop(transaction);
            return self.finish_permission_mutation(result);
        }
        query::validate_placement(&transaction, location.placement)?;
        validate_target(&transaction, &target)?;
        let target_id = target_entity(&target);
        let receipt = commit_receipt(&transaction, request, &input, vec![target_id])?;
        let pending = AccessProposal {
            id: ProposalId::new(),
            session: session.id.clone(),
            target,
            revision: receipt.revision,
            state: AccessProposalState::Pending,
            reason,
            grant: None,
        };
        transaction
            .execute(
                "INSERT INTO proposals VALUES(?1,?2)",
                params![pending.id.to_string(), encode(&pending)?],
            )
            .map_err(io)?;
        transaction
            .execute(
                "INSERT INTO proposal_references VALUES(?1,?2)",
                params![pending.id.to_string(), target_id.to_string()],
            )
            .map_err(io)?;
        let result = PermissionMutation {
            receipt,
            grant: None,
            proposal: Some(pending),
        };
        save_permission_result(&transaction, &result)?;
        transaction.commit().map_err(io)?;
        self.finish_permission_mutation(result)
    }

    fn finish_permission_mutation(&self, result: PermissionMutation) -> Result<PermissionMutation> {
        let lock = storage::private_file(
            &self
                .root
                .join("leases")
                .join(format!("permission-{}.lock", result.receipt.operation)),
            false,
        )?;
        lock.lock().map_err(io)?;
        let connection = self.connection()?;
        let body: String = connection
            .query_row(
                "SELECT body FROM operations WHERE id=?1 AND kind='permission'",
                [result.receipt.operation.to_string()],
                |r| r.get(0),
            )
            .map_err(corrupt)?;
        let mut stored: PermissionOperation = decode(&body)?;
        if stored.backup_pending {
            stored
                .result
                .receipt
                .issues
                .retain(|issue| issue.code != IssueCode::BackupFailed);
            stored.result.receipt = self.after_mutation(stored.result.receipt)?;
            stored.backup_pending = stored
                .result
                .receipt
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::BackupFailed);
            let transaction = connection.unchecked_transaction().map_err(io)?;
            transaction
                .execute(
                    "UPDATE operations SET body=?1 WHERE id=?2",
                    params![
                        encode(&stored)?,
                        stored.result.receipt.operation.to_string()
                    ],
                )
                .map_err(io)?;
            transaction
                .execute(
                    "UPDATE receipts SET body=?1 WHERE request=?2",
                    params![
                        encode(&stored.result.receipt)?,
                        stored.result.receipt.request.to_string()
                    ],
                )
                .map_err(io)?;
            transaction.commit().map_err(io)?;
        }
        Ok(stored.result)
    }
}

fn save_permission_result(connection: &Connection, result: &PermissionMutation) -> Result<()> {
    connection
        .execute(
            "INSERT INTO operations VALUES(?1,'permission','complete',?2)",
            params![
                result.receipt.operation.to_string(),
                encode(&PermissionOperation {
                    result: result.clone(),
                    backup_pending: true
                })?
            ],
        )
        .map_err(io)?;
    Ok(())
}
fn permission_result(connection: &Connection, receipt: Receipt) -> Result<PermissionMutation> {
    let body: String = connection
        .query_row(
            "SELECT body FROM operations WHERE id=?1 AND kind='permission' AND state='complete'",
            [receipt.operation.to_string()],
            |r| r.get(0),
        )
        .map_err(corrupt)?;
    let mut result = decode::<PermissionOperation>(&body)?.result;
    result.receipt = receipt;
    Ok(result)
}
fn grant(connection: &Connection, id: GrantId) -> Result<GrantDefinition> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM grants WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(io)?;
    let value: GrantDefinition =
        decode(&body.ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown grant"))?)?;
    if value.id != id {
        return Err(corrupt("Grant identity mismatch"));
    }
    Ok(value)
}
fn proposal(connection: &Connection, id: ProposalId) -> Result<AccessProposal> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM proposals WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(io)?;
    let value: AccessProposal =
        decode(&body.ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown access proposal"))?)?;
    if value.id != id {
        return Err(corrupt("Proposal identity mismatch"));
    }
    Ok(value)
}
fn target_entity(target: &WriteTarget) -> EntityId {
    match *target {
        WriteTarget::Root(id) => EntityId::Location(id),
        WriteTarget::ProjectMembers(id) => EntityId::Project(id),
        WriteTarget::WorkAreaMembers(id) => EntityId::WorkArea(id),
    }
}
fn validate_target(connection: &Connection, target: &WriteTarget) -> Result<()> {
    let target = entity(connection, target_entity(target))?;
    if matches!(
        target,
        Entity::Project(Project {
            state: OrganizationState::Retired,
            ..
        }) | Entity::WorkArea(WorkArea {
            state: OrganizationState::Retired,
            ..
        }) | Entity::Location(Location { retired: true, .. })
    ) {
        return Err(issue(
            IssueCode::InvalidIdentity,
            "Permission target is retired",
        ));
    }
    Ok(())
}
fn validate_audience(connection: &Connection, audience: &Audience) -> Result<()> {
    match audience {
        Audience::Session(id) => {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(issue(
                    IssueCode::InvalidIdentity,
                    "Invalid Session audience",
                ));
            }
            let session = crate::session::Session::load_startup_stub(id).map_err(io)?;
            let location = session.location.ok_or_else(|| {
                issue(
                    IssueCode::RecoveryRequired,
                    "Session audience needs explicit location adoption",
                )
            })?;
            query::validate_placement(connection, location.placement)
        }
        Audience::Project(id) => validate_target(connection, &WriteTarget::ProjectMembers(*id)),
        Audience::WorkArea(id) => validate_target(connection, &WriteTarget::WorkAreaMembers(*id)),
        Audience::Checkout(id) => {
            if !matches!(
                entity(connection, EntityId::Location(*id))?,
                Entity::Location(Location {
                    kind: LocationKind::Checkout { .. },
                    retired: false,
                    ..
                })
            ) {
                return Err(issue(
                    IssueCode::InvalidIdentity,
                    "Grant audience must be a registered checkout",
                ));
            }
            Ok(())
        }
    }
}
