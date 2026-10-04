use super::*;
use std::path::PathBuf;

fn proposal(broad: bool, default: Option<usize>) -> PlacementProposal {
    PlacementProposal {
        session: "session_fixture".into(),
        working_dir: PathBuf::from("/fixture/repo/src"),
        catalog_revision: 7,
        candidates: vec![
            PlacementCandidate {
                placement: PrimaryPlacement::Standalone {
                    root: PathBuf::from("/fixture/repo"),
                },
                root: PathBuf::from("/fixture/repo"),
                name: "repo".into(),
                project: None,
                broad,
            },
            PlacementCandidate {
                placement: PrimaryPlacement::Existing {
                    placement: Placement::Project(ProjectId::new()),
                },
                root: PathBuf::from("/fixture/repo"),
                name: "Alpha".into(),
                project: Some("Alpha".into()),
                broad: false,
            },
        ],
        default,
    }
}

fn reply(id: u64, response: PrimaryLocationResponse) -> ServerEvent {
    ServerEvent::PrimaryLocationResponse {
        id,
        response: Box::new(response),
    }
}

fn proposed(review: &mut PlacementReview, proposal: PlacementProposal) {
    let Some(Request::PrimaryLocation { id, command }) = review.reserve(10) else {
        panic!("proposal request")
    };
    assert_eq!(
        *command,
        PrimaryLocationCommand::ProposePlacement {
            session: "session_fixture".into()
        }
    );
    assert_eq!(
        review.accept(reply(
            id,
            PrimaryLocationResponse::Proposal {
                proposal: Box::new(proposal)
            }
        )),
        Outcome::None
    );
}

fn complete(request: &SessionPlacementRequest) -> PrimaryLocationResponse {
    PrimaryLocationResponse::State {
        record: Box::new(LocationChangeRecord {
            operation: request.request.to_string().parse().unwrap(),
            input: LocationChangeRequest {
                request: request.request,
                session: request.session.clone(),
                expected_session_revision: 0,
                expected_catalog_revision: request.expected_catalog_revision,
                placement: Placement::Standalone(LocationId::new()),
                cwd: request.working_dir.clone(),
            },
            state: LocationChangeState::Complete,
            effective_revision: Some(1),
            notice_message: None,
            issue: None,
            legacy_origin: None,
        }),
    }
}

#[test]
fn enter_places_the_proposed_default_and_reports_completion() {
    let mut review = PlacementReview::open("session_fixture".into(), true);
    proposed(&mut review, proposal(false, Some(0)));
    assert_eq!(review.stage, Stage::Choosing);
    assert_eq!(
        review.key(KeyCode::Enter, KeyModifiers::empty()),
        Outcome::None
    );
    assert_eq!(review.stage, Stage::Placing);
    let Some(Request::PrimaryLocation { id, command }) = review.reserve(11) else {
        panic!("place request")
    };
    let PrimaryLocationCommand::Place { request } = *command else {
        panic!("place")
    };
    assert_eq!(request.working_dir, PathBuf::from("/fixture/repo/src"));
    assert_eq!(request.expected_catalog_revision, 7);
    assert_eq!(
        request.placement,
        PrimaryPlacement::Standalone {
            root: PathBuf::from("/fixture/repo")
        }
    );
    // One request at a time: nothing else is sent while it is in flight.
    assert!(review.reserve(12).is_none());
    match review.accept(reply(id, complete(&request))) {
        Outcome::Placed { summary } => assert!(summary.contains("repo"), "{summary}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_broad_root_is_never_placed_by_a_single_keypress() {
    let mut review = PlacementReview::open("session_fixture".into(), false);
    proposed(&mut review, proposal(true, None));
    assert_eq!(review.selected, 0);
    review.key(KeyCode::Enter, KeyModifiers::empty());
    assert_eq!(review.stage, Stage::Choosing, "first Enter only arms");
    assert!(review.reserve(11).is_none());
    // Moving away disarms; the second candidate is ordinary.
    review.key(KeyCode::Down, KeyModifiers::empty());
    review.key(KeyCode::Up, KeyModifiers::empty());
    review.key(KeyCode::Enter, KeyModifiers::empty());
    assert_eq!(review.stage, Stage::Choosing);
    review.key(KeyCode::Enter, KeyModifiers::empty());
    assert_eq!(review.stage, Stage::Placing);
}

#[test]
fn a_stale_placement_is_reviewed_again_once() {
    let mut review = PlacementReview::open("session_fixture".into(), true);
    proposed(&mut review, proposal(false, Some(0)));
    review.key(KeyCode::Enter, KeyModifiers::empty());
    let Some(Request::PrimaryLocation { id, .. }) = review.reserve(11) else {
        panic!("place request")
    };
    let conflict = || PrimaryLocationResponse::Rejected {
        issue: Issue {
            code: IssueCode::Conflict,
            detail: "Workspace changed since this placement review; review it again".into(),
        },
    };
    review.accept(reply(id, conflict()));
    assert_eq!(review.stage, Stage::Loading);
    proposed(&mut review, proposal(false, Some(0)));
    review.key(KeyCode::Enter, KeyModifiers::empty());
    let Some(Request::PrimaryLocation { id, .. }) = review.reserve(12) else {
        panic!("second place request")
    };
    review.accept(reply(id, conflict()));
    assert!(matches!(review.stage, Stage::Failed(_)));
}

#[test]
fn an_already_placed_session_needs_no_review() {
    let mut review = PlacementReview::open("session_fixture".into(), true);
    let Some(Request::PrimaryLocation { id, .. }) = review.reserve(10) else {
        panic!("proposal request")
    };
    let outcome = review.accept(reply(
        id,
        PrimaryLocationResponse::Rejected {
            issue: Issue {
                code: IssueCode::Conflict,
                detail: "This session is already placed; use a location change instead".into(),
            },
        },
    ));
    assert!(matches!(outcome, Outcome::Placed { .. }));
}

#[test]
fn an_uncertain_placement_is_never_resent() {
    let mut review = PlacementReview::open("session_fixture".into(), true);
    proposed(&mut review, proposal(false, Some(0)));
    review.key(KeyCode::Enter, KeyModifiers::empty());
    assert!(review.reserve(11).is_some());
    review.transport_failed("broken pipe");
    assert!(matches!(review.stage, Stage::Failed(_)));
    assert!(review.reserve(12).is_none(), "no automatic resend");
    review.key(KeyCode::Char('r'), KeyModifiers::empty());
    let Some(Request::PrimaryLocation { command, .. }) = review.reserve(13) else {
        panic!("fresh review")
    };
    assert!(matches!(
        *command,
        PrimaryLocationCommand::ProposePlacement { .. }
    ));
}

#[test]
fn escape_and_workspace_keys_close_without_effects() {
    let mut review = PlacementReview::open("session_fixture".into(), true);
    proposed(&mut review, proposal(false, Some(0)));
    assert_eq!(
        review.key(KeyCode::Esc, KeyModifiers::empty()),
        Outcome::Close
    );
    assert_eq!(
        review.key(KeyCode::Char('w'), KeyModifiers::empty()),
        Outcome::OpenWorkspace
    );
    assert!(review.reserve(11).is_none());
}

#[test]
fn the_dialog_renders_at_supported_sizes() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut review = PlacementReview::open("session_fixture".into(), true);
    proposed(&mut review, proposal(true, None));
    for (width, height) in [(120, 32), (80, 24), (48, 12)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| review.render(frame, frame.area()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Place this session"), "{width}x{height}");
        assert!(text.contains("repo"), "{width}x{height}");
    }
}
