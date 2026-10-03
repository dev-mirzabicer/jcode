//! Presentation-only projection of the manager state. No filesystem or
//! transport I/O happens here; hit regions are recorded for mouse input.
use super::*;
use jcode_tui_style::theme;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

/// Below this width list and detail stack instead of sitting side by side.
pub(crate) const SPLIT_WIDTH: u16 = 100;

struct Styles {
    accent: Style,
    muted: Style,
    error: Style,
    warning: Style,
    success: Style,
    rule: Style,
}

impl Styles {
    fn new() -> Self {
        let monochrome = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let color = |value: Color| {
            if monochrome {
                Style::default()
            } else {
                Style::default().fg(value)
            }
        };
        Self {
            accent: color(theme::accent_color()),
            muted: color(theme::dim_color()),
            error: color(theme::error_color()),
            warning: color(theme::warning_color()),
            success: color(theme::success_color()),
            rule: color(theme::border_color()),
        }
    }
    fn tone(&self, tone: Tone) -> Style {
        match tone {
            Tone::Normal => Style::default(),
            Tone::Muted => self.muted,
            Tone::Accent => self.accent,
            Tone::Good => self.success,
            Tone::Warn => self.warning,
            Tone::Bad => self.error,
        }
    }
}

fn safe(value: &str) -> String {
    crate::message::strip_ansi_escape_sequences(value)
        .chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Wrap display text to terminal cells, preferring spaces and breaking long
/// tokens such as paths and IDs. Overlay heights, scrolling and hit regions are
/// computed from these exact rows.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut rows = Vec::new();
    for paragraph in safe(text).split('\n') {
        let mut line = String::new();
        for ch in paragraph.chars() {
            let cell = ch.width().unwrap_or(0);
            while !line.is_empty() && line.width() + cell > width {
                match line.rfind(' ').filter(|index| *index > 0) {
                    Some(index) => {
                        let rest = line[index + 1..].to_string();
                        rows.push(line[..index].trim_end().to_string());
                        line = rest;
                    }
                    None => rows.push(std::mem::take(&mut line)),
                }
            }
            if ch == ' ' && line.is_empty() && !rows.is_empty() {
                continue;
            }
            line.push(ch);
        }
        rows.push(line);
    }
    rows
}

fn fit(text: &str, width: usize) -> String {
    let text = safe(text).replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut next = out.clone();
        next.push(ch);
        if next.width() + 1 > width {
            break;
        }
        out.push(ch);
    }
    if width > 0 {
        out.push('…');
    }
    out
}

fn short_label(section: Section) -> &'static str {
    match section {
        Section::Organization => "Org",
        Section::Sessions => "Sessions",
        Section::Operations => "Ops",
        Section::Permissions => "Perms",
        Section::Closeout => "Closeout",
        Section::Backup => "Backup",
        Section::Runtime => "Runtime",
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

impl WorkspaceManager {
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.hit.clear();
        self.list_area = Rect::default();
        self.dimensions = (area.width, area.height);
        frame.render_widget(Clear, area);
        let styles = Styles::new();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Workspace needs {MIN_WIDTH}×{MIN_HEIGHT}\nResize or Esc to close"
                ))
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let inner = area.inner(Margin::new(1, 0));
        let [header, tabs, body, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(inner);
        self.render_header(frame, header, &styles);
        self.render_tabs(frame, tabs, &styles);
        self.render_body(frame, body, &styles);
        let status_text = if self.uncertain.is_empty() {
            self.status.clone()
        } else {
            format!(
                "{} unknown outcome(s) · U review · {}",
                self.uncertain.len(),
                self.status
            )
        };
        let status_style = if self.uncertain.is_empty() {
            self.outcomes
                .front()
                .filter(|(_, text)| *text == self.status)
                .map_or(styles.muted, |(tone, _)| styles.tone(*tone))
        } else {
            styles.warning
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                fit(&status_text, status.width as usize),
                status_style,
            )),
            status,
        );
        self.render_footer(frame, footer, &styles);
        if self.help {
            self.render_help(frame, inner, &styles);
        } else if self.show_uncertain {
            self.render_uncertain(frame, inner, &styles);
        } else if self.form.is_some() {
            self.render_form(frame, inner, &styles);
        } else if self.confirm.is_some() {
            self.render_confirm(frame, inner, &styles);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let right = if !self.remote {
            "local client".to_string()
        } else if !self.connected {
            "runtime unreachable".to_string()
        } else {
            match &self.catalog {
                Some(Ok(status)) => format!("catalog r{}", status.revision),
                Some(Err(_)) => "catalog unavailable".into(),
                None => "connecting…".into(),
            }
        };
        let title = "Workspace";
        let gap = (area.width as usize).saturating_sub(title.width() + right.width());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(title, styles.accent.add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(gap)),
                Span::styled(
                    right,
                    if self.connected {
                        styles.muted
                    } else {
                        styles.warning
                    },
                ),
            ])),
            area,
        );
    }

    fn render_tabs(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let mut spans = Vec::new();
        let mut x = area.x;
        let narrow = area.width < 96;
        for (index, section) in Section::ALL.into_iter().enumerate() {
            let label = format!(
                "{} {}",
                index + 1,
                if narrow {
                    short_label(section)
                } else {
                    section.label()
                }
            );
            let width = label.width() as u16;
            if x + width > area.x + area.width {
                break;
            }
            let style = if section == self.section {
                styles
                    .accent
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                styles.muted
            };
            self.hit
                .push((Rect::new(x, area.y, width, 1), Hit::Section(section)));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw("  "));
            x += width + 2;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn subtitle(&self) -> String {
        match self.section {
            Section::Organization => {
                let kind = match self.org.kind {
                    None => "all kinds",
                    Some(EntityKind::Project) => "projects",
                    Some(EntityKind::Repository) => "repositories",
                    Some(EntityKind::WorkArea) => "work areas",
                    Some(EntityKind::Location) => "locations",
                };
                let count = self.org.page.as_ref().map_or(String::new(), |page| {
                    let start = self.org.previous.len() * PAGE as usize;
                    format!(
                        " · {}–{} of {}",
                        if page.items.is_empty() { 0 } else { start + 1 },
                        start + page.items.len(),
                        page.total
                    )
                });
                format!("{kind} · {:?}{count}", self.org.visibility)
            }
            Section::Sessions => match self.sessions.target {
                Some(target) => format!(
                    "sessions in {}",
                    self.known
                        .get(&target.to_string())
                        .map(describe::entity_name)
                        .unwrap_or_else(|| target.to_string())
                ),
                None => "this client, inspected and indexed sessions".into(),
            },
            Section::Operations => format!(
                "{} operations{}",
                if self.ops.all { "all" } else { "unfinished" },
                self.ops
                    .page
                    .as_ref()
                    .map_or(String::new(), |page| format!(" · {} total", page.total))
            ),
            Section::Permissions => format!("{:?}", self.perms.mode).to_lowercase(),
            Section::Closeout => "checkout closeouts, newest first".into(),
            Section::Backup => "catalog snapshots".into(),
            Section::Runtime => "interrupted turns".into(),
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let rows = self.rows();
        let [title, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                fit(&self.subtitle(), title.width as usize),
                styles.muted,
            )),
            title,
        );
        self.list_area = list;
        let height = list.height as usize;
        let selected = self.selected_index(&rows);
        if let Some(selected) = selected {
            if selected < self.list_offset {
                self.list_offset = selected;
            } else if selected >= self.list_offset + height {
                self.list_offset = selected + 1 - height;
            }
        }
        self.list_offset = self.list_offset.min(rows.len().saturating_sub(1));
        if rows.is_empty() {
            let empty = match self.section {
                Section::Organization if self.catalog_ready() => {
                    "No entries. n new project · g register an existing path"
                }
                Section::Operations if self.catalog_ready() => "Nothing in progress.",
                Section::Permissions if self.catalog_ready() => {
                    "Nothing here. v switches view · n new grant"
                }
                Section::Closeout if self.catalog_ready() => {
                    "No closeouts. Start one from Organization (o on a checkout)."
                }
                Section::Backup if self.catalog_ready() => "No snapshots yet. b backs up now.",
                Section::Runtime => "No interrupted turns.",
                _ => "",
            };
            frame.render_widget(
                Paragraph::new(Span::styled(empty, styles.muted)).wrap(Wrap { trim: true }),
                list,
            );
            return;
        }
        let lines = rows
            .iter()
            .enumerate()
            .skip(self.list_offset)
            .take(height)
            .map(|(index, row)| {
                let mut style = styles.tone(row.tone);
                if Some(index) == selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                self.hit.push((
                    Rect::new(
                        list.x,
                        list.y + (index - self.list_offset) as u16,
                        list.width,
                        1,
                    ),
                    Hit::Row(index),
                ));
                Line::from(Span::styled(fit(&row.text, list.width as usize), style))
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), list);
    }

    fn render_detail(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let lines = describe::detail(self)
            .into_iter()
            .map(|(tone, text)| Line::from(Span::styled(safe(&text), styles.tone(tone))))
            .collect::<Vec<_>>();
        let max = lines.len().saturating_sub(1);
        self.detail_scroll = self.detail_scroll.min(max);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll as u16, 0)),
            area,
        );
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        if area.width >= SPLIT_WIDTH {
            let [list, rule, detail] = Layout::horizontal([
                Constraint::Percentage(42),
                Constraint::Length(2),
                Constraint::Min(20),
            ])
            .areas(area);
            self.render_list(frame, list, styles);
            frame.render_widget(
                Paragraph::new(
                    (0..rule.height)
                        .map(|_| Line::from(Span::styled("│", styles.rule)))
                        .collect::<Vec<_>>(),
                ),
                rule,
            );
            self.render_detail(frame, detail, styles);
        } else if self.detail {
            self.render_detail(frame, area, styles);
        } else {
            let detail_height = (area.height / 3).max(3).min(area.height.saturating_sub(4));
            if area.height >= 10 {
                let [list, rule, detail] = Layout::vertical([
                    Constraint::Min(3),
                    Constraint::Length(1),
                    Constraint::Length(detail_height),
                ])
                .areas(area);
                self.render_list(frame, list, styles);
                frame.render_widget(
                    Paragraph::new(Span::styled("─".repeat(rule.width as usize), styles.rule)),
                    rule,
                );
                self.render_detail(frame, detail, styles);
            } else {
                self.render_list(frame, area, styles);
            }
        }
    }

    fn render_footer(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        // Help and Close come first so they stay visible when the section's
        // actions overflow; `?` lists every action.
        let mut actions = actions::available(self);
        actions.sort_by_key(|spec| {
            !matches!(spec.action, actions::Action::Help | actions::Action::Close)
        });
        if area.width < SPLIT_WIDTH {
            actions.insert(
                0,
                actions::ActionSpec {
                    key: '⏎',
                    label: if self.detail { "List" } else { "Details" },
                    action: actions::Action::Refresh,
                },
            );
        }
        let mut lines = vec![Vec::new(), Vec::new()];
        let mut line = 0;
        let mut x = area.x;
        for spec in actions {
            let text = format!("{} {}", spec.key, spec.label);
            let width = text.width() as u16;
            if x + width > area.x + area.width {
                line += 1;
                x = area.x;
                if line >= lines.len() {
                    break;
                }
            }
            if spec.key != '⏎' {
                self.hit.push((
                    Rect::new(x, area.y + line as u16, width, 1),
                    Hit::Action(spec.action),
                ));
            }
            lines[line].push(Span::styled(spec.key.to_string(), styles.accent));
            lines[line].push(Span::raw(format!(" {}", spec.label)));
            lines[line].push(Span::styled(" · ", styles.muted));
            x += width + 3;
        }
        frame.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()),
            area,
        );
    }

    fn overlay(
        &self,
        frame: &mut Frame,
        area: Rect,
        width: u16,
        height: u16,
        title: &str,
        styles: &Styles,
    ) -> Rect {
        let rect = centered(area, width, height);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(styles.accent)
            .title(Span::styled(
                format!(" {title} "),
                styles.accent.add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        inner
    }

    fn render_help(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let mut lines = vec![
            Line::from(Span::styled("Navigation", styles.accent)),
            Line::from("1–7 or Tab/Shift-Tab  sections · ↑↓ j k  select · PgUp PgDn"),
            Line::from("Enter  details (narrow) · Esc/q  back or close · r refresh"),
            Line::from("Every effect opens a review. y confirms, n or Esc declines; Enter presses"),
            Line::from(
                "the highlighted button (Cancel by default). Escalations need a typed word.",
            ),
            Line::from(""),
            Line::from(Span::styled(
                format!("{} actions", self.section.label()),
                styles.accent,
            )),
        ];
        for spec in actions::available(self) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>3}  ", spec.key), styles.accent),
                Span::raw(spec.label),
            ]));
        }
        lines.push(Line::from(""));
        for row in wrap(
            "Native write policy is not a shell sandbox. Same-user clients are trusted, not proof of a human.",
            area.width.min(84).saturating_sub(2) as usize,
        ) {
            lines.push(Line::from(Span::styled(row, styles.muted)));
        }
        let inner = self.overlay(
            frame,
            area,
            84,
            lines.len() as u16 + 2,
            "Workspace help",
            styles,
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    fn render_uncertain(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let mut lines = vec![
            Line::from("These effects were sent but their replies were lost."),
            Line::from(Span::styled(
                "Inspect current state first. R retries the SAME request (the owner deduplicates it). D dismisses.",
                styles.muted,
            )),
            Line::from(""),
        ];
        for item in &self.uncertain {
            lines.push(Line::from(Span::styled(
                format!("• {}", item.label),
                styles.warning,
            )));
        }
        let inner = self.overlay(
            frame,
            area,
            90,
            lines.len() as u16 + 2,
            "Unknown outcomes",
            styles,
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    fn render_form(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let Some((_, form)) = &self.form else {
            return;
        };
        let width = area.width.min(96);
        let inner_width = width.saturating_sub(4) as usize;
        let mut lines: Vec<(Line, Option<Hit>)> = Vec::new();
        for intro in &form.intro {
            for row in wrap(intro, width.saturating_sub(2) as usize) {
                lines.push((Line::from(Span::styled(row, styles.muted)), None));
            }
        }
        if !form.intro.is_empty() {
            lines.push((Line::from(""), None));
        }
        let visible = form.visible();
        let mut focus_line = 0;
        for (position, index) in visible.iter().enumerate() {
            let field = &form.fields[*index];
            let focused = form.focus == position;
            let marker = if focused { "▶ " } else { "  " };
            let required = if field.required && matches!(field.kind, form::FieldKind::Text) {
                " (required)"
            } else {
                ""
            };
            lines.push((
                Line::from(Span::styled(
                    format!("{marker}{}{required}", field.label),
                    if focused {
                        styles.accent.add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )),
                Some(Hit::FormField(position)),
            ));
            if focused {
                focus_line = lines.len();
            }
            let value = match &field.kind {
                form::FieldKind::Text => {
                    if focused {
                        let (before, after) =
                            field.value.split_at(field.cursor.min(field.value.len()));
                        let mut cursor = after.chars();
                        let at = cursor
                            .next()
                            .map(String::from)
                            .unwrap_or_else(|| " ".into());
                        Line::from(vec![
                            Span::raw("    "),
                            Span::raw(safe(before)),
                            Span::styled(
                                safe(&at),
                                Style::default().add_modifier(Modifier::REVERSED),
                            ),
                            Span::raw(safe(cursor.as_str())),
                        ])
                    } else if field.value.is_empty() {
                        Line::from(Span::styled("    —", styles.muted))
                    } else {
                        Line::from(format!(
                            "    {}",
                            fit(&field.value, inner_width.saturating_sub(4))
                        ))
                    }
                }
                form::FieldKind::Choice(_) => Line::from(vec![
                    Span::raw("    "),
                    Span::styled(if focused { "◀ " } else { "" }, styles.accent),
                    Span::raw(fit(&field.display(), inner_width.saturating_sub(8))),
                    Span::styled(if focused { " ▶" } else { "" }, styles.accent),
                ]),
                form::FieldKind::Toggle => Line::from(format!("    {}", field.display())),
            };
            lines.push((value, Some(Hit::FormField(position))));
            if focused && !field.hint.is_empty() {
                lines.push((
                    Line::from(Span::styled(format!("    {}", field.hint), styles.muted)),
                    None,
                ));
            }
        }
        lines.push((Line::from(""), None));
        let submit_focused = form.on_submit();
        let cancel_focused = form.on_cancel();
        lines.push((
            Line::from(vec![
                Span::styled(
                    format!("[ {} ]", form.submit),
                    if submit_focused {
                        styles
                            .accent
                            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
                    } else {
                        styles.accent
                    },
                ),
                Span::raw("   "),
                Span::styled(
                    "[ Cancel ]",
                    if cancel_focused {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        styles.muted
                    },
                ),
            ]),
            Some(Hit::FormSubmit),
        ));
        if submit_focused || cancel_focused {
            focus_line = lines.len();
        }
        // The reason and key hints stay visible below the scrolled fields.
        let mut footer: Vec<Line> = Vec::new();
        if let Some(error) = &form.error {
            for row in wrap(error, width.saturating_sub(2) as usize) {
                footer.push(Line::from(Span::styled(row, styles.error)));
            }
        }
        footer.push(Line::from(Span::styled(
            fit(
                "Tab/↑↓ fields · ←→/Space choose · Ctrl+S submit · Esc cancel",
                width.saturating_sub(2) as usize,
            ),
            styles.muted,
        )));
        let title = form.title.clone();
        let height = ((lines.len() + footer.len()) as u16 + 2).min(area.height);
        let inner = self.overlay(frame, area, width, height, &title, styles);
        let footer_rows = footer.len().min(inner.height.saturating_sub(1) as usize);
        let visible_rows = (inner.height as usize).saturating_sub(footer_rows);
        let mut offset = form.offset;
        if focus_line > offset + visible_rows {
            offset = focus_line - visible_rows;
        } else if focus_line > 0 && focus_line - 1 < offset {
            offset = focus_line - 1;
        }
        if let Some((_, form)) = &mut self.form {
            form.offset = offset;
        }
        let mut rendered = Vec::new();
        for (row, (line, hit)) in lines
            .into_iter()
            .skip(offset)
            .take(visible_rows)
            .enumerate()
        {
            if let Some(hit) = hit {
                let rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
                if hit == Hit::FormSubmit {
                    let submit_width = (form_submit_width(&self.form) as u16).min(inner.width);
                    self.hit
                        .push((Rect::new(inner.x, rect.y, submit_width, 1), Hit::FormSubmit));
                    self.hit.push((
                        Rect::new(inner.x + submit_width + 3, rect.y, 10, 1),
                        Hit::FormCancel,
                    ));
                } else {
                    self.hit.push((rect, hit));
                }
            }
            rendered.push(line);
        }
        let body = Rect::new(inner.x, inner.y, inner.width, visible_rows as u16);
        frame.render_widget(Paragraph::new(rendered), body);
        let start = footer.len() - footer_rows;
        let footer_area = Rect::new(
            inner.x,
            inner.y + visible_rows as u16,
            inner.width,
            footer_rows as u16,
        );
        frame.render_widget(Paragraph::new(footer.split_off(start)), footer_area);
    }

    fn render_confirm(&mut self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let Some(confirm) = &self.confirm else {
            return;
        };
        let width = area.width.min(96);
        let text_width = width.saturating_sub(2) as usize;
        let mut lines = confirm
            .lines
            .iter()
            .flat_map(|(tone, text)| {
                wrap(text, text_width)
                    .into_iter()
                    .map(|row| Line::from(Span::styled(row, styles.tone(*tone))))
            })
            .collect::<Vec<_>>();
        lines.push(Line::from(""));
        if let Some(word) = confirm.typed {
            lines.push(Line::from(vec![
                Span::styled(format!("Type `{word}` to confirm: "), styles.warning),
                Span::raw(safe(&confirm.input)),
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(
                "[ Confirm ]",
                if confirm.yes {
                    styles
                        .accent
                        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    styles.accent
                },
            ),
            Span::raw("   "),
            Span::styled(
                "[ Cancel ]",
                if confirm.yes {
                    styles.muted
                } else {
                    Style::default().add_modifier(Modifier::REVERSED)
                },
            ),
        ]));
        lines.push(Line::from(Span::styled(
            if confirm.typed.is_some() {
                "Tab switch · Enter on Confirm sends · Esc cancels"
            } else {
                "y confirm · n/Esc cancel · Tab switch · Enter activates"
            },
            styles.muted,
        )));
        let title = confirm.title.clone();
        let scroll = confirm.scroll;
        let height = (lines.len() as u16 + 2).min(area.height);
        let inner = self.overlay(frame, area, width, height, &title, styles);
        let total = lines.len();
        let max_scroll = total.saturating_sub(inner.height as usize);
        let scroll = scroll.min(max_scroll);
        if let Some(confirm) = &mut self.confirm {
            confirm.scroll = scroll;
        }
        // Buttons are the second-to-last line; keep them reachable by mouse.
        let button_row = total - 2;
        if button_row >= scroll && button_row - scroll < inner.height as usize {
            let y = inner.y + (button_row - scroll) as u16;
            self.hit
                .push((Rect::new(inner.x, y, 11, 1), Hit::ConfirmYes));
            self.hit
                .push((Rect::new(inner.x + 14, y, 10, 1), Hit::ConfirmNo));
        }
        frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
    }
}

fn form_submit_width(form: &Option<(actions::FormKind, Form)>) -> usize {
    form.as_ref().map_or(0, |(_, form)| form.submit.width() + 4)
}
