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
    assert_eq!(
        editor.notice.as_deref(),
        Some("Added 2 direct file(s) from docs")
    );
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
            project_key_digest: "fixture-project".to_string(),
            expected_plan_revision: 7,
            generation: 8,
            draft_generation: editor.draft_generation,
            purpose: ApplySelectionPurpose::DraftValidation,
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
            project_key_digest: "fixture-project".to_string(),
            expected_plan_revision: 7,
            generation,
            draft_generation: editor.draft_generation,
            purpose: ApplySelectionPurpose::DraftValidation,
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
    click(&mut mouse, RowAction::ApplySession);
    assert!(matches!(
        mouse.take_action(),
        Some(StartupContextEditorAction::PreviewSelection {
            purpose: ApplySelectionPurpose::Apply(ApplyIntent::SessionOnly),
            ..
        })
    ));

    let mut mouse = ready_editor();
    render(&mut mouse, 120);
    assert!(click(&mut mouse, RowAction::CloseEditor));
    assert!(!mouse.visible);
}

fn pop_editor_action(
    editor: &mut StartupContextEditor,
    predicate: impl Fn(&StartupContextEditorAction) -> bool,
) -> StartupContextEditorAction {
    while let Some(action) = editor.take_action() {
        if predicate(&action) {
            return action;
        }
    }
    panic!("matching editor action was not queued");
}

fn register_selection_action(
    editor: &mut StartupContextEditor,
    id: u64,
    action: &StartupContextEditorAction,
) {
    let StartupContextEditorAction::PreviewSelection {
        lease,
        generation,
        draft_generation,
        purpose,
        ..
    } = action
    else {
        panic!("expected selection preview action: {action:?}");
    };
    editor.register_pending(
        id,
        StartupContextPendingRequest::Selection {
            lease_id: lease.lease_id.clone(),
            project_key_digest: lease.project_key_digest.clone(),
            expected_plan_revision: lease.plan_revision,
            generation: *generation,
            draft_generation: *draft_generation,
            purpose: *purpose,
        },
    );
}

fn selected_preview(editor: &StartupContextEditor) -> StartupContextSelectionPreview {
    let entries = editor
        .draft
        .iter()
        .enumerate()
        .map(
            |(index, entry)| StartupContextSelectionEntrySnapshot::Selected {
                input_index: index,
                spec_id: entry
                    .normalized_spec_id
                    .clone()
                    .unwrap_or_else(|| format!("selected-{index}")),
                logical_path: entry.input.path.clone(),
                resolved_path: entry
                    .resolved_path
                    .clone()
                    .unwrap_or_else(|| format!("/fixture/project/{}", entry.input.path)),
                classification: entry
                    .classification
                    .unwrap_or(StartupContextPathClassification::Project),
                bytes: entry.bytes.unwrap_or(128),
                estimated_tokens: entry.estimated_tokens.unwrap_or(32),
                requires_external_approval: entry.classification
                    == Some(StartupContextPathClassification::External),
            },
        )
        .collect::<Vec<_>>();
    StartupContextSelectionPreview {
        project_key_digest: editor.lease().unwrap().project_key_digest.clone(),
        plan_revision: editor.lease().unwrap().plan_revision,
        entry_count: entries.len(),
        selected_count: entries.len(),
        issue_count: 0,
        aggregate_bytes: entries.len() as u64 * 128,
        aggregate_estimated_tokens: entries.len() as u64 * 32,
        entries,
        batch_issues: Vec::new(),
    }
}

fn register_apply_action(
    editor: &mut StartupContextEditor,
    id: u64,
    action: &StartupContextEditorAction,
) -> String {
    let StartupContextEditorAction::ApplySelection {
        lease,
        operation_id,
        draft_generation,
        ..
    } = action
    else {
        panic!("expected apply action: {action:?}");
    };
    editor.register_pending(
        id,
        StartupContextPendingRequest::Apply {
            operation_id: operation_id.clone(),
            lease_id: lease.lease_id.clone(),
            project_key_digest: lease.project_key_digest.clone(),
            expected_plan_revision: lease.plan_revision,
            draft_generation: *draft_generation,
        },
    );
    operation_id.clone()
}

fn apply_status(
    editor: &StartupContextEditor,
    operation_id: String,
    phase: crate::protocol::StartupContextApplyPhase,
    session_target: crate::protocol::StartupContextApplyTargetState,
    project_default_target: crate::protocol::StartupContextApplyTargetState,
) -> StartupContextApplyStatus {
    let now = chrono::Utc::now();
    StartupContextApplyStatus {
        operation_id,
        session_id: editor.session_id.clone(),
        phase,
        session_target,
        project_default_target,
        batch_id: None,
        file_count: editor.draft.len(),
        created_at: now,
        updated_at: now,
        failure: None,
    }
}

#[test]
fn approved_apply_actions_preview_then_submit_the_exact_draft() {
    for intent in [
        ApplyIntent::SessionOnly,
        ApplyIntent::SessionAndProjectDefault,
    ] {
        let mut editor = ready_editor();
        let original_draft = editor
            .draft
            .iter()
            .map(|entry| entry.input.clone())
            .collect::<Vec<_>>();
        editor.begin_apply(intent);
        let preview_action = pop_editor_action(&mut editor, |action| {
            matches!(action, StartupContextEditorAction::PreviewSelection { .. })
        });
        assert!(matches!(
            preview_action,
            StartupContextEditorAction::PreviewSelection {
                purpose: ApplySelectionPurpose::Apply(actual),
                ..
            } if actual == intent
        ));
        register_selection_action(&mut editor, 1, &preview_action);
        assert!(editor.accept_selection(1, selected_preview(&editor)));
        assert!(matches!(
            editor.apply_overlay,
            Some(ApplyOverlay::Review(_))
        ));

        editor.confirm_apply_overlay();
        let apply_action = pop_editor_action(&mut editor, |action| {
            matches!(action, StartupContextEditorAction::ApplySelection { .. })
        });
        let StartupContextEditorAction::ApplySelection {
            selection,
            save_project_default,
            ..
        } = &apply_action
        else {
            unreachable!()
        };
        assert_eq!(selection, &original_draft);
        assert_eq!(*save_project_default, intent.save_project_default());
        let operation_id = register_apply_action(&mut editor, 2, &apply_action);
        let queued = apply_status(
            &editor,
            operation_id,
            crate::protocol::StartupContextApplyPhase::Queued,
            crate::protocol::StartupContextApplyTargetState::Pending,
            if intent.save_project_default() {
                crate::protocol::StartupContextApplyTargetState::Pending
            } else {
                crate::protocol::StartupContextApplyTargetState::NotRequested
            },
        );
        assert!(editor.accept_apply_status(2, queued));
        assert_eq!(
            editor.tracked_status().unwrap().phase,
            crate::protocol::StartupContextApplyPhase::Queued
        );
        assert_eq!(
            editor
                .draft
                .iter()
                .map(|entry| entry.input.clone())
                .collect::<Vec<_>>(),
            original_draft
        );
    }
}

#[test]
fn recoverable_apply_transport_failure_preserves_draft_and_operation() {
    let mut editor = ready_editor();
    editor.begin_apply(ApplyIntent::SessionOnly);
    let preview_action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    register_selection_action(&mut editor, 50, &preview_action);
    assert!(editor.accept_selection(50, selected_preview(&editor)));
    editor.confirm_apply_overlay();
    let apply_action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::ApplySelection { .. })
    });
    let operation_id = register_apply_action(&mut editor, 51, &apply_action);
    let draft = editor
        .draft
        .iter()
        .map(|entry| (entry.local_id, entry.input.clone()))
        .collect::<Vec<_>>();
    assert!(editor.reject_transport(51, "transport failed before apply confirmation".to_string()));
    editor.requeue_front(apply_action);
    assert!(matches!(
        editor.take_action(),
        Some(StartupContextEditorAction::ApplySelection {
            operation_id: ref actual,
            ..
        }) if actual == &operation_id
    ));
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| (entry.local_id, entry.input.clone()))
            .collect::<Vec<_>>(),
        draft
    );
}

#[test]
fn external_confirmation_binds_the_server_target_and_reconfirms_retargeting() {
    let mut editor =
        StartupContextEditor::debug_fixture("editor-external", "session".to_string(), Vec::new());
    editor.begin_apply(ApplyIntent::SessionAndProjectDefault);
    let first_action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    register_selection_action(&mut editor, 10, &first_action);
    let mut first_preview = selected_preview(&editor);
    let external_index = editor.draft.len() - 1;
    first_preview.entries[external_index] = StartupContextSelectionEntrySnapshot::Issue {
        issue: StartupContextFileIssueSnapshot {
            input_index: Some(external_index as u32),
            spec_id: None,
            logical_path: Some("/Users/mirza/private/NOTES.md".to_string()),
            kind: StartupContextFileIssueKind::ExternalApprovalRequired {
                resolved_target: "/Users/mirza/private/NOTES.md".to_string(),
            },
        },
    };
    first_preview.selected_count -= 1;
    first_preview.issue_count = 1;
    assert!(editor.accept_selection(10, first_preview));
    assert!(matches!(
        editor.apply_overlay,
        Some(ApplyOverlay::ConfirmExternal { ref targets, .. })
            if targets[0].resolved_target == "/Users/mirza/private/NOTES.md"
    ));

    editor.confirm_apply_overlay();
    let second_action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    let StartupContextEditorAction::PreviewSelection { selection, .. } = &second_action else {
        unreachable!()
    };
    assert_eq!(
        selection[external_index]
            .approved_external_target
            .as_deref(),
        Some("/Users/mirza/private/NOTES.md")
    );
    register_selection_action(&mut editor, 11, &second_action);
    let mut retargeted = selected_preview(&editor);
    retargeted.entries[external_index] = StartupContextSelectionEntrySnapshot::Issue {
        issue: StartupContextFileIssueSnapshot {
            input_index: Some(external_index as u32),
            spec_id: None,
            logical_path: Some("/Users/mirza/private/NOTES.md".to_string()),
            kind: StartupContextFileIssueKind::ExternalTargetChanged {
                approved_target: "/Users/mirza/private/NOTES.md".to_string(),
                resolved_target: "/Users/mirza/private/RETARGETED.md".to_string(),
            },
        },
    };
    retargeted.selected_count -= 1;
    retargeted.issue_count = 1;
    assert!(editor.accept_selection(11, retargeted));
    assert!(matches!(
        editor.apply_overlay,
        Some(ApplyOverlay::ConfirmExternal { ref targets, .. })
            if targets[0].approved_target.as_deref() == Some("/Users/mirza/private/NOTES.md")
                && targets[0].resolved_target == "/Users/mirza/private/RETARGETED.md"
    ));

    editor.confirm_apply_overlay();
    let third_action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    let StartupContextEditorAction::PreviewSelection { selection, .. } = &third_action else {
        unreachable!()
    };
    assert_eq!(
        selection[external_index]
            .approved_external_target
            .as_deref(),
        Some("/Users/mirza/private/RETARGETED.md")
    );
    register_selection_action(&mut editor, 12, &third_action);
    assert!(editor.accept_selection(12, selected_preview(&editor)));
    assert!(matches!(
        editor.apply_overlay,
        Some(ApplyOverlay::Review(_))
    ));
}

#[test]
fn stale_apply_preview_is_rejected_and_draft_mutation_cancels_local_review() {
    let mut editor = ready_editor();
    editor.begin_apply(ApplyIntent::SessionOnly);
    let action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    register_selection_action(&mut editor, 20, &action);
    let stale_preview = selected_preview(&editor);
    editor.add_paths(["NEW.md".to_string()]);
    assert!(!editor.accept_selection(20, stale_preview));
    assert!(editor.apply_overlay.is_none());
    assert!(
        editor
            .draft
            .iter()
            .any(|entry| entry.input.path == "NEW.md")
    );

    let mut editor = ready_editor();
    editor.begin_apply(ApplyIntent::SessionOnly);
    let action = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    register_selection_action(&mut editor, 21, &action);
    let mut stale_revision = selected_preview(&editor);
    stale_revision.plan_revision += 1;
    assert!(!editor.accept_selection(21, stale_revision));
    assert!(matches!(
        editor.apply_overlay,
        Some(ApplyOverlay::PreviewFailed { .. })
    ));
    assert!(matches!(editor.phase, EditorPhase::Closing));
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::Close { .. }
        )),
        StartupContextEditorAction::Close { .. }
    ));
}

#[test]
fn queued_apply_survives_reconnect_and_terminal_failure_preserves_the_draft() {
    let mut editor = ready_editor();
    let original = editor
        .draft
        .iter()
        .map(|entry| (entry.local_id, entry.input.clone()))
        .collect::<Vec<_>>();
    editor.install_debug_apply_fixture("editor-apply-queued");
    let operation_id = editor.apply_tracking.as_ref().unwrap().operation_id.clone();
    editor.restart_after_reconnect();
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| (entry.local_id, entry.input.clone()))
            .collect::<Vec<_>>(),
        original
    );
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::Open
        )),
        StartupContextEditorAction::Open
    ));
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::GetApplyStatus { .. }
        )),
        StartupContextEditorAction::GetApplyStatus { operation_id: ref actual }
            if actual == &operation_id
    ));

    let mut failed = apply_status(
        &editor,
        operation_id,
        crate::protocol::StartupContextApplyPhase::Failed,
        crate::protocol::StartupContextApplyTargetState::Failed {
            message: "late capture failed".to_string(),
            retryable: true,
        },
        crate::protocol::StartupContextApplyTargetState::NotRequested,
    );
    failed.failure = Some(StartupContextFailure {
        operation: crate::protocol::StartupContextOperation::ApplySelection,
        kind: crate::protocol::StartupContextFailureKind::Io,
        message: "late addition rejected atomically".to_string(),
        retryable: true,
        issues: Vec::new(),
    });
    assert!(editor.accept_apply_status(0, failed));
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| (entry.local_id, entry.input.clone()))
            .collect::<Vec<_>>(),
        original
    );
    assert_eq!(
        editor.tracked_status().unwrap().phase,
        crate::protocol::StartupContextApplyPhase::Failed
    );
}

#[test]
fn apply_mouse_regions_are_distinct_and_escape_unwinds_each_layer() {
    fn render(editor: &mut StartupContextEditor, width: u16, height: u16) {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| editor.render(frame, frame.area(), None, None))
            .expect("render editor");
    }

    for (width, height) in [(120, 30), (72, 24), (38, 12)] {
        let mut editor = ready_editor();
        render(&mut editor, width, height);
        let session = editor
            .hit_regions
            .iter()
            .find(|region| region.action == RowAction::ApplySession)
            .expect("session apply region")
            .rect;
        let save = editor
            .hit_regions
            .iter()
            .find(|region| region.action == RowAction::ApplyAndSave)
            .expect("combined apply region")
            .rect;
        assert!(session.right() <= save.x || save.right() <= session.x);
        assert!(session.width > 0 && save.width > 0);
    }

    for fixture in [
        "editor-apply-review",
        "editor-apply-external",
        "editor-apply-queued",
        "editor-apply-success",
        "editor-apply-failed",
    ] {
        let mut editor =
            StartupContextEditor::debug_fixture(fixture, "session".to_string(), Vec::new());
        assert!(editor.apply_overlay.is_some(), "{fixture}");
        assert!(!editor.handle_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(editor.apply_overlay.is_none(), "{fixture}");
        assert!(editor.visible);
    }
}

#[test]
fn apply_status_preserves_truthful_per_target_outcomes() {
    let cases = [
        (
            "editor-apply-queued",
            crate::protocol::StartupContextApplyPhase::Queued,
            crate::protocol::StartupContextApplyTargetState::Pending,
            crate::protocol::StartupContextApplyTargetState::Pending,
        ),
        (
            "editor-apply-applying",
            crate::protocol::StartupContextApplyPhase::Applying,
            crate::protocol::StartupContextApplyTargetState::Pending,
            crate::protocol::StartupContextApplyTargetState::Pending,
        ),
        (
            "editor-apply-success",
            crate::protocol::StartupContextApplyPhase::Succeeded,
            crate::protocol::StartupContextApplyTargetState::Applied { revision: None },
            crate::protocol::StartupContextApplyTargetState::Applied { revision: Some(8) },
        ),
        (
            "editor-apply-partial",
            crate::protocol::StartupContextApplyPhase::RecoveryRequired,
            crate::protocol::StartupContextApplyTargetState::Failed {
                message: "session persistence is retrying from durable recovery".to_string(),
                retryable: true,
            },
            crate::protocol::StartupContextApplyTargetState::Applied { revision: Some(8) },
        ),
        (
            "editor-apply-failed",
            crate::protocol::StartupContextApplyPhase::Failed,
            crate::protocol::StartupContextApplyTargetState::Failed {
                message: "selected file disappeared before late capture".to_string(),
                retryable: true,
            },
            crate::protocol::StartupContextApplyTargetState::Failed {
                message: "project default was not changed".to_string(),
                retryable: true,
            },
        ),
        (
            "editor-apply-canceled",
            crate::protocol::StartupContextApplyPhase::Canceled,
            crate::protocol::StartupContextApplyTargetState::Canceled,
            crate::protocol::StartupContextApplyTargetState::Canceled,
        ),
    ];
    for (fixture, phase, session_target, project_target) in cases {
        let editor =
            StartupContextEditor::debug_fixture(fixture, "session".to_string(), Vec::new());
        let status = editor.tracked_status().expect(fixture);
        assert_eq!(status.phase, phase, "{fixture}");
        assert_eq!(status.session_target, session_target, "{fixture}");
        assert_eq!(status.project_default_target, project_target, "{fixture}");
        if fixture == "editor-apply-partial" {
            assert_ne!(
                status.session_target,
                crate::protocol::StartupContextApplyTargetState::Applied { revision: None }
            );
        }
    }
}

#[test]
fn queued_apply_cancel_keeps_identity_and_failed_apply_restarts_with_fresh_preview() {
    let mut editor = StartupContextEditor::debug_fixture(
        "editor-apply-queued",
        "session".to_string(),
        Vec::new(),
    );
    let operation_id = editor.apply_tracking.as_ref().unwrap().operation_id.clone();
    editor.cancel_queued_apply();
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::CancelApply { .. }
        )),
        StartupContextEditorAction::CancelApply { operation_id: ref actual, .. }
            if actual == &operation_id
    ));

    let mut editor = StartupContextEditor::debug_fixture(
        "editor-apply-failed",
        "session".to_string(),
        Vec::new(),
    );
    let preserved = editor
        .draft
        .iter()
        .map(|entry| entry.input.clone())
        .collect::<Vec<_>>();
    editor.retry_apply();
    let retry = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::PreviewSelection { .. })
    });
    let StartupContextEditorAction::PreviewSelection {
        selection, purpose, ..
    } = retry
    else {
        unreachable!()
    };
    assert_eq!(selection, preserved);
    assert_eq!(
        purpose,
        ApplySelectionPurpose::Apply(ApplyIntent::SessionAndProjectDefault)
    );
    assert!(editor.apply_tracking.is_none());
}

#[test]
fn editor_reopens_after_close_and_expired_refresh_without_losing_the_draft() {
    let mut editor = ready_editor();
    editor.add_paths(["UNSAVED.md".to_string()]);
    let draft = editor
        .draft
        .iter()
        .map(|entry| (entry.local_id, entry.input.clone()))
        .collect::<Vec<_>>();
    editor.close();
    let close = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::Close { .. })
    });
    let StartupContextEditorAction::Close { lease_id, .. } = close else {
        unreachable!()
    };
    editor.register_pending(
        200,
        StartupContextPendingRequest::Close {
            lease_id: lease_id.clone(),
        },
    );
    editor.reopen();
    assert!(editor.accept_closed(200, &lease_id));
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::Open
        )),
        StartupContextEditorAction::Open
    ));
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| (entry.local_id, entry.input.clone()))
            .collect::<Vec<_>>(),
        draft
    );

    let mut editor = ready_editor();
    editor.add_paths(["UNSAVED.md".to_string()]);
    let draft = editor
        .draft
        .iter()
        .map(|entry| (entry.local_id, entry.input.clone()))
        .collect::<Vec<_>>();
    editor.request_authority_refresh();
    let close = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::Close { .. })
    });
    let StartupContextEditorAction::Close { lease_id, .. } = close else {
        unreachable!()
    };
    editor.register_pending(201, StartupContextPendingRequest::Close { lease_id });
    assert!(editor.accept_failure(
        201,
        StartupContextFailure {
            operation: crate::protocol::StartupContextOperation::CloseEditor,
            kind: crate::protocol::StartupContextFailureKind::LeaseExpired,
            message: "lease already expired".to_string(),
            retryable: true,
            issues: Vec::new(),
        }
    ));
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::Open
        )),
        StartupContextEditorAction::Open
    ));
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| (entry.local_id, entry.input.clone()))
            .collect::<Vec<_>>(),
        draft
    );
}

#[test]
fn terminal_apply_refreshes_saved_default_authoritatively_without_losing_draft_identity() {
    let mut editor = ready_editor();
    editor.active_pane = StartupContextEditorPane::Selection;
    editor.move_draft(1);
    let draft_ids = editor
        .draft
        .iter()
        .map(|entry| entry.local_id)
        .collect::<Vec<_>>();
    let draft_plan = editor
        .draft
        .iter()
        .map(|entry| crate::protocol::StartupContextPlanEntrySnapshot {
            spec_id: entry
                .normalized_spec_id
                .clone()
                .expect("normalized plan entry"),
            logical_path: entry.logical_path.clone(),
            approved_external_target: entry.input.approved_external_target.clone(),
        })
        .collect::<Vec<_>>();
    assert!(editor.is_dirty());
    let old_saved = editor.saved_default.clone();
    editor.install_debug_apply_fixture("editor-apply-applying");
    let operation_id = editor.apply_tracking.as_ref().unwrap().operation_id.clone();
    let status = apply_status(
        &editor,
        operation_id,
        crate::protocol::StartupContextApplyPhase::Succeeded,
        crate::protocol::StartupContextApplyTargetState::Unchanged,
        crate::protocol::StartupContextApplyTargetState::Applied { revision: Some(8) },
    );
    assert!(editor.accept_apply_status(0, status));
    assert_eq!(editor.saved_default, old_saved);
    let close = pop_editor_action(&mut editor, |action| {
        matches!(action, StartupContextEditorAction::Close { .. })
    });
    let StartupContextEditorAction::Close { lease_id, .. } = close else {
        unreachable!()
    };
    editor.register_pending(
        300,
        StartupContextPendingRequest::Close {
            lease_id: lease_id.clone(),
        },
    );
    assert!(editor.accept_closed(300, &lease_id));
    assert!(matches!(
        pop_editor_action(&mut editor, |action| matches!(
            action,
            StartupContextEditorAction::Open
        )),
        StartupContextEditorAction::Open
    ));
    editor.register_pending(
        301,
        StartupContextPendingRequest::Open {
            session_id: "session".to_string(),
        },
    );
    let now = chrono::Utc::now();
    assert!(editor.accept_opened(
        301,
        StartupContextEditorSnapshot {
            lease: StartupContextLeaseSnapshot {
                lease_id: "refreshed-lease".to_string(),
                project_key_digest: "fixture-project".to_string(),
                owner_session_id: "session".to_string(),
                acquired_at: now,
                renewed_at: now,
                expires_at: now + chrono::Duration::minutes(2),
                plan_revision: 8,
            },
            project: crate::protocol::StartupContextProjectSnapshot {
                key_digest: "fixture-project".to_string(),
                kind: crate::protocol::StartupContextProjectKind::Git,
                active_root: "/fixture/project".to_string(),
            },
            plan_revision: 8,
            plan_entries: draft_plan.clone(),
        }
    ));
    assert_eq!(editor.saved_default, draft_plan);
    assert_eq!(
        editor
            .draft
            .iter()
            .map(|entry| entry.local_id)
            .collect::<Vec<_>>(),
        draft_ids
    );
    assert!(!editor.is_dirty());
}

#[test]
fn editor_hit_regions_never_overlap_at_required_sizes() {
    fn overlap(left: Rect, right: Rect) -> bool {
        left.width > 0
            && left.height > 0
            && right.width > 0
            && right.height > 0
            && left.x < right.right()
            && right.x < left.right()
            && left.y < right.bottom()
            && right.y < left.bottom()
    }

    let fixtures = [
        "editor-populated",
        "editor-invalid",
        "editor-external",
        "editor-apply-review",
        "editor-apply-external",
        "editor-apply-queued",
        "editor-apply-applying",
        "editor-apply-recovery",
        "editor-apply-success",
        "editor-apply-partial",
        "editor-apply-failed",
        "editor-apply-canceled",
    ];
    for fixture in fixtures {
        for (width, height) in [(120, 30), (72, 24), (38, 12)] {
            let mut editor =
                StartupContextEditor::debug_fixture(fixture, "session".to_string(), Vec::new());
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| editor.render(frame, frame.area(), None, None))
                .expect("render editor");
            for (index, left) in editor.hit_regions.iter().enumerate() {
                assert!(
                    left.rect.width > 0 && left.rect.height > 0,
                    "{fixture} {width}x{height} {left:?}"
                );
                for right in editor.hit_regions.iter().skip(index + 1) {
                    assert!(
                        !overlap(left.rect, right.rect),
                        "{fixture} at {width}x{height} overlaps {left:?} and {right:?}"
                    );
                }
            }
        }
    }
}
