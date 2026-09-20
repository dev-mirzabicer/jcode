#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::client_state::{handle_get_history, spawn_model_prefetch_update};
use super::{
    ClientConnectionInfo, ClientDebugState, FileTouchService, SessionInterruptQueues, SwarmEvent,
    SwarmMember, SwarmState, VersionedPlan, broadcast_swarm_status, fanout_live_client_event,
    persist_swarm_state_for, register_background_tool_signal, register_session_event_sender,
    register_session_interrupt_queue, remove_background_tool_signal, remove_plan_participant,
    remove_session_channel_subscriptions, remove_session_from_swarm,
    remove_session_interrupt_queue, send_swarm_plan_to_session, swarm_id_for_session,
    unregister_session_event_sender, update_member_status,
};
use crate::agent::Agent;
use crate::message::ContentBlock;
use crate::protocol::{NotificationType, ServerEvent};
use crate::provider::Provider;
use crate::tool::Registry;
use crate::transport::WriteHalf;
use anyhow::Result;
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, broadcast};

type SessionAgents = Arc<crate::primary::PrimaryHost>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;
const RELOAD_RESTORE_MARKER_MAX_AGE: Duration = Duration::from_secs(60);

pub(super) fn session_was_interrupted_by_reload(agent: &Agent) -> bool {
    let messages = agent.messages();
    let Some(last) = messages.last() else {
        return false;
    };

    last.content.iter().any(|block| match block {
        ContentBlock::Text { text, .. } => {
            text.ends_with("[generation interrupted - server reloading]")
        }
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            content == "Reload initiated. Process restarting..."
                || (is_error.unwrap_or(false)
                    && (content.contains("interrupted by server reload")
                        || content.contains("Skipped - server reloading")))
        }
        _ => false,
    })
}

pub(super) fn restored_session_was_interrupted(
    session_id: &str,
    previous_status: &crate::session::SessionStatus,
    agent: &Agent,
) -> bool {
    let last_is_user = agent
        .last_message_role()
        .as_ref()
        .map(|role| *role == crate::message::Role::User)
        .unwrap_or(false);
    let last_is_reload_interrupted = session_was_interrupted_by_reload(agent);
    let closed_pending_user_during_reload =
        matches!(previous_status, crate::session::SessionStatus::Closed)
            && last_is_user
            && crate::server::reload_marker_active(RELOAD_RESTORE_MARKER_MAX_AGE);

    if last_is_user && matches!(previous_status, crate::session::SessionStatus::Active) {
        crate::logging::info(&format!(
            "Session {} was Active with pending user message - treating as interrupted",
            session_id
        ));
    }

    if last_is_reload_interrupted {
        crate::logging::info(&format!(
            "Session {} contains reload interruption markers - will auto-resume",
            session_id
        ));
    }

    if closed_pending_user_during_reload {
        crate::logging::info(&format!(
            "Session {} was Closed with a pending user message during a recent reload - treating as interrupted",
            session_id
        ));
    }

    matches!(
        previous_status,
        crate::session::SessionStatus::Crashed { .. }
    ) || (matches!(previous_status, crate::session::SessionStatus::Active) && last_is_user)
        || last_is_reload_interrupted
        || closed_pending_user_during_reload
}

fn mark_remote_reload_started(request_id: &str) {
    crate::server::write_reload_state(
        request_id,
        jcode_build_meta::version(),
        crate::server::ReloadPhase::Starting,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_clear_session(
    id: u64,
    client_session_id: &mut String,
    client_connection_id: &str,
    agent: &Arc<Mutex<Agent>>,
    instruction_repositories: &crate::instruction::InstructionRepositoryService,
    sessions: &SessionAgents,
    startup_context: &super::startup_context::StartupContextCoordinator,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) -> Arc<Mutex<Agent>> {
    let old_session = client_session_id.clone();
    let fresh = match sessions
        .clear_context(agent, &old_session, instruction_repositories, mcp_pool)
        .await
    {
        Ok(fresh) => fresh,
        Err(error) => {
            if let Some(activation) =
                error.downcast_ref::<crate::agent::StartupContextActivationError>()
            {
                let _ = client_event_tx.send(ServerEvent::StartupContextFailed {
                    id,
                    failure: super::startup_context::primary_activation_failure(activation),
                });
            } else {
                let _ = client_event_tx.send(ServerEvent::Error {
                    id,
                    message: format!("Clear was not applied: {error:#}"),
                    retry_after_secs: None,
                });
            }
            return agent.clone();
        }
    };
    let new_id = fresh.lock().await.session_id().to_owned();
    let resources = sessions
        .resources(&new_id, &fresh)
        .expect("published primary resources");
    register_session_interrupt_queue(soft_interrupt_queues, &new_id, resources.interrupts).await;
    shutdown_signals
        .write()
        .await
        .insert(new_id.clone(), resources.shutdown);
    register_background_tool_signal(&new_id, resources.background);
    unregister_session_event_sender(swarm_members, &old_session, client_connection_id).await;
    startup_context.release_connection(client_connection_id);
    *client_session_id = new_id.clone();
    client_event_tx.retarget(&new_id);
    ensure_client_swarm_member(
        &new_id,
        client_connection_id,
        &None,
        client_event_tx,
        &fresh,
        false,
        swarm_members,
        swarms_by_id,
        event_history,
        event_counter,
        swarm_event_tx,
    )
    .await;
    if let Some(connection) = client_connections
        .write()
        .await
        .get_mut(client_connection_id)
    {
        connection.session_id = new_id.clone();
        connection.is_processing = false;
        connection.current_tool_name = None;
        connection.last_seen = Instant::now();
    }
    let _ = client_event_tx.send(ServerEvent::SessionId { session_id: new_id });
    let _ = client_event_tx.send(ServerEvent::Done { id });
    fresh
}

#[allow(clippy::too_many_arguments)]
async fn ensure_client_swarm_member(
    client_session_id: &str,
    client_connection_id: &str,
    friendly_name: &Option<String>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
    agent: &Arc<Mutex<Agent>>,
    swarm_enabled: bool,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) -> bool {
    let client_event_tx = &client_event_tx.for_session(client_session_id);
    let swarm_enabled = swarm_enabled && crate::config::config().features.swarm;
    let (working_dir, derived_swarm_id, fallback_name) = {
        // A target-aware subscribe can attach to an agent that is in the middle
        // of a turn. Never wait for that turn's agent lock just to populate
        // connection metadata: doing so prevents the subscribe request from
        // completing, so subsequent state requests sit unread until the desktop
        // client times out. The persisted startup stub has the same immutable
        // identity metadata and is safe to read while the live agent is busy.
        let (working_dir, fallback_name) = match agent.try_lock() {
            Ok(agent_guard) => (
                agent_guard.working_dir().map(PathBuf::from),
                agent_guard
                    .session_short_name()
                    .map(|value| value.to_string()),
            ),
            Err(_) => {
                crate::logging::info(&format!(
                    "Subscribe metadata for busy session {} is using the persisted startup stub",
                    client_session_id
                ));
                crate::session::Session::load_startup_stub(client_session_id)
                    .map(|session| (session.working_dir.map(PathBuf::from), session.short_name))
                    .unwrap_or((None, None))
            }
        };
        let derived_swarm_id = if swarm_enabled {
            swarm_id_for_session(client_session_id)
        } else {
            None
        };
        (working_dir, derived_swarm_id, fallback_name)
    };

    // Prefer the currently restored agent/session identity over the temporary
    // name captured at raw socket accept time. During resume/reconnect bursts,
    // the temporary pre-resume session name can otherwise leak onto the real
    // resumed session and corrupt swarm metadata.
    let member_name = fallback_name.or_else(|| friendly_name.clone());
    let mut inserted = false;
    {
        let mut members = swarm_members.write().await;
        if let Some(member) = members.get_mut(client_session_id) {
            member.event_tx = client_event_tx.clone();
            member
                .event_txs
                .insert(client_connection_id.to_string(), client_event_tx.clone());
            member.swarm_enabled = swarm_enabled;
            if !swarm_enabled {
                member.swarm_id = None;
                member.role = "agent".to_string();
            }
            member.is_headless = false;
            if member_name.is_some() {
                member.friendly_name = member_name.clone();
            }
        } else {
            let now = Instant::now();
            members.insert(
                client_session_id.to_string(),
                SwarmMember {
                    session_id: client_session_id.to_string(),
                    event_tx: client_event_tx.clone(),
                    event_txs: HashMap::from([(
                        client_connection_id.to_string(),
                        client_event_tx.clone(),
                    )]),
                    working_dir: working_dir.clone(),
                    swarm_id: derived_swarm_id.clone(),
                    swarm_enabled,
                    status: "ready".to_string(),
                    detail: None,
                    task_label: None,
                    friendly_name: member_name.clone(),
                    report_back_to_session_id: None,
                    latest_completion_report: None,
                    role: "agent".to_string(),
                    joined_at: now,
                    last_status_change: now,
                    is_headless: false,
                    output_tail: None,
                    todo_progress: None,
                    todo_items: Vec::new(),
                    runtime: crate::protocol::SwarmMemberRuntime::default(),
                },
            );
            inserted = true;
        }
    }

    if inserted && let Some(ref swarm_id_ref) = derived_swarm_id {
        let mut swarms = swarms_by_id.write().await;
        swarms
            .entry(swarm_id_ref.to_string())
            .or_insert_with(HashSet::new)
            .insert(client_session_id.to_string());
        drop(swarms);
        super::record_swarm_event(
            event_history,
            event_counter,
            swarm_event_tx,
            client_session_id.to_string(),
            member_name,
            Some(swarm_id_ref.to_string()),
            crate::server::SwarmEventType::MemberChange {
                action: "joined".to_string(),
            },
        )
        .await;
    }

    crate::logging::event_info(
        "SESSION_LIFECYCLE",
        vec![
            ("phase", "swarm_member_registered".to_string()),
            ("session_id", client_session_id.to_string()),
            ("client_connection_id", client_connection_id.to_string()),
            ("inserted", inserted.to_string()),
            ("swarm_enabled", swarm_enabled.to_string()),
            (
                "swarm_id",
                derived_swarm_id.unwrap_or_else(|| "none".to_string()),
            ),
        ],
    );

    inserted
}

/// Attachment never selects a location. Busy sessions use the durable metadata
/// projection rather than waiting for inference or trusting another client's cwd.
fn session_working_dir(agent: &Arc<Mutex<Agent>>, session_id: &str) -> Result<Option<PathBuf>> {
    if let Ok(agent) = agent.try_lock() {
        return Ok(agent.working_dir().map(PathBuf::from));
    }
    Ok(crate::session::Session::load_startup_stub(session_id)?
        .working_dir
        .map(PathBuf::from))
}

fn apply_or_defer_subscribe_selfdev(agent: &Arc<Mutex<Agent>>, session_id: &str) {
    if let Ok(mut agent_guard) = agent.try_lock() {
        if !agent_guard.is_canary() {
            agent_guard.set_canary("self-dev");
        }
        return;
    }

    let agent = Arc::clone(agent);
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let mut agent_guard = agent.lock().await;
        if !agent_guard.is_canary() {
            agent_guard.set_canary("self-dev");
        }
        crate::logging::info(&format!(
            "Applied deferred self-dev subscribe metadata for session {}",
            session_id
        ));
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_subscribe(
    id: u64,
    subscribe_working_dir: Option<String>,
    selfdev: Option<bool>,
    register_mcp_tools: bool,
    client_selfdev: &mut bool,
    client_session_id: &str,
    client_connection_id: &str,
    friendly_name: &Option<String>,
    agent: &Arc<Mutex<Agent>>,
    registry: &Registry,
    swarm_enabled: bool,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) {
    let client_event_tx = &client_event_tx.for_session(client_session_id);
    let subscribe_start = Instant::now();
    let bound_working_dir = match session_working_dir(agent, client_session_id) {
        Ok(path) => path,
        Err(error) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Cannot inspect the session working directory: {error:#}"),
                retry_after_secs: None,
            });
            return;
        }
    };
    if let Err(error) =
        crate::execution::retention::record_session_use(client_session_id.to_string()).await
    {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Cannot durably record session opening: {error:#}"),
            retry_after_secs: None,
        });
        return;
    }
    crate::logging::event_info(
        "SESSION_LIFECYCLE",
        vec![
            ("phase", "subscribe_start".to_string()),
            ("request_id", id.to_string()),
            ("session_id", client_session_id.to_string()),
            ("client_connection_id", client_connection_id.to_string()),
            (
                "working_dir_set",
                subscribe_working_dir.is_some().to_string(),
            ),
            ("register_mcp_tools", register_mcp_tools.to_string()),
            ("swarm_enabled", swarm_enabled.to_string()),
        ],
    );
    let inserted_swarm_member = ensure_client_swarm_member(
        client_session_id,
        client_connection_id,
        friendly_name,
        client_event_tx,
        agent,
        swarm_enabled,
        swarm_members,
        swarms_by_id,
        event_history,
        event_counter,
        swarm_event_tx,
    )
    .await;

    if let Some(new_path) = bound_working_dir.clone() {
        let mut old_swarm_id: Option<String> = None;
        let mut updated_swarm_id: Option<String> = None;
        {
            let mut members = swarm_members.write().await;
            if let Some(member) = members.get_mut(client_session_id) {
                old_swarm_id = member.swarm_id.clone();
                // Existing members include reconnects and daemon-restored
                // sessions. Keep their persisted swarm id so an intentional
                // resume retains its workers and plan. Only a newly inserted
                // root receives the new session-scoped identity.
                let new_swarm_id = if inserted_swarm_member {
                    swarm_id_for_session(client_session_id)
                } else {
                    member
                        .swarm_id
                        .clone()
                        .or_else(|| swarm_id_for_session(client_session_id))
                };
                member.working_dir = Some(new_path);
                member.swarm_id = if member.swarm_enabled {
                    new_swarm_id.clone()
                } else {
                    None
                };
                updated_swarm_id = member.swarm_id.clone();
            }
        }

        if let Some(ref old_id) = old_swarm_id {
            if updated_swarm_id.as_ref() != Some(old_id) {
                remove_session_channel_subscriptions(
                    client_session_id,
                    channel_subscriptions,
                    channel_subscriptions_by_session,
                )
                .await;
            }
            let mut swarms = swarms_by_id.write().await;
            if let Some(swarm) = swarms.get_mut(old_id) {
                swarm.remove(client_session_id);
                if swarm.is_empty() {
                    swarms.remove(old_id);
                }
            }
        }

        if let Some(ref new_id) = updated_swarm_id {
            let mut swarms = swarms_by_id.write().await;
            swarms
                .entry(new_id.clone())
                .or_insert_with(HashSet::new)
                .insert(client_session_id.to_string());
        }

        if updated_swarm_id != old_swarm_id {
            crate::logging::event_info(
                "SESSION_LIFECYCLE",
                vec![
                    ("phase", "subscribe_swarm_changed".to_string()),
                    ("session_id", client_session_id.to_string()),
                    ("client_connection_id", client_connection_id.to_string()),
                    (
                        "old_swarm_id",
                        old_swarm_id.clone().unwrap_or_else(|| "none".to_string()),
                    ),
                    (
                        "new_swarm_id",
                        updated_swarm_id
                            .clone()
                            .unwrap_or_else(|| "none".to_string()),
                    ),
                ],
            );
            let mut members = swarm_members.write().await;
            if let Some(member) = members.get_mut(client_session_id) {
                member.role = "agent".to_string();
            }
        }

        if let Some(old_id) = old_swarm_id.clone() {
            let was_coordinator = {
                let coordinators = swarm_coordinators.read().await;
                coordinators
                    .get(&old_id)
                    .map(|session_id| session_id == client_session_id)
                    .unwrap_or(false)
            };
            if was_coordinator {
                let mut new_coordinator: Option<String> = None;
                {
                    let swarms = swarms_by_id.read().await;
                    if let Some(swarm) = swarms.get(&old_id) {
                        new_coordinator = swarm.iter().min().cloned();
                    }
                }
                {
                    let mut coordinators = swarm_coordinators.write().await;
                    coordinators.remove(&old_id);
                    if let Some(ref new_id) = new_coordinator {
                        coordinators.insert(old_id.clone(), new_id.clone());
                    }
                }
                if let Some(new_id) = new_coordinator.clone() {
                    let members = swarm_members.read().await;
                    if let Some(member) = members.get(&new_id) {
                        let _ = member.event_tx.send(ServerEvent::Notification {
                            from_session: new_id.clone(),
                            from_name: member.friendly_name.clone(),
                            notification_type: NotificationType::Message {
                                scope: Some("swarm".to_string()),
                                channel: None,
                                tldr: None,
                            },
                            message: super::notification::render(crate::instruction::notification::Notification::SwarmCoordinatorPromoted, member.working_dir.as_deref()),
                        });
                    }
                }
            }
        }

        if let Some(old_id) = old_swarm_id.clone() {
            if updated_swarm_id.as_ref() != Some(&old_id) {
                remove_plan_participant(&old_id, client_session_id, swarm_plans).await;
                let swarm_state = SwarmState {
                    members: Arc::clone(swarm_members),
                    swarms_by_id: Arc::clone(swarms_by_id),
                    plans: Arc::clone(swarm_plans),
                    coordinators: Arc::clone(swarm_coordinators),
                };
                persist_swarm_state_for(&old_id, &swarm_state).await;
            }
            broadcast_swarm_status(&old_id, swarm_members, swarms_by_id).await;
        }
        if let Some(new_id) = updated_swarm_id
            && old_swarm_id.as_ref() != Some(&new_id)
        {
            broadcast_swarm_status(&new_id, swarm_members, swarms_by_id).await;
        }
    }

    let should_selfdev = *client_selfdev || matches!(selfdev, Some(true));

    if should_selfdev {
        *client_selfdev = true;
        apply_or_defer_subscribe_selfdev(agent, client_session_id);
        registry.register_selfdev_tools().await;
    }

    let mcp_register_ms = if register_mcp_tools {
        let mcp_register_start = Instant::now();
        let mcp_working_dir = bound_working_dir;
        registry
            .register_mcp_tools_for_dir(
                Some(client_event_tx.clone()),
                Some(Arc::clone(mcp_pool)),
                Some(client_session_id.to_string()),
                mcp_working_dir,
            )
            .await;
        mcp_register_start.elapsed().as_millis()
    } else {
        0
    };

    crate::logging::info(&format!(
        "[TIMING] handle_subscribe: session={}, working_dir_set={}, selfdev={}, mcp_register={}ms, total={}ms",
        client_session_id,
        subscribe_working_dir.is_some(),
        should_selfdev,
        mcp_register_ms,
        subscribe_start.elapsed().as_millis(),
    ));
    crate::logging::event_info(
        "SESSION_LIFECYCLE",
        vec![
            ("phase", "subscribe_done".to_string()),
            ("request_id", id.to_string()),
            ("session_id", client_session_id.to_string()),
            ("client_connection_id", client_connection_id.to_string()),
            ("mcp_register_ms", mcp_register_ms.to_string()),
            (
                "elapsed_ms",
                subscribe_start.elapsed().as_millis().to_string(),
            ),
        ],
    );

    if subscribe_should_mark_ready(client_session_id, swarm_members).await {
        update_member_status(
            client_session_id,
            "ready",
            None,
            swarm_members,
            swarms_by_id,
            Some(event_history),
            Some(event_counter),
            Some(swarm_event_tx),
        )
        .await;
    }

    // Re-send the current swarm plan so a reconnecting client renders the
    // plan graph immediately instead of waiting for the next plan mutation.
    send_swarm_plan_to_session(client_session_id, swarm_members, swarm_plans).await;

    // Tell the client which session it is bound to. Local clients learn this
    // from their own launch state, but a remote client (gateway/WebSocket) has
    // no other source, and without it a dropped connection cannot reattach:
    // the next Subscribe carries no `target_session_id`, so the server hands
    // it a brand-new session and the in-flight turn becomes unreachable.
    let _ = client_event_tx.send(ServerEvent::SessionId {
        session_id: client_session_id.to_string(),
    });
    if let Ok(agent_guard) = agent.try_lock()
        && let Some(active_agent) = agent_guard.active_agent()
    {
        let _ = client_event_tx.send(ServerEvent::AgentSelected {
            id,
            agent_id: active_agent.id.clone(),
            display_name: active_agent.display_name.clone(),
            scope: active_agent.scope.to_string(),
            change: crate::protocol::AgentProfileChangeKind::NoChange,
            message_id: agent_guard
                .active_transition_message_id()
                .map(str::to_string),
            message_content: None,
            active_skill_id: agent_guard.active_skill_id().map(str::to_string),
        });
    }
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

async fn subscribe_should_mark_ready(
    client_session_id: &str,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> bool {
    let members = swarm_members.read().await;
    members
        .get(client_session_id)
        .is_none_or(|member| member.status != "running")
}

pub(super) async fn handle_reload(
    id: u64,
    force: bool,
    client_session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    // A non-forced reload (e.g. `jcode server reload`) is a graceful upgrade
    // request: only reload when this server is provably running older code than
    // an available reload candidate. This keeps us from downgrading a newer
    // server (such as a self-dev daemon next to an older release client) and
    // from re-entering the reload-loop family (#277), where a server that merely
    // "differs" can never make the difference go away by reloading.
    if !force && !super::server_has_newer_binary() {
        crate::logging::info(&format!(
            "handle_reload: skipping non-forced reload for client_session_id={} (no strictly-newer binary)",
            client_session_id
        ));
        // Tell the requester this was a deliberate no-op (not a silent success)
        // so callers like `jcode server reload` can report "already up to date"
        // distinctly from an actual reload.
        let _ = client_event_tx.send(ServerEvent::ReloadProgress {
            step: "skip".to_string(),
            message: "Server already running the newest binary; no reload needed.".to_string(),
            success: Some(true),
            output: None,
        });
        let _ = client_event_tx.send(ServerEvent::Done { id });
        return;
    }

    let request_id = crate::id::new_id("reload");
    mark_remote_reload_started(&request_id);

    let (triggering_session, prefer_selfdev_binary) = match agent.try_lock() {
        Ok(agent_guard) => (
            Some(agent_guard.session_id().to_string()),
            agent_guard.is_canary(),
        ),
        Err(_) => {
            crate::logging::warn(&format!(
                "SERVER_RELOAD_AGENT_BUSY request_id={} client_session_id={} fallback_triggering_session={} prefer_selfdev_binary=false",
                request_id, client_session_id, client_session_id
            ));
            (Some(client_session_id.to_string()), false)
        }
    };

    let live_sessions = {
        let members = swarm_members.read().await;
        members
            .iter()
            .filter_map(|(session_id, member)| {
                if member.event_txs.is_empty() {
                    None
                } else {
                    Some(session_id.clone())
                }
            })
            .collect::<Vec<_>>()
    };

    let mut delivered = 0;
    for session_id in &live_sessions {
        delivered += fanout_live_client_event(
            swarm_members,
            session_id,
            ServerEvent::Reloading { new_socket: None },
        )
        .await;
    }
    if delivered == 0 {
        let _ = client_event_tx.send(ServerEvent::Reloading { new_socket: None });
    }

    let hash = jcode_build_meta::git_hash().to_string();
    let signal_request_id =
        crate::server::send_reload_signal(hash, triggering_session.clone(), prefer_selfdev_binary);

    crate::logging::info(&format!(
        "handle_reload: queued reload signal {} from remote client request {} (triggering_session={:?}, prefer_selfdev_binary={}, reload_notified_sessions={}, reload_notified_clients={})",
        signal_request_id,
        request_id,
        triggering_session,
        prefer_selfdev_binary,
        live_sessions.len(),
        delivered
    ));

    let _ = client_event_tx.send(ServerEvent::Done { id });
}

#[allow(clippy::too_many_arguments)]
async fn cleanup_detached_source_session_if_unused(
    old_session_id: &str,
    client_connection_id: &str,
    source_agent: &Arc<Mutex<Agent>>,
    sessions: &SessionAgents,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    file_touch: &FileTouchService,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
) {
    unregister_session_event_sender(swarm_members, old_session_id, client_connection_id).await;

    if !remove_detached_source_if_unclaimed(
        old_session_id,
        client_connection_id,
        source_agent,
        sessions,
        client_connections,
    )
    .await
    {
        return;
    }

    {
        let mut agent_guard = source_agent.lock().await;
        agent_guard.mark_closed();
    }

    {
        let mut signals = shutdown_signals.write().await;
        signals.remove(old_session_id);
    }
    remove_background_tool_signal(old_session_id);
    remove_session_interrupt_queue(soft_interrupt_queues, old_session_id).await;
    remove_session_channel_subscriptions(
        old_session_id,
        channel_subscriptions,
        channel_subscriptions_by_session,
    )
    .await;
    file_touch.clear_session(old_session_id).await;

    let removed_swarm_id = {
        let mut members = swarm_members.write().await;
        members
            .remove(old_session_id)
            .and_then(|member| member.swarm_id)
    };
    if let Some(swarm_id) = removed_swarm_id {
        remove_session_from_swarm(
            old_session_id,
            &swarm_id,
            swarm_members,
            swarms_by_id,
            swarm_coordinators,
            swarm_plans,
        )
        .await;
    }
    match crate::session::remove_unpublished_session(old_session_id) {
        Ok(()) => sessions.release_provisional(old_session_id),
        Err(error) => crate::logging::error(&format!(
            "Unpublished primary cleanup failed for {old_session_id}: {error:#}"
        )),
    }
}

/// Removes a detached source only while holding the same connection-registry
/// write lock used to claim a live resume target. The connection registry is
/// the attachment authority, so the lock order for transitions is always
/// `client_connections` then `sessions`.
async fn remove_detached_source_if_unclaimed(
    old_session_id: &str,
    client_connection_id: &str,
    source_agent: &Arc<Mutex<Agent>>,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) -> bool {
    if !sessions.is_provisional(old_session_id) {
        return false;
    }
    let connections = client_connections.write().await;
    if connections
        .values()
        .any(|info| info.client_id != client_connection_id && info.session_id == old_session_id)
    {
        return false;
    }

    let mut sessions_guard = sessions.write().await;
    let owns_source = sessions_guard
        .get(old_session_id)
        .map(|existing| Arc::ptr_eq(existing, source_agent))
        .unwrap_or(false);
    if owns_source {
        sessions_guard.remove(old_session_id);
    }
    owns_source
}

/// Atomically reserves an existing live target for this connection.
///
/// Reserving under the connection write lock prevents another connection's
/// detached-source cleanup from observing no users after we have selected the
/// target but before our connection record is updated.
async fn claim_live_target_agent(
    session_id: &str,
    client_connection_id: &str,
    client_instance_id: Option<&str>,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) -> Option<Arc<Mutex<Agent>>> {
    let mut connections = client_connections.write().await;
    let sessions_guard = sessions.read().await;
    let target = sessions_guard.get(session_id).cloned()?;

    let info = connections.get_mut(client_connection_id)?;
    info.session_id = session_id.to_string();
    info.client_instance_id = client_instance_id.map(str::to_string);
    info.last_seen = Instant::now();
    Some(target)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_resume_session(
    id: u64,
    session_id: String,
    client_instance_id: Option<&str>,
    client_has_local_history: bool,
    allow_session_takeover: bool,
    client_selfdev: &mut bool,
    client_session_id: &mut String,
    client_connection_id: &str,
    agent: &Arc<Mutex<Agent>>,
    startup_context: &Arc<super::startup_context::StartupContextCoordinator>,
    provider: &Arc<dyn Provider>,
    instruction_repositories: &crate::instruction::InstructionRepositoryService,
    prepared_restore_status: Option<crate::session::SessionStatus>,
    sessions: &SessionAgents,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    file_touch: &FileTouchService,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    client_count: &Arc<RwLock<usize>>,
    writer: &Arc<Mutex<WriteHalf>>,
    server_name: &str,
    server_icon: &str,
    client_event_tx: &crate::client_delivery::ClientEventSender,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) -> Result<Arc<Mutex<Agent>>> {
    let resume_start = Instant::now();
    if crate::session::session_exists(&session_id) {
        crate::execution::retention::record_session_use(session_id.clone()).await?;
    }
    let incoming_client_instance_id = client_instance_id.map(str::to_string);
    crate::logging::event_info(
        "SESSION_LIFECYCLE",
        vec![
            ("phase", "resume_start".to_string()),
            ("request_id", id.to_string()),
            ("source_session_id", client_session_id.clone()),
            ("target_session_id", session_id.clone()),
            ("client_connection_id", client_connection_id.to_string()),
            (
                "client_instance_id",
                incoming_client_instance_id
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ),
            (
                "client_has_local_history",
                client_has_local_history.to_string(),
            ),
            ("allow_takeover", allow_session_takeover.to_string()),
        ],
    );
    let restored_status = match sessions
        .restore(&session_id, provider, mcp_pool, instruction_repositories)
        .await
    {
        Ok(status) => status.or(prepared_restore_status),
        Err(error) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to restore session: {error:#}"),
                retry_after_secs: None,
            });
            return Ok(agent.clone());
        }
    };
    let live_target_agent = claim_live_target_agent(
        &session_id,
        client_connection_id,
        incoming_client_instance_id.as_deref(),
        sessions,
        client_connections,
    )
    .await;

    if let Some(live_target_agent) = live_target_agent.as_ref() {
        let resources = sessions.resources(&session_id, live_target_agent)?;
        let provider = &resources.provider;
        let registry = &resources.registry;
        let mcp_working_dir = session_working_dir(live_target_agent, &session_id)?;
        let old_session_id = client_session_id.clone();

        let conflicting_live_client = {
            let connections = client_connections.read().await;
            connections
                .values()
                .find(|info| {
                    info.client_id != client_connection_id && info.session_id == session_id
                })
                .cloned()
        };
        let live_target_busy = live_target_agent.try_lock().is_err();
        crate::logging::info(&format!(
            "Resume attach to existing live session {} from temporary {} on connection {}: live_target_busy={}, conflict_owner={}, conflict_processing={}, allow_takeover={}, local_history={}, incoming_instance={:?}",
            session_id,
            old_session_id,
            client_connection_id,
            live_target_busy,
            conflicting_live_client
                .as_ref()
                .map(|info| info.client_id.as_str())
                .unwrap_or("<none>"),
            conflicting_live_client
                .as_ref()
                .map(|info| info.is_processing)
                .unwrap_or(false),
            allow_session_takeover,
            client_has_local_history,
            incoming_client_instance_id
        ));

        if old_session_id != session_id {
            cleanup_detached_source_session_if_unused(
                &old_session_id,
                client_connection_id,
                agent,
                sessions,
                shutdown_signals,
                soft_interrupt_queues,
                client_connections,
                swarm_members,
                swarms_by_id,
                file_touch,
                channel_subscriptions,
                channel_subscriptions_by_session,
                swarm_plans,
                swarm_coordinators,
            )
            .await;
        }

        if let Some(conflict) = conflicting_live_client {
            let incoming_instance_id = incoming_client_instance_id.as_deref();
            let existing_instance_id = conflict.client_instance_id.as_deref();
            let distinct_client_instances = incoming_instance_id
                .zip(existing_instance_id)
                .map(|(incoming, existing)| incoming != existing)
                .unwrap_or(false);
            let can_take_over_live_session =
                allow_session_takeover && client_has_local_history && !distinct_client_instances;

            if can_take_over_live_session {
                let (disconnect_tx, debug_client_id, transferred_processing, transferred_tool_name) = {
                    let mut connections = client_connections.write().await;
                    let removed = connections.remove(&conflict.client_id);
                    if let Some(info) = removed {
                        (
                            Some(info.disconnect_tx),
                            info.debug_client_id,
                            info.is_processing,
                            info.current_tool_name,
                        )
                    } else {
                        (
                            None,
                            conflict.debug_client_id,
                            conflict.is_processing,
                            conflict.current_tool_name,
                        )
                    }
                };
                if transferred_processing {
                    crate::logging::warn(&format!(
                        "Taking over live session {} from {} while old owner reports processing; new connection receives status/tool metadata but not the old processing task handle",
                        session_id, conflict.client_id
                    ));
                } else {
                    crate::logging::info(&format!(
                        "Taking over live session {} from idle owner {}",
                        session_id, conflict.client_id
                    ));
                }

                {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(client_connection_id) {
                        info.is_processing = transferred_processing;
                        info.current_tool_name = transferred_tool_name;
                    }
                }

                if let Some(debug_client_id) = debug_client_id.as_deref() {
                    let mut debug_state = client_debug_state.write().await;
                    debug_state.unregister(debug_client_id);
                }

                if let Some(disconnect_tx) = disconnect_tx {
                    let _ = disconnect_tx.send(());
                }
            }
        }

        let client_event_tx = &client_event_tx.begin_snapshot(&session_id, writer).await;
        ensure_client_swarm_member(
            &session_id,
            client_connection_id,
            &None,
            client_event_tx,
            live_target_agent,
            crate::config::config().features.swarm,
            swarm_members,
            swarms_by_id,
            event_history,
            event_counter,
            swarm_event_tx,
        )
        .await;
        if let Ok(agent) = live_target_agent.try_lock() {
            shutdown_signals
                .write()
                .await
                .insert(session_id.clone(), agent.graceful_shutdown_signal());
            register_session_interrupt_queue(
                soft_interrupt_queues,
                &session_id,
                agent.soft_interrupt_queue(),
            )
            .await;
        }
        let was_interrupted = restored_status.as_ref().and_then(|status| {
            live_target_agent
                .try_lock()
                .ok()
                .map(|agent| restored_session_was_interrupted(&session_id, status, &agent))
        });
        register_session_event_sender(
            swarm_members,
            &session_id,
            client_connection_id,
            client_event_tx.clone(),
        )
        .await;

        let is_canary = live_target_agent
            .try_lock()
            .ok()
            .map(|agent_guard| agent_guard.is_canary())
            .or_else(|| {
                crate::session::Session::load_startup_stub(&session_id)
                    .ok()
                    .map(|session| session.is_canary)
            })
            .unwrap_or(false);
        *client_selfdev = is_canary;
        if is_canary {
            registry.register_selfdev_tools().await;
        }

        if *client_session_id != session_id {
            startup_context.release_connection(client_connection_id);
        }
        *client_session_id = session_id.clone();

        handle_get_history(
            id,
            &session_id,
            false,
            live_target_agent,
            startup_context,
            provider,
            sessions,
            client_connections,
            client_count,
            writer,
            server_name,
            server_icon,
            was_interrupted,
            Some(client_event_tx),
        )
        .await?;
        for request in sessions.pending_stdin(&session_id) {
            let _ = client_event_tx.for_session(&session_id).send(request);
        }
        client_event_tx.finish_snapshot();
        let _ = client_event_tx.send(ServerEvent::Done { id });
        // Resolve project-local MCP config against the resumed session's
        // working dir, not the server process cwd (issue #420).
        // Do not block on the agent lock here: the target agent may be busy
        // mid-turn (lock held), and awaiting it would deadlock the resume.
        registry
            .register_mcp_tools_for_dir(
                Some(client_event_tx.clone()),
                Some(Arc::clone(mcp_pool)),
                Some(session_id.clone()),
                mcp_working_dir,
            )
            .await;
        spawn_model_prefetch_update(Arc::clone(provider), Arc::clone(live_target_agent));
        crate::logging::event_info(
            "SESSION_LIFECYCLE",
            vec![
                ("phase", "resume_live_attach_done".to_string()),
                ("request_id", id.to_string()),
                ("old_session_id", old_session_id),
                ("target_session_id", session_id.clone()),
                ("client_connection_id", client_connection_id.to_string()),
                ("live_target_busy", live_target_busy.to_string()),
                ("elapsed_ms", resume_start.elapsed().as_millis().to_string()),
            ],
        );
        return Ok(Arc::clone(live_target_agent));
    }

    let _ = client_event_tx.send(ServerEvent::Error {
        id,
        message: "Primary changed during attachment; retry against the current runtime".into(),
        retry_after_secs: Some(1),
    });
    Ok(Arc::clone(agent))
}

#[cfg(test)]
#[path = "client_session_tests.rs"]
mod tests;
