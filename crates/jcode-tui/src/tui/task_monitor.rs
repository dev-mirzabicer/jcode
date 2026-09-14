//! Native task monitor presentation. All execution and storage policy stays in services.
use crate::protocol::{Request, ServerEvent};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use jcode_tool_types::{
    cleanup::{CleanupRequest, CleanupResponse, CleanupReview, CleanupSelection},
    execution::{ExecutionContent, ExecutionRequest, ExecutionResponse},
    task_monitor::{
        TaskCursor, TaskMonitorRequest, TaskMonitorResponse, TaskRow, TaskTextPage, TaskView,
    },
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 12;

#[derive(Clone)]
pub enum Action {
    Refresh,
    Completed,
    Active,
    Scope,
    Details,
    Expand,
    Back,
    Input,
    Follow,
    Stop,
    ForceStop,
    Background,
    Context,
    Storage,
    Help,
    Older,
    Newer,
    Review,
    Confirm,
}
#[derive(Clone)]
pub enum Operation {
    Monitor(TaskMonitorRequest),
    Execution(ExecutionRequest),
    Cleanup(CleanupRequest),
    Probe,
}
struct Pending {
    generation: u64,
    operation: Operation,
}

pub struct TaskMonitor {
    pub session: String,
    pub visible: bool,
    pub child_context: Option<String>,
    pub capability: Option<bool>,
    pub status: String,
    view: TaskView,
    all: bool,
    rows: Vec<TaskRow>,
    selected: Option<TaskRow>,
    next: Option<TaskCursor>,
    before: Option<TaskCursor>,
    previous: Vec<Option<TaskCursor>>,
    parents: Vec<String>,
    detail: bool,
    content: ExecutionContent,
    page: Option<TaskTextPage>,
    scroll: usize,
    follow: bool,
    help: bool,
    menu_selection: usize,
    menu_area: Rect,
    menu_offset: usize,
    preview_area: Rect,
    output_focus: bool,
    content_error: Option<String>,
    storage: bool,
    storage_input: String,
    review: Option<CleanupReview>,
    storage_text: String,
    generation: u64,
    pending: HashMap<u64, Pending>,
    pub queued: VecDeque<Operation>,
    last_refresh: Instant,
    hit: Vec<(Rect, Action)>,
    list_area: Rect,
    list_offset: usize,
    dimensions: (u16, u16),
}
impl TaskMonitor {
    pub fn new(session: String, remote: bool) -> Self {
        let mut this = Self {
            session,
            visible: true,
            child_context: None,
            capability: (!remote).then_some(true),
            status: "Loading tasks…".into(),
            view: TaskView::Active,
            all: false,
            rows: vec![],
            selected: None,
            next: None,
            before: None,
            previous: vec![],
            parents: vec![],
            detail: false,
            content: ExecutionContent::Output,
            page: None,
            scroll: 0,
            follow: true,
            help: false,
            menu_selection: 0,
            menu_area: Rect::default(),
            menu_offset: 0,
            preview_area: Rect::default(),
            output_focus: false,
            content_error: None,
            storage: false,
            storage_input: "1073741824".into(),
            review: None,
            storage_text: String::new(),
            generation: 0,
            pending: HashMap::new(),
            queued: VecDeque::new(),
            last_refresh: Instant::now(),
            hit: vec![],
            list_area: Rect::default(),
            list_offset: 0,
            dimensions: (80, 24),
        };
        if remote {
            this.queued.push_back(Operation::Probe);
        } else {
            this.refresh();
        }
        this
    }
    pub fn reconnect(&mut self, session: &str) {
        self.generation += 1;
        self.pending.clear();
        self.queued.clear();
        self.capability = None;
        if session != self.session {
            self.visible = false;
            self.child_context = None;
            return;
        }
        self.status = "Reconnected. Refreshing capabilities and task state.".into();
        self.queued.push_back(Operation::Probe);
    }
    pub fn tick(&mut self) {
        if self.visible
            && !self.storage
            && !self.help
            && self.capability == Some(true)
            && self.last_refresh.elapsed() >= Duration::from_millis(750)
        {
            self.refresh();
        }
    }
    fn list_request(&self, before: Option<TaskCursor>) -> TaskMonitorRequest {
        TaskMonitorRequest::List {
            view: self.view,
            all_sessions: self.all,
            parent_run: self.parents.last().cloned(),
            before,
            limit: Some(100),
        }
    }
    fn queue(&mut self, operation: Operation) {
        let same = |a: &Operation, b: &Operation| {
            std::mem::discriminant(a) == std::mem::discriminant(b)
                && match (a, b) {
                    (Operation::Monitor(a), Operation::Monitor(b)) => {
                        std::mem::discriminant(a) == std::mem::discriminant(b)
                    }
                    _ => true,
                }
        };
        if !self
            .pending
            .values()
            .any(|p| p.generation == self.generation && same(&p.operation, &operation))
            && !self.queued.iter().any(|p| same(p, &operation))
        {
            self.queued.push_back(operation);
        }
    }
    fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        self.queue(Operation::Monitor(self.list_request(self.before.clone())));
        if let Some(selected) = &self.selected {
            self.queue(Operation::Monitor(TaskMonitorRequest::Inspect {
                run_id: selected.run.id.clone(),
            }));
        }
        if self.follow || self.page.is_none() {
            self.read_page(None);
        }
    }
    fn read_page(&mut self, offset: Option<u64>) {
        if let Some(selected) = &self.selected {
            self.queue(Operation::Monitor(TaskMonitorRequest::Read {
                run_id: selected.run.id.clone(),
                content: self.content,
                offset,
                limit: Some(32 * 1024),
            }));
        }
    }
    fn invalidate(&mut self) {
        self.generation += 1;
        self.queued.clear();
        self.page = None;
        self.content_error = None;
        self.scroll = 0;
    }
    fn select(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index).cloned() {
            if self
                .selected
                .as_ref()
                .is_some_and(|old| old.run.id == row.run.id)
            {
                return;
            }
            self.invalidate();
            self.selected = Some(row);
            self.follow = true;
            self.read_page(None);
        }
    }
    pub fn reserve(&mut self, id: u64) -> Option<Request> {
        let operation = self.queued.pop_front()?;
        let request = match &operation {
            Operation::Monitor(request) => Request::TaskMonitor {
                id,
                request: request.clone(),
            },
            Operation::Execution(request) => Request::Execution {
                id,
                request: request.clone(),
            },
            Operation::Cleanup(request) => Request::OutputCleanup {
                id,
                request: request.clone(),
            },
            Operation::Probe => Request::TaskMonitorProbe { id },
        };
        self.pending.insert(
            id,
            Pending {
                generation: self.generation,
                operation,
            },
        );
        Some(request)
    }
    pub fn accepts_id(&self, id: u64) -> bool {
        self.pending.contains_key(&id)
    }
    pub fn accept(&mut self, id: u64, event: ServerEvent) -> bool {
        let Some(pending) = self.pending.remove(&id) else {
            return false;
        };
        if pending.generation != self.generation {
            return true;
        }
        match event {
            ServerEvent::TaskMonitorCapabilities {
                version: 1,
                child_context: true,
                ..
            } => {
                self.capability = Some(true);
                self.refresh();
            }
            ServerEvent::TaskMonitorCapabilities { .. } => {
                self.capability = Some(false);
                self.status =
                    "Task monitor is unsupported by this server. Upgrade before using controls."
                        .into();
            }
            ServerEvent::TaskMonitorResponse { response, .. } => match response {
                TaskMonitorResponse::List { rows, next } => {
                    self.rows = rows;
                    if let Some(selected) = &self.selected {
                        if let Some(current) =
                            self.rows.iter().find(|row| row.run.id == selected.run.id)
                        {
                            self.selected = Some(current.clone());
                        } else {
                            self.rows.insert(0, selected.clone());
                        }
                    }
                    self.next = next;
                    if self.selected.is_none() && !self.rows.is_empty() {
                        self.select(0);
                    }
                    self.status = if self.rows.is_empty() {
                        "No tasks in this view. Switch to Completed or All sessions.".into()
                    } else {
                        format!(
                            "{} loaded{}",
                            self.rows.len(),
                            if self.next.is_some() {
                                " · more available"
                            } else {
                                ""
                            }
                        )
                    };
                }
                TaskMonitorResponse::Status { row } => {
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|old| old.run.id == row.run.id)
                    {
                        if let Some(current) = self
                            .rows
                            .iter_mut()
                            .find(|current| current.run.id == row.run.id)
                        {
                            *current = (*row).clone();
                        }
                        self.selected = Some(*row);
                    }
                }
                TaskMonitorResponse::Text {
                    run_id,
                    content,
                    page,
                } => {
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|row| row.run.id == run_id)
                        && content == self.content
                    {
                        if self.follow {
                            self.scroll =
                                page.text.lines().count().saturating_sub(usize::from(
                                    self.dimensions.1.saturating_sub(11),
                                ));
                        }
                        self.page = Some(page);
                    }
                }
            },
            ServerEvent::ExecutionResponse {
                response:
                    ExecutionResponse::Control {
                        accepted, state, ..
                    },
                ..
            } => {
                self.status = format!(
                    "{} · {:?}. Terminal state is shown only after work stops and persists.",
                    if accepted {
                        "Request accepted"
                    } else {
                        "No change"
                    },
                    state
                );
                self.refresh();
            }
            ServerEvent::OutputCleanupResponse { response, .. } => match response {
                CleanupResponse::Status { status } => {
                    self.storage_text = status.map_or_else(
                        || "No retention scan has completed.".into(),
                        |s| {
                            format!(
                                "Last scan: {}\nArchived: {} · pruned snapshots: {}\n{}",
                                s.checked_at,
                                s.archived_outputs,
                                s.pruned_snapshots,
                                s.issues
                                    .iter()
                                    .map(|i| format!("{}: {}", i.id, i.message))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            )
                        },
                    )
                }
                CleanupResponse::Review { review } => {
                    self.review = Some(review);
                    self.scroll = 0;
                }
                CleanupResponse::Outcome { outcome } => {
                    self.storage_text = outcome
                        .items
                        .iter()
                        .map(|i| {
                            format!(
                                "{}: {}",
                                i.run_id,
                                if i.deleted {
                                    "Deleted".into()
                                } else {
                                    i.error.clone().unwrap_or_else(|| "Not deleted".into())
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.review = None;
                    self.status =
                        "Cleanup outcome retained. Delivered history is unchanged.".into();
                }
            },
            ServerEvent::Error { message, .. } => {
                if matches!(pending.operation, Operation::Probe) {
                    self.capability = Some(false);
                }
                if matches!(
                    &pending.operation,
                    Operation::Monitor(TaskMonitorRequest::Read { .. })
                ) {
                    self.content_error = Some(message.clone());
                }
                self.status = message;
            }
            _ => {
                self.status = "Unexpected task response. Refresh before retrying an action.".into()
            }
        }
        true
    }
    pub fn action(&mut self, action: Action) {
        if self.dimensions.0 < MIN_WIDTH || self.dimensions.1 < MIN_HEIGHT {
            if matches!(action, Action::Back) {
                self.visible = false;
            }
            return;
        }
        if self.storage && !matches!(action, Action::Back | Action::Review | Action::Confirm) {
            return;
        }
        if self.capability != Some(true)
            && !matches!(action, Action::Back | Action::Help | Action::Refresh)
        {
            return;
        }
        self.hit.clear();
        self.list_area = Rect::default();
        self.preview_area = Rect::default();
        match action {
            Action::Back => {
                if self.help {
                    self.help = false;
                } else if self.review.is_some() {
                    self.review = None;
                } else if self.storage {
                    self.storage = false;
                } else if self.detail {
                    self.detail = false;
                    self.output_focus = false;
                } else if self.parents.pop().is_some() {
                    self.invalidate();
                    self.selected = None;
                    self.refresh();
                } else {
                    self.visible = false;
                    self.queued.clear();
                }
            }
            Action::Help => {
                self.help = !self.help;
                self.menu_selection = 0;
            }
            Action::Refresh => {
                if self.capability == Some(false) {
                    self.queued.push_back(Operation::Probe);
                } else {
                    self.refresh();
                }
            }
            Action::Active | Action::Completed | Action::Scope => {
                match action {
                    Action::Active => self.view = TaskView::Active,
                    Action::Completed => self.view = TaskView::Completed,
                    _ => self.all = !self.all,
                }
                self.invalidate();
                self.rows.clear();
                self.selected = None;
                self.parents.clear();
                self.before = None;
                self.previous.clear();
                self.refresh();
            }
            Action::Details => {
                self.detail = true;
                self.output_focus = true;
                self.read_page(None);
            }
            Action::Expand => {
                if let Some(row) = &self.selected
                    && row.expandable
                {
                    let id = row.run.id.clone();
                    self.invalidate();
                    self.parents.push(id);
                    self.before = None;
                    self.previous.clear();
                    self.rows.clear();
                    self.selected = None;
                    self.detail = false;
                    self.refresh();
                }
            }
            Action::Input => {
                self.content = if self.content == ExecutionContent::Input {
                    ExecutionContent::Output
                } else {
                    ExecutionContent::Input
                };
                self.invalidate();
                self.read_page(None);
            }
            Action::Follow => {
                self.follow = !self.follow;
                if !self.follow {
                    self.invalidate_pending_content();
                }
                if self.follow {
                    self.read_page(None);
                }
            }
            Action::ForceStop => {
                if let Some(row) = &self.selected
                    && row.force_stop_available
                    && self.capability == Some(true)
                {
                    self.queue(Operation::Execution(ExecutionRequest::ForceStop {
                        run_id: row.run.id.clone(),
                    }));
                    self.status = "Force stop requested from the verified execution owner.".into();
                }
            }
            Action::Stop | Action::Background => {
                if let Some(row) = &self.selected
                    && !row.run.state.terminal()
                    && self.capability == Some(true)
                {
                    self.queue(Operation::Execution(if matches!(action, Action::Stop) {
                        ExecutionRequest::Stop {
                            run_id: row.run.id.clone(),
                        }
                    } else {
                        ExecutionRequest::Background {
                            run_id: row.run.id.clone(),
                        }
                    }));
                    self.status = "Control requested. Waiting for the execution owner.".into();
                }
            }
            Action::Context => {
                if let Some(row) = &self.selected
                    && let Some(child) = &row.child_id
                {
                    self.child_context = Some(child.clone());
                }
            }
            Action::Storage => {
                self.storage = true;
                self.scroll = 0;
                self.queue(Operation::Cleanup(CleanupRequest::Status));
            }
            Action::Review => {
                if let Ok(bytes) = self.storage_input.parse::<u64>()
                    && bytes > 0
                {
                    self.queue(Operation::Cleanup(CleanupRequest::Review {
                        selection: CleanupSelection::OldestBytes { bytes },
                    }));
                } else {
                    self.status = "Enter a positive byte target.".into();
                }
            }
            Action::Confirm => {
                if let Some(review) = &self.review {
                    self.queue(Operation::Cleanup(CleanupRequest::Confirm {
                        review_id: review.review_id.clone(),
                        confirmation_id: review.confirmation_id.clone(),
                    }));
                }
            }
            Action::Older | Action::Newer => {
                if !self.detail && !self.output_focus {
                    let next = if matches!(action, Action::Newer) {
                        let Some(next) = self.next.clone() else {
                            return;
                        };
                        self.previous.push(self.before.clone());
                        Some(next)
                    } else {
                        let Some(previous) = self.previous.pop() else {
                            return;
                        };
                        previous
                    };
                    self.invalidate();
                    self.before = next;
                    self.selected = None;
                    self.rows.clear();
                    self.list_offset = 0;
                    self.refresh();
                    return;
                }
                self.follow = false;
                self.invalidate_pending_content();
                if let Some(page) = &self.page {
                    let offset = if matches!(action, Action::Older) {
                        page.start.saturating_sub(32 * 1024)
                    } else {
                        page.end
                    };
                    self.scroll = 0;
                    self.read_page(Some(offset));
                }
            }
        }
    }
    fn invalidate_pending_content(&mut self) {
        self.queued
            .retain(|op| !matches!(op, Operation::Monitor(TaskMonitorRequest::Read { .. })));
        for pending in self.pending.values_mut() {
            if matches!(
                &pending.operation,
                Operation::Monitor(TaskMonitorRequest::Read { .. })
            ) {
                pending.generation = u64::MAX;
            }
        }
    }
    fn actions(&self) -> Vec<(&'static str, Action)> {
        let mut actions = vec![
            ("1  Active work", Action::Active),
            ("2  Completed work", Action::Completed),
            ("a  This session / all sessions", Action::Scope),
            ("Enter  Task details", Action::Details),
            ("Right  Expand batch / child", Action::Expand),
            ("i  Input / output", Action::Input),
            ("f  Follow / pause output", Action::Follow),
            ("[  Previous page", Action::Older),
            ("]  Next page", Action::Newer),
        ];
        if self
            .selected
            .as_ref()
            .is_some_and(|row| !row.run.state.terminal())
        {
            actions.extend([
                ("s  Stop selected work", Action::Stop),
                ("b  Background selected work", Action::Background),
            ]);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|row| row.force_stop_available)
        {
            actions.push(("S  Force stop owned process", Action::ForceStop));
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|row| row.child_id.is_some())
        {
            actions.push(("c  Child Context Editor (idle only)", Action::Context));
        }
        actions.extend([
            ("g  Storage review", Action::Storage),
            ("r  Refresh", Action::Refresh),
            ("Esc  Back", Action::Back),
        ]);
        actions
    }
    pub fn paste(&mut self, text: &str) {
        if self.storage && self.review.is_none() && !self.help {
            let text = text.trim();
            if text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit()) {
                self.storage_input = text.into();
            } else {
                self.status = "Storage byte target must be a positive integer.".into();
            }
        }
    }
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.visible {
            return false;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.action(Action::Back);
            return true;
        }
        if self.dimensions.0 < MIN_WIDTH || self.dimensions.1 < MIN_HEIGHT {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                self.visible = false;
            }
            return true;
        }
        let delta = match code {
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::PageUp => -10,
            KeyCode::PageDown => 10,
            _ => 0,
        };
        if self.help {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')) {
                self.help = false;
            } else if code == KeyCode::Enter {
                if let Some((_, action)) = self.actions().get(self.menu_selection).cloned() {
                    self.help = false;
                    self.action(action);
                }
            } else {
                self.menu_selection = self
                    .menu_selection
                    .saturating_add_signed(delta)
                    .min(self.actions().len().saturating_sub(1));
            }
            return true;
        }
        if self.storage {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => self.action(Action::Back),
                KeyCode::Char('y') if self.review.is_some() => self.action(Action::Confirm),
                KeyCode::Enter if self.review.is_none() => self.action(Action::Review),
                KeyCode::Char(c)
                    if self.review.is_none()
                        && c.is_ascii_digit()
                        && self.storage_input.len() < 20 =>
                {
                    self.storage_input.push(c)
                }
                KeyCode::Backspace if self.review.is_none() => {
                    self.storage_input.pop();
                }
                _ => {
                    self.scroll = self.scroll.saturating_add_signed(delta);
                }
            }
            return true;
        }
        if code == KeyCode::Tab {
            self.output_focus = !self.output_focus;
            return true;
        }
        let action = match code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::Back),
            KeyCode::Char('?') => Some(Action::Help),
            KeyCode::Char('1') => Some(Action::Active),
            KeyCode::Char('2') => Some(Action::Completed),
            KeyCode::Char('a') => Some(Action::Scope),
            KeyCode::Enter => Some(Action::Details),
            KeyCode::Right | KeyCode::Char('l') => Some(Action::Expand),
            KeyCode::Left | KeyCode::Char('h') => Some(Action::Back),
            KeyCode::Char('i') => Some(Action::Input),
            KeyCode::Char('f') => Some(Action::Follow),
            KeyCode::Char('s') => Some(Action::Stop),
            KeyCode::Char('S') => Some(Action::ForceStop),
            KeyCode::Char('b') => Some(Action::Background),
            KeyCode::Char('c') => Some(Action::Context),
            KeyCode::Char('g') => Some(Action::Storage),
            KeyCode::Char('r') => Some(Action::Refresh),
            KeyCode::Char('y') if self.review.is_some() => Some(Action::Confirm),
            KeyCode::Char('[') => Some(Action::Older),
            KeyCode::Char(']') => Some(Action::Newer),
            _ => None,
        };
        if let Some(action) = action {
            self.action(action);
        } else {
            let delta = match code {
                KeyCode::Up | KeyCode::Char('k') => -1,
                KeyCode::Down | KeyCode::Char('j') => 1,
                KeyCode::PageUp => -10,
                KeyCode::PageDown => 10,
                _ => 0,
            };
            if delta != 0 {
                if self.detail || self.output_focus {
                    self.follow = false;
                    self.invalidate_pending_content();
                    self.scroll = self.scroll.saturating_add_signed(delta);
                } else {
                    let current = self
                        .selected
                        .as_ref()
                        .and_then(|r| self.rows.iter().position(|row| row.run.id == r.run.id))
                        .unwrap_or(0);
                    let index = current
                        .saturating_add_signed(delta)
                        .min(self.rows.len().saturating_sub(1));
                    self.select(index);
                }
            }
        }
        true
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        if !self.visible {
            return;
        }
        match event.kind {
            MouseEventKind::Down(_) => {
                let position = ratatui::layout::Position::new(event.column, event.row);
                if let Some((_, action)) = self.hit.iter().find(|(rect, _)| rect.contains(position))
                {
                    let action = action.clone();
                    self.help = false;
                    self.action(action);
                } else if self.preview_area.contains(position) {
                    self.output_focus = true;
                } else if self.list_area.contains(position) {
                    self.output_focus = false;
                    self.select(self.list_offset + usize::from(event.row - self.list_area.y));
                }
            }
            MouseEventKind::ScrollDown => {
                if !self.help && !self.storage {
                    self.output_focus = self
                        .preview_area
                        .contains(ratatui::layout::Position::new(event.column, event.row));
                }
                self.key(KeyCode::Down, KeyModifiers::NONE);
            }
            MouseEventKind::ScrollUp => {
                if !self.help && !self.storage {
                    self.output_focus = self
                        .preview_area
                        .contains(ratatui::layout::Position::new(event.column, event.row));
                }
                self.key(KeyCode::Up, KeyModifiers::NONE);
            }
            _ => {}
        }
    }
    fn buttons(&mut self, frame: &mut Frame, area: Rect, buttons: &[(&str, Action)]) {
        let mut x = area.x;
        for (label, action) in buttons {
            let width = unicode_width::UnicodeWidthStr::width(*label) as u16;
            if x.saturating_add(width) > area.right() {
                break;
            }
            let rect = Rect::new(x, area.y, width, 1);
            frame.render_widget(
                Paragraph::new(*label).style(Style::default().add_modifier(Modifier::BOLD)),
                rect,
            );
            self.hit.push((rect, action.clone()));
            x = x.saturating_add(width + 2);
        }
    }
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.dimensions = (area.width, area.height);
        self.hit.clear();
        self.list_area = Rect::default();
        self.preview_area = Rect::default();
        self.menu_area = Rect::default();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Tasks needs {MIN_WIDTH}×{MIN_HEIGHT}\nResize or Esc to close"
                ))
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let [header, nav, body, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(format!(
                "Tasks · {} · {}",
                if self.view == TaskView::Active {
                    "Active"
                } else {
                    "Completed"
                },
                if self.all {
                    "All Jcode sessions"
                } else {
                    "This session + children"
                }
            ))
            .style(Style::default().add_modifier(Modifier::BOLD)),
            header,
        );
        if !self.help && !self.storage {
            self.buttons(
                frame,
                nav,
                &[
                    ("1 Active", Action::Active),
                    ("2 Completed", Action::Completed),
                    ("? Actions", Action::Help),
                ],
            );
        }
        if self.help {
            let actions = self.actions();
            self.menu_area = body;
            let mut state = ListState::default()
                .with_selected(Some(self.menu_selection))
                .with_offset(self.menu_offset);
            frame.render_stateful_widget(
                List::new(
                    actions
                        .iter()
                        .map(|(label, _)| ListItem::new(*label))
                        .collect::<Vec<_>>(),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
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
            self.buttons(frame, footer, &[("Esc Back", Action::Back)]);
        } else if self.storage {
            let text = if let Some(review) = &self.review {
                format!(
                    "Cleanup review {}\nRequested: {:?} B · selected: {} B · overshoot: {} B\n{}\n\n{}\n\ny Confirm deletion · Esc Back (no deletion)",
                    review.review_id,
                    review.requested_bytes,
                    review.selected_bytes,
                    review.overshoot_bytes,
                    review.impact,
                    review
                        .candidates
                        .iter()
                        .map(|c| format!(
                            "{} · {} B\n  session {} · snapshots {}",
                            c.run_id,
                            c.bytes,
                            c.session_id,
                            c.affected_snapshot_ids.join(", ")
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else {
                format!(
                    "Storage administration\nCold archived completed outputs only. Live/recent spillover excluded.\n\nOldest whole outputs, byte target: {}\nEnter Review (does not delete)\n\n{}",
                    self.storage_input, self.storage_text
                )
            };
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll.min(usize::from(u16::MAX)) as u16, 0)),
                body,
            );
            if self.review.is_some() {
                self.buttons(
                    frame,
                    footer,
                    &[("y Confirm", Action::Confirm), ("Esc Back", Action::Back)],
                );
            } else {
                self.buttons(
                    frame,
                    footer,
                    &[("Enter Review", Action::Review), ("Esc Back", Action::Back)],
                );
            }
        } else {
            if self.detail {
                self.preview_area = body;
                self.render_detail(frame, body);
            } else if area.width >= 120 {
                let [list, preview] =
                    Layout::horizontal([Constraint::Percentage(45), Constraint::Fill(1)])
                        .areas(body);
                self.render_list(frame, list);
                self.preview_area = preview;
                self.render_detail(frame, preview);
            } else {
                self.render_list(frame, body);
            }
            self.buttons(
                frame,
                footer,
                &[
                    ("? Actions", Action::Help),
                    ("Enter Detail", Action::Details),
                    ("Esc Back", Action::Back),
                ],
            );
        }
        frame.render_widget(
            Paragraph::new(&*self.status).wrap(Wrap { trim: false }),
            status,
        );
    }
    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        self.list_area = area;
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
                ListItem::new(format!(
                    "{} {:10?} {} {}",
                    if row.expandable { "▸" } else { " " },
                    row.run.state,
                    row.run.tool,
                    row.run.id.get(4..12).unwrap_or(&row.run.id)
                ))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            area,
            &mut state,
        );
        self.list_offset = state.offset();
    }
    fn render_detail(&mut self, frame: &mut Frame, area: Rect) {
        let Some(row) = &self.selected else {
            frame.render_widget(
                Paragraph::new("Select a task to inspect input and retained output."),
                area,
            );
            return;
        };
        let header = format!(
            "{} · {:?}{}\n{}\nSession: {}\n{} · {} B · {}\n",
            row.run.tool,
            row.run.state,
            if row.run.stop_cause.is_some() {
                " · Stop requested"
            } else {
                ""
            },
            row.run.id,
            row.run.session_id,
            if row.run.background {
                "Background"
            } else {
                "Foreground"
            },
            row.run.output_bytes,
            if row.run.complete {
                "Complete capture"
            } else {
                "Partial/unavailable capture"
            }
        );
        let [meta, text] =
            Layout::vertical([Constraint::Length(6), Constraint::Min(1)]).areas(area);
        frame.render_widget(Paragraph::new(header).wrap(Wrap { trim: false }), meta);
        let body = self.page.as_ref().map_or_else(
            || {
                self.content_error
                    .clone()
                    .unwrap_or_else(|| "Loading retained content…".into())
            },
            |p| {
                format!(
                    "{:?} · {} · bytes {}..{}/{}\n{}",
                    self.content,
                    if self.follow { "Following" } else { "Paused" },
                    p.start,
                    p.end,
                    p.total,
                    p.text
                )
            },
        );
        let clean = crate::message::strip_ansi_escape_sequences(&body);
        let lines = clean
            .lines()
            .flat_map(|line| {
                crate::tui::markdown::wrap_line(
                    ratatui::text::Line::from(line.to_owned()),
                    usize::from(text.width),
                )
            })
            .collect::<Vec<_>>();
        let max_scroll = lines.len().saturating_sub(usize::from(text.height));
        if self.follow {
            self.scroll = max_scroll;
        } else {
            self.scroll = self.scroll.min(max_scroll);
        }
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.scroll)
                    .take(usize::from(text.height))
                    .collect::<Vec<_>>(),
            ),
            text,
        );
    }

    pub fn debug(&self) -> serde_json::Value {
        serde_json::json!({"visible":self.visible,"session":self.session,"rows":self.rows.len(),"selected":self.selected.as_ref().map(|r|&r.run.id),"selected_state":self.selected.as_ref().map(|r|r.run.state),"selected_child":self.selected.as_ref().and_then(|r|r.child_id.as_ref()),"items":self.rows.iter().map(|r|serde_json::json!({"id":r.run.id,"tool":r.run.tool,"state":r.run.state,"child_id":r.child_id})).collect::<Vec<_>>(),"follow":self.follow,"detail":self.detail,"storage":self.storage,"status":self.status,"all_sessions":self.all,"output_end":self.page.as_ref().map(|p|p.end),"dimensions":self.dimensions})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn row(id: &str, state: &str) -> TaskRow {
        serde_json::from_value(serde_json::json!({"run":{"id":id,"session_id":"parent","message_id":"m","tool":"bash","state":state,"owner":"runtime","input_path":"input","result_path":null,"output_path":"output","output_bytes":100,"complete":state=="completed","background":false,"stop_cause":null,"parent_id":null},"created":1,"updated":1,"child_id":null,"expandable":false,"force_stop_available":false})).unwrap()
    }
    fn fixture() -> TaskMonitor {
        let mut monitor = TaskMonitor::new("parent".into(), false);
        monitor.queued.clear();
        monitor.rows = vec![row("run-one", "running"), row("run-two", "running")];
        monitor.select(0);
        monitor.queued.clear();
        monitor
    }
    fn frame(m: &mut TaskMonitor, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| m.render(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn selected_completion_remains_pinned_without_retargeting_controls() {
        let mut m = fixture();
        m.queue(Operation::Monitor(TaskMonitorRequest::Inspect {
            run_id: "run-one".into(),
        }));
        m.reserve(1);
        m.accept(
            1,
            ServerEvent::TaskMonitorResponse {
                id: 1,
                response: TaskMonitorResponse::Status {
                    row: Box::new(row("run-one", "completed")),
                },
            },
        );
        m.queue(Operation::Monitor(m.list_request(None)));
        m.reserve(2);
        m.accept(
            2,
            ServerEvent::TaskMonitorResponse {
                id: 2,
                response: TaskMonitorResponse::List {
                    rows: vec![row("run-two", "running")],
                    next: None,
                },
            },
        );
        assert_eq!(m.selected.as_ref().unwrap().run.id, "run-one");
        assert_eq!(m.rows[0].run.id, "run-one");
        m.queued.clear();
        m.action(Action::Stop);
        assert!(m.queued.is_empty());
        assert!(frame(&mut m, 80, 24).contains("Completed"));
    }
    #[test]
    fn late_output_cannot_replace_new_selection_or_paused_view() {
        let mut m = fixture();
        m.read_page(None);
        m.reserve(1);
        m.select(1);
        m.accept(
            1,
            ServerEvent::TaskMonitorResponse {
                id: 1,
                response: TaskMonitorResponse::Text {
                    run_id: "run-one".into(),
                    content: ExecutionContent::Output,
                    page: TaskTextPage {
                        start: 0,
                        end: 3,
                        total: 3,
                        text: "OLD".into(),
                    },
                },
            },
        );
        assert!(m.page.is_none());
        m.reserve(2);
        m.action(Action::Follow);
        m.accept(
            2,
            ServerEvent::TaskMonitorResponse {
                id: 2,
                response: TaskMonitorResponse::Text {
                    run_id: "run-two".into(),
                    content: ExecutionContent::Output,
                    page: TaskTextPage {
                        start: 0,
                        end: 3,
                        total: 3,
                        text: "NEW".into(),
                    },
                },
            },
        );
        assert!(m.page.is_none());
        assert!(!m.follow);
    }
    #[test]
    fn cleanup_modal_traps_input_and_confirms_exact_review_once() {
        let mut m = fixture();
        m.action(Action::Storage);
        m.queued.clear();
        m.review = Some(CleanupReview {
            review_id: "review".into(),
            confirmation_id: "confirmed".into(),
            candidates: vec![],
            requested_bytes: Some(1),
            selected_bytes: 2,
            overshoot_bytes: 1,
            impact: "snapshot impact".into(),
        });
        for key in ['s', 'S', 'b', 'c', '1', '2'] {
            m.key(KeyCode::Char(key), KeyModifiers::NONE);
        }
        m.key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(m.queued.is_empty());
        m.key(KeyCode::Char('y'), KeyModifiers::NONE);
        m.key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(m.queued.len(), 1);
        assert!(
            matches!(m.reserve(3),Some(Request::OutputCleanup{request:CleanupRequest::Confirm{review_id,confirmation_id},..}) if review_id=="review" && confirmation_id=="confirmed")
        );
        m.key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(m.queued.is_empty());
    }
    #[test]
    fn responsive_frames_keep_actions_reachable_and_follow_actual_wrapped_tail() {
        for (w, h) in [(140, 32), (80, 24), (60, 24), (48, 12)] {
            let mut m = fixture();
            m.detail = true;
            m.page = Some(TaskTextPage {
                start: 0,
                end: 4000,
                total: 4000,
                text: format!("{}TAIL_SENTINEL", "漢字e\u{301}".repeat(700)),
            });
            let text = frame(&mut m, w, h);
            assert!(text.contains("TAIL_SENTINEL"), "{w}x{h}: {text}");
            assert!(text.contains("? Actions"));
            m.action(Action::Help);
            for index in 0..m.actions().len() {
                m.menu_selection = index;
                frame(&mut m, w, h);
                assert!(m.hit.iter().any(|(_, a)| std::mem::discriminant(a)
                    == std::mem::discriminant(&m.actions()[index].1)));
            }
        }
        let mut m = fixture();
        m.action(Action::Help);
        let text = frame(&mut m, 47, 11);
        assert!(text.contains("48×12"));
        assert!(m.hit.is_empty());
        m.queued.clear();
        m.key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(m.queued.is_empty());
        m.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.visible);
    }
    #[test]
    fn historical_pages_replace_bounded_metadata_and_refresh_same_cursor() {
        let mut m = fixture();
        m.next = Some(TaskCursor {
            created: 1,
            run_id: "run-one".into(),
        });
        m.action(Action::Newer);
        assert!(m.rows.is_empty());
        assert_eq!(m.previous.len(), 1);
        let request = m.reserve(1).unwrap();
        assert!(matches!(
            request,
            Request::TaskMonitor {
                request: TaskMonitorRequest::List {
                    before: Some(_),
                    ..
                },
                ..
            }
        ));
        m.accept(
            1,
            ServerEvent::TaskMonitorResponse {
                id: 1,
                response: TaskMonitorResponse::List {
                    rows: vec![row("old", "completed")],
                    next: None,
                },
            },
        );
        m.queued.clear();
        m.refresh();
        assert!(matches!(
            m.queued.front(),
            Some(Operation::Monitor(TaskMonitorRequest::List {
                before: Some(_),
                ..
            }))
        ));
        assert!(m.rows.len() <= 101);
    }
}
