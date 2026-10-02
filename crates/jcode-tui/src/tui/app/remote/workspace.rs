use super::{App, DisplayMessage, begin_remote_split_launch};
use crate::tui::backend::RemoteConnection;
use crate::tui::keybind::WorkspaceNavigationDirection;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};

pub(super) async fn handle_workspace_navigation_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    remote: &mut RemoteConnection,
) -> Result<bool> {
    if !app.workspace_client.is_enabled() {
        return Ok(false);
    }

    let Some(direction) = app.workspace_navigation_keys.direction_for(code, modifiers) else {
        return Ok(false);
    };

    let target = match direction {
        WorkspaceNavigationDirection::Left => app.workspace_client.navigate_left(),
        WorkspaceNavigationDirection::Right => app.workspace_client.navigate_right(),
        WorkspaceNavigationDirection::Up => app.workspace_client.navigate_up(),
        WorkspaceNavigationDirection::Down => app.workspace_client.navigate_down(),
    };

    if app.is_processing {
        app.set_status_notice("Finish current work before moving Niri focus");
        return Ok(true);
    }

    let Some(target_session_id) = target else {
        app.set_status_notice("No Niri session in that direction");
        return Ok(true);
    };
    remote.resume_session(&target_session_id).await?;
    let label = crate::id::extract_session_name(&target_session_id)
        .map(|name| name.to_string())
        .unwrap_or(target_session_id);
    app.set_status_notice(format!("Niri → {}", label));
    Ok(true)
}

/// Niri-style session rows. Formerly `/workspace`; that name now opens
/// workspace management, so the two features never share a command.
pub(super) async fn handle_niri_command(
    app: &mut App,
    remote: &mut RemoteConnection,
    trimmed: &str,
) -> Result<bool> {
    if trimmed != "/niri" && !trimmed.starts_with("/niri ") {
        return Ok(false);
    }

    let current_session = app
        .remote_session_id
        .as_deref()
        .or(app.resume_session_id.as_deref())
        .or(Some(app.session.id.as_str()));

    match trimmed {
        "/niri" | "/niri status" => {
            app.push_display_message(DisplayMessage::system(
                app.workspace_client.status_summary(),
            ));
            return Ok(true);
        }
        "/niri on" | "/niri import" => {
            app.workspace_client
                .enable(current_session, &app.remote_sessions);
            app.set_status_notice("Niri mode enabled");
            app.push_display_message(DisplayMessage::system(
                app.workspace_client.status_summary(),
            ));
            return Ok(true);
        }
        "/niri off" => {
            app.workspace_client.disable();
            app.set_status_notice("Niri mode disabled");
            app.push_display_message(DisplayMessage::system("Niri mode: off".to_string()));
            return Ok(true);
        }
        _ => {}
    }

    let target = match trimmed {
        "/niri add" | "/niri add right" => {
            Some(crate::tui::workspace_client::WorkspaceSplitTarget::Right)
        }
        "/niri add up" => Some(crate::tui::workspace_client::WorkspaceSplitTarget::Up),
        "/niri add down" => Some(crate::tui::workspace_client::WorkspaceSplitTarget::Down),
        _ => None,
    };

    if let Some(target) = target {
        app.workspace_client
            .enable(current_session, &app.remote_sessions);
        app.workspace_client.queue_split_target(target);
        app.pending_split_label = Some("Niri".to_string());
        if app.is_processing {
            app.pending_split_request = true;
            app.push_display_message(DisplayMessage::system(
                "Niri add queued - new session will be created when idle.".to_string(),
            ));
            app.set_status_notice("Niri add queued");
        } else {
            begin_remote_split_launch(app, "Niri");
            remote.split().await?;
        }
        return Ok(true);
    }

    app.push_display_message(DisplayMessage::system(
        "/niri\n  Show Niri session-row status.\n\n/niri on\n  Enable/import Niri mode for current remote sessions.\n\n/niri off\n  Disable Niri mode.\n\n/niri add\n  Split current session and add it to the right in the current row.\n\n/niri add up\n  Split current session into the row above.\n\n/niri add down\n  Split current session into the row below."
            .to_string(),
    ));
    Ok(true)
}
