//! Presentation-only projection of the monitor state. No filesystem or transport I/O.
use super::*;
use jcode_tool_types::RunState;
use jcode_tui_style::theme;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use unicode_width::UnicodeWidthStr;

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
    fn state(&self, row: &TaskRow) -> Style {
        if row.run.stop_cause.is_some() && !row.run.state.terminal() {
            return self.warning;
        }
        match row.run.state {
            RunState::Completed => self.success,
            RunState::Failed => self.error,
            RunState::Cancelled | RunState::Interrupted => self.warning,
            _ => self.accent,
        }
    }
}
fn state_label(row: &TaskRow) -> &'static str {
    if row.run.stop_cause.is_some() && !row.run.state.terminal() {
        return "Stopping";
    }
    match row.run.state {
        RunState::Prepared => "Preparing",
        RunState::Queued => "Queued",
        RunState::Running => "Running",
        RunState::Completed => "Completed",
        RunState::Failed => "Failed",
        RunState::Cancelled => "Cancelled",
        RunState::Interrupted => "Interrupted",
    }
}
fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}
fn short(id: &str) -> String {
    id.strip_prefix("run-")
        .or_else(|| id.strip_prefix("session_child_"))
        .unwrap_or(id)
        .chars()
        .take(8)
        .collect()
}
fn time(timestamp: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map_or_else(|| "Unknown".into(), |date| date.format(format).to_string())
}
fn safe(value: &str) -> String {
    crate::message::strip_ansi_escape_sequences(value)
        .chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
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
fn field(lines: &mut Vec<Line<'static>>, name: &str, value: impl ToString, styles: &Styles) {
    lines.push(Line::from(Span::styled(
        name.to_string(),
        styles.muted.add_modifier(Modifier::BOLD),
    )));
    lines.extend(
        safe(&value.to_string())
            .split('\n')
            .map(|line| Line::from(line.to_string())),
    );
    lines.push(Line::default());
}

/// Decode only a complete verified receipt page. Large inputs stay exact paged
/// JSON instead of inventing a partial argument or fetching an unbounded body.
fn argument_lines(page: &TaskTextPage, raw: bool, styles: &Styles) -> (String, Vec<Line<'static>>) {
    if page.start == 0
        && page.end == page.total
        && !raw
        && let Ok(receipt) = serde_json::from_str::<serde_json::Value>(&page.text)
        && let Some(input) = receipt.get("input")
    {
        let mut lines = vec![];
        if let Some(arguments) = input.as_object() {
            // Put the task-bearing value first without changing any value.
            let priority = ["command", "prompt", "file_path", "query", "intent"];
            let keys = priority
                .into_iter()
                .filter(|key| arguments.contains_key(*key))
                .chain(
                    arguments
                        .keys()
                        .map(String::as_str)
                        .filter(|key| !priority.contains(key)),
                );
            for key in keys {
                let value = &arguments[key];
                let kind = match value {
                    serde_json::Value::String(_) => "text",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object",
                    _ => "value",
                };
                let text = match value {
                    serde_json::Value::String(s) if !s.is_empty() => s.clone(),
                    _ => serde_json::to_string_pretty(value).unwrap_or_default(),
                };
                field(&mut lines, &format!("{key}  ·  {kind}"), text, styles);
            }
            if arguments.is_empty() {
                lines.push(Line::from("No arguments were supplied: {}"));
            }
        } else {
            field(
                &mut lines,
                "input",
                serde_json::to_string_pretty(input).unwrap_or_default(),
                styles,
            );
        }
        return (
            "Arguments sent by the agent · strings shown as text".into(),
            lines,
        );
    }
    let title = if page.start == 0 && page.end == page.total {
        "Original invocation receipt · JSON"
    } else {
        "Large input · original receipt JSON, paged"
    };
    (
        title.into(),
        safe(&page.text)
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect(),
    )
}

impl TaskMonitor {
    fn buttons(&mut self, frame: &mut Frame, area: Rect, buttons: &[(&str, Action)]) {
        let styles = Styles::new();
        let mut x = area.x;
        for (label, action) in buttons {
            let text = format!(" {label} ");
            let width = u16::try_from(text.width()).unwrap_or(u16::MAX);
            if x.saturating_add(width) > area.right() {
                break;
            }
            let rect = Rect::new(x, area.y, width, 1);
            let selected = match action {
                Action::Active => self.view == TaskView::Active,
                Action::Completed => self.view == TaskView::Completed,
                Action::Input => !self.info && self.content == ExecutionContent::Input,
                Action::Output => !self.info && self.content == ExecutionContent::Output,
                Action::Info => self.info,
                _ => false,
            };
            let style = if selected {
                styles
                    .accent
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                styles.muted
            };
            frame.render_widget(Paragraph::new(text).style(style), rect);
            self.hit.push((rect, action.clone()));
            x = x.saturating_add(width + 1);
        }
    }
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.dimensions = (area.width, area.height);
        self.hit.clear();
        self.list_area = Rect::default();
        self.preview_area = Rect::default();
        self.menu_area = Rect::default();
        let styles = Styles::new();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Tasks needs {MIN_WIDTH}×{MIN_HEIGHT}\nResize or Esc to close"
                )),
                area,
            );
            return;
        }
        let area = area.inner(Margin::new(1, 0));
        let [header, nav, body, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(area);
        let scope = if let Some(parent) = self.parents.last() {
            parent.row.child_id.as_ref().map_or_else(
                || format!("Nested {}", parent.row.run.tool),
                |child| format!("Child history {}", short(child)),
            )
        } else if self.all {
            "All Jcode sessions".into()
        } else {
            "This session + children".into()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Tasks", styles.accent.add_modifier(Modifier::BOLD)),
                Span::styled(format!("  /  {scope}"), styles.muted),
            ])),
            header,
        );
        frame.render_widget(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(styles.rule),
            nav,
        );
        if self.storage {
            frame.render_widget(
                Paragraph::new("Storage  /  reviewed cleanup")
                    .style(styles.accent.add_modifier(Modifier::BOLD)),
                nav,
            );
        } else if self.help {
            frame.render_widget(
                Paragraph::new("Actions  ·  choose with ↑/↓ and Enter, or click")
                    .style(styles.accent),
                nav,
            );
        } else {
            self.buttons(
                frame,
                nav,
                &[
                    ("1 Active", Action::Active),
                    ("2 Completed", Action::Completed),
                    ("a Scope", Action::Scope),
                    ("g Storage", Action::Storage),
                ],
            );
        }
        if self.help {
            self.render_actions(frame, body, &styles);
            let choose = self
                .actions()
                .get(self.menu_selection)
                .map(|(_, action)| action.clone())
                .unwrap_or(Action::Back);
            self.buttons(
                frame,
                footer,
                &[("Enter Choose", choose), ("Esc Back", Action::Back)],
            );
        } else if self.storage {
            self.render_storage(frame, body, &styles);
            self.buttons(
                frame,
                footer,
                if self.review.is_some() {
                    &[("y Confirm", Action::Confirm), ("Esc Back", Action::Back)]
                } else {
                    &[("Enter Review", Action::Review), ("Esc Back", Action::Back)]
                },
            );
        } else {
            if self.detail {
                self.render_detail(frame, body);
            } else if area.width >= 118 {
                let [list, divider, preview] = Layout::horizontal([
                    Constraint::Percentage(42),
                    Constraint::Length(2),
                    Constraint::Min(1),
                ])
                .areas(body);
                frame.render_widget(
                    Block::default()
                        .borders(Borders::LEFT)
                        .border_style(styles.rule),
                    divider,
                );
                self.render_list(frame, list);
                self.render_detail(frame, preview);
            } else {
                self.render_list(frame, body);
            }
            self.render_footer(frame, footer);
        }
        let notice = if self.status.is_empty() {
            let selected = self
                .selected
                .as_ref()
                .and_then(|selected| {
                    self.rows
                        .iter()
                        .position(|row| row.run.id == selected.run.id)
                })
                .map(|index| index + 1);
            format!(
                "{}{}{}",
                selected.map_or_else(String::new, |index| format!("{index} / ")),
                self.rows.len(),
                if self.next.is_some() {
                    " on this page · ] next page"
                } else {
                    " tasks"
                }
            )
        } else {
            self.status.clone()
        };
        frame.render_widget(
            Paragraph::new(fit(&notice, usize::from(status.width))).style(styles.muted),
            status,
        );
    }
    fn render_actions(&mut self, frame: &mut Frame, body: Rect, styles: &Styles) {
        let actions = self.actions();
        self.menu_area = body;
        let mut state = ListState::default()
            .with_selected(Some(self.menu_selection))
            .with_offset(self.menu_offset);
        let items = actions.iter().map(|(label, _)| ListItem::new(*label));
        frame.render_stateful_widget(
            List::new(items).highlight_style(styles.accent.add_modifier(Modifier::REVERSED)),
            body,
            &mut state,
        );
        self.menu_offset = state.offset();
        for (index, (_, action)) in actions
            .iter()
            .enumerate()
            .skip(self.menu_offset)
            .take(usize::from(body.height))
        {
            self.hit.push((
                Rect::new(
                    body.x,
                    body.y + u16::try_from(index - self.menu_offset).unwrap_or(0),
                    body.width,
                    1,
                ),
                action.clone(),
            ));
        }
    }
    fn render_footer(&mut self, frame: &mut Frame, area: Rect) {
        let [actions, navigation] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        if self.detail || self.output_focus {
            if self.info {
                self.buttons(
                    frame,
                    actions,
                    &[("i Input", Action::Input), ("o Output", Action::Output)],
                );
            } else if self.content == ExecutionContent::Input {
                self.buttons(
                    frame,
                    actions,
                    &[
                        (
                            if self.raw_input {
                                "v Arguments"
                            } else {
                                "v Receipt JSON"
                            },
                            Action::RawInput,
                        ),
                        ("[ Previous", Action::Older),
                        ("] Next", Action::Newer),
                    ],
                );
            } else {
                self.buttons(
                    frame,
                    actions,
                    &[
                        (
                            if self.follow { "f Pause" } else { "f Follow" },
                            Action::Follow,
                        ),
                        ("[ Older", Action::Older),
                        ("] Newer", Action::Newer),
                    ],
                );
            }
        } else {
            self.buttons(
                frame,
                actions,
                &[
                    ("Enter Open", Action::Details),
                    ("i Input", Action::Input),
                    ("o Output", Action::Output),
                ],
            );
        }
        let mut controls = vec![("? Actions", Action::Help), ("Esc Back", Action::Back)];
        if self
            .selected
            .as_ref()
            .is_some_and(|row| !row.run.state.terminal())
        {
            controls.push(("s Stop", Action::Stop));
            if navigation.width >= 70 {
                controls.push(("b Background", Action::Background));
            }
        } else if self
            .selected
            .as_ref()
            .is_some_and(|row| row.child_id.is_some())
        {
            controls.push(("c Context", Action::Context));
        }
        self.buttons(frame, navigation, &controls);
        if navigation.width >= 100 {
            frame.render_widget(
                Paragraph::new(if self.detail || self.output_focus {
                    "↑/↓ scroll · Home top"
                } else {
                    "↑/↓ select · → expand"
                })
                .alignment(Alignment::Right)
                .style(Styles::new().muted),
                Rect::new(navigation.x + 60, navigation.y, navigation.width - 60, 1),
            );
        }
    }
    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let styles = Styles::new();
        let [header, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        self.list_area = list;
        let wide = area.width >= 70;
        let fixed = if wide { 32 } else { 23 };
        let tool_width = usize::from(area.width).saturating_sub(fixed).max(8);
        let columns = |tool: String, state: String, started: String, bytes: String| {
            if wide {
                format!("{tool:tool_width$} {state:11} {started:8} {bytes:>9}")
            } else {
                format!("{tool:tool_width$} {state:11} {bytes:>9}")
            }
        };
        frame.render_widget(
            Paragraph::new(columns(
                fit(
                    if self.output_focus {
                        "Tool"
                    } else {
                        "Tool · focused"
                    },
                    tool_width,
                ),
                "State".into(),
                "Started".into(),
                "Output".into(),
            ))
            .style(styles.muted),
            header,
        );
        if self.rows.is_empty() {
            let text = if self.capability == Some(false) {
                "This server does not support the task monitor.\nUpgrade the server, then press r to retry."
            } else if self.capability.is_none() {
                "Connecting to task monitor…"
            } else if self.parents.is_empty() && self.view == TaskView::Active {
                "No active work\n\nCompleted results remain in 2 Completed.\nUse a Scope to inspect all Jcode sessions."
            } else if !self.parents.is_empty() {
                "No tool runs in this view\n\nThis child's conversation can still be inspected\nthrough Context on its parent row. Esc returns."
            } else {
                "No completed work\n\nUse 1 Active for running tasks, or a Scope\nfor other Jcode sessions."
            };
            let saved = self.scroll;
            self.scroll = 0;
            self.paint_lines(
                frame,
                list,
                text.lines()
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
                false,
            );
            self.scroll = saved;
            return;
        }
        let selected = self
            .selected
            .as_ref()
            .and_then(|r| self.rows.iter().position(|row| row.run.id == r.run.id));
        let mut state = ListState::default()
            .with_selected(selected)
            .with_offset(self.list_offset);
        let items = self
            .rows
            .iter()
            .map(|row| {
                let name = format!(
                    "{}{}{}",
                    if row.expandable { "▸ " } else { "  " },
                    row.run.tool,
                    if row.run.background { " [bg]" } else { "" }
                );
                let name = fit(&name, tool_width);
                let padding = " ".repeat(tool_width.saturating_sub(name.width()));
                let mut spans = vec![
                    Span::raw(format!("{name}{padding} ")),
                    Span::styled(format!("{:11} ", state_label(row)), styles.state(row)),
                ];
                if wide {
                    spans.push(Span::styled(
                        format!("{} ", time(row.created, "%H:%M:%S")),
                        styles.muted,
                    ));
                }
                spans.push(Span::styled(
                    format!("{:>9}", size(row.run.output_bytes)),
                    styles.muted,
                ));
                ListItem::new(Line::from(spans))
            })
            .collect::<Vec<_>>();
        let highlight = if self.output_focus {
            styles.muted.add_modifier(Modifier::UNDERLINED)
        } else {
            styles.accent.add_modifier(Modifier::REVERSED)
        };
        frame.render_stateful_widget(
            List::new(items).highlight_style(highlight),
            list,
            &mut state,
        );
        self.list_offset = state.offset();
    }
    fn render_detail(&mut self, frame: &mut Frame, area: Rect) {
        let styles = Styles::new();
        let Some(row) = self.selected.as_ref() else {
            frame.render_widget(Paragraph::new("Select a task\n\nInput shows exactly what the agent supplied.\nOutput shows retained results or the live stream.\nInfo holds identities and execution metadata.").style(styles.muted),area);
            return;
        };
        self.preview_area = area;
        let [summary, tabs, source, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);
        let heading = format!(
            "{}  ·  {}{}",
            row.run.tool,
            state_label(row),
            if row.run.background {
                "  ·  background"
            } else {
                ""
            }
        );
        let row = row.clone();
        frame.render_widget(
            Paragraph::new(fit(&heading, usize::from(summary.width)))
                .style(styles.accent.add_modifier(Modifier::BOLD)),
            summary,
        );
        self.buttons(
            frame,
            tabs,
            &[
                ("i Input", Action::Input),
                ("o Output", Action::Output),
                ("m Info", Action::Info),
            ],
        );
        let (caption, lines) = if self.info {
            (
                "Execution metadata · original run identity".to_string(),
                self.info_lines(&row, &styles),
            )
        } else if let Some(page) = &self.page {
            if self.content == ExecutionContent::Input {
                let (kind, lines) = argument_lines(page, self.raw_input, &styles);
                (
                    if page.end < page.total || page.start > 0 {
                        format!("{kind} · {}..{} / {} B", page.start, page.end, page.total)
                    } else {
                        kind
                    },
                    lines,
                )
            } else {
                (
                    format!(
                        "{} · {}..{} / {} B{}",
                        if self.follow { "Following" } else { "Paused" },
                        page.start,
                        page.end,
                        page.total,
                        if row.run.state.terminal() {
                            " · sealed"
                        } else {
                            " · live"
                        }
                    ),
                    safe(&page.text)
                        .split('\n')
                        .map(|line| Line::from(line.to_string()))
                        .collect(),
                )
            }
        } else {
            let text = self.content_error.as_ref().cloned().unwrap_or_else(|| {
                if self.content == ExecutionContent::Output
                    && row.run.output_path.is_none()
                    && !row.run.state.terminal()
                {
                    "Waiting for output…\n\nInput is already available in the Input tab.".into()
                } else {
                    "Loading retained content…".into()
                }
            });
            (
                if self.content == ExecutionContent::Input {
                    "Arguments sent by the agent"
                } else {
                    "Retained output"
                }
                .into(),
                safe(&text)
                    .lines()
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            )
        };
        frame.render_widget(
            Paragraph::new(fit(&caption, usize::from(source.width))).style(styles.muted),
            source,
        );
        let mut lines = lines;
        if self.page.is_some()
            && !self.info
            && let Some(error) = &self.content_error
        {
            lines.insert(
                0,
                Line::from(Span::styled(
                    format!("Unavailable: {} · last loaded window follows", safe(error)),
                    styles.error,
                )),
            );
        }
        self.paint_lines(
            frame,
            body,
            lines,
            self.follow && !self.info && self.content == ExecutionContent::Output,
        );
    }
    fn info_lines(&self, row: &TaskRow, styles: &Styles) -> Vec<Line<'static>> {
        let mut lines = vec![];
        field(&mut lines, "Run", &row.run.id, styles);
        field(&mut lines, "Session", &row.run.session_id, styles);
        if let Some(child) = &row.child_id {
            field(&mut lines, "Child conversation", child, styles);
        }
        field(
            &mut lines,
            "Started (UTC)",
            time(row.created, "%Y-%m-%d %H:%M:%S"),
            styles,
        );
        field(
            &mut lines,
            "Last change (UTC)",
            time(row.updated, "%Y-%m-%d %H:%M:%S"),
            styles,
        );
        field(
            &mut lines,
            "Capture",
            format!(
                "{} · {}",
                size(row.run.output_bytes),
                if row.run.complete {
                    "sealed capture"
                } else {
                    "partial or not yet published"
                }
            ),
            styles,
        );
        if let Some(cause) = row.run.stop_cause {
            field(&mut lines, "Stop cause", cause.description(), styles);
        }
        if let Some(exit) = &row.run.process_exit {
            field(
                &mut lines,
                "Process outcome",
                format!(
                    "Exit {:?} · signal {:?} · timed out {}",
                    exit.code, exit.signal, exit.timed_out
                ),
                styles,
            );
        }
        if let Some(progress) = &row.run.progress {
            field(
                &mut lines,
                "Progress",
                serde_json::to_string_pretty(&progress.value).unwrap_or_default(),
                styles,
            );
        }
        field(
            &mut lines,
            "Input receipt (server path)",
            row.run.input_path.display(),
            styles,
        );
        if let Some(path) = &row.run.output_path {
            field(&mut lines, "Output (server path)", path.display(), styles);
        }
        if let Some(parent) = &row.run.parent_id {
            field(&mut lines, "Parent invocation", parent, styles);
        }
        lines
    }
    fn paint_lines(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lines: Vec<Line<'static>>,
        follow: bool,
    ) {
        let lines = lines
            .into_iter()
            .flat_map(|line| crate::tui::markdown::wrap_line(line, usize::from(area.width)))
            .collect::<Vec<_>>();
        let max = lines.len().saturating_sub(usize::from(area.height));
        self.scroll = if follow { max } else { self.scroll.min(max) };
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.scroll)
                    .take(usize::from(area.height))
                    .collect::<Vec<_>>(),
            ),
            area,
        );
    }
    fn render_storage(&mut self, frame: &mut Frame, body: Rect, styles: &Styles) {
        let mut lines = vec![];
        if let Some(review) = &self.review {
            lines.push(Line::from(Span::styled(
                "Cleanup review",
                styles.warning.add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::default());
            let requested = review.requested_bytes.map_or_else(
                || "Selected outputs".into(),
                |value| format!("Requested {} ({value} bytes)", size(value)),
            );
            lines.push(Line::from(requested));
            lines.push(Line::from(format!(
                "Will remove {} in {} whole outputs",
                size(review.selected_bytes),
                review.candidates.len()
            )));
            lines.push(Line::from(format!(
                "Whole-output overshoot: {} ({} bytes)",
                size(review.overshoot_bytes),
                review.overshoot_bytes
            )));
            lines.push(Line::default());
            lines.extend(
                safe(&review.impact)
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
            lines.push(Line::default());
            for (index, candidate) in review.candidates.iter().enumerate() {
                field(
                    &mut lines,
                    &format!("Output {} · {}", index + 1, size(candidate.bytes)),
                    &candidate.run_id,
                    styles,
                );
                field(&mut lines, "Session", &candidate.session_id, styles);
                if !candidate.affected_snapshot_ids.is_empty() {
                    field(
                        &mut lines,
                        "Snapshots losing backing output",
                        candidate.affected_snapshot_ids.join("\n"),
                        styles,
                    );
                }
            }
            lines.push(Line::from(Span::styled(
                "y confirms this exact review. Esc returns without deleting.",
                styles.warning,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Storage administration",
                styles.accent.add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("Completed cold-archived output only."));
            lines.push(Line::from(
                "Live work and recent emergency spillover stay protected.",
            ));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "Reclaim target (bytes)",
                styles.muted,
            )));
            lines.push(Line::from(Span::styled(
                format!(" {} ", self.storage_input),
                styles.accent.add_modifier(Modifier::UNDERLINED),
            )));
            if let Ok(bytes) = self.storage_input.parse::<u64>() {
                lines.push(Line::from(Span::styled(
                    format!("{} · Enter reviews, it does not delete", size(bytes)),
                    styles.muted,
                )));
            }
            lines.push(Line::default());
            lines.extend(
                safe(&self.storage_text)
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
        }
        self.paint_lines(frame, body, lines, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argument_projection_keeps_every_typed_value_and_original_receipt_unchanged() {
        let text=serde_json::json!({"input":{"command":"echo first\necho second","empty":"","missing":null,"enabled":false,"count":0,"paths":["a","b"],"nested":{"x":1}},"session_id":"identity"}).to_string();
        let page = TaskTextPage {
            start: 0,
            end: text.len() as u64,
            total: text.len() as u64,
            text: text.clone(),
        };
        let (_, lines) = argument_lines(&page, false, &Styles::new());
        let rendered = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for value in [
            "echo first\necho second",
            "empty",
            "\"\"",
            "null",
            "false",
            "count",
            "0",
            "paths",
            "\"a\"",
            "nested",
            "\"x\": 1",
        ] {
            assert!(rendered.contains(value), "Missing {value}: {rendered}");
        }
        let (_, raw) = argument_lines(&page, true, &Styles::new());
        assert_eq!(
            raw.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            text
        );
        assert_eq!(page.text, text);
    }
    #[test]
    fn cell_clipping_preserves_unicode_boundaries_and_never_overflows_columns() {
        for width in 0..25 {
            for text in [
                "漢字 a very long path",
                "e\u{301} e\u{301} e\u{301} e\u{301}",
                "🙂🙂🙂🙂🙂",
                "plain",
            ] {
                assert!(fit(text, width).width() <= width);
            }
        }
    }
    #[test]
    fn monochrome_retains_visible_tabs_selection_and_status_without_color() {
        let _lock = crate::storage::lock_test_env();
        let original = std::env::var_os("NO_COLOR");
        crate::env::set_var("NO_COLOR", "1");
        let result = std::panic::catch_unwind(|| {
            let mut monitor = TaskMonitor::new("parent".into(), false);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 24)).unwrap();
            terminal
                .draw(|frame| monitor.render(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert!(
                buffer
                    .content
                    .iter()
                    .all(|cell| matches!(cell.fg, Color::Reset) && matches!(cell.bg, Color::Reset))
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.modifier.contains(Modifier::UNDERLINED))
            );
        });
        match original {
            Some(value) => crate::env::set_var("NO_COLOR", value),
            None => crate::env::remove_var("NO_COLOR"),
        };
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
    }
}
