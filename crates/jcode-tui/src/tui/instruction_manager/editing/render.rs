use super::super::render::{safe, tail_cells, wrap};
use super::*;
use forms::FormButton;
use jcode_tui_style::palette::{Role, role_color};
use ratatui::{
    Frame,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

impl InstructionManager {
    pub(crate) fn render_editing(&mut self, frame: &mut Frame, area: Rect) {
        self.editing.hits.clear();
        self.editing.field_hits.clear();
        let accent = if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(role_color(Role::Accent))
                .add_modifier(Modifier::BOLD)
        };
        let selected_style = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(
                if self.editing.draft.is_none()
                    && (self.editing.repository_plan.is_some()
                        || self.editing.repository_receipt.is_some()
                        || self
                            .editing
                            .form
                            .as_ref()
                            .is_some_and(|form| form.is_repository()))
                {
                    " Repository operation "
                } else {
                    " Instruction draft "
                },
            )
            .border_style(accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(form) = &mut self.editing.form {
            frame.render_widget(
                Paragraph::new(safe(&form.title)).style(accent),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
            let list = Rect::new(
                inner.x,
                inner.y + 1,
                inner.width,
                inner.height.saturating_sub(3),
            );
            if let Some(picker) = &form.picker {
                frame.render_widget(
                    Paragraph::new(format!("Find: {}|", safe(&picker.query))),
                    Rect::new(list.x, list.y, list.width, 1),
                );
                let matches = picker.matching();
                let count = usize::from(list.height.saturating_sub(1));
                let offset = picker.selected.saturating_sub(count.saturating_sub(1));
                for (visible, (index, value)) in matches
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(count)
                    .enumerate()
                {
                    let row = Rect::new(list.x, list.y + 1 + visible as u16, list.width, 1);
                    let text = if value.is_empty() {
                        "(none / inherit)"
                    } else {
                        value.as_str()
                    };
                    frame.render_widget(
                        Paragraph::new(safe(text)).style(if index == picker.selected {
                            selected_style
                        } else {
                            Style::default()
                        }),
                        row,
                    );
                    self.editing.field_hits.push((row, index));
                }
            } else {
                let total = form.fields.len() + form.buttons.len();
                let visible_rows = usize::from(list.height / 2).max(1);
                form.offset = form
                    .offset
                    .min(form.selected)
                    .max(form.selected.saturating_sub(visible_rows.saturating_sub(1)));
                for (visible, index) in (form.offset..total).take(visible_rows).enumerate() {
                    let y = list.y + (visible * 2) as u16;
                    let row = Rect::new(
                        list.x,
                        y,
                        list.width,
                        2.min(list.bottom().saturating_sub(y)),
                    );
                    if let Some(field) = form.fields.get(index) {
                        frame.render_widget(
                            Paragraph::new(safe(&field.label)).style(if index == form.selected {
                                accent
                            } else {
                                Style::default()
                            }),
                            Rect::new(row.x, row.y, row.width, 1),
                        );
                        let text = if index == form.selected {
                            let before = safe(&field.value[..field.cursor]);
                            let prefix =
                                tail_cells(&before, usize::from(row.width.saturating_sub(2)));
                            format!("{prefix}|{}", safe(&field.value[field.cursor..]))
                        } else if field.value.is_empty() {
                            "(empty)".into()
                        } else {
                            safe(&field.value)
                        };
                        if row.height > 1 {
                            frame.render_widget(
                                Paragraph::new(text).style(if index == form.selected {
                                    selected_style
                                } else {
                                    Style::default()
                                }),
                                Rect::new(row.x, row.y + 1, row.width, 1),
                            );
                        }
                    } else if let Some(button) = form.buttons.get(index - form.fields.len()) {
                        frame.render_widget(
                            Paragraph::new(format!("[ {} ]", button_label(*button))).style(
                                if index == form.selected {
                                    selected_style
                                } else {
                                    accent
                                },
                            ),
                            row,
                        );
                    }
                    self.editing.field_hits.push((row, index));
                }
            }
            let message = if form.error.is_empty() {
                "Tab/Arrows fields · Enter choose · Esc back"
            } else {
                &form.error
            };
            frame.render_widget(
                Paragraph::new(safe(message)),
                Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
            );
            frame.render_widget(
                Paragraph::new(if form.is_repository() {
                    "Review the repository action before explicit confirmation."
                } else {
                    "Changes stay in the draft until reviewed Save."
                }),
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
            );
            return;
        }
        let identity = self
            .editing
            .draft
            .as_ref()
            .map(|draft| {
                format!(
                    "{:?} · {} · {}",
                    draft.scope,
                    draft
                        .branch
                        .as_deref()
                        .unwrap_or(if draft.working_file_only {
                            "working file, no commit"
                        } else {
                            "DETACHED"
                        }),
                    draft
                        .files
                        .get(self.editing.file_index)
                        .map(|file| file.path.as_str())
                        .unwrap_or("draft")
                )
            })
            .or_else(|| {
                self.editing
                    .repository_receipt
                    .as_ref()
                    .map(|receipt| receipt.title.clone())
            })
            .or_else(|| {
                self.editing
                    .repository_plan
                    .as_ref()
                    .map(|plan| format!("{:?} · {}", plan.scope, plan.title))
            })
            .unwrap_or_else(|| "Prepare a reviewed edit".into());
        frame.render_widget(
            Paragraph::new(safe(&identity)).style(accent),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let body = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(3),
        );
        self.detail_height = usize::from(body.height);
        if self.editing.wrapped.is_empty() || self.editing.wrap_width != body.width {
            let content = if !self.editing.document.is_empty() {
                self.editing.document.clone()
            } else if let Some(draft) = &self.editing.draft {
                let mut value = format!(
                    "{}\nRepository: {}\nBranch: {}\nDraft: {} / {}\n\n{}\n\n",
                    draft.subject,
                    draft.repository,
                    draft
                        .branch
                        .as_deref()
                        .unwrap_or(if draft.working_file_only {
                            "Not applicable: parent remains uncommitted"
                        } else {
                            "DETACHED: close draft and select a branch before Save"
                        }),
                    draft.id,
                    draft.generation,
                    draft.warnings.join("\n")
                );
                if let Some(file) = draft.files.get(self.editing.file_index) {
                    value.push_str(&format!(
                        "FILE {}/{}: {}{}\n",
                        self.editing.file_index + 1,
                        draft.files.len(),
                        file.path,
                        if file.deleted { " [DELETE]" } else { "" }
                    ));
                    value.push_str("Body opens in your local external editor. Metadata opens typed fields. Tab selects another affected file.\n\n");
                    value.push_str(&file.body);
                }
                value
            } else {
                self.editing.status.clone()
            };
            self.editing.wrapped = wrap(&content, body.width);
            self.editing.wrap_width = body.width;
        }
        self.editing.scroll = self.editing.scroll.min(
            self.editing
                .wrapped
                .len()
                .saturating_sub(usize::from(body.height)),
        );
        frame.render_widget(
            Paragraph::new(
                self.editing
                    .wrapped
                    .iter()
                    .skip(self.editing.scroll)
                    .take(usize::from(body.height))
                    .map(|line| Line::raw(line.clone()))
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        let progress = format!(
            "{}/{} · {}",
            self.editing.scroll + 1,
            self.editing.wrapped.len(),
            self.editing.status
        );
        frame.render_widget(
            Paragraph::new(safe(&progress)),
            Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
        );
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        if self.editing.confirm.is_some() {
            frame.render_widget(
                Paragraph::new("Y Confirm · N/Esc Cancel").style(accent),
                footer,
            );
            self.controls.push((
                Rect::new(footer.x, footer.y, 9.min(footer.width), 1),
                KeyCode::Char('y'),
            ));
            if footer.width > 12 {
                self.controls.push((
                    Rect::new(footer.x + 12, footer.y, footer.width - 12, 1),
                    KeyCode::Char('n'),
                ));
            }
        } else {
            use unicode_width::UnicodeWidthStr;
            let buttons = if footer.width >= 70 {
                vec![
                    (KeyCode::Char(' '), "Actions:Space"),
                    (KeyCode::Char('b'), "Body:B"),
                    (KeyCode::Char('m'), "Metadata:M"),
                    (KeyCode::Char('r'), "Review:R"),
                    (KeyCode::Char('s'), "Save:S"),
                    (KeyCode::Esc, "Back:Esc"),
                ]
            } else {
                vec![
                    (KeyCode::Char(' '), "Actions:Space"),
                    (KeyCode::Esc, "Back:Esc"),
                ]
            };
            let mut x = footer.x;
            for (key, label) in buttons {
                let width = u16::try_from(label.width()).unwrap_or(u16::MAX);
                if x.saturating_add(width) > footer.right() {
                    break;
                }
                let rect = Rect::new(x, footer.y, width, 1);
                frame.render_widget(Paragraph::new(label).style(accent), rect);
                self.controls.push((rect, key));
                x = x.saturating_add(width).saturating_add(1);
            }
        }
    }
    pub(crate) fn edit_mouse(&mut self, event: MouseEvent) -> bool {
        self.editing.recovery_dirty |= self.editing.visible;
        if !self.editing.visible {
            return false;
        }
        match event.kind {
            MouseEventKind::ScrollDown => self.edit_key(KeyCode::Down, KeyModifiers::NONE),
            MouseEventKind::ScrollUp => self.edit_key(KeyCode::Up, KeyModifiers::NONE),
            MouseEventKind::Down(MouseButton::Left) => {
                let point = (event.column, event.row).into();
                if let Some((_, index)) = self
                    .editing
                    .field_hits
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    .copied()
                {
                    let mut activate = false;
                    if let Some(form) = &mut self.editing.form {
                        if let Some(picker) = &mut form.picker {
                            picker.selected = index;
                            activate = true;
                        } else {
                            form.focus(index);
                            activate = index >= form.fields.len()
                                || form
                                    .fields
                                    .get(index)
                                    .is_some_and(|field| !field.options.is_empty());
                        }
                    }
                    if activate {
                        self.edit_form_key(KeyCode::Enter, KeyModifiers::NONE);
                    }
                } else if let Some((_, key)) = self
                    .controls
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    .copied()
                {
                    self.edit_key(key, KeyModifiers::NONE);
                }
                true
            }
            _ => true,
        }
    }
}
fn button_label(button: FormButton) -> &'static str {
    match button {
        FormButton::EditValue => "Edit complete selected value in external editor",
        FormButton::ReviewTarget => "Review current target and retained values",
        FormButton::Submit => "Use these draft values",
        FormButton::AddReference => "Add module reference",
        FormButton::AddCandidate => "Add model candidate",
        FormButton::RemoveItem => "Remove selected reference/candidate",
        FormButton::Earlier => "Move selected item earlier",
        FormButton::Later => "Move selected item later",
        FormButton::AddAlias => "Create alias",
        FormButton::DeleteAlias => "Remove current alias from draft",
    }
}
