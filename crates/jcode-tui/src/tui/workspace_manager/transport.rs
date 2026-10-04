//! Request construction and reply correlation. A reply changes state only
//! when its transport identity and its logical identity both match.
use super::*;
use crate::protocol::Request;
use crate::workspace::runtime::{RuntimeRequest, RuntimeResponse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    Workspace,
    Primary,
    Launch,
    Runtime,
}

/// What a read reply updates, or how a review/effect reply is handled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum View {
    Status,
    Known,
    OrgPage,
    Entity,
    EntitySessions(EntityId),
    Sessions,
    SessionLocation,
    Scope(String),
    Operations,
    Closeouts,
    Clone,
    CloneOutput,
    Volumes(Option<Box<actions::FormKind>>),
    StartupPlans {
        source: PathBuf,
        target: LocationId,
    },
    Permissions,
    CarryReview(NewContextKind),
    Snapshots,
    CloseoutRecord,
    CloseoutReview,
    CloseoutRecovery,
    CloseoutInventory,
    CloseoutRemoval,
    CloseoutHistory,
    CloseoutAction,
    RuntimeStatus,
    Supervision,
    RuntimeOperation,
    /// The reply is a server review; show it and let the human confirm.
    Review,
    /// A confirmed effect.
    Effect,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    Probe(Probe),
    Workspace {
        view: View,
        request: WorkspaceRequest,
    },
    Location {
        view: View,
        command: PrimaryLocationCommand,
    },
    Launch(PrimaryLaunchRequest),
    Runtime {
        view: View,
        request: RuntimeRequest,
    },
}

impl Op {
    pub fn probe(probe: Probe) -> Self {
        Op::Probe(probe)
    }
    pub fn read(view: View, request: WorkspaceRequest) -> Self {
        Op::Workspace { view, request }
    }
    pub fn closeout(view: View, request: CloseoutRequest) -> Self {
        Op::Workspace {
            view,
            request: WorkspaceRequest::Closeout { request },
        }
    }
    pub fn permission(view: View, request: PermissionRequest) -> Self {
        Op::Workspace {
            view,
            request: WorkspaceRequest::Permissions { request },
        }
    }
    pub fn location(view: View, command: PrimaryLocationCommand) -> Self {
        Op::Location { view, command }
    }
    pub fn runtime(view: View, request: RuntimeRequest) -> Self {
        Op::Runtime { view, request }
    }

    pub fn request(&self, id: u64) -> Request {
        match self {
            Op::Probe(Probe::Workspace) => Request::WorkspaceProbe { id },
            Op::Probe(Probe::Primary) => Request::PrimaryControlProbe { id },
            Op::Probe(Probe::Launch) => Request::PrimaryLaunchProbe { id },
            Op::Probe(Probe::Runtime) => Request::RuntimeProbe { id },
            Op::Workspace { request, .. } => Request::Workspace {
                id,
                request: Box::new(request.clone()),
            },
            Op::Location { command, .. } => Request::PrimaryLocation {
                id,
                command: Box::new(command.clone()),
            },
            Op::Launch(request) => Request::PrimaryLaunch {
                id,
                request: Box::new(request.clone()),
            },
            Op::Runtime { request, .. } => Request::RuntimeControl {
                id,
                request: Box::new(request.clone()),
            },
        }
    }

    /// Effects change durable state. Their lost replies are never resent
    /// automatically; a human inspects and may retry the same request.
    pub fn is_effect(&self) -> bool {
        match self {
            Op::Probe(_) => false,
            Op::Launch(_) => true,
            Op::Workspace { request, .. } => match request {
                WorkspaceRequest::Initialize { .. }
                | WorkspaceRequest::Apply { .. }
                | WorkspaceRequest::BeginClone { .. }
                | WorkspaceRequest::CancelClone { .. }
                | WorkspaceRequest::ResumeClone { .. }
                | WorkspaceRequest::ApplyCloneTrust { .. }
                | WorkspaceRequest::ApplyStartupCopy { .. }
                | WorkspaceRequest::Backup { .. }
                | WorkspaceRequest::Export { .. }
                | WorkspaceRequest::ApplyImport { .. }
                | WorkspaceRequest::ApplyRestore { .. } => true,
                WorkspaceRequest::Permissions { request } => matches!(
                    request,
                    PermissionRequest::Apply { .. }
                        | PermissionRequest::DecideProposal { .. }
                        | PermissionRequest::AbandonContextScope { .. }
                        | PermissionRequest::ReconcileContextScope { .. }
                ),
                WorkspaceRequest::Closeout { request } => matches!(
                    request,
                    CloseoutRequest::Begin { .. }
                        | CloseoutRequest::Revoke { .. }
                        | CloseoutRequest::Execute { .. }
                ),
                _ => false,
            },
            Op::Location { command, .. } => matches!(
                command,
                PrimaryLocationCommand::Change { .. }
                    | PrimaryLocationCommand::AdoptLegacy { .. }
                    | PrimaryLocationCommand::Place { .. }
                    | PrimaryLocationCommand::Cancel { .. }
            ),
            Op::Runtime { request, .. } => matches!(
                request,
                RuntimeRequest::Begin { .. }
                    | RuntimeRequest::CancelWait { .. }
                    | RuntimeRequest::Retry { .. }
                    | RuntimeRequest::Force { .. }
                    | RuntimeRequest::Recover { .. }
            ),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Op::Probe(probe) => format!("{probe:?} capability probe"),
            Op::Launch(request) => format!("launch primary (request {})", request.request),
            Op::Workspace { request, .. } => match request {
                WorkspaceRequest::Initialize { .. } => "initialize workspace catalog".into(),
                WorkspaceRequest::Apply { request, .. } => {
                    format!("apply organization change (request {request})")
                }
                WorkspaceRequest::BeginClone { request, .. } => {
                    format!("begin clone (request {request})")
                }
                WorkspaceRequest::CancelClone { request } => format!("cancel clone {request}"),
                WorkspaceRequest::ResumeClone { request } => format!("resume clone {request}"),
                WorkspaceRequest::ApplyCloneTrust { request, .. } => {
                    format!("approve clone sources (request {request})")
                }
                WorkspaceRequest::ApplyStartupCopy { request, .. } => {
                    format!("copy Startup Context selection (request {request})")
                }
                WorkspaceRequest::Backup { name, .. } => format!("catalog backup {name:?}"),
                WorkspaceRequest::Export { name, .. } => format!("project export {name:?}"),
                WorkspaceRequest::ApplyImport { request, .. } => {
                    format!("apply import (request {request})")
                }
                WorkspaceRequest::ApplyRestore { request, .. } => {
                    format!("restore catalog snapshot (request {request})")
                }
                WorkspaceRequest::Permissions { request } => match request {
                    PermissionRequest::Apply { request, .. } => {
                        format!("apply permission review (request {request})")
                    }
                    PermissionRequest::DecideProposal {
                        decision, proposal, ..
                    } => format!("{decision:?} access proposal {proposal}").to_lowercase(),
                    PermissionRequest::AbandonContextScope { session } => {
                        format!("abandon unpublished context scope for {session}")
                    }
                    PermissionRequest::ReconcileContextScope { session } => {
                        format!("reconcile context scope for {session}")
                    }
                    other => format!("permission read {other:?}"),
                },
                WorkspaceRequest::Closeout { request } => match request {
                    CloseoutRequest::Begin { request, spec, .. } => {
                        format!("begin closeout of {} (request {request})", spec.location)
                    }
                    CloseoutRequest::Revoke { operation, .. } => {
                        format!("revoke closeout {operation}")
                    }
                    CloseoutRequest::Execute { request, spec } => format!(
                        "closeout {} (request {request})",
                        describe::closeout_action(&spec.action)
                    ),
                    other => format!("closeout read {other:?}"),
                },
                other => format!("{other:?}"),
            },
            Op::Location { command, .. } => match command {
                PrimaryLocationCommand::Change { request } => format!(
                    "move session {} (request {})",
                    request.session, request.request
                ),
                PrimaryLocationCommand::AdoptLegacy { request } => format!(
                    "adopt legacy session {} (request {})",
                    request.session, request.request
                ),
                PrimaryLocationCommand::Cancel { operation } => {
                    format!("cancel location change {operation}")
                }
                PrimaryLocationCommand::Inspect { operation } => {
                    format!("inspect location change {operation}")
                }
                PrimaryLocationCommand::InspectSession { session } => {
                    format!("inspect session {session}")
                }
                PrimaryLocationCommand::ProposePlacement { session } => {
                    format!("review placement for session {session}")
                }
                PrimaryLocationCommand::Place { request } => format!(
                    "place session {} (request {})",
                    request.session, request.request
                ),
            },
            Op::Runtime { request, .. } => match request {
                RuntimeRequest::Begin { request, .. } => {
                    format!("confirm runtime shutdown review (request {request})")
                }
                RuntimeRequest::CancelWait { operation, .. } => {
                    format!("cancel waiting shutdown {operation}")
                }
                RuntimeRequest::Retry { operation, .. } => {
                    format!("retry shutdown quiescence {operation}")
                }
                RuntimeRequest::Force { operation, .. } => format!("force shutdown {operation}"),
                RuntimeRequest::Recover {
                    item,
                    decision,
                    request,
                    ..
                } => format!("{decision:?} recovery item {item} (request {request})"),
                other => format!("runtime {other:?}"),
            },
        }
    }
}

fn workspace_matches(request: &WorkspaceRequest, response: &WorkspaceResponse) -> bool {
    use WorkspaceRequest as Q;
    use WorkspaceResponse as R;
    match (request, response) {
        (_, R::Error(_)) => true,
        (Q::Closeout { request }, R::Closeout(response)) => {
            request.matches_reply(&CloseoutReply::State {
                response: response.clone(),
            })
        }
        (Q::Volumes {}, R::Volumes(_)) => true,
        (Q::CloneOutput { .. }, R::CloneOutput(_)) => true,
        (Q::ReviewClone { spec, .. }, R::CloneReview(review)) => review.spec == *spec,
        (
            Q::BeginClone { request, .. }
            | Q::InspectClone { request }
            | Q::CancelClone { request }
            | Q::ResumeClone { request },
            R::Clone(record),
        ) => record.request == *request,
        (Q::ReviewCloneTrust { clone, .. }, R::CloneTrustReview(review)) => review.clone == *clone,
        (Q::InspectRebind { operation }, R::Rebind(record)) => record.operation == *operation,
        (Q::ReviewStartupCopy { target, .. }, R::StartupCopyReview(review)) => {
            review.target == *target
        }
        (
            Q::ApplyStartupCopy { request, .. } | Q::InspectStartupCopy { request },
            R::StartupCopy(record),
        ) => record.request == *request,
        (Q::StartupCopyPlans { .. }, R::StartupCopyPlans(_)) => true,
        (Q::Operations { .. }, R::Operations(_)) => true,
        (Q::Permissions { request }, R::Permissions(response)) => match (request, &**response) {
            (PermissionRequest::Apply { request, .. }, PermissionResponse::Mutation(m))
            | (
                PermissionRequest::DecideProposal { request, .. },
                PermissionResponse::Mutation(m),
            ) => m.receipt.request == *request,
            (PermissionRequest::Review { change, .. }, PermissionResponse::Review(review)) => {
                review.change == *change
            }
            (PermissionRequest::ReviewCarry { session }, PermissionResponse::CarryReview(r)) => {
                r.source == *session
            }
            (PermissionRequest::Scope { session }, PermissionResponse::Scope(scope)) => {
                scope.session == *session
            }
            (PermissionRequest::Grant { grant }, PermissionResponse::Grant(found)) => {
                found.id == *grant
            }
            (PermissionRequest::Proposal { proposal }, PermissionResponse::Proposal(found)) => {
                found.id == *proposal
            }
            (PermissionRequest::List { .. }, PermissionResponse::Page(_))
            | (
                PermissionRequest::ImportedGrants { .. },
                PermissionResponse::ImportedGrants { .. },
            )
            | (
                PermissionRequest::ContextScopeStatus { .. }
                | PermissionRequest::AbandonContextScope { .. }
                | PermissionRequest::ReconcileContextScope { .. },
                PermissionResponse::ContextScopes(_),
            ) => true,
            _ => false,
        },
        (Q::Status {} | Q::Initialize { .. }, R::Status(_)) => true,
        (Q::List { .. }, R::Page(_)) => true,
        (Q::Inspect { target }, R::Entity(entity)) => entity.id() == *target,
        (Q::Review { change, .. }, R::Review(review)) => review.change == *change,
        (
            Q::Apply { request, .. }
            | Q::InspectReceipt { request }
            | Q::ApplyCloneTrust { request, .. }
            | Q::ApplyImport { request, .. }
            | Q::ApplyRestore { request, .. },
            R::Receipt(receipt),
        ) => receipt.request == *request,
        (Q::Sessions { .. }, R::Sessions(_)) => true,
        (Q::Backup { .. }, R::Snapshot(_)) => true,
        (Q::Snapshots {}, R::Snapshots(_)) => true,
        (Q::Export { .. }, R::Export(_)) => true,
        (Q::ReviewImport { .. }, R::ImportReview(_)) => true,
        (Q::ReviewRestore { snapshot }, R::RestoreReview(review)) => {
            review.snapshot.id == *snapshot
        }
        _ => false,
    }
}

fn location_matches(command: &PrimaryLocationCommand, response: &PrimaryLocationResponse) -> bool {
    match (command, response) {
        (_, PrimaryLocationResponse::Rejected { .. }) => true,
        (
            PrimaryLocationCommand::InspectSession { session },
            PrimaryLocationResponse::Session { view },
        ) => view.session == *session,
        (PrimaryLocationCommand::Change { request }, PrimaryLocationResponse::State { record }) => {
            record.input.request == request.request
        }
        (
            PrimaryLocationCommand::AdoptLegacy { request },
            PrimaryLocationResponse::State { record },
        ) => record.input.request == request.request,
        (PrimaryLocationCommand::Place { request }, PrimaryLocationResponse::State { record }) => {
            record.input.request == request.request
        }
        (
            PrimaryLocationCommand::ProposePlacement { session },
            PrimaryLocationResponse::Proposal { proposal },
        ) => proposal.session == *session,
        (
            PrimaryLocationCommand::Inspect { operation }
            | PrimaryLocationCommand::Cancel { operation },
            PrimaryLocationResponse::State { record },
        ) => record.operation == *operation,
        _ => false,
    }
}

pub(super) fn reduce(manager: &mut WorkspaceManager, op: Op, event: ServerEvent) {
    if let ServerEvent::Error { message, .. } = &event {
        rejected(manager, &op, message.clone());
        return;
    }
    match (op, event) {
        (
            Op::Probe(Probe::Workspace),
            ServerEvent::WorkspaceCapabilities {
                catalog_version,
                permissions_version,
                checkout_version,
                closeout_version,
                management_version,
                managed_rollout,
                ..
            },
        ) => {
            manager.caps.catalog = Some(catalog_version == 1);
            manager.caps.permissions = permissions_version == Some(1);
            manager.caps.checkout = checkout_version == Some(1);
            manager.caps.closeout = closeout_version == Some(2);
            manager.caps.management = management_version == Some(1);
            manager.caps.managed_rollout = managed_rollout;
            if catalog_version == 1 {
                manager.queue(Op::read(View::Status, WorkspaceRequest::Status {}));
            } else {
                manager.status = "Connected server's workspace catalog version is unsupported. Upgrade the runtime.".into();
            }
        }
        (
            Op::Probe(Probe::Primary),
            ServerEvent::PrimaryControlCapabilities {
                location_version,
                location_enabled,
                legacy_adoption_version,
                context_scope_version,
                session_inspection_version,
                ..
            },
        ) => {
            manager.caps.primary = Some(location_version == 1);
            manager.caps.location_enabled = location_enabled && location_version == 1;
            manager.caps.adoption = legacy_adoption_version == Some(1);
            manager.caps.context_scope = context_scope_version == Some(1);
            manager.caps.session_inspection = session_inspection_version == Some(1);
            if manager.section == Section::Sessions {
                manager.load_sessions();
            }
        }
        (
            Op::Probe(Probe::Launch),
            ServerEvent::PrimaryLaunchCapabilities {
                version, enabled, ..
            },
        ) => manager.caps.launch = Some(version == 1 && enabled),
        (
            Op::Probe(Probe::Runtime),
            ServerEvent::RuntimeCapabilities {
                version,
                supervision,
                ..
            },
        ) => {
            manager.caps.runtime = Some(version == Some(1));
            manager.caps.supervision = supervision == Some(1);
            if manager.section == Section::Runtime {
                manager.load_runtime();
            }
        }
        (Op::Workspace { view, request }, ServerEvent::WorkspaceResponse { response, .. }) => {
            if !workspace_matches(&request, &response) {
                mismatch(manager, Op::Workspace { view, request });
                return;
            }
            workspace(manager, view, request, *response);
        }
        (Op::Location { view, command }, ServerEvent::PrimaryLocationResponse { response, .. }) => {
            if !location_matches(&command, &response) {
                mismatch(manager, Op::Location { view, command });
                return;
            }
            location(manager, view, command, *response);
        }
        (Op::Launch(request), ServerEvent::PrimaryLaunchResponse { response, .. }) => {
            match *response {
                PrimaryLaunchResponse::Launched { record } if record.request == request.request => {
                    manager.note(
                        Tone::Good,
                        format!(
                            "Launched {} at {}. o open here · t open in a new terminal.",
                            record.session,
                            record
                                .input
                                .cwd
                                .as_ref()
                                .map_or("its placement root".into(), |cwd| cwd
                                    .path()
                                    .display()
                                    .to_string())
                        ),
                    );
                    manager.launched = Some(*record);
                    manager.sessions.views.clear();
                    manager.draft_settled();
                }
                PrimaryLaunchResponse::Rejected { request: id, issue } if id == request.request => {
                    manager.note(Tone::Bad, format!("Launch rejected: {issue}"));
                    // Invalid preparation publishes nothing; the launch draft returns.
                    manager.draft_failed(&issue);
                }
                _ => mismatch(manager, Op::Launch(request)),
            }
        }
        (Op::Runtime { view, request }, ServerEvent::RuntimeResponse { response, .. }) => {
            if !request.matches_response(&response) {
                mismatch(manager, Op::Runtime { view, request });
                return;
            }
            runtime(manager, view, request, *response);
        }
        (op, other) => {
            crate::logging::warn(&format!(
                "Workspace manager ignored uncorrelated reply {:?} for {}",
                std::mem::discriminant(&other),
                op.label()
            ));
            mismatch(manager, op);
        }
    }
}

fn mismatch(manager: &mut WorkspaceManager, op: Op) {
    if op.is_effect() {
        manager.mark_uncertain(op);
    } else {
        manager.note(
            Tone::Warn,
            "A reply did not match its request. Refresh (r) to read current state.",
        );
    }
}

fn rejected(manager: &mut WorkspaceManager, op: &Op, message: String) {
    match op {
        Op::Probe(Probe::Workspace) => manager.caps.catalog = Some(false),
        Op::Probe(Probe::Primary) => manager.caps.primary = Some(false),
        Op::Probe(Probe::Launch) => manager.caps.launch = Some(false),
        Op::Probe(Probe::Runtime) => manager.caps.runtime = Some(false),
        _ => {}
    }
    manager.note(Tone::Bad, format!("{} failed: {message}", op.label()));
}

fn issue_note(manager: &mut WorkspaceManager, op: &str, issue: &Issue) {
    manager.note(
        Tone::Bad,
        format!("{op}: {:?} — {}", issue.code, issue.detail),
    );
}

fn workspace(
    manager: &mut WorkspaceManager,
    view: View,
    request: WorkspaceRequest,
    response: WorkspaceResponse,
) {
    use WorkspaceResponse as R;
    if let R::Error(issue) = &response {
        match view {
            View::Status => {
                manager.catalog = Some(Err(issue.clone()));
                if manager.section == Section::Sessions || manager.section == Section::Runtime {
                    manager.load_section();
                }
            }
            View::Scope(session) => {
                manager.sessions.scope = Some(Err(issue.clone()));
                let _ = session;
            }
            View::Effect => {
                let label = Op::Workspace {
                    view: View::Effect,
                    request,
                }
                .label();
                issue_note(manager, &format!("Rejected: {label}"), issue);
                manager.draft_failed(issue);
            }
            View::Review
            | View::CarryReview(_)
            | View::StartupPlans { .. }
            | View::Volumes(Some(_)) => {
                issue_note(manager, "Request rejected", issue);
                manager.draft_failed(issue);
            }
            View::OrgPage
            | View::Operations
            | View::Closeouts
            | View::Permissions
            | View::Known
                if issue.code == IssueCode::Conflict && list_cursor(&request).is_some() =>
            {
                manager.restart_list(&view);
            }
            _ => issue_note(manager, "Request rejected", issue),
        }
        return;
    }
    match (view, response) {
        (View::Status, R::Status(status)) => {
            let first = !manager.catalog_ready();
            manager.catalog = Some(Ok(status));
            if first {
                manager.load_known();
                manager.queue(Op::read(View::Volumes(None), WorkspaceRequest::Volumes {}));
                manager.load_section();
            }
        }
        (View::Known, R::Page(page)) => {
            manager.observe_revision(page.revision);
            // Names and form choices cover the whole catalog, not one page.
            if let (Some(next), WorkspaceRequest::List { query, limit, .. }) =
                (page.next.clone(), &request)
            {
                manager.queue(Op::read(
                    View::Known,
                    WorkspaceRequest::List {
                        query: query.clone(),
                        after: Some(next),
                        limit: *limit,
                    },
                ));
            }
            for entity in page.items {
                manager.known.insert(entity.id().to_string(), entity);
            }
        }
        (View::OrgPage, R::Page(page)) => {
            manager.observe_revision(page.revision);
            for entity in &page.items {
                manager
                    .known
                    .insert(entity.id().to_string(), entity.clone());
            }
            if manager.org.selected.is_none()
                && let Some(first) = page.items.first()
            {
                let id = first.id();
                manager.org.page = Some(page);
                manager.select_key(&id.to_string());
            } else {
                manager.org.page = Some(page);
            }
        }
        (View::Entity, R::Entity(entity)) => {
            manager.known.insert(entity.id().to_string(), entity);
        }
        (View::EntitySessions(target), R::Sessions(rows)) => {
            manager.org.sessions = Some((target, rows));
        }
        (View::Sessions, R::Sessions(rows)) => {
            manager.sessions.next = (rows.len() as u32 >= PAGE)
                .then(|| rows.last().map(|row| row.session.clone()))
                .flatten();
            manager.sessions.rows = rows;
        }
        (View::Operations, R::Operations(page)) => {
            manager.observe_revision(page.revision);
            if manager.ops.selected.is_none() {
                manager.ops.selected = page.items.first().map(|entry| entry.operation.operation());
            }
            manager.ops.page = Some(page);
        }
        (View::Closeouts, R::Operations(page)) => {
            manager.observe_revision(page.revision);
            let first = page.items.first().map(|entry| entry.operation.operation());
            manager.closeouts.page = Some(page);
            if manager.closeouts.selected.is_none()
                && let Some(first) = first
            {
                manager.select_key(&first.to_string());
            }
        }
        (View::Clone, R::Clone(record)) => {
            if matches!(
                record.state,
                CloneState::Ready
                    | CloneState::PreparationFailed
                    | CloneState::Cancelled
                    | CloneState::RecoveryRequired
                    | CloneState::AwaitingTrust
            ) && manager.ops.watch_clone == Some(record.request)
            {
                manager.ops.watch_clone = None;
                manager.note(
                    if record.state == CloneState::Ready {
                        Tone::Good
                    } else {
                        Tone::Warn
                    },
                    format!("Clone {} is {:?}.", record.review.spec.name, record.state),
                );
                manager.load_ops();
                manager.load_known();
                // Publication advanced the catalog; later reviews need it.
                manager.queue(Op::read(View::Status, WorkspaceRequest::Status {}));
            }
            manager.ops.clone = Some(*record);
        }
        (View::CloneOutput, R::CloneOutput(output)) => {
            if let (
                WorkspaceRequest::CloneOutput { clone, .. },
                jcode_tool_types::execution::ExecutionResponse::Content { page, .. },
            ) = (&request, output)
            {
                manager.ops.output = Some((*clone, page.output.clone()));
            }
        }
        (View::Volumes(then), R::Volumes(volumes)) => {
            manager.volumes = Some(volumes);
            if let Some(kind) = then {
                actions::open(manager, *kind);
            }
        }
        (View::StartupPlans { source, target }, R::StartupCopyPlans(plans)) => {
            manager.queue(Op::read(
                View::Review,
                WorkspaceRequest::ReviewStartupCopy {
                    expected_catalog_revision: plans.catalog_revision,
                    source,
                    target,
                    expected_source_plan_revision: plans.source_revision,
                    expected_target_plan_revision: plans.target_revision,
                    external_approvals: Vec::new(),
                },
            ));
        }
        (View::Permissions, R::Permissions(response)) => match *response {
            PermissionResponse::Page(page) => {
                manager.observe_revision(page.revision);
                if manager.perms.selected.is_none() {
                    manager.perms.selected = page.items.first().map(describe::permission_key);
                }
                manager.perms.page = Some(page);
                manager.perms.imported = None;
            }
            PermissionResponse::ImportedGrants {
                total, items, next, ..
            } => {
                if manager.perms.selected.is_none() {
                    manager.perms.selected = items.first().map(|item| item.grant.id.to_string());
                }
                manager.perms.imported = Some((total, items, next));
                manager.perms.page = None;
            }
            _ => manager.note(Tone::Warn, "Unexpected permission reply; refresh."),
        },
        (View::Scope(_), R::Permissions(response)) => {
            if let PermissionResponse::Scope(scope) = *response {
                manager.sessions.scope = Some(Ok(scope));
            }
        }
        (View::CarryReview(kind), R::Permissions(response)) => {
            if let PermissionResponse::CarryReview(review) = *response {
                actions::open(
                    manager,
                    actions::FormKind::NewContextCarry {
                        kind,
                        review: Box::new(review),
                    },
                );
            }
        }
        (View::Snapshots, R::Snapshots(snapshots)) => {
            if manager.backups.selected.is_none() {
                manager.backups.selected = snapshots.first().map(|snapshot| snapshot.id);
            }
            manager.backups.snapshots = Some(snapshots);
        }
        (view, R::Closeout(response)) => {
            if view == View::Effect {
                manager.draft_settled();
            }
            closeout(manager, view, request, *response)
        }
        (View::Review, response) => {
            if let Some(confirm) = describe::review_confirm(manager, &request, response) {
                manager.confirm = Some(confirm);
            }
        }
        (View::Effect, response) => {
            manager.draft_settled();
            effect(manager, request, response)
        }
        (_, _) => manager.note(
            Tone::Warn,
            "Unexpected reply kind; refresh to read current state.",
        ),
    }
}

fn closeout(
    manager: &mut WorkspaceManager,
    view: View,
    request: WorkspaceRequest,
    response: CloseoutResponse,
) {
    match (view, response) {
        (View::CloseoutRecord | View::Effect, CloseoutResponse::Record(record)) => {
            let operation = record.operation;
            if matches!(
                &request,
                WorkspaceRequest::Closeout {
                    request: CloseoutRequest::Begin { .. }
                }
            ) {
                manager.note(
                    Tone::Good,
                    format!("Closeout {operation} started. Refresh inventory with f."),
                );
                manager.switch(Section::Closeout);
                manager.closeouts.selected = Some(operation);
                manager.load_closeouts();
            } else if matches!(
                &request,
                WorkspaceRequest::Closeout {
                    request: CloseoutRequest::Revoke { .. }
                }
            ) {
                manager.note(Tone::Good, format!("Closeout {operation} revoked."));
                manager.load_closeouts();
            }
            if manager.closeouts.selected == Some(operation) {
                // An open pane follows the record it describes.
                let inventory_stale = record.inventory_digest.is_some()
                    && manager.closeouts.inventory.as_ref().is_none_or(|page| {
                        page.operation != operation
                            || Some(&page.digest) != record.inventory_digest.as_ref()
                    });
                let removal_stale = manager.closeouts.removal.as_ref().is_none_or(|page| {
                    page.operation != operation || page.revision != record.revision
                });
                manager.closeouts.record = Some(*record);
                match manager.closeouts.pane {
                    CloseoutPane::Inventory if inventory_stale => actions::load_inventory(manager),
                    CloseoutPane::Removal if removal_stale => actions::load_removal(manager),
                    _ => {}
                }
            }
        }
        (View::CloseoutReview, CloseoutResponse::Review(review)) => {
            manager.closeouts.review = review.map(|review| *review);
        }
        (View::CloseoutRecovery, CloseoutResponse::Recovery(review)) => {
            manager.closeouts.recovery = review.map(|review| *review);
        }
        (View::CloseoutInventory, CloseoutResponse::Inventory(page)) => {
            manager.closeouts.entry = manager
                .closeouts
                .entry
                .min(page.entries.len().saturating_sub(1));
            manager.closeouts.inventory = Some(page);
        }
        (View::CloseoutRemoval, CloseoutResponse::RemovalProgress(page)) => {
            manager.closeouts.removal = Some(page);
        }
        (View::CloseoutHistory, CloseoutResponse::History(history)) => {
            manager.closeouts.history = Some(*history);
        }
        (View::CloseoutAction | View::Effect, CloseoutResponse::Action(record)) => {
            let request_id = record.request;
            let done = record.result.is_some() || record.issue.is_some();
            let operation = record.spec.operation;
            if let Some(issue) = &record.issue {
                issue_note(manager, "Closeout action failed", issue);
            }
            match &record.result {
                Some(CloseoutActionResult::Review(review)) => {
                    manager.closeouts.review = Some((**review).clone());
                    manager.note(
                        if review.issues.is_empty() {
                            Tone::Good
                        } else {
                            Tone::Warn
                        },
                        format!(
                            "Removal review ready with {} issue(s). a approve when none remain.",
                            review.issues.len()
                        ),
                    );
                }
                Some(CloseoutActionResult::Recovery(review)) => {
                    manager.closeouts.recovery = Some((**review).clone());
                    manager.note(
                        Tone::Accent,
                        "Recovery review ready. Inspect it, then apply (Y).",
                    );
                }
                Some(CloseoutActionResult::Record(result)) => {
                    let summary = match &record.spec.action {
                        CloseoutAction::Refresh => "Inventory refreshed".to_string(),
                        CloseoutAction::Preserve => "Preservation verified".to_string(),
                        CloseoutAction::Disposition { decision } => {
                            format!("Disposition recorded for entry {}", decision.entry)
                        }
                        CloseoutAction::ApproveRemoval { .. } => "Removal approved".to_string(),
                        CloseoutAction::Finish => "Removal finished".to_string(),
                        CloseoutAction::ApplyRecovery { .. } => "Recovery applied".to_string(),
                        CloseoutAction::DeclareNoLoss { .. } => {
                            "Agent declared no information loss".to_string()
                        }
                        CloseoutAction::ReviewRemoval | CloseoutAction::ReviewRecovery { .. } => {
                            "Done".to_string()
                        }
                    };
                    manager.note(
                        Tone::Good,
                        format!("{summary}; closeout is now {:?}.", result.stage),
                    );
                    manager.closeouts.record = Some((**result).clone());
                }
                None => {}
            }
            manager.closeouts.action = Some((request_id, done.then_some(*record)));
            if done {
                manager.load_closeout(operation);
                manager.load_closeouts();
            }
        }
        (View::Effect, _) | (_, _) => {
            manager.note(Tone::Warn, "Unexpected closeout reply; refresh.");
        }
    }
}

fn effect(manager: &mut WorkspaceManager, request: WorkspaceRequest, response: WorkspaceResponse) {
    use WorkspaceResponse as R;
    let label = Op::Workspace {
        view: View::Effect,
        request: request.clone(),
    }
    .label();
    match response {
        R::Status(status) => {
            manager.note(
                Tone::Good,
                format!("Catalog initialized at revision {}.", status.revision),
            );
            manager.catalog = None;
            manager.queue(Op::read(View::Status, WorkspaceRequest::Status {}));
        }
        R::Receipt(receipt) => {
            let mut text = format!("Done: {label} at revision {}.", receipt.revision);
            for issue in &receipt.issues {
                text.push_str(&format!(" {:?}: {}.", issue.code, issue.detail));
            }
            manager.note(
                if receipt.issues.is_empty() {
                    Tone::Good
                } else {
                    Tone::Warn
                },
                text,
            );
            if matches!(request, WorkspaceRequest::ApplyCloneTrust { .. }) {
                manager.note(
                    Tone::Accent,
                    "Sources approved. Resume the clone (R) to continue from its stage.",
                );
            }
            manager.load_known();
            manager.refresh();
        }
        R::Clone(record) => {
            let state = record.state;
            manager.ops.watch_clone = (!matches!(
                state,
                CloneState::Ready
                    | CloneState::PreparationFailed
                    | CloneState::Cancelled
                    | CloneState::RecoveryRequired
                    | CloneState::AwaitingTrust
            ))
            .then_some(record.request);
            manager.note(Tone::Accent, format!("{label}: {state:?}."));
            manager.ops.selected = Some(record.operation);
            manager.ops.clone = Some(*record);
            manager.load_ops();
        }
        R::StartupCopy(record) => {
            manager.note(
                if record.issue.is_none() {
                    Tone::Good
                } else {
                    Tone::Warn
                },
                format!(
                    "{label}: {:?}{}",
                    record.state,
                    record
                        .issue
                        .as_ref()
                        .map(|issue| format!(" — {}", issue.detail))
                        .unwrap_or_default()
                ),
            );
        }
        R::Snapshot(snapshot) => {
            manager.note(
                Tone::Good,
                format!("Backup {:?} written to {}.", snapshot.name, snapshot.path.display()),
            );
            manager.queue(Op::read(View::Snapshots, WorkspaceRequest::Snapshots {}));
        }
        R::Export(path) => manager.note(
            Tone::Good,
            format!("Exported project definition to {}. It contains no credentials, transcripts or files.", path.display()),
        ),
        R::Permissions(response) => match *response {
            PermissionResponse::Mutation(mutation) => {
                manager.note(
                    Tone::Good,
                    format!(
                        "Done: {label} at revision {}{}.",
                        mutation.receipt.revision,
                        mutation
                            .grant
                            .as_ref()
                            .map(|grant| format!(", grant {} is {:?}", grant.id, grant.state))
                            .unwrap_or_default()
                    ),
                );
                manager.perms.review = None;
                manager.load_perms();
            }
            PermissionResponse::ContextScopes(scopes) => {
                manager.note(Tone::Good, format!("Done: {label}."));
                manager.perms.scopes = Some(scopes);
            }
            _ => manager.note(Tone::Warn, format!("{label}: unexpected reply; refresh.")),
        },
        _ => manager.note(Tone::Warn, format!("{label}: unexpected reply; refresh.")),
    }
}

fn location(
    manager: &mut WorkspaceManager,
    view: View,
    command: PrimaryLocationCommand,
    response: PrimaryLocationResponse,
) {
    match response {
        PrimaryLocationResponse::Session { view: session } => {
            manager
                .sessions
                .views
                .insert(session.session.clone(), *session);
        }
        PrimaryLocationResponse::State { record } => {
            let session = record.input.session.clone();
            if matches!(
                command,
                PrimaryLocationCommand::Change { .. }
                    | PrimaryLocationCommand::AdoptLegacy { .. }
                    | PrimaryLocationCommand::Place { .. }
            ) {
                // Accepted, even if pending: the request now has its own
                // durable operation identity and can be inspected or cancelled.
                manager.draft_settled();
            }
            let tone = match record.state {
                LocationChangeState::Complete => Tone::Good,
                LocationChangeState::Pending => Tone::Accent,
                _ => Tone::Warn,
            };
            let label = Op::Location {
                view,
                command: command.clone(),
            }
            .label();
            manager.note(
                tone,
                format!(
                    "{label}: {:?}{}",
                    record.state,
                    record
                        .issue
                        .as_ref()
                        .map(|issue| format!(" — {}", issue.detail))
                        .unwrap_or_default()
                ),
            );
            manager.inspect_session(session);
        }
        // The placement review owns its own proposals; the manager never asks.
        PrimaryLocationResponse::Proposal { .. } => {}
        PrimaryLocationResponse::Rejected { issue } => {
            let label = Op::Location { view, command }.label();
            issue_note(manager, &format!("Rejected: {label}"), &issue);
            manager.draft_failed(&issue);
        }
    }
}

fn runtime(
    manager: &mut WorkspaceManager,
    view: View,
    request: RuntimeRequest,
    response: RuntimeResponse,
) {
    let human_step = matches!(view, View::Review | View::Effect);
    let effect = view == View::Effect;
    match (view, response) {
        (_, RuntimeResponse::Error(issue)) => {
            let label = Op::runtime(View::Effect, request).label();
            issue_note(manager, &format!("Rejected: {label}"), &issue);
            if human_step {
                manager.draft_failed(&issue);
            }
        }
        (View::RuntimeStatus, RuntimeResponse::Status(status)) => {
            if let Some(operation) = &status.operation {
                manager.runtime.operation = Some(operation.clone());
            }
            manager.runtime.status = Some(status);
        }
        (View::Supervision, RuntimeResponse::Supervision(supervision)) => {
            if manager.runtime.selected.is_none() {
                manager.runtime.selected = supervision
                    .recoveries
                    .iter()
                    .find(|item| item.resolved.is_none())
                    .map(|item| item.id.to_string());
            }
            manager.runtime.supervision = Some(supervision);
        }
        (View::Review, RuntimeResponse::Review(review)) => {
            manager.confirm = Some(describe::shutdown_confirm(&review));
        }
        (View::RuntimeOperation | View::Effect, RuntimeResponse::Operation(operation)) => {
            if effect {
                manager.draft_settled();
            }
            if matches!(view_label(&request), Some(label) if !label.is_empty()) {
                manager.note(
                    describe::phase_tone(operation.phase),
                    format!(
                        "Shutdown {} is {:?} (revision {}).",
                        operation.id, operation.phase, operation.revision
                    ),
                );
            }
            manager.runtime.operation = Some(operation);
        }
        (View::Effect, RuntimeResponse::Recovery(item)) => {
            manager.note(
                Tone::Good,
                format!(
                    "Recovery item for {} resolved: {}.",
                    item.session,
                    item.resolved
                        .as_ref()
                        .map(describe::resolution)
                        .unwrap_or_else(|| "unresolved".into())
                ),
            );
            manager.load_runtime();
        }
        _ => manager.note(Tone::Warn, "Unexpected runtime reply; refresh."),
    }
}

/// Effects announce their new phase; polling inspections stay quiet.
fn view_label(request: &RuntimeRequest) -> Option<&'static str> {
    match request {
        RuntimeRequest::Inspect { .. } => None,
        _ => Some("effect"),
    }
}

/// The continuation cursor a list request carried, if it was not a first page.
fn list_cursor(request: &WorkspaceRequest) -> Option<&Cursor> {
    match request {
        WorkspaceRequest::List { after, .. } | WorkspaceRequest::Operations { after, .. } => {
            after.as_ref()
        }
        WorkspaceRequest::Permissions {
            request:
                PermissionRequest::List { after, .. } | PermissionRequest::ImportedGrants { after, .. },
        } => after.as_ref(),
        _ => None,
    }
}
