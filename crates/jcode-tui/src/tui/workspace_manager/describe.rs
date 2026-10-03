//! Textual projections of typed records for lists, details and reviews.
//! Presentation only: no request is sent from here.
use super::transport::View;
use super::*;
use crate::workspace::runtime::{
    RecoveryItem, RecoveryResolution, RecoveryResolved, RuntimeRequest, ShutdownPhase,
    ShutdownReview,
};

pub(crate) fn title(kind: &str) -> String {
    let mut chars = kind.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn short(id: impl ToString) -> String {
    id.to_string().chars().take(8).collect()
}

pub(crate) fn entity_name(entity: &Entity) -> String {
    match entity {
        Entity::Project(p) => p.name.clone(),
        Entity::Repository(r) => r.name.clone(),
        Entity::WorkArea(a) => a.name.clone(),
        Entity::Location(l) => l.name.clone(),
    }
}

pub(crate) fn project_name(manager: &WorkspaceManager, id: ProjectId) -> String {
    match manager.known.get(&id.to_string()) {
        Some(Entity::Project(project)) => project.name.clone(),
        _ => format!("project {}", short(id)),
    }
}

fn name_of(manager: &WorkspaceManager, id: impl ToString) -> String {
    let id = id.to_string();
    manager
        .known
        .get(&id)
        .map(entity_name)
        .unwrap_or_else(|| short(&id))
}

pub(crate) fn location_name(manager: &WorkspaceManager, id: LocationId) -> String {
    match manager.known.get(&id.to_string()) {
        Some(Entity::Location(location)) => {
            format!("{} ({})", location.name, location.observed_path.display())
        }
        _ => format!("location {}", short(id)),
    }
}

pub(crate) fn home(manager: &WorkspaceManager, home: Home) -> String {
    match home {
        Home::Project(id) => format!("project {}", project_name(manager, id)),
        Home::WorkArea(id) => match manager.known.get(&id.to_string()) {
            Some(Entity::WorkArea(area)) => {
                format!("area {}/{}", project_name(manager, area.project), area.name)
            }
            _ => format!("area {}", short(id)),
        },
    }
}

pub(crate) fn home_project_name(manager: &WorkspaceManager, value: Option<Home>) -> String {
    let project = match value {
        Some(Home::Project(id)) => Some(id),
        Some(Home::WorkArea(id)) => match manager.known.get(&id.to_string()) {
            Some(Entity::WorkArea(area)) => Some(area.project),
            _ => None,
        },
        None => None,
    };
    project
        .map(|id| project_name(manager, id))
        .unwrap_or_default()
}

pub(crate) fn placement(manager: &WorkspaceManager, placement: Placement) -> String {
    match placement {
        Placement::Project(id) => format!("project {}", project_name(manager, id)),
        Placement::WorkArea(id) => home(manager, Home::WorkArea(id)),
        Placement::Checkout(id) => format!("checkout {}", location_name(manager, id)),
        Placement::Directory(id) => format!("directory {}", location_name(manager, id)),
        Placement::Standalone(id) => format!("standalone {}", location_name(manager, id)),
    }
}

pub(crate) fn target(manager: &WorkspaceManager, target: &WriteTarget) -> String {
    match target {
        WriteTarget::Root(id) => format!("root {}", location_name(manager, *id)),
        WriteTarget::ProjectMembers(id) => {
            format!("member roots of project {}", project_name(manager, *id))
        }
        WriteTarget::WorkAreaMembers(id) => {
            format!("member roots of {}", home(manager, Home::WorkArea(*id)))
        }
    }
}

pub(crate) fn audience(manager: &WorkspaceManager, audience: &Audience) -> String {
    match audience {
        Audience::Session(id) => format!("session {id}"),
        Audience::Project(id) => format!("sessions of project {}", project_name(manager, *id)),
        Audience::WorkArea(id) => format!("sessions of {}", home(manager, Home::WorkArea(*id))),
        Audience::Checkout(id) => format!("sessions of checkout {}", location_name(manager, *id)),
    }
}

/// Form choice (kind, value) for an existing write target.
/// The form value for a write target. A root uses its location's actual kind,
/// matching the choice list, so a prefilled directory shows as that directory.
pub(crate) fn target_choice(
    manager: &WorkspaceManager,
    target: &WriteTarget,
) -> (&'static str, String) {
    match target {
        WriteTarget::Root(id) => {
            let kind = match manager.known.get(&id.to_string()) {
                Some(Entity::Location(location)) => match location.kind {
                    LocationKind::Directory => "directory",
                    LocationKind::Standalone { .. } => "standalone",
                    LocationKind::Checkout { .. } => "checkout",
                },
                _ => "checkout",
            };
            ("root", format!("{kind}:{id}"))
        }
        WriteTarget::ProjectMembers(id) => ("project", format!("project:{id}")),
        WriteTarget::WorkAreaMembers(id) => ("area", format!("area:{id}")),
    }
}

pub(crate) fn permission_key(item: &PermissionItem) -> String {
    match item {
        PermissionItem::Grant(grant) => grant.id.to_string(),
        PermissionItem::Proposal(proposal) => proposal.id.to_string(),
    }
}

pub(crate) fn volume_choices(manager: &WorkspaceManager) -> Vec<form::Choice> {
    manager
        .volumes
        .iter()
        .flatten()
        .map(|volume| {
            form::Choice::new(
                volume.uuid.clone(),
                format!(
                    "{} · {} · {} · {:.1} GiB free{}",
                    volume.label,
                    volume.mount.display(),
                    if volume.internal {
                        "internal"
                    } else {
                        "external"
                    },
                    volume.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    if volume.writable { "" } else { " · READ-ONLY" }
                ),
            )
        })
        .collect()
}

pub(crate) fn closeout_action(action: &CloseoutAction) -> &'static str {
    match action {
        CloseoutAction::Refresh => "refresh inventory",
        CloseoutAction::Preserve => "preserve",
        CloseoutAction::Disposition { .. } => "record disposition",
        CloseoutAction::ReviewRemoval => "review removal",
        CloseoutAction::ApproveRemoval { .. } => "approve removal",
        CloseoutAction::Finish => "finish removal",
        CloseoutAction::ReviewRecovery { .. } => "review recovery",
        CloseoutAction::ApplyRecovery { .. } => "apply recovery",
        CloseoutAction::DeclareNoLoss { .. } => "agent no-loss declaration",
    }
}

pub(crate) fn phase_tone(phase: ShutdownPhase) -> Tone {
    match phase {
        ShutdownPhase::Stopped | ShutdownPhase::Cancelled => Tone::Good,
        ShutdownPhase::Blocked | ShutdownPhase::Forced | ShutdownPhase::Interrupted => Tone::Warn,
        _ => Tone::Accent,
    }
}

pub(crate) fn resolution(resolved: &RecoveryResolved) -> String {
    match &resolved.resolution {
        RecoveryResolution::Continued { input } => format!("continued by input {input}"),
        RecoveryResolution::LeftStopped {} => "left stopped".into(),
        RecoveryResolution::SupersededByInput { input } => {
            format!("superseded by a new message ({input})")
        }
    }
}

fn lifecycle_tone(lifecycle: LocationLifecycle) -> Tone {
    match lifecycle {
        LocationLifecycle::Ready => Tone::Normal,
        LocationLifecycle::Provisioning | LocationLifecycle::Closing => Tone::Accent,
        LocationLifecycle::PreparationFailed | LocationLifecycle::Unavailable => Tone::Warn,
        LocationLifecycle::Closed | LocationLifecycle::Unregistered => Tone::Muted,
    }
}

fn entity_row(manager: &WorkspaceManager, entity: &Entity) -> Row {
    let (tag, text, tone) = match entity {
        Entity::Project(p) => (
            "P",
            p.name.clone(),
            if p.state == OrganizationState::Active {
                Tone::Normal
            } else {
                Tone::Muted
            },
        ),
        Entity::Repository(r) => (
            "R",
            format!(
                "{}{}",
                r.name,
                r.remotes
                    .first()
                    .map(|remote| format!("  {remote}"))
                    .unwrap_or_default()
            ),
            if r.state == OrganizationState::Active {
                Tone::Normal
            } else {
                Tone::Muted
            },
        ),
        Entity::WorkArea(a) => (
            "A",
            format!("{}/{}", project_name(manager, a.project), a.name),
            if a.state == OrganizationState::Active {
                Tone::Normal
            } else {
                Tone::Muted
            },
        ),
        Entity::Location(l) => (
            match l.kind {
                LocationKind::Checkout { .. } => "C",
                LocationKind::Directory => "D",
                LocationKind::Standalone { .. } => "S",
            },
            format!(
                "{}  {:?}  {}",
                l.name,
                l.lifecycle,
                l.observed_path.display()
            ),
            if l.retired {
                Tone::Muted
            } else {
                lifecycle_tone(l.lifecycle)
            },
        ),
    };
    Row {
        key: entity.id().to_string(),
        text: format!("{tag}  {text}"),
        tone,
    }
}

fn operation_summary(manager: &WorkspaceManager, entry: &OperationEntry) -> String {
    match &entry.operation {
        WorkspaceOperation::Clone(record) => format!(
            "Clone     {:?}  {} → {}",
            record.state,
            record.review.spec.name,
            record.review.destination.display()
        ),
        WorkspaceOperation::Closeout(record) => format!(
            "Closeout  {:?}  {}",
            record.stage,
            location_name(manager, record.spec.location)
        ),
        WorkspaceOperation::StartupCopy(record) => format!(
            "Copy      {:?}  Startup Context → {}",
            record.state,
            record.review.target_path.display()
        ),
        WorkspaceOperation::PrimaryLaunch(record) => {
            format!("Launch    {:?}  {}", record.state, record.session)
        }
        WorkspaceOperation::PrimaryLocation(record) => format!(
            "Move      {:?}  {} → {}",
            record.state,
            record.input.session,
            record.input.cwd.display()
        ),
    }
}

fn state_tone(state: OperationState) -> Tone {
    match state {
        OperationState::Pending => Tone::Accent,
        OperationState::Complete => Tone::Muted,
        OperationState::Failed | OperationState::RecoveryRequired => Tone::Warn,
    }
}

/// Inventory paths are relative to the checkout; its root has an empty path.
fn entry_path(path: &std::path::Path) -> String {
    if path.as_os_str().is_empty() {
        "(checkout root)".into()
    } else {
        path.display().to_string()
    }
}

pub(super) fn rows(manager: &WorkspaceManager) -> Vec<Row> {
    match manager.section {
        Section::Organization => {
            // Pages follow the owner's stable ID order. Within a page, group by
            // kind and name so related rows read together; selection stays by ID.
            let mut rows: Vec<(u8, Row)> = manager
                .org
                .page
                .iter()
                .flat_map(|page| &page.items)
                .map(|entity| {
                    let current = manager
                        .known
                        .get(&entity.id().to_string())
                        .unwrap_or(entity);
                    let rank = match current {
                        Entity::Project(_) => 0,
                        Entity::WorkArea(_) => 1,
                        Entity::Repository(_) => 2,
                        Entity::Location(_) => 3,
                    };
                    (rank, entity_row(manager, current))
                })
                .collect();
            rows.sort_by(|(a, x), (b, y)| {
                a.cmp(b)
                    .then_with(|| x.text.to_lowercase().cmp(&y.text.to_lowercase()))
                    .then_with(|| x.key.cmp(&y.key))
            });
            rows.into_iter().map(|(_, row)| row).collect()
        }
        Section::Sessions => {
            let mut rows = vec![Row {
                key: manager.session.clone(),
                text: format!("● {}  (this client)", manager.session),
                tone: Tone::Accent,
            }];
            for extra in &manager.sessions.extra {
                if *extra != manager.session {
                    rows.push(Row {
                        key: extra.clone(),
                        text: format!("  {extra}  (inspected)"),
                        tone: Tone::Normal,
                    });
                }
            }
            for index in &manager.sessions.rows {
                if rows.iter().any(|row| row.key == index.session) {
                    continue;
                }
                rows.push(Row {
                    key: index.session.clone(),
                    text: format!(
                        "  {}  {}{}",
                        index.session,
                        placement(manager, index.placement),
                        if index.reconciled {
                            ""
                        } else {
                            "  (index pending)"
                        }
                    ),
                    tone: if index.active {
                        Tone::Normal
                    } else {
                        Tone::Muted
                    },
                });
            }
            rows
        }
        Section::Operations => manager
            .ops
            .page
            .iter()
            .flat_map(|page| &page.items)
            .map(|entry| Row {
                key: entry.operation.operation().to_string(),
                text: operation_summary(manager, entry),
                tone: state_tone(entry.state),
            })
            .collect(),
        Section::Permissions => {
            if let Some((_, items, _)) = &manager.perms.imported {
                return items
                    .iter()
                    .map(|item| Row {
                        key: item.grant.id.to_string(),
                        text: format!(
                            "{}  {} → {}",
                            if item.bound_grant.is_some() {
                                "Bound  "
                            } else {
                                "Unbound"
                            },
                            audience(manager, &item.grant.audience),
                            target(manager, &item.grant.target)
                        ),
                        tone: if item.bound_grant.is_some() {
                            Tone::Muted
                        } else {
                            Tone::Accent
                        },
                    })
                    .collect();
            }
            manager
                .perms
                .page
                .iter()
                .flat_map(|page| &page.items)
                .map(|item| match item {
                    PermissionItem::Proposal(p) => Row {
                        key: p.id.to_string(),
                        text: format!(
                            "{:<8} {} wants {}",
                            format!("{:?}", p.state),
                            p.session,
                            target(manager, &p.target)
                        ),
                        tone: if p.state == AccessProposalState::Pending {
                            Tone::Accent
                        } else {
                            Tone::Muted
                        },
                    },
                    PermissionItem::Grant(g) => Row {
                        key: g.id.to_string(),
                        text: format!(
                            "{:<8} {} → {}",
                            format!("{:?}", g.state),
                            audience(manager, &g.audience),
                            target(manager, &g.target)
                        ),
                        tone: match g.state {
                            GrantState::Active => Tone::Good,
                            GrantState::Disabled => Tone::Warn,
                            GrantState::Revoked => Tone::Muted,
                        },
                    },
                })
                .collect()
        }
        Section::Closeout => manager
            .closeouts
            .page
            .iter()
            .flat_map(|page| &page.items)
            .filter_map(|entry| match &entry.operation {
                WorkspaceOperation::Closeout(record) => Some(Row {
                    key: record.operation.to_string(),
                    text: format!(
                        "{:<16} {}",
                        format!("{:?}", record.stage),
                        location_name(manager, record.spec.location)
                    ),
                    tone: state_tone(entry.state),
                }),
                _ => None,
            })
            .collect(),
        Section::Backup => manager
            .backups
            .snapshots
            .iter()
            .flatten()
            .map(|snapshot| Row {
                key: snapshot.id.to_string(),
                text: format!(
                    "{}  {}  revision {}",
                    if snapshot.automatic {
                        "auto  "
                    } else {
                        "manual"
                    },
                    snapshot.name,
                    snapshot.revision
                ),
                tone: if snapshot.automatic {
                    Tone::Muted
                } else {
                    Tone::Normal
                },
            })
            .collect(),
        Section::Runtime => manager
            .runtime
            .supervision
            .iter()
            .flat_map(|supervision| &supervision.recoveries)
            .map(|item| Row {
                key: item.id.to_string(),
                text: format!(
                    "{}  {}  {:?}",
                    if item.resolved.is_none() {
                        "Needs decision"
                    } else {
                        "Resolved      "
                    },
                    item.session,
                    item.cause
                ),
                tone: if item.resolved.is_none() {
                    Tone::Warn
                } else {
                    Tone::Muted
                },
            })
            .collect(),
    }
}

fn push(lines: &mut Vec<(Tone, String)>, tone: Tone, text: impl Into<String>) {
    lines.push((tone, text.into()));
}

fn issues(lines: &mut Vec<(Tone, String)>, issues: &[Issue]) {
    for issue in issues {
        push(
            lines,
            Tone::Warn,
            format!("! {:?}: {}", issue.code, issue.detail),
        );
    }
}

fn catalog_lines(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) -> bool {
    match &manager.catalog {
        None if manager.remote && manager.connected => {
            push(lines, Tone::Muted, "Reading catalog status…");
            false
        }
        Some(Err(issue)) => {
            if issue.is_not_initialized() {
                push(
                    lines,
                    Tone::Warn,
                    "The workspace catalog is not initialized.",
                );
                push(
                    lines,
                    Tone::Muted,
                    "Initializing creates the private catalog only. Press I to review.",
                );
            } else if let Some(request) = issue.interrupted_initialization() {
                push(
                    lines,
                    Tone::Warn,
                    format!("Catalog initialization {request} stopped before it was ready."),
                );
                push(lines, Tone::Muted, "Press I to resume that same request.");
            } else {
                push(
                    lines,
                    Tone::Bad,
                    format!("Catalog unavailable: {:?} — {}", issue.code, issue.detail),
                );
            }
            false
        }
        Some(Ok(_)) => true,
        None => false,
    }
}

pub(super) fn detail(manager: &WorkspaceManager) -> Vec<(Tone, String)> {
    let mut lines = Vec::new();
    if !manager.remote {
        push(
            &mut lines,
            Tone::Warn,
            "Workspace management needs the shared runtime.",
        );
        push(
            &mut lines,
            Tone::Muted,
            "This client runs a private local session. Start jcode normally (attached to the server) to manage projects, checkouts, permissions and the runtime.",
        );
        return lines;
    }
    match manager.section {
        Section::Organization => {
            if !catalog_lines(manager, &mut lines) {
                return lines;
            }
            let Some(entity) = manager
                .org
                .selected
                .and_then(|id| manager.known.get(&id.to_string()))
            else {
                push(
                    &mut lines,
                    Tone::Muted,
                    "No entry selected. Create a project (n) or register a path (g).",
                );
                return lines;
            };
            entity_detail(manager, entity, &mut lines);
            if let Some((target, sessions)) = &manager.org.sessions
                && *target == entity.id()
            {
                push(&mut lines, Tone::Normal, "");
                push(
                    &mut lines,
                    Tone::Accent,
                    format!("Sessions here ({})", sessions.len()),
                );
                for session in sessions {
                    push(
                        &mut lines,
                        if session.active {
                            Tone::Normal
                        } else {
                            Tone::Muted
                        },
                        format!(
                            "  {}  {}",
                            session.session,
                            placement(manager, session.placement)
                        ),
                    );
                }
            }
        }
        Section::Sessions => session_detail(manager, &mut lines),
        Section::Operations => {
            if !catalog_lines(manager, &mut lines) {
                return lines;
            }
            operation_detail(manager, &mut lines);
        }
        Section::Permissions => {
            if !catalog_lines(manager, &mut lines) {
                return lines;
            }
            permission_detail(manager, &mut lines);
        }
        Section::Closeout => {
            if !catalog_lines(manager, &mut lines) {
                return lines;
            }
            closeout_detail(manager, &mut lines);
        }
        Section::Backup => {
            if !catalog_lines(manager, &mut lines) {
                return lines;
            }
            if let Some(snapshot) = manager.backups.selected.and_then(|id| {
                manager
                    .backups
                    .snapshots
                    .iter()
                    .flatten()
                    .find(|snapshot| snapshot.id == id)
            }) {
                push(&mut lines, Tone::Accent, snapshot.name.clone());
                push(
                    &mut lines,
                    Tone::Normal,
                    format!("Path: {}", snapshot.path.display()),
                );
                push(
                    &mut lines,
                    Tone::Normal,
                    format!("Catalog revision: {}", snapshot.revision),
                );
                push(
                    &mut lines,
                    Tone::Muted,
                    format!("SHA-256: {}", snapshot.sha256),
                );
                push(
                    &mut lines,
                    Tone::Muted,
                    if snapshot.automatic {
                        "Automatic rolling snapshot"
                    } else {
                        "Named snapshot, kept until you remove it"
                    },
                );
            } else {
                push(&mut lines, Tone::Muted, "No snapshot selected.");
            }
            push(&mut lines, Tone::Normal, "");
            push(
                &mut lines,
                Tone::Muted,
                "Catalog backups hold organization metadata only, not checkouts, transcripts or preserved files.",
            );
        }
        Section::Runtime => runtime_detail(manager, &mut lines),
    }
    lines
}

fn entity_detail(manager: &WorkspaceManager, entity: &Entity, lines: &mut Vec<(Tone, String)>) {
    push(lines, Tone::Accent, entity_name(entity));
    push(lines, Tone::Muted, format!("ID {}", entity.id()));
    match entity {
        Entity::Project(p) => {
            push(
                lines,
                Tone::Normal,
                format!("Project · {:?} · revision {}", p.state, p.revision),
            );
        }
        Entity::Repository(r) => {
            push(
                lines,
                Tone::Normal,
                format!(
                    "Logical repository · {:?} · revision {}",
                    r.state, r.revision
                ),
            );
            for remote in &r.remotes {
                push(lines, Tone::Normal, format!("Remote reference: {remote}"));
            }
        }
        Entity::WorkArea(a) => {
            push(
                lines,
                Tone::Normal,
                format!(
                    "Work area of project {} · {:?} · revision {}",
                    project_name(manager, a.project),
                    a.state,
                    a.revision
                ),
            );
        }
        Entity::Location(l) => {
            let kind = match &l.kind {
                LocationKind::Checkout {
                    repository, origin, ..
                } => {
                    format!("Checkout of {} ({origin:?})", name_of(manager, repository))
                }
                LocationKind::Directory => "Directory reference".into(),
                LocationKind::Standalone { git } => {
                    format!("Standalone {}", if *git { "Git root" } else { "directory" })
                }
            };
            push(lines, Tone::Normal, kind);
            push(
                lines,
                Tone::Normal,
                format!("Path: {}", l.observed_path.display()),
            );
            push(
                lines,
                Tone::Normal,
                format!(
                    "Home: {}",
                    l.home
                        .map(|h| home(manager, h))
                        .unwrap_or_else(|| "none (standalone)".into())
                ),
            );
            push(
                lines,
                lifecycle_tone(l.lifecycle),
                format!(
                    "Lifecycle: {:?}{}",
                    l.lifecycle,
                    if l.retired { " · retired" } else { "" }
                ),
            );
            push(
                lines,
                Tone::Muted,
                format!(
                    "Volume {} · binding generation {} · revision {}",
                    l.volume_uuid, l.binding_generation, l.revision
                ),
            );
            if let LocationKind::Checkout {
                common_directory, ..
            } = &l.kind
            {
                push(
                    lines,
                    Tone::Muted,
                    format!("Git common directory: {}", common_directory.display()),
                );
            }
        }
    }
}

fn session_detail(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) {
    let Some(key) = manager.selected_key() else {
        return;
    };
    push(lines, Tone::Accent, key.clone());
    if !manager.caps.session_inspection {
        push(
            lines,
            Tone::Muted,
            match manager.caps.primary {
                None => "Negotiating session controls…",
                _ => {
                    "This runtime does not offer session location inspection. Upgrade it to manage moves here."
                }
            },
        );
        return;
    }
    let Some(view) = manager.sessions.views.get(&key) else {
        push(lines, Tone::Muted, "Reading session location…");
        return;
    };
    if view.isolated_child {
        push(
            lines,
            Tone::Muted,
            "Isolated child session. Its location is owned by its original parent.",
        );
    }
    match &view.location {
        Some(location) => {
            push(
                lines,
                Tone::Normal,
                format!("Placement: {}", placement(manager, location.placement)),
            );
            push(
                lines,
                Tone::Normal,
                format!("Working directory: {}", location.cwd.display()),
            );
            push(
                lines,
                Tone::Muted,
                format!("Initial directory: {}", location.initial_cwd.display()),
            );
            push(
                lines,
                Tone::Muted,
                format!("Session location revision {}", location.revision),
            );
        }
        None => {
            push(
                lines,
                Tone::Warn,
                "Legacy session: no managed placement yet.",
            );
            push(
                lines,
                Tone::Normal,
                format!(
                    "Recorded directory: {}",
                    view.legacy_working_dir
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "unknown".into())
                ),
            );
            push(
                lines,
                Tone::Muted,
                "It stays readable. Adopt it (a) to choose its placement and cwd explicitly.",
            );
        }
    }
    if let Some(issue) = &view.catalog_issue {
        push(
            lines,
            Tone::Warn,
            format!("Catalog facts unavailable: {}", issue.detail),
        );
    }
    for change in &view.pending {
        push(
            lines,
            Tone::Accent,
            format!(
                "Pending move {}: {} → {} ({:?})",
                short(change.operation),
                placement(manager, change.input.placement),
                change.input.cwd.display(),
                change.state
            ),
        );
    }
    if !manager.caps.location_enabled {
        push(
            lines,
            Tone::Muted,
            "Moves, adoption and scoped new contexts are staged on this runtime.",
        );
    }
    match &manager.sessions.scope {
        Some(Ok(scope)) if scope.session == key => {
            push(lines, Tone::Normal, "");
            push(
                lines,
                Tone::Accent,
                format!("Write scope at catalog revision {}", scope.catalog_revision),
            );
            for root in &scope.roots {
                push(
                    lines,
                    if root.issue.is_some() {
                        Tone::Warn
                    } else {
                        Tone::Normal
                    },
                    format!(
                        "  {} {}{}",
                        if root.ordinary {
                            "ordinary"
                        } else {
                            "granted "
                        },
                        root.location.observed_path.display(),
                        root.issue
                            .as_ref()
                            .map(|issue| format!("  ({})", issue.detail))
                            .unwrap_or_default()
                    ),
                );
            }
            for grant in &scope.grants {
                push(
                    lines,
                    Tone::Muted,
                    format!(
                        "  grant {} {:?}: {}",
                        short(grant.id),
                        grant.state,
                        target(manager, &grant.target)
                    ),
                );
            }
            push(
                lines,
                Tone::Muted,
                "Native file tools enforce this scope. Shell and external tools are not sandboxed.",
            );
        }
        Some(Err(issue)) => push(lines, Tone::Warn, format!("Write scope: {}", issue.detail)),
        _ => {}
    }
    for scope in manager
        .perms
        .scopes
        .iter()
        .flatten()
        .filter(|scope| scope.source == key || scope.target == key)
    {
        push(
            lines,
            Tone::Muted,
            format!(
                "New-context scope {:?} {} → {}: {:?}{}",
                scope.kind,
                scope.source,
                scope.target,
                scope.state,
                if scope.backup_pending {
                    " · backup pending"
                } else {
                    ""
                }
            ),
        );
    }
}

fn operation_detail(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) {
    let Some(entry) = manager.ops.selected.and_then(|id| {
        manager
            .ops
            .page
            .as_ref()?
            .items
            .iter()
            .find(|entry| entry.operation.operation() == id)
    }) else {
        push(
            lines,
            Tone::Muted,
            if manager.ops.all {
                "No operations recorded."
            } else {
                "No unfinished operations. Press u to include finished ones."
            },
        );
        return;
    };
    push(
        lines,
        Tone::Accent,
        format!(
            "{:?} operation {}",
            entry.operation.kind(),
            entry.operation.operation()
        ),
    );
    push(
        lines,
        state_tone(entry.state),
        format!("Ledger state: {:?}", entry.state),
    );
    match &entry.operation {
        WorkspaceOperation::Clone(stored) => {
            let record = manager
                .ops
                .clone
                .as_ref()
                .filter(|fresh| fresh.request == stored.request)
                .unwrap_or(stored);
            let review = &record.review;
            push(
                lines,
                Tone::Normal,
                format!(
                    "State: {:?}{}",
                    record.state,
                    if record.cancel_requested {
                        " · cancel requested"
                    } else {
                        ""
                    }
                ),
            );
            push(
                lines,
                Tone::Normal,
                format!("{} → {}", review.spec.name, review.destination.display()),
            );
            push(
                lines,
                Tone::Muted,
                format!("Source commit {}", review.source_commit),
            );
            if let Some(stage) = &record.stage {
                push(lines, Tone::Muted, format!("Stage: {}", stage.display()));
            }
            for source in &record.pending_trust {
                push(
                    lines,
                    Tone::Warn,
                    format!(
                        "Awaiting trust: {:?} {} ({})",
                        source.kind,
                        source.url,
                        source.path.display()
                    ),
                );
            }
            if let Some(issue) = record.issue.as_ref().or(record.output_issue.as_ref()) {
                push(
                    lines,
                    Tone::Warn,
                    format!("! {:?}: {}", issue.code, issue.detail),
                );
            }
            push(
                lines,
                Tone::Muted,
                format!("Retained runs: {}", record.output_runs.len()),
            );
            if let Some((clone, output)) = &manager.ops.output
                && *clone == record.request
            {
                push(lines, Tone::Normal, "");
                push(lines, Tone::Accent, "Latest retained output");
                for line in output.lines() {
                    push(lines, Tone::Muted, line.to_string());
                }
            }
        }
        WorkspaceOperation::Closeout(record) => {
            push(
                lines,
                Tone::Normal,
                format!(
                    "Stage {:?} for {}",
                    record.stage,
                    location_name(manager, record.spec.location)
                ),
            );
            push(lines, Tone::Muted, "Manage it in the Closeout section (5).");
        }
        WorkspaceOperation::StartupCopy(record) => {
            push(
                lines,
                Tone::Normal,
                format!(
                    "State: {:?} → {}",
                    record.state,
                    record.review.target_path.display()
                ),
            );
            for entry in &record.review.entries {
                push(
                    lines,
                    Tone::Muted,
                    format!(
                        "  {} → {}",
                        entry.selected_path.display(),
                        entry.resolved_target.display()
                    ),
                );
            }
            if let Some(issue) = &record.issue {
                push(
                    lines,
                    Tone::Warn,
                    format!("! {:?}: {}", issue.code, issue.detail),
                );
            }
        }
        WorkspaceOperation::PrimaryLaunch(record) => {
            push(
                lines,
                Tone::Normal,
                format!("Session {} · {:?}", record.session, record.state),
            );
            push(
                lines,
                Tone::Muted,
                format!(
                    "Route {} / {} / {}",
                    record.concrete_model.provider,
                    record.concrete_model.model,
                    record.concrete_model.api_method
                ),
            );
            if let Some(issue) = &record.issue {
                push(lines, Tone::Warn, format!("! {issue}"));
            }
        }
        WorkspaceOperation::PrimaryLocation(record) => {
            push(
                lines,
                Tone::Normal,
                format!("Session {} · {:?}", record.input.session, record.state),
            );
            push(
                lines,
                Tone::Normal,
                format!(
                    "To {} at {}",
                    placement(manager, record.input.placement),
                    record.input.cwd.display()
                ),
            );
            if let Some(issue) = &record.issue {
                push(
                    lines,
                    Tone::Warn,
                    format!("! {:?}: {}", issue.code, issue.detail),
                );
            }
        }
    }
    let targets = entry
        .targets
        .iter()
        .map(|target| name_of(manager, target))
        .collect::<Vec<_>>();
    if !targets.is_empty() {
        push(
            lines,
            Tone::Muted,
            format!("Targets: {}", targets.join(", ")),
        );
    }
}

fn permission_detail(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) {
    push(
        lines,
        Tone::Muted,
        format!("Showing {:?}. Press v to switch.", manager.perms.mode),
    );
    let Some(key) = &manager.perms.selected else {
        push(lines, Tone::Muted, "Nothing selected.");
        return;
    };
    if let Some((_, items, _)) = &manager.perms.imported {
        if let Some(item) = items.iter().find(|item| item.grant.id.to_string() == *key) {
            push(
                lines,
                Tone::Accent,
                format!("Imported grant {}", item.grant.id),
            );
            push(
                lines,
                Tone::Normal,
                format!(
                    "{} → {}",
                    audience(manager, &item.grant.audience),
                    target(manager, &item.grant.target)
                ),
            );
            push(
                lines,
                Tone::Muted,
                format!(
                    "From installation {} · reference {}",
                    item.installation, item.reference
                ),
            );
            push(
                lines,
                Tone::Normal,
                match item.bound_grant {
                    Some(grant) => format!("Bound as grant {grant} (activate to enable)"),
                    None => "Not bound. Bind it to verified local identities (b).".into(),
                },
            );
        }
        return;
    }
    let item = manager
        .perms
        .page
        .iter()
        .flat_map(|page| &page.items)
        .find(|item| permission_key(item) == *key);
    match item {
        Some(PermissionItem::Proposal(p)) => {
            push(lines, Tone::Accent, format!("Access proposal {}", p.id));
            push(lines, Tone::Normal, format!("Session: {}", p.session));
            push(
                lines,
                Tone::Normal,
                format!("Requests: {}", target(manager, &p.target)),
            );
            push(lines, Tone::Normal, format!("Reason: {}", p.reason));
            push(
                lines,
                Tone::Muted,
                format!("{:?} · revision {}", p.state, p.revision),
            );
            push(
                lines,
                Tone::Muted,
                "A proposal is not permission. Approving issues a reviewed grant.",
            );
        }
        Some(PermissionItem::Grant(g)) => {
            push(lines, Tone::Accent, format!("Grant {}", g.id));
            push(
                lines,
                Tone::Normal,
                format!("Audience: {}", audience(manager, &g.audience)),
            );
            push(
                lines,
                Tone::Normal,
                format!("Target: {}", target(manager, &g.target)),
            );
            push(
                lines,
                Tone::Normal,
                format!("State: {:?} · revision {}", g.state, g.revision),
            );
            if let Some(source) = g.copied_from {
                push(
                    lines,
                    Tone::Muted,
                    format!("Copied from grant {source} (independently revocable)"),
                );
            }
            if let Some(authorization) = &g.authorization {
                push(
                    lines,
                    Tone::Muted,
                    format!(
                        "Issued by client {} (request {})",
                        authorization.client, authorization.request
                    ),
                );
            }
        }
        None => push(lines, Tone::Muted, "Nothing selected."),
    }
}

pub(crate) fn closeout_review_lines(review: &CloseoutReview) -> Vec<(Tone, String)> {
    let mut lines = Vec::new();
    push(
        &mut lines,
        Tone::Normal,
        format!(
            "Review {} at revision {}",
            short(review.id),
            review.revision
        ),
    );
    if let Some(digest) = &review.inventory_digest {
        push(&mut lines, Tone::Muted, format!("Inventory {digest}"));
    }
    if let Some(digest) = &review.preservation_digest {
        push(&mut lines, Tone::Muted, format!("Preservation {digest}"));
    }
    push(
        &mut lines,
        Tone::Muted,
        format!(
            "Preservation volume ownership enforced: {}",
            review
                .preservation_volume_ownership
                .map_or("unknown".into(), |v| v.to_string())
        ),
    );
    for finding in &review.work.findings {
        push(
            &mut lines,
            Tone::Warn,
            format!(
                "Live work {:?}: {} — {}",
                finding.kind, finding.identity, finding.detail
            ),
        );
    }
    issues(&mut lines, &review.issues);
    if review.issues.is_empty() && review.work.findings.is_empty() {
        push(&mut lines, Tone::Good, "No unresolved findings.");
    }
    lines
}

pub(crate) fn recovery_lines(review: &CloseoutRecoveryReview) -> Vec<(Tone, String)> {
    let mut lines = vec![(
        Tone::Normal,
        format!(
            "Recovery {:?} at revision {}",
            review.action, review.revision
        ),
    )];
    for path in &review.paths {
        push(
            &mut lines,
            Tone::Normal,
            format!(
                "  {} present={} matches={}",
                path.path.display(),
                path.present,
                path.matches_recorded_root
            ),
        );
    }
    if let Some(holding) = &review.adopt_empty_holding {
        push(
            &mut lines,
            Tone::Muted,
            format!("Empty holding directory to adopt: {}", holding.display()),
        );
    }
    for finding in &review.work.findings {
        push(
            &mut lines,
            Tone::Warn,
            format!("Live work {:?}: {}", finding.kind, finding.detail),
        );
    }
    issues(&mut lines, &review.issues);
    for issue in &review.evidence_issues {
        push(
            &mut lines,
            Tone::Muted,
            format!("Unavailable evidence {:?}: {}", issue.code, issue.detail),
        );
    }
    lines
}

fn closeout_detail(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) {
    let Some(record) = &manager.closeouts.record else {
        push(
            lines,
            Tone::Muted,
            "No closeout selected. Start one from Organization on a Ready checkout (o).",
        );
        return;
    };
    match manager.closeouts.pane {
        CloseoutPane::Record => {
            push(
                lines,
                Tone::Accent,
                format!(
                    "Closeout of {}",
                    location_name(manager, record.spec.location)
                ),
            );
            push(
                lines,
                Tone::Normal,
                format!("Stage {:?} · revision {}", record.stage, record.revision),
            );
            push(
                lines,
                Tone::Muted,
                format!(
                    "Started by {} · operation {}",
                    record.initiated_by, record.operation
                ),
            );
            push(
                lines,
                Tone::Normal,
                format!("Preservation: {}", record.preservation_directory.display()),
            );
            push(
                lines,
                if record.spec.conditional_no_loss {
                    Tone::Warn
                } else {
                    Tone::Muted
                },
                format!(
                    "Conditional no-loss authorization: {}",
                    if record.spec.conditional_no_loss {
                        "on"
                    } else {
                        "off"
                    }
                ),
            );
            if let Some(authorization) = &record.authorization {
                let source = match &authorization.source {
                    CloseoutAuthorizationSource::Human { client } => {
                        format!("human approval from client {client}")
                    }
                    CloseoutAuthorizationSource::Conditional {
                        session,
                        assessment,
                    } => format!("agent declaration by {session}: {assessment}"),
                };
                push(lines, Tone::Accent, format!("Authorized by {source}"));
            }
            push(
                lines,
                Tone::Normal,
                format!(
                    "Inventory: {} entries{}",
                    record.inventory_entries,
                    record
                        .inventory_digest
                        .as_ref()
                        .map(|d| format!(" · {d}"))
                        .unwrap_or_default()
                ),
            );
            if let Some(digest) = &record.preservation_digest {
                push(
                    lines,
                    Tone::Normal,
                    format!("Preservation verified · {digest}"),
                );
            }
            if record.removed_entries > 0 {
                push(
                    lines,
                    Tone::Normal,
                    format!("Removed entries: {}", record.removed_entries),
                );
            }
            issues(lines, &record.issues);
            match &manager.closeouts.action {
                Some((request, None)) => push(
                    lines,
                    Tone::Accent,
                    format!("Action {} running…", short(request)),
                ),
                Some((_, Some(action))) => push(
                    lines,
                    Tone::Muted,
                    format!(
                        "Last action {} (run {})",
                        closeout_action(&action.spec.action),
                        action.run_id
                    ),
                ),
                None => {}
            }
            if let Some(review) = &manager.closeouts.review {
                push(lines, Tone::Normal, "");
                push(lines, Tone::Accent, "Removal review");
                lines.extend(closeout_review_lines(review));
            }
            if let Some(recovery) = &manager.closeouts.recovery {
                push(lines, Tone::Normal, "");
                push(lines, Tone::Accent, "Recovery review");
                lines.extend(recovery_lines(recovery));
            }
        }
        CloseoutPane::Inventory => match &manager.closeouts.inventory {
            Some(page) => {
                push(
                    lines,
                    Tone::Accent,
                    format!(
                        "Inventory {}–{} of {} (↑↓ select · d disposition{})",
                        manager.closeouts.inventory_after + u64::from(!page.entries.is_empty()),
                        manager.closeouts.inventory_after + page.entries.len() as u64,
                        page.total,
                        if page.next.is_some() || !manager.closeouts.inventory_previous.is_empty() {
                            " · [ ] page"
                        } else {
                            ""
                        }
                    ),
                );
                for (index, entry) in page.entries.iter().enumerate() {
                    let marker = if index == manager.closeouts.entry {
                        "▶"
                    } else {
                        " "
                    };
                    push(
                        lines,
                        if entry.blockers.is_empty() {
                            Tone::Normal
                        } else {
                            Tone::Warn
                        },
                        format!(
                            "{marker} {:?} {} {} B{}",
                            entry.kind,
                            entry_path(&entry.path),
                            entry.bytes,
                            if entry.blockers.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "  ! {}",
                                    entry
                                        .blockers
                                        .iter()
                                        .map(|b| b.detail.clone())
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                )
                            }
                        ),
                    );
                    if index == manager.closeouts.entry {
                        for fact in &entry.facts {
                            push(lines, Tone::Muted, format!("    {fact}"));
                        }
                    }
                }
            }
            None => push(lines, Tone::Muted, "Reading inventory…"),
        },
        CloseoutPane::Removal => match &manager.closeouts.removal {
            Some(page) => {
                push(
                    lines,
                    Tone::Accent,
                    format!("Removal {} of {} complete", page.completed, page.total),
                );
                if let Some(pending) = &page.pending {
                    push(
                        lines,
                        Tone::Warn,
                        format!(
                            "In progress: {} ({:?})",
                            entry_path(&pending.path),
                            pending.progress
                        ),
                    );
                }
                for entry in &page.entries {
                    push(
                        lines,
                        Tone::Muted,
                        format!("  {:?} {}", entry.progress, entry_path(&entry.path)),
                    );
                }
            }
            None => push(lines, Tone::Muted, "Reading removal progress…"),
        },
        CloseoutPane::History => match &manager.closeouts.history {
            Some(history) => {
                push(
                    lines,
                    Tone::Accent,
                    format!("History of {}", history.location.name),
                );
                push(
                    lines,
                    Tone::Normal,
                    format!(
                        "Lifecycle {:?} · {}",
                        history.location.lifecycle,
                        history.location.observed_path.display()
                    ),
                );
                for path in &history.preservation_paths {
                    push(
                        lines,
                        Tone::Normal,
                        format!("Preserved: {}", path.display()),
                    );
                }
                if let Some(report) = &history.report {
                    push(lines, Tone::Normal, format!("Report: {}", report.display()));
                }
            }
            None => push(lines, Tone::Muted, "Reading history…"),
        },
    }
}

pub(crate) fn recovery_item_lines(item: &RecoveryItem) -> Vec<(Tone, String)> {
    let mut lines = vec![
        (Tone::Normal, format!("Session {}", item.session)),
        (
            Tone::Normal,
            format!("Interrupted by {:?} at {}", item.cause, item.detected_at),
        ),
        (
            Tone::Muted,
            format!(
                "Turn {} of runtime {} · revision {}",
                item.turn, item.runtime, item.revision
            ),
        ),
    ];
    for execution in &item.executions {
        lines.push((
            if execution.live_owner {
                Tone::Accent
            } else {
                Tone::Warn
            },
            format!(
                "  {} {} {}{}",
                execution.tool,
                execution.id,
                execution.state,
                if execution.live_owner {
                    " (still running; not replayed)"
                } else {
                    ""
                }
            ),
        ));
    }
    if let Some(resolved) = &item.resolved {
        lines.push((Tone::Good, format!("Resolved: {}", resolution(resolved))));
    }
    lines
}

fn runtime_detail(manager: &WorkspaceManager, lines: &mut Vec<(Tone, String)>) {
    if !manager.connected {
        push(lines, Tone::Warn, "Runtime unreachable from this client.");
        match &manager.runtime.offline {
            Some(offline) => {
                if let Some(status) = &offline.status {
                    push(
                        lines,
                        if status.desired_stopped {
                            Tone::Warn
                        } else {
                            Tone::Normal
                        },
                        if status.desired_stopped {
                            "Durable intent: intentionally stopped."
                        } else {
                            "Durable intent: available (reconnecting)."
                        },
                    );
                    if let Some(operation) = &status.operation {
                        push(
                            lines,
                            phase_tone(operation.phase),
                            format!("Last shutdown {} · {:?}", operation.id, operation.phase),
                        );
                    }
                }
                push(lines, Tone::Muted, offline.detail.clone());
            }
            None => push(lines, Tone::Muted, "Reading durable runtime intent…"),
        }
        push(
            lines,
            Tone::Normal,
            "Press S to start it (runs `jcode runtime start`). Status alone never starts it.",
        );
        if let Some(start) = &manager.runtime.start {
            match start {
                Ok(text) => push(lines, Tone::Good, text.clone()),
                Err(text) => push(lines, Tone::Bad, text.clone()),
            }
        }
        return;
    }
    match manager.caps.runtime {
        None => {
            push(lines, Tone::Muted, "Negotiating runtime controls…");
            return;
        }
        Some(false) => {
            push(
                lines,
                Tone::Warn,
                "This runtime has no reviewed lifecycle control on this platform.",
            );
            return;
        }
        Some(true) => {}
    }
    if let Some(status) = &manager.runtime.status {
        push(
            lines,
            Tone::Accent,
            format!("Runtime {}", status.runtime.as_deref().unwrap_or("unknown")),
        );
        push(
            lines,
            Tone::Muted,
            format!(
                "Namespace {} · revision {}",
                status.namespace, status.revision
            ),
        );
        if status.reload_in_progress {
            push(lines, Tone::Accent, "Reload in progress.");
        }
        push(
            lines,
            Tone::Normal,
            format!("Admitted work: {}", status.work.len()),
        );
        for work in status.work.iter().take(20) {
            push(
                lines,
                Tone::Muted,
                format!(
                    "  {:?} {}{}{}",
                    work.kind,
                    work.id,
                    work.session
                        .as_ref()
                        .map(|s| format!(" · {s}"))
                        .unwrap_or_default(),
                    if work.supported_survivor {
                        " · can survive Stop"
                    } else {
                        ""
                    }
                ),
            );
        }
    } else {
        push(lines, Tone::Muted, "Reading runtime status…");
    }
    if let Some(operation) = &manager.runtime.operation {
        push(lines, Tone::Normal, "");
        push(
            lines,
            phase_tone(operation.phase),
            format!(
                "{} {} · {:?} · revision {}",
                if operation.phase.terminal() {
                    "Last shutdown"
                } else {
                    "Shutdown"
                },
                operation.id,
                operation.phase,
                operation.revision
            ),
        );
        let options = &operation.review.options;
        push(
            lines,
            Tone::Muted,
            format!(
                "{:?} · independent tasks {:?} · quiescence {}s · then {:?}",
                options.strategy,
                options.independent,
                options.quiescence_timeout_seconds,
                options.destination
            ),
        );
        if operation.cancellation_closed && !operation.phase.terminal() {
            push(
                lines,
                Tone::Muted,
                "Cancellation closed: stopping has begun.",
            );
        }
        for work in &operation.remaining {
            push(
                lines,
                Tone::Warn,
                format!("  remaining {:?} {}", work.kind, work.id),
            );
        }
        for work in &operation.preserved {
            push(
                lines,
                Tone::Good,
                format!("  preserved {:?} {}", work.kind, work.id),
            );
        }
        for issue in &operation.issues {
            push(lines, Tone::Warn, format!("  ! {issue}"));
        }
    }
    if let Some(supervision) = &manager.runtime.supervision {
        push(lines, Tone::Normal, "");
        push(
            lines,
            Tone::Normal,
            format!(
                "Login service supervision: {}",
                if supervision.supervised { "yes" } else { "no" }
            ),
        );
        let power = &supervision.power;
        push(
            lines,
            Tone::Muted,
            format!(
                "Sleep prevention: {} · available {} · held {} · active work {}",
                if power.enabled { "on" } else { "off" },
                power.available,
                power.active,
                power.active_work
            ),
        );
        let unresolved = supervision
            .recoveries
            .iter()
            .filter(|item| item.resolved.is_none())
            .count();
        push(
            lines,
            if unresolved > 0 {
                Tone::Warn
            } else {
                Tone::Muted
            },
            format!("Interrupted turns needing a decision: {unresolved}"),
        );
    }
    match &manager.runtime.service {
        Some(Ok(text)) => push(lines, Tone::Muted, text.clone()),
        Some(Err(text)) => push(lines, Tone::Muted, format!("Login service: {text}")),
        None => {}
    }
    if let Some(key) = &manager.runtime.selected
        && let Some(item) = manager
            .runtime
            .supervision
            .as_ref()
            .and_then(|s| s.recoveries.iter().find(|item| item.id.to_string() == *key))
    {
        push(lines, Tone::Normal, "");
        push(lines, Tone::Accent, "Selected interrupted turn");
        lines.extend(recovery_item_lines(item));
    }
    push(lines, Tone::Normal, "");
    push(
        lines,
        Tone::Muted,
        "Service install/uninstall stays with `jcode runtime service`, which shows its exact plan digest.",
    );
}

fn change_lines(manager: &WorkspaceManager, change: &OrganizationChange) -> Vec<(Tone, String)> {
    let line = match change {
        OrganizationChange::CreateProject { name } => format!("Create project {name:?}"),
        OrganizationChange::CreateRepository { name, remotes } => format!(
            "Create repository {name:?} with {} remote reference(s)",
            remotes.len()
        ),
        OrganizationChange::AssociateRepository {
            project,
            repository,
        } => format!(
            "Associate {} with project {}",
            name_of(manager, repository),
            project_name(manager, *project)
        ),
        OrganizationChange::RemoveRepositoryAssociation {
            project,
            repository,
        } => format!(
            "Remove association of {} from project {}",
            name_of(manager, repository),
            project_name(manager, *project)
        ),
        OrganizationChange::CreateWorkArea { project, name } => format!(
            "Create work area {name:?} in project {}",
            project_name(manager, *project)
        ),
        OrganizationChange::RegisterLocation {
            name,
            path,
            registration,
        } => format!(
            "Register {name:?} at {} as {}",
            path.display(),
            match registration {
                Registration::Checkout {
                    home: h,
                    repository,
                } => format!(
                    "checkout of {} in {}",
                    name_of(manager, repository),
                    home(manager, *h)
                ),
                Registration::Directory { home: h } =>
                    format!("directory in {}", home(manager, *h)),
                Registration::Standalone => "standalone location".into(),
            }
        ),
        OrganizationChange::RebindLocation {
            location,
            expected_old_path,
            new_path,
            ..
        } => format!(
            "Rebind {} from {} to {}",
            name_of(manager, location),
            expected_old_path.display(),
            new_path.display()
        ),
        OrganizationChange::MoveLocation {
            location,
            home: h,
            associate_repository,
        } => format!(
            "Move {} to {}{}",
            name_of(manager, location),
            home(manager, *h),
            if *associate_repository {
                ", associating its repository"
            } else {
                ""
            }
        ),
        OrganizationChange::AdoptStandalone {
            location, home: h, ..
        } => format!(
            "Adopt standalone {} into {}",
            name_of(manager, location),
            home(manager, *h)
        ),
        OrganizationChange::Rename { target, name } => {
            format!("Rename {} to {name:?}", name_of(manager, target))
        }
        OrganizationChange::Archive { target, archived } => format!(
            "{} {}",
            if *archived { "Archive" } else { "Unarchive" },
            name_of(manager, target)
        ),
        OrganizationChange::Retire { target } => {
            format!("Retire {} (history is kept)", name_of(manager, target))
        }
        OrganizationChange::DiscardUnused { target } => {
            format!("Discard unused {}", name_of(manager, target))
        }
        OrganizationChange::SetVolumeDefault { volume_uuid, path } => format!(
            "Default checkout directory on volume {volume_uuid}: {}",
            path.display()
        ),
    };
    let mut lines = vec![(Tone::Normal, line)];
    if matches!(change, OrganizationChange::Archive { .. }) {
        lines.push((
            Tone::Muted,
            "Archive changes visibility only. Work, permissions and files are unchanged.".into(),
        ));
    }
    lines
}

pub(crate) fn launch_lines(
    manager: &WorkspaceManager,
    request: &PrimaryLaunchRequest,
) -> Vec<(Tone, String)> {
    let input = &request.input;
    let mut lines = vec![
        (
            Tone::Normal,
            match &input.placement {
                PrimaryPlacement::Existing { placement: p } => {
                    format!("Placement: {}", placement(manager, *p))
                }
                PrimaryPlacement::Standalone { root } => {
                    format!("Placement: new standalone location at {}", root.display())
                }
            },
        ),
        (
            Tone::Normal,
            match &input.cwd {
                Some(PrimaryCwd::Existing { path }) => {
                    format!("Working directory: {}", path.display())
                }
                Some(PrimaryCwd::CreateEmpty { path, .. }) => {
                    format!("Working directory: create empty {}", path.display())
                }
                None => "Working directory: placement root".into(),
            },
        ),
        (
            Tone::Normal,
            format!("Agent: {}", input.agent.as_deref().unwrap_or("default")),
        ),
        (
            Tone::Normal,
            match &input.model {
                Some(model) => format!(
                    "Route: {} / {} / {}{}",
                    model.provider,
                    model.model,
                    model.api_method,
                    model
                        .effort
                        .as_ref()
                        .map(|e| format!(" · {e}"))
                        .unwrap_or_default()
                ),
                None => "Route: default".into(),
            },
        ),
    ];
    if input.selfdev {
        lines.push((Tone::Warn, "Self-development session".into()));
    }
    lines.push((
        Tone::Muted,
        format!(
            "Request {}. Startup Context is captured for that cwd; no clone is created.",
            request.request
        ),
    ));
    lines
}

pub(crate) fn shutdown_confirm(review: &ShutdownReview) -> Confirm {
    let options = &review.options;
    let mut lines = vec![
        (
            Tone::Normal,
            format!(
                "Runtime {} · review revision {}",
                review.runtime, review.revision
            ),
        ),
        (
            Tone::Normal,
            format!("Current work: {:?}", options.strategy),
        ),
        (
            Tone::Normal,
            format!("Independent native commands: {:?}", options.independent),
        ),
        (
            Tone::Normal,
            format!(
                "Quiescence deadline after stopping begins: {}s",
                options.quiescence_timeout_seconds
            ),
        ),
        (
            if options.destination.is_stopped() {
                Tone::Warn
            } else {
                Tone::Accent
            },
            if options.destination.is_stopped() {
                "Afterwards the runtime stays stopped until an explicit Start. This client will disconnect.".into()
            } else {
                "Afterwards a fresh runtime starts; interrupted turns continue once.".into()
            },
        ),
        (
            Tone::Normal,
            format!("Affected work ({}):", review.work.len()),
        ),
    ];
    for work in &review.work {
        lines.push((
            Tone::Muted,
            format!(
                "  {:?} {}{}{}",
                work.kind,
                work.id,
                work.session
                    .as_ref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default(),
                if work.supported_survivor {
                    " · may survive"
                } else {
                    ""
                }
            ),
        ));
    }
    if let Some(replaces) = &review.replaces {
        lines.push((
            Tone::Muted,
            format!(
                "Replaces operation {} at revision {}",
                replaces.operation, replaces.revision
            ),
        ));
    }
    lines.push((
        Tone::Muted,
        "Acceptance is not completion; progress appears here.".into(),
    ));
    Confirm {
        title: if options.destination.is_stopped() {
            "Stop the runtime".into()
        } else {
            "Restart the runtime".into()
        },
        lines,
        op: Op::runtime(
            View::Effect,
            RuntimeRequest::Begin {
                request: RequestId::new(),
                review: review.id,
            },
        ),
        typed: None,
        input: String::new(),
        yes: false,
        scroll: 0,
    }
}

/// A server review becomes a confirmation of exactly that review.
pub(crate) fn review_confirm(
    manager: &WorkspaceManager,
    request: &WorkspaceRequest,
    response: WorkspaceResponse,
) -> Option<Confirm> {
    use WorkspaceResponse as R;
    let make = |title: &str,
                lines: Vec<(Tone, String)>,
                apply: WorkspaceRequest,
                typed: Option<&'static str>| Confirm {
        title: title.into(),
        lines,
        op: Op::read(View::Effect, apply),
        typed,
        input: String::new(),
        yes: false,
        scroll: 0,
    };
    match response {
        R::Review(review) => {
            let mut lines = change_lines(manager, &review.change);
            lines.push((
                Tone::Muted,
                format!(
                    "Review {} at catalog revision {}",
                    short(review.id),
                    review.revision
                ),
            ));
            issues(&mut lines, &review.issues);
            Some(make(
                "Apply organization change",
                lines,
                WorkspaceRequest::Apply {
                    request: RequestId::new(),
                    review: review.id,
                },
                None,
            ))
        }
        R::CloneReview(review) => {
            let spec = &review.spec;
            let mut lines = vec![
                (
                    Tone::Normal,
                    format!("{} in {}", spec.name, home(manager, spec.home)),
                ),
                (
                    Tone::Normal,
                    format!(
                        "Source: {}",
                        match &spec.source {
                            CloneSource::Remote { url } => url.clone(),
                            CloneSource::Local { path } =>
                                format!("local {} (committed work only)", path.display()),
                        }
                    ),
                ),
                (
                    Tone::Normal,
                    format!("Base: {:?} → commit {}", spec.base, review.source_commit),
                ),
                (Tone::Normal, format!("Branch: {:?}", spec.branch)),
                (
                    Tone::Normal,
                    format!(
                        "Destination: {} (volume {})",
                        review.destination.display(),
                        review.volume_uuid
                    ),
                ),
                (
                    Tone::Normal,
                    format!(
                        "Remotes: {}",
                        spec.remotes
                            .iter()
                            .map(|r| format!("{}={}", r.name, r.url))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                (
                    Tone::Normal,
                    format!("Submodules: {} · LFS: {}", spec.submodules, spec.lfs),
                ),
                (
                    Tone::Muted,
                    "No hosted fork, push or project setup command is run.".into(),
                ),
            ];
            issues(&mut lines, &review.issues);
            Some(make(
                "Begin clone",
                lines,
                WorkspaceRequest::BeginClone {
                    request: RequestId::new(),
                    review: review.id,
                },
                None,
            ))
        }
        R::CloneTrustReview(review) => {
            let mut lines = vec![(
                Tone::Normal,
                format!(
                    "Stage {} at commit {}",
                    review.stage.display(),
                    review.source_commit
                ),
            )];
            for source in &review.sources {
                lines.push((
                    Tone::Warn,
                    format!(
                        "  {:?} {} declared at {}/{}",
                        source.kind,
                        source.url,
                        source.repository.display(),
                        source.path.display()
                    ),
                ));
            }
            lines.push((
                Tone::Muted,
                "Approves exactly these sources for this clone, not any future transport.".into(),
            ));
            Some(make(
                "Approve clone sources",
                lines,
                WorkspaceRequest::ApplyCloneTrust {
                    request: RequestId::new(),
                    review: review.id,
                },
                None,
            ))
        }
        R::StartupCopyReview(review) => {
            let mut lines = vec![(
                Tone::Normal,
                format!(
                    "{} → {}",
                    review.source.display(),
                    review.target_path.display()
                ),
            )];
            for entry in &review.entries {
                lines.push((
                    if entry.external {
                        Tone::Warn
                    } else {
                        Tone::Normal
                    },
                    format!(
                        "  {} → {}{}",
                        entry.selected_path.display(),
                        entry.resolved_target.display(),
                        if entry.external { " (external)" } else { "" }
                    ),
                ));
            }
            lines.push((
                Tone::Muted,
                "Paths only; the destination recaptures its own bytes.".into(),
            ));
            Some(make(
                "Copy Startup Context selection",
                lines,
                WorkspaceRequest::ApplyStartupCopy {
                    request: RequestId::new(),
                    review: review.id,
                },
                None,
            ))
        }
        R::ImportReview(review) => {
            let mut lines = vec![
                (
                    Tone::Normal,
                    format!("From installation {}", review.source_installation),
                ),
                (
                    Tone::Normal,
                    format!(
                        "{} entities · {} collisions · {} remapped · {} unavailable",
                        review.entities.len(),
                        review.collisions.len(),
                        review.remapped.len(),
                        review.unavailable.len()
                    ),
                ),
                (
                    Tone::Warn,
                    format!(
                        "{} grant definitions stay disabled until reviewed",
                        review.disabled_grants.len()
                    ),
                ),
            ];
            for item in &review.external_content {
                lines.push((Tone::Muted, format!("Not included: {item}")));
            }
            issues(&mut lines, &review.issues);
            Some(make(
                "Apply import",
                lines,
                WorkspaceRequest::ApplyImport {
                    request: RequestId::new(),
                    review: review.id,
                },
                None,
            ))
        }
        R::RestoreReview(review) => {
            let mut lines = vec![
                (
                    Tone::Warn,
                    format!(
                        "Replace the catalog with snapshot {:?} (revision {}).",
                        review.snapshot.name, review.snapshot.revision
                    ),
                ),
                (
                    Tone::Normal,
                    format!(
                        "Current revision: {}",
                        review
                            .current_revision
                            .map_or("unknown".into(), |r| r.to_string())
                    ),
                ),
                (
                    Tone::Normal,
                    format!("{} grant definitions in the snapshot", review.grants.len()),
                ),
                (
                    Tone::Muted,
                    "A pre-restore snapshot is kept. No filesystem effect is replayed.".into(),
                ),
            ];
            issues(&mut lines, &review.issues);
            Some(make(
                "Restore catalog",
                lines,
                WorkspaceRequest::ApplyRestore {
                    request: RequestId::new(),
                    review: review.id,
                },
                Some("restore"),
            ))
        }
        R::Permissions(response) => match *response {
            PermissionResponse::Review(review) => {
                let mut lines = vec![(
                    Tone::Normal,
                    match &review.change {
                        GrantChange::Issue { proposal, .. } => format!(
                            "Issue grant{}",
                            proposal
                                .map(|p| format!(" for proposal {p}"))
                                .unwrap_or_default()
                        ),
                        GrantChange::Revoke { grant } => format!("Revoke grant {grant}"),
                        GrantChange::BindImported { .. } => {
                            "Bind imported grant (stays disabled)".into()
                        }
                        GrantChange::ActivateImported { grant } => {
                            format!("Activate imported grant {grant}")
                        }
                    },
                )];
                lines.push((
                    Tone::Normal,
                    format!("Audience: {}", audience(manager, &review.grant.audience)),
                ));
                lines.push((
                    Tone::Normal,
                    format!("Target: {}", target(manager, &review.grant.target)),
                ));
                for root in &review.roots {
                    lines.push((
                        Tone::Normal,
                        format!("  writable now: {}", root.observed_path.display()),
                    ));
                }
                for root in &review.excluded_roots {
                    lines.push((
                        Tone::Muted,
                        format!("  excluded: {}", root.observed_path.display()),
                    ));
                }
                if matches!(
                    review.grant.target,
                    WriteTarget::ProjectMembers(_) | WriteTarget::WorkAreaMembers(_)
                ) {
                    lines.push((Tone::Muted, "Future member roots are included too.".into()));
                }
                lines.push((
                    Tone::Muted,
                    "Native writes only; shell and external tools are not sandboxed.".into(),
                ));
                let _ = request;
                Some(Confirm {
                    title: "Apply permission change".into(),
                    lines,
                    op: Op::permission(
                        View::Effect,
                        PermissionRequest::Apply {
                            request: RequestId::new(),
                            review: review.id,
                        },
                    ),
                    typed: None,
                    input: String::new(),
                    yes: false,
                    scroll: 0,
                })
            }
            _ => None,
        },
        _ => None,
    }
}
