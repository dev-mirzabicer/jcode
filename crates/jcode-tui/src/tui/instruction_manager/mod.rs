//! One read-only state machine for wide and narrow local/remote inspection.
pub(crate) mod editing;
mod menu;
mod navigation;
use navigation::Destination;
mod render;
use menu::{FilterField, Menu};
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ux_tests;
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
    destination: Destination,
    pub editing: editing::EditingUi,
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
    small: bool,
    menu: Option<Menu>,
    pub menu_hits: Vec<(Rect, usize)>,
    pub list_hits: Vec<(Rect, Pane, usize)>,
    pub detail_row: Option<InstructionRow>,
    pub revision_open: bool,
    pub revision_selection: Option<InstructionRevisionSelection>,
    pub return_pane: Pane,
    pub detail_height: usize,
    pub help_scroll: usize,
    pub last_error: bool,
    pub search_cursor: usize,
}

impl InstructionManager {
    pub fn new(session: String, roster: bool) -> Self {
        let filter = InstructionFilter {
            kind: Some(if roster { "model-roster" } else { "agent" }.into()),
            grouped: true,
            main_catalog: true,
            ..Default::default()
        };
        Self {
            destination: Destination::Catalog,
            editing: editing::EditingUi::default(),
            visible: true,
            session,
            snapshot: None,
            filter: filter.clone(),
            rows: Vec::new(),
            row_offset: 0,
            row_total: 0,
            row_next: None,
            selected: 0,
            repository_selected: if roster { 7 } else { 0 },
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
            status: "Loading instruction types and sources…".into(),
            queued: Some(InstructionInspectionRequest::Open { filter }),
            pending: None,
            areas: [Rect::default(); 3],
            tabs: Vec::new(),
            controls: Vec::new(),
            render_only: false,
            wrapped: Vec::new(),
            wrap_key: None,
            expanded: false,
            small: false,
            menu: None,
            menu_hits: Vec::new(),
            list_hits: Vec::new(),
            detail_row: None,
            revision_open: false,
            revision_selection: None,
            return_pane: Pane::Resources,
            detail_height: 1,
            help_scroll: 0,
            last_error: false,
            search_cursor: 0,
        }
    }

    pub fn refresh(&mut self, session: &str) {
        let same_session = self.session == session;
        if !same_session {
            self.rows.clear();
            self.target = None;
            self.detail_row = None;
            self.history_base = None;
            self.filter.repository = None;
        }
        self.menu = None;
        self.pane = Pane::Resources;
        self.revision_open = false;
        self.last_error = false;
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
        self.last_error = false;
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
        let listed = matches!(
            &reply.result,
            InstructionInspectionResult::Opened(_) | InstructionInspectionResult::Resources(_)
        );
        let pending = self.pending.take().expect("matched pending");
        match reply.result {
            InstructionInspectionResult::Opened(snapshot) => {
                let selected_key = self.rows.get(self.selected).map(|row| row.key.clone());
                self.set_rows(snapshot.resources.clone(), selected_key.as_deref());
                self.snapshot = Some(snapshot);
                self.status = "Sources loaded. Choose a type and scope; each Edit action names its destination.".into();
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
                self.status = "Instructions loaded".into();
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
                self.scroll = if matches!(&pending.request, InstructionInspectionRequest::Text {offset,..} if self.text_offsets.iter().any(|previous| previous > offset))
                {
                    usize::MAX
                } else {
                    0
                };
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
                self.last_error = true;
                self.pane = Pane::Detail;
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
        if listed && self.filter.grouped {
            if self.destination == Destination::Session {
                self.detail(InstructionInspectionView::Metadata, None);
            } else {
                self.preview_selection();
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
        self.repository_selected = if self.filter.origin == Some(InstructionOrigin::Legacy) {
            9
        } else {
            navigation::CATEGORIES
                .iter()
                .take(9)
                .position(|(_, kind)| kind.map(str::to_string) == self.filter.kind)
                .unwrap_or(10)
        };
        self.last_error = false;
        self.pane = Pane::Resources;
        self.text = None;
        self.target = None;
        self.detail_row = None;
        self.wrapped.clear();
        self.wrap_key = None;
        self.history_visible = false;
        self.revision_open = false;
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
        if self.destination == Destination::Session {
            Some(InstructionInspectionTarget::Session)
        } else if self.pane == Pane::Repositories {
            None
        } else if self.destination == Destination::Repositories && self.pane == Pane::Resources {
            self.managed_repositories()
                .get(self.selected)
                .map(|store| InstructionInspectionTarget::Repository(store.key.clone()))
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
        if revision.is_none()
            && let Some(reason) = self.view_unavailable(view)
        {
            self.status = reason;
            return;
        }
        let Some(snapshot) = self.snapshot_id() else {
            return;
        };
        let Some(target) = self.selected_target() else {
            return;
        };
        if self.target.as_ref() != Some(&target) {
            self.history.clear();
            self.history_base = None;
            self.revision_open = false;
            self.detail_row = match &target {
                InstructionInspectionTarget::Resource(key) => {
                    self.rows.iter().find(|row| &row.key == key).cloned()
                }
                _ => None,
            };
        }
        if self.pane != Pane::Detail {
            self.return_pane = self.pane;
        }
        self.revision_open = revision.is_some();
        self.revision_selection = revision.clone();
        self.last_error = false;
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
        if self.destination != Destination::Catalog && self.pane != Pane::Detail {
            return;
        }
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
        let max_scroll = self.wrapped.len().saturating_sub(self.detail_height);
        let (position, count) = match self.pane {
            Pane::Repositories => (&mut self.repository_selected, navigation::SESSION + 1),
            Pane::Resources => {
                let count = if self.destination == Destination::Repositories {
                    self.managed_repositories().len()
                } else {
                    self.rows.len()
                };
                (&mut self.selected, count)
            }
            Pane::Detail if self.history_visible => {
                (&mut self.history_selected, self.history.len())
            }
            Pane::Detail => (&mut self.scroll, max_scroll.saturating_add(1)),
        };
        let previous = *position;
        *position = if down {
            position.saturating_add(amount).min(count.saturating_sub(1))
        } else {
            position.saturating_sub(amount)
        };
        let changed = *position != previous;
        if *position == previous && amount != usize::MAX && self.pane != Pane::Repositories {
            self.page(down);
        }
        if changed {
            if self.pane == Pane::Repositories {
                let index = self.repository_selected;
                self.choose_navigation(index);
                self.pane = Pane::Repositories;
            } else if self.pane == Pane::Resources {
                self.preview_selection();
            }
        }
    }

    pub fn paste(&mut self, text: &str) -> bool {
        if !self.visible {
            return false;
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.query.push_str(text);
            menu.selected = 0;
            return true;
        }
        if self.edit_paste(text) {
            return true;
        }
        if self.search_editing {
            self.filter.search.insert_str(self.search_cursor, text);
            self.search_cursor += text.len();
            self.filter_changed();
        } else {
            self.status = "Press / to search, or choose Edit to change the selected source.".into();
        }
        true
    }

    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.visible {
            return false;
        }
        if self.small {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q' | 'Q')) {
                self.visible = false;
                self.pending = None;
                self.queued = Some(InstructionInspectionRequest::Close);
            }
            return true;
        }
        if self.menu.is_some() {
            self.menu_key(code, modifiers);
            return true;
        }
        if self.edit_key(code, modifiers) {
            return true;
        }
        if code == KeyCode::Char('e') && modifiers.contains(KeyModifiers::CONTROL) {
            self.edit_action(editing::EditAction::Open);
            return true;
        }
        if code == KeyCode::Char('p')
            && modifiers.contains(KeyModifiers::CONTROL)
            && !self.search_editing
        {
            self.open_actions(false);
            return true;
        }
        if self.search_editing {
            match code {
                KeyCode::Esc | KeyCode::Enter => self.search_editing = false,
                KeyCode::Char('u' | 'U') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.search.clear();
                    self.search_cursor = 0;
                    self.filter_changed();
                }
                KeyCode::Backspace => {
                    if self.search_cursor > 0 {
                        let start = self.filter.search[..self.search_cursor]
                            .char_indices()
                            .last()
                            .map_or(0, |(i, _)| i);
                        self.filter.search.drain(start..self.search_cursor);
                        self.search_cursor = start;
                        self.filter_changed();
                    }
                }
                KeyCode::Delete => {
                    if self.search_cursor < self.filter.search.len() {
                        let end = self.search_cursor
                            + self.filter.search[self.search_cursor..]
                                .chars()
                                .next()
                                .expect("character")
                                .len_utf8();
                        self.filter.search.drain(self.search_cursor..end);
                        self.filter_changed();
                    }
                }
                KeyCode::Left => {
                    self.search_cursor = self.filter.search[..self.search_cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(i, _)| i)
                }
                KeyCode::Right => {
                    self.search_cursor += self.filter.search[self.search_cursor..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8)
                }
                KeyCode::Home => self.search_cursor = 0,
                KeyCode::End => self.search_cursor = self.filter.search.len(),
                KeyCode::Char(ch)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.filter.search.insert(self.search_cursor, ch);
                    self.search_cursor += ch.len_utf8();
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
                    self.help_scroll = 0;
                }
                KeyCode::Up | KeyCode::PageUp => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::PageDown => {
                    self.help_scroll = self.help_scroll.saturating_add(1)
                }
                KeyCode::Home => self.help_scroll = 0,
                KeyCode::End => self.help_scroll = usize::MAX,
                _ => {}
            }
            return true;
        }
        let code = match code {
            KeyCode::Char(ch) => KeyCode::Char(ch.to_ascii_lowercase()),
            other => other,
        };
        match code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => self.back(),
            KeyCode::Char('q') => {
                self.visible = false;
                self.pending = None;
                self.queued = Some(InstructionInspectionRequest::Close);
            }
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char(' ' | ':') => self.open_actions(false),
            KeyCode::Char('t') => self.open_actions(true),
            KeyCode::F(1) => self.pane = Pane::Repositories,
            KeyCode::F(2) => self.pane = Pane::Resources,
            KeyCode::F(3) => self.pane = Pane::Detail,
            KeyCode::Char('/') => {
                self.search_editing = true;
                self.search_cursor = self.filter.search.len();
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
                Pane::Repositories => {
                    self.choose_navigation(0);
                    self.pane = Pane::Repositories;
                }
                Pane::Resources => {
                    self.selected = 0;
                    self.preview_selection();
                }
                Pane::Detail => {
                    self.scroll = 0;
                    self.history_selected = 0;
                }
            },
            KeyCode::End => self.navigate(true, usize::MAX),
            KeyCode::Char('n') => self.page(true),
            KeyCode::Char('p') => self.page(false),
            KeyCode::Char('f') => self.open_filters(FilterField::All),
            KeyCode::Char('s') => self.open_scope_choices(),
            KeyCode::Char('e') => self.edit_action(editing::EditAction::Open),
            KeyCode::F(6) => self.new_instruction(),
            KeyCode::Char('v') => self.open_sources(),
            KeyCode::F(5) => self.edit_action(editing::EditAction::GlobalRepository),
            KeyCode::F(4) => self.edit_action(editing::EditAction::ProjectRepository),
            KeyCode::Char('g') => {
                self.filter.redefinitions =
                    (self.filter.redefinitions != Some(true)).then_some(true);
                self.filter_changed();
            }
            KeyCode::Char('o') => self.open_filters(FilterField::Origin),
            KeyCode::Char('c') => {
                self.destination = Destination::Catalog;
                self.repository_selected = 10;
                self.filter = InstructionFilter {
                    grouped: true,
                    main_catalog: true,
                    ..Default::default()
                };
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
            KeyCode::Char('i') if self.history_visible && self.pane == Pane::Detail => {
                if let Some(entry) = self.history.get(self.history_selected) {
                    self.detail(
                        InstructionInspectionView::Metadata,
                        Some(InstructionRevisionSelection {
                            from: entry.commit.clone(),
                            to: None,
                        }),
                    );
                }
            }
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
            KeyCode::Enter | KeyCode::Right => {
                if self.pane == Pane::Repositories {
                    self.choose_navigation(self.repository_selected);
                    return true;
                }

                if self.pane == Pane::Detail {
                    self.open_actions(true);
                } else {
                    self.detail(InstructionInspectionView::Metadata, None);
                }
            }
            _ => {}
        }
        true
    }

    pub fn mouse(&mut self, event: MouseEvent) {
        if !self.visible {
            return;
        }
        if self.menu.is_some() {
            match event.kind {
                MouseEventKind::ScrollDown => self.menu_key(KeyCode::Down, KeyModifiers::NONE),
                MouseEventKind::ScrollUp => self.menu_key(KeyCode::Up, KeyModifiers::NONE),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, index)) = self
                        .menu_hits
                        .iter()
                        .find(|(area, _)| area.contains((event.column, event.row).into()))
                    {
                        if let Some(menu) = self.menu.as_mut() {
                            menu.selected = *index;
                        }
                        self.choose_menu();
                    } else if let Some((_, key)) = self
                        .controls
                        .iter()
                        .find(|(area, _)| area.contains((event.column, event.row).into()))
                    {
                        self.menu_key(*key, KeyModifiers::NONE);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.edit_mouse(event) {
            return;
        }
        if self.help {
            match event.kind {
                MouseEventKind::ScrollDown => self.help_scroll = self.help_scroll.saturating_add(3),
                MouseEventKind::ScrollUp => self.help_scroll = self.help_scroll.saturating_sub(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    if self
                        .controls
                        .iter()
                        .any(|(area, _)| area.contains((event.column, event.row).into()))
                    {
                        self.help = false;
                    }
                }
                _ => {}
            }
            return;
        }
        let point = (event.column, event.row).into();
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            if let Some((_, key)) = self.controls.iter().find(|(area, _)| area.contains(point)) {
                let key = *key;
                self.key(key, KeyModifiers::NONE);
                return;
            }
            if let Some((_, view)) = self.tabs.iter().find(|(area, _)| area.contains(point)) {
                let view = *view;
                self.detail(view, None);
                return;
            }
            if let Some((_, pane, index)) = self
                .list_hits
                .iter()
                .find(|(area, _, _)| area.contains(point))
            {
                self.pane = *pane;
                match pane {
                    Pane::Repositories => {
                        let index = *index;
                        self.choose_navigation(index);
                        self.pane = Pane::Repositories;
                    }
                    Pane::Resources => {
                        self.selected = *index;
                        self.preview_selection();
                    }
                    Pane::Detail => self.history_selected = *index,
                };
                return;
            }
        }
        if let Some(index) = self.areas.iter().position(|area| area.contains(point)) {
            self.pane = [Pane::Repositories, Pane::Resources, Pane::Detail][index];
            match event.kind {
                MouseEventKind::ScrollDown => self.navigate(true, 3),
                MouseEventKind::ScrollUp => self.navigate(false, 3),
                _ => {}
            }
        }
    }

    fn back(&mut self) {
        if self.destination == Destination::Session && self.pane != Pane::Repositories {
            self.pane = Pane::Repositories;
            return;
        }
        if self.revision_open && self.pane == Pane::Detail {
            let loading = self.pending.is_some() || self.queued.is_some();
            self.pending = None;
            self.queued = loading.then_some(InstructionInspectionRequest::Cancel);
            self.revision_open = false;
            self.revision_selection = None;
            self.history_visible = true;
            self.view = InstructionInspectionView::History;
            self.text = None;
            self.status = "Returned to Git history. Comparison base and selection retained.".into();
        } else if self.pane == Pane::Detail {
            let loading = self.pending.is_some() || self.queued.is_some();
            self.pending = None;
            self.queued = loading.then_some(InstructionInspectionRequest::Cancel);
            self.pane = self.return_pane;
            self.status = "Back to browsing. Selection retained.".into();
        } else if self.pane == Pane::Resources && self.filter.repository.is_some() {
            self.filter.repository = None;
            self.filter_changed();
        } else if self.pane == Pane::Resources {
            self.pane = Pane::Repositories;
        } else {
            self.visible = false;
            self.pending = None;
            self.queued = Some(InstructionInspectionRequest::Close);
        }
    }

    fn can_page(&self, forward: bool) -> bool {
        match self.pane {
            Pane::Repositories => false,
            Pane::Resources => {
                if forward {
                    self.row_next.is_some()
                } else {
                    self.row_offset > 0
                }
            }
            Pane::Detail if self.history_visible => {
                if forward {
                    self.history_next.is_some()
                } else {
                    self.history_offset > 0
                }
            }
            Pane::Detail => self.text.as_ref().is_some_and(|page| {
                if forward {
                    page.next.is_some()
                } else {
                    page.offset > 0
                }
            }),
        }
    }

    pub fn debug(&self) -> serde_json::Value {
        serde_json::json!({
            "editing": { "visible":self.editing.visible, "draft":self.editing.draft.as_ref().map(|draft|&draft.id), "generation":self.editing.draft.as_ref().map(|draft|draft.generation), "reviewed":self.editing.draft.as_ref().is_some_and(|draft|draft.reviewed), "working_file_only":self.editing.draft.as_ref().is_some_and(|draft|draft.working_file_only), "pending":self.editing.pending.as_ref().map(|(id,_,_)|id), "failed":self.editing.failed, "storage_blocked":self.editing.storage_blocked },
 "category":self.category_label(),"browse_scope":self.scope_label(),"visible": self.visible, "section": format!("{:?}", self.view), "pane": format!("{:?}", self.pane), "resource_id": self.rows.get(self.selected).map(|row| &row.id), "scope": self.filter.scope, "rows_loaded": self.rows.len(), "resource_offset": self.row_offset, "detail_pages_visited": self.text_offsets.len(), "detail_bytes_loaded": self.text.as_ref().map_or(0, |page| page.text.len()), "valid": self.rows.get(self.selected).map(|row| row.valid), "repositories": self.snapshot.as_ref().map(|snapshot| snapshot.repositories.iter().map(|store| serde_json::json!({"id":store.key,"kind":store.kind,"dirty":store.dirty,"detached":store.detached,"conflicts":store.conflicts,"active_lease":store.active_lease})).collect::<Vec<_>>()), "pending_id": self.pending.as_ref().map(|pending| pending.id), "menu": self.menu.as_ref().map(|menu| &menu.title), "scroll":self.scroll,"detail_view":self.view_label(), "layout": if self.areas.iter().filter(|area| area.width > 0).count() > 1 { "wide" } else { "tabs" } })
    }
}
