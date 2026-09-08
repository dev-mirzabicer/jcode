use super::tests::{populated, render, reply};
use super::*;

fn choose(manager: &mut InstructionManager, query: &str) {
    assert!(manager.menu.is_some());
    manager.paste(query);
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
}
fn accepted_source(manager: &mut InstructionManager, content: &str) {
    manager.detail(InstructionInspectionView::Source, None);
    manager.reserve(30);
    assert!(manager.accept(
        30,
        reply(
            "snapshot",
            InstructionInspectionResult::Text(InstructionTextPage {
                document: "source".into(),
                title: "Source title".into(),
                offset: 0,
                total_bytes: content.len(),
                next: None,
                text: content.into()
            })
        )
    ));
}
fn history(manager: &mut InstructionManager) {
    manager.detail(InstructionInspectionView::History, None);
    manager.reserve(31);
    let commits = (0..3)
        .map(|index| InstructionCommitRow {
            commit: index.to_string().repeat(40),
            author: "Synthetic author".into(),
            date: "2026-09-06".into(),
            subject: format!("Revision {index}"),
            paths: vec!["modules/source.md".into()],
        })
        .collect();
    assert!(manager.accept(
        31,
        reply(
            "snapshot",
            InstructionInspectionResult::History(InstructionHistoryPage {
                offset: 0,
                next: None,
                commits
            })
        )
    ));
}

#[test]
fn ux_explicit_filters_show_choices_and_preserve_other_filters() {
    let mut manager = populated();
    manager.filter.search = "retained search".into();
    manager.key(KeyCode::Char('f'), KeyModifiers::NONE);
    choose(&mut manager, "Global / project scope");
    assert_eq!(manager.menu.as_ref().unwrap().title, "Source scope");
    choose(&mut manager, "project");
    assert!(manager.menu.is_none());
    assert_eq!(manager.filter.scope.as_deref(), Some("project"));
    assert_eq!(manager.filter.search, "retained search");
    assert!(manager.filter_summary().contains("project"));
    assert!(
        matches!(&manager.queued,Some(InstructionInspectionRequest::Resources{filter,..}) if filter==&manager.filter)
    );
}

#[test]
fn ux_action_menu_is_contextual_and_refuses_selection_races() {
    let mut manager = populated();
    manager.rows[0].kind = "module".into();
    manager.key(KeyCode::Char(' '), KeyModifiers::NONE);
    let system = manager
        .menu
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|item| item.label == "System prompt")
        .unwrap();
    assert!(system.disabled.is_some());
    let text = render(&mut manager, 80, 24);
    assert!(text.contains("Actions") && text.contains("Find:"));
    assert!(manager.areas.iter().all(|rect| rect.width == 0));
    assert!(manager.list_hits.is_empty());
    choose(&mut manager, "System prompt");
    assert!(manager.menu.is_some());
    assert!(manager.queued.is_none());
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    manager.open_actions(false);
    manager.rows[0].key = "changed-selection".into();
    choose(&mut manager, "Overview");
    assert!(manager.menu.is_none());
    assert!(manager.queued.is_none());
    assert!(manager.status.contains("selection changed"));
}

#[test]
fn ux_back_preserves_history_base_selection_and_cached_reader() {
    let mut manager = populated();
    accepted_source(&mut manager, "one\ntwo\nthree\nfour\nfive\nsix");
    render(&mut manager, 24, 8);
    manager.key(KeyCode::End, KeyModifiers::NONE);
    let scroll = manager.scroll;
    assert!(scroll > 0);
    manager.key(KeyCode::Char('?'), KeyModifiers::NONE);
    render(&mut manager, 24, 8);
    manager.key(KeyCode::Down, KeyModifiers::NONE);
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(manager.scroll, scroll);
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(manager.pane, Pane::Resources);
    assert!(manager.visible);
    assert!(manager.queued.is_none());
    manager.key(KeyCode::F(3), KeyModifiers::NONE);
    assert_eq!(manager.scroll, scroll);
    history(&mut manager);
    manager.key(KeyCode::Char('a'), KeyModifiers::NONE);
    manager.key(KeyCode::Down, KeyModifiers::NONE);
    manager.key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(manager.revision_open);
    assert_eq!(manager.view_label(), "Revision comparison");
    manager.reserve(32);
    assert!(manager.accept(
        32,
        reply(
            "snapshot",
            InstructionInspectionResult::Text(InstructionTextPage {
                document: "diff".into(),
                title: "Diff".into(),
                offset: 0,
                total_bytes: 4,
                next: None,
                text: "DIFF".into()
            })
        )
    ));
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(manager.history_visible);
    assert_eq!(manager.history_selected, 1);
    assert_eq!(
        manager.history_base.as_deref(),
        Some("0000000000000000000000000000000000000000")
    );
    manager.key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(manager.view_label(), "Commit details");
    assert!(matches!(
        &manager.queued,
        Some(InstructionInspectionRequest::Detail {
            view: InstructionInspectionView::Metadata,
            revision: Some(_),
            ..
        })
    ));
}

#[test]
fn ux_sticky_identity_menus_and_hit_regions_work_at_every_floor() {
    let mut manager = populated();
    let content = "界 e\u{301} 🙂\n".repeat(100) + "LAST LINE";
    accepted_source(&mut manager, &content);
    for (width, height) in [
        (160, 42),
        (100, 30),
        (80, 24),
        (60, 24),
        (40, 12),
        (24, 8),
        (24, 12),
    ] {
        render(&mut manager, width, height);
        manager.key(KeyCode::End, KeyModifiers::NONE);
        let text = render(&mut manager, width, height);
        assert!(
            text.contains("global:synthetic"),
            "{width}x{height}\n{text}"
        );
        assert!(text.contains("LAST LINE"), "{width}x{height}\n{text}");
        assert!(text.contains("Source"));
        assert!(
            manager
                .controls
                .iter()
                .any(|(_, key)| *key == KeyCode::Char(' '))
        );
        manager.key(KeyCode::Char(' '), KeyModifiers::NONE);
        render(&mut manager, width, height);
        assert!(!manager.menu_hits.is_empty());
        assert!(manager.controls.iter().any(|(_, key)| *key == KeyCode::Esc));
        let index = manager
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|item| item.label == "Source")
            .unwrap();
        manager.menu.as_mut().unwrap().selected = index;
        render(&mut manager, width, height);
        let rect = manager
            .menu_hits
            .iter()
            .find(|(_, i)| *i == index)
            .unwrap()
            .0;
        manager.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(manager.menu.is_none());
        assert!(matches!(
            manager.queued,
            Some(InstructionInspectionRequest::Detail { .. })
        ));
        manager.queued = None;
        manager.pending = None;
        accepted_source(&mut manager, &content);
    }
    manager.open_actions(false);
    let too_small = render(&mut manager, 23, 7);
    assert!(too_small.contains("Need 24"));
    assert!(manager.controls.is_empty() && manager.menu_hits.is_empty());
    manager.key(KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(!manager.visible);
}

#[test]
fn ux_search_cursor_edits_utf8_and_paste_without_triggering_actions() {
    let mut manager = populated();
    manager.key(KeyCode::Char('/'), KeyModifiers::NONE);
    manager.paste("a界b");
    manager.key(KeyCode::Left, KeyModifiers::NONE);
    manager.key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(manager.filter.search, "ab");
    manager.paste("🙂");
    assert_eq!(manager.filter.search, "a🙂b");
    manager.key(KeyCode::Home, KeyModifiers::NONE);
    manager.key(KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(manager.filter.search, "🙂b");
    manager.key(KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(manager.visible);
    assert_eq!(manager.filter.search, "q🙂b");
}

#[test]
fn ux_monochrome_focus_and_clutter_remain_semantic() {
    let _lock = crate::storage::lock_test_env();
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::env::set_var("NO_COLOR", value),
                None => crate::env::remove_var("NO_COLOR"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("NO_COLOR"));
    crate::env::set_var("NO_COLOR", "1");
    let mut manager = populated();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| manager.render(frame, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rect = manager.list_hits[0].0;
    assert!(
        buffer[(rect.x, rect.y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    for cell in &buffer.content {
        assert_eq!(cell.fg, ratatui::style::Color::Reset);
    }
    let text = render(&mut manager, 80, 24);
    assert!(!text.contains("●") && !text.contains("○"));
}

#[test]
fn ux_render_gallery_and_complete_graphemes() {
    let body="Source identity stays visible while reading.\n\nFamily 👨‍👩‍👧‍👦, combining e\u{301}, and CJK 界 remain whole.\n".repeat(6)+"END";
    for (width, height) in [
        (160, 40),
        (100, 30),
        (80, 24),
        (60, 24),
        (40, 12),
        (24, 8),
        (24, 12),
    ] {
        let mut manager = populated();
        accepted_source(&mut manager, &body);
        let reading = render(&mut manager, width, height);
        let joined = manager.wrapped.join("");
        assert!(joined.contains("👨‍👩‍👧‍👦"));
        assert!(manager.wrapped.iter().any(|line| line.contains("👨‍👩‍👧‍👦")));
        if let Some(root) = std::env::var_os("JCODE_MANAGER_FRAME_DIR") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(format!("reading-{width}x{height}.txt")), reading).unwrap();
            manager.open_actions(false);
            std::fs::write(
                root.join(format!("actions-{width}x{height}.txt")),
                render(&mut manager, width, height),
            )
            .unwrap();
            manager.open_filters(FilterField::Scope);
            std::fs::write(
                root.join(format!("filters-{width}x{height}.txt")),
                render(&mut manager, width, height),
            )
            .unwrap();
        }
    }
}

#[test]
fn ux_menu_revision_actions_reject_a_changed_history_cursor() {
    let mut manager = populated();
    history(&mut manager);
    manager.open_actions(false);
    manager.paste("Commit details");
    manager.history_selected = 2;
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(manager.menu.is_none());
    assert!(manager.queued.is_none());
    assert!(manager.status.contains("History selection changed"));
}

#[test]
fn ux_narrow_action_explanations_are_complete_and_scrollable() {
    let mut manager = populated();
    manager.rows[0].kind = "module".into();
    manager.open_actions(false);
    choose(&mut manager, "System prompt");
    assert!(manager.menu.as_ref().unwrap().explanation);
    let first = render(&mut manager, 24, 8);
    assert!(first.contains("System prompt"));
    manager.key(KeyCode::End, KeyModifiers::NONE);
    let last = render(&mut manager, 24, 8);
    assert!(last.contains("prompt"));
    assert!(manager.queued.is_none());
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!manager.menu.as_ref().unwrap().explanation);
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(manager.menu.is_none());
}

#[test]
fn ux_types_scopes_and_secondary_pages_change_real_navigation_without_editing_destinations() {
    let mut manager = super::tests::populated();
    manager.queued = None;
    manager.key(KeyCode::F(1), KeyModifiers::NONE);
    manager.key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(manager.category_label(), "Skills");
    assert_eq!(manager.filter.kind.as_deref(), Some("skill"));
    assert!(manager.filter.grouped);
    assert_eq!(manager.pane, Pane::Repositories);
    manager.choose_scope(Some("project".into()));
    assert_eq!(manager.scope_label(), "Project");
    assert_eq!(manager.filter.scope.as_deref(), Some("project"));
    assert!(manager.editing.queued.is_none());
    manager.choose_navigation(navigation::REPOSITORIES);
    assert_eq!(manager.category_label(), "Repositories & sync");
    assert_eq!(
        manager.selected_target(),
        Some(InstructionInspectionTarget::Repository("global".into()))
    );
    manager.choose_navigation(navigation::SESSION);
    assert_eq!(
        manager.selected_target(),
        Some(InstructionInspectionTarget::Session)
    );
    assert!(manager.editing.queued.is_none());
}

#[test]
fn ux_names_scope_external_badges_and_setup_are_visible_without_selection() {
    let mut manager = super::tests::populated();
    manager.queued = None;
    manager.filter.kind = Some("skill".into());
    manager.rows[0].id = "SKILL.md".into();
    manager.rows[0].name = "frontend-fixture".into();
    manager.rows[0].kind = "skill".into();
    manager.rows[0].origin = InstructionOrigin::External;
    manager.rows[0].description = "Design synthetic interfaces".into();
    for (width, height) in [(150, 40), (80, 24), (60, 24), (24, 10)] {
        let screen = super::tests::render(&mut manager, width, height);
        assert!(!screen.contains("READ ONLY"));
        assert!(!screen.contains("SKILL.md"));
        assert!(screen.contains("[G ext]"));
        assert!(screen.contains("frontend"));
        assert!(
            manager
                .controls
                .iter()
                .any(|(_, key)| *key == KeyCode::F(4)),
            "Project setup must have a visible hit region at {width}"
        );
    }
}

#[test]
fn ux_source_copy_destination_collisions_are_explicit_and_selection_correlated() {
    let mut manager = super::tests::populated();
    manager.queued = None;
    manager.rows[0].variants = vec![
        InstructionSourceVariant {
            key: "global-source".into(),
            name: "Synthetic".into(),
            scope: "global".into(),
            repository: "global".into(),
            origin: InstructionOrigin::Managed,
            path: "/global/agent.md".into(),
            effective: false,
            valid: true,
        },
        InstructionSourceVariant {
            key: "project-source".into(),
            name: "Synthetic project".into(),
            scope: "project".into(),
            repository: "project".into(),
            origin: InstructionOrigin::Managed,
            path: "/project/agent.md".into(),
            effective: true,
            valid: true,
        },
    ];
    manager.open_sources();
    let menu = manager.menu.as_ref().unwrap();
    assert!(menu.items.iter().any(|item| item.label == "Edit project"));
    assert!(
        menu.items
            .iter()
            .any(|item| item.label == "Copy to project and edit" && item.disabled.is_some())
    );
    let index = menu
        .items
        .iter()
        .position(|item| item.label == "Edit project")
        .unwrap();
    manager.menu.as_mut().unwrap().selected = index;
    manager.rows[0].key = "changed".into();
    manager.choose_menu();
    assert!(manager.editing.queued.is_none());
    assert!(manager.status.contains("changed"));
}
