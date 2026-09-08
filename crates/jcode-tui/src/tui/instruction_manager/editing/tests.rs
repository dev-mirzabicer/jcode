use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn synthetic_draft() -> InstructionEditDraft {
    InstructionEditDraft {
        working_file_only: false,
        id: "draft-synthetic".into(),
        generation: 4,
        title: "Synthetic edit".into(),
        scope: InstructionEditScope::Global,
        repository: "/synthetic/instructions".into(),
        branch: Some("main".into()),
        files: vec![InstructionEditFile {
            executable: false,
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
        committed: None,
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

fn repository_choices() -> InstructionRepositoryChoices {
    InstructionRepositoryChoices {
        scope: InstructionEditScope::Global,
        project_is_git: false,
        git_available: true,
        can_initialize: false,
        can_recreate: true,
        root: Some("/synthetic/instructions".into()),
        branches: vec!["main".into()],
        remote_branches: Vec::new(),
        remotes: Vec::new(),
        configured_branch: Some("main".into()),
        current_branch: Some("main".into()),
        health: "Ready".into(),
        warnings: Vec::new(),
    }
}

#[test]
fn repository_forms_require_review_and_explicit_confirmation_at_every_width() {
    for (width, height) in [(150, 40), (80, 24), (60, 24), (24, 8)] {
        let mut manager = InstructionManager::new("synthetic-session".into(), false);
        manager.queued = None;
        manager.edit_action(EditAction::GlobalRepository);
        manager.editing.reserve(1, "synthetic-session").unwrap();
        assert!(manager.editing.accept(
            1,
            InstructionManagementReply {
                session_id: "synthetic-session".into(),
                result: InstructionManagementResult::RepositoryChoices(repository_choices())
            }
        ));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| manager.render(frame, frame.area()))
            .unwrap();
        let form = manager.editing.form.as_mut().unwrap();
        form.selected = 2;
        manager.paste("/synthetic/bare.git");
        let form = manager.editing.form.as_mut().unwrap();
        form.selected = form.fields.len();
        manager.key(KeyCode::Enter, KeyModifiers::NONE);
        let request = manager.editing.reserve(2, "synthetic-session").unwrap();
        let InstructionManagementRequest::PlanRepository { scope, action } = request else {
            panic!("expected reviewed plan")
        };
        assert_eq!(scope, InstructionEditScope::Global);
        assert!(
            matches!(&action, InstructionRepositoryAction::ConfigureRemote { name, url } if name == "origin" && url == "/synthetic/bare.git")
        );
        let plan = InstructionRepositoryPlan {
            id: "operation".into(),
            scope,
            action,
            title: "Configure remote".into(),
            detail: "Complete synthetic consequences".repeat(20),
            network: false,
            outgoing_commits: Vec::new(),
        };
        manager.editing.accept(
            2,
            InstructionManagementReply {
                session_id: "synthetic-session".into(),
                result: InstructionManagementResult::RepositoryPlan(plan.clone()),
            },
        );
        terminal
            .draw(|frame| manager.render(frame, frame.area()))
            .unwrap();
        assert_eq!(manager.editing.confirm, Some(EditAction::ApplyRepository));
        manager.key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(manager.editing.queued.is_none());
        assert!(manager.editing.form.is_some());
        let form = manager.editing.form.as_mut().unwrap();
        form.selected = form.fields.len();
        manager.key(KeyCode::Enter, KeyModifiers::NONE);
        manager.editing.reserve(3, "synthetic-session").unwrap();
        manager.editing.accept(
            3,
            InstructionManagementReply {
                session_id: "synthetic-session".into(),
                result: InstructionManagementResult::RepositoryPlan(plan),
            },
        );
        manager.key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(
            matches!(manager.editing.queued, Some(InstructionManagementRequest::ApplyRepository { ref operation_id }) if operation_id == "operation")
        );
    }
}

#[test]
fn closed_metadata_choices_never_silently_become_another_value() {
    let mut manager = manager();
    manager.edit_action(EditAction::Metadata);
    manager.editing.form.as_mut().unwrap().selected = 2;
    manager.paste("not-a-template-mode");
    let form = manager.editing.form.as_ref().unwrap();
    assert_eq!(form.fields[2].value, "plain");
    assert!(form.picker.is_some());
    assert!(manager.editing.queued.is_none());
}

#[test]
fn stale_form_requires_comparison_before_rebinding_to_a_new_draft_generation() {
    let mut manager = manager();
    manager.edit_action(EditAction::Metadata);
    manager.paste("retained local name");
    manager.editing.draft.as_mut().unwrap().generation += 1;
    let form = manager.editing.form.as_mut().unwrap();
    form.selected = form.fields.len();
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(manager.editing.queued.is_none());
    assert!(
        manager
            .editing
            .form
            .as_ref()
            .unwrap()
            .error
            .contains("changed")
    );
    manager.review_local_values();
    assert!(manager.editing.document.contains("CURRENT SERVER DRAFT"));
    manager.key(KeyCode::Char('y'), KeyModifiers::NONE);
    let form = manager.editing.form.as_mut().unwrap();
    form.selected = form.fields.len();
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        manager.editing.queued,
        Some(InstructionManagementRequest::Update { generation: 5, .. })
    ));
}

#[test]
fn stale_destructive_confirmation_does_not_apply_to_a_new_generation() {
    let mut manager = manager();
    manager.edit_action(EditAction::Discard);
    manager.editing.draft.as_mut().unwrap().generation += 1;
    manager.key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(manager.editing.queued.is_none());
    assert!(manager.editing.status.contains("changed"));
}

#[test]
fn complete_metadata_value_uses_external_editor_without_dispatching_a_source_change() {
    let mut manager = manager();
    manager.edit_action(EditAction::Metadata);
    let form = manager.editing.form.as_mut().unwrap();
    form.selected = 1;
    form.fields[1].value = "line one\n合成 line two".into();
    manager.key(KeyCode::Left, KeyModifiers::NONE);
    let form = manager.editing.form.as_mut().unwrap();
    form.selected = form.fields.len()
        + form
            .buttons
            .iter()
            .position(|button| *button == super::forms::FormButton::EditValue)
            .unwrap();
    manager.key(KeyCode::Enter, KeyModifiers::NONE);
    let editor = manager.editing.editor.as_ref().unwrap();
    assert_eq!(editor.body, "line one\n合成 line two");
    assert_eq!(editor.metadata_field, Some(1));
    assert!(manager.editing.queued.is_none());
}

#[test]
fn direct_close_during_update_preserves_reply_then_releases_the_server_draft() {
    let mut manager = manager();
    manager.editing.queued = Some(InstructionManagementRequest::Update {
        draft: "draft-synthetic".into(),
        generation: 4,
        change: InstructionDraftChange::Body {
            file: "modules/example.md".into(),
            body: "NEW".into(),
        },
    });
    manager.editing.reserve(91, "synthetic-session").unwrap();
    manager.key(KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(!manager.visible);
    let mut updated = synthetic_draft();
    updated.generation = 5;
    assert!(manager.accept_management(
        91,
        InstructionManagementReply {
            session_id: "synthetic-session".into(),
            result: InstructionManagementResult::Draft(updated)
        }
    ));
    assert!(matches!(
        manager.editing.queued,
        Some(InstructionManagementRequest::Close)
    ));
    assert_eq!(manager.editing.draft.as_ref().unwrap().generation, 5);
}

#[test]
fn question_mark_opens_contextual_draft_actions_without_leaking_input() {
    let mut manager = manager();
    manager.key(KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(
        manager
            .menu
            .as_ref()
            .is_some_and(|menu| menu.items.iter().any(|item| matches!(
                item.action,
                super::super::menu::MenuAction::Edit(EditAction::Save)
            )))
    );
    assert!(manager.editing.queued.is_none());
}

#[test]
fn edit_opens_local_editor_and_successful_update_reviews_without_ever_saving_automatically() {
    let mut manager = manager();
    manager.editing.draft = None;
    manager.editing.queued = Some(InstructionManagementRequest::Begin {
        snapshot: "snapshot".into(),
        target: InstructionInspectionTarget::Resource("source".into()),
        action: InstructionEditAction::Edit,
    });
    manager.editing.reserve(70, "synthetic-session");
    assert!(manager.accept_management(
        70,
        InstructionManagementReply {
            session_id: "synthetic-session".into(),
            result: InstructionManagementResult::Draft(synthetic_draft())
        }
    ));
    assert_eq!(
        manager.editing.editor.take().unwrap().body,
        "BODY 合成 {{literal}}"
    );
    manager.editing.queued = Some(InstructionManagementRequest::Update {
        draft: "draft-synthetic".into(),
        generation: 4,
        change: InstructionDraftChange::Body {
            file: "modules/example.md".into(),
            body: "NEW".into(),
        },
    });
    manager.editing.reserve(71, "synthetic-session");
    let mut updated = synthetic_draft();
    updated.generation = 5;
    assert!(manager.accept_management(
        71,
        InstructionManagementReply {
            session_id: "synthetic-session".into(),
            result: InstructionManagementResult::Draft(updated)
        }
    ));
    assert!(matches!(
        manager.editing.queued,
        Some(InstructionManagementRequest::Review { generation: 5, .. })
    ));
}
