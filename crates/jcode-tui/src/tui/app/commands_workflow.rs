//! Command input, managed rendering, and existing local/remote dispatch policy.
use super::{App, DisplayMessage, ImproveMode};
use super::{commands, commands_improve as improve, remote};
use crate::workflow::{
    CommandWorkflow as C, WorkflowLoopMode as M, WorkflowPromptRequest, WorkflowTodo,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PendingCommand {
    pub command: C,
    pub original: String,
    pub session_id: String,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(skip)]
    pub suspended: bool,
    #[serde(skip)]
    pub request_id: Option<u64>,
    #[serde(skip)]
    pub rendered: Option<std::result::Result<String, String>>,
}

fn mode_to_request(mode: ImproveMode) -> M {
    match mode {
        ImproveMode::ImproveRun => M::ImproveRun,
        ImproveMode::ImprovePlan => M::ImprovePlan,
        ImproveMode::RefactorRun => M::RefactorRun,
        ImproveMode::RefactorPlan => M::RefactorPlan,
    }
}
fn mode_from_request(mode: M) -> ImproveMode {
    match mode {
        M::ImproveRun => ImproveMode::ImproveRun,
        M::ImprovePlan => ImproveMode::ImprovePlan,
        M::RefactorRun => ImproveMode::RefactorRun,
        M::RefactorPlan => ImproveMode::RefactorPlan,
    }
}
fn mode_change(command: &C) -> Option<Option<ImproveMode>> {
    match command {
        C::Improve { plan_only, .. } => Some(Some(commands::improve_mode_for(*plan_only))),
        C::Refactor { plan_only, .. } => Some(Some(commands::refactor_mode_for(*plan_only))),
        C::ImproveResume { mode, .. } | C::RefactorResume { mode, .. } => {
            Some(Some(mode_from_request(*mode)))
        }
        C::ImproveStop | C::RefactorStop => Some(None),
        _ => None,
    }
}
fn current_mode(app: &App, refactor: bool) -> Option<ImproveMode> {
    app.improve_mode
        .or_else(|| app.session.improve_mode.map(commands::restore_improve_mode))
        .filter(|mode| {
            if refactor {
                mode.is_refactor()
            } else {
                mode.is_improve()
            }
        })
}
fn loop_request(
    app: &mut App,
    refactor: bool,
    operation: &str,
    run: Option<(bool, Option<String>)>,
) -> Option<C> {
    if let Some((plan_only, focus)) = run {
        return Some(if refactor {
            C::Refactor { plan_only, focus }
        } else {
            C::Improve { plan_only, focus }
        });
    }
    let family = if refactor { "refactor" } else { "improve" };
    if operation == "status" {
        app.push_display_message(DisplayMessage::system(if refactor {
            commands::format_refactor_status(app)
        } else {
            commands::format_improve_status(app)
        }));
        return None;
    }
    let todos = crate::todo::load_todos(&commands::active_session_id(app)).unwrap_or_default();
    let incomplete = todos
        .iter()
        .filter(|todo| todo.status != "completed" && todo.status != "cancelled")
        .map(WorkflowTodo::from)
        .collect::<Vec<_>>();
    let mode = current_mode(app, refactor);
    if operation == "stop" {
        if mode.is_none() && !app.is_processing && incomplete.is_empty() {
            app.push_display_message(DisplayMessage::system(format!(
                "No active {family} loop to stop. Use /{family} to start one."
            )));
            return None;
        }
        return Some(if refactor {
            C::RefactorStop
        } else {
            C::ImproveStop
        });
    }
    let Some(mode) = mode else {
        app.push_display_message(DisplayMessage::system(format!("No saved {family} run found for this session. Use /{family} or /{family} plan to start one.")));
        return None;
    };
    Some(if refactor {
        C::RefactorResume {
            mode: mode_to_request(mode),
            todos: incomplete,
        }
    } else {
        C::ImproveResume {
            mode: mode_to_request(mode),
            todos: incomplete,
        }
    })
}

pub(super) fn handle(app: &mut App, text: &str) -> bool {
    let command = match text {
        "/commit" => Some(C::Commit),
        "/commit-push" | "/commit-and-push" => Some(C::CommitPush),
        "/fast-release" | "/cut-release" | "/commit-push-release" => Some(C::ReleaseFast),
        "/fast-macos-release" => Some(C::ReleaseMacos),
        "/remote-release" => Some(C::ReleaseRemote),
        _ => None,
    };
    let command = if command.is_some() {
        command
    } else if text == "/triage" || text.starts_with("/triage ") {
        Some(C::Triage {
            focus: text.strip_prefix("/triage").unwrap_or_default().into(),
        })
    } else if text == "/test" || text.starts_with("/test ") {
        let claim = text.strip_prefix("/test").unwrap_or_default().trim();
        if matches!(claim, "help" | "--help" | "-h") {
            app.push_display_message(DisplayMessage::system("Usage: /test [claim|feature|current changes]\n\nRuns a layered verification pass and returns evidence, confidence, and gaps."));
            return true;
        }
        Some(C::Test {
            claim: claim.into(),
        })
    } else if let Some(plan) = commands::parse_plan_command(text) {
        Some(C::Plan { goal: plan.goal })
    } else if let Some(parsed) = commands::parse_improve_command(text) {
        match parsed {
            Err(e) => {
                app.push_display_message(DisplayMessage::error(e));
                None
            }
            Ok(commands::ImproveCommand::Status) => loop_request(app, false, "status", None),
            Ok(commands::ImproveCommand::Stop) => loop_request(app, false, "stop", None),
            Ok(commands::ImproveCommand::Resume) => loop_request(app, false, "resume", None),
            Ok(commands::ImproveCommand::Run { plan_only, focus }) => {
                loop_request(app, false, "run", Some((plan_only, focus)))
            }
        }
    } else if let Some(parsed) = commands::parse_refactor_command(text) {
        match parsed {
            Err(e) => {
                app.push_display_message(DisplayMessage::error(e));
                None
            }
            Ok(commands::RefactorCommand::Status) => loop_request(app, true, "status", None),
            Ok(commands::RefactorCommand::Stop) => loop_request(app, true, "stop", None),
            Ok(commands::RefactorCommand::Resume) => loop_request(app, true, "resume", None),
            Ok(commands::RefactorCommand::Run { plan_only, focus }) => {
                loop_request(app, true, "run", Some((plan_only, focus)))
            }
        }
    } else {
        return false;
    };
    let Some(command) = command else {
        return true;
    };
    if app.is_remote {
        let session_id = commands::active_session_id(app);
        app.pending_workflow_commands
            .retain(|p| !(p.suspended && p.original == text && p.session_id == session_id));
        app.pending_workflow_commands.push(PendingCommand {
            command,
            original: text.into(),
            session_id: commands::active_session_id(app),
            working_dir: app.session.working_dir.clone(),
            cancelled: false,
            suspended: false,
            request_id: None,
            rendered: None,
        });
        app.pending_queued_dispatch = true;
        app.set_status_notice("Preparing managed workflow instructions");
        app.save_input_for_reload(&commands::active_session_id(app));
    } else {
        let result = crate::instruction::workflow::render_command(
            &crate::instruction::InstructionRepositoryService::new(),
            commands::active_working_dir(app).as_deref(),
            &command,
        )
        .map_err(anyhow::Error::new)
        .and_then(|body| dispatch_local(app, &command, body));
        if let Err(error) = result {
            restore_failed(app, text, &error.to_string());
        }
    }
    true
}

fn family(command: &C) -> &'static str {
    match command {
        C::Commit => "commit",
        C::CommitPush => "commit-push",
        C::ReleaseFast => "fast-release",
        C::ReleaseMacos => "fast-macos-release",
        C::ReleaseRemote => "remote-release",
        C::Triage { .. } => "triage",
        C::Test { .. } => "test",
        C::Plan { .. } => "plan",
        C::Improve { .. } | C::ImproveResume { .. } | C::ImproveStop => "improve",
        _ => "refactor",
    }
}
fn commit_family(command: &C) -> bool {
    matches!(
        command,
        C::Commit
            | C::CommitPush
            | C::ReleaseFast
            | C::ReleaseMacos
            | C::ReleaseRemote
            | C::Triage { .. }
    )
}
fn notice(command: &C, busy: bool, is_remote: bool) -> String {
    match command {
        C::Commit => commands::commit_launch_notice(busy),
        C::CommitPush => commands::commit_push_launch_notice(busy),
        C::ReleaseFast => commands::fast_release_launch_notice(busy),
        C::ReleaseMacos => commands::fast_macos_release_launch_notice(busy),
        C::ReleaseRemote => commands::remote_release_launch_notice(busy),
        C::Triage { .. } => commands::triage_launch_notice(busy),
        C::Plan { goal } => commands::plan_launch_notice(goal.as_deref(), busy),
        C::Improve { plan_only, focus } => {
            commands::improve_launch_notice(*plan_only, focus.as_deref(), busy)
        }
        C::Refactor { plan_only, focus } => {
            commands::refactor_launch_notice(*plan_only, focus.as_deref(), busy)
        }
        C::ImproveStop => commands::improve_stop_notice(busy),
        C::RefactorStop => commands::refactor_stop_notice(busy),
        C::ImproveResume { mode, .. } | C::RefactorResume { mode, .. } => {
            let mode = mode_from_request(*mode);
            if busy && !is_remote {
                if mode.is_refactor() {
                    commands::refactor_launch_notice(
                        matches!(mode, ImproveMode::RefactorPlan),
                        None,
                        true,
                    )
                } else {
                    commands::improve_launch_notice(
                        matches!(mode, ImproveMode::ImprovePlan),
                        None,
                        true,
                    )
                }
            } else {
                format!(
                    "♻️ {}{}...",
                    if busy {
                        "Interrupting and resuming "
                    } else {
                        "Resuming "
                    },
                    mode.status_label()
                )
            }
        }
        C::Test { .. } => if busy {
            "Queued /test; verification will run after the current turn."
        } else {
            "Running /test verification orchestrator."
        }
        .into(),
    }
}
fn interrupt_status(command: &C) -> String {
    let suffix = match command {
        C::ImproveStop | C::RefactorStop => " stop",
        C::ImproveResume { .. } | C::RefactorResume { .. } => " resume",
        C::Improve {
            plan_only: true, ..
        }
        | C::Refactor {
            plan_only: true, ..
        } => " plan",
        _ => "",
    };
    format!("Interrupting for /{}{suffix}...", family(command))
}
fn cancel_reason(command: &C) -> &'static str {
    match command {
        C::Plan { .. } => "slash_plan",
        C::ImproveStop => "slash_improve_stop",
        C::RefactorStop => "slash_refactor_stop",
        C::ImproveResume { .. } => "slash_improve_resume",
        C::RefactorResume { .. } => "slash_refactor_resume",
        C::Improve { .. } => "slash_improve_run",
        C::Refactor { .. } => "slash_refactor_run",
        _ => "slash_workflow",
    }
}
fn persist_mode(app: &mut App, command: &C, is_remote: bool) -> anyhow::Result<()> {
    if let Some(mode) = mode_change(command) {
        let stored = mode.map(commands::session_improve_mode_for);
        if is_remote {
            remote::persist_remote_session_metadata(app, |s| s.improve_mode = stored)?;
        } else {
            let mut session = app.session.clone();
            session.improve_mode = stored;
            session.save()?;
            app.session = session;
        }
        app.improve_mode = mode;
    }
    Ok(())
}
fn queue_test(app: &mut App, body: String) {
    app.queued_messages.push(body);
    let busy = app.is_processing;
    if !busy {
        app.pending_queued_dispatch = true;
    }
    app.push_display_message(DisplayMessage::system(notice(
        &C::Test {
            claim: String::new(),
        },
        busy,
        app.is_remote,
    )));
    app.set_status_notice(if busy {
        "Queued /test"
    } else {
        "Running /test"
    });
}
fn dispatch_local(app: &mut App, command: &C, body: String) -> anyhow::Result<()> {
    persist_mode(app, command, false)?;
    if matches!(command, C::Test { .. }) {
        queue_test(app, body);
        return Ok(());
    }
    let busy = app.is_processing;
    let display = notice(command, busy, false);
    if busy {
        improve::interrupt_and_queue_synthetic_message(
            app,
            body,
            &interrupt_status(command),
            display,
        );
    } else {
        app.push_display_message(DisplayMessage::system(display));
        improve::start_synthetic_user_turn(app, body);
    }
    Ok(())
}
async fn dispatch_remote(
    app: &mut App,
    connection: &mut crate::tui::backend::RemoteConnection,
    command: &C,
    body: String,
) -> anyhow::Result<()> {
    persist_mode(app, command, true)?;
    if matches!(command, C::Test { .. }) {
        queue_test(app, body);
        return Ok(());
    }
    let busy = app.is_processing;
    app.push_display_message(DisplayMessage::system(notice(command, busy, true)));
    if busy {
        if commit_family(command) {
            let id = connection
                .soft_interrupt(body.clone(), vec![], false)
                .await?;
            app.track_pending_soft_interrupt(id, body);
        } else {
            connection
                .cancel_with_reason(cancel_reason(command))
                .await?;
            app.queued_messages.push(body);
        }
        app.set_status_notice(interrupt_status(command));
    } else {
        let system = !commit_family(command);
        remote::begin_remote_send(app, connection, body, vec![], system, None, system, 0).await?;
    }
    Ok(())
}
fn restore_failed(app: &mut App, original: &str, error: &str) {
    if app.input.is_empty() {
        app.input = original.into();
        app.cursor_pos = app.input.len();
    }
    app.push_display_message(DisplayMessage::error(format!(
        "Workflow {original} was not dispatched: {error}"
    )));
    app.set_status_notice("Workflow instructions need repair");
}

pub(super) fn accept_event(
    app: &mut App,
    event: crate::protocol::ServerEvent,
) -> std::result::Result<(), Box<crate::protocol::ServerEvent>> {
    use crate::protocol::ServerEvent as E;
    let id = match &event {
        E::WorkflowPromptRendered { id, .. } | E::Error { id, .. } => *id,
        _ => return Err(Box::new(event)),
    };
    let Some(pending) = app
        .pending_workflow_commands
        .iter_mut()
        .find(|p| p.request_id == Some(id))
    else {
        return Err(Box::new(event));
    };
    pending.rendered = Some(match event {
        E::WorkflowPromptRendered { content, .. } => Ok(content),
        E::Error { message, .. } => Err(message),
        _ => unreachable!(),
    });
    app.pending_queued_dispatch = true;
    Ok(())
}

pub(super) async fn poll(
    app: &mut App,
    connection: &mut crate::tui::backend::RemoteConnection,
) -> bool {
    app.pending_workflow_commands
        .retain(|p| !p.cancelled || (p.request_id.is_some() && p.rendered.is_none()));
    let current = commands::active_session_id(app);
    let Some(index) = app
        .pending_workflow_commands
        .iter()
        .position(|p| !p.cancelled && !p.suspended)
    else {
        return false;
    };
    if app.pending_workflow_commands[index].session_id != current {
        app.pending_workflow_commands[index].cancelled = true;
        app.pending_queued_dispatch |= has_ready(app);
        app.push_display_message(DisplayMessage::error(
            "Pending workflow cancelled because the active session changed.",
        ));
        return true;
    }
    if app.pending_workflow_commands[index].working_dir != app.session.working_dir {
        app.pending_workflow_commands[index].cancelled = true;
        let original = app.pending_workflow_commands[index].original.clone();
        restore_failed(
            app,
            &original,
            "working directory changed during preparation",
        );
        app.pending_queued_dispatch |= has_ready(app);
        return true;
    }
    if let Some(result) = app.pending_workflow_commands[index].rendered.take() {
        let pending = app.pending_workflow_commands.remove(index);
        match result {
            Ok(body) => {
                if let Err(error) = dispatch_remote(app, connection, &pending.command, body).await {
                    restore_failed(app, &pending.original, &error.to_string());
                }
            }
            Err(error) => restore_failed(app, &pending.original, &error),
        }
        app.pending_queued_dispatch |= has_ready(app);
        app.save_input_for_reload(&commands::active_session_id(app));
        return true;
    }
    if app.pending_workflow_commands[index].request_id.is_none() {
        let id = connection.reserve_workflow_request_id();
        let pending = &mut app.pending_workflow_commands[index];
        pending.request_id = Some(id);
        let request = WorkflowPromptRequest::Command {
            command: pending.command.clone(),
        };
        if let Err(error) = connection.render_workflow_prompt(id, request).await {
            app.pending_workflow_commands[index].rendered = Some(Err(error.to_string()));
        }
        return true;
    }
    false
}

pub(super) fn cancel_pending(app: &mut App) -> bool {
    let session = commands::active_session_id(app);
    let mut changed = false;
    for pending in &mut app.pending_workflow_commands {
        if pending.session_id == session && !pending.cancelled {
            pending.cancelled = true;
            changed = true;
        }
    }
    if changed {
        app.set_status_notice("Pending workflow preparation cancelled");
        app.save_input_for_reload(&session);
    }
    changed
}

pub(super) fn has_ready(app: &App) -> bool {
    app.pending_workflow_commands.iter().any(|pending| {
        !pending.cancelled
            && !pending.suspended
            && (pending.request_id.is_none() || pending.rendered.is_some())
    })
}

pub(super) fn reset_connection(app: &mut App) {
    app.pending_workflow_commands.retain(|p| !p.cancelled);
    for pending in &mut app.pending_workflow_commands {
        pending.request_id = None;
        pending.rendered = None;
    }
    app.pending_queued_dispatch |= has_ready(app);
}

impl PendingCommand {
    pub(super) fn allocated_bytes(&self) -> usize {
        let rendered = self.rendered.as_ref().map_or(0, |result| match result {
            Ok(text) | Err(text) => text.capacity(),
        });
        self.command
            .allocated_bytes()
            .saturating_add(self.original.capacity())
            .saturating_add(self.session_id.capacity())
            .saturating_add(self.working_dir.as_ref().map_or(0, String::capacity))
            .saturating_add(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Request, ServerEvent};
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn remote_commands_render_on_server_before_mode_changes_and_preserve_dispatch_policy() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut app = crate::tui::app::tests::create_test_app();
        app.is_remote = true;
        app.session.save().unwrap();
        let had_store = home.root().join("instructions").exists();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let mut connection = crate::tui::backend::RemoteConnection::dummy();
        let mut reader = tokio::io::BufReader::new(connection.take_dummy_peer().unwrap());
        let read = |reader: &mut tokio::io::BufReader<crate::transport::Stream>| {
            let mut line = String::new();
            runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    reader.read_line(&mut line),
                )
                .await
                .unwrap()
                .unwrap();
            });
            serde_json::from_str::<Request>(&line).unwrap()
        };
        assert!(handle(&mut app, "/improve plan focused"));
        assert!(app.improve_mode.is_none());
        assert!(!app.is_processing);
        assert!(runtime.block_on(poll(&mut app, &mut connection)));
        let Request::RenderWorkflowPrompt {
            id,
            workflow:
                WorkflowPromptRequest::Command {
                    command:
                        C::Improve {
                            plan_only: true,
                            focus,
                        },
                },
        } = read(&mut reader)
        else {
            panic!("expected typed render request")
        };
        assert_eq!(focus.as_deref(), Some("focused"));
        assert_eq!(app.pending_workflow_commands[0].request_id, Some(id));
        accept_event(
            &mut app,
            ServerEvent::WorkflowPromptRendered {
                id,
                content: "SERVER-PLAN".into(),
            },
        )
        .unwrap();
        runtime.block_on(poll(&mut app, &mut connection));
        assert_eq!(app.improve_mode, Some(ImproveMode::ImprovePlan));
        let Request::Message {
            content,
            observe_startup_context,
            ..
        } = read(&mut reader)
        else {
            panic!("expected model turn")
        };
        assert_eq!(content, "SERVER-PLAN");
        assert!(!observe_startup_context);
        let turn = app.current_message_id;
        assert!(handle(&mut app, "/commit"));
        runtime.block_on(poll(&mut app, &mut connection));
        let Request::RenderWorkflowPrompt { id, .. } = read(&mut reader) else {
            panic!("expected render")
        };
        accept_event(
            &mut app,
            ServerEvent::WorkflowPromptRendered {
                id,
                content: "SERVER-COMMIT".into(),
            },
        )
        .unwrap();
        runtime.block_on(poll(&mut app, &mut connection));
        assert!(
            matches!(read(&mut reader),Request::SoftInterrupt{content,..} if content=="SERVER-COMMIT")
        );
        assert_eq!(app.current_message_id, turn);
        assert!(handle(&mut app, "/plan next"));
        runtime.block_on(poll(&mut app, &mut connection));
        let Request::RenderWorkflowPrompt { id, .. } = read(&mut reader) else {
            panic!("expected render")
        };
        accept_event(
            &mut app,
            ServerEvent::WorkflowPromptRendered {
                id,
                content: "SERVER-NEXT".into(),
            },
        )
        .unwrap();
        runtime.block_on(poll(&mut app, &mut connection));
        assert!(matches!(read(&mut reader), Request::Cancel { .. }));
        assert_eq!(
            app.queued_messages.last().unwrap().human_text(),
            Some("SERVER-NEXT")
        );
        assert_eq!(home.root().join("instructions").exists(), had_store);
    }

    #[test]
    fn pending_command_failure_cancellation_and_reload_preserve_intent_and_current_turn() {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut app = crate::tui::app::tests::create_test_app();
        app.is_remote = true;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let mut connection = crate::tui::backend::RemoteConnection::dummy();
        app.is_processing = true;
        app.current_message_id = Some(80);
        assert!(handle(&mut app, "/refactor plan preserve"));
        runtime.block_on(poll(&mut app, &mut connection));
        let id = app.pending_workflow_commands[0].request_id.unwrap();
        app.save_input_for_reload(&app.session.id);
        let restored = App::restore_input_for_reload(&app.session.id).unwrap();
        assert_eq!(restored.pending_workflow_commands.len(), 1);
        assert!(restored.pending_workflow_commands[0].request_id.is_none());
        accept_event(
            &mut app,
            ServerEvent::Error {
                id,
                message: "SYNTHETIC_RENDER_FAILURE".into(),
                retry_after_secs: None,
            },
        )
        .unwrap();
        runtime.block_on(poll(&mut app, &mut connection));
        assert_eq!(app.input, "/refactor plan preserve");
        assert_eq!(app.current_message_id, Some(80));
        assert!(app.is_processing);
        assert!(app.improve_mode.is_none());
        assert!(handle(&mut app, "/commit"));
        runtime.block_on(poll(&mut app, &mut connection));
        let id = app.pending_workflow_commands[0].request_id.unwrap();
        runtime
            .block_on(app.handle_remote_key(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::CONTROL,
                &mut connection,
            ))
            .unwrap();
        assert!(app.pending_workflow_commands[0].cancelled);
        accept_event(
            &mut app,
            ServerEvent::Error {
                id,
                message: "cancelled response".into(),
                retry_after_secs: None,
            },
        )
        .unwrap();
        runtime.block_on(poll(&mut app, &mut connection));
        assert!(app.pending_workflow_commands.is_empty());
        assert_eq!(app.current_message_id, Some(80));
        assert!(handle(&mut app, "/plan after reconnect"));
        runtime.block_on(poll(&mut app, &mut connection));
        reset_connection(&mut app);
        assert!(app.pending_workflow_commands[0].request_id.is_none());
        app.apply_restored_reload_input(restored);
        assert!(app.pending_workflow_commands[0].suspended);
        assert!(!runtime.block_on(poll(&mut app, &mut connection)));
        assert!(handle(&mut app, "/refactor plan preserve"));
        assert_eq!(app.pending_workflow_commands.len(), 1);
        assert!(!app.pending_workflow_commands[0].suspended);
    }
}

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use crate::protocol::{Request, ServerEvent};
    use crossterm::event::{KeyCode, KeyModifiers};
    use tokio::io::AsyncBufReadExt;
    #[test]
    fn actual_enter_and_render_reply_wake_the_shared_event_loop_dispatcher() {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut app = crate::tui::app::tests::create_test_app();
        app.is_remote = true;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let mut connection = crate::tui::backend::RemoteConnection::dummy();
        connection.mark_history_loaded();
        let mut reader = tokio::io::BufReader::new(connection.take_dummy_peer().unwrap());
        app.input = "/plan EXPLICIT".into();
        app.cursor_pos = app.input.len();
        runtime
            .block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut connection))
            .unwrap();
        assert!(app.pending_queued_dispatch);
        assert!(!app.is_processing);
        assert!(runtime.block_on(remote::flush_requested_followups(&mut app, &mut connection)));
        assert!(!app.pending_queued_dispatch);
        let mut line = String::new();
        runtime.block_on(reader.read_line(&mut line)).unwrap();
        let Request::RenderWorkflowPrompt {
            id,
            workflow:
                WorkflowPromptRequest::Command {
                    command: C::Plan { goal },
                },
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("expected render request")
        };
        assert_eq!(goal.as_deref(), Some("EXPLICIT"));
        assert!(
            !runtime.block_on(remote::flush_requested_followups(&mut app, &mut connection)),
            "waiting for a reply must not spin"
        );
        app.handle_server_event(
            ServerEvent::WorkflowPromptRendered {
                id,
                content: "SYNTHETIC-PLAN".into(),
            },
            &mut connection,
        );
        assert!(app.pending_queued_dispatch);
        assert!(runtime.block_on(remote::flush_requested_followups(&mut app, &mut connection)));
        line.clear();
        runtime.block_on(reader.read_line(&mut line)).unwrap();
        assert!(
            matches!(serde_json::from_str::<Request>(&line).unwrap(),Request::Message{content,..} if content=="SYNTHETIC-PLAN")
        );
        assert!(app.is_processing);
        assert!(app.pending_workflow_commands.is_empty());
        assert!(!app.pending_queued_dispatch);
    }
}
