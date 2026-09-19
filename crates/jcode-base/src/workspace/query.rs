use super::*;
use rusqlite::{TransactionBehavior, params};

const VIEW: &str = "WITH locations AS (
    SELECT e.*,coalesce(e.home_project,a.area_project) owner_project,
    coalesce(json_extract(e.body,'$.value.state'), CASE WHEN json_extract(e.body,'$.value.retired')=1 THEN 'retired' ELSE 'active' END) own_state,
    coalesce(json_extract(a.body,'$.value.state'),'active') area_state
    FROM entities e LEFT JOIN entities a ON a.id=e.home_area
), visible AS (
    SELECT l.*,coalesce(json_extract(p.body,'$.value.state'),'active') project_state,
    EXISTS(SELECT 1 FROM session_index s LEFT JOIN entities sl ON sl.id=s.target LEFT JOIN entities sa ON sa.id=sl.home_area
        WHERE json_extract(s.body,'$.active')=1 AND
        (s.target=l.id OR sl.home_area=l.id OR sl.home_project=l.id OR sl.area_project=l.id OR sa.area_project=l.id)) has_active
    FROM locations l LEFT JOIN entities p ON p.id=coalesce(l.owner_project,l.area_project)
) ";

impl WorkspaceService {
    pub fn list(&self, query: Query, after: Option<Cursor>, limit: u32) -> Result<Page> {
        page_limit(limit)?;
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(io)?;
        let revision = storage::status(&transaction)?.revision;
        let query_digest = digest(encode(&query)?.as_bytes());
        if let Some(cursor) = &after {
            if cursor.revision != revision || cursor.query_digest != query_digest {
                return Err(issue(
                    IssueCode::Conflict,
                    "List changed or continuation belongs to another query; refresh from the first page",
                ));
            }
        }
        let kind = query.kind.map(|v| match v {
            EntityKind::Project => "project",
            EntityKind::Repository => "repository",
            EntityKind::WorkArea => "work_area",
            EntityKind::Location => "location",
        });
        let project = query.project.map(|v| v.to_string());
        let repository = query.repository.map(|v| v.to_string());
        let (home_project, home_area) = match query.home {
            Some(Home::Project(id)) => (Some(id.to_string()), None),
            Some(Home::WorkArea(id)) => (None, Some(id.to_string())),
            None => (None, None),
        };
        let visibility = match query.visibility {
            Visibility::All => "1",
            Visibility::Current => {
                "(has_active OR (own_state='active' AND area_state='active' AND project_state='active' AND coalesce(json_extract(body,'$.value.lifecycle'),'ready')!='closed'))"
            }
            Visibility::Archived => {
                "(own_state='archived' OR area_state='archived' OR project_state='archived')"
            }
            Visibility::Retired => "own_state='retired'",
            Visibility::Closed => "json_extract(body,'$.value.lifecycle')='closed'",
        };
        let filter=format!(" WHERE (?1 IS NULL OR kind=?1)
            AND (?2 IS NULL OR owner_project=?2 OR area_project=?2 OR (kind='repository' AND EXISTS(SELECT 1 FROM associations WHERE project=?2 AND repository=visible.id)))
            AND (?3 IS NULL OR home_project=?3) AND (?4 IS NULL OR home_area=?4)
            AND (?5 IS NULL OR repository=?5) AND (?6=0 OR has_active) AND {visibility}");
        let args = params![
            kind,
            project,
            home_project,
            home_area,
            repository,
            query.active_sessions_only
        ];
        let total: i64 = transaction
            .query_row(
                &format!("{VIEW} SELECT count(*) FROM visible{filter}"),
                args,
                |r| r.get(0),
            )
            .map_err(io)?;
        let mut statement = transaction
            .prepare(&format!(
                "{VIEW} SELECT body FROM visible{filter} AND id>?7 ORDER BY id LIMIT ?8"
            ))
            .map_err(io)?;
        let rows = statement
            .query_map(
                params![
                    kind,
                    project,
                    home_project,
                    home_area,
                    repository,
                    query.active_sessions_only,
                    after.as_ref().map(|v| v.after.as_str()).unwrap_or(""),
                    limit + 1
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(io)?;
        let mut items = rows
            .map(|r| decode(&r.map_err(io)?))
            .collect::<Result<Vec<Entity>>>()?;
        let next = if items.len() > limit as usize {
            items.pop();
            items.last().map(|v| Cursor {
                revision,
                after: v.id().to_string(),
                query_digest,
            })
        } else {
            None
        };
        Ok(Page {
            revision,
            total: total.try_into().map_err(corrupt)?,
            items,
            next,
        })
    }

    /// Called only by a Session checkpoint owner after its authoritative commit.
    /// Not exposed as a trusted-client mutation: clients cannot manufacture Session facts.
    pub fn reconcile_session_index(&self, index: &SessionIndex) -> Result<()> {
        if index.session.is_empty() || index.session.chars().any(char::is_control) {
            return Err(issue(IssueCode::InvalidInput, "Invalid Session identity"));
        }
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let target = placement_target(index.placement);
        validate_placement(&transaction, index.placement)?;
        let old: Option<String> = transaction
            .query_row(
                "SELECT body FROM session_index WHERE session=?1",
                [&index.session],
                |r| r.get(0),
            )
            .optional()
            .map_err(io)?;
        if let Some(old) = old {
            let old: SessionIndex = decode(&old)?;
            if old == *index {
                return Ok(());
            }
            if old.session_revision > index.session_revision
                || (old.session_revision == index.session_revision
                    && (old.operation != index.operation || old.placement != index.placement))
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Session index is newer or contradicts the committed checkpoint",
                ));
            }
        }
        transaction.execute("INSERT INTO session_index VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET target=excluded.target,body=excluded.body",params![index.session,target.to_string(),encode(index)?]).map_err(io)?;
        // Index visibility changes invalidate paged projections, not Session state.
        transaction
            .execute("UPDATE catalog SET revision=revision+1", [])
            .map_err(io)?;
        transaction.commit().map_err(io)
    }

    pub fn sessions(
        &self,
        target: Option<EntityId>,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionIndex>> {
        page_limit(limit)?;
        let _lease = self.lease(false)?;
        let connection = self.connection()?;
        if let Some(id) = target {
            entity(&connection, id)?;
        }
        let mut statement=connection.prepare("SELECT s.body FROM session_index s LEFT JOIN entities l ON l.id=s.target LEFT JOIN entities a ON a.id=l.home_area WHERE s.session>?1 AND (?2 IS NULL OR s.target=?2 OR l.home_project=?2 OR l.home_area=?2 OR l.area_project=?2 OR a.area_project=?2) ORDER BY s.session LIMIT ?3").map_err(io)?;
        let rows = statement
            .query_map(
                params![after.unwrap_or(""), target.map(|v| v.to_string()), limit],
                |r| r.get::<_, String>(0),
            )
            .map_err(io)?;
        rows.map(|r| decode(&r.map_err(io)?)).collect()
    }
}

fn page_limit(limit: u32) -> Result<()> {
    if !(1..=200).contains(&limit) {
        return Err(issue(
            IssueCode::InvalidInput,
            "Page size must be 1 through 200. Continue for additional members",
        ));
    }
    Ok(())
}
pub(super) fn placement_target(placement: Placement) -> EntityId {
    match placement {
        Placement::Project(id) => EntityId::Project(id),
        Placement::WorkArea(id) => EntityId::WorkArea(id),
        Placement::Checkout(id) | Placement::Directory(id) | Placement::Standalone(id) => {
            EntityId::Location(id)
        }
    }
}
pub(super) fn validate_placement(connection: &Connection, placement: Placement) -> Result<()> {
    let value = entity(connection, placement_target(placement))?;
    let valid = matches!(
        (placement, value),
        (Placement::Project(_), Entity::Project(_))
            | (Placement::WorkArea(_), Entity::WorkArea(_))
            | (
                Placement::Checkout(_),
                Entity::Location(Location {
                    kind: LocationKind::Checkout { .. },
                    ..
                })
            )
            | (
                Placement::Directory(_),
                Entity::Location(Location {
                    kind: LocationKind::Directory,
                    ..
                })
            )
            | (
                Placement::Standalone(_),
                Entity::Location(Location {
                    kind: LocationKind::Standalone { .. },
                    ..
                })
            )
    );
    if !valid {
        return Err(issue(
            IssueCode::InvalidIdentity,
            "Placement kind contradicts catalog identity",
        ));
    }
    Ok(())
}
