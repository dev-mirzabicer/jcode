//! Agent workspace discovery, access proposals and narrowly authorized checkout
//! closeout work for a placed primary Session.
//!
//! Every action reads the caller's authoritative Session from storage. Model
//! arguments select targets only; they never carry Session identity, grant
//! approval or closeout authorization. Exposure is limited to placed primary
//! sessions by the Agent tool surface and to non-children by the child policy.
use super::{Tool, ToolContext, ToolOutput};
use crate::session::Session;
use crate::workspace::{
    AccessProposalState, CloseoutAction, CloseoutActionSpec, CloseoutDecision, CloseoutDisposition,
    Cursor, EntityId, Home, Issue, IssueCode, LocationId, OperationId, OperationKind,
    OperationQuery, PermissionQuery, Placement, ProjectId, Query, RepositoryId, RequestId,
    ReviewId, Visibility, WorkAreaId, WorkspaceService, WriteTarget,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Registry and tool-surface name.
pub const WORKSPACE_TOOL: &str = "workspace";
const DEFAULT_LIMIT: u32 = 50;

/// Exposure owner: the tool is advertised only to placed primary Sessions,
/// whose proposals and closeouts the human management client can answer.
/// Unplaced (legacy) and isolated sessions never see it.
pub fn retain_for_session(tools: &mut Vec<crate::message::ToolDefinition>, session: &Session) {
    if session.location.is_none() || session.isolated_child.is_some() {
        tools.retain(|tool| tool.name != WORKSPACE_TOOL);
    }
}

pub struct WorkspaceTool;

impl WorkspaceTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct WorkspaceInput {
    action: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    proposal: Option<String>,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    step: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    disposition: Option<String>,
    #[serde(default)]
    review: Option<String>,
    #[serde(default)]
    assessment: Option<String>,
    #[serde(default)]
    request: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[async_trait]
impl Tool for WorkspaceTool {
    fn name(&self) -> &str {
        WORKSPACE_TOOL
    }

    fn description(&self) -> &str {
        "Workspace locations, write access, access requests and checkout closeout steps."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": [
                        "inspect", "list", "entity", "locate", "scope",
                        "request_access", "access_status",
                        "closeouts", "closeout", "closeout_inventory",
                        "closeout_action", "closeout_action_status"
                    ],
                    "description": "inspect: own placement. entity: one record. locate: path owner. scope: writable roots."
                },
                "kind": {
                    "type": "string",
                    "enum": ["project", "work_area", "repository", "location"],
                    "description": "Kind of id. request_access: location, or project/work_area for its member roots."
                },
                "id": {"type": "string", "description": "Catalog ID (UUID)."},
                "path": {"type": "string", "description": "Path for locate, relative to the session cwd."},
                "reason": {"type": "string", "description": "Why access is needed, or why an entry is retained or redundant."},
                "proposal": {"type": "string", "description": "Access proposal ID."},
                "operation": {"type": "string", "description": "Closeout operation ID."},
                "expected_revision": {"type": "integer", "description": "Closeout revision the step was prepared against."},
                "step": {
                    "type": "string",
                    "enum": ["refresh", "preserve", "disposition", "review_removal", "declare_no_loss", "finish"],
                    "description": "Closeout step."
                },
                "entry": {"type": "string", "description": "Inventory entry ID for a disposition."},
                "disposition": {
                    "type": "string",
                    "enum": ["retain", "preserve", "preserved", "redundant"],
                    "description": "Inventory entry disposition. preserved needs path."
                },
                "review": {"type": "string", "description": "Removal review ID for declare_no_loss."},
                "assessment": {"type": "string", "description": "declare_no_loss: why removal loses no information."},
                "request": {"type": "string", "description": "Closeout step request ID."},
                "visibility": {
                    "type": "string",
                    "enum": ["current", "all", "archived", "retired", "closed"],
                    "description": "List visibility. Defaults to current."
                },
                "after": {"type": "string", "description": "Continuation from a previous page, passed back unchanged."},
                "limit": {"type": "integer", "description": "Page size, 1 to 200. Defaults to 50."}
            }
        })
    }

    fn decode_input(&self, input: &Value) -> Result<()> {
        jcode_tool_core::input::decode_as::<WorkspaceInput>(input)
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input: WorkspaceInput = serde_json::from_value(input)?;
        let session = load_session(&ctx.session_id)?;
        let request = derived_request(&ctx, &input.action);
        let output = if input.action == "closeout_action" {
            closeout_action(session, request, &input).await?
        } else {
            // Catalog reads and proposals stay inside the admitted turn's scope.
            crate::runtime_lifecycle::admission::spawn_blocking(move || {
                read_or_propose(&session, request, &input)
            })
            .await
            .map_err(|error| anyhow!("Workspace worker stopped: {error}"))??
        };
        Ok(ToolOutput::new(serde_json::to_string_pretty(&output)?))
    }
}

/// The caller's authoritative Session, never model-supplied identity.
fn load_session(id: &str) -> Result<Session> {
    let session = Session::load_startup_stub_in(&crate::storage::jcode_dir()?, id)
        .map_err(|error| anyhow!("Read authoritative Session {id}: {error:#}"))?;
    if session.isolated_child.is_some() {
        return Err(problem(Issue {
            code: IssueCode::PermissionRequired,
            detail: "Workspace administration belongs to the original primary parent".into(),
        }));
    }
    if session.location.is_none() {
        return Err(problem(Issue {
            code: IssueCode::RecoveryRequired,
            detail: "This Session has no workspace placement; a legacy Session needs reviewed adoption in workspace management first".into(),
        }));
    }
    Ok(session)
}

/// One stable domain request per tool invocation, so a recovered or replayed
/// invocation converges on the original receipt instead of a new effect.
fn derived_request(ctx: &ToolContext, action: &str) -> RequestId {
    let seed = format!(
        "jcode-workspace-tool\0{}\0{action}",
        crate::execution::invocation_id(ctx)
    );
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
        .to_string()
        .parse()
        .expect("UUID text parses as a request ID")
}

fn service() -> WorkspaceService {
    WorkspaceService::new(&crate::storage::durable_state_dir())
}

fn problem(issue: Issue) -> anyhow::Error {
    anyhow!("{:?}: {}", issue.code, issue.detail)
}

fn required<'a>(value: &'a Option<String>, name: &str, action: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{name} is required for {action}"))
}

fn uuid_arg<T: std::str::FromStr>(value: &Option<String>, name: &str, action: &str) -> Result<T> {
    required(value, name, action)?
        .trim()
        .parse()
        .map_err(|_| anyhow!("{name} is not a valid ID"))
}

fn limit(input: &WorkspaceInput) -> Result<u32> {
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=200).contains(&limit) {
        return Err(anyhow!("limit must be 1 through 200"));
    }
    Ok(limit)
}

fn cursor(input: &WorkspaceInput) -> Result<Option<Cursor>> {
    input
        .after
        .as_deref()
        .map(|text| {
            serde_json::from_str(text)
                .map_err(|_| anyhow!("after must be a continuation returned by a previous page"))
        })
        .transpose()
}

fn entity_id(input: &WorkspaceInput, action: &str) -> Result<EntityId> {
    let kind = required(&input.kind, "kind", action)?;
    Ok(match kind {
        "project" => EntityId::Project(uuid_arg::<ProjectId>(&input.id, "id", action)?),
        "work_area" => EntityId::WorkArea(uuid_arg::<WorkAreaId>(&input.id, "id", action)?),
        "repository" => EntityId::Repository(uuid_arg::<RepositoryId>(&input.id, "id", action)?),
        "location" => EntityId::Location(uuid_arg::<LocationId>(&input.id, "id", action)?),
        other => return Err(anyhow!("Unknown kind {other}")),
    })
}

/// `load_session` guarantees a placement.
fn placement_entity(placement: Placement) -> EntityId {
    match placement {
        Placement::Project(id) => EntityId::Project(id),
        Placement::WorkArea(id) => EntityId::WorkArea(id),
        Placement::Checkout(id) | Placement::Directory(id) | Placement::Standalone(id) => {
            EntityId::Location(id)
        }
    }
}

fn read_or_propose(session: &Session, request: RequestId, input: &WorkspaceInput) -> Result<Value> {
    let service = service();
    let action = input.action.as_str();
    let value = match action {
        "inspect" => json!(service.location_context(session).map_err(problem)?),
        "entity" => json!(
            service
                .inspect(entity_id(input, action)?)
                .map_err(problem)?
        ),
        "list" => {
            let target = if input.kind.is_some() || input.id.is_some() {
                entity_id(input, action)?
            } else {
                placement_entity(
                    session
                        .location
                        .as_ref()
                        .map(|location| location.placement)
                        .ok_or_else(|| anyhow!("Session placement disappeared"))?,
                )
            };
            let mut query = Query {
                visibility: match input.visibility.as_deref().unwrap_or("current") {
                    "current" => Visibility::Current,
                    "all" => Visibility::All,
                    "archived" => Visibility::Archived,
                    "retired" => Visibility::Retired,
                    "closed" => Visibility::Closed,
                    other => return Err(anyhow!("Unknown visibility {other}")),
                },
                ..Default::default()
            };
            match target {
                EntityId::Project(id) => query.project = Some(id),
                EntityId::WorkArea(id) => query.home = Some(Home::WorkArea(id)),
                EntityId::Repository(id) => query.repository = Some(id),
                EntityId::Location(_) => {
                    return Err(anyhow!(
                        "A location has no members; use entity or locate for its details"
                    ));
                }
            }
            json!(
                service
                    .list(query, cursor(input)?, limit(input)?)
                    .map_err(problem)?
            )
        }
        "locate" => json!(
            service
                .locate_path(
                    session,
                    &PathBuf::from(required(&input.path, "path", action)?)
                )
                .map_err(problem)?
        ),
        "scope" => json!(
            service
                .session_scope_page(session, cursor(input)?, limit(input)?)
                .map_err(problem)?
        ),
        "request_access" => {
            let target = match entity_id(input, action)? {
                EntityId::Location(id) => WriteTarget::Root(id),
                EntityId::Project(id) => WriteTarget::ProjectMembers(id),
                EntityId::WorkArea(id) => WriteTarget::WorkAreaMembers(id),
                EntityId::Repository(_) => {
                    return Err(anyhow!(
                        "Request access to a location or to the member roots of a project or work area"
                    ));
                }
            };
            let reason = required(&input.reason, "reason", action)?.to_string();
            let mutation = service
                .request_access(session, request, target, reason)
                .map_err(problem)?;
            json!({
                "proposal": mutation.proposal,
                "receipt": mutation.receipt,
                "note": "Pending human review in workspace management. The proposal grants nothing until approved; check access_status before writing.",
            })
        }
        "access_status" => match &input.proposal {
            Some(_) => {
                let proposal = service
                    .inspect_access_proposal(uuid_arg(&input.proposal, "proposal", action)?)
                    .map_err(problem)?;
                if proposal.session != session.id {
                    return Err(problem(Issue {
                        code: IssueCode::InvalidIdentity,
                        detail: "That proposal belongs to another Session".into(),
                    }));
                }
                json!(proposal)
            }
            None => json!(
                service
                    .list_permissions(
                        PermissionQuery::Proposals {
                            session: Some(session.id.clone()),
                            state: None::<AccessProposalState>,
                        },
                        cursor(input)?,
                        limit(input)?,
                    )
                    .map_err(problem)?
            ),
        },
        "closeouts" => {
            let location: LocationId = uuid_arg(&input.id, "id", action)?;
            json!(
                service
                    .operations(
                        OperationQuery {
                            target: Some(EntityId::Location(location)),
                            kinds: vec![OperationKind::Closeout],
                            ..Default::default()
                        },
                        cursor(input)?,
                        limit(input)?,
                    )
                    .map_err(problem)?
            )
        }
        "closeout" => {
            let operation: OperationId = uuid_arg(&input.operation, "operation", action)?;
            json!({
                "record": service.inspect_closeout(operation).map_err(problem)?,
                "review": service.inspect_closeout_review(operation).map_err(problem)?,
            })
        }
        "closeout_inventory" => {
            let operation: OperationId = uuid_arg(&input.operation, "operation", action)?;
            let record = service.inspect_closeout(operation).map_err(problem)?;
            let digest = record.inventory_digest.ok_or_else(|| {
                anyhow!("This closeout has no inventory yet; run the refresh step first")
            })?;
            let after = match input.after.as_deref() {
                None => 0,
                Some(text) => text
                    .parse()
                    .map_err(|_| anyhow!("after must be the next offset from a previous page"))?,
            };
            json!(
                service
                    .closeout_inventory(operation, &digest, after, limit(input)?)
                    .map_err(problem)?
            )
        }
        "closeout_action_status" => json!(
            service
                .inspect_closeout_action(uuid_arg(&input.request, "request", action)?)
                .map_err(problem)?
        ),
        other => return Err(anyhow!("Unknown workspace action {other}")),
    };
    Ok(value)
}

async fn closeout_action(
    session: Session,
    request: RequestId,
    input: &WorkspaceInput,
) -> Result<Value> {
    let action = "closeout_action";
    let operation: OperationId = uuid_arg(&input.operation, "operation", action)?;
    let expected_revision = input
        .expected_revision
        .ok_or_else(|| anyhow!("expected_revision is required for closeout_action"))?;
    let step = match required(&input.step, "step", action)? {
        "refresh" => CloseoutAction::Refresh,
        "preserve" => CloseoutAction::Preserve,
        "review_removal" => CloseoutAction::ReviewRemoval,
        "finish" => CloseoutAction::Finish,
        "declare_no_loss" => CloseoutAction::DeclareNoLoss {
            review: uuid_arg::<ReviewId>(&input.review, "review", action)?,
            assessment: required(&input.assessment, "assessment", action)?.to_string(),
        },
        "disposition" => {
            let disposition = match required(&input.disposition, "disposition", action)? {
                "retain" => CloseoutDisposition::Retain {
                    reason: required(&input.reason, "reason", action)?.to_string(),
                },
                "redundant" => CloseoutDisposition::Redundant {
                    reason: required(&input.reason, "reason", action)?.to_string(),
                },
                "preserve" => CloseoutDisposition::Preserve,
                "preserved" => CloseoutDisposition::Preserved {
                    path: PathBuf::from(required(&input.path, "path", action)?),
                },
                other => return Err(anyhow!("Unknown disposition {other}")),
            };
            CloseoutAction::Disposition {
                decision: CloseoutDecision {
                    entry: required(&input.entry, "entry", action)?.to_string(),
                    disposition,
                    // Replaced by the authoritative Session at admission.
                    recorded_by: String::new(),
                },
            }
        }
        other => return Err(anyhow!("Unknown closeout step {other}")),
    };
    let spec = CloseoutActionSpec {
        operation,
        expected_revision,
        action: step,
    };
    let record = crate::workspace::agent_closeout_action(session, request, spec)
        .await
        .map_err(problem)?;
    Ok(json!(record))
}
