use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn synthetic_draft() -> InstructionEditDraft {
    InstructionEditDraft {
        id: "draft-synthetic".into(),
        generation: 4,
        title: "Synthetic edit".into(),
        scope: InstructionEditScope::Global,
        repository: "/synthetic/instructions".into(),
        branch: Some("main".into()),
        files: vec![InstructionEditFile {
            key: "modules/example.md".into(),
            path: "modules/example.md".into(),
            body: "BODY 合成 {{literal}}".into(),
            metadata: InstructionEditMetadata::Resource(InstructionResourceFields {
                id: "example".into(),
                kind: InstructionEditKind::Module,
                name: None,
                description: None,
                template: InstructionEditTemplate::Plain,
                availability: None,
                target: None,
                includes: Vec::new(),
                allowed_tools: None,
            }),
            deleted: false,
        }],
        subject: "instruction: synthetic edit".into(),
        warnings: vec!["Synthetic consequence".into()],
        reviewed: false,
        save_started: false,
        choices: InstructionEditChoices::default(),
    }
}
fn manager() -> InstructionManager {
    let mut manager = InstructionManager::new("synthetic-session".into(), false);
    manager.queued = None;
    manager.editing.visible = true;
    manager.editing.draft = Some(synthetic_draft());
    manager
}
#[test]
fn edit_reply_correlation_rejects_wrong_session_draft_and_generation() {
    let mut ui = EditingUi {
        queued: Some(InstructionManagementRequest::Update {
            draft: "draft-synthetic".into(),
            generation: 3,
            change: InstructionDraftChange::Body {
                file: "modules/example.md".into(),
                body: "new".into(),
            },
        }),
        ..Default::default()
    };
    ui.reserve(11, "session").unwrap();
    let mut draft = synthetic_draft();
    assert!(!ui.accept(
        11,
        InstructionManagementReply {
            session_id: "other".into(),
            result: InstructionManagementResult::Draft(draft.clone())
        }
    ));
    draft.generation = 3;
    assert!(!ui.accept(
        11,
        InstructionManagementReply {
            session_id: "session".into(),
            result: InstructionManagementResult::Draft(draft.clone())
        }
    ));
    draft.generation = 4;
    assert!(ui.accept(
        11,
        InstructionManagementReply {
            session_id: "session".into(),
            result: InstructionManagementResult::Draft(draft)
        }
    ));
    assert!(ui.pending.is_none());
    assert_eq!(ui.draft.unwrap().generation, 4);
}
#[test]
fn edit_save_requires_review_and_metadata_paste_cannot_trigger_commands() {
    let mut manager = manager();
    manager.key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(manager.editing.queued.is_none());
    manager.key(KeyCode::Char('m'), KeyModifiers::NONE);
    manager.paste("/clear 合成");
    assert!(manager.editing.queued.is_none());
    let form = manager.editing.form.as_ref().unwrap();
    assert_eq!(form.fields[0].value, "/clear 合成");
    manager.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(manager.editing.visible);
    manager.editing.draft.as_mut().unwrap().reviewed = true;
    manager.key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(
        manager.editing.queued,
        Some(InstructionManagementRequest::Save { generation: 4, .. })
    ));
}
#[test]
fn edit_render_and_hit_regions_work_at_wide_narrow_and_minimum_sizes() {
    for (width, height) in [(150, 40), (80, 24), (60, 24), (24, 8)] {
        let mut manager = manager();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| manager.render(frame, frame.area()))
            .unwrap();
        assert!(
            manager
                .controls
                .iter()
                .all(|(rect, _)| rect.right() <= width && rect.bottom() <= height)
        );
        let actions = manager
            .controls
            .iter()
            .find(|(_, key)| *key == KeyCode::Char(' '))
            .unwrap()
            .0;
        manager.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: actions.x,
            row: actions.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(manager.menu.is_some());
        manager.key(KeyCode::Esc, KeyModifiers::NONE);
        manager.key(KeyCode::Char('m'), KeyModifiers::NONE);
        terminal
            .draw(|frame| manager.render(frame, frame.area()))
            .unwrap();
        assert!(!manager.editing.field_hits.is_empty());
        let first = manager.editing.field_hits[0].0;
        manager.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: first.x,
            row: first.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(manager.editing.form.as_ref().unwrap().selected, 0);
        manager.paste("中文 e\u{301}");
        terminal
            .draw(|frame| manager.render(frame, frame.area()))
            .unwrap();
        assert!(
            manager.editing.form.as_ref().unwrap().fields[0]
                .value
                .ends_with("中文 e\u{301}")
        );
    }
}
#[test]
fn wide_body_button_opens_editor_not_back() {
    let mut manager = manager();
    let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
    terminal
        .draw(|frame| manager.render(frame, frame.area()))
        .unwrap();
    let button = manager
        .controls
        .iter()
        .find(|(_, key)| *key == KeyCode::Char('b'))
        .unwrap()
        .0;
    manager.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: button.x,
        row: button.y,
        modifiers: KeyModifiers::NONE,
    });
    let editor = manager.editing.editor.as_ref().unwrap();
    assert_eq!(editor.body, "BODY 合成 {{literal}}");
    assert_eq!(editor.file, "modules/example.md");
    assert!(manager.editing.queued.is_none());
}
#[test]
fn invalid_form_submission_restores_complete_user_input() {
    let mut manager = manager();
    manager.edit_action(EditAction::Metadata);
    manager.paste("preserved value");
    let form = manager.editing.form.as_mut().unwrap();
    form.selected = form.fields.len();
    manager.edit_form_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(manager.editing.form.is_none());
    let request = manager.editing.reserve(8, "synthetic-session").unwrap();
    assert!(matches!(
        request,
        InstructionManagementRequest::Update { .. }
    ));
    assert!(manager.editing.accept(
        8,
        InstructionManagementReply {
            session_id: "synthetic-session".into(),
            result: InstructionManagementResult::Failed(InstructionManagementFailure {
                operation: "validate metadata".into(),
                detail: "synthetic failure".into(),
                draft: Some("draft-synthetic".into()),
                source_unchanged: true
            })
        }
    ));
    assert_eq!(
        manager.editing.form.as_ref().unwrap().fields[0].value,
        "preserved value"
    );
}
#[test]
fn edit_form_focus_is_not_color_only() {
    let _lock = crate::storage::lock_test_env();
    // Focus uses reverse video and a text cursor independently of colors.
    let mut manager = manager();
    manager.edit_action(EditAction::Metadata);
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal
        .draw(|frame| manager.render(frame, frame.area()))
        .unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED))
    );
}
