//! App wiring for `/workspace` and `/runtime`. The manager owns presentation
//! and correlation; this module only moves its typed requests over the attached
//! connection and performs the few client-side intents it asks for.
use super::{App, DisplayMessage};
use crate::protocol::ServerEvent;
use crate::tui::backend::RemoteConnection;
use crate::tui::placement_review::{self, PlacementReview};
use crate::tui::workspace_manager::{Intent, OfflineRuntime, Section, WorkspaceManager};
use crossterm::event::{KeyCode, KeyModifiers};
use std::cell::RefCell;
use std::collections::HashSet;
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct WorkspaceUi {
    pub manager: Option<RefCell<WorkspaceManager>>,
    owned: HashSet<u64>,
    service: Option<oneshot::Receiver<Result<String, String>>>,
    start: Option<oneshot::Receiver<Result<String, String>>>,
    /// First-send placement review for an unplaced session.
    pub placement: Option<PlacementReview>,
    /// The placement completed and the composer's message should be sent.
    pub resend_after_placement: bool,
}

/// Parse `/workspace [section]` and `/runtime`. Other `/runtime` subcommands
/// stay with the CLI (`jcode runtime …`), which works without this client.
fn parse_command(command: &str) -> Option<Option<Section>> {
    let mut words = command.split_whitespace();
    match words.next()? {
        "/workspace" => match (words.next(), words.next()) {
            (None, _) => Some(None),
            (Some(name), None) => Section::parse(name).map(Some),
            _ => None,
        },
        "/runtime" => match words.next() {
            None | Some("status" | "start" | "stop" | "restart" | "recover") => {
                Some(Some(Section::Runtime))
            }
            _ => None,
        },
        _ => None,
    }
}

impl App {
    fn workspace_session(&self) -> String {
        self.remote_session_id
            .clone()
            .unwrap_or_else(|| self.session.id.clone())
    }

    pub(super) fn workspace_manager_visible(&self) -> bool {
        self.workspace_ui
            .manager
            .as_ref()
            .is_some_and(|manager| manager.borrow().visible)
    }

    pub(super) fn handle_workspace_command(&mut self, command: &str) -> bool {
        let trimmed = command.trim();
        if trimmed == "/place" {
            if self.is_remote {
                self.open_placement_review(false);
            } else {
                self.push_display_message(DisplayMessage::system(
                    "Placement is managed by the shared runtime; this local session has none."
                        .to_string(),
                ));
            }
            return true;
        }
        if trimmed != "/workspace"
            && !trimmed.starts_with("/workspace ")
            && trimmed != "/runtime"
            && !trimmed.starts_with("/runtime ")
        {
            return false;
        }
        let Some(section) = parse_command(trimmed) else {
            self.push_display_message(DisplayMessage::system(
                "/workspace [organization|sessions|operations|permissions|closeout|backup|runtime]\n  Open workspace management.\n\n/runtime\n  Open its runtime section (status, stop, restart, interrupted turns, start).\n\nNiri-style session rows moved to /niri. Scripted runtime control stays `jcode runtime …`."
                    .to_string(),
            ));
            return true;
        };
        let remote = self.is_remote;
        let session = self.workspace_session();
        match &self.workspace_ui.manager {
            Some(manager) => manager.borrow_mut().reopen(section),
            None => {
                self.workspace_ui.manager = Some(RefCell::new(WorkspaceManager::new(
                    session,
                    remote,
                    section.unwrap_or(Section::Organization),
                )));
            }
        }
        self.force_full_redraw = true;
        true
    }

    pub(super) fn handle_workspace_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Terminals with enhanced keyboard reporting send Shift+letter as the
        // lowercase key plus SHIFT; actions are bound to the typed character.
        let code = match code {
            KeyCode::Char(c) => {
                KeyCode::Char(super::input::shifted_printable_fallback(c, modifiers))
            }
            other => other,
        };
        let handled = self
            .workspace_ui
            .manager
            .as_ref()
            .is_some_and(|manager| manager.borrow_mut().key(code, modifiers));
        if handled {
            self.force_full_redraw = true;
        }
        handled
    }

    pub(super) fn handle_workspace_paste(&mut self, text: &str) -> bool {
        if !self.workspace_manager_visible() {
            return false;
        }
        if let Some(manager) = &self.workspace_ui.manager {
            manager.borrow_mut().paste(text);
        }
        true
    }

    /// Open the placement review for the attached session. `resend` sends the
    /// composer's message once placement completes.
    pub(super) fn open_placement_review(&mut self, resend: bool) {
        let Some(session) = self.remote_session_id.clone() else {
            return;
        };
        match &mut self.workspace_ui.placement {
            Some(review) if review.session == session => review.resend |= resend,
            _ => self.workspace_ui.placement = Some(PlacementReview::open(session, resend)),
        }
        self.force_full_redraw = true;
    }

    pub(super) fn placement_review_visible(&self) -> bool {
        self.workspace_ui.placement.is_some()
    }

    pub(super) fn handle_placement_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(review) = &mut self.workspace_ui.placement else {
            return false;
        };
        let outcome = review.key(code, modifiers);
        self.apply_placement_outcome(outcome);
        self.force_full_redraw = true;
        true
    }

    fn apply_placement_outcome(&mut self, outcome: placement_review::Outcome) {
        use placement_review::Outcome;
        match outcome {
            Outcome::None => {}
            Outcome::Close => {
                self.workspace_ui.placement = None;
                self.set_status_notice("Session not placed; your message is still in the composer");
            }
            Outcome::OpenWorkspace => {
                self.workspace_ui.placement = None;
                self.handle_workspace_command("/workspace sessions");
            }
            Outcome::Placed { summary } => {
                let resend = self
                    .workspace_ui
                    .placement
                    .take()
                    .is_some_and(|review| review.resend);
                self.set_status_notice(summary);
                self.workspace_ui.resend_after_placement = resend && !self.input.trim().is_empty();
            }
        }
    }

    /// Accept only replies to the placement review's own request.
    pub(super) fn reduce_placement_event(
        &mut self,
        event: ServerEvent,
    ) -> Result<bool, Box<ServerEvent>> {
        let id = match &event {
            ServerEvent::PrimaryLocationResponse { id, .. } | ServerEvent::Error { id, .. } => *id,
            _ => return Err(Box::new(event)),
        };
        let Some(review) = &mut self.workspace_ui.placement else {
            return Err(Box::new(event));
        };
        if !review.owns(id) {
            return Err(Box::new(event));
        }
        let outcome = review.accept(event);
        self.apply_placement_outcome(outcome);
        self.force_full_redraw = true;
        Ok(true)
    }

    /// Send the review's queued request, if any.
    pub(super) async fn dispatch_placement_review(&mut self, remote: &mut RemoteConnection) {
        let Some(review) = &mut self.workspace_ui.placement else {
            return;
        };
        let id = remote.reserve_context_request_id();
        if let Some(request) = review.reserve(id)
            && let Err(error) = remote.send_reserved_workspace_request(request).await
        {
            review.transport_failed(&format!("{error:#}"));
            self.force_full_redraw = true;
        }
    }

    /// Send the held message after a completed placement through the
    /// ordinary Enter path. Called from the tick loop only, outside key
    /// handling.
    pub(super) async fn resend_after_placement(&mut self, remote: &mut RemoteConnection) -> bool {
        if !std::mem::take(&mut self.workspace_ui.resend_after_placement) {
            return false;
        }
        if let Err(error) =
            super::remote::handle_remote_key(self, KeyCode::Enter, KeyModifiers::empty(), remote)
                .await
        {
            self.push_display_message(DisplayMessage::error(format!(
                "Could not send the held message after placement: {error:#}"
            )));
        }
        true
    }

    pub(super) fn placement_review_debug(&self) -> serde_json::Value {
        let review = self.workspace_ui.placement.as_ref();
        serde_json::json!({
            "visible": review.is_some(),
            "session": review.map(|review| &review.session),
            "resend": review.map(|review| review.resend),
            "stage": review.map(|review| format!("{:?}", review.stage)),
            "selected": review.map(|review| review.selected),
            "candidates": review.and_then(|review| review.proposal.as_ref()).map(|proposal| {
                proposal.candidates.iter().map(|candidate| serde_json::json!({
                    "placement": candidate.placement,
                    "root": candidate.root,
                    "name": candidate.name,
                    "broad": candidate.broad,
                })).collect::<Vec<_>>()
            }),
            "default": review.and_then(|review| review.proposal.as_ref()).map(|proposal| proposal.default),
            "composer": self.input,
            "resend_pending": self.workspace_ui.resend_after_placement,
        })
    }

    pub(super) fn draw_placement_overlay(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
    ) -> bool {
        let Some(review) = &self.workspace_ui.placement else {
            return false;
        };
        review.render(frame, area);
        true
    }

    /// Accept only replies to requests this manager reserved.
    pub(super) fn reduce_workspace_event(
        &mut self,
        event: ServerEvent,
    ) -> Result<bool, Box<ServerEvent>> {
        let id = match &event {
            ServerEvent::WorkspaceCapabilities { id, .. }
            | ServerEvent::WorkspaceResponse { id, .. }
            | ServerEvent::PrimaryControlCapabilities { id, .. }
            | ServerEvent::PrimaryLocationResponse { id, .. }
            | ServerEvent::PrimaryLaunchCapabilities { id, .. }
            | ServerEvent::PrimaryLaunchResponse { id, .. }
            | ServerEvent::RuntimeCapabilities { id, .. }
            | ServerEvent::RuntimeResponse { id, .. }
            | ServerEvent::Error { id, .. }
            | ServerEvent::Ack { id } => *id,
            _ => return Err(Box::new(event)),
        };
        if !self.workspace_ui.owned.contains(&id) {
            return Err(Box::new(event));
        }
        if !matches!(event, ServerEvent::Ack { .. }) {
            self.workspace_ui.owned.remove(&id);
        }
        let Some(manager) = &self.workspace_ui.manager else {
            return Ok(false);
        };
        let accepted = manager.borrow_mut().accept(id, event);
        Ok(accepted)
    }

    pub(super) fn reconnect_workspace_manager(&mut self, session: &str) {
        self.workspace_ui.owned.clear();
        // An in-flight review reply cannot arrive on the new connection.
        if let Some(review) = &mut self.workspace_ui.placement {
            if review.session == session {
                review.transport_failed("connection was replaced");
            } else {
                self.workspace_ui.placement = None;
            }
        }
        if let Some(manager) = &self.workspace_ui.manager {
            manager.borrow_mut().reconnect(session);
        }
    }

    /// The attached session changed on the same connection (Clear, resume).
    pub(super) fn retarget_workspace_manager(&mut self, session: &str) {
        if self
            .workspace_ui
            .placement
            .as_ref()
            .is_some_and(|review| review.session != session)
        {
            self.workspace_ui.placement = None;
        }
        if let Some(manager) = &self.workspace_ui.manager {
            let mut manager = manager.borrow_mut();
            if manager.session != session {
                manager.session = session.to_string();
                manager.load_section();
            }
        }
    }

    pub(super) fn workspace_disconnected(&mut self) {
        self.workspace_ui.owned.clear();
        if let Some(manager) = &self.workspace_ui.manager {
            manager.borrow_mut().disconnected();
        }
        self.poll_workspace_local();
    }

    /// Keys while the runtime is unreachable: the manager keeps its Runtime
    /// controls (Start) and Esc; nothing is sent until reconnect.
    pub(super) fn handle_workspace_key_disconnected(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        if !self.workspace_manager_visible() {
            return false;
        }
        self.handle_workspace_key(code, modifiers);
        self.poll_workspace_local();
        true
    }

    /// Typed `/runtime` or `/workspace` while disconnected opens the manager
    /// instead of queueing the text as a message for the next connection.
    pub(super) fn open_workspace_while_disconnected(&mut self) -> bool {
        let trimmed = self.input.trim().to_string();
        if parse_command(&trimmed).is_none() {
            return false;
        }
        self.input.clear();
        self.cursor_pos = 0;
        let session = self.workspace_session();
        match &self.workspace_ui.manager {
            Some(manager) => {
                let mut manager = manager.borrow_mut();
                manager.reopen(Some(Section::Runtime));
                manager.disconnected();
            }
            None => {
                let mut manager = WorkspaceManager::new(session, self.is_remote, Section::Runtime);
                manager.disconnected();
                self.workspace_ui.manager = Some(RefCell::new(manager));
            }
        }
        self.poll_workspace_local();
        self.force_full_redraw = true;
        true
    }

    /// Client-side intents that need no attached connection, plus their
    /// completions. Safe to call from both connected and reconnect loops.
    pub(super) fn poll_workspace_local(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &mut self.workspace_ui.service {
            match receiver.try_recv() {
                Ok(result) => {
                    self.workspace_ui.service = None;
                    if let Some(manager) = &self.workspace_ui.manager {
                        manager.borrow_mut().accept_service(result);
                    }
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Closed) => self.workspace_ui.service = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if let Some(receiver) = &mut self.workspace_ui.start {
            match receiver.try_recv() {
                Ok(result) => {
                    self.workspace_ui.start = None;
                    if let Some(manager) = &self.workspace_ui.manager {
                        manager.borrow_mut().accept_start(result);
                    }
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Closed) => self.workspace_ui.start = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let Some(manager) = &self.workspace_ui.manager else {
            return changed;
        };
        let mut deferred = Vec::new();
        loop {
            let intent = manager.borrow_mut().take_intent();
            let Some(intent) = intent else {
                break;
            };
            changed = true;
            match intent {
                Intent::ServiceStatus => {
                    if self.workspace_ui.service.is_none() {
                        let (tx, rx) = oneshot::channel();
                        self.workspace_ui.service = Some(rx);
                        tokio::task::spawn_blocking(move || {
                            let _ = tx.send(service_status());
                        });
                    }
                }
                Intent::OfflineRuntime => {
                    manager.borrow_mut().accept_offline(offline_runtime());
                }
                Intent::StartRuntime => {
                    if self.workspace_ui.start.is_none() {
                        let (tx, rx) = oneshot::channel();
                        self.workspace_ui.start = Some(rx);
                        manager.borrow_mut().note(
                            crate::tui::workspace_manager::Tone::Accent,
                            "Starting the runtime through `jcode runtime start`…",
                        );
                        tokio::spawn(async move {
                            let _ = tx.send(start_runtime().await);
                        });
                    }
                }
                other => deferred.push(other),
            }
        }
        for intent in deferred.into_iter().rev() {
            manager.borrow_mut().intents.push_front(intent);
        }
        changed
    }

    pub(super) async fn dispatch_remote_workspace_requests(
        &mut self,
        remote: &mut RemoteConnection,
    ) {
        if self.workspace_ui.manager.is_none() {
            return;
        }
        if let Some(manager) = &self.workspace_ui.manager {
            manager.borrow_mut().tick();
        }
        self.poll_workspace_local();
        loop {
            let id = remote.reserve_context_request_id();
            let request = self
                .workspace_ui
                .manager
                .as_ref()
                .and_then(|manager| manager.borrow_mut().reserve(id));
            let Some(request) = request else {
                break;
            };
            self.workspace_ui.owned.insert(id);
            if let Err(error) = remote.send_reserved_workspace_request(request).await {
                self.workspace_ui.owned.remove(&id);
                if let Some(manager) = &self.workspace_ui.manager {
                    manager
                        .borrow_mut()
                        .transport_failed(id, &format!("{error:#}"));
                }
                break;
            }
        }
        let intents = self
            .workspace_ui
            .manager
            .as_ref()
            .map(|manager| {
                let mut manager = manager.borrow_mut();
                std::iter::from_fn(|| manager.take_intent()).collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for intent in intents {
            self.run_workspace_intent(remote, intent).await;
        }
    }

    async fn run_workspace_intent(&mut self, remote: &mut RemoteConnection, intent: Intent) {
        let note = |app: &App, tone, text: String| {
            if let Some(manager) = &app.workspace_ui.manager {
                manager.borrow_mut().note(tone, text);
            }
        };
        use crate::tui::workspace_manager::Tone;
        match intent {
            Intent::Resume(session) => {
                if Some(&session) == self.remote_session_id.as_ref() {
                    note(
                        self,
                        Tone::Muted,
                        "Already attached to that session.".into(),
                    );
                    return;
                }
                match remote.resume_session(&session).await {
                    Ok(()) => {
                        if let Some(manager) = &self.workspace_ui.manager {
                            manager.borrow_mut().visible = false;
                        }
                        self.set_status_notice(format!("Workspace → {session}"));
                    }
                    Err(error) => note(
                        self,
                        Tone::Bad,
                        format!("Could not open {session}: {error:#}"),
                    ),
                }
            }
            Intent::OpenTerminal { session, cwd } => {
                let exe = super::launch_client_executable();
                let cwd = cwd
                    .filter(|path| path.is_dir())
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let socket = std::env::var("JCODE_SOCKET").ok();
                match super::spawn_in_new_terminal(&exe, &session, &cwd, socket.as_deref()) {
                    Ok(true) => note(
                        self,
                        Tone::Good,
                        format!("Opened {session} in a new terminal."),
                    ),
                    Ok(false) => note(
                        self,
                        Tone::Warn,
                        format!(
                            "No supported terminal found. Resume with `jcode --resume {session}`."
                        ),
                    ),
                    Err(error) => note(
                        self,
                        Tone::Bad,
                        format!("Terminal launch failed: {error:#}"),
                    ),
                }
            }
            Intent::NewContext { kind, choice } => {
                if let Some(manager) = &self.workspace_ui.manager {
                    manager.borrow_mut().visible = false;
                }
                super::remote::start_scoped_context(self, remote, kind, choice).await;
            }
            other => {
                if let Some(manager) = &self.workspace_ui.manager {
                    manager.borrow_mut().intents.push_back(other);
                }
                self.poll_workspace_local();
            }
        }
    }

    pub(super) fn workspace_debug(&self) -> serde_json::Value {
        self.workspace_ui.manager.as_ref().map_or_else(
            || serde_json::json!({"visible": false}),
            |manager| manager.borrow().debug(),
        )
    }

    pub(super) fn draw_workspace_overlay(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
    ) -> bool {
        let Some(manager) = &self.workspace_ui.manager else {
            return false;
        };
        if !manager.borrow().visible {
            return false;
        }
        manager.borrow_mut().render(frame, area);
        true
    }

    pub(super) fn handle_workspace_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        if let Some(manager) = &self.workspace_ui.manager {
            manager.borrow_mut().mouse(mouse);
        }
        self.poll_workspace_local();
    }
}

/// This host's login-service registration, read like `jcode runtime service status`.
fn service_status() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let socket = crate::server::socket_path();
        crate::runtime_service::status(&socket)
            .map(|status| {
                if !status.installed {
                    "Login service: not installed (`jcode runtime service install` reviews its plan).".to_string()
                } else {
                    format!(
                        "Login service {}: installed{}, {}{}{}",
                        status.label,
                        match status.current {
                            Some(false) => " (definition differs from the reviewed plan)",
                            _ => "",
                        },
                        if status.loaded { "loaded" } else { "not loaded" },
                        status.pid.map(|pid| format!(", pid {pid}")).unwrap_or_default(),
                        status.last_exit.map(|exit| format!(", last exit {exit}")).unwrap_or_default(),
                    )
                }
            })
            .map_err(|error| format!("{error:#}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Login service supervision is supported on macOS only.".into())
    }
}

/// Durable intent for the selected socket. Never starts a process and never
/// creates the IPC directory.
fn offline_runtime() -> OfflineRuntime {
    let socket = crate::server::socket_path();
    if !socket.parent().is_some_and(std::path::Path::exists) {
        return OfflineRuntime {
            status: None,
            detail: "Runtime socket directory is unavailable. No process started and no directory created.".into(),
        };
    }
    match crate::runtime_lifecycle::RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &socket,
    )
    .and_then(|store| store.status())
    {
        Ok(status) => OfflineRuntime {
            status: Some(status),
            detail: "Durable intent only; not proof of live work.".into(),
        },
        Err(error) => OfflineRuntime {
            status: None,
            detail: format!("Durable runtime state unreadable: {error:#}"),
        },
    }
}

/// Explicit Start through the CLI control, which owns the spawn lock, service
/// handoff and namespace verification.
async fn start_runtime() -> Result<String, String> {
    let exe = super::launch_client_executable();
    let socket = crate::server::socket_path();
    let output = tokio::process::Command::new(&exe)
        .arg("--socket")
        .arg(&socket)
        .args(["runtime", "start", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| format!("Could not run {}: {error}", exe.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let detail = serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|value| {
                value
                    .get("detail")
                    .and_then(|d| d.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Runtime started.".into());
        Ok(detail)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_as_complete_tokens() {
        assert_eq!(parse_command("/workspace"), Some(None));
        assert_eq!(
            parse_command("/workspace closeout"),
            Some(Some(Section::Closeout))
        );
        assert_eq!(parse_command("/runtime"), Some(Some(Section::Runtime)));
        assert_eq!(
            parse_command("/runtime start"),
            Some(Some(Section::Runtime))
        );
        assert_eq!(
            parse_command("/workspace on"),
            None,
            "Niri subcommands moved to /niri"
        );
        assert_eq!(parse_command("/workspacex"), None);
        assert_eq!(parse_command("/runtime frobnicate"), None);
    }
}
