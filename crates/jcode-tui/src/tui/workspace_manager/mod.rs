//! Basic workspace management mode (`/workspace`, `/runtime`).
//!
//! Presentation and request correlation only. Each decision is a typed request
//! to its existing server owner; each effect is preceded by an explicit review
//! the human confirms. The final command-center experience belongs to C04.
mod actions;
mod describe;
pub(crate) mod form;
mod transport;
mod view;

#[cfg(test)]
mod tests;

pub(crate) use transport::Op;

use crate::protocol::ServerEvent;
use crate::workspace::runtime::{RuntimeStatus, ShutdownOperation, SupervisionStatus};
use crate::workspace::*;
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use form::{Form, FormEvent};
// The catalog's `Result` alias is domain-specific; UI code uses std's.
use ratatui::layout::Rect;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::result::Result;
use std::time::{Duration, Instant};

pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 12;
const PAGE: u32 = 100;
const POLL: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Organization,
    Sessions,
    Operations,
    Permissions,
    Closeout,
    Backup,
    Runtime,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Organization,
        Section::Sessions,
        Section::Operations,
        Section::Permissions,
        Section::Closeout,
        Section::Backup,
        Section::Runtime,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Section::Organization => "Organization",
            Section::Sessions => "Sessions",
            Section::Operations => "Operations",
            Section::Permissions => "Permissions",
            Section::Closeout => "Closeout",
            Section::Backup => "Backup",
            Section::Runtime => "Runtime",
        }
    }
    pub fn parse(name: &str) -> Option<Section> {
        Section::ALL
            .into_iter()
            .find(|section| section.label().eq_ignore_ascii_case(name))
    }
    fn index(self) -> usize {
        Section::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Work the app performs on the manager's behalf. These never carry
/// authority beyond the explicit human action that produced them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Attach this client to a session (navigation, never execution).
    Resume(String),
    OpenTerminal {
        session: String,
        cwd: Option<PathBuf>,
    },
    /// Create a new context through the existing Clear/Split/Transfer owners
    /// with the reviewed grant-carry choice.
    NewContext {
        kind: NewContextKind,
        choice: GrantCarryChoice,
    },
    /// Read this host's login-service registration.
    ServiceStatus,
    /// Read durable runtime intent while the runtime is unreachable.
    OfflineRuntime,
    /// Run the explicit `jcode runtime start` control.
    StartRuntime,
}

/// Capabilities negotiated with the attached server. `None` means unknown.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Capabilities {
    pub catalog: Option<bool>,
    pub permissions: bool,
    pub checkout: bool,
    pub closeout: bool,
    pub management: bool,
    pub managed_rollout: bool,
    pub primary: Option<bool>,
    pub location_enabled: bool,
    pub adoption: bool,
    pub context_scope: bool,
    pub session_inspection: bool,
    pub launch: Option<bool>,
    pub runtime: Option<bool>,
    pub supervision: bool,
}

/// A confirmable effect. The request identity is fixed when the review is
/// shown, so a retry after an uncertain reply replays the same request.
#[derive(Clone, Debug)]
pub(crate) struct Confirm {
    pub title: String,
    pub lines: Vec<(Tone, String)>,
    pub op: Op,
    /// The human must type this word before the effect is sent.
    pub typed: Option<&'static str>,
    pub input: String,
    pub yes: bool,
    pub scroll: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Normal,
    Muted,
    Accent,
    Good,
    Warn,
    Bad,
}

/// One manager-initiated effect whose outcome was not observed.
#[derive(Clone, Debug)]
pub(crate) struct Uncertain {
    pub label: String,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub key: String,
    pub text: String,
    pub tone: Tone,
}

#[derive(Default)]
pub(crate) struct Org {
    pub kind: Option<EntityKind>,
    pub visibility: Visibility,
    pub page: Option<Page>,
    pub cursor: Option<Cursor>,
    pub previous: Vec<Option<Cursor>>,
    pub selected: Option<EntityId>,
    pub sessions: Option<(EntityId, Vec<SessionIndex>)>,
}

#[derive(Default)]
pub(crate) struct Sessions {
    pub target: Option<EntityId>,
    pub rows: Vec<SessionIndex>,
    pub after: Option<String>,
    pub previous: Vec<Option<String>>,
    pub next: Option<String>,
    pub selected: Option<String>,
    pub views: HashMap<String, SessionLocationView>,
    pub scope: Option<Result<SessionWriteScope, Issue>>,
    /// Sessions a human asked to inspect that the index does not list.
    pub extra: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Ops {
    pub all: bool,
    pub page: Option<OperationPage>,
    pub cursor: Option<Cursor>,
    pub previous: Vec<Option<Cursor>>,
    pub selected: Option<OperationId>,
    pub clone: Option<CloneRecord>,
    pub output: Option<(RequestId, String)>,
    pub trust: Option<CloneTrustReview>,
    pub watch_clone: Option<RequestId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermMode {
    #[default]
    Proposals,
    Grants,
    Imported,
}

#[derive(Default)]
pub(crate) struct Perms {
    pub mode: PermMode,
    pub page: Option<PermissionPage>,
    pub imported: Option<(u64, Vec<ImportedGrantReference>, Option<Cursor>)>,
    pub cursor: Option<Cursor>,
    pub previous: Vec<Option<Cursor>>,
    pub selected: Option<String>,
    pub review: Option<GrantReview>,
    pub scopes: Option<Vec<ContextScopeStatus>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloseoutPane {
    #[default]
    Record,
    Inventory,
    Removal,
    History,
}

#[derive(Default)]
pub(crate) struct Closeouts {
    pub page: Option<OperationPage>,
    pub selected: Option<OperationId>,
    pub record: Option<CloseoutRecord>,
    pub review: Option<CloseoutReview>,
    pub recovery: Option<CloseoutRecoveryReview>,
    pub inventory: Option<CloseoutInventoryPage>,
    pub inventory_after: u64,
    pub entry: usize,
    pub removal: Option<CloseoutRemovalPage>,
    pub history: Option<CloseoutHistory>,
    pub pane: CloseoutPane,
    /// Runtime-owned action awaiting its domain result.
    pub action: Option<(RequestId, Option<CloseoutActionRecord>)>,
}

#[derive(Default)]
pub(crate) struct Backups {
    pub snapshots: Option<Vec<Snapshot>>,
    pub selected: Option<SnapshotId>,
}

/// Durable intent read while the runtime cannot answer.
#[derive(Clone, Debug)]
pub struct OfflineRuntime {
    pub status: Option<RuntimeStatus>,
    pub detail: String,
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub status: Option<RuntimeStatus>,
    pub supervision: Option<SupervisionStatus>,
    pub service: Option<Result<String, String>>,
    pub selected: Option<String>,
    pub operation: Option<ShutdownOperation>,
    pub offline: Option<OfflineRuntime>,
    pub start: Option<Result<String, String>>,
}

pub(crate) struct Pending {
    pub generation: u64,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Section(Section),
    Row(usize),
    Action(actions::Action),
    FormField(usize),
    FormSubmit,
    FormCancel,
    ConfirmYes,
    ConfirmNo,
}

pub struct WorkspaceManager {
    pub session: String,
    pub visible: bool,
    pub remote: bool,
    pub connected: bool,
    pub(crate) section: Section,
    pub(crate) caps: Capabilities,
    pub(crate) catalog: Option<Result<CatalogStatus, Issue>>,
    pub(crate) known: BTreeMap<String, Entity>,
    pub(crate) volumes: Option<Vec<WorkspaceVolume>>,
    pub(crate) org: Org,
    pub(crate) sessions: Sessions,
    pub(crate) ops: Ops,
    pub(crate) perms: Perms,
    pub(crate) closeouts: Closeouts,
    pub(crate) backups: Backups,
    pub(crate) runtime: Runtime,
    pub(crate) form: Option<(actions::FormKind, Form)>,
    pub(crate) confirm: Option<Confirm>,
    pub(crate) uncertain: Vec<Uncertain>,
    pub(crate) show_uncertain: bool,
    pub(crate) outcomes: VecDeque<(Tone, String)>,
    pub(crate) status: String,
    pub(crate) help: bool,
    pub(crate) detail: bool,
    pub(crate) detail_scroll: usize,
    pub(crate) launched: Option<PrimaryLaunchRecord>,
    pub(crate) generation: u64,
    pub(crate) pending: HashMap<u64, Pending>,
    pub(crate) queued: VecDeque<Op>,
    pub(crate) intents: VecDeque<Intent>,
    pub(crate) last_poll: Instant,
    pub(crate) hit: Vec<(Rect, Hit)>,
    pub(crate) list_area: Rect,
    pub(crate) list_offset: usize,
    pub(crate) dimensions: (u16, u16),
}

impl WorkspaceManager {
    pub fn new(session: String, remote: bool, section: Section) -> Self {
        let mut this = Self {
            session,
            visible: true,
            remote,
            connected: remote,
            section,
            caps: Capabilities::default(),
            catalog: None,
            known: BTreeMap::new(),
            volumes: None,
            org: Org::default(),
            sessions: Sessions::default(),
            ops: Ops::default(),
            perms: Perms::default(),
            closeouts: Closeouts::default(),
            backups: Backups::default(),
            runtime: Runtime::default(),
            form: None,
            confirm: None,
            uncertain: Vec::new(),
            show_uncertain: false,
            outcomes: VecDeque::new(),
            status: String::new(),
            help: false,
            detail: false,
            detail_scroll: 0,
            launched: None,
            generation: 0,
            pending: HashMap::new(),
            queued: VecDeque::new(),
            intents: VecDeque::new(),
            last_poll: Instant::now(),
            hit: Vec::new(),
            list_area: Rect::default(),
            list_offset: 0,
            dimensions: (80, 24),
        };
        if remote {
            this.probe();
        } else {
            this.status = "Workspace management runs against the shared runtime. Start Jcode attached to the server (the default) to use it.".into();
        }
        this
    }

    /// Show again, keeping drafts, selection and outcomes.
    pub fn reopen(&mut self, section: Option<Section>) {
        self.visible = true;
        if let Some(section) = section {
            self.switch(section);
        }
        if self.remote && self.connected {
            self.refresh();
        }
    }

    fn probe(&mut self) {
        for op in [
            Op::probe(transport::Probe::Workspace),
            Op::probe(transport::Probe::Primary),
            Op::probe(transport::Probe::Launch),
            Op::probe(transport::Probe::Runtime),
        ] {
            self.queue(op);
        }
    }

    /// A new transport. Requests in flight belong to the old connection: reads
    /// are re-issued, effects become uncertain until a human inspects them.
    pub fn reconnect(&mut self, session: &str) {
        self.generation += 1;
        let pending = std::mem::take(&mut self.pending);
        for pending in pending.into_values() {
            self.mark_uncertain(pending.op);
        }
        for op in std::mem::take(&mut self.queued) {
            if op.is_effect() {
                self.mark_uncertain(op);
            }
        }
        self.session = session.to_string();
        self.connected = true;
        self.runtime.offline = None;
        self.caps = Capabilities::default();
        self.note(
            Tone::Muted,
            "Reconnected. Refreshing capabilities and state.",
        );
        self.probe();
    }

    /// The transport is gone. Nothing new is sent until `reconnect`.
    pub fn disconnected(&mut self) {
        if !self.connected {
            return;
        }
        self.connected = false;
        self.generation += 1;
        let pending = std::mem::take(&mut self.pending);
        for pending in pending.into_values() {
            self.mark_uncertain(pending.op);
        }
        for op in std::mem::take(&mut self.queued) {
            if op.is_effect() {
                self.mark_uncertain(op);
            }
        }
        self.status = "Runtime unreachable. Showing durable state; reconnecting.".into();
        self.intents.push_back(Intent::OfflineRuntime);
    }

    fn mark_uncertain(&mut self, op: Op) {
        if op.is_effect() {
            let label = op.label();
            self.note(
                Tone::Warn,
                format!("Outcome unknown: {label}. Inspect before retrying (U)."),
            );
            self.uncertain.push(Uncertain { label, op });
        }
    }

    pub(crate) fn note(&mut self, tone: Tone, text: impl Into<String>) {
        let text = text.into();
        self.status = text.clone();
        self.outcomes.push_front((tone, text));
        self.outcomes.truncate(50);
    }

    pub(crate) fn queue(&mut self, op: Op) {
        if !op.is_effect()
            && (self.queued.iter().any(|queued| *queued == op)
                || self.pending.values().any(|pending| pending.op == op))
        {
            return;
        }
        self.queued.push_back(op);
    }

    pub fn take_intent(&mut self) -> Option<Intent> {
        self.intents.pop_front()
    }

    /// Reserve a transport identity and record the exact expectation before
    /// the request is written; replies can arrive as soon as it is.
    pub fn reserve(&mut self, id: u64) -> Option<crate::protocol::Request> {
        if !self.connected {
            return None;
        }
        let op = self.queued.pop_front()?;
        let request = op.request(id);
        self.pending.insert(
            id,
            Pending {
                generation: self.generation,
                op,
            },
        );
        Some(request)
    }

    pub fn accepts_id(&self, id: u64) -> bool {
        self.pending.contains_key(&id)
    }

    /// The write failed. Reads are reported; effects become uncertain.
    pub fn transport_failed(&mut self, id: u64, error: &str) {
        if let Some(pending) = self.pending.remove(&id) {
            if pending.op.is_effect() {
                self.mark_uncertain(pending.op);
            } else {
                self.note(Tone::Bad, format!("Request not sent: {error}"));
            }
        }
    }

    pub fn accept(&mut self, id: u64, event: ServerEvent) -> bool {
        if !self.pending.contains_key(&id) {
            return false;
        }
        // Transport acknowledgement only; the typed reply follows on this id.
        if matches!(event, ServerEvent::Ack { .. }) {
            return true;
        }
        let Some(pending) = self.pending.remove(&id) else {
            return false;
        };
        if pending.generation != self.generation {
            return true;
        }
        transport::reduce(self, pending.op, event);
        true
    }

    pub fn tick(&mut self) {
        if !self.visible || !self.connected || self.last_poll.elapsed() < POLL {
            return;
        }
        self.last_poll = Instant::now();
        if let Some(request) = self.ops.watch_clone {
            self.queue(Op::read(
                transport::View::Clone,
                WorkspaceRequest::InspectClone { request },
            ));
        }
        if let Some((request, None)) = &self.closeouts.action {
            self.queue(Op::closeout(
                transport::View::CloseoutAction,
                CloseoutRequest::InspectAction { request: *request },
            ));
        }
        if let Some(operation) = self
            .runtime
            .operation
            .as_ref()
            .filter(|operation| !operation.phase.terminal())
            .map(|operation| operation.id)
        {
            self.queue(Op::runtime(
                transport::View::RuntimeOperation,
                crate::workspace::runtime::RuntimeRequest::Inspect { operation },
            ));
        }
    }

    pub(crate) fn switch(&mut self, section: Section) {
        if self.section != section {
            self.section = section;
            self.detail = false;
            self.detail_scroll = 0;
            self.list_offset = 0;
        }
        self.load_section();
    }

    /// Re-read everything the current section shows.
    pub fn refresh(&mut self) {
        self.queue(Op::read(
            transport::View::Status,
            WorkspaceRequest::Status {},
        ));
        self.load_section();
    }

    pub(crate) fn catalog_ready(&self) -> bool {
        matches!(self.catalog, Some(Ok(_)))
    }

    pub(crate) fn load_section(&mut self) {
        if !self.remote || !self.connected {
            return;
        }
        let ready = self.catalog_ready();
        match self.section {
            Section::Organization if ready => self.load_org(),
            Section::Sessions => self.load_sessions(),
            Section::Operations if ready => self.load_ops(),
            Section::Permissions if ready => self.load_perms(),
            Section::Closeout if ready => self.load_closeouts(),
            Section::Backup if ready => self.queue(Op::read(
                transport::View::Snapshots,
                WorkspaceRequest::Snapshots {},
            )),
            Section::Runtime => self.load_runtime(),
            _ => {}
        }
    }

    pub(crate) fn load_org(&mut self) {
        self.queue(Op::read(
            transport::View::OrgPage,
            WorkspaceRequest::List {
                query: Query {
                    kind: self.org.kind,
                    visibility: self.org.visibility,
                    ..Default::default()
                },
                after: self.org.cursor.clone(),
                limit: PAGE,
            },
        ));
        if let Some(target) = self.org.selected {
            self.queue(Op::read(
                transport::View::Entity,
                WorkspaceRequest::Inspect { target },
            ));
            self.queue(Op::read(
                transport::View::EntitySessions(target),
                WorkspaceRequest::Sessions {
                    target: Some(target),
                    after: None,
                    limit: 50,
                },
            ));
        }
    }

    pub(crate) fn load_known(&mut self) {
        for kind in [
            EntityKind::Project,
            EntityKind::Repository,
            EntityKind::WorkArea,
            EntityKind::Location,
        ] {
            self.queue(Op::read(
                transport::View::Known,
                WorkspaceRequest::List {
                    query: Query {
                        kind: Some(kind),
                        ..Default::default()
                    },
                    after: None,
                    limit: 200,
                },
            ));
        }
    }

    pub(crate) fn load_sessions(&mut self) {
        if self.catalog_ready() {
            self.queue(Op::read(
                transport::View::Sessions,
                WorkspaceRequest::Sessions {
                    target: self.sessions.target,
                    after: self.sessions.after.clone(),
                    limit: PAGE,
                },
            ));
        }
        let selected = self
            .sessions
            .selected
            .clone()
            .unwrap_or_else(|| self.session.clone());
        self.inspect_session(selected);
    }

    pub(crate) fn inspect_session(&mut self, session: String) {
        if self.caps.session_inspection {
            self.queue(Op::location(
                transport::View::SessionView,
                PrimaryLocationCommand::InspectSession { session },
            ));
        }
    }

    pub(crate) fn load_ops(&mut self) {
        self.queue(Op::read(
            transport::View::Operations,
            WorkspaceRequest::Operations {
                query: OperationQuery {
                    unfinished_only: !self.ops.all,
                    ..Default::default()
                },
                after: self.ops.cursor.clone(),
                limit: PAGE,
            },
        ));
    }

    pub(crate) fn load_perms(&mut self) {
        let request = match self.perms.mode {
            PermMode::Proposals => PermissionRequest::List {
                query: PermissionQuery::Proposals {
                    session: None,
                    state: None,
                },
                after: self.perms.cursor.clone(),
                limit: PAGE,
            },
            PermMode::Grants => PermissionRequest::List {
                query: PermissionQuery::Grants { audience: None },
                after: self.perms.cursor.clone(),
                limit: PAGE,
            },
            PermMode::Imported => PermissionRequest::ImportedGrants {
                after: self.perms.cursor.clone(),
                limit: PAGE,
            },
        };
        self.queue(Op::read(
            transport::View::Permissions,
            WorkspaceRequest::Permissions { request },
        ));
    }

    pub(crate) fn load_closeouts(&mut self) {
        self.queue(Op::read(
            transport::View::Closeouts,
            WorkspaceRequest::Operations {
                query: OperationQuery {
                    kinds: vec![OperationKind::Closeout],
                    ..Default::default()
                },
                after: None,
                limit: PAGE,
            },
        ));
        if let Some(operation) = self.closeouts.selected {
            self.load_closeout(operation);
        }
    }

    pub(crate) fn load_closeout(&mut self, operation: OperationId) {
        self.queue(Op::closeout(
            transport::View::CloseoutRecord,
            CloseoutRequest::Inspect { operation },
        ));
        self.queue(Op::closeout(
            transport::View::CloseoutReview,
            CloseoutRequest::Review { operation },
        ));
        self.queue(Op::closeout(
            transport::View::CloseoutRecovery,
            CloseoutRequest::Recovery { operation },
        ));
    }

    pub(crate) fn load_runtime(&mut self) {
        if self.caps.runtime == Some(true) {
            self.queue(Op::runtime(
                transport::View::RuntimeStatus,
                crate::workspace::runtime::RuntimeRequest::Status {},
            ));
            if self.caps.supervision {
                self.queue(Op::runtime(
                    transport::View::Supervision,
                    crate::workspace::runtime::RuntimeRequest::Supervision {},
                ));
            }
        }
        self.intents.push_back(Intent::ServiceStatus);
    }

    pub fn accept_service(&mut self, status: Result<String, String>) {
        self.runtime.service = Some(status);
    }

    pub fn accept_offline(&mut self, offline: OfflineRuntime) {
        self.runtime.offline = Some(offline);
    }

    pub fn accept_start(&mut self, result: Result<String, String>) {
        match &result {
            Ok(_) => self.note(Tone::Good, "Runtime start requested. Reconnecting."),
            Err(error) => self.note(Tone::Bad, format!("Start failed: {error}")),
        }
        self.runtime.start = Some(result);
    }

    pub fn paste(&mut self, text: &str) {
        if let Some((_, form)) = &mut self.form {
            form.paste(text);
        } else if let Some(confirm) = &mut self.confirm
            && confirm.typed.is_some()
        {
            confirm.input.push_str(text.trim());
        }
    }

    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.visible {
            return false;
        }
        if self.dimensions.0 < MIN_WIDTH || self.dimensions.1 < MIN_HEIGHT {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q'))
                || (modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c'))
            {
                self.visible = false;
            }
            return true;
        }
        if let Some((kind, form)) = &mut self.form {
            match form.key(code, modifiers) {
                FormEvent::None => {}
                FormEvent::Cancel => {
                    self.form = None;
                    self.status = "Draft discarded; nothing was sent.".into();
                }
                FormEvent::Submit => {
                    let kind = kind.clone();
                    let form = form.clone();
                    match actions::build(self, &kind, &form) {
                        Ok(()) => self.form = None,
                        Err(error) => {
                            if let Some((_, form)) = &mut self.form {
                                form.error = Some(error);
                            }
                        }
                    }
                }
            }
            return true;
        }
        if self.confirm.is_some() {
            self.confirm_key(code, modifiers);
            return true;
        }
        if self.show_uncertain {
            self.uncertain_key(code);
            return true;
        }
        if self.help {
            if matches!(code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                self.help = false;
            }
            return true;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.close();
            return true;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.detail {
                    self.detail = false;
                    self.detail_scroll = 0;
                } else {
                    self.close();
                }
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(c @ '1'..='7') => {
                self.switch(Section::ALL[(c as u8 - b'1') as usize]);
            }
            KeyCode::Tab => {
                let next = (self.section.index() + 1) % Section::ALL.len();
                self.switch(Section::ALL[next]);
            }
            KeyCode::BackTab => {
                let next = (self.section.index() + Section::ALL.len() - 1) % Section::ALL.len();
                self.switch(Section::ALL[next]);
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.select_index(0),
            KeyCode::End => self.select_index(usize::MAX),
            KeyCode::Enter => {
                if self.dimensions.0 < view::SPLIT_WIDTH {
                    self.detail = !self.detail;
                    self.detail_scroll = 0;
                } else if let Some(action) = actions::primary(self) {
                    actions::run(self, action);
                }
            }
            KeyCode::Char('U') if !self.uncertain.is_empty() => self.show_uncertain = true,
            KeyCode::Char(c) => {
                if let Some(action) = actions::available(self)
                    .into_iter()
                    .find(|spec| spec.key == c)
                    .map(|spec| spec.action)
                {
                    actions::run(self, action);
                }
            }
            _ => {}
        }
        true
    }

    fn close(&mut self) {
        self.visible = false;
        self.help = false;
        self.show_uncertain = false;
    }

    fn confirm_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(confirm) = &mut self.confirm else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.confirm = None;
                self.status = "Review closed; nothing was sent.".into();
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.confirm = None;
                self.status = "Review closed; nothing was sent.".into();
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                confirm.yes = !confirm.yes;
            }
            KeyCode::Up => confirm.scroll = confirm.scroll.saturating_sub(1),
            KeyCode::Down => confirm.scroll = confirm.scroll.saturating_add(1),
            KeyCode::PageUp => confirm.scroll = confirm.scroll.saturating_sub(10),
            KeyCode::PageDown => confirm.scroll = confirm.scroll.saturating_add(10),
            KeyCode::Backspace if confirm.typed.is_some() => {
                confirm.input.pop();
            }
            KeyCode::Char(c) if confirm.typed.is_some() => confirm.input.push(c),
            KeyCode::Char('y') => {
                confirm.yes = true;
                self.confirm_submit();
            }
            KeyCode::Char('n') => {
                self.confirm = None;
                self.status = "Review declined; nothing was sent.".into();
            }
            KeyCode::Enter => {
                if confirm.typed.is_some() {
                    confirm.yes = true;
                }
                self.confirm_submit();
            }
            _ => {}
        }
    }

    pub(crate) fn confirm_submit(&mut self) {
        let Some(confirm) = &self.confirm else {
            return;
        };
        if !confirm.yes {
            self.confirm = None;
            self.status = "Review declined; nothing was sent.".into();
            return;
        }
        if let Some(word) = confirm.typed
            && confirm.input.trim() != word
        {
            self.status = format!("Type `{word}` exactly to confirm this escalation.");
            return;
        }
        let confirm = self.confirm.take().expect("checked above");
        self.status = format!("Sent: {}", confirm.op.label());
        self.queue(confirm.op);
    }

    fn uncertain_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.show_uncertain = false,
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if !self.uncertain.is_empty() {
                    let item = self.uncertain.remove(0);
                    self.note(
                        Tone::Accent,
                        format!("Retrying the same request: {}", item.label),
                    );
                    self.queue(item.op);
                }
                if self.uncertain.is_empty() {
                    self.show_uncertain = false;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if !self.uncertain.is_empty() {
                    let item = self.uncertain.remove(0);
                    self.note(
                        Tone::Muted,
                        format!("Dismissed uncertainty: {}", item.label),
                    );
                }
                if self.uncertain.is_empty() {
                    self.show_uncertain = false;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        describe::rows(self)
    }

    pub(crate) fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        let key = self.selected_key()?;
        rows.iter().position(|row| row.key == key)
    }

    pub(crate) fn selected_key(&self) -> Option<String> {
        match self.section {
            Section::Organization => self.org.selected.map(|id| id.to_string()),
            Section::Sessions => Some(
                self.sessions
                    .selected
                    .clone()
                    .unwrap_or_else(|| self.session.clone()),
            ),
            Section::Operations => self.ops.selected.map(|id| id.to_string()),
            Section::Permissions => self.perms.selected.clone(),
            Section::Closeout => self.closeouts.selected.map(|id| id.to_string()),
            Section::Backup => self.backups.selected.map(|id| id.to_string()),
            Section::Runtime => self.runtime.selected.clone(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.detail
            || (self.section == Section::Closeout && self.closeouts.pane == CloseoutPane::Inventory)
        {
            if self.section == Section::Closeout && self.closeouts.pane == CloseoutPane::Inventory {
                let count = self
                    .closeouts
                    .inventory
                    .as_ref()
                    .map_or(0, |page| page.entries.len());
                self.closeouts.entry = self
                    .closeouts
                    .entry
                    .saturating_add_signed(delta)
                    .min(count.saturating_sub(1));
            } else {
                self.detail_scroll = self.detail_scroll.saturating_add_signed(delta);
            }
            return;
        }
        let rows = self.rows();
        let current = self.selected_index(&rows).unwrap_or(0);
        self.select_index(current.saturating_add_signed(delta));
    }

    pub(crate) fn select_index(&mut self, index: usize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let key = rows[index.min(rows.len() - 1)].key.clone();
        self.select_key(&key);
    }

    pub(crate) fn select_key(&mut self, key: &str) {
        if self.selected_key().as_deref() == Some(key) {
            return;
        }
        self.detail_scroll = 0;
        match self.section {
            Section::Organization => {
                let Some(entity) = self.known.get(key).map(Entity::id) else {
                    return;
                };
                self.org.selected = Some(entity);
                self.queue(Op::read(
                    transport::View::Entity,
                    WorkspaceRequest::Inspect { target: entity },
                ));
                self.queue(Op::read(
                    transport::View::EntitySessions(entity),
                    WorkspaceRequest::Sessions {
                        target: Some(entity),
                        after: None,
                        limit: 50,
                    },
                ));
            }
            Section::Sessions => {
                self.sessions.selected = Some(key.to_string());
                self.sessions.scope = None;
                self.inspect_session(key.to_string());
            }
            Section::Operations => {
                if let Ok(id) = key.parse() {
                    self.ops.selected = Some(id);
                    self.ops.clone = None;
                    self.ops.output = None;
                    self.ops.trust = None;
                }
            }
            Section::Permissions => {
                self.perms.selected = Some(key.to_string());
                self.perms.review = None;
            }
            Section::Closeout => {
                if let Ok(id) = key.parse() {
                    self.closeouts.selected = Some(id);
                    self.closeouts.record = None;
                    self.closeouts.review = None;
                    self.closeouts.recovery = None;
                    self.closeouts.inventory = None;
                    self.closeouts.removal = None;
                    self.closeouts.history = None;
                    self.closeouts.pane = CloseoutPane::Record;
                    self.load_closeout(id);
                }
            }
            Section::Backup => {
                if let Ok(id) = key.parse() {
                    self.backups.selected = Some(id);
                }
            }
            Section::Runtime => self.runtime.selected = Some(key.to_string()),
        }
    }

    pub fn mouse(&mut self, event: MouseEvent) {
        if !self.visible {
            return;
        }
        let position = ratatui::layout::Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(_) => {
                let Some((_, hit)) = self
                    .hit
                    .iter()
                    .find(|(rect, _)| rect.contains(position))
                    .cloned()
                else {
                    return;
                };
                match hit {
                    Hit::Section(section) => {
                        if self.form.is_none() && self.confirm.is_none() {
                            self.help = false;
                            self.switch(section);
                        }
                    }
                    Hit::Row(index) => self.select_index(index),
                    Hit::Action(action) => {
                        self.help = false;
                        actions::run(self, action);
                    }
                    Hit::FormField(index) => {
                        if let Some((_, form)) = &mut self.form {
                            form.focus = index;
                        }
                    }
                    Hit::FormSubmit => {
                        self.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
                    }
                    Hit::FormCancel => {
                        self.form = None;
                        self.status = "Draft discarded; nothing was sent.".into();
                    }
                    Hit::ConfirmYes => {
                        if let Some(confirm) = &mut self.confirm {
                            confirm.yes = true;
                        }
                        self.confirm_submit();
                    }
                    Hit::ConfirmNo => {
                        self.confirm = None;
                        self.status = "Review declined; nothing was sent.".into();
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(confirm) = &mut self.confirm {
                    confirm.scroll += 1;
                } else if self.form.is_none() {
                    self.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(confirm) = &mut self.confirm {
                    confirm.scroll = confirm.scroll.saturating_sub(1);
                } else if self.form.is_none() {
                    self.move_selection(-1);
                }
            }
            _ => {}
        }
    }

    pub fn debug(&self) -> serde_json::Value {
        let rows = self.rows();
        serde_json::json!({
            "visible": self.visible,
            "remote": self.remote,
            "connected": self.connected,
            "section": self.section,
            "capabilities": self.caps,
            "catalog": self.catalog.as_ref().map(|status| match status {
                Ok(status) => serde_json::json!({"revision": status.revision, "managed_rollout": status.managed_rollout}),
                Err(issue) => serde_json::json!({"issue": issue.code, "detail": issue.detail}),
            }),
            "rows": rows.iter().map(|row| serde_json::json!({"key": row.key, "text": row.text})).collect::<Vec<_>>(),
            "selected": self.selected_key(),
            "detail": self.detail,
            "form": self.form.as_ref().map(|(kind, form)| serde_json::json!({
                "kind": format!("{kind:?}"),
                "title": form.title,
                "focus": form.focus,
                "error": form.error,
                "values": form.fields.iter().map(|f| (f.key, f.value.clone())).collect::<BTreeMap<_, _>>(),
            })),
            "confirm": self.confirm.as_ref().map(|confirm| serde_json::json!({
                "title": confirm.title,
                "op": confirm.op.label(),
                "typed": confirm.typed,
                "yes": confirm.yes,
            })),
            "uncertain": self.uncertain.iter().map(|u| u.label.clone()).collect::<Vec<_>>(),
            "pending": self.pending.len(),
            "queued": self.queued.len(),
            "status": self.status,
            "outcomes": self.outcomes.iter().take(10).map(|(_, text)| text.clone()).collect::<Vec<_>>(),
            "actions": actions::available(self).iter().map(|spec| format!("{} {}", spec.key, spec.label)).collect::<Vec<_>>(),
            "runtime": {
                "desired_stopped": self.runtime.status.as_ref().map(|s| s.desired_stopped),
                "operation": self.runtime.operation.as_ref().map(|o| serde_json::json!({"id": o.id, "phase": o.phase, "revision": o.revision})),
                "recoveries": self.runtime.supervision.as_ref().map(|s| s.recoveries.len()),
                "offline": self.runtime.offline.as_ref().map(|o| o.detail.clone()),
            },
            "launched": self.launched.as_ref().map(|r| r.session.clone()),
            "dimensions": self.dimensions,
        })
    }
}
