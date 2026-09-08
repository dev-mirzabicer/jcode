use super::*;
use ratatui::{Terminal, backend::TestBackend};

pub(super) fn reply(
    snapshot: &str,
    result: InstructionInspectionResult,
) -> InstructionInspectionReply {
    InstructionInspectionReply {
        session_id: "fixture".into(),
        snapshot: Some(snapshot.into()),
        result,
    }
}

pub(super) fn populated() -> InstructionManager {
    let mut manager = InstructionManager::new("fixture".into(), false);
    manager.reserve(1);
    let rows = vec![InstructionRow {
        description: String::new(),
        variants: Vec::new(),
        key: "resource-1".into(),
        id: "synthetic".into(),
        name: "Synthetic".into(),
        kind: "agent".into(),
        scope: "global".into(),
        repository: "global".into(),
        origin: InstructionOrigin::Managed,
        effective: true,
        redefines_global: false,
        high_impact: false,
        valid: true,
        warning: None,
    }];
    let snapshot = InstructionInspectionSnapshot {
        snapshot: "snapshot".into(),
        session_id: "fixture".into(),
        active_agent: Some("global:synthetic".into()),
        repositories: vec![InstructionRepositoryRow {
            key: "global".into(),
            kind: "global".into(),
            root: "/fixture".into(),
            branch: Some("main".into()),
            detached: false,
            health: "Ready".into(),
            dirty: false,
            conflicts: 0,
            active_lease: false,
        }],
        resources: InstructionRowsPage {
            offset: 0,
            total: 1,
            next: None,
            rows,
        },
    };
    assert!(manager.accept(
        1,
        reply("snapshot", InstructionInspectionResult::Opened(snapshot))
    ));
    // Complete the automatic overview request, as a real source-owning adapter does.
    manager.reserve(2);
    assert!(manager.accept(
        2,
        reply(
            "snapshot",
            InstructionInspectionResult::Text(InstructionTextPage {
                document: "initial-overview".into(),
                title: "Synthetic".into(),
                offset: 0,
                total_bytes: 8,
                next: None,
                text: "Overview".into()
            })
        )
    ));
    manager
}
pub(super) fn render(manager: &mut InstructionManager, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| manager.render(frame, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                + "\n"
        })
        .collect()
}
fn key(manager: &mut InstructionManager, ch: char) {
    assert!(manager.key(KeyCode::Char(ch), KeyModifiers::NONE));
}

#[test]
fn instruction_manager_correlates_session_snapshot_request_operation_and_offset() {
    let mut manager = populated();
    key(&mut manager, '1');
    manager.reserve(10);
    let page = InstructionTextPage {
        document: "doc".into(),
        title: "Source".into(),
        offset: 0,
        total_bytes: 6,
        next: Some(3),
        text: "ABC".into(),
    };
    assert!(!manager.accept(
        9,
        reply("snapshot", InstructionInspectionResult::Text(page.clone()))
    ));
    assert!(!manager.accept(
        10,
        reply("stale", InstructionInspectionResult::Text(page.clone()))
    ));
    let mut wrong_session = reply("snapshot", InstructionInspectionResult::Text(page.clone()));
    wrong_session.session_id = "other".into();
    assert!(!manager.accept(10, wrong_session));
    assert!(!manager.accept(10, reply("snapshot", InstructionInspectionResult::Closed)));
    assert!(manager.accept(
        10,
        reply("snapshot", InstructionInspectionResult::Text(page))
    ));
    key(&mut manager, 'n');
    let request = manager.reserve(11).unwrap();
    assert!(matches!(
        request,
        InstructionInspectionRequest::Text { offset: 3, .. }
    ));
    let next = InstructionTextPage {
        document: "doc".into(),
        title: "Source".into(),
        offset: 3,
        total_bytes: 6,
        next: None,
        text: "DEF".into(),
    };
    let mut stale = next.clone();
    stale.document = "other".into();
    assert!(!manager.accept(
        11,
        reply("snapshot", InstructionInspectionResult::Text(stale))
    ));
    assert!(manager.accept(
        11,
        reply("snapshot", InstructionInspectionResult::Text(next))
    ));
    key(&mut manager, 'p');
    assert!(matches!(
        manager.queued,
        Some(InstructionInspectionRequest::Text { offset: 0, .. })
    ));
    manager.refresh("other");
    assert!(!manager.accept(11, reply("snapshot", InstructionInspectionResult::Canceled)));
}

#[test]
fn instruction_manager_all_layouts_preserve_complete_scrolling_and_mouse_controls() {
    for (width, height) in [(160, 42), (80, 24), (40, 12), (24, 8)] {
        let mut manager = populated();
        let text = "界a".repeat(4_000) + "\nEND-SENTINEL";
        manager.text = Some(InstructionTextPage {
            document: "fixture-doc".into(),
            title: "Synthetic".into(),
            offset: 0,
            total_bytes: text.len(),
            next: None,
            text,
        });
        manager.pane = Pane::Detail;
        render(&mut manager, width, height);
        assert!(
            manager
                .controls
                .iter()
                .any(|(_, key)| *key == KeyCode::F(1))
        );
        assert!(
            manager
                .controls
                .iter()
                .any(|(_, key)| *key == KeyCode::Char(' '))
        );
        manager.key(KeyCode::End, KeyModifiers::NONE);
        assert!(render(&mut manager, width, height).contains("END-SENTINEL"));
        assert!(!manager.debug().to_string().contains("END-SENTINEL"));
        let rect = manager
            .controls
            .iter()
            .find(|(_, key)| *key == KeyCode::F(1))
            .unwrap()
            .0;
        manager.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(manager.pane, Pane::Repositories);
        manager.key(KeyCode::F(2), KeyModifiers::NONE);
        let cells = render(&mut manager, width, height);
        assert!(cells.contains("Synthetic"));
        for a in manager.areas {
            assert!(a.width == 0 || (a.right() <= width && a.bottom() <= height));
        }
        key(&mut manager, 'z');
        render(&mut manager, width, height);
        assert_eq!(
            manager.areas.iter().filter(|area| area.width > 0).count(),
            1
        );
    }
}

#[test]
fn instruction_manager_filters_history_cancel_and_render_only_fixture_are_structural() {
    let mut manager = populated();
    // Approved UX refinement replaces blind cycling with explicit choices.
    for ch in ['f', 's', 'o'] {
        key(&mut manager, ch);
        assert!(manager.menu.is_some());
        assert!(manager.queued.is_none());
        manager.key(KeyCode::Esc, KeyModifiers::NONE);
        if manager.menu.is_some() {
            manager.key(KeyCode::Esc, KeyModifiers::NONE);
        }
    }
    key(&mut manager, 'g');
    assert!(matches!(
        manager.queued,
        Some(InstructionInspectionRequest::Resources { offset: 0, .. })
    ));
    key(&mut manager, '/');
    for ch in "a界b".chars() {
        key(&mut manager, ch);
    }
    assert_eq!(manager.filter.search, "a界b");
    manager.key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(manager.filter.search, "a界");
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    key(&mut manager, 'c');
    assert_eq!(
        manager.filter,
        InstructionFilter {
            grouped: true,
            main_catalog: true,
            ..Default::default()
        }
    );
    let rows = manager.rows.clone();
    manager.reserve(4);
    assert!(manager.accept(
        4,
        reply(
            "snapshot",
            InstructionInspectionResult::Resources(InstructionRowsPage {
                offset: 0,
                total: rows.len(),
                next: None,
                rows
            })
        )
    ));
    key(&mut manager, '6');
    manager.reserve(5);
    let commits = vec![
        InstructionCommitRow {
            commit: "a".repeat(40),
            author: "Fixture".into(),
            date: "2026-09-06".into(),
            subject: "One".into(),
            paths: vec!["modules/a.md".into()],
        },
        InstructionCommitRow {
            commit: "b".repeat(40),
            author: "Fixture".into(),
            date: "2026-09-05".into(),
            subject: "Two".into(),
            paths: vec!["modules/a.md".into()],
        },
    ];
    assert!(manager.accept(
        5,
        reply(
            "snapshot",
            InstructionInspectionResult::History(InstructionHistoryPage {
                offset: 0,
                next: None,
                commits
            })
        )
    ));
    key(&mut manager, 'a');
    key(&mut manager, 'j');
    key(&mut manager, 'b');
    assert!(matches!(
        &manager.queued,
        Some(InstructionInspectionRequest::Detail {
            revision: Some(InstructionRevisionSelection { to: Some(_), .. }),
            ..
        })
    ));
    key(&mut manager, 'x');
    assert!(matches!(
        manager.queued,
        Some(InstructionInspectionRequest::Cancel)
    ));
    manager.render_only = true;
    assert!(manager.reserve(12).is_none());
    assert!(manager.pending.is_none());
}

#[test]
fn instruction_manager_explicit_visual_frames_capture_only_the_visible_viewport() {
    let _lock = crate::storage::lock_test_env();
    let previously_enabled = crate::tui::visual_debug::is_enabled();
    crate::tui::visual_debug::enable();
    let mut manager = populated();
    manager.pane = Pane::Detail;
    manager.text = Some(InstructionTextPage {
        document: "visual".into(),
        title: "Visible fixture".into(),
        offset: 0,
        total_bytes: 4,
        next: None,
        text: "BODY".into(),
    });
    let cells = render(&mut manager, 80, 24);
    let capture = crate::tui::visual_debug::latest_frame().unwrap();
    assert_eq!(capture.render_order, vec!["instruction_manager"]);
    assert_eq!(capture.state.scroll_offset, manager.scroll);
    assert_eq!(
        capture.rendered_text.recent_messages[0].content_preview,
        cells
    );
    assert!(!manager.debug().to_string().contains("BODY"));
    if !previously_enabled {
        crate::tui::visual_debug::disable();
    }
}

#[test]
fn instruction_manager_control_characters_are_visible_without_terminal_execution() {
    let mut manager = populated();
    manager.pane = Pane::Detail;
    let text = "A\u{001b}[31mB\rC".to_string();
    manager.text = Some(InstructionTextPage {
        document: "controls".into(),
        title: "Fixture".into(),
        offset: 0,
        total_bytes: text.len(),
        next: None,
        text: text.clone(),
    });
    let cells = render(&mut manager, 80, 24);
    assert!(cells.contains("\\u{001B}") && cells.contains("\\u{000D}"));
    assert!(!cells.contains('\u{001b}'));
    assert_eq!(manager.text.as_ref().unwrap().text, text);
}

#[test]
fn instruction_manager_search_from_detail_targets_the_new_result() {
    let mut manager = populated();
    key(&mut manager, '1');
    assert_eq!(manager.pane, Pane::Detail);
    key(&mut manager, '/');
    manager.paste("another");
    let request = manager.reserve(20).unwrap();
    assert!(matches!(
        request,
        InstructionInspectionRequest::Resources { .. }
    ));
    let mut row = manager.rows[0].clone();
    row.key = "another-key".into();
    row.id = "another".into();
    assert!(manager.accept(
        20,
        reply(
            "snapshot",
            InstructionInspectionResult::Resources(InstructionRowsPage {
                offset: 0,
                total: 1,
                next: None,
                rows: vec![row]
            })
        )
    ));
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    key(&mut manager, '1');
    assert!(
        matches!(&manager.queued, Some(InstructionInspectionRequest::Detail { target: InstructionInspectionTarget::Resource(key), .. }) if key == "another-key")
    );
}
