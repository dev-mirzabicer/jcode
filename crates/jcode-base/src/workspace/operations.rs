//! Read-only operation discovery over the existing ledger. Owners keep their
//! records; this module only projects the public record each owner stores.
use super::*;
use rusqlite::params;

const KINDS: [(OperationKind, &str, &str); 5] = [
    (OperationKind::Clone, "checkout_clone", "/public"),
    (OperationKind::Closeout, "closeout", "/record"),
    (OperationKind::StartupCopy, "startup_copy", "/public"),
    (OperationKind::PrimaryLaunch, "primary_launch", ""),
    (
        OperationKind::PrimaryLocation,
        "primary_location",
        "/record",
    ),
];

fn stored_kind(kind: OperationKind) -> &'static str {
    KINDS
        .iter()
        .find(|(value, ..)| *value == kind)
        .map(|(_, stored, _)| *stored)
        .expect("every operation kind has a ledger name")
}

fn decode_entry(
    kind: &str,
    state: &str,
    body: &str,
    targets: Vec<EntityId>,
) -> Result<OperationEntry> {
    let (kind, _, pointer) = KINDS
        .iter()
        .find(|(_, stored, _)| *stored == kind)
        .ok_or_else(|| corrupt(format!("Unsupported operation kind {kind}")))?;
    let value: serde_json::Value = decode(body)?;
    let public = value
        .pointer(pointer)
        .cloned()
        .ok_or_else(|| corrupt("Operation body has no public record"))?;
    let record = |error: serde_json::Error| corrupt(format!("Operation record: {error}"));
    let operation = match kind {
        OperationKind::Clone => {
            WorkspaceOperation::Clone(Box::new(serde_json::from_value(public).map_err(record)?))
        }
        OperationKind::Closeout => {
            WorkspaceOperation::Closeout(Box::new(serde_json::from_value(public).map_err(record)?))
        }
        OperationKind::StartupCopy => WorkspaceOperation::StartupCopy(Box::new(
            serde_json::from_value(public).map_err(record)?,
        )),
        OperationKind::PrimaryLaunch => WorkspaceOperation::PrimaryLaunch(Box::new(
            serde_json::from_value(public).map_err(record)?,
        )),
        OperationKind::PrimaryLocation => WorkspaceOperation::PrimaryLocation(Box::new(
            serde_json::from_value(public).map_err(record)?,
        )),
    };
    let state = match state {
        "pending" => OperationState::Pending,
        "complete" => OperationState::Complete,
        "failed" => OperationState::Failed,
        "recovery_required" => OperationState::RecoveryRequired,
        other => return Err(corrupt(format!("Unknown operation state {other}"))),
    };
    let mut targets = targets;
    let inferred = match &operation {
        WorkspaceOperation::Clone(record) => Some(EntityId::Location(record.location)),
        WorkspaceOperation::Closeout(record) => Some(EntityId::Location(record.spec.location)),
        WorkspaceOperation::StartupCopy(record) => Some(EntityId::Location(record.review.target)),
        WorkspaceOperation::PrimaryLaunch(record) => match record.input.placement {
            PrimaryPlacement::Existing { placement } => Some(query::placement_target(placement)),
            PrimaryPlacement::Standalone { .. } => None,
        },
        WorkspaceOperation::PrimaryLocation(record) => {
            Some(query::placement_target(record.input.placement))
        }
    };
    if let Some(target) = inferred
        && !targets.contains(&target)
    {
        targets.push(target);
    }
    Ok(OperationEntry {
        state,
        targets,
        operation,
    })
}

impl WorkspaceService {
    pub fn operations(
        &self,
        query: OperationQuery,
        after: Option<Cursor>,
        limit: u32,
    ) -> Result<OperationPage> {
        if !(1..=200).contains(&limit) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Page size must be 1 through 200. Continue for additional operations",
            ));
        }
        if query
            .session
            .as_ref()
            .is_some_and(|session| session.is_empty() || session.chars().any(char::is_control))
        {
            return Err(issue(IssueCode::InvalidInput, "Invalid Session identity"));
        }
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(io)?;
        let revision = storage::status(&transaction)?.revision;
        let query_digest = digest(encode(&query)?.as_bytes());
        let after_row = match &after {
            Some(cursor) if cursor.revision != revision || cursor.query_digest != query_digest => {
                return Err(issue(
                    IssueCode::Conflict,
                    "Operations changed or continuation belongs to another query; refresh from the first page",
                ));
            }
            Some(cursor) => Some(
                cursor
                    .after
                    .parse::<i64>()
                    .map_err(|_| issue(IssueCode::InvalidInput, "Invalid operation cursor"))?,
            ),
            None => None,
        };
        if let Some(target) = query.target {
            entity(&transaction, target)?;
        }
        let kinds = if query.kinds.is_empty() {
            KINDS.iter().map(|(kind, ..)| *kind).collect::<Vec<_>>()
        } else {
            query.kinds.clone()
        };
        let kinds = serde_json::to_string(&kinds.into_iter().map(stored_kind).collect::<Vec<_>>())
            .map_err(io)?;
        let target = query.target.map(|target| target.to_string());
        let filter = "o.kind IN (SELECT value FROM json_each(?1))
            AND (?2 IS NULL OR EXISTS(SELECT 1 FROM operation_targets t WHERE t.operation=o.id AND t.target=?2)
                OR json_extract(o.body,'$.public.location')=?2
                OR json_extract(o.body,'$.record.spec.location')=?2
                OR json_extract(o.body,'$.public.review.target')=?2
                OR (o.kind='primary_launch' AND json_extract(o.body,'$.input.placement.placement.id')=?2)
                OR (o.kind='primary_location' AND json_extract(o.body,'$.record.input.placement.id')=?2))
            AND (?3 IS NULL OR (o.kind='primary_launch' AND json_extract(o.body,'$.session')=?3)
                OR (o.kind='primary_location' AND json_extract(o.body,'$.record.input.session')=?3))
            AND (?4=0 OR o.state<>'complete')";
        let total: i64 = transaction
            .query_row(
                &format!("SELECT count(*) FROM operations o WHERE {filter}"),
                params![kinds, target, query.session, query.unfinished_only],
                |row| row.get(0),
            )
            .map_err(io)?;
        let mut statement = transaction
            .prepare(&format!(
                "SELECT o.rowid,o.id,o.kind,o.state,o.body FROM operations o WHERE {filter}
                 AND (?5 IS NULL OR o.rowid<?5) ORDER BY o.rowid DESC LIMIT ?6"
            ))
            .map_err(io)?;
        let rows = statement
            .query_map(
                params![
                    kinds,
                    target,
                    query.session,
                    query.unfinished_only,
                    after_row,
                    limit + 1
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(io)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io)?;
        let mut targets = transaction
            .prepare("SELECT target FROM operation_targets WHERE operation=?1 ORDER BY target")
            .map_err(io)?;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        let mut last_row = None;
        for (row, id, kind, state, body) in rows.iter().take(limit as usize) {
            let explicit = targets
                .query_map([id], |row| row.get::<_, String>(0))
                .map_err(io)?
                .map(|target| entity_id(&transaction, &target.map_err(io)?))
                .collect::<Result<Vec<_>>>()?;
            items.push(decode_entry(kind, state, body, explicit)?);
            last_row = Some(*row);
        }
        let next = (rows.len() > limit as usize)
            .then(|| last_row)
            .flatten()
            .map(|row| Cursor {
                revision,
                after: row.to_string(),
                query_digest,
            });
        Ok(OperationPage {
            revision,
            total: total.try_into().map_err(corrupt)?,
            items,
            next,
        })
    }
}

/// Resolve a stored identity string to its typed catalog identity.
fn entity_id(connection: &Connection, id: &str) -> Result<EntityId> {
    let kind: String = connection
        .query_row("SELECT kind FROM entities WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .map_err(io)?;
    let parse = |error: uuid::Error| corrupt(format!("Operation target {id}: {error}"));
    Ok(match kind.as_str() {
        "project" => EntityId::Project(id.parse().map_err(parse)?),
        "repository" => EntityId::Repository(id.parse().map_err(parse)?),
        "work_area" => EntityId::WorkArea(id.parse().map_err(parse)?),
        "location" => EntityId::Location(id.parse().map_err(parse)?),
        other => return Err(corrupt(format!("Unknown entity kind {other}"))),
    })
}
