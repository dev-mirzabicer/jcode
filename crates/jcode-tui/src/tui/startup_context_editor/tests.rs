use super::*;

fn ready_editor() -> StartupContextEditor {
    StartupContextEditor::debug_fixture("editor-populated", "session".to_string(), Vec::new())
}

fn receipt() -> StartupContextFileReceiptSnapshot {
    use crate::protocol::{StartupContextBatchKind, StartupContextDeliveryState};
    StartupContextFileReceiptSnapshot {
        batch_id: "batch".to_string(),
        batch_kind: StartupContextBatchKind::Initial,
        delivery_state: StartupContextDeliveryState::ProviderAccepted,
        spec_id: "spec".to_string(),
        message_id: "message".to_string(),
        ordinal: 1,
        logical_path: "docs/PLAN.md".to_string(),
        resolved_path: "/project/docs/PLAN.md".to_string(),
        classification: StartupContextPathClassification::Project,
        sha256: "0123456789abcdef".repeat(4),
        bytes: 12,
        estimated_tokens: 3,
        latest_observation: StartupContextObservedState::Current,
        notification_count: 0,
    }
}

#[test]
fn draft_order_uses_stable_local_identity() {
    let mut editor = StartupContextEditor::new("session".to_string(), None);
    editor.phase = EditorPhase::Ready;
    editor.draft = vec![
        DraftEntry::pending(10, "a.md".to_string()),
        DraftEntry::pending(20, "b.md".to_string()),
    ];
    editor.draft_cursor = 0;
    editor.move_draft(1);
    assert_eq!(editor.draft[0].local_id, 20);
    assert_eq!(editor.draft[1].local_id, 10);
    assert_eq!(editor.draft_cursor, 1);
}

#[test]
fn exact_duplicate_paths_are_ignored_without_destroying_draft() {
    let mut editor = StartupContextEditor::new("session".to_string(), None);
    editor.phase = EditorPhase::Ready;
    editor.draft = vec![DraftEntry::pending(10, "a.md".to_string())];
    editor.add_paths(["a.md".to_string(), "b.md".to_string()]);
    assert_eq!(editor.draft.len(), 2);
    assert_eq!(editor.draft[0].local_id, 10);
    assert!(editor.notice.as_deref().unwrap().contains("duplicate"));
}

#[test]
fn narrow_back_unwinds_search_before_closing() {
    let mut editor = StartupContextEditor::new("session".to_string(), None);
    editor.phase = EditorPhase::Ready;
    editor.browser.search_query = Some("plan".to_string());
    assert!(!editor.handle_key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(editor.visible);
    assert!(editor.browser.search_query.is_none());
    assert!(editor.handle_key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!editor.visible);
}

#[test]
fn directory_bulk_selection_uses_every_direct_non_directory_child_in_server_order() {
    let mut editor = ready_editor();
    editor.browser.cursor = 0;
    editor.select_current_browser_entry();
    let action = editor.take_action().expect("bulk directory action");
    let (lease_id, directory, generation) = match action {
        StartupContextEditorAction::ListDirectory {
            lease,
            directory,
            page_start: 0,
            generation,
            bulk: true,
        } => (lease.lease_id, directory, generation),
        other => panic!("unexpected action: {other:?}"),
    };
    editor.register_pending(
        10,
        StartupContextPendingRequest::Directory {
            lease_id,
            directory: directory.clone(),
            page_start: 0,
            generation,
            bulk: true,
        },
    );
    assert!(editor.accept_directory(
        10,
        StartupContextDirectoryPage {
            project_key_digest: "fixture-project".to_string(),
            plan_revision: 7,
            directory,
            total_entries: 3,
            page_start: 0,
            page_end: 3,
            next_page_start: None,
            entries: vec![
                StartupContextDirectoryEntry {
                    name: "01-first.md".to_string(),
                    project_relative_path: "docs/01-first.md".to_string(),
                    resolved_path: "/fixture/project/docs/01-first.md".to_string(),
                    path_valid_utf8: true,
                    kind: StartupContextDirectoryEntryKind::File,
                    classification: StartupContextPathClassification::Project,
                    navigable: false,
                    bytes: Some(1),
                    selected_spec_id: None,
                },
                StartupContextDirectoryEntry {
                    name: "nested".to_string(),
                    project_relative_path: "docs/nested".to_string(),
                    resolved_path: "/fixture/project/docs/nested".to_string(),
                    path_valid_utf8: true,
                    kind: StartupContextDirectoryEntryKind::Directory,
                    classification: StartupContextPathClassification::Project,
                    navigable: true,
                    bytes: None,
                    selected_spec_id: None,
                },
                StartupContextDirectoryEntry {
                    name: "02-special".to_string(),
                    project_relative_path: "docs/02-special".to_string(),
                    resolved_path: "/fixture/project/docs/02-special".to_string(),
                    path_valid_utf8: true,
                    kind: StartupContextDirectoryEntryKind::Other,
                    classification: StartupContextPathClassification::Project,
                    navigable: false,
                    bytes: None,
                    selected_spec_id: None,
                },
            ],
        }
    ));
    let paths = editor
        .draft
        .iter()
        .map(|entry| entry.input.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.ends_with(&["docs/01-first.md", "docs/02-special"]));
    assert!(!paths.contains(&"docs/nested"));
}

#[test]
fn normalized_duplicate_preview_drops_only_duplicate_and_preserves_stable_identity() {
    let mut editor = ready_editor();
    editor.draft = vec![
        DraftEntry::pending(41, "docs/a.md".to_string()),
        DraftEntry::pending(42, "docs/link-to-a.md".to_string()),
    ];
    editor.selection_generation = 8;
    let lease_id = editor.lease().unwrap().lease_id.clone();
    editor.register_pending(
        20,
        StartupContextPendingRequest::Selection {
            lease_id,
            generation: 8,
        },
    );
    assert!(editor.accept_selection(
        20,
        StartupContextSelectionPreview {
            project_key_digest: "fixture-project".to_string(),
            plan_revision: 7,
            entry_count: 2,
            selected_count: 1,
            issue_count: 1,
            aggregate_bytes: 4,
            aggregate_estimated_tokens: 1,
            entries: vec![
                StartupContextSelectionEntrySnapshot::Selected {
                    input_index: 0,
                    spec_id: "normalized-a".to_string(),
                    logical_path: "docs/a.md".to_string(),
                    resolved_path: "/fixture/project/docs/a.md".to_string(),
                    classification: StartupContextPathClassification::Project,
                    bytes: 4,
                    estimated_tokens: 1,
                    requires_external_approval: false,
                },
                StartupContextSelectionEntrySnapshot::Issue {
                    issue: StartupContextFileIssueSnapshot {
                        input_index: Some(1),
                        spec_id: None,
                        logical_path: Some("docs/link-to-a.md".to_string()),
                        kind: StartupContextFileIssueKind::DuplicateSelection {
                            first_input_index: 0,
                        },
                    },
                },
            ],
            batch_issues: Vec::new(),
        }
    ));
    assert_eq!(editor.draft.len(), 1);
    assert_eq!(editor.draft[0].local_id, 41);
    assert_eq!(
        editor.draft[0].normalized_spec_id.as_deref(),
        Some("normalized-a")
    );
    assert!(
        editor
            .notice
            .as_deref()
            .unwrap()
            .contains("Ignored 1 duplicate")
    );
}

#[test]
fn stale_directory_preview_and_selection_responses_are_rejected() {
    let mut editor = ready_editor();
    let lease_id = editor.lease().unwrap().lease_id.clone();
    editor.browser.generation = 4;
    editor.register_pending(
        30,
        StartupContextPendingRequest::Directory {
            lease_id: lease_id.clone(),
            directory: String::new(),
            page_start: 0,
            generation: 3,
            bulk: false,
        },
    );
    assert!(!editor.accept_directory(
        30,
        StartupContextDirectoryPage {
            project_key_digest: "fixture-project".to_string(),
            plan_revision: 7,
            directory: String::new(),
            total_entries: 0,
            page_start: 0,
            page_end: 0,
            next_page_start: None,
            entries: Vec::new(),
        }
    ));
    editor.preview_generation = 9;
    editor.preview.begin_current("new.md".to_string(), 9);
    editor.register_pending(
        31,
        StartupContextPendingRequest::Preview {
            lease_id,
            path: "old.md".to_string(),
            start_char: 0,
            generation: 8,
        },
    );
    assert!(!editor.accept_preview(
        31,
        StartupContextFilePreview {
            project_key_digest: "fixture-project".to_string(),
            plan_revision: 7,
            logical_path: "old.md".to_string(),
            resolved_path: "/fixture/project/old.md".to_string(),
            classification: StartupContextPathClassification::Project,
            requires_external_approval: false,
            sha256: "0".repeat(64),
            bytes: 3,
            estimated_tokens: 1,
            total_chars: 3,
            start_char: 0,
            end_char: 3,
            next_start_char: None,
            truncated: false,
            content: "old".to_string(),
        }
    ));
    assert_eq!(editor.preview.path.as_deref(), Some("new.md"));
    assert!(editor.preview.content.is_empty());
}

#[test]
fn exact_receipt_detail_reconstructs_unicode_chunks_without_eager_loading() {
    let mut editor = ready_editor();
    editor.receipt = vec![receipt()];
    editor.receipt_cursor = 0;
    editor.preview_generation = 5;
    editor.preview.begin_receipt(&editor.receipt[0], 5);
    editor.register_pending(
        40,
        StartupContextPendingRequest::Detail {
            batch_id: "batch".to_string(),
            spec_id: "spec".to_string(),
            start_char: 0,
            generation: 5,
        },
    );
    assert!(editor.accept_detail(
        40,
        StartupContextFileDetail {
            session_id: "session".to_string(),
            batch_id: "batch".to_string(),
            spec_id: "spec".to_string(),
            message_id: "message".to_string(),
            sha256: "0123456789abcdef".repeat(4),
            total_chars: 4,
            start_char: 0,
            end_char: 2,
            next_start_char: Some(2),
            content: "αβ".to_string(),
        }
    ));
    editor.register_pending(
        41,
        StartupContextPendingRequest::Detail {
            batch_id: "batch".to_string(),
            spec_id: "spec".to_string(),
            start_char: 2,
            generation: 5,
        },
    );
    assert!(editor.accept_detail(
        41,
        StartupContextFileDetail {
            session_id: "session".to_string(),
            batch_id: "batch".to_string(),
            spec_id: "spec".to_string(),
            message_id: "message".to_string(),
            sha256: "0123456789abcdef".repeat(4),
            total_chars: 4,
            start_char: 2,
            end_char: 4,
            next_start_char: None,
            content: "γδ".to_string(),
        }
    ));
    assert_eq!(editor.preview.content, "αβγδ");
    assert!(editor.preview.next_start_char.is_none());
}

#[test]
fn external_path_failure_preserves_unsaved_draft_and_exposes_target() {
    let mut editor = ready_editor();
    let before = editor.draft.len();
    editor.input_mode = Some(InputMode::ExternalPath {
        value: "/external/NOTES.md".to_string(),
    });
    editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(editor.draft.len(), before + 1);
    let generation = editor.selection_generation;
    let lease_id = editor.lease().unwrap().lease_id.clone();
    editor.register_pending(
        50,
        StartupContextPendingRequest::Selection {
            lease_id,
            generation,
        },
    );
    assert!(editor.accept_failure(
        50,
        StartupContextFailure {
            operation: crate::protocol::StartupContextOperation::PreviewSelection,
            kind: crate::protocol::StartupContextFailureKind::Io,
            message: "preview temporarily failed".to_string(),
            retryable: true,
            issues: Vec::new(),
        }
    ));
    assert_eq!(editor.draft.len(), before + 1);
    assert_eq!(
        editor.draft.last().unwrap().input.path,
        "/external/NOTES.md"
    );
}

#[test]
fn external_entry_rejects_relative_paths_without_mutating_draft() {
    let mut editor = ready_editor();
    let before = editor.draft.len();
    editor.input_mode = Some(InputMode::ExternalPath {
        value: "relative/NOTES.md".to_string(),
    });
    editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(editor.draft.len(), before);
    assert!(editor.notice.as_deref().unwrap().contains("absolute path"));
}

#[test]
fn preview_keyboard_and_mouse_scrolling_share_the_same_transition() {
    let mut keyboard = ready_editor();
    keyboard.active_pane = StartupContextEditorPane::Preview;
    keyboard.preview.content = (0..30)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    keyboard.move_cursor(3);
    assert_eq!(keyboard.preview.scroll, 3);

    let mut mouse = ready_editor();
    mouse.active_pane = StartupContextEditorPane::Preview;
    mouse.preview.content = keyboard.preview.content.clone();
    mouse.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(mouse.preview.scroll, keyboard.preview.scroll);
}

#[test]
fn closing_cancels_active_search_before_releasing_lease() {
    let mut editor = ready_editor();
    let lease_id = editor.lease().unwrap().lease_id.clone();
    editor.register_pending(
        77,
        StartupContextPendingRequest::Search {
            lease_id: lease_id.clone(),
            query: "plan".to_string(),
            generation: editor.browser.generation,
        },
    );
    editor.close();
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::CancelSearch {
            search_request_id: 77
        })
    ));
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::Close {
            lease_id: closed,
            ..
        }) if closed == lease_id
    ));
}

#[test]
fn directory_page_continuation_preserves_order_and_cursor_generation() {
    let mut editor = ready_editor();
    editor.browser.entries.clear();
    editor.browser.generation = 11;
    let lease_id = editor.lease().unwrap().lease_id.clone();
    editor.register_pending(
        80,
        StartupContextPendingRequest::Directory {
            lease_id,
            directory: String::new(),
            page_start: 0,
            generation: 11,
            bulk: false,
        },
    );
    assert!(editor.accept_directory(
        80,
        StartupContextDirectoryPage {
            project_key_digest: "fixture-project".to_string(),
            plan_revision: 7,
            directory: String::new(),
            total_entries: 2,
            page_start: 0,
            page_end: 1,
            next_page_start: Some(1),
            entries: vec![StartupContextDirectoryEntry {
                name: "a.md".to_string(),
                project_relative_path: "a.md".to_string(),
                resolved_path: "/fixture/project/a.md".to_string(),
                path_valid_utf8: true,
                kind: StartupContextDirectoryEntryKind::File,
                classification: StartupContextPathClassification::Project,
                navigable: false,
                bytes: Some(1),
                selected_spec_id: None,
            }],
        }
    ));
    assert_eq!(editor.browser.entries.len(), 1);
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::ListDirectory {
            page_start: 1,
            generation: 11,
            bulk: false,
            ..
        })
    ));
}

#[test]
fn reconnect_reacquires_lease_and_preserves_unsaved_draft_identity() {
    let mut editor = ready_editor();
    editor.add_paths(["README.md".to_string()]);
    let draft_ids = editor
        .draft
        .iter()
        .map(|entry| entry.local_id)
        .collect::<Vec<_>>();
    assert!(editor.is_dirty());
    editor.restart_after_reconnect();
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::Open)
    ));
    editor.register_pending(
        90,
        StartupContextPendingRequest::Open {
            session_id: "session".to_string(),
        },
    );
    let snapshot =
        StartupContextEditor::debug_fixture("editor-populated", "session".to_string(), Vec::new())
            .editor
            .expect("fixture editor snapshot");
    assert!(editor.accept_opened(90, snapshot));
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| entry.local_id)
            .collect::<Vec<_>>(),
        draft_ids
    );
    assert!(editor.is_dirty());
}

#[test]
fn lease_renews_while_visible_and_explicit_close_queues_release() {
    let mut editor = ready_editor();
    editor.renew_due = Some(Instant::now() - Duration::from_secs(1));
    editor.tick(Instant::now());
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::Renew { .. })
    ));
    editor.close();
    assert!(!editor.visible);
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::Close { .. })
    ));
}

#[test]
fn browser_mouse_add_uses_the_same_transition_as_space() {
    let mut keyboard = ready_editor();
    keyboard.browser.cursor = 1;
    keyboard.select_current_browser_entry();
    let keyboard_paths = keyboard
        .draft
        .iter()
        .map(|entry| entry.input.path.clone())
        .collect::<Vec<_>>();

    let mut mouse = ready_editor();
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| mouse.render(frame, frame.area(), None, None))
        .expect("render editor");
    let rect = mouse
        .hit_regions
        .iter()
        .find_map(|region| match region.action {
            RowAction::SelectBrowser(1) => Some(region.rect),
            _ => None,
        })
        .expect("browser add hit region");
    mouse.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    let mouse_paths = mouse
        .draft
        .iter()
        .map(|entry| entry.input.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(mouse_paths, keyboard_paths);
}

#[test]
fn foundation_mouse_targets_match_keyboard_transitions() {
    fn render(editor: &mut StartupContextEditor, width: u16) {
        let backend = ratatui::backend::TestBackend::new(width, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| editor.render(frame, frame.area(), None, None))
            .expect("render editor");
    }

    fn click(editor: &mut StartupContextEditor, action: RowAction) -> bool {
        let rect = editor
            .hit_regions
            .iter()
            .find(|region| region.action == action)
            .map(|region| region.rect)
            .unwrap_or_else(|| panic!("missing hit region for {action:?}"));
        editor.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    let mut keyboard = ready_editor();
    keyboard.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::StartSearch);
    assert!(matches!(
        keyboard.input_mode,
        Some(InputMode::Search { .. })
    ));
    assert!(matches!(mouse.input_mode, Some(InputMode::Search { .. })));

    let mut keyboard = ready_editor();
    keyboard.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::StartExternal);
    assert!(matches!(
        keyboard.input_mode,
        Some(InputMode::ExternalPath { .. })
    ));
    assert!(matches!(
        mouse.input_mode,
        Some(InputMode::ExternalPath { .. })
    ));

    let mut keyboard = ready_editor();
    keyboard.active_pane = StartupContextEditorPane::Selection;
    keyboard.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
    let mut mouse = ready_editor();
    mouse.active_pane = StartupContextEditorPane::Selection;
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::ToggleReceipt);
    assert_eq!(keyboard.selection_view, mouse.selection_view);

    let mut keyboard = ready_editor();
    keyboard.active_pane = StartupContextEditorPane::Selection;
    keyboard.handle_key(KeyCode::Char('J'), KeyModifiers::NONE);
    let keyboard_ids = keyboard
        .draft
        .iter()
        .map(|entry| entry.local_id)
        .collect::<Vec<_>>();
    let mut mouse = ready_editor();
    mouse.active_pane = StartupContextEditorPane::Selection;
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::MoveDraftDown(0));
    assert_eq!(
        mouse
            .draft
            .iter()
            .map(|entry| entry.local_id)
            .collect::<Vec<_>>(),
        keyboard_ids
    );

    let mut keyboard = ready_editor();
    keyboard.active_pane = StartupContextEditorPane::Selection;
    keyboard.handle_key(KeyCode::Delete, KeyModifiers::NONE);
    let mut mouse = ready_editor();
    mouse.active_pane = StartupContextEditorPane::Selection;
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::RemoveDraft(0));
    assert_eq!(mouse.draft.len(), keyboard.draft.len());

    let mut keyboard = ready_editor();
    keyboard.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    let mut mouse = ready_editor();
    render(&mut mouse, 72);
    click(
        &mut mouse,
        RowAction::FocusPane(StartupContextEditorPane::Selection),
    );
    assert_eq!(keyboard.active_pane, mouse.active_pane);

    let mut keyboard = ready_editor();
    keyboard.browser.cursor = 0;
    keyboard.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::OpenDirectory(0));
    assert!(matches!(
        keyboard.take_action(),
        Some(StartupContextEditorAction::ListDirectory { bulk: false, .. })
    ));
    assert!(matches!(
        mouse.take_action(),
        Some(StartupContextEditorAction::ListDirectory { bulk: false, .. })
    ));

    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    click(&mut mouse, RowAction::DisabledApply);
    assert!(mouse.notice.as_deref().unwrap().contains("disabled"));

    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    assert!(click(&mut mouse, RowAction::CloseEditor));
    assert!(!mouse.visible);
}
