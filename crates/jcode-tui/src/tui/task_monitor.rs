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
use ratatui::layout::Rect;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

mod view;

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
    Output,
    Info,
    RawInput,
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

struct Breadcrumb {
    row: TaskRow,
    before: Option<TaskCursor>,
    previous: Vec<Option<TaskCursor>>,
}

pub struct TaskMonitor {
    pub session: String,
    pub visible: bool,
    pub child_context: Option<String>,
    pub capability: Option<bool>,
    pub status: String,
    status_until: Option<Instant>,
    view: TaskView,
    all: bool,
    rows: Vec<TaskRow>,
    selected: Option<TaskRow>,
    next: Option<TaskCursor>,
    before: Option<TaskCursor>,
    previous: Vec<Option<TaskCursor>>,
    parents: Vec<Breadcrumb>,
    detail: bool,
    content: ExecutionContent,
    info: bool,
    raw_input: bool,
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
    deferred_read: Option<Operation>,
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
            status_until: None,
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
            info: false,
            raw_input: false,
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
            deferred_read: None,
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
        self.deferred_read = None;
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
        if self
            .status_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.status.clear();
            self.status_until = None;
        }
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
            parent_run: self.parents.last().map(|parent| parent.row.run.id.clone()),
            before,
            limit: Some(100),
        }
    }
    fn queue(&mut self, operation: Operation) {
        // Keep one body read in flight even across selection generations. A slow
        // archive cannot grow a new blocking worker for every arrow/mouse event.
        if matches!(
            &operation,
            Operation::Monitor(TaskMonitorRequest::Read { .. })
        ) && self.pending.values().any(|pending| {
            matches!(
                &pending.operation,
                Operation::Monitor(TaskMonitorRequest::Read { .. })
            )
        }) {
            self.deferred_read = Some(operation);
            return;
        }
        let same = |a: &Operation, b: &Operation| match (a, b) {
            (Operation::Monitor(a), Operation::Monitor(b)) => {
                std::mem::discriminant(a) == std::mem::discriminant(b)
            }
            (Operation::Execution(a), Operation::Execution(b)) => a == b,
            (Operation::Cleanup(_), Operation::Cleanup(_))
            | (Operation::Probe, Operation::Probe) => true,
            _ => false,
        };
        if !self
            .pending
            .values()
            .any(|pending| same(&pending.operation, &operation))
            && !self.queued.iter().any(|queued| same(queued, &operation))
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
        if (self.page.is_none() && self.content_error.is_none())
            || (self.follow
                && self.content == ExecutionContent::Output
                && self
                    .selected
                    .as_ref()
                    .is_some_and(|row| !row.run.state.terminal()))
        {
            self.read_page(None);
        }
    }
    fn read_page(&mut self, offset: Option<u64>) {
        if self.info {
            return;
        }
        if let Some(selected) = &self.selected {
            if self.content == ExecutionContent::Output
                && selected.run.output_path.is_none()
                && !selected.run.state.terminal()
            {
                return;
            }
            self.queue(Operation::Monitor(TaskMonitorRequest::Read {
                run_id: selected.run.id.clone(),
                content: self.content,
                offset,
                limit: Some(32 * 1024),
            }));
        }
    }
    fn invalidate(&mut self) {
        self.status.clear();
        self.status_until = None;
        self.generation += 1;
        self.queued.clear();
        self.page = None;
        self.deferred_read = None;
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
            self.follow = self.content == ExecutionContent::Output && !self.info;
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
        if matches!(
            &pending.operation,
            Operation::Monitor(TaskMonitorRequest::Read { .. })
        ) && let Some(operation) = self.deferred_read.take()
            && self.visible
        {
            self.queued.push_back(operation);
        }
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
                self.status.clear();
                self.status_until = None;
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
                    if self.status == "Loading tasks…" {
                        self.status.clear();
                    }
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
                        let behind = self.follow
                            && self.content == ExecutionContent::Output
                            && self
                                .page
                                .as_ref()
                                .is_some_and(|page| page.total < row.run.output_bytes);
                        self.selected = Some(*row);
                        if behind {
                            self.read_page(None);
                        }
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
                        let behind = self.follow
                            && content == ExecutionContent::Output
                            && self
                                .selected
                                .as_ref()
                                .is_some_and(|row| row.run.output_bytes > page.total);
                        self.page = Some(page);
                        if self.content_error.as_ref() == Some(&self.status) {
                            self.status.clear();
                        }
                        self.content_error = None;
                        if behind {
                            self.read_page(None);
                        }
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
                self.status_until = Some(Instant::now() + Duration::from_secs(5));
                self.status = format!(
                    "{} · {:?}. Waiting for the owner's final state.",
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
                self.status_until = None;
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
                } else if let Some(parent) = self.parents.pop() {
                    self.invalidate();
                    self.rows.clear();
                    self.list_offset = 0;
                    self.before = parent.before;
                    self.previous = parent.previous;
                    self.selected = Some(parent.row);
                    self.output_focus = false;
                    self.refresh();
                } else {
                    self.visible = false;
                    self.queued.clear();
                    self.deferred_read = None;
                }
            }
            Action::Help => {
                self.help = !self.help;
                self.menu_selection = 0;
            }
            Action::Refresh => {
                self.content_error = None;
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
                    let parent = Breadcrumb {
                        row: row.clone(),
                        before: self.before.clone(),
                        previous: std::mem::take(&mut self.previous),
                    };
                    self.invalidate();
                    self.parents.push(parent);
                    self.before = None;
                    self.previous.clear();
                    self.rows.clear();
                    self.selected = None;
                    self.detail = false;
                    self.refresh();
                }
            }
            Action::Input | Action::Output => {
                let content = if matches!(action, Action::Input) {
                    ExecutionContent::Input
                } else {
                    ExecutionContent::Output
                };
                self.output_focus = true;
                if self.dimensions.0 < 120 {
                    self.detail = true;
                }
                if self.content != content || self.info {
                    self.invalidate();
                    self.content = content;
                    self.info = false;
                    self.follow = content == ExecutionContent::Output;
                    self.read_page(None);
                }
            }
            Action::Info => {
                self.output_focus = true;
                if self.dimensions.0 < 120 {
                    self.detail = true;
                }
                self.invalidate_pending_content();
                self.info = true;
                self.follow = false;
                self.scroll = 0;
            }
            Action::RawInput => {
                if self.content == ExecutionContent::Input && !self.info {
                    self.raw_input = !self.raw_input;
                    self.scroll = 0;
                }
            }
            Action::Follow => {
                if self.content != ExecutionContent::Output || self.info {
                    return;
                }
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
                self.status.clear();
                self.status_until = None;
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
        self.deferred_read = None;
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
            ("i  Input: arguments sent by the agent", Action::Input),
            ("o  Output: retained result / live stream", Action::Output),
            ("m  Info: identity, timing and capture", Action::Info),
            ("[  Previous page", Action::Older),
            ("]  Next page", Action::Newer),
        ];
        if self.content == ExecutionContent::Output && !self.info {
            actions.push(("f  Follow / pause output", Action::Follow));
        }
        if self.content == ExecutionContent::Input && !self.info {
            actions.push(("v  Arguments / raw receipt JSON", Action::RawInput));
        }
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
        if self.storage
            && !matches!(code, KeyCode::Esc | KeyCode::Char('q'))
            && (self
                .pending
                .values()
                .any(|pending| matches!(&pending.operation, Operation::Cleanup(_)))
                || self
                    .queued
                    .iter()
                    .any(|operation| matches!(operation, Operation::Cleanup(_))))
        {
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
        if matches!(code, KeyCode::Tab | KeyCode::BackTab) {
            if self.dimensions.0 >= 120 && !self.detail {
                self.output_focus = !self.output_focus;
            }
            return true;
        }
        if matches!(code, KeyCode::Home | KeyCode::End) && (self.detail || self.output_focus) {
            self.follow = false;
            self.invalidate_pending_content();
            if self.info {
                self.scroll = if code == KeyCode::Home { 0 } else { usize::MAX };
            } else if code == KeyCode::Home {
                self.scroll = 0;
                self.read_page(Some(0));
            } else if self.content == ExecutionContent::Output {
                self.follow = true;
                self.read_page(None);
            } else {
                self.scroll = usize::MAX;
            }
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
            KeyCode::Char('o') => Some(Action::Output),
            KeyCode::Char('m') => Some(Action::Info),
            KeyCode::Char('v') => Some(Action::RawInput),
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
    pub fn debug(&self) -> serde_json::Value {
        serde_json::json!({"visible":self.visible,"capability":self.capability,"generation":self.generation,"session":self.session,"rows":self.rows.len(),"selected":self.selected.as_ref().map(|r|&r.run.id),"selected_state":self.selected.as_ref().map(|r|r.run.state),"selected_child":self.selected.as_ref().and_then(|r|r.child_id.as_ref()),"items":self.rows.iter().map(|r|serde_json::json!({"id":r.run.id,"tool":r.run.tool,"state":r.run.state,"child_id":r.child_id})).collect::<Vec<_>>(),"follow":self.follow,"detail":self.detail,"content":self.content,"info":self.info,"raw_input":self.raw_input,"storage":self.storage,"status":self.status,"all_sessions":self.all,"output_end":self.page.as_ref().map(|p|p.end),"dimensions":self.dimensions})
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
            // The sentinel may wrap at a different column after adding visible tabs.
            assert!(
                text.split_whitespace()
                    .collect::<String>()
                    .contains("TAIL_SENTINEL"),
                "{w}x{h}: {text}"
            );
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
    #[test]
    fn rapid_selection_keeps_one_slow_body_read_and_controls_stay_independent() {
        let mut m = fixture();
        m.read_page(None);
        m.reserve(1);
        for i in 0..1000 {
            m.select(i % 2);
        }
        assert_eq!(m.pending.len(), 1);
        assert!(m.deferred_read.is_some());
        assert!(!m.queued.iter().any(|operation| matches!(
            operation,
            Operation::Monitor(TaskMonitorRequest::Read { .. })
        )));
        m.action(Action::Stop);
        assert!(m.queued.iter().any(|operation| matches!(
            operation,
            Operation::Execution(ExecutionRequest::Stop { .. })
        )));
        m.accept(
            1,
            ServerEvent::Error {
                id: 1,
                message: "old read failed".into(),
                retry_after_secs: None,
            },
        );
        assert_eq!(
            m.queued
                .iter()
                .filter(|operation| matches!(
                    operation,
                    Operation::Monitor(TaskMonitorRequest::Read { .. })
                ))
                .count(),
            1
        );
        assert!(m.page.is_none());
    }
    #[test]
    fn back_from_child_or_batch_restores_parent_identity_and_page() {
        let mut m = fixture();
        m.rows[1].expandable = true;
        m.select(1);
        m.before = Some(TaskCursor {
            created: 5,
            run_id: "page".into(),
        });
        m.action(Action::Expand);
        assert_eq!(m.parents.len(), 1);
        assert!(m.selected.is_none());
        m.action(Action::Back);
        assert!(m.parents.is_empty());
        assert_eq!(m.selected.as_ref().unwrap().run.id, "run-two");
        assert_eq!(m.before.as_ref().unwrap().run_id, "page");
    }
    #[test]
    fn explicit_input_opens_arguments_at_top_and_mouse_output_tab_switches_without_effects() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut m = fixture();
        frame(&mut m, 80, 24);
        m.key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(m.detail);
        assert!(!m.follow);
        assert!(!m.info);
        assert_eq!(m.content, ExecutionContent::Input);
        let request = m.reserve(80).unwrap();
        assert!(matches!(
            request,
            Request::TaskMonitor {
                request: TaskMonitorRequest::Read {
                    content: ExecutionContent::Input,
                    offset: None,
                    ..
                },
                ..
            }
        ));
        let original=serde_json::json!({"session_id":"parent","message_id":"m","tool":"bash","input":{"command":"printf INPUT_FIRST\nprintf NEXT_LINE","intent":"Read actual arguments","list":[1,true,null]},"working_dir":"/fixture"}).to_string();
        m.accept(
            80,
            ServerEvent::TaskMonitorResponse {
                id: 80,
                response: TaskMonitorResponse::Text {
                    run_id: "run-one".into(),
                    content: ExecutionContent::Input,
                    page: TaskTextPage {
                        start: 0,
                        end: original.len() as u64,
                        total: original.len() as u64,
                        text: original.clone(),
                    },
                },
            },
        );
        let rendered = frame(&mut m, 80, 24);
        assert!(rendered.contains("INPUT_FIRST"));
        assert!(
            rendered.contains("i Input")
                && rendered.contains("o Output")
                && rendered.contains("m Info")
        );
        assert_eq!(m.scroll, 0);
        assert!(!rendered.contains("session_id"));
        m.action(Action::RawInput);
        let raw = frame(&mut m, 80, 24);
        assert!(raw.contains("session_id"));
        assert_eq!(m.page.as_ref().unwrap().text, original);
        let rect = m
            .hit
            .iter()
            .find(|(_, action)| matches!(action, Action::Output))
            .unwrap()
            .0;
        m.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(m.content, ExecutionContent::Output);
        assert!(m.follow);
        assert!(
            m.queued
                .iter()
                .all(|op| matches!(op, Operation::Monitor(_)))
        );
    }

    #[test]
    fn input_tabs_and_exact_argument_view_survive_narrow_floor_and_large_receipt_paging() {
        let mut m = fixture();
        m.action(Action::Input);
        m.queued.clear();
        for (w, h) in [(140, 32), (80, 24), (60, 24), (48, 12)] {
            let input=serde_json::json!({"input":{"command":"FIRST ARGUMENT","empty":"","null":null,"flag":false}}).to_string();
            m.page = Some(TaskTextPage {
                start: 0,
                end: input.len() as u64,
                total: input.len() as u64,
                text: input,
            });
            let text = frame(&mut m, w, h);
            assert!(
                text.contains("i Input") && text.contains("o Output") && text.contains("m Info"),
                "{text}"
            );
            assert!(text.contains("FIRST ARGUMENT"), "{text}");
            assert_eq!(m.scroll, 0);
        }
        m.page = Some(TaskTextPage {
            start: 0,
            end: 20,
            total: 90000,
            text: "{\"input\":{\"command\":".into(),
        });
        assert!(frame(&mut m, 80, 24).contains("Large input"));
        m.action(Action::Newer);
        assert!(matches!(
            m.reserve(81),
            Some(Request::TaskMonitor {
                request: TaskMonitorRequest::Read {
                    offset: Some(20),
                    content: ExecutionContent::Input,
                    ..
                },
                ..
            })
        ));
    }
}
