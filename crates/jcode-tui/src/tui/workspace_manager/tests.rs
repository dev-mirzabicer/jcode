//! Manager reducer, correlation, form and frame tests over synthetic typed
//! replies. These exercise the client mechanism, not server behavior.
use super::*;
use crate::protocol::Request;
use crate::workspace::runtime::{
    PowerStatus, RecoveryCause, RecoveryItem, RuntimeRequest, RuntimeResponse, RuntimeStatus,
    ShutdownOperation, ShutdownPhase, ShutdownReview, SupervisionStatus,
};
use ratatui::{Terminal, backend::TestBackend};

struct Wire {
    next: u64,
}

impl Wire {
    fn new() -> Self {
        Self { next: 1 }
    }
    /// Reserve and "send" everything queued, like the app dispatcher.
    fn drain(&mut self, m: &mut WorkspaceManager) -> Vec<(u64, Request)> {
        let mut sent = Vec::new();
        while let Some(request) = m.reserve(self.next) {
            sent.push((self.next, request));
            self.next += 1;
        }
        sent
    }
}

fn status(revision: Revision) -> CatalogStatus {
    CatalogStatus {
        installation: InstallationId::new(),
        schema: 1,
        revision,
        managed_rollout: false,
    }
}

fn answer_probes(m: &mut WorkspaceManager, wire: &mut Wire, launch: bool) {
    for (id, request) in wire.drain(m) {
        let event = match request {
            Request::WorkspaceProbe { .. } => ServerEvent::WorkspaceCapabilities {
                id,
                catalog_version: 1,
                permissions_version: Some(1),
                checkout_version: Some(1),
                closeout_version: Some(2),
                management_version: Some(1),
                managed_rollout: false,
            },
            Request::PrimaryControlProbe { .. } => ServerEvent::PrimaryControlCapabilities {
                id,
                input_version: 1,
                location_version: 1,
                location_enabled: launch,
                legacy_adoption_version: Some(1),
                context_scope_version: Some(1),
                session_inspection_version: Some(1),
                session_placement_version: Some(1),
            },
            Request::PrimaryLaunchProbe { .. } => ServerEvent::PrimaryLaunchCapabilities {
                id,
                version: 1,
                enabled: launch,
            },
            Request::RuntimeProbe { .. } => ServerEvent::RuntimeCapabilities {
                id,
                version: Some(1),
                supervision: Some(1),
            },
            other => panic!("unexpected probe {other:?}"),
        };
        assert!(m.accept(id, event));
    }
}

/// Answer only the catalog Status read; leave everything else unanswered.
fn answer_status(
    m: &mut WorkspaceManager,
    wire: &mut Wire,
    result: Result<CatalogStatus, Issue>,
) -> Vec<(u64, Request)> {
    let mut rest = Vec::new();
    for (id, request) in wire.drain(m) {
        if matches!(&request, Request::Workspace { request, .. } if matches!(**request, WorkspaceRequest::Status {}))
        {
            let response = match &result {
                Ok(status) => WorkspaceResponse::Status(status.clone()),
                Err(issue) => WorkspaceResponse::Error(issue.clone()),
            };
            assert!(m.accept(
                id,
                ServerEvent::WorkspaceResponse {
                    id,
                    response: Box::new(response)
                }
            ));
        } else {
            rest.push((id, request));
        }
    }
    rest
}

fn ready(launch: bool) -> (WorkspaceManager, Wire) {
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Organization);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, launch);
    answer_status(&mut m, &mut wire, Ok(status(3)));
    (m, wire)
}

fn workspace(id: u64, response: WorkspaceResponse) -> ServerEvent {
    ServerEvent::WorkspaceResponse {
        id,
        response: Box::new(response),
    }
}

#[track_caller]
fn find(
    sent: &[(u64, Request)],
    pred: impl Fn(&WorkspaceRequest) -> bool,
) -> (u64, &WorkspaceRequest) {
    sent.iter()
        .find_map(|(id, request)| match request {
            Request::Workspace { request, .. } if pred(request) => Some((*id, &**request)),
            _ => None,
        })
        .expect("expected workspace request")
}

fn frame(m: &mut WorkspaceManager, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| m.render(f, f.area())).unwrap();
    let buffer = terminal.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn project(name: &str) -> Entity {
    Entity::Project(Project {
        id: ProjectId::new(),
        name: name.into(),
        state: OrganizationState::Active,
        revision: 1,
    })
}

fn key(m: &mut WorkspaceManager, code: KeyCode) {
    m.key(code, KeyModifiers::NONE);
}

fn type_text(m: &mut WorkspaceManager, text: &str) {
    for c in text.chars() {
        key(m, KeyCode::Char(c));
    }
}

#[test]
fn uninitialized_catalog_offers_reviewed_initialize_only() {
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Organization);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    answer_status(&mut m, &mut wire, Err(Issue::not_initialized()));
    let actions = actions::available(&m);
    assert!(actions.iter().any(|spec| spec.key == 'I'));
    assert!(
        !actions.iter().any(|spec| spec.key == 'n'),
        "no organization effects before init"
    );
    key(&mut m, KeyCode::Char('I'));
    assert!(m.confirm.is_some());
    assert!(
        wire.drain(&mut m).is_empty(),
        "a review shows; nothing is sent yet"
    );
    key(&mut m, KeyCode::Char('n'));
    assert!(m.confirm.is_none());
    assert!(wire.drain(&mut m).is_empty(), "declining sends nothing");
    key(&mut m, KeyCode::Char('I'));
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let (id, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Initialize { .. }));
    let _ = request;
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Status(status(0)))));
    assert!(m.status.contains("initialized"));
}

#[test]
fn review_then_confirm_applies_exactly_that_review() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('n'));
    type_text(&mut m, "Acme");
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(m.form.is_none());
    let sent = wire.drain(&mut m);
    let (id, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Review { .. }));
    let WorkspaceRequest::Review {
        expected_revision,
        change,
    } = request.clone()
    else {
        unreachable!()
    };
    assert_eq!(expected_revision, 3);
    assert_eq!(
        change,
        OrganizationChange::CreateProject {
            name: "Acme".into()
        }
    );
    let review = Review {
        id: ReviewId::new(),
        revision: 3,
        change: change.clone(),
        targets: vec![],
        issues: vec![],
    };
    // A review for a different change is not accepted as this reply.
    let other = Review {
        change: OrganizationChange::CreateProject {
            name: "Other".into(),
        },
        ..review.clone()
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Review(other))));
    assert!(
        m.confirm.is_none(),
        "mismatched review never becomes a confirmation"
    );
    key(&mut m, KeyCode::Char('n'));
    type_text(&mut m, "Acme");
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let sent = wire.drain(&mut m);
    let (id, _) = find(&sent, |r| matches!(r, WorkspaceRequest::Review { .. }));
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Review(review.clone()))));
    let confirm = m.confirm.as_ref().expect("confirmation");
    assert!(confirm.lines.iter().any(|(_, line)| line.contains("Acme")));
    key(&mut m, KeyCode::Enter);
    assert!(
        m.confirm.is_none(),
        "Enter on the default Cancel button declines"
    );
    assert!(wire.drain(&mut m).is_empty());
    assert!(
        !m.accept(id, workspace(id, WorkspaceResponse::Review(review.clone()))),
        "a consumed reply id is never reused"
    );
    assert!(m.confirm.is_none());
}

#[test]
fn confirmed_effect_uncertainty_retries_the_same_request() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.confirm = Some(Confirm {
        title: "t".into(),
        lines: vec![],
        op: Op::read(
            transport::View::Effect,
            WorkspaceRequest::Apply {
                request: RequestId::new(),
                review: ReviewId::new(),
            },
        ),
        typed: None,
        input: String::new(),
        yes: false,
        scroll: 0,
    });
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let (_, first) = find(&sent, |r| matches!(r, WorkspaceRequest::Apply { .. }));
    let first = first.clone();
    m.reconnect("session_self");
    assert_eq!(
        m.uncertain.len(),
        1,
        "lost effect reply is uncertain, not resent"
    );
    let resent = wire.drain(&mut m);
    assert!(resent.iter().all(|(_, r)| !matches!(r, Request::Workspace { request, .. } if matches!(**request, WorkspaceRequest::Apply { .. }))));
    key(&mut m, KeyCode::Char('U'));
    assert!(m.show_uncertain);
    key(&mut m, KeyCode::Char('R'));
    let retried = wire.drain(&mut m);
    let (id, again) = find(&retried, |r| matches!(r, WorkspaceRequest::Apply { .. }));
    assert_eq!(
        *again, first,
        "retry replays the identical request identity"
    );
    let WorkspaceRequest::Apply { request, .. } = first else {
        unreachable!()
    };
    assert!(m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Receipt(Receipt {
                operation: OperationId::new(),
                request,
                revision: 4,
                targets: vec![],
                issues: vec![],
            })
        )
    ));
    assert!(m.uncertain.is_empty());
    assert!(m.status.starts_with("Done"));
}

#[test]
fn foreign_and_stale_replies_cannot_change_state() {
    let (mut m, mut wire) = ready(false);
    let sent = wire.drain(&mut m);
    let (id, _) = find(&sent, |r| matches!(r, WorkspaceRequest::List { .. }));
    assert!(!m.accept(
        999,
        workspace(
            999,
            WorkspaceResponse::Page(Page {
                revision: 3,
                total: 0,
                items: vec![],
                next: None
            })
        )
    ));
    m.reconnect("session_self");
    // The old request's reply is now from another generation.
    assert!(!m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Page(Page {
                revision: 3,
                total: 1,
                items: vec![project("x")],
                next: None
            })
        )
    ));
    assert!(m.org.page.is_none());
}

#[test]
fn organization_rows_select_by_stable_identity_and_prefill_actions() {
    let (mut m, mut wire) = ready(true);
    let sent = wire.drain(&mut m);
    let (id, _) = find(
        &sent,
        |r| matches!(r, WorkspaceRequest::List { query, .. } if query.kind.is_none()),
    );
    let a = project("Alpha");
    let b = project("Beta");
    assert!(m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Page(Page {
                revision: 3,
                total: 2,
                items: vec![a.clone(), b.clone()],
                next: None
            })
        )
    ));
    assert_eq!(m.org.selected, Some(a.id()));
    key(&mut m, KeyCode::Down);
    assert_eq!(m.org.selected, Some(b.id()));
    // A refresh that reorders rows keeps the selected identity.
    let sent = wire.drain(&mut m);
    assert!(sent.iter().any(|(_, r)| matches!(r, Request::Workspace { request, .. } if matches!(**request, WorkspaceRequest::Inspect { .. }))));
    m.load_org();
    let sent = wire.drain(&mut m);
    let (id, _) = find(&sent, |r| matches!(r, WorkspaceRequest::List { .. }));
    m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Page(Page {
                revision: 4,
                total: 2,
                items: vec![b.clone(), a.clone()],
                next: None,
            }),
        ),
    );
    assert_eq!(m.org.selected, Some(b.id()));
    key(&mut m, KeyCode::Char('a'));
    let (_, form) = m.form.as_ref().expect("area form");
    assert_eq!(form.value("project"), format!("project:{}", b.id()));
}

#[test]
fn clone_form_requires_a_volume_and_builds_exact_spec() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    let repo = Repository {
        id: RepositoryId::new(),
        name: "jcode".into(),
        remotes: vec!["https://example.invalid/jcode.git".into()],
        state: OrganizationState::Active,
        revision: 1,
    };
    let p = project("Acme");
    m.known.insert(p.id().to_string(), p.clone());
    m.known
        .insert(repo.id.to_string(), Entity::Repository(repo.clone()));
    m.org.selected = Some(EntityId::Repository(repo.id));
    m.volumes = Some(vec![]);
    actions::open(&mut m, actions::FormKind::Clone);
    let (kind, mut form) = m.form.clone().unwrap();
    form.set("home", format!("project:{}", p.id()));
    assert!(
        actions::build(&mut m, &kind, &form)
            .unwrap_err()
            .contains("volume")
    );
    m.volumes = Some(vec![WorkspaceVolume {
        uuid: "UUID-1".into(),
        mount: "/".into(),
        label: "Internal".into(),
        internal: true,
        writable: true,
        available_bytes: 1 << 34,
    }]);
    actions::open(&mut m, actions::FormKind::Clone);
    let (kind, mut form) = m.form.clone().unwrap();
    form.set("home", format!("project:{}", p.id()));
    form.set("project_component", "acme");
    actions::build(&mut m, &kind, &form).unwrap();
    let sent = wire.drain(&mut m);
    let (_, request) = find(&sent, |r| matches!(r, WorkspaceRequest::ReviewClone { .. }));
    let WorkspaceRequest::ReviewClone { spec, .. } = request else {
        unreachable!()
    };
    assert_eq!(spec.repository, repo.id);
    assert_eq!(
        spec.source,
        CloneSource::Remote {
            url: "https://example.invalid/jcode.git".into()
        }
    );
    assert_eq!(
        spec.remotes,
        vec![CloneRemote {
            name: "origin".into(),
            url: "https://example.invalid/jcode.git".into()
        }]
    );
    assert_eq!(
        spec.destination,
        CloneDestination::Default {
            volume_uuid: "UUID-1".into(),
            project_component: "acme".into(),
            checkout_component: "jcode".into()
        }
    );
    assert!(spec.submodules && spec.lfs);
}

#[test]
fn runtime_stop_review_confirm_and_force_needs_typed_word() {
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Runtime);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    let rest = answer_status(&mut m, &mut wire, Ok(status(1)));
    for (id, request) in rest {
        if let Request::RuntimeControl { request, .. } = request {
            let response = match *request {
                RuntimeRequest::Status {} => RuntimeResponse::Status(RuntimeStatus {
                    namespace: "ns".into(),
                    runtime: Some("rt".into()),
                    reload_in_progress: false,
                    desired_stopped: false,
                    revision: 1,
                    operation: None,
                    work: vec![],
                }),
                RuntimeRequest::Supervision {} => RuntimeResponse::Supervision(SupervisionStatus {
                    namespace: "ns".into(),
                    runtime: "rt".into(),
                    supervised: true,
                    power: PowerStatus {
                        enabled: true,
                        available: true,
                        active: false,
                        active_work: 0,
                    },
                    recoveries: vec![],
                }),
                other => panic!("{other:?}"),
            };
            m.accept(
                id,
                ServerEvent::RuntimeResponse {
                    id,
                    response: Box::new(response),
                },
            );
        }
    }
    key(&mut m, KeyCode::Char('s'));
    let (kind, form) = m.form.clone().expect("stop form");
    assert_eq!(form.value("strategy"), "finish");
    actions::build(&mut m, &kind, &form).unwrap();
    m.form = None;
    let sent = wire.drain(&mut m);
    let (id, request) = sent
        .iter()
        .find_map(|(id, r)| match r {
            Request::RuntimeControl { request, .. } => Some((*id, (**request).clone())),
            _ => None,
        })
        .unwrap();
    let RuntimeRequest::Review { options } = request else {
        panic!()
    };
    assert!(options.destination.is_stopped());
    let review = ShutdownReview {
        id: ReviewId::new(),
        runtime: "rt".into(),
        revision: 1,
        options: options.clone(),
        work: vec![],
        replaces: None,
    };
    m.accept(
        id,
        ServerEvent::RuntimeResponse {
            id,
            response: Box::new(RuntimeResponse::Review(review.clone())),
        },
    );
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let (id, begin) = sent
        .iter()
        .find_map(|(id, r)| match r {
            Request::RuntimeControl { request, .. } => Some((*id, (**request).clone())),
            _ => None,
        })
        .unwrap();
    let RuntimeRequest::Begin {
        request,
        review: bound,
    } = begin
    else {
        panic!()
    };
    assert_eq!(bound, review.id);
    let blocked = ShutdownOperation {
        id: OperationId::new(),
        request,
        review: review.clone(),
        revision: 2,
        phase: ShutdownPhase::Blocked,
        force_requested: false,
        cancellation_closed: true,
        remaining: vec![],
        preserved: vec![],
        issues: vec!["deadline".into()],
        origin: Default::default(),
    };
    m.accept(
        id,
        ServerEvent::RuntimeResponse {
            id,
            response: Box::new(RuntimeResponse::Operation(blocked.clone())),
        },
    );
    assert!(actions::available(&m).iter().any(|spec| spec.key == 'F'));
    assert!(
        !actions::available(&m).iter().any(|spec| spec.key == 'c'),
        "cancel closes once stopping began"
    );
    {
        // A disconnected client keeps the last operation it saw, but only the
        // live coordinator can act on it.
        let mut offline = WorkspaceManager::new("session".into(), true, Section::Runtime);
        offline.caps.runtime = Some(true);
        offline.runtime.operation = Some(blocked.clone());
        assert!(
            actions::available(&offline)
                .iter()
                .any(|spec| spec.key == 'F')
        );
        offline.disconnected();
        let keys: Vec<char> = actions::available(&offline)
            .iter()
            .map(|spec| spec.key)
            .collect();
        for control in ['F', 'c', 'h', 'y', 's', 'R'] {
            assert!(
                !keys.contains(&control),
                "{control} offered while offline: {keys:?}"
            );
        }
        assert!(keys.contains(&'S'), "{keys:?}");
    }
    key(&mut m, KeyCode::Char('F'));
    type_text(&mut m, "forc");
    key(&mut m, KeyCode::Enter);
    assert!(m.confirm.is_some(), "wrong word keeps the review open");
    assert!(wire.drain(&mut m).is_empty());
    type_text(&mut m, "e");
    key(&mut m, KeyCode::Enter);
    let sent = wire.drain(&mut m);
    assert!(sent.iter().any(|(_, r)| matches!(r, Request::RuntimeControl { request, .. } if matches!(**request, RuntimeRequest::Force { expected_revision: 2, .. }))));
}

#[test]
fn recovery_decision_binds_item_revision_and_one_request() {
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Runtime);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    let item = RecoveryItem {
        id: crate::workspace::runtime::RecoveryId::new(),
        session: "session_x".into(),
        turn: "t".into(),
        runtime: "old".into(),
        cause: RecoveryCause::UnexpectedExit,
        detected_at: "now".into(),
        revision: 7,
        resolved: None,
        executions: vec![],
    };
    m.runtime.supervision = Some(SupervisionStatus {
        namespace: "ns".into(),
        runtime: "rt".into(),
        supervised: false,
        power: PowerStatus {
            enabled: true,
            available: true,
            active: false,
            active_work: 0,
        },
        recoveries: vec![item.clone()],
    });
    m.runtime.selected = Some(item.id.to_string());
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('C'));
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let recover = sent
        .iter()
        .find_map(|(_, r)| match r {
            Request::RuntimeControl { request, .. } => Some((**request).clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(recover, RuntimeRequest::Recover { item: id, expected_revision: 7, decision: crate::workspace::runtime::RecoveryDecision::Continue, .. } if id == item.id)
    );
}

#[test]
fn closeout_begin_records_checkbox_and_removal_approval_is_typed() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    let location = LocationId::new();
    actions::open(
        &mut m,
        actions::FormKind::CloseoutBegin {
            location,
            generation: 2,
        },
    );
    let (kind, mut form) = m.form.clone().unwrap();
    assert_eq!(
        form.value("conditional"),
        "false",
        "conditional authorization defaults off"
    );
    form.set("conditional", "true");
    actions::build(&mut m, &kind, &form).unwrap();
    m.form = None;
    let confirm = m.confirm.clone().unwrap();
    assert!(confirm.lines.iter().any(|(_, l)| l.contains("ON")));
    let Op::Workspace {
        request:
            WorkspaceRequest::Closeout {
                request:
                    CloseoutRequest::Begin {
                        spec,
                        expected_revision,
                        ..
                    },
            },
        ..
    } = &confirm.op
    else {
        panic!()
    };
    assert!(spec.conditional_no_loss);
    assert_eq!(
        (spec.location, spec.expected_generation, *expected_revision),
        (location, 2, 3)
    );
    m.confirm = None;
    let record = CloseoutRecord {
        operation: OperationId::new(),
        request: RequestId::new(),
        spec: spec.clone(),
        revision: 5,
        stage: CloseoutStage::ReadyForApproval,
        initiated_by: "client".into(),
        inventory_digest: Some("d".into()),
        inventory_entries: 1,
        preservation_directory: "/tmp/p".into(),
        preservation_volume_ownership: None,
        authorization: None,
        preservation_digest: None,
        quarantine: None,
        removed_entries: 0,
        issues: vec![],
    };
    let review = CloseoutReview {
        id: ReviewId::new(),
        operation: record.operation,
        revision: 5,
        inventory_digest: None,
        preservation_digest: None,
        references_digest: "r".into(),
        work: CloseoutWorkReport {
            operation: record.operation,
            observed_at: "now".into(),
            findings: vec![],
        },
        issues: vec![],
        preservation_volume_ownership: None,
    };
    m.switch(Section::Closeout);
    m.closeouts.record = Some(record.clone());
    m.closeouts.review = Some(review.clone());
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('a'));
    assert_eq!(m.confirm.as_ref().unwrap().typed, Some("approve"));
    key(&mut m, KeyCode::Char('y'));
    assert!(
        wire.drain(&mut m).is_empty(),
        "'y' is typed text inside a typed review"
    );
    m.confirm.as_mut().unwrap().input.clear();
    type_text(&mut m, "approve");
    key(&mut m, KeyCode::Enter);
    let sent = wire.drain(&mut m);
    let (_, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Closeout { .. }));
    assert!(
        matches!(request, WorkspaceRequest::Closeout { request: CloseoutRequest::Execute { spec, .. } } if spec.expected_revision == 5 && spec.action == CloseoutAction::ApproveRemoval { review: review.id })
    );
}

#[test]
fn session_move_binds_session_and_catalog_revisions_and_staging_is_honest() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.switch(Section::Sessions);
    let target = LocationId::new();
    m.known.insert(
        target.to_string(),
        Entity::Location(Location {
            id: target,
            name: "b".into(),
            home: None,
            kind: LocationKind::Standalone { git: true },
            observed_path: "/synthetic/b".into(),
            volume_uuid: "U".into(),
            binding_generation: 1,
            lifecycle: LocationLifecycle::Ready,
            retired: false,
            revision: 1,
        }),
    );
    let view = SessionLocationView {
        session: "session_self".into(),
        location: Some(SessionLocationState {
            placement: Placement::Standalone(LocationId::new()),
            cwd: "/synthetic/a".into(),
            initial_cwd: "/synthetic/a".into(),
            revision: 4,
        }),
        legacy_working_dir: None,
        isolated_child: false,
        pending: vec![],
        catalog_revision: Some(9),
        catalog_issue: None,
    };
    m.sessions.views.insert("session_self".into(), view.clone());
    key(&mut m, KeyCode::Char('m'));
    assert!(m.form.is_none());
    assert!(
        m.status.contains("staged"),
        "rollout gate reported, not hidden"
    );
    m.caps.location_enabled = true;
    key(&mut m, KeyCode::Char('m'));
    let (kind, mut form) = m.form.clone().unwrap();
    form.set("placement", format!("standalone:{target}"));
    actions::build(&mut m, &kind, &form).unwrap();
    let Op::Location {
        command: PrimaryLocationCommand::Change { request },
        ..
    } = &m.confirm.as_ref().unwrap().op
    else {
        panic!()
    };
    assert_eq!(
        (
            request.expected_session_revision,
            request.expected_catalog_revision
        ),
        (4, 9)
    );
    assert_eq!(
        request.cwd,
        PathBuf::from("/synthetic/b"),
        "checkout/standalone target proposes its root"
    );
}

#[test]
fn proposal_approval_prefills_exact_audience_and_target() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.switch(Section::Permissions);
    let root = LocationId::new();
    let proposal = AccessProposal {
        id: ProposalId::new(),
        session: "session_x".into(),
        target: WriteTarget::Root(root),
        revision: 2,
        state: AccessProposalState::Pending,
        reason: "needs B".into(),
        grant: None,
    };
    m.perms.page = Some(PermissionPage {
        revision: 3,
        total: 1,
        items: vec![PermissionItem::Proposal(proposal.clone())],
        next: None,
    });
    m.perms.selected = Some(proposal.id.to_string());
    key(&mut m, KeyCode::Char('a'));
    let (kind, form) = m.form.clone().unwrap();
    assert_eq!(form.value("session"), "session_x");
    actions::build(&mut m, &kind, &form).unwrap();
    let sent = wire.drain(&mut m);
    let (_, request) = find(&sent, |r| {
        matches!(
            r,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::Review { .. }
            }
        )
    });
    assert!(
        matches!(request, WorkspaceRequest::Permissions { request: PermissionRequest::Review { change: GrantChange::Issue { audience: Audience::Session(s), target: WriteTarget::Root(t), proposal: Some(p) }, .. } } if s == "session_x" && *t == root && *p == proposal.id)
    );
}

#[test]
fn disconnected_runtime_offers_start_and_sends_nothing() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.switch(Section::Runtime);
    m.disconnected();
    assert!(wire.drain(&mut m).is_empty());
    assert!(matches!(
        m.take_intent(),
        Some(Intent::ServiceStatus) | Some(Intent::OfflineRuntime)
    ));
    while m.take_intent().is_some() {}
    m.accept_offline(OfflineRuntime {
        status: Some(RuntimeStatus {
            namespace: "ns".into(),
            runtime: None,
            reload_in_progress: false,
            desired_stopped: true,
            revision: 3,
            operation: None,
            work: vec![],
        }),
        detail: "durable".into(),
    });
    let text = frame(&mut m, 100, 30);
    assert!(text.contains("intentionally stopped"), "{text}");
    key(&mut m, KeyCode::Char('S'));
    assert_eq!(m.take_intent(), Some(Intent::StartRuntime));
}

#[test]
fn frames_are_usable_at_wide_standard_narrow_minimum_and_too_small() {
    let (mut m, mut wire) = ready(true);
    let sent = wire.drain(&mut m);
    let (id, _) = find(
        &sent,
        |r| matches!(r, WorkspaceRequest::List { query, .. } if query.kind.is_none()),
    );
    let p = project("Projekt 界 😀 with a very long name that must be truncated cleanly");
    m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Page(Page {
                revision: 3,
                total: 1,
                items: vec![p],
                next: None,
            }),
        ),
    );
    for (w, h) in [(120, 32), (80, 24), (60, 24), (48, 12)] {
        let text = frame(&mut m, w, h);
        assert!(text.contains("Workspace"), "{w}x{h}: header");
        assert!(text.contains("Projekt"), "{w}x{h}: row\n{text}");
        assert!(
            text.contains("q Close") || text.contains("q Clo") || text.contains(" q "),
            "{w}x{h}: footer keeps close reachable\n{text}"
        );
        actions::open(&mut m, actions::FormKind::CreateProject);
        let form = frame(&mut m, w, h);
        assert!(
            form.contains("New project") && form.contains("Review"),
            "{w}x{h}: form\n{form}"
        );
        m.form = None;
    }
    let tiny = frame(&mut m, 40, 10);
    assert!(tiny.contains("Workspace needs 48×12"));
    key(&mut m, KeyCode::Char('n'));
    assert!(m.form.is_none(), "too-small state accepts only close");
}

#[test]
fn mouse_selects_tabs_rows_and_footer_actions() {
    let (mut m, mut wire) = ready(true);
    wire.drain(&mut m);
    frame(&mut m, 120, 32);
    let (rect, _) = m
        .hit
        .iter()
        .find(|(_, hit)| *hit == Hit::Section(Section::Backup))
        .cloned()
        .unwrap();
    m.mouse(MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(m.section, Section::Backup);
    frame(&mut m, 120, 32);
    let (rect, _) = m
        .hit
        .iter()
        .find(|(_, hit)| *hit == Hit::Action(actions::Action::Backup))
        .cloned()
        .unwrap();
    m.mouse(MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(m.form.is_some(), "footer action opens its form");
    frame(&mut m, 120, 32);
    let (rect, _) = m
        .hit
        .iter()
        .find(|(_, hit)| *hit == Hit::FormCancel)
        .cloned()
        .unwrap();
    m.mouse(MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(m.form.is_none());
}

#[test]
fn drafts_survive_reconnect_and_resize() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('n'));
    type_text(&mut m, "Draft");
    frame(&mut m, 120, 32);
    m.reconnect("session_self");
    frame(&mut m, 60, 24);
    let (_, form) = m.form.as_ref().expect("draft kept");
    assert_eq!(form.value("name"), "Draft");
    assert!(form.dirty);
}

#[test]
fn local_client_reports_shared_runtime_requirement() {
    let mut m = WorkspaceManager::new("session_local".into(), false, Section::Organization);
    let mut wire = Wire::new();
    assert!(wire.drain(&mut m).is_empty());
    let text = frame(&mut m, 100, 24);
    assert!(text.contains("needs the shared runtime"), "{text}");
}

#[test]
fn rejected_or_declined_review_reopens_the_draft_and_conflict_refreshes_revision() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('n'));
    type_text(&mut m, "Acme");
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(m.form.is_none());
    let sent = wire.drain(&mut m);
    let (id, _) = find(&sent, |r| {
        matches!(
            r,
            WorkspaceRequest::Review {
                expected_revision: 3,
                ..
            }
        )
    });
    // Another writer advanced the catalog: the server refuses the stale review.
    let conflict = Issue {
        code: IssueCode::Conflict,
        detail: "Catalog changed: expected 3, current 9".into(),
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Error(conflict))));
    let (_, form) = m
        .form
        .as_ref()
        .expect("the draft returns instead of being lost");
    assert_eq!(form.fields[0].value, "Acme");
    assert!(
        form.error
            .as_deref()
            .is_some_and(|e| e.contains("Conflict"))
    );
    // The conflict refreshes catalog status; the resubmission uses it.
    let rest = answer_status(&mut m, &mut wire, Ok(status(9)));
    assert!(rest.is_empty(), "{rest:?}");
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let sent = wire.drain(&mut m);
    let (id, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Review { .. }));
    assert!(matches!(
        request,
        WorkspaceRequest::Review {
            expected_revision: 9,
            ..
        }
    ));
    let WorkspaceRequest::Review { change, .. } = request.clone() else {
        unreachable!()
    };
    let review = Review {
        id: ReviewId::new(),
        revision: 9,
        change,
        targets: vec![],
        issues: vec![],
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Review(review))));
    assert!(m.confirm.is_some());
    // Declining the review sends nothing and returns the same draft.
    key(&mut m, KeyCode::Char('n'));
    assert!(m.confirm.is_none());
    assert!(wire.drain(&mut m).is_empty());
    assert_eq!(
        m.form.as_ref().expect("draft reopened").1.fields[0].value,
        "Acme"
    );
    // Discarding the reopened draft still needs the dirty double-Esc.
    key(&mut m, KeyCode::Esc);
    key(&mut m, KeyCode::Esc);
    assert!(m.form.is_none() && m.draft.is_none());
}

#[test]
fn an_accepted_effect_settles_the_draft() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    key(&mut m, KeyCode::Char('n'));
    type_text(&mut m, "Acme");
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let sent = wire.drain(&mut m);
    let (id, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Review { .. }));
    let WorkspaceRequest::Review { change, .. } = request.clone() else {
        unreachable!()
    };
    let review = Review {
        id: ReviewId::new(),
        revision: 3,
        change,
        targets: vec![],
        issues: vec![],
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Review(review))));
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let (id, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Apply { .. }));
    let WorkspaceRequest::Apply {
        request: request_id,
        ..
    } = request.clone()
    else {
        unreachable!()
    };
    let receipt = Receipt {
        operation: OperationId::new(),
        request: request_id,
        revision: 4,
        targets: vec![],
        issues: vec![],
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Receipt(receipt))));
    assert!(m.draft.is_none() && m.form.is_none());
}

#[test]
fn interrupted_initialization_resumes_its_own_request_and_damage_offers_nothing() {
    let recorded = RequestId::new();
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Organization);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    answer_status(
        &mut m,
        &mut wire,
        Err(Issue::initialization_incomplete(recorded)),
    );
    assert!(frame(&mut m, 120, 32).contains(&recorded.to_string()));
    key(&mut m, KeyCode::Char('I'));
    key(&mut m, KeyCode::Char('y'));
    let sent = wire.drain(&mut m);
    let (_, request) = find(&sent, |r| matches!(r, WorkspaceRequest::Initialize { .. }));
    assert_eq!(
        request,
        &WorkspaceRequest::Initialize { request: recorded },
        "only the recorded request can finish initialization"
    );
    // Damaged state is reported but never offered a fresh initialization.
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Organization);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    let damaged = Issue {
        code: IssueCode::CorruptState,
        detail: "Workspace exists without installation identity".into(),
    };
    answer_status(&mut m, &mut wire, Err(damaged));
    assert!(!actions::available(&m).iter().any(|spec| spec.key == 'I'));
    key(&mut m, KeyCode::Char('I'));
    assert!(m.confirm.is_none() && wire.drain(&mut m).is_empty());
}

#[test]
fn directory_proposal_prefills_its_listed_choice_and_newer_pages_advance_the_review_revision() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    let docs = Location {
        id: LocationId::new(),
        name: "Docs".into(),
        home: None,
        kind: LocationKind::Directory,
        observed_path: "/synthetic/docs".into(),
        volume_uuid: "VOL".into(),
        binding_generation: 1,
        lifecycle: LocationLifecycle::Ready,
        retired: false,
        revision: 1,
    };
    m.known
        .insert(docs.id.to_string(), Entity::Location(docs.clone()));
    m.switch(Section::Permissions);
    let sent = wire.drain(&mut m);
    let proposal = AccessProposal {
        id: ProposalId::new(),
        session: "session_x".into(),
        target: WriteTarget::Root(docs.id),
        revision: 8,
        state: AccessProposalState::Pending,
        reason: String::new(),
        grant: None,
    };
    // Another client advanced the catalog; this list was read at revision 8.
    let (id, _) = find(&sent, |r| {
        matches!(
            r,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::List { .. }
            }
        )
    });
    let page = PermissionPage {
        revision: 8,
        total: 1,
        items: vec![PermissionItem::Proposal(proposal.clone())],
        next: None,
    };
    assert!(m.accept(
        id,
        workspace(
            id,
            WorkspaceResponse::Permissions(Box::new(PermissionResponse::Page(page)))
        )
    ));
    m.perms.selected = Some(proposal.id.to_string());
    key(&mut m, KeyCode::Char('a'));
    let (_, form) = m.form.clone().unwrap();
    let value = form.value("target").to_string();
    assert_eq!(
        value,
        format!("directory:{}", docs.id),
        "the prefill names a listed choice"
    );
    m.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let sent = wire.drain(&mut m);
    let (_, request) = find(&sent, |r| {
        matches!(
            r,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::Review { .. }
            }
        )
    });
    assert!(matches!(
        request,
        WorkspaceRequest::Permissions {
            request: PermissionRequest::Review {
                expected_revision: 8,
                ..
            }
        }
    ));
}

#[test]
fn organization_rows_group_by_kind_and_name_within_a_page() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    let repository = Entity::Repository(Repository {
        id: RepositoryId::new(),
        name: "tool".into(),
        remotes: vec![],
        state: OrganizationState::Active,
        revision: 1,
    });
    let items = vec![repository, project("beta"), project("Alpha")];
    m.org.page = Some(Page {
        revision: 3,
        total: 3,
        items,
        next: None,
    });
    let texts: Vec<String> = describe::rows(&m).into_iter().map(|row| row.text).collect();
    assert_eq!(texts, vec!["P  Alpha", "P  beta", "R  tool"]);
}

#[test]
fn large_closeout_inventories_page_and_a_refreshed_digest_starts_over() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.switch(Section::Closeout);
    wire.drain(&mut m);
    let operation = OperationId::new();
    let record = |digest: &str| CloseoutRecord {
        operation,
        request: RequestId::new(),
        spec: CloseoutSpec {
            location: LocationId::new(),
            expected_generation: 1,
            preservation_directory: None,
            conditional_no_loss: false,
            full_archive: false,
        },
        revision: 5,
        stage: CloseoutStage::NeedsDecision,
        initiated_by: "client".into(),
        inventory_digest: Some(digest.into()),
        inventory_entries: 450,
        preservation_directory: "/tmp/p".into(),
        preservation_volume_ownership: None,
        authorization: None,
        preservation_digest: None,
        quarantine: None,
        removed_entries: 0,
        issues: vec![],
    };
    let entry = |n: u64| CloseoutEntry {
        id: format!("e{n}"),
        path: if n == 0 {
            "".into()
        } else {
            format!("f{n}").into()
        },
        kind: CloseoutEntryKind::File,
        bytes: 1,
        sha256: None,
        link_target: None,
        links: 1,
        mode: 0o644,
        facts: vec![],
        blockers: vec![],
    };
    let page = |digest: &str, after: u64, limit: u64| CloseoutInventoryPage {
        operation,
        digest: digest.into(),
        total: 450,
        entries: (after..(after + limit).min(450)).map(entry).collect(),
        next: (after + limit < 450).then_some(after + limit),
    };
    m.closeouts.selected = Some(operation);
    m.closeouts.record = Some(record("d1"));
    let inventory = |m: &mut WorkspaceManager, wire: &mut Wire, digest: &str| -> Option<u64> {
        let sent = wire.drain(m);
        let (id, request) = sent.iter().find_map(|(id, r)| match r {
            Request::Workspace { request, .. } => match &**request {
                WorkspaceRequest::Closeout {
                    request:
                        CloseoutRequest::Inventory {
                            after, digest: d, ..
                        },
                } => Some((*id, (*after, d.clone()))),
                _ => None,
            },
            _ => None,
        })?;
        assert_eq!(request.1, digest);
        let response = CloseoutResponse::Inventory(page(digest, request.0, 200));
        assert!(m.accept(
            id,
            workspace(id, WorkspaceResponse::Closeout(Box::new(response)))
        ));
        Some(request.0)
    };
    key(&mut m, KeyCode::Char('i'));
    assert_eq!(inventory(&mut m, &mut wire, "d1"), Some(0));
    assert!(frame(&mut m, 120, 32).contains("(checkout root)"));
    key(&mut m, KeyCode::Char(']'));
    assert_eq!(inventory(&mut m, &mut wire, "d1"), Some(200));
    key(&mut m, KeyCode::Char(']'));
    assert_eq!(inventory(&mut m, &mut wire, "d1"), Some(400));
    assert!(frame(&mut m, 120, 32).contains("401–450 of 450"));
    key(&mut m, KeyCode::Char(']'));
    assert_eq!(
        inventory(&mut m, &mut wire, "d1"),
        None,
        "no page past the end"
    );
    key(&mut m, KeyCode::Char('['));
    assert_eq!(inventory(&mut m, &mut wire, "d1"), Some(200));
    // A refreshed inventory has a new digest; its first page is read again.
    let operation_record = CloseoutResponse::Record(Box::new(record("d2")));
    m.queue(Op::closeout(
        transport::View::CloseoutRecord,
        CloseoutRequest::Inspect { operation },
    ));
    let sent = wire.drain(&mut m);
    let (id, _) = sent
        .iter()
        .find(|(_, r)| matches!(r, Request::Workspace { request, .. } if matches!(**request, WorkspaceRequest::Closeout { request: CloseoutRequest::Inspect { .. } })))
        .unwrap();
    assert!(m.accept(
        *id,
        workspace(*id, WorkspaceResponse::Closeout(Box::new(operation_record)))
    ));
    assert_eq!(inventory(&mut m, &mut wire, "d2"), Some(0));
}

#[test]
fn known_entities_follow_every_page_and_a_stale_cursor_restarts_the_list() {
    let mut m = WorkspaceManager::new("session_self".into(), true, Section::Organization);
    let mut wire = Wire::new();
    answer_probes(&mut m, &mut wire, false);
    answer_status(&mut m, &mut wire, Ok(status(3)));
    // The first Known page for projects has a continuation; it is followed.
    let rest = wire.drain(&mut m);
    let (id, query) = rest
        .iter()
        .find_map(|(id, r)| match r {
            Request::Workspace { request, .. } => match &**request {
                WorkspaceRequest::List {
                    query,
                    after: None,
                    limit: 200,
                } if query.kind == Some(EntityKind::Project) => Some((*id, query.clone())),
                _ => None,
            },
            _ => None,
        })
        .expect("known projects read");
    let cursor = Cursor {
        revision: 3,
        after: "p".into(),
        query_digest: "q".into(),
    };
    let page = Page {
        revision: 3,
        total: 201,
        items: vec![project("first")],
        next: Some(cursor.clone()),
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Page(page))));
    let sent = wire.drain(&mut m);
    let (id, _) = find(
        &sent,
        |r| matches!(r, WorkspaceRequest::List { query: q, after: Some(c), .. } if *q == query && *c == cursor),
    );
    let last = project("second");
    let page = Page {
        revision: 3,
        total: 201,
        items: vec![last.clone()],
        next: None,
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Page(page))));
    assert!(
        m.known.contains_key(&last.id().to_string()),
        "entities past the first page are known"
    );
    assert!(wire.drain(&mut m).iter().all(|(_, r)| !matches!(r, Request::Workspace { request, .. } if matches!(&**request, WorkspaceRequest::List { query: q, .. } if *q == query))));
    // A continued organization page refused after a change restarts at page one.
    m.org.cursor = Some(cursor.clone());
    m.load_org();
    let sent = wire.drain(&mut m);
    let (id, _) = find(&sent, |r| {
        matches!(r, WorkspaceRequest::List { after: Some(_), .. })
    });
    let conflict = Issue {
        code: IssueCode::Conflict,
        detail:
            "List changed or continuation belongs to another query; refresh from the first page"
                .into(),
    };
    assert!(m.accept(id, workspace(id, WorkspaceResponse::Error(conflict))));
    assert!(m.org.cursor.is_none());
    // The first page is requested again (or is still in flight from the
    // initial load, which reads deduplicate).
    wire.drain(&mut m);
    assert!(m.pending.values().any(|pending| matches!(
        &pending.op,
        Op::Workspace {
            view: transport::View::OrgPage,
            request: WorkspaceRequest::List { after: None, .. }
        }
    )));
    assert!(m.status.contains("first page"));
}

#[test]
fn a_new_standalone_directory_is_its_own_root() {
    let (mut m, mut wire) = ready(true);
    wire.drain(&mut m);
    actions::open(&mut m, actions::FormKind::Launch);
    let (kind, mut form) = m.form.clone().unwrap();
    form.set("placement", "new-standalone");
    form.set("cwd_mode", "create");
    form.set("cwd", "/synthetic/fresh");
    form.set("root", "/synthetic");
    assert!(
        actions::build(&mut m, &kind, &form)
            .unwrap_err()
            .contains("standalone root itself")
    );
    form.set("root", "");
    actions::build(&mut m, &kind, &form).unwrap();
    let Some(confirm) = &m.confirm else {
        panic!("launch review")
    };
    let Op::Launch(request) = &confirm.op else {
        panic!("{:?}", confirm.op)
    };
    assert_eq!(
        request.input.placement,
        PrimaryPlacement::Standalone {
            root: "/synthetic/fresh".into()
        }
    );
    assert_eq!(
        request.input.cwd,
        Some(PrimaryCwd::CreateEmpty {
            path: "/synthetic/fresh".into(),
            home: None
        })
    );
    m.confirm = None;
    // An existing cwd needs an explicitly chosen root.
    form.set("cwd_mode", "existing");
    assert!(
        actions::build(&mut m, &kind, &form)
            .unwrap_err()
            .contains("standalone root")
    );
}

#[test]
fn wrapping_prefers_spaces_breaks_long_tokens_and_respects_cell_width() {
    let rows = view::wrap("Remap it explicitly /a/very/long/path/that/exceeds", 12);
    assert!(
        rows.iter()
            .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 12),
        "{rows:?}"
    );
    assert_eq!(rows[0], "Remap it");
    assert_eq!(
        rows.concat().replace(' ', ""),
        "Remapitexplicitly/a/very/long/path/that/exceeds"
    );
    let wide = view::wrap("日本語のパス名", 5);
    assert!(
        wide.iter()
            .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 5),
        "{wide:?}"
    );
    assert_eq!(wide.concat(), "日本語のパス名");
}

#[test]
fn a_long_refusal_is_fully_visible_below_the_form_fields() {
    let (mut m, mut wire) = ready(false);
    wire.drain(&mut m);
    m.switch(Section::Backup);
    actions::open(&mut m, actions::FormKind::Import);
    let detail = "Location 3a0c2118-de5e-40b1-b243-723fb04d32f2 already has a local physical identity. Remap it explicitly rather than duplicate ownership";
    m.form.as_mut().unwrap().1.error = Some(format!("Conflict: {detail}"));
    for (w, h) in [(120, 32), (80, 24), (60, 24), (48, 12)] {
        let text = frame(&mut m, w, h);
        for word in [
            "Conflict:",
            "Remap",
            "explicitly",
            "duplicate",
            "ownership",
            "Ctrl+S",
        ] {
            assert!(text.contains(word), "{w}x{h} lacks {word}:\n{text}");
        }
    }
}
