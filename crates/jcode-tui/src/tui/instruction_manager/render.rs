use super::*;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthChar;

const VIEWS: [(InstructionInspectionView, &str); 8] = [
    (InstructionInspectionView::Source, "Source"),
    (InstructionInspectionView::Metadata, "Metadata"),
    (InstructionInspectionView::Rendered, "Render"),
    (InstructionInspectionView::System, "System"),
    (InstructionInspectionView::Dependencies, "Relations"),
    (InstructionInspectionView::History, "History"),
    (InstructionInspectionView::WorkingDiff, "Diff"),
    (InstructionInspectionView::ScopeComparison, "Scopes"),
];
const HELP: &str = "INSTRUCTION INSPECTION · READ ONLY\n\nTab / Shift-Tab: change pane. F1/F2/F3: repository, resources, detail.\nUp/Down or J/K: navigate. PageUp/PageDown: scroll. Home/End: page start/end.\nEnter: open resource metadata; repository filters its resources. Space: inspect repository/session.\n1 Source · 2 Metadata · 3 Rendered preview · 4 Full system / exact stored system\n5 Dependencies and consumers · 6 Git history · 7 Working diff · 8 Global/project comparison\n/ Search by ID/name/kind/scope. Enter or Escape leaves search.\nF: kind filter · S: scope · V: validity · E: effective/shadowed · O: managed/legacy/external\nC: clear all filters. Z: expand/collapse the focused pane.\nN/P: next/previous resource, history, or exact text page.\nHistory: Enter reads selected revision. A selects the base. B compares base to selected.\nR: fresh authoritative snapshot. X: cancel pending loading. Q/Escape: close.\nMouse: click panes, rows and view tabs; wheel scrolls the pointed pane.\n\nNo editor, save, commit, restore, copy, setup, branch or network Git action exists here.\nPreviews do not activate a profile, change the model, or make an inference request.\nPlain and typed templates use existing owners. Missing occurrence values produce a clear failure, never fabricated input.\nDetail is an exact captured document. The visible byte range is one transport page, not a clipped complete file. N/P and scrolling traverse every byte.\nRefresh/reconnect discards captured detail and retains safe navigation. Existing session instructions stay frozen.\nExternal skill Copy and all mutations belong to WP-10.\n\n? or Escape closes this help. Arrow keys scroll.";

impl InstructionManager {
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        self.areas = [Rect::default(); 3];
        self.tabs.clear();
        self.controls.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let header_height = if area.height >= 16 { 2 } else { 1 };
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        let accent = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        if header_height == 2 {
            let active = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.active_agent.as_deref())
                .unwrap_or("none");
            frame.render_widget(
                Paragraph::new(safe(&format!(
                    " INSTRUCTIONS / READ ONLY     Active: {active}"
                )))
                .style(accent),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        let pane_row = Rect::new(
            regions[0].x,
            regions[0].bottom().saturating_sub(1),
            regions[0].width,
            1,
        );
        self.buttons(
            frame,
            pane_row,
            &[
                (KeyCode::F(1), "Repos"),
                (KeyCode::F(2), "List"),
                (KeyCode::F(3), "Detail"),
            ],
        );
        if self.search_editing {
            frame.render_widget(
                Paragraph::new(safe(&format!("/ > {}", self.filter.search)))
                    .style(Style::default().fg(Color::Yellow)),
                regions[1],
            );
        } else {
            self.buttons(
                frame,
                regions[1],
                &[
                    (KeyCode::Char('/'), "/"),
                    (KeyCode::Char('f'), "F"),
                    (KeyCode::Char('s'), "S"),
                    (KeyCode::Char('v'), "V"),
                    (KeyCode::Char('e'), "E"),
                    (KeyCode::Char('o'), "O"),
                    (KeyCode::Char('c'), "C"),
                ],
            );
            if regions[1].width > 28 {
                let filters = format!(
                    "{} · {} · {}",
                    self.filter.search,
                    self.filter.kind.as_deref().unwrap_or("all kinds"),
                    self.filter.scope.as_deref().unwrap_or("all scopes")
                );
                frame.render_widget(
                    Paragraph::new(safe(&filters)).style(Style::default().fg(Color::Gray)),
                    Rect::new(regions[1].x + 22, regions[1].y, regions[1].width - 22, 1),
                );
            }
        }
        let compact = area.width < 90;
        let mut x = regions[2].x;
        for (index, (view, name)) in VIEWS.iter().enumerate() {
            let label = if compact {
                format!("{} ", index + 1)
            } else {
                format!("{} {}  ", index + 1, name)
            };
            let width = (label.len() as u16).min(regions[2].right().saturating_sub(x));
            let tab = Rect::new(x, regions[2].y, width, regions[2].height);
            frame.render_widget(
                Paragraph::new(label).style(if *view == self.view {
                    accent
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
                tab,
            );
            self.tabs.push((tab, *view));
            x = x.saturating_add(width);
        }
        if self.help {
            let help = format!("{HELP}\n\nCurrent filters:\n{:#?}", self.filter);
            let lines = wrap(&help, regions[3].width.saturating_sub(2).max(1));
            self.scroll = self.scroll.min(lines.len().saturating_sub(1));
            let text = lines
                .iter()
                .skip(self.scroll)
                .take(regions[3].height as usize)
                .map(|line| Line::raw(line.clone()))
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(text), regions[3]);
        } else {
            let wide = area.width >= 120 && !self.expanded;
            if wide {
                let columns = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(26),
                        Constraint::Length(35),
                        Constraint::Min(1),
                    ])
                    .split(regions[3]);
                self.areas = [columns[0], columns[1], columns[2]];
            } else {
                self.areas[match self.pane {
                    Pane::Repositories => 0,
                    Pane::Resources => 1,
                    Pane::Detail => 2,
                }] = regions[3];
            }
            self.render_repositories(frame, self.areas[0]);
            self.render_resources(frame, self.areas[1]);
            self.render_detail(frame, self.areas[2]);
        }
        frame.render_widget(
            Paragraph::new(safe(&self.status)).style(Style::default().fg(
                if self.pending.is_some() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                },
            )),
            regions[4],
        );
        self.buttons(
            frame,
            regions[5],
            &[
                (KeyCode::Enter, "↵"),
                (KeyCode::Char('n'), "N"),
                (KeyCode::Char('p'), "P"),
                (KeyCode::Char('a'), "A"),
                (KeyCode::Char('b'), "B"),
                (KeyCode::Char('z'), "Z"),
                (KeyCode::Char('r'), "R"),
                (KeyCode::Char('x'), "X"),
                (KeyCode::Char('?'), "?"),
                (KeyCode::Char('q'), "Q"),
            ],
        );
    }

    fn buttons(&mut self, frame: &mut Frame, area: Rect, buttons: &[(KeyCode, &str)]) {
        let mut x = area.x;
        for (key, label) in buttons {
            let width = (label.chars().count() as u16 + 1).min(area.right().saturating_sub(x));
            if width == 0 {
                break;
            }
            let rect = Rect::new(x, area.y, width, area.height.min(1));
            frame.render_widget(
                Paragraph::new(format!("{label} ")).style(Style::default().fg(Color::Cyan)),
                rect,
            );
            self.controls.push((rect, *key));
            x = x.saturating_add(width);
        }
    }

    fn render_repositories(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }
        let mut labels = vec!["Session / defaults".to_string()];
        if let Some(snapshot) = &self.snapshot {
            labels.extend(snapshot.repositories.iter().map(|store| {
                format!(
                    "{} {} {}{}",
                    store.kind,
                    store.branch.as_deref().unwrap_or(if store.detached {
                        "DETACHED"
                    } else {
                        "no branch"
                    }),
                    if store.dirty { "*" } else { "" },
                    if store.active_lease { " LOCKED" } else { "" }
                )
            }));
        }
        draw_list(
            frame,
            area,
            "F1 Repositories",
            &labels,
            self.repository_selected,
            self.pane == Pane::Repositories,
        );
    }

    fn render_resources(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }
        let labels = self
            .rows
            .iter()
            .map(|row| {
                format!(
                    "{}{} {}:{} / {}",
                    if row.valid { " " } else { "!" },
                    if row.effective { "●" } else { "○" },
                    row.scope,
                    row.id,
                    row.kind
                )
            })
            .collect::<Vec<_>>();
        let title = format!(
            "F2 Resources {}-{}/{}",
            if self.rows.is_empty() {
                0
            } else {
                self.row_offset + 1
            },
            self.row_offset + self.rows.len(),
            self.row_total
        );
        draw_list(
            frame,
            area,
            &title,
            &labels,
            self.selected,
            self.pane == Pane::Resources,
        );
    }

    fn render_detail(&mut self, frame: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }
        if self.history_visible {
            let labels = self
                .history
                .iter()
                .map(|entry| {
                    format!(
                        "{} {} {}",
                        if self.history_base.as_deref() == Some(&entry.commit) {
                            "A"
                        } else {
                            " "
                        },
                        entry.commit.get(..10).unwrap_or(&entry.commit),
                        entry.subject
                    )
                })
                .collect::<Vec<_>>();
            draw_list(
                frame,
                area,
                "F3 History · Enter read · A/B compare",
                &labels,
                self.history_selected,
                self.pane == Pane::Detail,
            );
            return;
        }
        let block = pane_block("F3 Detail", self.pane == Pane::Detail, area);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(page) = &self.text else {
            frame.render_widget(Paragraph::new("Select a resource and press 1-8.\nSpace inspects a repository.\nSession + 4 reads exact stored system text."), inner);
            return;
        };
        let key = (page.document.clone(), page.offset, inner.width);
        if self.wrap_key.as_ref() != Some(&key) {
            let heading = format!(
                "{}\nBytes {}-{} / {} · {}\n\n",
                page.title,
                page.offset,
                page.offset + page.text.len(),
                page.total_bytes,
                if page.offset == 0 && page.next.is_none() {
                    "complete document"
                } else {
                    "page; N/P for all content"
                }
            );
            self.wrapped = wrap(&(heading + &page.text), inner.width.max(1));
            self.wrap_key = Some(key);
        }
        self.scroll = self.scroll.min(self.wrapped.len().saturating_sub(1));
        let lines = self
            .wrapped
            .iter()
            .skip(self.scroll)
            .take(inner.height as usize)
            .map(|line| Line::raw(line.clone()))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn pane_block(title: &str, active: bool, area: Rect) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(if area.height > 2 && area.width > 2 {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_style(Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }))
}

fn draw_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    labels: &[String],
    selected: usize,
    active: bool,
) {
    let block = pane_block(title, active, area);
    let inner = block.inner(area);
    let height = usize::from(inner.height).max(1);
    let start = selected / height * height;
    let lines = labels
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, label)| {
            Line::from(Span::styled(
                format!(
                    "{} {}",
                    if index == selected { "›" } else { " " },
                    safe(label)
                ),
                if index == selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' && ch != '\t' {
                '�'
            } else {
                ch
            }
        })
        .collect()
}

/// Wrap only the current bounded transport page. No u16 scroll offset can make
/// a later line unreachable and unchanged frames do not rewrap large documents.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let limit = usize::from(width).max(1);
    let mut lines = Vec::new();
    for source in safe(text).split('\n') {
        let mut line = String::new();
        let mut columns = 0;
        for ch in source.chars() {
            let chars = if ch == '\t' {
                "    ".to_string()
            } else {
                ch.to_string()
            };
            for ch in chars.chars() {
                let size = ch.width().unwrap_or(0);
                if columns + size > limit && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    columns = 0;
                }
                line.push(ch);
                columns += size;
            }
        }
        lines.push(line);
    }
    lines
}
