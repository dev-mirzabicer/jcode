use super::menu::{VIEWS, format_key};
use super::*;
use jcode_tui_style::palette::{Role, role_color};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

struct Theme {
    accent: Style,
    muted: Style,
    warning: Style,
    error: Style,
    selected: Style,
}
impl Theme {
    fn new() -> Self {
        let plain = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let color = |role| {
            if plain {
                Color::Reset
            } else {
                role_color(role)
            }
        };
        Self {
            accent: Style::default()
                .fg(color(Role::Accent))
                .add_modifier(Modifier::BOLD),
            muted: Style::default().fg(color(Role::Dim)),
            warning: Style::default().fg(color(Role::Warning)),
            error: Style::default()
                .fg(color(Role::Error))
                .add_modifier(Modifier::BOLD),
            selected: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }
}

impl InstructionManager {
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        self.areas = [Rect::default(); 3];
        self.tabs.clear();
        self.controls.clear();
        self.list_hits.clear();
        self.menu_hits.clear();
        let theme = Theme::new();
        self.small = area.width < 24 || area.height < 8;
        if self.small {
            frame.render_widget(
                Paragraph::new(
                    "Instructions\nNeed 24 columns x 8 rows\nResize to continue. Q closes.",
                )
                .style(theme.warning),
                area,
            );
            self.capture(frame, area);
            return;
        }
        if self.menu.is_some() {
            self.render_menu(frame, area, &theme);
            self.capture(frame, area);
            return;
        }
        if self.help {
            self.render_help(frame, area, &theme);
            self.capture(frame, area);
            return;
        }
        if self.editing.visible {
            self.render_editing(frame, area);
            self.capture(frame, area);
            return;
        }
        let compact = area.height < 12;
        let filtered = self.filter != InstructionFilter::default();
        let top = if compact {
            2
        } else if filtered {
            4
        } else {
            3
        };
        let bottom = if compact { 1 } else { 2 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(top),
                Constraint::Min(1),
                Constraint::Length(bottom),
            ])
            .split(area);
        let title = if area.width >= 65 {
            format!(
                "Instructions   READ ONLY   |   Active agent: {}",
                self.snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.active_agent.as_deref())
                    .unwrap_or("not available")
            )
        } else {
            "Instructions [read only]".into()
        };
        line(
            frame,
            Rect::new(area.x, area.y, area.width, 1),
            &title,
            theme.accent,
        );
        let nav = [
            (
                KeyCode::F(1),
                if area.width >= 60 {
                    "F1 Repositories"
                } else {
                    "Repos"
                },
            ),
            (
                KeyCode::F(2),
                if area.width >= 60 {
                    "F2 Resources"
                } else {
                    "List"
                },
            ),
            (
                KeyCode::F(3),
                if area.width >= 60 {
                    "F3 Reading"
                } else {
                    "Read"
                },
            ),
        ];
        self.buttons(
            frame,
            Rect::new(area.x, area.y + 1, area.width, 1),
            &nav,
            &theme,
        );
        if !compact {
            if self.search_editing {
                let before = safe(&self.filter.search[..self.search_cursor]);
                let after = safe(&self.filter.search[self.search_cursor..]);
                let before = tail_cells(&before, usize::from(area.width.saturating_sub(15)));
                line(
                    frame,
                    Rect::new(area.x, area.y + 2, area.width, 1),
                    &format!("Search: {before}|{after}"),
                    theme.accent,
                );
            } else {
                self.buttons(
                    frame,
                    Rect::new(area.x, area.y + 2, area.width, 1),
                    &[
                        (KeyCode::Char('/'), "/ Search"),
                        (KeyCode::Char('f'), "F Filters"),
                        (KeyCode::Char('t'), "T Views"),
                        (KeyCode::Char(' '), "Space Actions"),
                        (KeyCode::Char('r'), "R Refresh"),
                    ],
                    &theme,
                );
            }
            if filtered {
                line(
                    frame,
                    Rect::new(area.x, area.y + 3, area.width, 1),
                    &self.filter_summary(),
                    theme.muted,
                );
            }
        } else if self.search_editing {
            line(
                frame,
                Rect::new(area.x, area.y + 1, area.width, 1),
                &format!(
                    "Search: {}|",
                    tail_cells(
                        &safe(&self.filter.search),
                        usize::from(area.width.saturating_sub(9))
                    )
                ),
                theme.accent,
            );
            self.controls.clear();
        }
        let body = chunks[1];
        if area.width >= 140 && !self.expanded {
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(25),
                    Constraint::Length(39),
                    Constraint::Min(1),
                ])
                .split(body);
            self.areas = [panes[0], panes[1], panes[2]];
        } else if area.width >= 90 && !self.expanded {
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(33), Constraint::Min(1)])
                .split(body);
            self.areas[if self.pane == Pane::Repositories {
                0
            } else {
                1
            }] = panes[0];
            self.areas[2] = panes[1];
        } else {
            self.areas[match self.pane {
                Pane::Repositories => 0,
                Pane::Resources => 1,
                Pane::Detail => 2,
            }] = body;
        }
        self.render_repositories(frame, self.areas[0], &theme);
        self.render_resources(frame, self.areas[1], &theme);
        self.render_detail(frame, self.areas[2], &theme);
        if !compact {
            line(
                frame,
                Rect::new(area.x, chunks[2].y, area.width, 1),
                &self.status,
                if self.last_error {
                    theme.error
                } else if self.pending.is_some() {
                    theme.warning
                } else {
                    theme.muted
                },
            );
        }
        let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        if self.search_editing {
            self.buttons(
                frame,
                footer,
                &[(KeyCode::Enter, "Enter Done"), (KeyCode::Esc, "Esc Done")],
                &theme,
            );
        } else if compact || area.width < 44 {
            self.buttons(
                frame,
                footer,
                &[
                    (KeyCode::Char(' '), "Space Actions"),
                    (KeyCode::Esc, "Esc Back"),
                ],
                &theme,
            );
        } else {
            let mut actions = vec![
                (
                    KeyCode::Enter,
                    if self.history_visible && self.pane == Pane::Detail {
                        "Enter Read revision"
                    } else if self.pane == Pane::Detail {
                        "Enter Views"
                    } else {
                        "Enter Open"
                    },
                ),
                (KeyCode::Esc, "Esc Back"),
                (KeyCode::Char(' '), "Space Actions"),
            ];
            if self.history_visible && self.pane == Pane::Detail {
                actions.push((KeyCode::Char('a'), "A Mark base"));
                if self.history_base.is_some() {
                    actions.push((KeyCode::Char('b'), "B Compare"));
                }
            }
            if self.can_page(true) {
                actions.push((KeyCode::Char('n'), "N Next page"));
            }
            if self.can_page(false) {
                actions.push((KeyCode::Char('p'), "P Previous"));
            }
            actions.extend([
                (KeyCode::Char('?'), "? Help"),
                (KeyCode::Char('q'), "Q Close"),
            ]);
            self.buttons(frame, footer, &actions, &theme);
        }
        self.capture(frame, area);
    }

    fn buttons(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        buttons: &[(KeyCode, &str)],
        theme: &Theme,
    ) {
        let mut x = area.x;
        for (key, label) in buttons {
            let width = u16::try_from(label.width() + 1).unwrap_or(u16::MAX);
            if width > area.right().saturating_sub(x) {
                break;
            }
            let rect = Rect::new(x, area.y, width, area.height.min(1));
            let focused = matches!(
                (self.pane, key),
                (Pane::Repositories, KeyCode::F(1))
                    | (Pane::Resources, KeyCode::F(2))
                    | (Pane::Detail, KeyCode::F(3))
            );
            line(
                frame,
                rect,
                &format!("{label} "),
                if focused {
                    theme.selected
                } else {
                    theme.accent
                },
            );
            self.controls.push((rect, *key));
            x = x.saturating_add(width);
        }
    }

    fn pane(&self, frame: &mut Frame, area: Rect, title: &str, pane: Pane, theme: &Theme) -> Rect {
        let title = format!("{}{}", if self.pane == pane { "> " } else { "" }, title);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(clip(&title, usize::from(area.width.saturating_sub(2))))
            .border_style(if self.pane == pane {
                theme.accent
            } else {
                theme.muted
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    }

    fn draw_rows(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        labels: &[String],
        selected: usize,
        pane: Pane,
        theme: &Theme,
    ) {
        let height = usize::from(area.height).max(1);
        let start = selected
            .saturating_sub(height / 2)
            .min(labels.len().saturating_sub(height));
        for (index, label) in labels.iter().enumerate().skip(start).take(height) {
            let rect = Rect::new(
                area.x,
                area.y + u16::try_from(index - start).unwrap_or(0),
                area.width.saturating_sub(1),
                1,
            );
            let selected = index == selected;
            line(
                frame,
                rect,
                &format!("{} {}", if selected { ">" } else { " " }, label),
                if selected && self.pane == pane {
                    theme.selected
                } else {
                    Style::default()
                },
            );
            self.list_hits.push((rect, pane, index));
        }
        scrollbar(frame, area, start, labels.len(), theme);
    }

    fn render_repositories(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if area.width == 0 {
            return;
        }
        let inner = self.pane(frame, area, "Repositories", Pane::Repositories, theme);
        let mut rows = vec!["Session & active instructions".into()];
        if let Some(snapshot) = &self.snapshot {
            rows.extend(snapshot.repositories.iter().map(|store| {
                format!(
                    "{}{}{}",
                    store.kind,
                    if store.detached {
                        " [detached]"
                    } else if store.conflicts > 0 {
                        " [conflict]"
                    } else if store.dirty {
                        " [changed]"
                    } else {
                        ""
                    },
                    if store.active_lease { " [busy]" } else { "" }
                )
            }));
        }
        self.draw_rows(
            frame,
            inner,
            &rows,
            self.repository_selected,
            Pane::Repositories,
            theme,
        );
    }

    fn render_resources(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if area.width == 0 {
            return;
        }
        let label = if self.filter.redefinitions == Some(true) {
            "Redefinitions"
        } else {
            "Resources"
        };
        let inner = self.pane(
            frame,
            area,
            &format!("{label} · {} results", self.row_total),
            Pane::Resources,
            theme,
        );
        if self.rows_loading() {
            line(frame, inner, "Loading resources…", theme.warning);
            return;
        }
        if self.rows.is_empty() {
            text(
                frame,
                inner,
                if self.last_error {
                    "Source discovery failed.\nEnter Views or Space Actions to inspect the error or refresh."
                } else {
                    "No matching resources.\nF: adjust filters\nC: clear all filters\nR: refresh sources"
                },
                theme.muted,
            );
            return;
        }
        let labels = self
            .rows
            .iter()
            .map(|row| {
                format!(
                    "{}{}{}{}",
                    if row.high_impact {
                        "[HIGH] "
                    } else if row.redefines_global {
                        "[override] "
                    } else {
                        ""
                    },
                    if !row.valid {
                        "[invalid] "
                    } else if !row.effective {
                        "[shadowed] "
                    } else {
                        ""
                    },
                    row.id,
                    if row.scope == "project" {
                        " · project"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>();
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        );
        self.draw_rows(frame, body, &labels, self.selected, Pane::Resources, theme);
        let row = self.rows.get(self.selected).expect("nonempty selection");
        line(
            frame,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
            &format!(
                "{} | {} | {}-{} / {}",
                row.scope,
                row.kind,
                self.row_offset + 1,
                self.row_offset + self.rows.len(),
                self.row_total
            ),
            theme.muted,
        );
    }

    fn breadcrumb(&self) -> String {
        if let Some(row) = &self.detail_row {
            return format!("{}:{}", row.scope, row.id);
        }
        match &self.target {
            Some(InstructionInspectionTarget::Session) => {
                "Session · exact active instructions".into()
            }
            Some(InstructionInspectionTarget::Repository(key)) => self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.repositories.iter().find(|store| &store.key == key))
                .map_or_else(
                    || "Repository".into(),
                    |store| {
                        format!(
                            "{} · {}",
                            store.kind,
                            store.branch.as_deref().unwrap_or("detached / no branch")
                        )
                    },
                ),
            _ => "Choose a resource".into(),
        }
    }

    fn render_detail(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if area.width == 0 {
            return;
        }
        let title = if let Some(revision) = &self.revision_selection {
            format!(
                "{} {}{}",
                self.view_label(),
                short(&revision.from),
                revision
                    .to
                    .as_ref()
                    .map_or_else(String::new, |to| format!(" -> {}", short(to)))
            )
        } else {
            self.view_label().into()
        };
        let inner = self.pane(frame, area, &title, Pane::Detail, theme);
        if inner.height == 0 {
            return;
        }
        line(
            frame,
            Rect::new(inner.x, inner.y, inner.width, 1),
            &self.breadcrumb(),
            theme.accent,
        );
        let body = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(2),
        );
        let progress = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        if self.pending.is_some() && self.text.is_none() && !self.history_visible {
            text(
                frame,
                body,
                "Loading selected view…\nX cancels loading. Esc returns to browsing.",
                theme.warning,
            );
            return;
        }
        if self.history_visible {
            let selected = self.history.get(self.history_selected);
            let info = if let Some(base) = &self.history_base {
                format!(
                    "Base {} -> {} | B Compare",
                    short(base),
                    selected.map_or("select a revision", |entry| short(&entry.commit))
                )
            } else {
                "Enter: read · A: mark base for comparison".into()
            };
            line(frame, progress, &info, theme.muted);
            if self.history.is_empty() {
                text(
                    frame,
                    body,
                    if self.pending.is_some() {
                        "Loading Git history…"
                    } else {
                        "No commits for this source at the inspected HEAD.\nEsc returns without changing anything."
                    },
                    theme.muted,
                );
                return;
            }
            let info_height = if body.height >= 8 { 3 } else { 0 };
            let list = Rect::new(
                body.x,
                body.y,
                body.width,
                body.height.saturating_sub(info_height),
            );
            let labels = self
                .history
                .iter()
                .map(|entry| {
                    format!(
                        "{}{} {}",
                        if self.history_base.as_deref() == Some(&entry.commit) {
                            "[base] "
                        } else {
                            ""
                        },
                        short(&entry.commit),
                        entry.subject
                    )
                })
                .collect::<Vec<_>>();
            self.draw_rows(
                frame,
                list,
                &labels,
                self.history_selected,
                Pane::Detail,
                theme,
            );
            if info_height > 0
                && let Some(selected) = self.history.get(self.history_selected)
            {
                text(
                    frame,
                    Rect::new(body.x, list.bottom(), body.width, info_height),
                    &format!(
                        "{}\n{} · {}\n{} changed paths · Enter to inspect",
                        selected.subject,
                        selected.date,
                        selected.author,
                        selected.paths.len()
                    ),
                    theme.muted,
                );
            }
            return;
        }
        let Some(page) = &self.text else {
            if let Some(row) = self.rows.get(self.selected) {
                text(
                    frame,
                    body,
                    &format!(
                        "{}\n\n{} · {}\n{}\n\nEnter opens the complete source.\nSpace shows every available action.\n\nSource is on disk. A preview is not an activation. Session offers the exact stored prompt.",
                        row.name,
                        row.scope,
                        row.kind,
                        if !row.valid {
                            "Validation failed: open Overview for details."
                        } else if row.high_impact {
                            "High-impact project redefinition. Inspect before editing."
                        } else if !row.effective {
                            "Shadowed definition; still available for explicit inspection."
                        } else {
                            "Effective definition for unqualified lookup."
                        }
                    ),
                    Style::default(),
                );
            } else {
                text(
                    frame,
                    body,
                    "Choose a repository or resource.\nEnter opens it.\nSpace lists actions.\n\nThis manager is read only.",
                    theme.muted,
                );
            }
            return;
        };
        let width = body.width.saturating_sub(1).max(1);
        let key = (page.document.clone(), page.offset, width);
        if self.wrap_key.as_ref() != Some(&key) {
            self.wrapped = wrap(&page.text, width);
            self.wrap_key = Some(key);
        }
        self.detail_height = usize::from(body.height).max(1);
        self.scroll = self
            .scroll
            .min(self.wrapped.len().saturating_sub(self.detail_height));
        let lines = self
            .wrapped
            .iter()
            .skip(self.scroll)
            .take(self.detail_height)
            .map(|value| Line::raw(value.clone()))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).style(if self.last_error {
                theme.error
            } else {
                Style::default()
            }),
            Rect::new(body.x, body.y, width, body.height),
        );
        scrollbar(frame, body, self.scroll, self.wrapped.len(), theme);
        let position = if page.next.is_none() && page.offset == 0 {
            format!(
                "Complete · lines {}-{}/{}",
                self.scroll + 1,
                (self.scroll + self.detail_height).min(self.wrapped.len()),
                self.wrapped.len()
            )
        } else {
            format!(
                "Bytes {}-{}/{} · N/P pages",
                page.offset,
                page.offset + page.text.len(),
                page.total_bytes
            )
        };
        line(frame, progress, &position, theme.muted);
    }

    fn render_menu(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let menu = self.menu.as_ref().expect("menu open");
        let popup = if area.width >= 70 && area.height >= 20 {
            let height = if menu.explanation {
                area.height - 4
            } else {
                u16::try_from(menu.matches().len().saturating_add(8))
                    .unwrap_or(u16::MAX)
                    .max(10)
                    .min(area.height - 4)
            };
            Rect::new(
                area.x + (area.width - 68) / 2,
                area.y + (area.height - height) / 2,
                68,
                height,
            )
        } else {
            area
        };
        let menu = self.menu.as_mut().expect("menu open");
        let block = Block::default()
            .title(format!(" {} ", menu.title))
            .borders(Borders::ALL)
            .border_style(theme.accent);
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        if menu.explanation {
            let indices = menu.matches();
            let explanation = indices
                .get(menu.selected)
                .map(|index| {
                    let item = &menu.items[*index];
                    format!(
                        "{} [{}]\n\n{}\n\n{}",
                        item.label,
                        item.key,
                        item.disabled
                            .as_deref()
                            .unwrap_or("Available for this selection"),
                        item.hint
                    )
                })
                .unwrap_or_else(|| "No selected action. Escape returns to the menu.".into());
            let lines = wrap(&explanation, inner.width.saturating_sub(1).max(1));
            let body = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            );
            menu.explanation_scroll = menu
                .explanation_scroll
                .min(lines.len().saturating_sub(usize::from(body.height)));
            frame.render_widget(
                Paragraph::new(
                    lines
                        .iter()
                        .skip(menu.explanation_scroll)
                        .take(usize::from(body.height))
                        .map(|line| Line::raw(line.clone()))
                        .collect::<Vec<_>>(),
                ),
                body,
            );
            scrollbar(frame, body, menu.explanation_scroll, lines.len(), theme);
            self.buttons(
                frame,
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                &[(KeyCode::Esc, "Esc Back to menu")],
                theme,
            );
            return;
        }
        line(
            frame,
            Rect::new(inner.x, inner.y, inner.width, 1),
            &format!(
                "Find: {}|",
                tail_cells(&menu.query, usize::from(inner.width.saturating_sub(7)))
            ),
            theme.accent,
        );
        let note_height = if inner.height >= 12 { 4 } else { 2 };
        let list = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(note_height + 2),
        );
        let matches = menu.matches();
        menu.selected = menu.selected.min(matches.len().saturating_sub(1));
        let start = menu
            .selected
            .saturating_sub(usize::from(list.height) / 2)
            .min(matches.len().saturating_sub(usize::from(list.height)));
        for (position, index) in matches
            .iter()
            .enumerate()
            .skip(start)
            .take(usize::from(list.height))
        {
            let item = &menu.items[*index];
            let label = format!(
                "{}{}",
                item.label,
                if item.disabled.is_some() {
                    " [unavailable]"
                } else {
                    ""
                }
            );
            let rect = Rect::new(
                list.x,
                list.y + u16::try_from(position - start).unwrap_or(0),
                list.width.saturating_sub(1),
                1,
            );
            line(
                frame,
                rect,
                &format!(
                    "{} {}",
                    if position == menu.selected { ">" } else { " " },
                    label
                ),
                if position == menu.selected {
                    theme.selected
                } else if item.disabled.is_some() {
                    theme.muted
                } else {
                    Style::default()
                },
            );
            self.menu_hits.push((rect, position));
        }
        if matches.is_empty() {
            line(frame, list, "No matching actions.", theme.muted);
        }
        scrollbar(frame, list, start, matches.len(), theme);
        if let Some(index) = matches.get(menu.selected) {
            let item = &menu.items[*index];
            let info = format!(
                "{}{}\n{}",
                item.label,
                if item.key.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", item.key)
                },
                item.disabled.as_deref().unwrap_or(&item.hint)
            );
            text(
                frame,
                Rect::new(inner.x, list.bottom(), inner.width, note_height),
                &info,
                if item.disabled.is_some() {
                    theme.warning
                } else {
                    theme.muted
                },
            );
        }
        self.buttons(
            frame,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            &[
                (KeyCode::Enter, "Enter Choose"),
                (KeyCode::Esc, "Esc Back"),
                (KeyCode::Char('?'), "? Details"),
            ],
            theme,
        );
        // At the 24-column floor both controls remain keyboard reachable; the
        // explicit Back button receives the remaining cells rather than vanishing.
        if !self.controls.iter().any(|(_, key)| *key == KeyCode::Esc) {
            let rect = Rect::new(inner.right().saturating_sub(8), inner.bottom() - 1, 8, 1);
            line(frame, rect, "Esc Back", theme.accent);
            self.controls.push((rect, KeyCode::Esc));
        }
    }

    fn render_help(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let view_help = VIEWS
            .iter()
            .map(|(_, key, label, hint)| format!("{key}  {label}\n   {hint}"))
            .collect::<Vec<_>>()
            .join("\n");
        let help = format!(
            "INSTRUCTION MANAGER\n\nOpening and browsing are read-only. Mutations use explicit reviewed actions.\n\nBrowse with arrows or J/K. Enter opens a resource or repository.\nSpace / : / Ctrl-P: searchable Actions. T: choose a view.\nF: filters with explicit values. /: search resources.\nSearch supports arrows, Home/End, Delete, Backspace, Ctrl-U and paste.\n\nEsc / Left / Backspace returns from revision to history, from detail to browsing, then repositories. Q closes directly.\nTab / Shift-Tab moves focus. F1 repositories, F2 resources, F3 reading.\nZ expands/collapses the focused pane. PageUp/Down scroll.\nN/P changes transport page. At content page boundaries, scrolling continues to the adjacent page. Home/End stays within the page.\n\nVIEWS\n{view_help}\n\nHISTORY\nEnter reads a revision. I shows complete commit details. A marks a base. B compares base to selected.\nEsc restores the same history selection and base. Reading and comparing never apply changes. Named Actions provides Restore and Export.\n\nFILTER SHORTCUTS\nS scope, V validation, E effectiveness, O origin, G toggles redefinitions.\nC clears all filters.\nCurrent: {}\n\nR refreshes sources. X cancels loading. No file, session prompt, model, branch or remote is changed.\nSource is the original file. Preview uses current source and captured inspection inputs. Session / Stored system prompt is exact active text, not a new preview.\nComplete content is paged, never shortened. Scope/identity and position stay visible while scrolling.\nTerminal control characters are shown as escapes. Mouse accelerates keyboard actions; Shift normally bypasses mouse capture for terminal text selection.\n\nAt 80 and 60 columns this is one navigable pane. At wide widths repositories/list/reader share space. At least 24x8 is required.\n\nEDITING\nCtrl-E edits a selected source. Actions offers create, redefine, addendum, Copy, clear, rename/delete, import, restore/export and repository controls.\nInside a draft: B external body editor, M typed metadata, R Review, S Save. Space or ? opens named draft actions. Tab/Shift-Tab selects another affected file. Esc closes and preserves the draft; Discard is separate and confirmed. AGENTS.md saves its working file without a parent commit.\nReview current target and retained values before reusing a stale form. Recover drafts and operations lists server records; Recover local unsent changes lists private client intent. Recovery never automatically replays Save or network operations.\nRepository operations display their destination and consequences, then Y applies once; Enter/N returns without applying. Ordinary reviewed Save needs no second confirmation.\n\nEsc closes help and restores your reading position.",
            self.filter_summary()
        );
        let body = Rect::new(area.x, area.y, area.width, area.height - 1);
        let lines = wrap(&help, body.width.saturating_sub(1).max(1));
        self.help_scroll = self
            .help_scroll
            .min(lines.len().saturating_sub(usize::from(body.height)));
        frame.render_widget(
            Paragraph::new(
                lines
                    .iter()
                    .skip(self.help_scroll)
                    .take(usize::from(body.height))
                    .map(|line| Line::raw(line.clone()))
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        scrollbar(frame, body, self.help_scroll, lines.len(), theme);
        self.buttons(
            frame,
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
            &[
                (KeyCode::Esc, "Esc Back"),
                (KeyCode::Char('?'), "? Close help"),
            ],
            theme,
        );
    }

    fn capture(&self, frame: &mut Frame, area: Rect) {
        if !crate::tui::visual_debug::is_enabled() {
            return;
        }
        use crate::tui::visual_debug::{
            FrameCaptureBuilder, MessageCapture, RectCapture, WidgetPlacementCapture,
        };
        let mut capture = FrameCaptureBuilder::new(area.width, area.height);
        capture.state.status = "instruction manager (read-only)".into();
        capture.state.scroll_offset = self.scroll;
        capture.render_order.push("instruction_manager".into());
        let cells = frame.buffer_mut();
        let mut viewport = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                viewport.push_str(cells[(x, y)].symbol());
            }
            viewport.push('\n');
        }
        capture.rendered_text.recent_messages.push(MessageCapture {
            role: "instruction-manager-viewport".into(),
            content_len: viewport.len(),
            content_preview: viewport,
        });
        for (index, rect) in self
            .areas
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.width > 0)
        {
            capture
                .layout
                .widget_placements
                .push(WidgetPlacementCapture {
                    kind: format!("instruction-pane-{index}"),
                    side: "overlay".into(),
                    rect: RectCapture {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    },
                });
        }
        for (rect, key) in &self.controls {
            capture
                .layout
                .widget_placements
                .push(WidgetPlacementCapture {
                    kind: format!("instruction-action-{}", format_key(*key)),
                    side: "overlay".into(),
                    rect: RectCapture {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    },
                });
        }
        crate::tui::visual_debug::record_frame(capture.build());
    }
}
fn short(value: &str) -> &str {
    value.get(..10).unwrap_or(value)
}
fn line(frame: &mut Frame, area: Rect, value: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(clip(
            &safe(value).replace(['\n', '\t'], " "),
            usize::from(area.width),
        ))
        .style(style),
        area,
    );
}
fn text(frame: &mut Frame, area: Rect, value: &str, style: Style) {
    let lines = wrap(value, area.width.max(1));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .take(usize::from(area.height))
                .map(Line::raw)
                .collect::<Vec<_>>(),
        )
        .style(style),
        area,
    );
}
fn scrollbar(frame: &mut Frame, area: Rect, start: usize, total: usize, theme: &Theme) {
    let height = usize::from(area.height);
    if height == 0 || total <= height || area.width < 2 {
        return;
    }
    let position =
        start.saturating_mul(height.saturating_sub(1)) / total.saturating_sub(height).max(1);
    for y in 0..height {
        frame.render_widget(
            Paragraph::new(if y == position.min(height - 1) {
                "#"
            } else {
                "|"
            })
            .style(theme.muted),
            Rect::new(
                area.right() - 1,
                area.y + u16::try_from(y).unwrap_or(0),
                1,
                1,
            ),
        );
    }
}
fn clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let line = Line::raw(text);
    let mut result = String::new();
    let mut used = 0;
    for grapheme in line.styled_graphemes(Style::default()) {
        let size = grapheme.symbol.width();
        if used + size > width - 1 {
            break;
        }
        result.push_str(grapheme.symbol);
        used += size;
    }
    result.push('…');
    result
}
pub(super) fn tail_cells(text: &str, width: usize) -> String {
    let line = Line::raw(text);
    let graphemes = line.styled_graphemes(Style::default()).collect::<Vec<_>>();
    let mut start = graphemes.len();
    let mut used = 0;
    for grapheme in graphemes.iter().rev() {
        let size = grapheme.symbol.width();
        if used + size > width {
            break;
        }
        start -= 1;
        used += size;
    }
    graphemes[start..]
        .iter()
        .map(|grapheme| grapheme.symbol)
        .collect()
}
pub(super) fn safe(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_control() && !matches!(ch, '\n' | '\t') {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{{{:04X}}}", u32::from(ch));
        } else {
            out.push(ch);
        }
    }
    out
}
pub(super) fn wrap(text: &str, width: u16) -> Vec<String> {
    let limit = usize::from(width).max(1);
    let mut lines = Vec::new();
    for source in safe(text).split('\n') {
        let expanded = source.replace('\t', "    ");
        let source = Line::raw(expanded);
        let mut line = String::new();
        let mut columns = 0;
        for grapheme in source.styled_graphemes(Style::default()) {
            let size = grapheme.symbol.width();
            if columns + size > limit && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                columns = 0;
            }
            line.push_str(grapheme.symbol);
            columns += size;
        }
        lines.push(line);
    }
    lines
}
