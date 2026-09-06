//! One read-only state machine for wide and narrow local/remote inspection.
mod render;
#[cfg(test)]
mod tests;
use crate::protocol::*;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pane {
    Repositories,
    #[default]
    Resources,
    Detail,
}

pub(crate) struct Pending {
    pub id: u64,
    pub session: String,
    pub request: InstructionInspectionRequest,
}

pub(crate) struct InstructionManager {
    pub visible: bool,
    pub session: String,
    pub snapshot: Option<InstructionInspectionSnapshot>,
    pub filter: InstructionFilter,
    pub rows: Vec<InstructionRow>,
    pub row_offset: usize,
    pub row_total: usize,
    pub row_next: Option<usize>,
    pub selected: usize,
    pub repository_selected: usize,
    pub pane: Pane,
    pub view: InstructionInspectionView,
    pub target: Option<InstructionInspectionTarget>,
    pub text: Option<InstructionTextPage>,
    pub text_offsets: Vec<usize>,
    pub scroll: usize,
    pub history: Vec<InstructionCommitRow>,
    pub history_offset: usize,
    pub history_next: Option<usize>,
    pub history_selected: usize,
    pub history_base: Option<String>,
    pub history_visible: bool,
    pub help: bool,
    pub search_editing: bool,
    pub status: String,
    pub queued: Option<InstructionInspectionRequest>,
    pub pending: Option<Pending>,
    pub areas: [Rect; 3],
    pub tabs: Vec<(Rect, InstructionInspectionView)>,
    pub controls: Vec<(Rect, KeyCode)>,
    pub render_only: bool,
    pub wrapped: Vec<String>,
    pub wrap_key: Option<(String, usize, u16)>,
    pub expanded: bool,
}

impl InstructionManager {
    pub fn new(session: String, roster: bool) -> Self {
        let filter = InstructionFilter {
            kind: roster.then(|| "model-roster".into()),
            ..Default::default()
        };
        Self {
            visible: true,
            session,
            snapshot: None,
            filter: filter.clone(),
            rows: Vec::new(),
            row_offset: 0,
            row_total: 0,
            row_next: None,
            selected: 0,
            repository_selected: 0,
            pane: Pane::Resources,
            view: InstructionInspectionView::Source,
            target: None,
            text: None,
            text_offsets: Vec::new(),
            scroll: 0,
            history: Vec::new(),
            history_offset: 0,
            history_next: None,
            history_selected: 0,
            history_base: None,
            history_visible: false,
            help: false,
            search_editing: false,
            status: "Loading read-only inspection…".into(),
            queued: Some(InstructionInspectionRequest::Open { filter }),
            pending: None,
            areas: [Rect::default(); 3],
            tabs: Vec::new(),
            controls: Vec::new(),
            render_only: false,
            wrapped: Vec::new(),
            wrap_key: None,
            expanded: false,
        }
    }

    pub fn refresh(&mut self, session: &str) {
        self.session = session.into();
        self.pending = None;
        self.snapshot = None;
        self.text = None;
        self.history.clear();
        self.wrap_key = None;
        self.queued = Some(InstructionInspectionRequest::Open {
            filter: self.filter.clone(),
        });
        self.status =
            "Refreshing authoritative sources. Session instructions are unchanged.".into();
    }

    pub fn reserve(&mut self, id: u64) -> Option<InstructionInspectionRequest> {
        if self.render_only {
            self.queued = None;
            self.status = "Render-only fixture. No protocol request sent.".into();
            return None;
        }
        let request = self.queued.take()?;
        self.pending = Some(Pending {
            id,
            session: self.session.clone(),
            request: request.clone(),
        });
        Some(request)
    }

    pub fn accept(&mut self, id: u64, reply: InstructionInspectionReply) -> bool {
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        if pending.id != id
            || pending.session != reply.session_id
            || self.session != reply.session_id
        {
            return false;
        }
        if !matches!(
            pending.request,
            InstructionInspectionRequest::Open { .. }
                | InstructionInspectionRequest::Close
                | InstructionInspectionRequest::Cancel
        ) && !matches!(reply.result, InstructionInspectionResult::Failed(_))
            && self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot.as_str())
                != reply.snapshot.as_deref()
        {
            return false;
        }
        let result_matches = match (&pending.request, &reply.result) {
            (_, InstructionInspectionResult::Failed(_)) => true,
            (
                InstructionInspectionRequest::Open { .. },
                InstructionInspectionResult::Opened(value),
            ) => {
                value.session_id == reply.session_id
                    && reply.snapshot.as_deref() == Some(value.snapshot.as_str())
            }
            (
                InstructionInspectionRequest::Resources { offset, filter, .. },
                InstructionInspectionResult::Resources(page),
            ) => *offset == page.offset && filter == &self.filter,
            (
                InstructionInspectionRequest::Detail { .. },
                InstructionInspectionResult::Text(page),
            ) => page.offset == 0,
            (
                InstructionInspectionRequest::Text {
                    offset, document, ..
                },
                InstructionInspectionResult::Text(page),
            ) => *offset == page.offset && document == &page.document,
            (
                InstructionInspectionRequest::History { offset, .. },
                InstructionInspectionResult::History(page),
            ) => *offset == page.offset,
            (InstructionInspectionRequest::Cancel, InstructionInspectionResult::Canceled)
            | (InstructionInspectionRequest::Close, InstructionInspectionResult::Closed) => true,
            _ => false,
        };
        if !result_matches {
            return false;
        }
        let pending = self.pending.take().expect("matched pending");
        match reply.result {
            InstructionInspectionResult::Opened(snapshot) => {
                let selected_key = self.rows.get(self.selected).map(|row| row.key.clone());
                self.set_rows(snapshot.resources.clone(), selected_key.as_deref());
                self.snapshot = Some(snapshot);
                self.status = "Read-only. Sources captured for browsing. R refreshes; previews never activate.".into();
            }
            InstructionInspectionResult::Resources(page) => {
                if let InstructionInspectionRequest::Resources { offset, filter, .. } =
                    &pending.request
                    && (*offset != page.offset || filter != &self.filter)
                {
                    return false;
                }
                let selected_key = self.rows.get(self.selected).map(|row| row.key.clone());
                self.set_rows(page, selected_key.as_deref());
                self.status = "Resource page loaded".into();
            }
            InstructionInspectionResult::Text(page) => {
                if let InstructionInspectionRequest::Text {
                    document, offset, ..
                } = &pending.request
                {
                    if document != &page.document || *offset != page.offset {
                        return false;
                    }
                } else {
                    self.text_offsets.clear();
                }
                if !self.text_offsets.contains(&page.offset) {
                    self.text_offsets.push(page.offset);
                }
                self.text = Some(page);
                self.scroll = 0;
                self.wrap_key = None;
                self.history_visible = false;
                self.status =
                    "Exact captured detail. Scroll or N/P to traverse all content pages.".into();
            }
            InstructionInspectionResult::History(page) => {
                if let InstructionInspectionRequest::History { offset, .. } = pending.request
                    && offset != page.offset
                {
                    return false;
                }
                self.history = page.commits;
                self.history_offset = page.offset;
                self.history_next = page.next;
                self.history_selected = 0;
                self.history_visible = true;
                self.status = "History pinned at inspected HEAD. Enter: revision. A: base. B: compare base to selected.".into();
            }
            InstructionInspectionResult::Failed(error) => {
                self.status = format!(
                    "{}: {}{}",
                    error.operation,
                    error.detail,
                    if error.refresh_required {
                        " [R refresh]"
                    } else {
                        ""
                    }
                );
                // Errors are complete detail, not a disappearing one-line notice.
                self.text = Some(InstructionTextPage {
                    document: "error".into(),
                    title: "Inspection failed; source unchanged".into(),
                    offset: 0,
                    total_bytes: self.status.len(),
                    next: None,
                    text: self.status.clone(),
                });
                self.wrap_key = None;
                self.scroll = 0;
                self.history_visible = false;
            }
            InstructionInspectionResult::Canceled => {
                self.status = "Loading canceled. No source or session changed.".into();
            }
            InstructionInspectionResult::Closed => {
                self.visible = false;
            }
        }
        true
    }

    fn set_rows(&mut self, page: InstructionRowsPage, selected: Option<&str>) {
        self.rows = page.rows;
        self.row_offset = page.offset;
        self.row_total = page.total;
        self.row_next = page.next;
        self.selected = selected
            .and_then(|key| self.rows.iter().position(|row| row.key == key))
            .unwrap_or(0);
    }

    fn snapshot_id(&self) -> Option<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot.clone())
    }

    fn filter_changed(&mut self) {
        self.pane = Pane::Resources;
        self.pending = None;
        if let Some(snapshot) = self.snapshot_id() {
            self.queued = Some(InstructionInspectionRequest::Resources {
                snapshot,
                filter: self.filter.clone(),
                offset: 0,
            });
        } else {
            self.queued = Some(InstructionInspectionRequest::Open {
                filter: self.filter.clone(),
            });
        }
        self.status = "Filtering…".into();
    }

    fn selected_target(&self) -> Option<InstructionInspectionTarget> {
        if self.pane == Pane::Repositories {
            if self.repository_selected == 0 {
                Some(InstructionInspectionTarget::Session)
            } else {
                self.snapshot
                    .as_ref()?
                    .repositories
                    .get(self.repository_selected - 1)
                    .map(|store| InstructionInspectionTarget::Repository(store.key.clone()))
            }
        } else if self.pane == Pane::Resources {
            self.rows
                .get(self.selected)
                .map(|row| InstructionInspectionTarget::Resource(row.key.clone()))
        } else {
            self.target.clone()
        }
    }

    fn detail(
        &mut self,
        view: InstructionInspectionView,
        revision: Option<InstructionRevisionSelection>,
    ) {
        let Some(snapshot) = self.snapshot_id() else {
            return;
        };
        let Some(target) = self.selected_target() else {
            return;
        };
        self.target = Some(target.clone());
        self.view = view;
        self.pane = Pane::Detail;
        self.scroll = 0;
        self.pending = None;
        self.text = None;
        self.wrapped.clear();
        self.wrap_key = None;
        self.history_visible = view == InstructionInspectionView::History && revision.is_none();
        self.queued = Some(if self.history_visible {
            InstructionInspectionRequest::History {
                snapshot,
                target,
                offset: 0,
            }
        } else {
            InstructionInspectionRequest::Detail {
                snapshot,
                target,
                view,
                revision,
            }
        });
        self.status = "Loading selected detail…".into();
    }

    fn page(&mut self, forward: bool) {
        let Some(snapshot) = self.snapshot_id() else {
            return;
        };
        self.queued = match self.pane {
            Pane::Resources => {
                let offset = if forward {
                    self.row_next
                } else {
                    self.row_offset.checked_sub(ROW_PAGE_SIZE)
                };
                offset.map(|offset| InstructionInspectionRequest::Resources {
                    snapshot,
                    filter: self.filter.clone(),
                    offset,
                })
            }
            Pane::Detail if self.history_visible => {
                let offset = if forward {
                    self.history_next
                } else {
                    self.history_offset.checked_sub(ROW_PAGE_SIZE)
                };
                offset.and_then(|offset| {
                    self.target
                        .clone()
                        .map(|target| InstructionInspectionRequest::History {
                            snapshot,
                            target,
                            offset,
                        })
                })
            }
            Pane::Detail => self.text.as_ref().and_then(|page| {
                let offset = if forward {
                    page.next
                } else {
                    self.text_offsets
                        .iter()
                        .copied()
                        .filter(|offset| *offset < page.offset)
                        .max()
                };
                offset.map(|offset| InstructionInspectionRequest::Text {
                    snapshot,
                    document: page.document.clone(),
                    offset,
                })
            }),
            _ => None,
        };
        if self.queued.is_some() {
            self.pending = None;
            self.status = "Loading next page…".into();
        }
    }

    fn navigate(&mut self, down: bool, amount: usize) {
        let (position, count) = match self.pane {
            Pane::Repositories => (
                &mut self.repository_selected,
                self.snapshot
                    .as_ref()
                    .map_or(1, |snapshot| snapshot.repositories.len() + 1),
            ),
            Pane::Resources => (&mut self.selected, self.rows.len()),
            Pane::Detail if self.history_visible => {
                (&mut self.history_selected, self.history.len())
            }
            Pane::Detail => (&mut self.scroll, self.wrapped.len()),
        };
        let previous = *position;
        *position = if down {
            position.saturating_add(amount).min(count.saturating_sub(1))
        } else {
            position.saturating_sub(amount)
        };
        if *position == previous && self.pane != Pane::Repositories {
            self.page(down);
        }
    }

    pub fn paste(&mut self, text: &str) -> bool {
        if !self.visible {
            return false;
        }
        if self.search_editing {
            self.filter.search.push_str(text);
            self.filter_changed();
        } else {
            self.status = "Read-only view: press / before pasting search text.".into();
        }
        true
    }

    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.visible {
            return false;
        }
        if self.search_editing {
            match code {
                KeyCode::Esc | KeyCode::Enter => self.search_editing = false,
                KeyCode::Char('u' | 'U') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.search.clear();
                    self.filter_changed();
                }
                KeyCode::Backspace => {
                    self.filter.search.pop();
                    self.filter_changed();
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.search.push(ch);
                    self.filter_changed();
                }
                _ => {}
            }
            return true;
        }
        if self.help {
            match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => {
                    self.help = false;
                    self.scroll = 0;
                }
                KeyCode::Up | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::PageDown => self.scroll = self.scroll.saturating_add(1),
                _ => {}
            }
            return true;
        }
        let code = match code {
            KeyCode::Char(ch) => KeyCode::Char(ch.to_ascii_lowercase()),
            other => other,
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.visible = false;
                self.pending = None;
                self.queued = Some(InstructionInspectionRequest::Close);
            }
            KeyCode::Char('?') => {
                self.help = true;
                self.scroll = 0;
            }
            KeyCode::F(1) => self.pane = Pane::Repositories,
            KeyCode::F(2) => self.pane = Pane::Resources,
            KeyCode::F(3) => self.pane = Pane::Detail,
            KeyCode::Char('/') => {
                self.search_editing = true;
                self.pane = Pane::Resources;
            }
            KeyCode::Char('r' | 'R') => self.refresh(&self.session.clone()),
            KeyCode::Char('x') => {
                self.pending = None;
                self.queued = Some(InstructionInspectionRequest::Cancel);
                self.status = "Canceling loading…".into();
            }
            KeyCode::Tab => {
                self.pane = match self.pane {
                    Pane::Repositories => Pane::Resources,
                    Pane::Resources => Pane::Detail,
                    Pane::Detail => Pane::Repositories,
                }
            }
            KeyCode::BackTab => {
                self.pane = match self.pane {
                    Pane::Repositories => Pane::Detail,
                    Pane::Resources => Pane::Repositories,
                    Pane::Detail => Pane::Resources,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.navigate(false, 1),
            KeyCode::Down | KeyCode::Char('j') => self.navigate(true, 1),
            KeyCode::PageUp => self.navigate(false, 12),
            KeyCode::PageDown => self.navigate(true, 12),
            KeyCode::Home => match self.pane {
                Pane::Repositories => self.repository_selected = 0,
                Pane::Resources => self.selected = 0,
                Pane::Detail => {
                    self.scroll = 0;
                    self.history_selected = 0;
                }
            },
            KeyCode::End => self.navigate(true, usize::MAX),
            KeyCode::Char('n') => self.page(true),
            KeyCode::Char('p') => self.page(false),
            KeyCode::Char('f') => {
                let kinds = [
                    "system",
                    "agent",
                    "agent-addendum",
                    "module",
                    "notification",
                    "tool-guidance",
                    "skill",
                    "model-roster",
                    "AGENTS.md",
                    "legacy-prompt",
                    "store-settings",
                    "invalid-resource",
                    "configuration",
                ];
                self.filter.kind = match self.filter.kind.as_deref() {
                    None => Some(kinds[0].into()),
                    Some(kind) => kinds
                        .iter()
                        .position(|candidate| *candidate == kind)
                        .and_then(|index| kinds.get(index + 1))
                        .map(|kind| (*kind).into()),
                };
                self.filter_changed();
            }
            KeyCode::Char('s') => {
                self.filter.scope = match self.filter.scope.as_deref() {
                    None => Some("global".into()),
                    Some("global") => Some("project".into()),
                    _ => None,
                };
                self.filter_changed();
            }
            KeyCode::Char('g') => {
                self.filter.redefinitions = cycle_bool(self.filter.redefinitions);
                self.filter_changed();
            }
            KeyCode::Char('v') => {
                self.filter.valid = cycle_bool(self.filter.valid);
                self.filter_changed();
            }
            KeyCode::Char('e') => {
                self.filter.effective = cycle_bool(self.filter.effective);
                self.filter_changed();
            }
            KeyCode::Char('o') => {
                self.filter.origin = match self.filter.origin {
                    None => Some(InstructionOrigin::Managed),
                    Some(InstructionOrigin::Managed) => Some(InstructionOrigin::Legacy),
                    Some(InstructionOrigin::Legacy) => Some(InstructionOrigin::External),
                    Some(InstructionOrigin::External) => None,
                };
                self.filter_changed();
            }
            KeyCode::Char('c') => {
                self.filter = InstructionFilter::default();
                self.filter_changed();
            }
            KeyCode::Char('z') => self.expanded = !self.expanded,
            KeyCode::Char('1') => self.detail(InstructionInspectionView::Source, None),
            KeyCode::Char('2') => self.detail(InstructionInspectionView::Metadata, None),
            KeyCode::Char('3') => self.detail(InstructionInspectionView::Rendered, None),
            KeyCode::Char('4') => self.detail(InstructionInspectionView::System, None),
            KeyCode::Char('5') => self.detail(InstructionInspectionView::Dependencies, None),
            KeyCode::Char('6') => self.detail(InstructionInspectionView::History, None),
            KeyCode::Char('7') => self.detail(InstructionInspectionView::WorkingDiff, None),
            KeyCode::Char('8') => self.detail(InstructionInspectionView::ScopeComparison, None),
            KeyCode::Char('a') if self.history_visible => {
                self.history_base = self
                    .history
                    .get(self.history_selected)
                    .map(|entry| entry.commit.clone());
                self.status =
                    "Base revision selected. Select another revision, then B to compare.".into();
            }
            KeyCode::Char('b') if self.history_visible => {
                if let (Some(from), Some(to)) = (
                    self.history_base.clone(),
                    self.history
                        .get(self.history_selected)
                        .map(|entry| entry.commit.clone()),
                ) {
                    self.detail(
                        InstructionInspectionView::WorkingDiff,
                        Some(InstructionRevisionSelection { from, to: Some(to) }),
                    );
                }
            }
            KeyCode::Enter if self.pane == Pane::Repositories && self.repository_selected > 0 => {
                if let Some(InstructionInspectionTarget::Repository(key)) = self.selected_target() {
                    self.filter.repository = Some(key);
                    self.pane = Pane::Resources;
                    self.filter_changed();
                }
            }
            KeyCode::Enter if self.history_visible && self.pane == Pane::Detail => {
                if let Some(entry) = self.history.get(self.history_selected) {
                    self.detail(
                        InstructionInspectionView::Source,
                        Some(InstructionRevisionSelection {
                            from: entry.commit.clone(),
                            to: None,
                        }),
                    );
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.detail(InstructionInspectionView::Metadata, None)
            }
            _ => {}
        }
        true
    }

    pub fn mouse(&mut self, event: MouseEvent) {
        if !self.visible || self.help {
            return;
        }
        let point = (event.column, event.row);
        if let MouseEventKind::Down(MouseButton::Left) = event.kind
            && let Some((_, key)) = self
                .controls
                .iter()
                .find(|(area, _)| area.contains(point.into()))
        {
            self.key(*key, KeyModifiers::NONE);
            return;
        }
        if let MouseEventKind::Down(MouseButton::Left) = event.kind
            && let Some((_, view)) = self
                .tabs
                .iter()
                .find(|(area, _)| area.contains(point.into()))
        {
            self.detail(*view, None);
            return;
        }
        for (index, area) in self.areas.iter().enumerate() {
            if !area.contains(point.into()) {
                continue;
            }
            self.pane = [Pane::Repositories, Pane::Resources, Pane::Detail][index];
            match event.kind {
                MouseEventKind::ScrollDown => self.navigate(true, 3),
                MouseEventKind::ScrollUp => self.navigate(false, 3),
                MouseEventKind::Down(MouseButton::Left) => {
                    let row = usize::from(event.row.saturating_sub(area.y + 1));
                    let height = usize::from(area.height.saturating_sub(2)).max(1);
                    match self.pane {
                        Pane::Resources => {
                            self.selected = (self.selected / height * height + row)
                                .min(self.rows.len().saturating_sub(1))
                        }
                        Pane::Repositories => {
                            self.repository_selected =
                                (self.repository_selected / height * height + row).min(
                                    self.snapshot
                                        .as_ref()
                                        .map_or(0, |snapshot| snapshot.repositories.len()),
                                )
                        }
                        Pane::Detail if self.history_visible => {
                            self.history_selected = (self.history_selected / height * height + row)
                                .min(self.history.len().saturating_sub(1))
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            break;
        }
    }

    pub fn debug(&self) -> serde_json::Value {
        serde_json::json!({ "visible": self.visible, "section": format!("{:?}", self.view), "pane": format!("{:?}", self.pane), "resource_id": self.rows.get(self.selected).map(|row| &row.id), "scope": self.filter.scope, "rows_loaded": self.rows.len(), "resource_offset": self.row_offset, "detail_pages_visited": self.text_offsets.len(), "detail_bytes_loaded": self.text.as_ref().map_or(0, |page| page.text.len()), "valid": self.rows.get(self.selected).map(|row| row.valid), "repositories": self.snapshot.as_ref().map(|snapshot| snapshot.repositories.iter().map(|store| serde_json::json!({"id":store.key,"kind":store.kind,"dirty":store.dirty,"detached":store.detached,"conflicts":store.conflicts,"active_lease":store.active_lease})).collect::<Vec<_>>()), "pending_id": self.pending.as_ref().map(|pending| pending.id), "layout": if self.areas.iter().filter(|area| area.width > 0).count() > 1 { "wide" } else { "tabs" } })
    }
}

fn cycle_bool(value: Option<bool>) -> Option<bool> {
    match value {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}
