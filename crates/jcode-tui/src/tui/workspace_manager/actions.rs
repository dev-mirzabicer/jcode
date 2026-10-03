//! Section actions, their forms and the typed requests built from them.
//! Every effect is either a server review followed by confirmation or a local
//! confirmation that names the exact request.
use super::form::{Choice, Field, Form};
use super::transport::View;
use super::*;
use crate::workspace::runtime::{
    IndependentTasks, RecoveryDecision, RuntimeDestination, RuntimeRequest, ShutdownOptions,
    ShutdownPhase, StopStrategy,
};
use std::result::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Refresh,
    Help,
    Close,
    Older,
    Newer,
    Initialize,
    Uncertain,
    // Organization
    FilterKind,
    FilterVisibility,
    NewProject,
    NewRepository,
    NewArea,
    Register,
    Associate,
    Clone,
    Rename,
    Move,
    Rebind,
    Archive,
    Retire,
    Discard,
    Launch,
    BeginCloseout,
    StartupCopy,
    VolumeDefault,
    ShowSessions,
    // Sessions
    OpenHere,
    OpenTerminal,
    ChangeLocation,
    AdoptLegacy,
    CancelChange,
    NewContext,
    WriteScope,
    InspectSession,
    GrantSession,
    ReconcileScope,
    AbandonScope,
    ClearFilter,
    // Operations
    ToggleFinished,
    CancelClone,
    ResumeClone,
    TrustClone,
    CloneOutput,
    // Permissions
    PermMode,
    Approve,
    Decline,
    NewGrant,
    Revoke,
    BindImported,
    ActivateImported,
    // Closeout
    InventoryRefresh,
    Preserve,
    InventoryPane,
    Disposition,
    ReviewRemoval,
    ApproveRemoval,
    Finish,
    RevokeCloseout,
    ReviewRecovery,
    ApplyRecovery,
    RemovalPane,
    HistoryPane,
    RecordPane,
    // Backup
    Backup,
    Restore,
    Export,
    Import,
    // Runtime
    Stop,
    Restart,
    CancelWait,
    ChangeShutdown,
    RetryShutdown,
    Force,
    Continue,
    Leave,
    Start,
}

pub(crate) struct ActionSpec {
    pub key: char,
    pub label: &'static str,
    pub action: Action,
}

const fn spec(key: char, label: &'static str, action: Action) -> ActionSpec {
    ActionSpec { key, label, action }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FormKind {
    CreateProject,
    CreateRepository,
    CreateArea,
    Register,
    Associate,
    Rename(EntityId),
    Move(LocationId),
    AdoptStandalone(LocationId),
    Rebind {
        location: LocationId,
        old_path: PathBuf,
        generation: u64,
    },
    VolumeDefault,
    Clone,
    StartupCopy(LocationId),
    Launch,
    SessionMove(Box<SessionLocationView>),
    SessionAdopt(Box<SessionLocationView>),
    NewContext(String),
    NewContextCarry {
        kind: NewContextKind,
        review: Box<GrantCarryReview>,
    },
    InspectSession,
    Grant(Option<ProposalId>),
    BindImported(Box<ImportedGrantReference>),
    CloseoutBegin {
        location: LocationId,
        generation: u64,
    },
    Disposition {
        operation: OperationId,
        revision: Revision,
        entry: String,
    },
    CloseoutRecovery {
        operation: OperationId,
        revision: Revision,
    },
    Backup,
    Export,
    Import,
    Shutdown(RuntimeDestination),
    ShutdownChange {
        operation: OperationId,
        revision: Revision,
        destination: RuntimeDestination,
    },
}

fn selected_entity(manager: &WorkspaceManager) -> Option<&Entity> {
    manager
        .org
        .selected
        .and_then(|id| manager.known.get(&id.to_string()))
}

fn selected_location(manager: &WorkspaceManager) -> Option<&Location> {
    match selected_entity(manager) {
        Some(Entity::Location(location)) => Some(location),
        _ => None,
    }
}

fn selected_view(manager: &WorkspaceManager) -> Option<&SessionLocationView> {
    let key = manager.selected_key()?;
    manager.sessions.views.get(&key)
}

fn selected_operation(manager: &WorkspaceManager) -> Option<&OperationEntry> {
    let id = manager.ops.selected?;
    manager
        .ops
        .page
        .as_ref()?
        .items
        .iter()
        .find(|entry| entry.operation.operation() == id)
}

fn selected_permission(manager: &WorkspaceManager) -> Option<&PermissionItem> {
    let key = manager.perms.selected.as_ref()?;
    manager
        .perms
        .page
        .as_ref()?
        .items
        .iter()
        .find(|item| describe::permission_key(item) == *key)
}

fn selected_imported(manager: &WorkspaceManager) -> Option<&ImportedGrantReference> {
    let key = manager.perms.selected.as_ref()?;
    manager
        .perms
        .imported
        .as_ref()?
        .1
        .iter()
        .find(|item| item.grant.id.to_string() == *key)
}

fn selected_recovery(
    manager: &WorkspaceManager,
) -> Option<&crate::workspace::runtime::RecoveryItem> {
    let key = manager.runtime.selected.as_ref()?;
    manager
        .runtime
        .supervision
        .as_ref()?
        .recoveries
        .iter()
        .find(|item| item.id.to_string() == *key)
}

/// The actions valid for the current section, selection and capabilities.
pub(crate) fn available(manager: &WorkspaceManager) -> Vec<ActionSpec> {
    let mut actions = Vec::new();
    if !manager.remote {
        actions.push(spec('q', "Close", Action::Close));
        return actions;
    }
    let ready = manager.catalog_ready();
    if let Some(Err(issue)) = &manager.catalog
        && manager.section != Section::Runtime
    {
        if issue.is_not_initialized() {
            actions.push(spec('I', "Initialize catalog", Action::Initialize));
        } else if issue.interrupted_initialization().is_some() {
            actions.push(spec('I', "Resume initialization", Action::Initialize));
        }
    }
    match manager.section {
        Section::Organization if ready => {
            actions.extend([
                spec('n', "New project", Action::NewProject),
                spec('N', "New repository", Action::NewRepository),
                spec('a', "New work area", Action::NewArea),
                spec('g', "Register path", Action::Register),
                spec('A', "Associate repo", Action::Associate),
            ]);
            if let Some(entity) = selected_entity(manager) {
                if matches!(
                    entity,
                    Entity::Repository(_) | Entity::Project(_) | Entity::WorkArea(_)
                ) && manager.caps.checkout
                {
                    actions.push(spec('c', "Clone", Action::Clone));
                }
                actions.push(spec('e', "Rename", Action::Rename));
                if let Entity::Location(location) = entity {
                    actions.push(spec('m', "Move / adopt", Action::Move));
                    actions.push(spec('b', "Rebind path", Action::Rebind));
                    if location.lifecycle == LocationLifecycle::Ready {
                        actions.push(spec('p', "Copy Startup Context", Action::StartupCopy));
                    }
                    if matches!(location.kind, LocationKind::Checkout { .. })
                        && location.lifecycle == LocationLifecycle::Ready
                        && manager.caps.closeout
                    {
                        actions.push(spec('o', "Close out", Action::BeginCloseout));
                    }
                }
                actions.push(spec('z', "Archive / unarchive", Action::Archive));
                actions.push(spec('x', "Retire", Action::Retire));
                actions.push(spec('D', "Discard unused", Action::Discard));
                actions.push(spec('s', "Sessions here", Action::ShowSessions));
            }
            actions.push(spec('l', "Launch session", Action::Launch));
            actions.push(spec('v', "Volume default", Action::VolumeDefault));
            actions.push(spec('f', "Kind filter", Action::FilterKind));
            actions.push(spec('h', "Visibility", Action::FilterVisibility));
            actions.push(spec('[', "Prev page", Action::Older));
            actions.push(spec(']', "Next page", Action::Newer));
        }
        Section::Sessions => {
            let key = manager.selected_key();
            let current = key.as_deref() == Some(manager.session.as_str());
            if !current && key.is_some() {
                actions.push(spec('o', "Open here", Action::OpenHere));
            }
            if key.is_some() {
                actions.push(spec('t', "Open in terminal", Action::OpenTerminal));
            }
            if let Some(view) = selected_view(manager) {
                if view.location.is_some() && !view.isolated_child {
                    actions.push(spec('m', "Move / change cwd", Action::ChangeLocation));
                } else if !view.isolated_child {
                    actions.push(spec('a', "Adopt legacy", Action::AdoptLegacy));
                }
                if !view.pending.is_empty() {
                    actions.push(spec('x', "Cancel pending move", Action::CancelChange));
                }
            }
            if current && manager.caps.context_scope {
                actions.push(spec('n', "New context", Action::NewContext));
            }
            if ready {
                actions.push(spec('w', "Write scope", Action::WriteScope));
                actions.push(spec('g', "Grant access", Action::GrantSession));
                actions.push(spec('R', "Reconcile scope", Action::ReconcileScope));
                actions.push(spec('X', "Abandon scope", Action::AbandonScope));
                actions.push(spec('l', "Launch session", Action::Launch));
            }
            actions.push(spec('i', "Inspect by ID", Action::InspectSession));
            if manager.sessions.target.is_some() {
                actions.push(spec('u', "All sessions", Action::ClearFilter));
            }
            actions.push(spec('[', "Prev page", Action::Older));
            actions.push(spec(']', "Next page", Action::Newer));
        }
        Section::Operations if ready => {
            actions.push(spec(
                'u',
                if manager.ops.all {
                    "Unfinished only"
                } else {
                    "Include finished"
                },
                Action::ToggleFinished,
            ));
            if let Some(entry) = selected_operation(manager) {
                match &entry.operation {
                    WorkspaceOperation::Clone(record) => {
                        let record = manager
                            .ops
                            .clone
                            .as_ref()
                            .filter(|fresh| fresh.request == record.request)
                            .unwrap_or(record);
                        if !matches!(record.state, CloneState::Ready | CloneState::Cancelled) {
                            actions.push(spec('C', "Cancel clone", Action::CancelClone));
                        }
                        if matches!(
                            record.state,
                            CloneState::AwaitingTrust
                                | CloneState::PreparationFailed
                                | CloneState::RecoveryRequired
                        ) {
                            actions.push(spec('R', "Resume clone", Action::ResumeClone));
                        }
                        if record.state == CloneState::AwaitingTrust {
                            actions.push(spec('T', "Review sources", Action::TrustClone));
                        }
                        if !record.output_runs.is_empty() {
                            actions.push(spec('O', "Output", Action::CloneOutput));
                        }
                    }
                    WorkspaceOperation::PrimaryLaunch(record)
                        if record.state == PrimaryLaunchState::Complete =>
                    {
                        actions.push(spec('o', "Open here", Action::OpenHere));
                        actions.push(spec('t', "Open in terminal", Action::OpenTerminal));
                    }
                    WorkspaceOperation::PrimaryLocation(record)
                        if record.state == LocationChangeState::Pending =>
                    {
                        actions.push(spec('x', "Cancel move", Action::CancelChange));
                    }
                    _ => {}
                }
            }
            if manager.launched.is_some() && !actions.iter().any(|a| a.action == Action::OpenHere) {
                actions.push(spec('o', "Open launched", Action::OpenHere));
                actions.push(spec('t', "Launched in terminal", Action::OpenTerminal));
            }
            actions.push(spec('[', "Prev page", Action::Older));
            actions.push(spec(']', "Next page", Action::Newer));
        }
        Section::Permissions if ready => {
            actions.push(spec('v', "Proposals/grants/imported", Action::PermMode));
            actions.push(spec('n', "New grant", Action::NewGrant));
            match selected_permission(manager) {
                Some(PermissionItem::Proposal(proposal))
                    if proposal.state == AccessProposalState::Pending =>
                {
                    actions.push(spec('a', "Approve", Action::Approve));
                    actions.push(spec('d', "Decline", Action::Decline));
                }
                Some(PermissionItem::Grant(grant)) if grant.state == GrantState::Active => {
                    actions.push(spec('x', "Revoke", Action::Revoke));
                }
                Some(PermissionItem::Grant(grant)) if grant.state == GrantState::Disabled => {
                    actions.push(spec('A', "Activate imported", Action::ActivateImported));
                }
                _ => {}
            }
            if let Some(imported) = selected_imported(manager) {
                if imported.bound_grant.is_none() {
                    actions.push(spec('b', "Bind imported", Action::BindImported));
                } else {
                    actions.push(spec('A', "Activate bound", Action::ActivateImported));
                }
            }
            actions.push(spec('[', "Prev page", Action::Older));
            actions.push(spec(']', "Next page", Action::Newer));
        }
        Section::Closeout if ready => {
            if let Some(record) = &manager.closeouts.record {
                let busy = matches!(manager.closeouts.action, Some((_, None)));
                let open = !matches!(
                    record.stage,
                    CloseoutStage::Closed | CloseoutStage::Revoked | CloseoutStage::Retained
                );
                if open && !busy {
                    if !matches!(
                        record.stage,
                        CloseoutStage::Removing | CloseoutStage::RecoveryRequired
                    ) {
                        actions.push(spec('f', "Refresh inventory", Action::InventoryRefresh));
                        actions.push(spec('p', "Preserve", Action::Preserve));
                        actions.push(spec('w', "Review removal", Action::ReviewRemoval));
                    }
                    if manager
                        .closeouts
                        .review
                        .as_ref()
                        .is_some_and(|review| review.issues.is_empty())
                        && record.stage == CloseoutStage::ReadyForApproval
                    {
                        actions.push(spec('a', "Approve removal", Action::ApproveRemoval));
                    }
                    if record.stage == CloseoutStage::Authorized
                        || record.stage == CloseoutStage::Removing
                    {
                        actions.push(spec('F', "Finish removal", Action::Finish));
                    }
                    actions.push(spec('y', "Recovery review", Action::ReviewRecovery));
                    if manager.closeouts.recovery.is_some() {
                        actions.push(spec('Y', "Apply recovery", Action::ApplyRecovery));
                    }
                    if !matches!(record.stage, CloseoutStage::Removing) {
                        actions.push(spec('X', "Revoke", Action::RevokeCloseout));
                    }
                }
                if record.inventory_digest.is_some() {
                    actions.push(spec('i', "Inventory", Action::InventoryPane));
                }
                if manager.closeouts.pane == CloseoutPane::Inventory && open && !busy {
                    actions.push(spec('d', "Disposition", Action::Disposition));
                }
                actions.push(spec('g', "Removal progress", Action::RemovalPane));
                actions.push(spec('H', "History", Action::HistoryPane));
                if manager.closeouts.pane != CloseoutPane::Record {
                    actions.push(spec('b', "Record", Action::RecordPane));
                }
            }
        }
        Section::Backup if ready => {
            actions.push(spec('b', "Backup now", Action::Backup));
            if manager.backups.selected.is_some() {
                actions.push(spec('R', "Restore", Action::Restore));
            }
            actions.push(spec('e', "Export project", Action::Export));
            actions.push(spec('i', "Import", Action::Import));
        }
        Section::Runtime => {
            let live = manager.connected && manager.caps.runtime == Some(true);
            let operation = manager
                .runtime
                .operation
                .as_ref()
                .filter(|op| !op.phase.terminal());
            if live && operation.is_none() {
                actions.push(spec('s', "Stop runtime", Action::Stop));
                if manager.caps.supervision {
                    actions.push(spec('R', "Restart", Action::Restart));
                }
            }
            if let Some(op) = operation {
                if op.phase == ShutdownPhase::WaitingForCurrent && !op.cancellation_closed {
                    actions.push(spec('c', "Cancel wait", Action::CancelWait));
                }
                if matches!(
                    op.phase,
                    ShutdownPhase::WaitingForCurrent | ShutdownPhase::Blocked
                ) {
                    actions.push(spec('h', "Change options", Action::ChangeShutdown));
                }
                if op.phase == ShutdownPhase::Blocked {
                    actions.push(spec('y', "Retry quiescence", Action::RetryShutdown));
                }
                if matches!(op.phase, ShutdownPhase::Blocked | ShutdownPhase::Stopping) {
                    actions.push(spec('F', "Force", Action::Force));
                }
            }
            if let Some(item) = selected_recovery(manager)
                && item.resolved.is_none()
                && live
            {
                actions.push(spec('C', "Continue turn", Action::Continue));
                actions.push(spec('L', "Leave stopped", Action::Leave));
            }
            if !manager.connected {
                actions.push(spec('S', "Start runtime", Action::Start));
            }
        }
        _ => {}
    }
    if !manager.uncertain.is_empty() {
        actions.push(spec('U', "Unknown outcomes", Action::Uncertain));
    }
    actions.push(spec('r', "Refresh", Action::Refresh));
    actions.push(spec('?', "Help", Action::Help));
    actions.push(spec('q', "Close", Action::Close));
    actions
}

/// What Enter does in a wide layout.
pub(crate) fn primary(manager: &WorkspaceManager) -> Option<Action> {
    match manager.section {
        Section::Sessions => available(manager)
            .iter()
            .find(|spec| spec.action == Action::OpenHere)
            .map(|_| Action::OpenHere),
        Section::Closeout => Some(Action::InventoryPane),
        _ => None,
    }
}

fn new_request() -> RequestId {
    RequestId::new()
}

fn revision(manager: &WorkspaceManager) -> Revision {
    match &manager.catalog {
        Some(Ok(status)) => status.revision,
        _ => 0,
    }
}

fn confirm(title: impl Into<String>, lines: Vec<(Tone, String)>, op: Op) -> Confirm {
    Confirm {
        title: title.into(),
        lines,
        op,
        typed: None,
        input: String::new(),
        yes: false,
        scroll: 0,
    }
}

fn review_change(manager: &mut WorkspaceManager, change: OrganizationChange) {
    let expected_revision = revision(manager);
    manager.queue(Op::read(
        View::Review,
        WorkspaceRequest::Review {
            expected_revision,
            change,
        },
    ));
}

fn review_grant(manager: &mut WorkspaceManager, change: GrantChange) {
    let expected_revision = revision(manager);
    manager.queue(Op::permission(
        View::Review,
        PermissionRequest::Review {
            expected_revision,
            change,
        },
    ));
}

pub(crate) fn run(manager: &mut WorkspaceManager, action: Action) {
    if !available(manager).iter().any(|spec| spec.action == action) {
        return;
    }
    // A new action supersedes any earlier submitted draft.
    manager.draft = None;
    match action {
        Action::Refresh => {
            manager.refresh();
            if manager.section == Section::Runtime && !manager.connected {
                manager.intents.push_back(Intent::OfflineRuntime);
            }
        }
        Action::Help => manager.help = true,
        Action::Close => manager.close(),
        Action::Uncertain => manager.show_uncertain = true,
        Action::Older | Action::Newer => page(manager, action == Action::Newer),
        Action::Initialize => {
            // An interrupted initialization can only be finished by its own
            // request identity; a fresh one would be refused.
            let interrupted = match &manager.catalog {
                Some(Err(issue)) => issue.interrupted_initialization(),
                _ => None,
            };
            let (title, first) = match interrupted {
                Some(request) => (
                    "Resume the interrupted catalog initialization",
                    format!("Finishes initialize request {request}, which stopped before it was ready."),
                ),
                None => (
                    "Initialize the workspace catalog",
                    "Creates the private catalog under this installation's durable state.".into(),
                ),
            };
            manager.confirm = Some(confirm(
                title,
                vec![
                    (Tone::Normal, first),
                    (Tone::Muted, "No project, checkout, session or file is created or changed.".into()),
                ],
                Op::read(
                    View::Effect,
                    WorkspaceRequest::Initialize {
                        request: interrupted.unwrap_or_else(new_request),
                    },
                ),
            ));
        }
        Action::FilterKind => {
            manager.org.kind = match manager.org.kind {
                None => Some(EntityKind::Project),
                Some(EntityKind::Project) => Some(EntityKind::Repository),
                Some(EntityKind::Repository) => Some(EntityKind::WorkArea),
                Some(EntityKind::WorkArea) => Some(EntityKind::Location),
                Some(EntityKind::Location) => None,
            };
            reset_org_page(manager);
        }
        Action::FilterVisibility => {
            manager.org.visibility = match manager.org.visibility {
                Visibility::Current => Visibility::Archived,
                Visibility::Archived => Visibility::Retired,
                Visibility::Retired => Visibility::Closed,
                Visibility::Closed => Visibility::All,
                Visibility::All => Visibility::Current,
            };
            reset_org_page(manager);
        }
        Action::NewProject => open(manager, FormKind::CreateProject),
        Action::NewRepository => open(manager, FormKind::CreateRepository),
        Action::NewArea => open(manager, FormKind::CreateArea),
        Action::Register => open(manager, FormKind::Register),
        Action::Associate => open(manager, FormKind::Associate),
        Action::Clone => {
            manager.queue(Op::read(
                View::Volumes(Some(Box::new(FormKind::Clone))),
                WorkspaceRequest::Volumes {},
            ));
            manager.status = "Reading mounted volumes…".into();
        }
        Action::VolumeDefault => {
            manager.queue(Op::read(
                View::Volumes(Some(Box::new(FormKind::VolumeDefault))),
                WorkspaceRequest::Volumes {},
            ));
            manager.status = "Reading mounted volumes…".into();
        }
        Action::Rename => {
            if let Some(entity) = selected_entity(manager) {
                open(manager, FormKind::Rename(entity.id()));
            }
        }
        Action::Move => {
            if let Some(location) = selected_location(manager) {
                let kind = if location.home.is_none() {
                    FormKind::AdoptStandalone(location.id)
                } else {
                    FormKind::Move(location.id)
                };
                open(manager, kind);
            }
        }
        Action::Rebind => {
            if let Some(location) = selected_location(manager) {
                let kind = FormKind::Rebind {
                    location: location.id,
                    old_path: location.observed_path.clone(),
                    generation: location.binding_generation,
                };
                open(manager, kind);
            }
        }
        Action::Archive => {
            if let Some(entity) = selected_entity(manager) {
                let archived = !matches!(state_of(entity), OrganizationState::Archived);
                let target = entity.id();
                review_change(manager, OrganizationChange::Archive { target, archived });
            }
        }
        Action::Retire => {
            if let Some(entity) = selected_entity(manager) {
                let target = entity.id();
                review_change(manager, OrganizationChange::Retire { target });
            }
        }
        Action::Discard => {
            if let Some(entity) = selected_entity(manager) {
                let target = entity.id();
                review_change(manager, OrganizationChange::DiscardUnused { target });
            }
        }
        Action::ShowSessions => {
            manager.sessions.target = manager.org.selected;
            manager.sessions.after = None;
            manager.sessions.previous.clear();
            manager.switch(Section::Sessions);
        }
        Action::ClearFilter => {
            manager.sessions.target = None;
            manager.sessions.after = None;
            manager.sessions.previous.clear();
            manager.load_sessions();
        }
        Action::Launch => match manager.caps.launch {
            Some(true) => open(manager, FormKind::Launch),
            Some(false) => manager.note(
                Tone::Warn,
                "Managed launch is staged on this runtime (features.managed_primary_launch = false). Existing sessions are unaffected.",
            ),
            None => manager.note(Tone::Muted, "Launch capability is still being negotiated."),
        },
        Action::BeginCloseout => {
            if let Some(location) = selected_location(manager) {
                let kind = FormKind::CloseoutBegin {
                    location: location.id,
                    generation: location.binding_generation,
                };
                open(manager, kind);
            }
        }
        Action::StartupCopy => {
            if let Some(location) = selected_location(manager) {
                open(manager, FormKind::StartupCopy(location.id));
            }
        }
        Action::OpenHere | Action::OpenTerminal => {
            let session = match manager.section {
                Section::Sessions => manager.selected_key(),
                _ => match selected_operation(manager).map(|entry| &entry.operation) {
                    Some(WorkspaceOperation::PrimaryLaunch(record)) => Some(record.session.clone()),
                    _ => manager.launched.as_ref().map(|record| record.session.clone()),
                },
            };
            if let Some(session) = session {
                let intent = if action == Action::OpenHere {
                    Intent::Resume(session)
                } else {
                    let cwd = manager
                        .sessions
                        .views
                        .get(&session)
                        .and_then(|view| view.location.as_ref().map(|l| l.cwd.clone()));
                    Intent::OpenTerminal { session, cwd }
                };
                manager.intents.push_back(intent);
            }
        }
        Action::ChangeLocation => {
            if !manager.caps.location_enabled {
                manager.note(Tone::Warn, "Managed location changes are staged on this runtime (features.managed_primary_launch = false).");
            } else if let Some(view) = selected_view(manager).cloned() {
                open(manager, FormKind::SessionMove(Box::new(view)));
            }
        }
        Action::AdoptLegacy => {
            if !manager.caps.location_enabled || !manager.caps.adoption {
                manager.note(Tone::Warn, "Legacy adoption is staged on this runtime (features.managed_primary_launch = false).");
            } else if let Some(view) = selected_view(manager).cloned() {
                open(manager, FormKind::SessionAdopt(Box::new(view)));
            }
        }
        Action::CancelChange => {
            let operation = match manager.section {
                Section::Sessions => selected_view(manager)
                    .and_then(|view| view.pending.first())
                    .map(|record| record.operation),
                _ => match selected_operation(manager).map(|entry| &entry.operation) {
                    Some(WorkspaceOperation::PrimaryLocation(record)) => Some(record.operation),
                    _ => None,
                },
            };
            if let Some(operation) = operation {
                manager.confirm = Some(confirm(
                    "Cancel pending location change",
                    vec![(Tone::Normal, format!("Operation {operation}. A change already committed cannot be cancelled."))],
                    Op::location(View::Effect, PrimaryLocationCommand::Cancel { operation }),
                ));
            }
        }
        Action::NewContext => {
            if !manager.caps.location_enabled {
                manager.note(Tone::Warn, "Scoped new contexts are staged on this runtime (features.managed_primary_launch = false). /clear, /split and /transfer keep their existing behavior.");
            } else {
                let session = manager.session.clone();
                open(manager, FormKind::NewContext(session));
            }
        }
        Action::WriteScope => {
            if let Some(session) = manager.selected_key() {
                manager.sessions.scope = None;
                manager.queue(Op::permission(
                    View::Scope(session.clone()),
                    PermissionRequest::Scope { session },
                ));
            }
        }
        Action::InspectSession => open(manager, FormKind::InspectSession),
        Action::GrantSession | Action::NewGrant => open(manager, FormKind::Grant(None)),
        Action::ReconcileScope | Action::AbandonScope => {
            if let Some(session) = manager.selected_key() {
                let (title, request, warn) = if action == Action::ReconcileScope {
                    (
                        "Reconcile context scope",
                        PermissionRequest::ReconcileContextScope { session: session.clone() },
                        "Publishes exactly the committed scope of an interrupted new context, if its checkpoint is ready.",
                    )
                } else {
                    (
                        "Abandon unpublished context scope",
                        PermissionRequest::AbandonContextScope { session: session.clone() },
                        "Fails only unpublished intent. Published grant copies are never undone.",
                    )
                };
                manager.confirm = Some(confirm(
                    title,
                    vec![(Tone::Normal, format!("Session {session}.")), (Tone::Muted, warn.into())],
                    Op::permission(View::Effect, request),
                ));
            }
        }
        Action::ToggleFinished => {
            manager.ops.all = !manager.ops.all;
            manager.ops.cursor = None;
            manager.ops.previous.clear();
            manager.ops.selected = None;
            manager.load_ops();
        }
        Action::CancelClone | Action::ResumeClone => {
            if let Some(WorkspaceOperation::Clone(record)) =
                selected_operation(manager).map(|entry| entry.operation.clone())
            {
                let request = record.request;
                let (title, request_op, lines) = if action == Action::CancelClone {
                    (
                        "Cancel clone",
                        WorkspaceRequest::CancelClone { request },
                        vec![
                            (Tone::Normal, format!("{} → {}", record.review.spec.name, record.review.destination.display())),
                            (Tone::Muted, "Stops owned Git work. Only an empty, witnessed, owned stage is removed; other content stays for inspection.".to_string()),
                        ],
                    )
                } else {
                    (
                        "Resume clone",
                        WorkspaceRequest::ResumeClone { request },
                        vec![
                            (Tone::Normal, format!("{} from its retained stage.", record.review.spec.name)),
                            (Tone::Muted, "Waits for the previous run to be terminal and continues without reacquiring Git.".to_string()),
                        ],
                    )
                };
                manager.confirm = Some(confirm(title, lines, Op::read(View::Effect, request_op)));
            }
        }
        Action::TrustClone => {
            if let Some(WorkspaceOperation::Clone(record)) =
                selected_operation(manager).map(|entry| entry.operation.clone())
            {
                let expected_revision = revision(manager);
                manager.queue(Op::read(
                    View::Review,
                    WorkspaceRequest::ReviewCloneTrust {
                        clone: record.request,
                        expected_revision,
                    },
                ));
            }
        }
        Action::CloneOutput => {
            if let Some(WorkspaceOperation::Clone(record)) =
                selected_operation(manager).map(|entry| entry.operation.clone())
            {
                let record = manager
                    .ops
                    .clone
                    .clone()
                    .filter(|fresh| fresh.request == record.request)
                    .unwrap_or(*record);
                if let Some(run_id) = record.output_runs.last().cloned() {
                    manager.queue(Op::read(
                        View::CloneOutput,
                        WorkspaceRequest::CloneOutput {
                            clone: record.request,
                            request: jcode_tool_types::execution::ExecutionRequest::Read {
                                run_id,
                                content: jcode_tool_types::execution::ExecutionContent::Output,
                                read_point: None,
                                output_size: None,
                            },
                        },
                    ));
                }
            }
        }
        Action::PermMode => {
            manager.perms.mode = match manager.perms.mode {
                PermMode::Proposals => PermMode::Grants,
                PermMode::Grants => PermMode::Imported,
                PermMode::Imported => PermMode::Proposals,
            };
            manager.perms.cursor = None;
            manager.perms.previous.clear();
            manager.perms.selected = None;
            manager.load_perms();
        }
        Action::Approve => {
            if let Some(PermissionItem::Proposal(proposal)) = selected_permission(manager).cloned() {
                open(manager, FormKind::Grant(Some(proposal.id)));
                let (kind, value) = describe::target_choice(manager, &proposal.target);
                if let Some((_, form)) = &mut manager.form {
                    form.set("audience_kind", "session");
                    form.set("session", proposal.session.clone());
                    form.set("target_kind", kind);
                    form.set("target", value);
                }
            }
        }
        Action::Decline => {
            if let Some(PermissionItem::Proposal(proposal)) = selected_permission(manager).cloned() {
                manager.confirm = Some(confirm(
                    "Decline access proposal",
                    vec![
                        (Tone::Normal, format!("Session {} asked for {}.", proposal.session, describe::target(manager, &proposal.target))),
                        (Tone::Muted, format!("Reason: {}", proposal.reason)),
                        (Tone::Muted, "No grant is created. The session can propose again.".into()),
                    ],
                    Op::permission(
                        View::Effect,
                        PermissionRequest::DecideProposal {
                            request: new_request(),
                            proposal: proposal.id,
                            expected_revision: proposal.revision,
                            decision: ProposalDecision::Decline,
                        },
                    ),
                ));
            }
        }
        Action::Revoke => {
            if let Some(PermissionItem::Grant(grant)) = selected_permission(manager).cloned() {
                review_grant(manager, GrantChange::Revoke { grant: grant.id });
            }
        }
        Action::ActivateImported => {
            let grant = match selected_permission(manager) {
                Some(PermissionItem::Grant(grant)) => Some(grant.id),
                _ => selected_imported(manager).and_then(|item| item.bound_grant),
            };
            if let Some(grant) = grant {
                review_grant(manager, GrantChange::ActivateImported { grant });
            }
        }
        Action::BindImported => {
            if let Some(item) = selected_imported(manager).cloned() {
                open(manager, FormKind::BindImported(Box::new(item)));
            }
        }
        Action::InventoryRefresh => closeout_action(manager, CloseoutAction::Refresh),
        Action::Preserve => closeout_action(manager, CloseoutAction::Preserve),
        Action::ReviewRemoval => closeout_action(manager, CloseoutAction::ReviewRemoval),
        Action::Finish => {
            if let Some(record) = manager.closeouts.record.clone() {
                let mut confirm = confirm(
                    "Finish authorized removal",
                    vec![
                        (Tone::Warn, format!("Removes the inventoried entries of {} through the journaled quarantine.", record.spec.location)),
                        (Tone::Muted, "Unexpected new entries stop removal and stay recoverable. Closed history and preservation remain.".into()),
                    ],
                    closeout_execute(&record, CloseoutAction::Finish),
                );
                confirm.typed = Some("remove");
                manager.confirm = Some(confirm);
            }
        }
        Action::ApproveRemoval => {
            if let (Some(record), Some(review)) =
                (manager.closeouts.record.clone(), manager.closeouts.review.clone())
            {
                let mut lines = describe::closeout_review_lines(&review);
                lines.insert(0, (Tone::Warn, format!("Approve removal of {} for this exact review.", record.spec.location)));
                let mut confirm = confirm(
                    "Approve checkout removal",
                    lines,
                    closeout_execute(&record, CloseoutAction::ApproveRemoval { review: review.id }),
                );
                confirm.typed = Some("approve");
                manager.confirm = Some(confirm);
            }
        }
        Action::RevokeCloseout => {
            if let Some(record) = manager.closeouts.record.clone() {
                manager.confirm = Some(confirm(
                    "Revoke closeout",
                    vec![
                        (Tone::Normal, format!("Ends closeout {} for {}.", record.operation, record.spec.location)),
                        (Tone::Muted, "Conditional authorization ends too. Files and preservation stay.".into()),
                    ],
                    Op::closeout(
                        View::Effect,
                        CloseoutRequest::Revoke {
                            request: new_request(),
                            operation: record.operation,
                            expected_revision: record.revision,
                        },
                    ),
                ));
            }
        }
        Action::ReviewRecovery => {
            if let Some(record) = &manager.closeouts.record {
                let kind = FormKind::CloseoutRecovery {
                    operation: record.operation,
                    revision: record.revision,
                };
                open(manager, kind);
            }
        }
        Action::ApplyRecovery => {
            if let (Some(record), Some(recovery)) =
                (manager.closeouts.record.clone(), manager.closeouts.recovery.clone())
            {
                manager.confirm = Some(confirm(
                    "Apply closeout recovery",
                    describe::recovery_lines(&recovery),
                    closeout_execute(&record, CloseoutAction::ApplyRecovery { review: recovery.id }),
                ));
            }
        }
        Action::InventoryPane => {
            if let Some(record) = &manager.closeouts.record
                && let Some(digest) = record.inventory_digest.clone()
            {
                let operation = record.operation;
                manager.closeouts.pane = CloseoutPane::Inventory;
                manager.queue(Op::closeout(
                    View::CloseoutInventory,
                    CloseoutRequest::Inventory {
                        operation,
                        digest,
                        after: manager.closeouts.inventory_after,
                        limit: 200,
                    },
                ));
            }
        }
        Action::RemovalPane => {
            if let Some(record) = &manager.closeouts.record {
                let (operation, expected_revision) = (record.operation, record.revision);
                manager.closeouts.pane = CloseoutPane::Removal;
                manager.queue(Op::closeout(
                    View::CloseoutRemoval,
                    CloseoutRequest::RemovalProgress {
                        operation,
                        expected_revision,
                        after: 0,
                        limit: 200,
                    },
                ));
            }
        }
        Action::HistoryPane => {
            if let Some(record) = &manager.closeouts.record {
                let location = record.spec.location;
                manager.closeouts.pane = CloseoutPane::History;
                manager.queue(Op::closeout(
                    View::CloseoutHistory,
                    CloseoutRequest::History { location },
                ));
            }
        }
        Action::RecordPane => manager.closeouts.pane = CloseoutPane::Record,
        Action::Disposition => {
            if let (Some(record), Some(entry)) = (
                manager.closeouts.record.as_ref(),
                manager
                    .closeouts
                    .inventory
                    .as_ref()
                    .and_then(|page| page.entries.get(manager.closeouts.entry)),
            ) {
                let kind = FormKind::Disposition {
                    operation: record.operation,
                    revision: record.revision,
                    entry: entry.id.clone(),
                };
                open(manager, kind);
            }
        }
        Action::Backup => open(manager, FormKind::Backup),
        Action::Export => open(manager, FormKind::Export),
        Action::Import => open(manager, FormKind::Import),
        Action::Restore => {
            if let Some(snapshot) = manager.backups.selected {
                manager.queue(Op::read(View::Review, WorkspaceRequest::ReviewRestore { snapshot }));
            }
        }
        Action::Stop => open(manager, FormKind::Shutdown(RuntimeDestination::Stopped)),
        Action::Restart => open(manager, FormKind::Shutdown(RuntimeDestination::Restart)),
        Action::ChangeShutdown => {
            if let Some(op) = manager.runtime.operation.clone() {
                open(
                    manager,
                    FormKind::ShutdownChange {
                        operation: op.id,
                        revision: op.revision,
                        destination: op.review.options.destination,
                    },
                );
            }
        }
        Action::CancelWait | Action::RetryShutdown | Action::Force => {
            if let Some(op) = manager.runtime.operation.clone() {
                let (title, request, line) = match action {
                    Action::CancelWait => (
                        "Cancel waiting shutdown",
                        RuntimeRequest::CancelWait { operation: op.id, expected_revision: op.revision },
                        "Restores admission exactly once. Work admitted meanwhile keeps running.",
                    ),
                    Action::RetryShutdown => (
                        "Retry quiescence",
                        RuntimeRequest::Retry { operation: op.id, expected_revision: op.revision },
                        "Retries quiescence only. No work or effect is replayed.",
                    ),
                    _ => (
                        "Force shutdown",
                        RuntimeRequest::Force { operation: op.id, expected_revision: op.revision },
                        "Terminates only proven reviewed owners. Remaining outcomes become uncertain recovery items; nothing is rolled back.",
                    ),
                };
                let mut confirm = confirm(
                    title,
                    vec![
                        (Tone::Normal, format!("Operation {} is {:?} at revision {}.", op.id, op.phase, op.revision)),
                        (if action == Action::Force { Tone::Warn } else { Tone::Muted }, line.into()),
                    ],
                    Op::runtime(View::Effect, request),
                );
                if action == Action::Force {
                    confirm.typed = Some("force");
                }
                manager.confirm = Some(confirm);
            }
        }
        Action::Continue | Action::Leave => {
            if let Some(item) = selected_recovery(manager).cloned() {
                let decision = if action == Action::Continue {
                    RecoveryDecision::Continue
                } else {
                    RecoveryDecision::LeaveStopped
                };
                let mut lines = describe::recovery_item_lines(&item);
                lines.push((
                    Tone::Muted,
                    if action == Action::Continue {
                        "Sends one durable continuation that inspects retained results before repeating effects.".into()
                    } else {
                        "Keeps the session stopped. A later message from you continues it normally.".into()
                    },
                ));
                manager.confirm = Some(confirm(
                    if action == Action::Continue { "Continue interrupted turn" } else { "Leave interrupted turn stopped" },
                    lines,
                    Op::runtime(
                        View::Effect,
                        RuntimeRequest::Recover {
                            item: item.id,
                            expected_revision: item.revision,
                            request: new_request(),
                            decision,
                        },
                    ),
                ));
            }
        }
        Action::Start => manager.intents.push_back(Intent::StartRuntime),
    }
}

fn state_of(entity: &Entity) -> OrganizationState {
    match entity {
        Entity::Project(p) => p.state,
        Entity::Repository(r) => r.state,
        Entity::WorkArea(a) => a.state,
        Entity::Location(l) => {
            if l.retired {
                OrganizationState::Retired
            } else {
                OrganizationState::Active
            }
        }
    }
}

fn reset_org_page(manager: &mut WorkspaceManager) {
    manager.org.cursor = None;
    manager.org.previous.clear();
    manager.org.page = None;
    manager.org.selected = None;
    manager.load_org();
}

fn page(manager: &mut WorkspaceManager, forward: bool) {
    match manager.section {
        Section::Organization => {
            if forward {
                if let Some(next) = manager.org.page.as_ref().and_then(|p| p.next.clone()) {
                    let current = manager.org.cursor.replace(next);
                    manager.org.previous.push(current);
                }
            } else if let Some(previous) = manager.org.previous.pop() {
                manager.org.cursor = previous;
            } else {
                return;
            }
            manager.org.selected = None;
            manager.load_org();
        }
        Section::Sessions => {
            if forward {
                if let Some(next) = manager.sessions.next.clone() {
                    let current = manager.sessions.after.replace(next);
                    manager.sessions.previous.push(current);
                }
            } else if let Some(previous) = manager.sessions.previous.pop() {
                manager.sessions.after = previous;
            } else {
                return;
            }
            manager.load_sessions();
        }
        Section::Operations => {
            if forward {
                if let Some(next) = manager.ops.page.as_ref().and_then(|p| p.next.clone()) {
                    let current = manager.ops.cursor.replace(next);
                    manager.ops.previous.push(current);
                }
            } else if let Some(previous) = manager.ops.previous.pop() {
                manager.ops.cursor = previous;
            } else {
                return;
            }
            manager.ops.selected = None;
            manager.load_ops();
        }
        Section::Permissions => {
            let next = manager
                .perms
                .page
                .as_ref()
                .and_then(|p| p.next.clone())
                .or_else(|| manager.perms.imported.as_ref().and_then(|i| i.2.clone()));
            if forward {
                if let Some(next) = next {
                    let current = manager.perms.cursor.replace(next);
                    manager.perms.previous.push(current);
                }
            } else if let Some(previous) = manager.perms.previous.pop() {
                manager.perms.cursor = previous;
            } else {
                return;
            }
            manager.perms.selected = None;
            manager.load_perms();
        }
        _ => {}
    }
}

fn closeout_execute(record: &CloseoutRecord, action: CloseoutAction) -> Op {
    Op::closeout(
        View::Effect,
        CloseoutRequest::Execute {
            request: new_request(),
            spec: CloseoutActionSpec {
                operation: record.operation,
                expected_revision: record.revision,
                action,
            },
        },
    )
}

fn closeout_action(manager: &mut WorkspaceManager, action: CloseoutAction) {
    if let Some(record) = manager.closeouts.record.clone() {
        let (title, line) = match &action {
            CloseoutAction::Refresh => (
                "Refresh closeout inventory",
                "Re-inventories files, Git state, references and live work. Nothing is removed.",
            ),
            CloseoutAction::Preserve => (
                "Preserve checkout information",
                "Writes verified preservation for entries that need it. Nothing is removed.",
            ),
            _ => (
                "Review removal",
                "Rechecks preservation and live work, then prepares a removal review for your approval.",
            ),
        };
        manager.confirm = Some(confirm(
            title,
            vec![
                (
                    Tone::Normal,
                    format!(
                        "Closeout {} of {} at revision {}.",
                        record.operation, record.spec.location, record.revision
                    ),
                ),
                (Tone::Muted, line.into()),
            ],
            closeout_execute(&record, action),
        ));
    }
}

fn choices(manager: &WorkspaceManager, kinds: &[&str]) -> Vec<Choice> {
    let mut out = Vec::new();
    for entity in manager.known.values() {
        let (kind, name) = match entity {
            Entity::Project(p) if p.state != OrganizationState::Retired => {
                ("project", format!("Project · {}", p.name))
            }
            Entity::WorkArea(a) if a.state != OrganizationState::Retired => (
                "area",
                format!(
                    "Area · {}/{}",
                    describe::project_name(manager, a.project),
                    a.name
                ),
            ),
            Entity::Repository(r) if r.state != OrganizationState::Retired => {
                ("repository", format!("Repository · {}", r.name))
            }
            Entity::Location(l) if !l.lifecycle.is_historical() && !l.retired => {
                let kind = match l.kind {
                    LocationKind::Checkout { .. } => "checkout",
                    LocationKind::Directory => "directory",
                    LocationKind::Standalone { .. } => "standalone",
                };
                (
                    kind,
                    format!(
                        "{} · {} ({})",
                        describe::title(kind),
                        l.name,
                        l.observed_path.display()
                    ),
                )
            }
            _ => continue,
        };
        if kinds.contains(&kind) {
            out.push(Choice::new(format!("{kind}:{}", entity.id()), name));
        }
    }
    out
}

fn home_value(home: Option<Home>) -> String {
    match home {
        Some(Home::Project(id)) => format!("project:{id}"),
        Some(Home::WorkArea(id)) => format!("area:{id}"),
        None => String::new(),
    }
}

fn context_home(manager: &WorkspaceManager) -> Option<Home> {
    match selected_entity(manager)? {
        Entity::Project(p) => Some(Home::Project(p.id)),
        Entity::WorkArea(a) => Some(Home::WorkArea(a.id)),
        Entity::Location(l) => l.home,
        Entity::Repository(_) => None,
    }
}

fn context_project(manager: &WorkspaceManager) -> Option<ProjectId> {
    match context_home(manager)? {
        Home::Project(id) => Some(id),
        Home::WorkArea(id) => match manager.known.get(&id.to_string()) {
            Some(Entity::WorkArea(area)) => Some(area.project),
            _ => None,
        },
    }
}

fn parse_id<T: std::str::FromStr>(value: &str, prefix: &str) -> Option<T> {
    value.strip_prefix(prefix)?.strip_prefix(':')?.parse().ok()
}

fn parse_home(value: &str) -> Result<Home, String> {
    parse_id(value, "project")
        .map(Home::Project)
        .or_else(|| parse_id(value, "area").map(Home::WorkArea))
        .ok_or_else(|| "Choose a project or work area".into())
}

fn parse_placement(value: &str) -> Result<Placement, String> {
    parse_id(value, "project")
        .map(Placement::Project)
        .or_else(|| parse_id(value, "area").map(Placement::WorkArea))
        .or_else(|| parse_id(value, "checkout").map(Placement::Checkout))
        .or_else(|| parse_id(value, "directory").map(Placement::Directory))
        .or_else(|| parse_id(value, "standalone").map(Placement::Standalone))
        .ok_or_else(|| "Choose a placement".into())
}

fn placement_value(placement: Placement) -> String {
    match placement {
        Placement::Project(id) => format!("project:{id}"),
        Placement::WorkArea(id) => format!("area:{id}"),
        Placement::Checkout(id) => format!("checkout:{id}"),
        Placement::Directory(id) => format!("directory:{id}"),
        Placement::Standalone(id) => format!("standalone:{id}"),
    }
}

fn context_placement(manager: &WorkspaceManager) -> Option<Placement> {
    Some(match selected_entity(manager)? {
        Entity::Project(p) => Placement::Project(p.id),
        Entity::WorkArea(a) => Placement::WorkArea(a.id),
        Entity::Location(l) => match l.kind {
            LocationKind::Checkout { .. } => Placement::Checkout(l.id),
            LocationKind::Directory => Placement::Directory(l.id),
            LocationKind::Standalone { .. } => Placement::Standalone(l.id),
        },
        Entity::Repository(_) => return None,
    })
}

fn location_root(manager: &WorkspaceManager, placement: Placement) -> Option<PathBuf> {
    let id = match placement {
        Placement::Checkout(id) | Placement::Directory(id) | Placement::Standalone(id) => id,
        _ => return None,
    };
    match manager.known.get(&id.to_string()) {
        Some(Entity::Location(location)) => Some(location.observed_path.clone()),
        _ => None,
    }
}

fn absolute(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{label} must be an absolute path"))
    }
}

fn placement_choices(manager: &WorkspaceManager) -> Vec<Choice> {
    choices(
        manager,
        &["project", "area", "checkout", "directory", "standalone"],
    )
}

fn audience_fields(manager: &WorkspaceManager) -> Vec<Field> {
    vec![
        Field::choice(
            "audience_kind",
            "Who receives access",
            vec![
                Choice::new("session", "One session"),
                Choice::new("project", "Sessions of a project (existing and future)"),
                Choice::new("area", "Sessions of a work area (existing and future)"),
                Choice::new("checkout", "Sessions of a checkout (existing and future)"),
            ],
            "session",
        ),
        Field::text(
            "session",
            "Session ID",
            manager
                .selected_key()
                .filter(|_| manager.section == Section::Sessions)
                .unwrap_or_else(|| manager.session.clone()),
        )
        .required()
        .when("audience_kind", &["session"]),
        Field::choice(
            "audience_project",
            "Project",
            choices(manager, &["project"]),
            "",
        )
        .when("audience_kind", &["project"]),
        Field::choice(
            "audience_area",
            "Work area",
            choices(manager, &["area"]),
            "",
        )
        .when("audience_kind", &["area"]),
        Field::choice(
            "audience_checkout",
            "Checkout",
            choices(manager, &["checkout"]),
            "",
        )
        .when("audience_kind", &["checkout"]),
        Field::choice(
            "target_kind",
            "Writable target",
            vec![
                Choice::new("root", "One registered root"),
                Choice::new("project", "Current and future member roots of a project"),
                Choice::new("area", "Current and future member roots of a work area"),
            ],
            "root",
        ),
        Field::choice(
            "target",
            "Root",
            choices(manager, &["checkout", "directory", "standalone"]),
            "",
        )
        .when("target_kind", &["root"]),
        Field::choice(
            "target_project",
            "Project",
            choices(manager, &["project"]),
            "",
        )
        .when("target_kind", &["project"]),
        Field::choice("target_area", "Work area", choices(manager, &["area"]), "")
            .when("target_kind", &["area"]),
    ]
}

fn parse_audience(form: &Form) -> Result<Audience, String> {
    match form.value("audience_kind") {
        "session" => {
            let session = form.value("session").trim();
            if session.is_empty() || session.chars().any(char::is_control) {
                return Err("Enter a valid session ID".into());
            }
            Ok(Audience::Session(session.into()))
        }
        "project" => parse_id(form.value("audience_project"), "project")
            .map(Audience::Project)
            .ok_or_else(|| "Choose a project".into()),
        "area" => parse_id(form.value("audience_area"), "area")
            .map(Audience::WorkArea)
            .ok_or_else(|| "Choose a work area".into()),
        _ => parse_id(form.value("audience_checkout"), "checkout")
            .map(Audience::Checkout)
            .ok_or_else(|| "Choose a checkout".into()),
    }
}

fn parse_target(form: &Form) -> Result<WriteTarget, String> {
    match form.value("target_kind") {
        "root" => {
            let value = form.value("target");
            parse_id(value, "checkout")
                .or_else(|| parse_id(value, "directory"))
                .or_else(|| parse_id(value, "standalone"))
                .map(WriteTarget::Root)
                .ok_or_else(|| "Choose a root".into())
        }
        "project" => parse_id(form.value("target_project"), "project")
            .map(WriteTarget::ProjectMembers)
            .ok_or_else(|| "Choose a project".into()),
        _ => parse_id(form.value("target_area"), "area")
            .map(WriteTarget::WorkAreaMembers)
            .ok_or_else(|| "Choose a work area".into()),
    }
}

fn shutdown_fields(destination: RuntimeDestination) -> Vec<Field> {
    vec![
        Field::choice(
            "strategy",
            "Current work",
            vec![
                Choice::new(
                    "finish",
                    "Finish current work, then stop (cancellable while waiting)",
                ),
                Choice::new("interrupt", "Interrupt current work"),
            ],
            "finish",
        ),
        Field::choice(
            "independent",
            "Independent native commands",
            vec![
                Choice::new("stop", "Stop them with the runtime"),
                Choice::new("keep", "Keep verified native commands running"),
            ],
            "stop",
        ),
        Field::text(
            "quiescence",
            "Quiescence deadline after stopping begins (seconds)",
            "30",
        )
        .required(),
        Field::text(
            "destination",
            "Afterwards",
            if destination.is_stopped() {
                "Stay stopped until explicit Start"
            } else {
                "Start a fresh runtime; interrupted turns continue once"
            },
        ),
    ]
}

fn shutdown_options(
    form: &Form,
    destination: RuntimeDestination,
) -> Result<ShutdownOptions, String> {
    let seconds: u32 = form
        .value("quiescence")
        .trim()
        .parse()
        .map_err(|_| "Quiescence deadline must be a whole number of seconds".to_string())?;
    if seconds == 0 {
        return Err("Quiescence deadline must be at least one second".into());
    }
    Ok(ShutdownOptions {
        strategy: if form.value("strategy") == "interrupt" {
            StopStrategy::Interrupt
        } else {
            StopStrategy::FinishCurrent
        },
        independent: if form.value("independent") == "keep" {
            IndependentTasks::KeepSupported
        } else {
            IndependentTasks::Stop
        },
        quiescence_timeout_seconds: seconds,
        destination,
    })
}

/// Open the form for an action, prefilled from the current selection.
pub(crate) fn open(manager: &mut WorkspaceManager, kind: FormKind) {
    manager.draft = None;
    let form = match &kind {
        FormKind::CreateProject => Form::new("New project", "Review", vec![
            Field::text("name", "Name", "").required(),
        ]),
        FormKind::CreateRepository => Form::new("New logical repository", "Review", vec![
            Field::text("name", "Name", "").required(),
            Field::text("remotes", "Remote URLs (comma separated, optional)", "")
                .hint("Descriptive references only; equal URLs do not merge identities."),
        ]),
        FormKind::CreateArea => Form::new("New work area", "Review", vec![
            Field::choice("project", "Project", choices(manager, &["project"]), &context_project(manager).map(|p| format!("project:{p}")).unwrap_or_default()),
            Field::text("name", "Name", "").required(),
        ]),
        FormKind::Register => Form::new("Register an existing path", "Review", vec![
            Field::choice("kind", "Kind", vec![
                Choice::new("checkout", "Git checkout (adopt; files unchanged)"),
                Choice::new("directory", "Directory reference"),
                Choice::new("standalone", "Standalone location (no project)"),
            ], "checkout"),
            Field::text("path", "Absolute path", "").required(),
            Field::text("name", "Display name", "").required(),
            Field::choice("home", "Home", choices(manager, &["project", "area"]), &home_value(context_home(manager)))
                .when("kind", &["checkout", "directory"]),
            Field::choice("repository", "Logical repository", choices(manager, &["repository"]), &match selected_entity(manager) {
                Some(Entity::Repository(r)) => format!("repository:{}", r.id),
                _ => String::new(),
            }).when("kind", &["checkout"]),
        ]).intro(&["Registration records an existing root. It never clones, moves or changes files."]),
        FormKind::Associate => Form::new("Associate repository with project", "Review", vec![
            Field::choice("project", "Project", choices(manager, &["project"]), &context_project(manager).map(|p| format!("project:{p}")).unwrap_or_default()),
            Field::choice("repository", "Repository", choices(manager, &["repository"]), &match selected_entity(manager) {
                Some(Entity::Repository(r)) => format!("repository:{}", r.id),
                _ => String::new(),
            }),
            Field::choice("mode", "Change", vec![Choice::new("add", "Associate"), Choice::new("remove", "Remove association")], "add"),
        ]),
        FormKind::Rename(target) => {
            let name = manager.known.get(&target.to_string()).map(describe::entity_name).unwrap_or_default();
            Form::new("Rename", "Review", vec![Field::text("name", "New display name", name).required()])
                .intro(&["Display names are metadata. Identity, paths and authority do not change."])
        }
        FormKind::Move(_) => Form::new("Move location to another home", "Review", vec![
            Field::choice("home", "New home", choices(manager, &["project", "area"]), ""),
            Field::toggle("associate", "Also associate its repository with the new project", true),
        ]).intro(&["Organization only: files, sessions' cwd and history do not move."]),
        FormKind::AdoptStandalone(_) => Form::new("Adopt standalone location into a project", "Review", vec![
            Field::choice("home", "Home", choices(manager, &["project", "area"]), ""),
            Field::choice("repository", "Repository (Git roots)", {
                let mut c = vec![Choice::new("", "None (directory)")];
                c.extend(choices(manager, &["repository"]));
                c
            }, ""),
            Field::toggle("associate", "Associate the repository with the project", true),
        ]).intro(&["Keeps the stable location identity and its history."]),
        FormKind::Rebind { old_path, generation, .. } => Form::new("Rebind relocated root", "Review", vec![
            Field::text("new_path", "New absolute path", "").required(),
        ]).intro(&[&format!("Current binding: {} (generation {generation}).", old_path.display()), "Rebind verifies the new volume and root. Sessions change cwd only through their own reviewed moves."]),
        FormKind::VolumeDefault => Form::new("Default checkout directory for a volume", "Review", vec![
            Field::choice("volume", "Volume", describe::volume_choices(manager), ""),
            Field::text("path", "Absolute directory on that volume", "").required(),
        ]),
        FormKind::Clone => {
            let repository = match selected_entity(manager) {
                Some(Entity::Repository(r)) => Some(r.clone()),
                _ => None,
            };
            let url = repository.as_ref().and_then(|r| r.remotes.first().cloned()).unwrap_or_default();
            Form::new("Clone an independent checkout", "Review", vec![
                Field::text("name", "Checkout name", repository.as_ref().map(|r| r.name.clone()).unwrap_or_default()).required(),
                Field::choice("home", "Home", choices(manager, &["project", "area"]), &home_value(context_home(manager))),
                Field::choice("repository", "Logical repository", choices(manager, &["repository"]), &repository.as_ref().map(|r| format!("repository:{}", r.id)).unwrap_or_default()),
                Field::choice("source", "Source", vec![Choice::new("remote", "Git remote"), Choice::new("local", "Existing local checkout (committed work only)")], "remote"),
                Field::text("url", "Remote URL", url.clone()).required().when("source", &["remote"]),
                Field::text("local", "Local checkout path", "").required().when("source", &["local"]),
                Field::choice("base_kind", "Base", vec![Choice::new("branch", "Branch"), Choice::new("tag", "Tag"), Choice::new("commit", "Commit")], "branch"),
                Field::text("base", "Branch, tag or commit", "main").required(),
                Field::choice("branch", "Checkout branch", vec![Choice::new("keep", "Keep the source branch name"), Choice::new("create", "Create a new branch"), Choice::new("detached", "Detached HEAD")], "keep"),
                Field::text("new_branch", "New branch name", "").required().when("branch", &["create"]),
                Field::text("remotes", "Resulting remotes (name=url, comma separated)", if url.is_empty() { String::new() } else { format!("origin={url}") }),
                Field::choice("volume", "Volume", describe::volume_choices(manager), ""),
                Field::choice("destination", "Destination", vec![Choice::new("default", "Volume default (…/jcode-checkouts/<project>/<checkout>)"), Choice::new("custom", "Custom absolute path")], "default"),
                Field::text("project_component", "Project directory name", describe::home_project_name(manager, context_home(manager))).required().when("destination", &["default"]),
                Field::text("custom", "Custom path on that volume", "").required().when("destination", &["custom"]),
                Field::toggle("submodules", "Materialize submodules at recorded gitlinks", true),
                Field::toggle("lfs", "Materialize Git LFS content", true),
            ]).intro(&["Creates an independent full clone. Nothing starts until you confirm the review."])
        }
        FormKind::StartupCopy(target) => Form::new("Copy Startup Context selection", "Review", vec![
            Field::text("source", "Source root (absolute path of a Git or directory root)", "").required(),
        ]).intro(&[&format!("Target location {target}. Paths are copied and recaptured; file bodies are not copied.")]),
        FormKind::Launch => {
            let placement = context_placement(manager);
            let cwd = placement.and_then(|p| location_root(manager, p)).map(|p| p.display().to_string()).unwrap_or_default();
            let mut placements = placement_choices(manager);
            placements.push(Choice::new("new-standalone", "New standalone location (no project)"));
            Form::new("Launch a primary session", "Review", vec![
                Field::choice("placement", "Placement", placements, &placement.map(placement_value).unwrap_or_default()),
                Field::text("root", "Standalone root (absolute)", "").required().when("placement", &["new-standalone"]),
                Field::choice("cwd_mode", "Command working directory", vec![Choice::new("existing", "Existing directory"), Choice::new("create", "Create a new empty directory")], "existing"),
                Field::text("cwd", "Working directory (absolute)", cwd).required(),
                Field::text("agent", "Agent profile (empty: default)", ""),
                Field::text("model", "Model (empty: default route)", ""),
                Field::text("provider", "Provider", "").required().when("model_set", &["true"]),
                Field::text("api_method", "API method", "").required().when("model_set", &["true"]),
                Field::text("effort", "Effort (optional)", "").when("model_set", &["true"]),
                Field::toggle("model_set", "Choose an explicit route", false),
                Field::toggle("selfdev", "Self-development session", false),
            ]).intro(&["Instructions and Startup Context follow the concrete cwd. No checkout is created."])
        }
        FormKind::SessionMove(view) => {
            let location = view.location.as_ref();
            Form::new("Move session or change its cwd", "Review", vec![
                Field::choice("placement", "Placement", placement_choices(manager), &location.map(|l| placement_value(l.placement)).unwrap_or_default()),
                Field::text("cwd", "Working directory (empty: checkout root, or keep current cwd)", ""),
            ]).intro(&[
                &format!("Current cwd: {}", location.map(|l| l.cwd.display().to_string()).unwrap_or_default()),
                "Applies at an idle or safe boundary with one notice. Running tasks keep their cwd.",
            ])
        }
        FormKind::SessionAdopt(view) => Form::new("Adopt legacy session", "Review", vec![
            Field::choice("placement", "Placement", placement_choices(manager), ""),
            Field::text("cwd", "Working directory", view.legacy_working_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).required(),
        ]).intro(&[
            &format!("Recorded directory: {}", view.legacy_working_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "unknown".into())),
            "Appends one location notice. The original transcript and context stay unchanged.",
        ]),
        FormKind::NewContext(_) => Form::new("New context from this session", "Continue", vec![
            Field::choice("kind", "Kind", vec![
                Choice::new("split", "Split: continue history in a new session"),
                Choice::new("clear", "Clear: fresh context here"),
                Choice::new("transfer", "Transfer: compacted handoff session"),
            ], "split"),
        ]).intro(&["Next you choose whether this session's direct grants carry over."]),
        FormKind::NewContextCarry { kind, review } => {
            let mut intro = vec![format!("{kind:?}: {} direct grant(s) on {}.", review.direct_grants.len(), review.source)];
            for grant in &review.direct_grants {
                intro.push(format!("  {} → {}", grant.id, describe::target(manager, &grant.target)));
            }
            intro.push("Carried grants become new, independently revocable copies.".into());
            let mut form = Form::new("Grant carry", "Create", vec![
                Field::choice("carry", "Direct grants", vec![Choice::new("no", "Do not carry"), Choice::new("yes", "Carry as new copies")], "no"),
            ]);
            form.intro = intro;
            form
        }
        FormKind::InspectSession => Form::new("Inspect a session", "Inspect", vec![
            Field::text("session", "Session ID", "").required(),
        ]),
        FormKind::Grant(proposal) => Form::new(
            if proposal.is_some() { "Approve access proposal" } else { "New write grant" },
            "Review",
            audience_fields(manager),
        ).intro(&["Grants native writes only. Shell and external tools are not sandboxed by this policy."]),
        FormKind::BindImported(item) => {
            let mut form = Form::new("Bind imported grant", "Review", audience_fields(manager))
                .intro(&["Binds the imported definition to verified identities here. It stays disabled until activated."]);
            if let Audience::Session(session) = &item.grant.audience {
                form.set("session", session.clone());
            }
            let (kind, value) = describe::target_choice(manager, &item.grant.target);
            form.set("target_kind", kind);
            form.set("target", value);
            form
        }
        FormKind::CloseoutBegin { location, .. } => Form::new("Begin checkout closeout", "Review", vec![
            Field::text("preservation", "Preservation directory (empty: private local store)", ""),
            Field::toggle("conditional", "Don't ask for approval if there are no ambiguities", false),
            Field::toggle("full_archive", "Also keep a verified full archive", false),
        ]).intro(&[
            &format!("Location {location}. Begin authorizes preparation of this exact checkout only."),
            "With the checkbox on, an agent may complete removal only after resolving every finding with no information loss.",
        ]),
        FormKind::Disposition { entry, .. } => Form::new("Record disposition", "Record", vec![
            Field::choice("kind", "Disposition", vec![
                Choice::new("preserve", "Preserve to the closeout destination"),
                Choice::new("retain", "Retain in checkout (blocks removal)"),
                Choice::new("preserved", "Already preserved at a verified path"),
                Choice::new("redundant", "Regenerable or redundant"),
            ], "preserve"),
            Field::text("reason", "Reason", "").required().when("kind", &["retain", "redundant"]),
            Field::text("path", "Existing preserved copy (absolute)", "").required().when("kind", &["preserved"]),
        ]).intro(&[&format!("Entry {entry}.")]),
        FormKind::CloseoutRecovery { .. } => Form::new("Closeout recovery", "Review", vec![
            Field::choice("choice", "Recovery", vec![
                Choice::new("restart", "Restart preparation"),
                Choice::new("resume", "Resume journaled removal"),
                Choice::new("retain", "Unregister and retain files"),
            ], "restart"),
        ]),
        FormKind::Backup => Form::new("Back up the catalog", "Back up", vec![
            Field::text("name", "Snapshot name", "manual").required(),
        ]).intro(&["Metadata only. Checkouts, transcripts and preserved files are not included."]),
        FormKind::Export => Form::new("Export project definition", "Export", vec![
            Field::choice("project", "Project", choices(manager, &["project"]), &context_project(manager).map(|p| format!("project:{p}")).unwrap_or_default()),
            Field::text("name", "Export name", "").required(),
        ]).intro(&["Organization, bindings, grant definitions and closeout references. No credentials, transcripts or files."]),
        FormKind::Import => Form::new("Import project definition", "Review", vec![
            Field::text("path", "Export file (absolute)", "").required(),
            Field::choice("collisions", "Identity collisions", vec![Choice::new("reject", "Reject"), Choice::new("new", "Import as new identities")], "reject"),
            Field::text("remap", "Location remaps (LOCATION_ID=/abs/path, comma separated)", ""),
        ]).intro(&["Imported grants stay disabled until reviewed. No recorded filesystem effect is replayed."]),
        FormKind::Shutdown(destination) => Form::new(
            if destination.is_stopped() { "Stop the runtime" } else { "Restart the runtime" },
            "Review",
            shutdown_fields(*destination),
        ),
        FormKind::ShutdownChange { destination, .. } => Form::new("Change shutdown options", "Review", shutdown_fields(*destination))
            .intro(&["Creates a new review. The current operation becomes Superseded only when you confirm."]),
    };
    manager.form = Some((kind, form));
}

fn csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build the typed request for a submitted form. Errors keep the draft open.
pub(crate) fn build(
    manager: &mut WorkspaceManager,
    kind: &FormKind,
    form: &Form,
) -> Result<(), String> {
    let text = |key: &str| form.value(key).trim().to_string();
    match kind {
        FormKind::CreateProject => review_change(
            manager,
            OrganizationChange::CreateProject { name: text("name") },
        ),
        FormKind::CreateRepository => review_change(
            manager,
            OrganizationChange::CreateRepository {
                name: text("name"),
                remotes: csv(form.value("remotes")),
            },
        ),
        FormKind::CreateArea => {
            let project = parse_id(form.value("project"), "project").ok_or("Choose a project")?;
            review_change(
                manager,
                OrganizationChange::CreateWorkArea {
                    project,
                    name: text("name"),
                },
            );
        }
        FormKind::Register => {
            let path = absolute(&text("path"), "Path")?;
            let registration = match form.value("kind") {
                "checkout" => Registration::Checkout {
                    home: parse_home(form.value("home"))?,
                    repository: parse_id(form.value("repository"), "repository")
                        .ok_or("Choose a repository")?,
                },
                "directory" => Registration::Directory {
                    home: parse_home(form.value("home"))?,
                },
                _ => Registration::Standalone,
            };
            review_change(
                manager,
                OrganizationChange::RegisterLocation {
                    name: text("name"),
                    path,
                    registration,
                },
            );
        }
        FormKind::Associate => {
            let project = parse_id(form.value("project"), "project").ok_or("Choose a project")?;
            let repository =
                parse_id(form.value("repository"), "repository").ok_or("Choose a repository")?;
            review_change(
                manager,
                if form.value("mode") == "remove" {
                    OrganizationChange::RemoveRepositoryAssociation {
                        project,
                        repository,
                    }
                } else {
                    OrganizationChange::AssociateRepository {
                        project,
                        repository,
                    }
                },
            );
        }
        FormKind::Rename(target) => review_change(
            manager,
            OrganizationChange::Rename {
                target: *target,
                name: text("name"),
            },
        ),
        FormKind::Move(location) => review_change(
            manager,
            OrganizationChange::MoveLocation {
                location: *location,
                home: parse_home(form.value("home"))?,
                associate_repository: form.flag("associate"),
            },
        ),
        FormKind::AdoptStandalone(location) => review_change(
            manager,
            OrganizationChange::AdoptStandalone {
                location: *location,
                home: parse_home(form.value("home"))?,
                repository: parse_id(form.value("repository"), "repository"),
                associate_repository: form.flag("associate"),
            },
        ),
        FormKind::Rebind {
            location,
            old_path,
            generation,
        } => review_change(
            manager,
            OrganizationChange::RebindLocation {
                location: *location,
                expected_old_path: old_path.clone(),
                expected_generation: *generation,
                new_path: absolute(&text("new_path"), "New path")?,
            },
        ),
        FormKind::VolumeDefault => {
            let volume_uuid = text("volume");
            if volume_uuid.is_empty() {
                return Err("Choose a mounted volume".into());
            }
            review_change(
                manager,
                OrganizationChange::SetVolumeDefault {
                    volume_uuid,
                    path: absolute(&text("path"), "Path")?,
                },
            );
        }
        FormKind::Clone => {
            let home = parse_home(form.value("home"))?;
            let repository =
                parse_id(form.value("repository"), "repository").ok_or("Choose a repository")?;
            let volume_uuid = text("volume");
            if volume_uuid.is_empty() {
                return Err("Choose a mounted volume".into());
            }
            let source = if form.value("source") == "local" {
                CloneSource::Local {
                    path: absolute(&text("local"), "Local checkout path")?,
                }
            } else {
                CloneSource::Remote { url: text("url") }
            };
            let base = match form.value("base_kind") {
                "tag" => CloneBase::Tag { name: text("base") },
                "commit" => CloneBase::Commit { oid: text("base") },
                _ => CloneBase::Branch { name: text("base") },
            };
            let branch = match form.value("branch") {
                "create" => CloneBranch::Create {
                    name: text("new_branch"),
                },
                "detached" => CloneBranch::Detached,
                _ => CloneBranch::KeepName,
            };
            let mut remotes = Vec::new();
            for item in csv(form.value("remotes")) {
                let (name, url) = item
                    .split_once('=')
                    .ok_or_else(|| format!("Remote `{item}` must be name=url"))?;
                remotes.push(CloneRemote {
                    name: name.trim().into(),
                    url: url.trim().into(),
                });
            }
            let destination = if form.value("destination") == "custom" {
                CloneDestination::Custom {
                    volume_uuid,
                    path: absolute(&text("custom"), "Custom path")?,
                }
            } else {
                CloneDestination::Default {
                    volume_uuid,
                    project_component: text("project_component"),
                    checkout_component: text("name"),
                }
            };
            let expected_revision = revision(manager);
            manager.queue(Op::read(
                View::Review,
                WorkspaceRequest::ReviewClone {
                    expected_revision,
                    spec: CloneSpec {
                        home,
                        repository,
                        name: text("name"),
                        source,
                        base,
                        branch,
                        remotes,
                        destination,
                        submodules: form.flag("submodules"),
                        lfs: form.flag("lfs"),
                        trusted_submodule_urls: Vec::new(),
                        trusted_lfs_urls: Vec::new(),
                    },
                },
            ));
        }
        FormKind::StartupCopy(target) => {
            let source = absolute(&text("source"), "Source root")?;
            manager.queue(Op::read(
                View::StartupPlans {
                    source: source.clone(),
                    target: *target,
                },
                WorkspaceRequest::StartupCopyPlans {
                    source,
                    target: *target,
                },
            ));
        }
        FormKind::Launch => {
            let placement = if form.value("placement") == "new-standalone" {
                PrimaryPlacement::Standalone {
                    root: absolute(&text("root"), "Standalone root")?,
                }
            } else {
                PrimaryPlacement::Existing {
                    placement: parse_placement(form.value("placement"))?,
                }
            };
            let path = absolute(&text("cwd"), "Working directory")?;
            let cwd = if form.value("cwd_mode") == "create" {
                let home = match &placement {
                    PrimaryPlacement::Existing {
                        placement: Placement::Project(id),
                    } => Some(Home::Project(*id)),
                    PrimaryPlacement::Existing {
                        placement: Placement::WorkArea(id),
                    } => Some(Home::WorkArea(*id)),
                    _ => None,
                };
                PrimaryCwd::CreateEmpty { path, home }
            } else {
                PrimaryCwd::Existing { path }
            };
            let model = if form.flag("model_set") {
                if text("model").is_empty() {
                    return Err("An explicit route needs a model".into());
                }
                Some(PrimaryModel {
                    model: text("model"),
                    provider: text("provider"),
                    api_method: text("api_method"),
                    effort: Some(text("effort")).filter(|e| !e.is_empty()),
                })
            } else {
                None
            };
            let request = PrimaryLaunchRequest {
                request: new_request(),
                expected_revision: revision(manager),
                input: PrimaryLaunchInput {
                    placement,
                    cwd: Some(cwd),
                    agent: Some(text("agent")).filter(|a| !a.is_empty()),
                    model,
                    selfdev: form.flag("selfdev"),
                },
            };
            manager.confirm = Some(confirm(
                "Launch primary session",
                describe::launch_lines(manager, &request),
                Op::Launch(request),
            ));
        }
        FormKind::SessionMove(view) => {
            let location = view
                .location
                .as_ref()
                .ok_or("This session has no managed location")?;
            let placement = parse_placement(form.value("placement"))?;
            let cwd = if text("cwd").is_empty() {
                // A concrete root proposes itself; a project or work area, or
                // the current placement, keeps the current cwd.
                location_root(manager, placement)
                    .filter(|_| location.placement != placement)
                    .unwrap_or_else(|| location.cwd.clone())
            } else {
                absolute(&text("cwd"), "Working directory")?
            };
            let request = LocationChangeRequest {
                request: new_request(),
                session: view.session.clone(),
                expected_session_revision: location.revision,
                expected_catalog_revision: view
                    .catalog_revision
                    .ok_or("Catalog revision unavailable; refresh first")?,
                placement,
                cwd: cwd.clone(),
            };
            manager.confirm = Some(confirm("Change session location", vec![
                (Tone::Normal, format!("Session {}", view.session)),
                (Tone::Normal, format!("Placement: {} → {}", describe::placement(manager, location.placement), describe::placement(manager, placement))),
                (Tone::Normal, format!("Working directory: {} → {}", location.cwd.display(), cwd.display())),
                (Tone::Muted, "One notice is appended at the next safe boundary. Idle sessions do not start a model turn.".into()),
            ], Op::location(View::Effect, PrimaryLocationCommand::Change { request })));
        }
        FormKind::SessionAdopt(view) => {
            let placement = parse_placement(form.value("placement"))?;
            let cwd = absolute(&text("cwd"), "Working directory")?;
            let request = LegacyLocationAdoptionRequest {
                request: new_request(),
                session: view.session.clone(),
                expected_working_dir: view.legacy_working_dir.clone(),
                expected_catalog_revision: view
                    .catalog_revision
                    .ok_or("Catalog revision unavailable; refresh first")?,
                placement,
                cwd: cwd.clone(),
            };
            manager.confirm = Some(confirm(
                "Adopt legacy session",
                vec![
                    (Tone::Normal, format!("Session {}", view.session)),
                    (
                        Tone::Normal,
                        format!("Placement: {}", describe::placement(manager, placement)),
                    ),
                    (
                        Tone::Normal,
                        format!("Working directory: {}", cwd.display()),
                    ),
                    (
                        Tone::Muted,
                        "Appends one location notice; earlier history is unchanged.".into(),
                    ),
                ],
                Op::location(
                    View::Effect,
                    PrimaryLocationCommand::AdoptLegacy { request },
                ),
            ));
        }
        FormKind::NewContext(session) => {
            let kind = match form.value("kind") {
                "clear" => NewContextKind::Clear,
                "transfer" => NewContextKind::Transfer,
                _ => NewContextKind::Split,
            };
            manager.queue(Op::permission(
                View::CarryReview(kind),
                PermissionRequest::ReviewCarry {
                    session: session.clone(),
                },
            ));
        }
        FormKind::NewContextCarry { kind, review } => {
            manager.intents.push_back(Intent::NewContext {
                kind: *kind,
                choice: GrantCarryChoice {
                    review: review.id,
                    carry: form.value("carry") == "yes",
                },
            });
            manager.note(
                Tone::Accent,
                format!("Creating {kind:?} context with the reviewed carry choice."),
            );
        }
        FormKind::InspectSession => {
            let session = text("session");
            if session.chars().any(char::is_control) {
                return Err("Invalid session ID".into());
            }
            if !manager.sessions.extra.contains(&session) {
                manager.sessions.extra.push(session.clone());
            }
            manager.sessions.selected = Some(session.clone());
            manager.inspect_session(session);
        }
        FormKind::Grant(proposal) => review_grant(
            manager,
            GrantChange::Issue {
                audience: parse_audience(form)?,
                target: parse_target(form)?,
                proposal: *proposal,
            },
        ),
        FormKind::BindImported(item) => review_grant(
            manager,
            GrantChange::BindImported {
                reference: item.reference,
                installation: item.installation,
                grant: item.grant.id,
                audience: parse_audience(form)?,
                target: parse_target(form)?,
            },
        ),
        FormKind::CloseoutBegin {
            location,
            generation,
        } => {
            let preservation = text("preservation");
            let spec = CloseoutSpec {
                location: *location,
                expected_generation: *generation,
                preservation_directory: if preservation.is_empty() {
                    None
                } else {
                    Some(absolute(&preservation, "Preservation directory")?)
                },
                conditional_no_loss: form.flag("conditional"),
                full_archive: form.flag("full_archive"),
            };
            let mut lines = vec![
                (
                    Tone::Normal,
                    format!("Checkout {}", describe::location_name(manager, *location)),
                ),
                (
                    Tone::Normal,
                    format!(
                        "Preservation: {}",
                        spec.preservation_directory
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "private local store".into())
                    ),
                ),
                (
                    if spec.conditional_no_loss {
                        Tone::Warn
                    } else {
                        Tone::Muted
                    },
                    format!(
                        "Conditional no-loss authorization: {}",
                        if spec.conditional_no_loss {
                            "ON — an agent may remove after resolving every finding"
                        } else {
                            "off — your final approval is required"
                        }
                    ),
                ),
                (
                    Tone::Muted,
                    format!(
                        "Full archive: {}",
                        if spec.full_archive { "yes" } else { "no" }
                    ),
                ),
                (
                    Tone::Muted,
                    "Begin does not remove anything. The checkout becomes Closing for new work."
                        .into(),
                ),
            ];
            if spec.conditional_no_loss {
                lines.push((
                    Tone::Muted,
                    "You can revoke this closeout before removal.".into(),
                ));
            }
            manager.confirm = Some(confirm(
                "Begin closeout",
                lines,
                Op::closeout(
                    View::Effect,
                    CloseoutRequest::Begin {
                        request: new_request(),
                        expected_revision: revision(manager),
                        spec,
                    },
                ),
            ));
        }
        FormKind::Disposition {
            operation,
            revision,
            entry,
        } => {
            let disposition = match form.value("kind") {
                "retain" => CloseoutDisposition::Retain {
                    reason: text("reason"),
                },
                "preserved" => CloseoutDisposition::Preserved {
                    path: absolute(&text("path"), "Preserved copy")?,
                },
                "redundant" => CloseoutDisposition::Redundant {
                    reason: text("reason"),
                },
                _ => CloseoutDisposition::Preserve,
            };
            manager.queue(Op::closeout(
                View::Effect,
                CloseoutRequest::Execute {
                    request: new_request(),
                    spec: CloseoutActionSpec {
                        operation: *operation,
                        expected_revision: *revision,
                        action: CloseoutAction::Disposition {
                            decision: CloseoutDecision {
                                entry: entry.clone(),
                                disposition,
                                recorded_by: "human (workspace manager)".into(),
                            },
                        },
                    },
                },
            ));
        }
        FormKind::CloseoutRecovery {
            operation,
            revision,
        } => {
            let choice = match form.value("choice") {
                "resume" => CloseoutRecoveryAction::ResumeRemoval,
                "retain" => CloseoutRecoveryAction::UnregisterRetainFiles,
                _ => CloseoutRecoveryAction::RestartPreparation,
            };
            manager.queue(Op::closeout(
                View::Effect,
                CloseoutRequest::Execute {
                    request: new_request(),
                    spec: CloseoutActionSpec {
                        operation: *operation,
                        expected_revision: *revision,
                        action: CloseoutAction::ReviewRecovery { choice },
                    },
                },
            ));
        }
        FormKind::Backup => {
            let name = text("name");
            manager.confirm = Some(confirm(
                "Back up the catalog",
                vec![(Tone::Normal, format!("Snapshot name: {name}"))],
                Op::read(
                    View::Effect,
                    WorkspaceRequest::Backup {
                        request: new_request(),
                        name,
                    },
                ),
            ));
        }
        FormKind::Export => {
            let project = parse_id(form.value("project"), "project").ok_or("Choose a project")?;
            let name = text("name");
            manager.confirm = Some(confirm(
                "Export project definition",
                vec![
                    (
                        Tone::Normal,
                        format!("Project {}", describe::project_name(manager, project)),
                    ),
                    (Tone::Normal, format!("Export name: {name}")),
                ],
                Op::read(
                    View::Effect,
                    WorkspaceRequest::Export {
                        request: new_request(),
                        project,
                        name,
                    },
                ),
            ));
        }
        FormKind::Import => {
            let mut remap = Vec::new();
            for item in csv(form.value("remap")) {
                let (location, path) = item
                    .split_once('=')
                    .ok_or_else(|| format!("Remap `{item}` must be LOCATION_ID=/path"))?;
                remap.push(LocationRemap {
                    location: location
                        .trim()
                        .parse()
                        .map_err(|_| format!("`{location}` is not a location ID"))?,
                    path: absolute(path.trim(), "Remap path")?,
                });
            }
            manager.queue(Op::read(
                View::Review,
                WorkspaceRequest::ReviewImport {
                    path: absolute(&text("path"), "Export file")?,
                    expected_revision: revision(manager),
                    collisions: if form.value("collisions") == "new" {
                        ImportCollisionPolicy::NewIdentities
                    } else {
                        ImportCollisionPolicy::Reject
                    },
                    remap,
                },
            ));
        }
        FormKind::Shutdown(destination) => {
            let options = shutdown_options(form, *destination)?;
            manager.queue(Op::runtime(
                View::Review,
                RuntimeRequest::Review { options },
            ));
        }
        FormKind::ShutdownChange {
            operation,
            revision,
            destination,
        } => {
            let options = shutdown_options(form, *destination)?;
            manager.queue(Op::runtime(
                View::Review,
                RuntimeRequest::ReviewChange {
                    operation: *operation,
                    expected_revision: *revision,
                    options,
                },
            ));
        }
    }
    Ok(())
}
