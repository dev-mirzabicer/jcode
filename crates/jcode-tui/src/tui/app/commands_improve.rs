use super::commands::active_session_id;
use super::commands_review::{ImproveCommand, RefactorCommand};
use super::{App, DisplayMessage, ImproveMode, ProcessingStatus};
use crate::message::{ContentBlock, Message, Role};
use std::time::Instant;

pub(super) fn improve_usage() -> &'static str {
    "Usage: /improve [focus], /improve plan [focus], /improve resume, /improve status, or /improve stop"
}

pub(super) fn parse_improve_command(trimmed: &str) -> Option<Result<ImproveCommand, String>> {
    let rest = trimmed.strip_prefix("/improve")?.trim();
    if rest.is_empty() {
        return Some(Ok(ImproveCommand::Run {
            plan_only: false,
            focus: None,
        }));
    }

    if rest == "status" {
        return Some(Ok(ImproveCommand::Status));
    }

    if rest == "resume" {
        return Some(Ok(ImproveCommand::Resume));
    }

    if rest == "stop" {
        return Some(Ok(ImproveCommand::Stop));
    }

    if rest == "plan" {
        return Some(Ok(ImproveCommand::Run {
            plan_only: true,
            focus: None,
        }));
    }

    if let Some(focus) = rest.strip_prefix("plan ") {
        let focus = focus.trim();
        return Some(if focus.is_empty() {
            Err(improve_usage().to_string())
        } else {
            Ok(ImproveCommand::Run {
                plan_only: true,
                focus: Some(focus.to_string()),
            })
        });
    }

    if rest.starts_with("status ") || rest.starts_with("resume ") || rest.starts_with("stop ") {
        return Some(Err(improve_usage().to_string()));
    }

    Some(Ok(ImproveCommand::Run {
        plan_only: false,
        focus: Some(rest.to_string()),
    }))
}

pub(super) fn refactor_usage() -> &'static str {
    "Usage: /refactor [focus], /refactor plan [focus], /refactor resume, /refactor status, or /refactor stop"
}

pub(super) fn parse_refactor_command(trimmed: &str) -> Option<Result<RefactorCommand, String>> {
    let rest = trimmed.strip_prefix("/refactor")?.trim();
    if rest.is_empty() {
        return Some(Ok(RefactorCommand::Run {
            plan_only: false,
            focus: None,
        }));
    }

    if rest == "status" {
        return Some(Ok(RefactorCommand::Status));
    }

    if rest == "resume" {
        return Some(Ok(RefactorCommand::Resume));
    }

    if rest == "stop" {
        return Some(Ok(RefactorCommand::Stop));
    }

    if rest == "plan" {
        return Some(Ok(RefactorCommand::Run {
            plan_only: true,
            focus: None,
        }));
    }

    if let Some(focus) = rest.strip_prefix("plan ") {
        let focus = focus.trim();
        return Some(if focus.is_empty() {
            Err(refactor_usage().to_string())
        } else {
            Ok(RefactorCommand::Run {
                plan_only: true,
                focus: Some(focus.to_string()),
            })
        });
    }

    if rest.starts_with("status ") || rest.starts_with("resume ") || rest.starts_with("stop ") {
        return Some(Err(refactor_usage().to_string()));
    }

    Some(Ok(RefactorCommand::Run {
        plan_only: false,
        focus: Some(rest.to_string()),
    }))
}

pub(super) fn improve_mode_for(plan_only: bool) -> ImproveMode {
    if plan_only {
        ImproveMode::ImprovePlan
    } else {
        ImproveMode::ImproveRun
    }
}

pub(super) fn refactor_mode_for(plan_only: bool) -> ImproveMode {
    if plan_only {
        ImproveMode::RefactorPlan
    } else {
        ImproveMode::RefactorRun
    }
}

pub(super) fn session_improve_mode_for(mode: ImproveMode) -> crate::session::SessionImproveMode {
    match mode {
        ImproveMode::ImproveRun => crate::session::SessionImproveMode::ImproveRun,
        ImproveMode::ImprovePlan => crate::session::SessionImproveMode::ImprovePlan,
        ImproveMode::RefactorRun => crate::session::SessionImproveMode::RefactorRun,
        ImproveMode::RefactorPlan => crate::session::SessionImproveMode::RefactorPlan,
    }
}

pub(super) fn restore_improve_mode(mode: crate::session::SessionImproveMode) -> ImproveMode {
    match mode {
        crate::session::SessionImproveMode::ImproveRun => ImproveMode::ImproveRun,
        crate::session::SessionImproveMode::ImprovePlan => ImproveMode::ImprovePlan,
        crate::session::SessionImproveMode::RefactorRun => ImproveMode::RefactorRun,
        crate::session::SessionImproveMode::RefactorPlan => ImproveMode::RefactorPlan,
    }
}

pub(super) fn improve_launch_notice(
    plan_only: bool,
    focus: Option<&str>,
    interrupted: bool,
) -> String {
    let action = if plan_only {
        "improvement plan"
    } else {
        "improvement loop"
    };
    let prefix = if interrupted {
        "👉 Interrupting and starting"
    } else {
        "🚀 Starting"
    };
    match focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        Some(focus) => format!("{} {} focused on {}...", prefix, action, focus),
        None => format!("{} {}...", prefix, action),
    }
}

pub(super) fn improve_stop_notice(interrupted: bool) -> String {
    if interrupted {
        "🛑 Interrupting and stopping the improve loop at the next safe point...".to_string()
    } else {
        "🛑 Stopping the improve loop after the next safe point...".to_string()
    }
}

pub(super) fn refactor_launch_notice(
    plan_only: bool,
    focus: Option<&str>,
    interrupted: bool,
) -> String {
    let action = if plan_only {
        "refactor plan"
    } else {
        "refactor loop"
    };
    let prefix = if interrupted {
        "👉 Interrupting and starting"
    } else {
        "🚀 Starting"
    };
    match focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        Some(focus) => format!("{} {} focused on {}...", prefix, action, focus),
        None => format!("{} {}...", prefix, action),
    }
}

pub(super) fn refactor_stop_notice(interrupted: bool) -> String {
    if interrupted {
        "🛑 Interrupting and stopping the refactor loop at the next safe point...".to_string()
    } else {
        "🛑 Stopping the refactor loop after the next safe point...".to_string()
    }
}

fn current_mode_for(app: &App, predicate: impl Fn(ImproveMode) -> bool) -> Option<ImproveMode> {
    app.improve_mode
        .or_else(|| app.session.improve_mode.map(restore_improve_mode))
        .filter(|mode| predicate(*mode))
}

pub(super) fn start_synthetic_user_turn(app: &mut App, content: String) {
    app.commit_pending_streaming_assistant_message();
    app.add_provider_message(Message::user(&content));
    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: content,
            cache_control: None,
        }],
    );
    let _ = app.session.save();

    app.is_processing = true;
    app.status = ProcessingStatus::Sending;
    app.clear_streaming_render_state();
    app.stream_buffer.clear();
    app.thought_line_inserted = false;
    app.thinking_prefix_emitted = false;
    app.thinking_buffer.clear();
    app.streaming_tool_calls.clear();
    app.batch_progress = None;
    app.streaming.streaming_input_tokens = 0;
    app.streaming.streaming_output_tokens = 0;
    app.streaming.streaming_cache_read_tokens = None;
    app.streaming.streaming_cache_creation_tokens = None;
    app.kv_cache.current_api_usage_recorded = false;
    app.upstream_provider = None;
    app.status_detail = None;
    app.streaming.streaming_tps_start = None;
    app.streaming.streaming_tps_elapsed = std::time::Duration::ZERO;
    app.streaming.streaming_tps_collect_output = false;
    app.streaming.streaming_total_output_tokens = 0;
    app.streaming.streaming_tps_observed_output_tokens = 0;
    app.streaming.streaming_tps_observed_elapsed = std::time::Duration::ZERO;
    app.processing_started = Some(Instant::now());
    app.visible_turn_started = Some(Instant::now());
    app.pending_turn = true;
}

pub(super) fn interrupt_and_queue_synthetic_message(
    app: &mut App,
    content: String,
    status_notice: &str,
    display_notice: String,
) {
    app.cancel_requested = true;
    app.interleave_message = None;
    app.interleave_images.clear();
    app.pending_soft_interrupts.clear();
    app.pending_soft_interrupt_requests.clear();
    app.set_status_notice(status_notice);
    app.push_display_message(DisplayMessage::system(display_notice));
    app.queued_messages.push(content);
}

pub(super) fn format_improve_status(app: &App) -> String {
    let session_id = active_session_id(app);
    let todos = crate::todo::load_todos(&session_id).unwrap_or_default();
    let completed = todos.iter().filter(|t| t.status == "completed").count();
    let cancelled = todos.iter().filter(|t| t.status == "cancelled").count();
    let incomplete: Vec<_> = todos
        .iter()
        .filter(|t| t.status != "completed" && t.status != "cancelled")
        .collect();

    let phase = if app.is_processing {
        if current_mode_for(app, ImproveMode::is_improve).is_some() || !incomplete.is_empty() {
            "running"
        } else {
            "busy (no improve batch detected yet)"
        }
    } else if !incomplete.is_empty() {
        "paused / resumable"
    } else if completed > 0 || cancelled > 0 {
        "idle (last improve batch finished)"
    } else {
        "idle"
    };

    let mode = current_mode_for(app, ImproveMode::is_improve)
        .map(|mode| mode.status_label())
        .unwrap_or("not yet started in this session");

    let mut lines = vec![
        format!("Improve status: {}", phase),
        format!("Last requested mode: {}", mode),
        format!(
            "Todos: {} incomplete · {} completed · {} cancelled",
            incomplete.len(),
            completed,
            cancelled
        ),
    ];

    if !incomplete.is_empty() {
        lines.push(String::new());
        lines.push("Current improve batch:".to_string());
        for todo in incomplete.iter().take(5) {
            let icon = if todo.status == "in_progress" {
                "🔄"
            } else {
                "⬜"
            };
            lines.push(format!(
                "- {} [{}] {}{}",
                icon,
                todo.priority,
                todo.content,
                todo_confidence_suffix(todo)
            ));
        }
        if incomplete.len() > 5 {
            lines.push(format!("- …and {} more", incomplete.len() - 5));
        }
    } else {
        lines.push(String::new());
        lines.push("No current improve todo batch for this session.".to_string());
    }

    lines.push(String::new());
    lines.push("Use /improve to start/continue, /improve resume to continue the last saved mode, /improve plan for plan-only mode, or /improve stop to halt after a safe point.".to_string());
    lines.join("\n")
}

pub(super) fn format_refactor_status(app: &App) -> String {
    let session_id = active_session_id(app);
    let todos = crate::todo::load_todos(&session_id).unwrap_or_default();
    let completed = todos.iter().filter(|t| t.status == "completed").count();
    let cancelled = todos.iter().filter(|t| t.status == "cancelled").count();
    let incomplete: Vec<_> = todos
        .iter()
        .filter(|t| t.status != "completed" && t.status != "cancelled")
        .collect();

    let phase = if app.is_processing {
        if current_mode_for(app, ImproveMode::is_refactor).is_some() || !incomplete.is_empty() {
            "running"
        } else {
            "busy (no refactor batch detected yet)"
        }
    } else if !incomplete.is_empty() {
        "paused / resumable"
    } else if completed > 0 || cancelled > 0 {
        "idle (last refactor batch finished)"
    } else {
        "idle"
    };

    let mode = current_mode_for(app, ImproveMode::is_refactor)
        .map(|mode| mode.status_label())
        .unwrap_or("not yet started in this session");

    let mut lines = vec![
        format!("Refactor status: {}", phase),
        format!("Last requested mode: {}", mode),
        format!(
            "Todos: {} incomplete · {} completed · {} cancelled",
            incomplete.len(),
            completed,
            cancelled
        ),
    ];

    if !incomplete.is_empty() {
        lines.push(String::new());
        lines.push("Current refactor batch:".to_string());
        for todo in incomplete.iter().take(5) {
            let icon = if todo.status == "in_progress" {
                "🔄"
            } else {
                "⬜"
            };
            lines.push(format!(
                "- {} [{}] {}{}",
                icon,
                todo.priority,
                todo.content,
                todo_confidence_suffix(todo)
            ));
        }
        if incomplete.len() > 5 {
            lines.push(format!("- …and {} more", incomplete.len() - 5));
        }
    } else {
        lines.push(String::new());
        lines.push("No current refactor todo batch for this session.".to_string());
    }

    lines.push(String::new());
    lines.push("Use /refactor to start/continue, /refactor resume to continue the last saved mode, /refactor plan for plan-only mode, or /refactor stop to halt after a safe point.".to_string());
    lines.join("\n")
}

fn todo_confidence_suffix(todo: &crate::todo::TodoItem) -> String {
    match todo.confidence {
        Some(state) => format!(" · confidence {}", state.as_str()),
        None => " · confidence unknown".to_string(),
    }
}
