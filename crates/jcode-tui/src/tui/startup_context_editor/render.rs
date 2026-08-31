use super::*;

impl StartupContextEditor {
    pub(crate) fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        action_required: Option<&crate::protocol::StartupContextActionRequired>,
    ) {
        frame.render_widget(Clear, area);
        self.hit_regions.clear();
        let accent = Color::Rgb(120, 190, 255);
        let text = Style::default().fg(Color::Rgb(220, 220, 230));
        let dim = Style::default().fg(Color::Rgb(130, 135, 150));
        let warn = Style::default().fg(Color::Rgb(235, 190, 105));
        let error = Style::default().fg(Color::Rgb(240, 110, 110));
        let good = Style::default().fg(Color::Rgb(120, 220, 150));
        let styles = EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            good,
        };

        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(" Startup Context editor ");
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let vertical = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(inner);
        self.render_header(frame, vertical[0], status, styles);

        match &self.phase {
            EditorPhase::Opening => {
                self.render_opening(frame, vertical[1], status, action_required, styles)
            }
            EditorPhase::Busy { owner } => {
                let detail = owner
                    .as_ref()
                    .map(|owner| {
                        format!(
                            "Editor busy on {} · session {}",
                            owner.server_name, owner.session_id
                        )
                    })
                    .unwrap_or_else(|| "Another live editor owns this project".to_string());
                centered_message(frame, vertical[1], &detail, warn);
            }
            EditorPhase::Error(failure) => centered_message(
                frame,
                vertical[1],
                &format!("Editor unavailable: {}", failure.message),
                error,
            ),
            EditorPhase::Unsupported => centered_message(
                frame,
                vertical[1],
                "The connected server does not support the Startup Context editor.",
                warn,
            ),
            EditorPhase::Closing => {
                centered_message(frame, vertical[1], "Releasing editor lease…", dim)
            }
            EditorPhase::Ready => self.render_workspace(frame, vertical[1], styles),
        }

        self.render_footer(frame, vertical[2], status, dim, accent, warn);
        if let Some(mode) = &self.input_mode {
            self.render_input_modal(frame, area, mode, text, dim, accent);
        }
    }

    fn render_opening(
        &self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        action_required: Option<&crate::protocol::StartupContextActionRequired>,
        styles: EditorStyles,
    ) {
        let EditorStyles {
            text, dim, error, ..
        } = styles;
        let mut lines = vec![Line::from(Span::styled(
            "Acquiring the project editor lease…",
            dim,
        ))];
        if action_required.is_some() {
            lines.push(Line::from(Span::styled("Request not sent", error)));
        }
        if let Some(status) = status {
            for issue in status
                .issues
                .iter()
                .take(area.height.saturating_sub(2) as usize)
            {
                lines.push(Line::from(vec![
                    Span::styled("• ", error),
                    Span::styled(
                        issue
                            .logical_path
                            .as_deref()
                            .unwrap_or("<project>")
                            .to_string(),
                        text,
                    ),
                    Span::styled(format!(" · {}", issue_label(&issue.kind)), error),
                ]));
            }
        }
        if let Some(action) = action_required {
            lines.push(Line::from(Span::styled(action.detail.clone(), error)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn render_header(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        styles: EditorStyles,
    ) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            ..
        } = styles;
        let dirty = if self.is_dirty() {
            " · Unsaved draft"
        } else {
            ""
        };
        let counts = Line::from(vec![
            Span::styled(" Saved default ", dim),
            Span::styled(self.saved_default.len().to_string(), text),
            Span::styled(" · Session receipt ", dim),
            Span::styled(self.receipt.len().to_string(), text),
            Span::styled(" · Draft ", dim),
            Span::styled(self.draft.len().to_string(), text),
            Span::styled(dirty.to_string(), if dirty.is_empty() { dim } else { warn }),
        ]);
        let root = self
            .editor
            .as_ref()
            .map(|editor| editor.project.active_root.as_str())
            .or_else(|| {
                status.and_then(|status| {
                    status
                        .compact
                        .project
                        .as_ref()
                        .map(|project| project.active_root.as_str())
                })
            })
            .unwrap_or("authoritative project loading");
        let paragraph = Paragraph::new(vec![
            counts,
            Line::from(vec![
                Span::styled(" Project ", dim),
                Span::styled(root.to_string(), Style::default().fg(accent)),
            ]),
        ]);
        frame.render_widget(paragraph, area);
    }

    fn render_workspace(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            ..
        } = styles;
        if area.width >= 96 {
            let panes = Layout::horizontal([
                Constraint::Percentage(32),
                Constraint::Percentage(33),
                Constraint::Percentage(35),
            ])
            .split(area);
            self.render_browser(frame, panes[0], text, dim, accent, warn);
            self.render_selection(frame, panes[1], styles);
            self.render_preview(frame, panes[2], styles);
        } else {
            let tabs = Rect::new(area.x, area.y, area.width, 1.min(area.height));
            let body = Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height.saturating_sub(1),
            );
            let labels = [
                (StartupContextEditorPane::Browser, " Browser "),
                (StartupContextEditorPane::Selection, " Selection "),
                (StartupContextEditorPane::Preview, " Preview "),
            ];
            let mut x = tabs.x;
            for (pane, label) in labels {
                let width = label
                    .len()
                    .min(tabs.width.saturating_sub(x.saturating_sub(tabs.x)) as usize)
                    as u16;
                if width == 0 {
                    continue;
                }
                let rect = Rect::new(x, tabs.y, width, 1);
                let style = if pane == self.active_pane {
                    Style::default().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    dim
                };
                frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
                self.hit_regions.push(HitRegion {
                    rect,
                    action: RowAction::FocusPane(pane),
                });
                x = x.saturating_add(width);
            }
            match self.active_pane {
                StartupContextEditorPane::Browser => {
                    self.render_browser(frame, body, text, dim, accent, warn)
                }
                StartupContextEditorPane::Selection => self.render_selection(frame, body, styles),
                StartupContextEditorPane::Preview => self.render_preview(frame, body, styles),
            }
        }
    }

    fn render_browser(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        text: Style,
        dim: Style,
        accent: Color,
        warn: Style,
    ) {
        let focused = self.active_pane == StartupContextEditorPane::Browser;
        let title = if let Some(query) = &self.browser.search_query {
            format!(" Browser · search {query:?} ")
        } else if self.browser.directory.is_empty() {
            " Browser · Project ".to_string()
        } else {
            format!(
                " Browser · Project › {} ",
                self.browser.directory.replace('/', " › ")
            )
        };
        let block = pane_block(title, focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let toolbar = Rect::new(inner.x, inner.y, inner.width, 1);
        let search_label = "[/ search]";
        let external_label = "[a external]";
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(search_label, Style::default().fg(accent)),
                Span::styled(" ", dim),
                Span::styled(external_label, Style::default().fg(accent)),
            ])),
            toolbar,
        );
        self.hit_regions.push(HitRegion {
            rect: Rect::new(toolbar.x, toolbar.y, search_label.len() as u16, 1),
            action: RowAction::StartSearch,
        });
        self.hit_regions.push(HitRegion {
            rect: Rect::new(
                toolbar.x.saturating_add(search_label.len() as u16 + 1),
                toolbar.y,
                external_label.len() as u16,
                1,
            ),
            action: RowAction::StartExternal,
        });
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        let entries = self.browser.visible_entries();
        if entries.is_empty() {
            let message = if self.browser.loading {
                "Loading bounded project entries…"
            } else if self.browser.search_query.is_some() {
                "No matching project files"
            } else {
                "This directory has no entries"
            };
            frame.render_widget(Paragraph::new(Span::styled(message, dim)), list_area);
            return;
        }
        let start = window_start(
            self.browser.cursor,
            entries.len(),
            list_area.height as usize,
        );
        for (row, (index, entry)) in entries
            .iter()
            .enumerate()
            .skip(start)
            .take(list_area.height as usize)
            .enumerate()
        {
            let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
            let selected = index == self.browser.cursor;
            let marker = match entry.kind {
                StartupContextDirectoryEntryKind::Directory => "▸",
                StartupContextDirectoryEntryKind::File => "·",
                StartupContextDirectoryEntryKind::Symlink => "↗",
                StartupContextDirectoryEntryKind::Other => "×",
            };
            let action = if entry.navigable { "[open][+]" } else { "[+]" };
            let action_width = action.len().min(rect.width as usize) as u16;
            let name_width = rect.width.saturating_sub(action_width);
            let row_style = if selected {
                Style::default().fg(Color::Black).bg(accent)
            } else if !entry.path_valid_utf8 {
                warn
            } else {
                text
            };
            let name = truncate_middle(&format!("{marker} {}", entry.name), name_width as usize);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{name:<width$}", width = name_width as usize),
                        row_style,
                    ),
                    Span::styled(action, if selected { row_style } else { dim }),
                ])),
                rect,
            );
            self.hit_regions.push(HitRegion {
                rect: Rect::new(rect.x, rect.y, name_width, 1),
                action: RowAction::FocusBrowser(index),
            });
            if entry.navigable {
                let open_width = "[open]".len() as u16;
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(rect.x + name_width, rect.y, open_width, 1),
                    action: RowAction::OpenDirectory(index),
                });
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(
                        rect.x + name_width + open_width,
                        rect.y,
                        action_width.saturating_sub(open_width),
                        1,
                    ),
                    action: RowAction::SelectBrowser(index),
                });
            } else {
                self.hit_regions.push(HitRegion {
                    rect: Rect::new(rect.x + name_width, rect.y, action_width, 1),
                    action: RowAction::SelectBrowser(index),
                });
            }
        }
        if self.browser.search_truncated {
            let y = list_area.bottom().saturating_sub(1);
            frame.render_widget(
                Paragraph::new(Span::styled("Search results bounded by server", warn)),
                Rect::new(list_area.x, y, list_area.width, 1),
            );
        }
    }

    fn render_selection(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            good,
        } = styles;
        let focused = self.active_pane == StartupContextEditorPane::Selection;
        let (title, count) = match self.selection_view {
            SelectionView::Draft => ("Ordered draft", self.draft.len()),
            SelectionView::Receipt => ("Persisted receipt", self.receipt.len()),
        };
        let block = pane_block(format!(" {title} · {count} "), focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let toggle = match self.selection_view {
            SelectionView::Draft => "[r inspect receipt]",
            SelectionView::Receipt => "[r back to draft]",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(toggle, Style::default().fg(accent))),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        self.hit_regions.push(HitRegion {
            rect: Rect::new(
                inner.x,
                inner.y,
                toggle.len().min(inner.width as usize) as u16,
                1,
            ),
            action: RowAction::ToggleReceipt,
        });
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        match self.selection_view {
            SelectionView::Draft => {
                if self.draft.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "No files selected. Browse or add an external path.",
                            dim,
                        )),
                        list_area,
                    );
                    return;
                }
                let start = window_start(
                    self.draft_cursor,
                    self.draft.len(),
                    list_area.height as usize,
                );
                for (row, (index, entry)) in self
                    .draft
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(list_area.height as usize)
                    .enumerate()
                {
                    let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
                    let selected = index == self.draft_cursor;
                    let controls = "[↑][↓][x]";
                    let controls_width = controls.len().min(rect.width as usize) as u16;
                    let state = if let Some(issue) = &entry.issue {
                        format!(" ! {}", compact_issue_label(&issue.kind))
                    } else if entry.bytes.is_some() {
                        format!(
                            " · {} · ~{}t",
                            format_bytes(entry.bytes.unwrap_or_default()),
                            entry.estimated_tokens.unwrap_or_default()
                        )
                    } else {
                        " · validating".to_string()
                    };
                    let state_width = state
                        .chars()
                        .count()
                        .min(24)
                        .min(rect.width.saturating_sub(controls_width) as usize)
                        as u16;
                    let label_width = rect
                        .width
                        .saturating_sub(controls_width)
                        .saturating_sub(state_width);
                    let class = match entry.classification {
                        Some(StartupContextPathClassification::External) => " ext",
                        Some(StartupContextPathClassification::Project) => "",
                        None => "",
                    };
                    let label = truncate_middle(
                        &format!("{:>3}. {}{class}", index + 1, entry.logical_path),
                        label_width as usize,
                    );
                    let state = truncate_middle(&state, state_width as usize);
                    let row_style = if selected {
                        Style::default().fg(Color::Black).bg(accent)
                    } else if entry.issue.is_some() {
                        error
                    } else if entry.classification
                        == Some(StartupContextPathClassification::External)
                    {
                        warn
                    } else if entry.bytes.is_some() {
                        good
                    } else {
                        text
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(
                                format!("{label:<width$}", width = label_width as usize),
                                row_style,
                            ),
                            Span::styled(
                                format!("{state:<width$}", width = state_width as usize),
                                row_style,
                            ),
                            Span::styled(controls, if selected { row_style } else { dim }),
                        ])),
                        rect,
                    );
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(rect.x, rect.y, label_width.saturating_add(state_width), 1),
                        action: RowAction::FocusDraft(index),
                    });
                    let button = controls_width / 3;
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(rect.x + label_width + state_width, rect.y, button, 1),
                        action: RowAction::MoveDraftUp(index),
                    });
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(
                            rect.x + label_width + state_width + button,
                            rect.y,
                            button,
                            1,
                        ),
                        action: RowAction::MoveDraftDown(index),
                    });
                    self.hit_regions.push(HitRegion {
                        rect: Rect::new(
                            rect.x + label_width + state_width + button.saturating_mul(2),
                            rect.y,
                            controls_width.saturating_sub(button.saturating_mul(2)),
                            1,
                        ),
                        action: RowAction::RemoveDraft(index),
                    });
                }
            }
            SelectionView::Receipt => {
                if self.receipt.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "This session has no captured receipt files.",
                            dim,
                        )),
                        list_area,
                    );
                    return;
                }
                let start = window_start(
                    self.receipt_cursor,
                    self.receipt.len(),
                    list_area.height as usize,
                );
                for (row, (index, receipt)) in self
                    .receipt
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(list_area.height as usize)
                    .enumerate()
                {
                    let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
                    let selected = index == self.receipt_cursor;
                    let observation = match receipt.latest_observation {
                        StartupContextObservedState::Current => "current",
                        StartupContextObservedState::Changed { .. } => "changed",
                        StartupContextObservedState::Missing => "missing",
                        StartupContextObservedState::Unreadable => "unreadable",
                        StartupContextObservedState::Unsupported => "unsupported",
                    };
                    let label = truncate_middle(
                        &format!(
                            "{:>3}. {} · {} · {observation}",
                            receipt.ordinal,
                            receipt.logical_path,
                            format_bytes(receipt.bytes)
                        ),
                        rect.width as usize,
                    );
                    let style = if selected {
                        Style::default().fg(Color::Black).bg(accent)
                    } else if observation == "current" {
                        text
                    } else {
                        warn
                    };
                    frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
                    self.hit_regions.push(HitRegion {
                        rect,
                        action: RowAction::FocusReceipt(index),
                    });
                }
            }
        }
    }

    fn render_preview(&mut self, frame: &mut Frame, area: Rect, styles: EditorStyles) {
        let EditorStyles {
            text,
            dim,
            accent,
            warn,
            error,
            ..
        } = styles;
        let focused = self.active_pane == StartupContextEditorPane::Preview;
        let title = if self.preview.exact_receipt {
            " Captured receipt detail "
        } else {
            " Current file preview "
        };
        let block = pane_block(title.to_string(), focused, accent);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }
        let Some(path) = self.preview.path.clone() else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Focus a project, draft, or receipt file to inspect it.",
                    dim,
                )),
                inner,
            );
            return;
        };
        let classification = match self.preview.classification {
            Some(StartupContextPathClassification::Project) => "project",
            Some(StartupContextPathClassification::External) => "external",
            None => "validating",
        };
        let mut lines = vec![
            Line::from(vec![Span::styled("Path ", dim), Span::styled(path, text)]),
            Line::from(vec![
                Span::styled("Target ", dim),
                Span::styled(
                    self.preview
                        .resolved_path
                        .clone()
                        .unwrap_or_else(|| "loading".to_string()),
                    if classification == "external" {
                        warn
                    } else {
                        text
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("Class ", dim),
                Span::styled(
                    classification.to_string(),
                    if classification == "external" {
                        warn
                    } else {
                        text
                    },
                ),
                Span::styled(" · UTF-8 full · ", dim),
                Span::styled(
                    self.preview
                        .bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "loading".to_string()),
                    text,
                ),
                Span::styled(
                    format!(
                        " · ~{} tokens",
                        self.preview.estimated_tokens.unwrap_or_default()
                    ),
                    dim,
                ),
            ]),
        ];
        if let Some(hash) = &self.preview.sha256 {
            lines.push(Line::from(vec![
                Span::styled("SHA-256 ", dim),
                Span::styled(hash.clone(), text),
            ]));
        }
        lines.push(Line::from(""));
        if let Some(failure) = &self.preview.failure {
            lines.push(Line::from(Span::styled(failure.clone(), error)));
        } else if self.preview.loading && self.preview.content.is_empty() {
            lines.push(Line::from(Span::styled("Loading bounded content…", dim)));
        } else if self.preview.exact_receipt && self.preview.content.is_empty() {
            lines.push(Line::from(Span::styled(
                "Exact captured content is lazy. Press Enter to load the first bounded chunk.",
                dim,
            )));
        } else {
            for line in self.preview.content.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), text)));
            }
        }
        if self.preview.next_start_char.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "[Enter / click: load next exact chunk]",
                Style::default().fg(accent),
            )));
        } else if self.preview.exact_receipt && !self.preview.content.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "Complete captured content inspected · {} characters",
                    self.preview.total_chars
                ),
                dim,
            )));
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(
            paragraph.scroll((self.preview.scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
        if self.preview.next_start_char.is_some() {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                action: RowAction::LoadMorePreview,
            });
        }
    }

    fn render_footer(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        status: Option<&StartupContextStatusSnapshot>,
        dim: Style,
        accent: Color,
        warn: Style,
    ) {
        let consequence = self.consequence_text(status);
        let buttons = "[ Use in this session ] [ Use in this session + save as project default ]";
        let foundation = "WP-07 foundation · Apply disabled until WP-08 · [ Close editor ]";
        let mut lines = vec![
            Line::from(Span::styled(
                foundation,
                Style::default().fg(Color::Rgb(105, 110, 125)),
            )),
            Line::from(Span::styled(
                buttons,
                Style::default().fg(Color::Rgb(105, 110, 125)),
            )),
            Line::from(Span::styled(consequence, dim)),
        ];
        if let Some(notice) = &self.notice {
            lines[2] = Line::from(Span::styled(notice.clone(), warn));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        if let Some(start) = foundation.find("[ Close editor ]") {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(
                    area.x.saturating_add(start as u16),
                    area.y,
                    "[ Close editor ]".len() as u16,
                    1,
                ),
                action: RowAction::CloseEditor,
            });
        }
        if area.height >= 2 {
            self.hit_regions.push(HitRegion {
                rect: Rect::new(area.x, area.y + 1, area.width, 1),
                action: RowAction::DisabledApply,
            });
        }
        let _ = accent;
    }

    fn render_input_modal(
        &self,
        frame: &mut Frame,
        area: Rect,
        mode: &InputMode,
        text: Style,
        dim: Style,
        accent: Color,
    ) {
        let modal = centered_rect(
            72.min(area.width.saturating_sub(2)),
            7.min(area.height),
            area,
        );
        frame.render_widget(Clear, modal);
        let (title, value, help) = match mode {
            InputMode::Search { value } => (
                " Search project files ",
                value,
                "Enter search · Esc cancel · server search is bounded and cancellable",
            ),
            InputMode::ExternalPath { value } => (
                " Add exact external path ",
                value,
                "Enter add · Esc cancel · external target remains visibly unconfirmed until WP-08",
            ),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(title);
        let inner = block.inner(modal);
        frame.render_widget(block, modal);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(value.clone(), text)),
                Line::from(""),
                Line::from(Span::styled(help, dim)),
            ]),
            inner,
        );
    }

    pub(crate) fn debug_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "open": self.visible,
            "phase": format!("{:?}", self.phase),
            "session_id": self.session_id,
            "pane": format!("{:?}", self.active_pane),
            "directory": self.browser.directory,
            "search": self.browser.search_query,
            "browser_cursor": self.browser.cursor,
            "browser_entries": self.browser.visible_entries().len(),
            "saved_default": self.saved_default.len(),
            "receipt": self.receipt.len(),
            "draft": self.draft.len(),
            "draft_paths": self.draft.iter().map(|entry| entry.logical_path.as_str()).collect::<Vec<_>>(),
            "dirty": self.is_dirty(),
            "input_mode": self.input_mode.as_ref().map(|mode| match mode {
                InputMode::Search { .. } => "search",
                InputMode::ExternalPath { .. } => "external_path",
            }),
            "preview_chars": self.preview.content.chars().count(),
            "preview_exact_receipt": self.preview.exact_receipt,
            "pending_requests": self.pending.len(),
        })
    }
}
