use super::available_models_dedup::available_models_dedup_key;
use super::client_actions::{
    NotifySessionContext, handle_input_shell, handle_notify_session, handle_rename_session,
    handle_run_subagent, handle_set_feature, handle_set_subagent_model, handle_split,
    handle_stdin_response, handle_transfer, handle_trigger_memory_extraction,
};
use super::client_comm::{
    handle_comm_channel_members, handle_comm_list, handle_comm_list_channels, handle_comm_message,
    handle_comm_read, handle_comm_share, handle_comm_subscribe_channel,
    handle_comm_unsubscribe_channel,
};
use super::client_disconnect_cleanup::cleanup_client_connection;
use super::client_lifecycle_logging::{
    ServerRequestLifecycleFields, interrupt_request_log_fields, protocol_type_from_line,
    request_payload_summary, request_type_is_read_only, server_request_lifecycle_fields,
};
use super::client_lightweight_control::{
    LightweightControlContext, handle_lightweight_control_request, parse_swarm_spawn_mode,
    unavailable_swarm_response,
};
use super::client_session::{
    handle_clear_session, handle_reload, handle_resume_session, handle_subscribe,
};
use super::client_state::{
    handle_get_compacted_history, handle_get_history, handle_get_model_catalog, handle_get_state,
};
use super::client_writer::write_direct_event;
use super::comm_await::{CommAwaitMembersContext, handle_comm_await_members};
use super::comm_control::{
    handle_client_debug_command, handle_client_debug_response, handle_comm_assign_next,
    handle_comm_assign_role, handle_comm_assign_task, handle_comm_task_control,
};
use super::comm_plan::{
    handle_comm_approve_plan, handle_comm_propose_plan, handle_comm_reject_plan,
};
use super::comm_session::{handle_comm_spawn, handle_comm_stop};
use super::comm_sync::{
    CommResyncPlanContext, handle_comm_plan_status, handle_comm_read_context,
    handle_comm_resync_plan, handle_comm_status, handle_comm_summary,
};
use super::context_control::{handle_set_context_emergency_policy, reject_legacy_context_request};
use super::provider_control::{
    handle_cycle_model, handle_notify_auth_changed, handle_refresh_models, handle_set_model,
    handle_set_premium_mode, handle_set_reasoning_effort, handle_set_route,
    handle_set_service_tier, handle_set_transport, handle_switch_anthropic_account,
    handle_switch_openai_account, try_available_models_updated_event,
};
use super::{
    AwaitMembersRuntime, ClientConnectionInfo, ClientDebugState, FileTouchService,
    SessionControlHandle, SessionInterruptQueues, SharedContext, SwarmEvent, SwarmMember,
    SwarmMutationRuntime, VersionedPlan, fanout_live_client_event,
    format_structured_completion_report, register_session_interrupt_queue,
    send_swarm_plan_to_session, truncate_detail, update_member_status,
    update_member_status_with_report_tldr,
};
use crate::agent::Agent;
use crate::bus::{Bus, BusEvent};
use crate::id;
use crate::protocol::{Request, ServerEvent, decode_request, encode_event};
use crate::provider::Provider;
use crate::session::Session;
use crate::tool::Registry;
use crate::transport::Stream;
use anyhow::{Context, Result};
use jcode_agent_runtime::{InterruptSignal, SoftInterruptSource};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

type SessionAgents = Arc<crate::primary::PrimaryHost>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;
const RELOAD_STARTING_GUARD_MAX_AGE: Duration = Duration::from_secs(30);
const REQUEST_HANDLER_STALL_THRESHOLDS_MS: [u64; 3] = [2_000, 10_000, 60_000];

struct FreshPrimaryRuntime {
    provider: Arc<dyn Provider>,
    friendly_name: Option<String>,
    is_selfdev: bool,
}

fn required_subscribe_working_dir(working_dir: Option<&str>) -> std::result::Result<&str, String> {
    let working_dir = working_dir
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| "Subscribe requires the client's working directory".to_string())?;
    if !Path::new(working_dir).is_absolute() {
        return Err("Subscribe working_dir must be an absolute path".to_string());
    }
    Ok(working_dir)
}

fn initial_subscribe_working_dir(request: &Request) -> std::result::Result<String, String> {
    match request {
        Request::Subscribe { working_dir, .. } => {
            required_subscribe_working_dir(working_dir.as_deref()).map(str::to_string)
        }
        _ => Err(
            "Client must Subscribe with a working_dir before sending stateful requests".to_string(),
        ),
    }
}

fn initial_subscribe_terminal_env(request: &Request) -> Vec<(String, String)> {
    match request {
        Request::Subscribe { terminal_env, .. } => terminal_env.clone(),
        _ => Vec::new(),
    }
}

fn initial_subscribe_target_session(request: &Request) -> Option<&str> {
    match request {
        Request::Subscribe {
            target_session_id, ..
        } => target_session_id.as_deref(),
        _ => None,
    }
}

fn initial_subscribe_agent(request: &Request) -> Option<&str> {
    match request {
        Request::Subscribe { agent, .. } => agent.as_deref(),
        _ => None,
    }
}

fn initial_subscribe_selfdev(request: &Request) -> bool {
    matches!(
        request,
        Request::Subscribe {
            selfdev: Some(true),
            ..
        }
    )
}

fn initial_subscribe_startup_caller(request: &Request) -> crate::agent::StartupContextCaller {
    match request {
        Request::Subscribe {
            startup_context_caller:
                Some(
                    crate::protocol::StartupContextPrimaryCaller::HarnessApiCreate
                    | crate::protocol::StartupContextPrimaryCaller::HarnessApiAttach,
                ),
            ..
        } => crate::agent::StartupContextCaller::HarnessApi,
        Request::Subscribe { .. } => crate::agent::StartupContextCaller::InteractiveTui,
        _ => crate::agent::StartupContextCaller::InteractiveTui,
    }
}

fn initial_subscribe_allows_fresh_fallback(request: &Request) -> bool {
    !matches!(
        request,
        Request::Subscribe {
            startup_context_caller: Some(
                crate::protocol::StartupContextPrimaryCaller::HarnessApiAttach
            ),
            ..
        }
    )
}

struct ProcessingMessage {
    queued_messages: Option<Vec<crate::todo::QueuedMessage>>,
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    observe_startup_context: bool,
    activate_skill: Option<String>,
}

struct ProcessingState<'a> {
    client_is_processing: &'a mut bool,
    message_id: &'a mut Option<u64>,
    session_id: &'a mut Option<String>,
}

struct SwarmStatusRefs<'a> {
    members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: &'a Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &'a Arc<std::sync::atomic::AtomicU64>,
    event_tx: &'a broadcast::Sender<SwarmEvent>,
}

struct RequestHandlerWatchdog {
    done: Arc<AtomicBool>,
}

struct RequestHandlerWatchdogContext {
    request_id: u64,
    request_kind: String,
    client_session_id: String,
    client_connection_id: String,
    client_instance_id: Option<String>,
    client_is_processing: bool,
    message_id: Option<u64>,
    processing_session_id: Option<String>,
    line_bytes: usize,
    lifecycle_logged: bool,
}

impl RequestHandlerWatchdog {
    fn spawn(ctx: RequestHandlerWatchdogContext) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let done_for_task = Arc::clone(&done);
        tokio::spawn(async move {
            let started = Instant::now();
            let mut previous_threshold = Duration::ZERO;
            for threshold_ms in REQUEST_HANDLER_STALL_THRESHOLDS_MS {
                let threshold = Duration::from_millis(threshold_ms);
                tokio::time::sleep(threshold.saturating_sub(previous_threshold)).await;
                previous_threshold = threshold;
                if done_for_task.load(Ordering::Acquire) {
                    return;
                }
                crate::logging::event_warn(
                    "SERVER_REQUEST_HANDLER_STALLED",
                    vec![
                        ("request_id", ctx.request_id.to_string()),
                        ("request_kind", ctx.request_kind.clone()),
                        ("session_id", ctx.client_session_id.clone()),
                        ("client_connection_id", ctx.client_connection_id.clone()),
                        (
                            "client_instance_id",
                            ctx.client_instance_id
                                .clone()
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        ("client_processing", ctx.client_is_processing.to_string()),
                        (
                            "message_id",
                            ctx.message_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        (
                            "processing_session_id",
                            ctx.processing_session_id
                                .clone()
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        ("line_bytes", ctx.line_bytes.to_string()),
                        ("lifecycle_logged", ctx.lifecycle_logged.to_string()),
                        ("threshold_ms", threshold_ms.to_string()),
                        ("elapsed_ms", started.elapsed().as_millis().to_string()),
                    ],
                );
            }
        });
        Self { done }
    }
}

impl Drop for RequestHandlerWatchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

fn log_request_lifecycle_handled(
    fields: ServerRequestLifecycleFields<'_>,
    request_lifecycle_start: Instant,
    request_decoded_at: Instant,
) {
    let mut fields = server_request_lifecycle_fields(fields);
    fields.push((
        "handler_total_ms".to_string(),
        request_lifecycle_start.elapsed().as_millis().to_string(),
    ));
    fields.push((
        "since_decode_ms".to_string(),
        request_decoded_at.elapsed().as_millis().to_string(),
    ));
    crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
}

fn reject_if_agent_busy_for_request(
    request_id: u64,
    request_kind: &'static str,
    client_session_id: &str,
    client_is_processing: bool,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) -> bool {
    if agent.try_lock().is_ok() {
        return false;
    }

    send_agent_busy_error(
        request_id,
        request_kind,
        client_session_id,
        client_is_processing,
        client_event_tx,
    );
    true
}

fn send_agent_busy_error(
    request_id: u64,
    request_kind: &'static str,
    client_session_id: &str,
    client_is_processing: bool,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    crate::logging::event_warn(
        "SERVER_REQUEST_BUSY_AGENT_REJECTED",
        vec![
            ("request_id", request_id.to_string()),
            ("request_kind", request_kind.to_string()),
            ("session_id", client_session_id.to_string()),
            ("client_processing", client_is_processing.to_string()),
            ("reason", "agent_busy".to_string()),
        ],
    );
    let _ = client_event_tx.send(ServerEvent::Error {
        id: request_id,
        message: format!(
            "Cannot handle {request_kind} while the session is busy. Try again after the current turn finishes."
        ),
        retry_after_secs: Some(1),
    });
}

fn try_lock_idle_agent_for_request<'a>(
    request_id: u64,
    request_kind: &'static str,
    client_session_id: &str,
    client_is_processing: bool,
    agent: &'a Arc<Mutex<Agent>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) -> Option<tokio::sync::MutexGuard<'a, Agent>> {
    if client_is_processing {
        send_agent_busy_error(
            request_id,
            request_kind,
            client_session_id,
            true,
            client_event_tx,
        );
        return None;
    }
    match agent.try_lock() {
        Ok(agent_guard) => Some(agent_guard),
        Err(_) => {
            send_agent_busy_error(
                request_id,
                request_kind,
                client_session_id,
                false,
                client_event_tx,
            );
            None
        }
    }
}

pub(crate) fn server_reload_starting() -> bool {
    matches!(
        crate::server::recent_reload_state(RELOAD_STARTING_GUARD_MAX_AGE),
        Some(state) if state.phase == crate::server::ReloadPhase::Starting
    )
}

async fn refresh_session_control_handle(
    session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
) -> SessionControlHandle {
    let started = Instant::now();
    let agent_guard = match agent.try_lock() {
        Ok(agent_guard) => agent_guard,
        Err(_) => {
            crate::logging::warn(&format!(
                "refresh_session_control_handle: waiting for busy agent lock for session {}; cancel/control requests on this connection may be delayed",
                session_id
            ));
            let fallback_stop_signal = shutdown_signals.read().await.get(session_id).cloned();
            let fallback_soft_interrupt_queue =
                soft_interrupt_queues.read().await.get(session_id).cloned();
            if let Some(soft_interrupt_queue) = fallback_soft_interrupt_queue {
                // A missing shutdown-signal registration (e.g. a session created
                // through the headless spawn path) must not force this connection
                // to block on the busy agent mutex: cancels fired through the
                // handle fan out to the running turn's own signal via the
                // turn-cancel registry (issue #428), so a detached signal is a
                // safe stand-in.
                let stop_signal = fallback_stop_signal.unwrap_or_else(|| {
                    crate::logging::warn(&format!(
                        "refresh_session_control_handle: no registered shutdown signal for busy session {}; using detached signal (cancels reach the running turn via the turn cancel registry)",
                        session_id
                    ));
                    InterruptSignal::new()
                });
                crate::logging::warn(&format!(
                    "refresh_session_control_handle: using lock-free cancel-only control handle for busy session {} after {}ms",
                    session_id,
                    started.elapsed().as_millis()
                ));
                return SessionControlHandle::cancel_only(
                    session_id,
                    soft_interrupt_queue,
                    stop_signal,
                );
            }
            let agent_guard = agent.lock().await;
            crate::logging::warn(&format!(
                "refresh_session_control_handle: acquired agent lock for session {} after {}ms",
                session_id,
                started.elapsed().as_millis()
            ));
            agent_guard
        }
    };
    SessionControlHandle::new(
        session_id,
        agent_guard.soft_interrupt_queue(),
        agent_guard.background_tool_signal(),
        agent_guard.graceful_shutdown_signal(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "client lifecycle wiring spans sessions, swarm state, file state, channels, debug, and runtime coordination"
)]
#[cfg(test)]
pub(super) async fn handle_client(
    stream: Stream,
    sessions: SessionAgents,
    global_event_tx: broadcast::Sender<ServerEvent>,
    provider_template: Arc<dyn Provider>,
    context_transactions: Arc<crate::context::ContextTransactionService>,
    startup_context: Arc<super::startup_context::StartupContextCoordinator>,
    global_is_processing: Arc<RwLock<bool>>,
    global_session_id: Arc<RwLock<String>>,
    client_count: Arc<RwLock<usize>>,
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    shared_context: Arc<RwLock<HashMap<String, HashMap<String, SharedContext>>>>,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    file_touch: FileTouchService,
    channel_subscriptions: ChannelSubscriptions,
    channel_subscriptions_by_session: ChannelSubscriptions,
    client_debug_state: Arc<RwLock<ClientDebugState>>,
    client_debug_response_tx: broadcast::Sender<(u64, String)>,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    server_name: String,
    server_icon: String,
    mcp_pool: Arc<crate::mcp::SharedMcpPool>,
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: SessionInterruptQueues,
    await_members_runtime: AwaitMembersRuntime,
    swarm_mutation_runtime: SwarmMutationRuntime,
) -> Result<()> {
    handle_client_with_instruction_repositories(
        stream,
        sessions,
        global_event_tx,
        provider_template,
        context_transactions,
        startup_context,
        Arc::new(crate::instruction::InstructionRepositoryService::new()),
        global_is_processing,
        global_session_id,
        client_count,
        client_connections,
        swarm_members,
        swarms_by_id,
        shared_context,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        channel_subscriptions,
        channel_subscriptions_by_session,
        client_debug_state,
        client_debug_response_tx,
        event_history,
        event_counter,
        swarm_event_tx,
        server_name,
        server_icon,
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        await_members_runtime,
        swarm_mutation_runtime,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "client lifecycle wiring spans sessions, swarm state, file state, channels, debug, and runtime coordination"
)]
pub(super) async fn handle_client_with_instruction_repositories(
    stream: Stream,
    sessions: SessionAgents,
    _global_event_tx: broadcast::Sender<ServerEvent>,
    provider_template: Arc<dyn Provider>,
    context_transactions: Arc<crate::context::ContextTransactionService>,
    startup_context: Arc<super::startup_context::StartupContextCoordinator>,
    instruction_repositories: Arc<crate::instruction::InstructionRepositoryService>,
    _global_is_processing: Arc<RwLock<bool>>,
    global_session_id: Arc<RwLock<String>>,
    client_count: Arc<RwLock<usize>>,
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    shared_context: Arc<RwLock<HashMap<String, HashMap<String, SharedContext>>>>,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    file_touch: FileTouchService,
    channel_subscriptions: ChannelSubscriptions,
    channel_subscriptions_by_session: ChannelSubscriptions,
    client_debug_state: Arc<RwLock<ClientDebugState>>,
    client_debug_response_tx: broadcast::Sender<(u64, String)>,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    server_name: String,
    server_icon: String,
    mcp_pool: Arc<crate::mcp::SharedMcpPool>,
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: SessionInterruptQueues,
    await_members_runtime: AwaitMembersRuntime,
    swarm_mutation_runtime: SwarmMutationRuntime,
) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));
    let mut line = String::new();

    let mut primary_stream_enabled = false;
    let initial_request = loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(error) => {
                crate::logging::error(&format!(
                    "Client read error before initialization: {}",
                    error
                ));
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }

        match decode_request(&line) {
            Ok(request) => {
                if let Request::PrimaryStreamSubscribe { id } = &request {
                    primary_stream_enabled = true;
                    write_direct_event(
                        &writer,
                        &ServerEvent::PrimaryStreamCapabilities {
                            id: *id,
                            version: 1,
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::PrimaryLaunchProbe { id } = &request {
                    write_direct_event(
                        &writer,
                        &ServerEvent::PrimaryLaunchCapabilities {
                            id: *id,
                            version: 1,
                            enabled: crate::primary::launch_enabled(),
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::PrimaryLaunch { id, request } = &request {
                    let response = sessions
                        .request_launch(
                            *request.clone(),
                            provider_template.clone(),
                            mcp_pool.clone(),
                            (*instruction_repositories).clone(),
                        )
                        .await;
                    write_direct_event(
                        &writer,
                        &ServerEvent::PrimaryLaunchResponse {
                            id: *id,
                            response: Box::new(response),
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::NotifySession {
                    id,
                    session_id,
                    message,
                    unattended_context,
                } = &request
                {
                    let (tx, mut rx) = crate::client_delivery::local_event_channel();
                    handle_notify_session(
                        *id,
                        session_id.clone(),
                        message.clone(),
                        unattended_context.clone(),
                        NotifySessionContext {
                            sessions: &sessions,
                            soft_interrupt_queues: &soft_interrupt_queues,
                            swarm_members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            swarm_event_tx: &swarm_event_tx,
                            client_event_tx: &tx,
                        },
                    )
                    .await;
                    drop(tx);
                    while let Some(event) = rx.recv().await {
                        write_direct_event(&writer, &event).await?;
                    }
                    continue;
                }
                if let Request::WorkspaceProbe { id } = &request {
                    write_direct_event(
                        &writer,
                        &ServerEvent::WorkspaceCapabilities {
                            id: *id,
                            catalog_version: 1,
                            managed_rollout: false,
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::Workspace { id, request } = &request {
                    let response = crate::workspace::dispatch(*request.clone()).await;
                    write_direct_event(
                        &writer,
                        &ServerEvent::WorkspaceResponse {
                            id: *id,
                            response: Box::new(response),
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::TaskMonitorProbe { id } = &request {
                    write_direct_event(
                        &writer,
                        &ServerEvent::TaskMonitorCapabilities {
                            id: *id,
                            version: 1,
                            child_context: true,
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::ChildContext {
                    id,
                    child_id,
                    request,
                } = &request
                {
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    let (id, target, request) = (*id, child_id.clone(), request.clone());
                    let service = context_transactions.clone();
                    let repositories = (*instruction_repositories).clone();
                    let route_target = target.clone();
                    let operation = tokio::spawn(async move {
                        let result = if request.id() != id {
                            Err(anyhow::anyhow!("Child context correlation mismatch"))
                        } else {
                            super::child_context::handle(
                                route_target,
                                *request,
                                service,
                                repositories,
                                tx.clone().into(),
                            )
                            .await
                        };
                        if let Err(error) = result {
                            let _ = tx.send(ServerEvent::Error {
                                id,
                                message: format!("Child context: {error:#}"),
                                retry_after_secs: None,
                            });
                        }
                    });
                    while let Some(event) = rx.recv().await {
                        write_direct_event(
                            &writer,
                            &ServerEvent::ChildContextResponse {
                                id,
                                child_id: target.clone(),
                                event: Box::new(event),
                            },
                        )
                        .await?;
                    }
                    operation.await?;
                    return Ok(());
                }
                if let Request::DelegationProbe { id } = &request {
                    let namespace = crate::storage::jcode_dir()?.canonicalize()?;
                    write_direct_event(
                        &writer,
                        &ServerEvent::DelegationCapabilities {
                            id: *id,
                            version: 1,
                            namespace: namespace
                                .to_str()
                                .ok_or_else(|| {
                                    anyhow::anyhow!("Delegation namespace must be UTF-8")
                                })?
                                .to_string(),
                        },
                    )
                    .await?;
                    continue;
                }
                if let Request::DelegationExecute { id, invocation } = request {
                    let host = Arc::new(crate::delegation::Host::new(
                        provider_template.clone(),
                        mcp_pool.clone(),
                        (*instruction_repositories).clone(),
                    )?);
                    if let Err(error) = crate::delegation::serve_request(
                        &mut reader,
                        writer.clone(),
                        id,
                        *invocation,
                        host,
                    )
                    .await
                    {
                        write_direct_event(
                            &writer,
                            &ServerEvent::Error {
                                id,
                                message: format!("Hosted delegation failed: {error:#}"),
                                retry_after_secs: None,
                            },
                        )
                        .await?;
                    }
                    return Ok(());
                }
                if let Some(error) = unavailable_swarm_response(&request) {
                    write_direct_event(&writer, &ServerEvent::Ack { id: request.id() }).await?;
                    write_direct_event(&writer, &error).await?;
                    return Ok(());
                }
                if request.is_lightweight_control_request() {
                    handle_lightweight_control_request(
                        request,
                        Arc::clone(&writer),
                        LightweightControlContext {
                            sessions: &sessions,
                            global_session_id: &global_session_id,
                            provider_template: &provider_template,
                            swarm_members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            shared_context: &shared_context,
                            swarm_plans: &swarm_plans,
                            swarm_coordinators: &swarm_coordinators,
                            file_touch: &file_touch,
                            channel_subscriptions: &channel_subscriptions,
                            channel_subscriptions_by_session: &channel_subscriptions_by_session,
                            client_connections: &client_connections,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            swarm_event_tx: &swarm_event_tx,
                            mcp_pool: &mcp_pool,
                            soft_interrupt_queues: &soft_interrupt_queues,
                            await_members_runtime: &await_members_runtime,
                            swarm_mutation_runtime: &swarm_mutation_runtime,
                        },
                    )
                    .await?;
                    return Ok(());
                }
                break request;
            }
            Err(error) => {
                write_direct_event(
                    &writer,
                    &ServerEvent::Error {
                        id: 0,
                        message: format!("Invalid request: {}", error),
                        retry_after_secs: None,
                    },
                )
                .await?;
            }
        }
    };

    let initial_working_dir = match initial_subscribe_working_dir(&initial_request) {
        Ok(working_dir) => working_dir,
        Err(message) => {
            write_direct_event(
                &writer,
                &ServerEvent::Error {
                    id: initial_request.id(),
                    message,
                    retry_after_secs: None,
                },
            )
            .await?;
            return Ok(());
        }
    };
    let mut active_terminal_env = initial_subscribe_terminal_env(&initial_request);

    // Per-client state
    let mut primary_changes = sessions.subscribe();
    let mut current_client_instance_id: Option<String> = None;
    // Client selfdev status is determined by Subscribe request, not server's env
    let mut client_selfdev = false;

    let client_start = std::time::Instant::now();

    let requested_target = initial_subscribe_target_session(&initial_request);
    let target_available = match requested_target {
        Some(target) => {
            sessions.read().await.contains_key(target) || crate::session::session_exists(target)
        }
        None => false,
    };
    let initial_startup_caller = initial_subscribe_startup_caller(&initial_request);
    let allow_fresh_fallback = initial_subscribe_allows_fresh_fallback(&initial_request);
    if requested_target.is_some() && !target_available && !allow_fresh_fallback {
        write_direct_event(
            &writer,
            &ServerEvent::Error {
                id: initial_request.id(),
                message:
                    "Target session is unavailable; attach did not create a replacement session"
                        .into(),
                retry_after_secs: None,
            },
        )
        .await?;
        return Ok(());
    }
    let mut initial_restore_status = None;
    let (mut agent, mut provider, mut registry, mut client_session_id, mut friendly_name) =
        if let Some(target) = requested_target.filter(|_| target_available) {
            match sessions
                .restore(
                    target,
                    &provider_template,
                    &mcp_pool,
                    &instruction_repositories,
                )
                .await
            {
                Ok(status) => initial_restore_status = status,
                Err(error) => {
                    write_direct_event(
                        &writer,
                        &ServerEvent::Error {
                            id: initial_request.id(),
                            message: format!("Failed to restore session: {error:#}"),
                            retry_after_secs: None,
                        },
                    )
                    .await?;
                    return Ok(());
                }
            }
            let agent = sessions
                .read()
                .await
                .get(target)
                .cloned()
                .context("Restored primary is unavailable")?;
            let resources = sessions.resources(target, &agent)?;
            let name = if let Ok(agent) = agent.try_lock() {
                agent.session_short_name().map(str::to_string)
            } else {
                crate::session::Session::load_startup_stub(target)?.short_name
            };
            (
                agent,
                resources.provider,
                resources.registry,
                target.to_string(),
                name,
            )
        } else {
            let selection = match crate::instruction::AgentSelection::parse(
                initial_subscribe_agent(&initial_request),
            ) {
                Ok(selection) => selection,
                Err(error) => {
                    write_direct_event(
                        &writer,
                        &ServerEvent::Error {
                            id: initial_request.id(),
                            message: format!("Invalid initial agent selection: {error}"),
                            retry_after_secs: None,
                        },
                    )
                    .await?;
                    return Ok(());
                }
            };
            let provider = provider_template.fork_for_new_session();
            let registry = Registry::new_for_shared_session(
                provider.clone(),
                mcp_pool.clone(),
                (*instruction_repositories).clone(),
            )
            .await?;
            let prepared =
                crate::hooks::with_client_terminal_env(active_terminal_env.clone(), async {
                    Agent::new_with_startup_context_and_agent_with_repositories(
                        provider.clone(),
                        registry.clone(),
                        Some(&initial_working_dir),
                        crate::agent::StartupContextActivation::primary(initial_startup_caller),
                        selection,
                        initial_subscribe_selfdev(&initial_request),
                        (*instruction_repositories).clone(),
                    )
                })
                .await;
            let (mut prepared, _) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    let event = if matches!(
                        error,
                        crate::agent::StartupContextActivationError::Instruction { .. }
                    ) {
                        ServerEvent::Error {
                            id: initial_request.id(),
                            message: format!("Initial agent activation failed: {error}"),
                            retry_after_secs: None,
                        }
                    } else {
                        ServerEvent::StartupContextFailed {
                            id: initial_request.id(),
                            failure: super::startup_context::primary_activation_failure(&error),
                        }
                    };
                    write_direct_event(&writer, &event).await?;
                    return Ok(());
                }
            };
            prepared.set_memory_enabled(crate::config::config().features.memory);
            let id = prepared.session_id().to_string();
            let name = prepared.session_short_name().map(str::to_string);
            if let Err(error) = sessions.adopt_owner(&prepared) {
                prepared.mark_closed();
                crate::tool::clear_session_tool_policy(&id);
                crate::session::remove_unpublished_session(&id)?;
                return Err(error);
            }
            let agent = Arc::new(Mutex::new(prepared));
            sessions.write().await.insert(id.clone(), agent.clone());
            (agent, provider, registry, id, name)
        };
    let mut swarm_enabled = crate::config::config().features.swarm;
    let mut last_available_models_snapshot: Option<String> = None;
    const MAX_LIVE_AVAILABLE_MODELS_UPDATE_BYTES: usize = 64 * 1024;
    let mut client_primary_startup_activated = true;
    crate::logging::info(&format!(
        "[TIMING] handle_client prepared primary: existing={}, total={}ms",
        target_available,
        client_start.elapsed().as_millis()
    ));
    let client_connection_id = id::new_id("conn");
    let connected_at = Instant::now();
    let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel::<()>();

    {
        let mut connections = client_connections.write().await;
        connections.insert(
            client_connection_id.clone(),
            ClientConnectionInfo {
                client_id: client_connection_id.clone(),
                session_id: client_session_id.clone(),
                client_instance_id: None,
                debug_client_id: None,
                connected_at,
                last_seen: connected_at,
                is_processing: sessions.processing(&client_session_id).is_some(),
                current_tool_name: None,
                terminal_env: active_terminal_env.clone(),
                disconnect_tx: disconnect_tx.clone(),
            },
        );
    }

    {
        let mut current = global_session_id.write().await;
        if current.is_empty() || *current != client_session_id {
            *current = client_session_id.clone();
        }
    }

    let resources = sessions.resources(&client_session_id, &agent)?;
    let mut session_control = SessionControlHandle::new(
        &client_session_id,
        resources.interrupts.clone(),
        resources.background,
        resources.shutdown,
    );
    shutdown_signals.write().await.insert(
        client_session_id.clone(),
        session_control.stop_current_turn_signal(),
    );
    register_session_interrupt_queue(
        &soft_interrupt_queues,
        &client_session_id,
        resources.interrupts,
    )
    .await;
    crate::runtime_memory_log::emit_event(
        crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
            "session_attached",
            "prepared_primary_attached",
        )
        .with_session_id(client_session_id.clone())
        .force_attribution(),
    );

    let (client_event_tx, mut client_event_rx, delivery_cancelled) =
        crate::client_delivery::ClientEventSender::bounded_client();
    let _delivery_owner = delivery_cancelled.clone().drop_guard();
    if primary_stream_enabled {
        client_event_tx.enable_primary_stream();
    }
    client_event_tx.retarget(&client_session_id);

    let writer_clone = Arc::clone(&writer);
    let client_connection_id_for_events = client_connection_id.clone();
    let client_connections_for_events = Arc::clone(&client_connections);
    let delivery_stop = delivery_cancelled.clone();
    let event_handle = tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                biased;
                _ = delivery_stop.cancelled() => break,
                frame = client_event_rx.recv() => match frame { Some(frame) => frame, None => break },
            };
            let result: std::io::Result<bool> = tokio::select! {
                biased;
                _ = delivery_stop.cancelled() => break,
                result = async {
                    loop {
                        frame.ready().await;
                        let mut writer = writer_clone.lock().await;
                        if !frame.is_current() { return Ok(false); }
                        if frame.paused() { continue; }
                        super::client_writer::write_bytes(&mut *writer,frame.json.as_bytes()).await?;
                        frame.written();
                        return Ok(true);
                    }
                } => result,
            };
            match result {
                Ok(false) => continue,
                Ok(true) => {}
                Err(_) => {
                    crate::logging::warn(&format!(
                        "Client delivery disconnected: connection={} type={} bytes={}",
                        client_connection_id_for_events,
                        protocol_type_from_line(&frame.json),
                        frame.json.len()
                    ));
                    delivery_stop.cancel();
                    break;
                }
            }
            let mut connections = client_connections_for_events.write().await;
            if frame.is_current()
                && let Some(info) = connections.get_mut(&client_connection_id_for_events)
            {
                match &frame.event {
                    ServerEvent::ToolStart { name, .. } => {
                        info.is_processing = true;
                        info.current_tool_name = Some(name.clone());
                    }
                    ServerEvent::ToolDone { .. } => info.current_tool_name = None,
                    ServerEvent::Done { .. }
                    | ServerEvent::Error { .. }
                    | ServerEvent::Interrupted => {
                        info.is_processing = false;
                        info.current_tool_name = None;
                    }
                    _ => {}
                }
            }
        }
    });

    // Note: Don't send initial SessionId here - it's sent by the Subscribe handler
    // Sending it via the channel causes race conditions where it can arrive after
    // other events (like History) that are written directly to the socket.

    // Set up client debug command channel
    // This client becomes the "active" debug client that receives client: commands
    let (debug_cmd_tx, mut debug_cmd_rx) = mpsc::unbounded_channel::<(u64, String)>();
    let client_debug_id = id::new_id("client");
    {
        let mut debug_state = client_debug_state.write().await;
        debug_state.register(client_debug_id.clone(), debug_cmd_tx);
    }
    {
        let mut connections = client_connections.write().await;
        if let Some(info) = connections.get_mut(&client_connection_id) {
            info.debug_client_id = Some(client_debug_id.clone());
        }
    }

    // Subscribe to bus events so we can forward ModelsUpdated to this client
    // (e.g. when Copilot finishes async init after the initial History was sent)
    let mut bus_rx = Bus::global().subscribe();

    // Do not drain global bus traffic until the client has completed its first
    // subscribe. Under heavy swarm file-activity load, ignored bus frames can
    // otherwise monopolize the select loop before the initial subscribe/read.
    let mut client_subscribed = false;
    let mut pending_request = Some(initial_request);
    let mut instruction_inspection = crate::instruction::inspection::InspectionWorker::default();
    let instruction_management =
        crate::instruction::management::InstructionManagementWorker::default();
    let mut inspection_requests = tokio::task::JoinSet::new();

    let client_result: Result<()> = async {
    loop {
        while let Some(result) = inspection_requests.try_join_next() {
            if let Err(error) = result {
                crate::logging::warn(&format!(
                    "Inspection request task ended unexpectedly: {error}"
                ));
            }
        }
        let request = if let Some(request) = pending_request.take() {
            request
        } else {
            line.clear();
            tokio::select! {
            biased;
            // Prioritize direct client I/O so subscribe/ping/message requests do not get
            // starved behind noisy background bus traffic.
            n = reader.read_line(&mut line) => {
                let n = match n {
                    Ok(n) => n,
                    Err(e) => {
                        crate::logging::error(&format!("Client read error: {}", e));
                        break;
                    }
                };
                if n == 0 {
                    break; // Client disconnected
                }
                let mut connections = client_connections.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id) {
                    info.last_seen = Instant::now();
                }
            }
            changed = primary_changes.changed() => {
                if changed.is_err() { break; }
                let client_is_processing = sessions.processing(&client_session_id).is_some();
                let mut connections = client_connections.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id) {
                    info.is_processing = client_is_processing;
                    if !client_is_processing { info.current_tool_name = None; }
                }
                continue;
            }
            _ = delivery_cancelled.cancelled() => { break; }
            disconnect_signal = disconnect_rx.recv() => {
                if disconnect_signal.is_some() {
                    crate::logging::info(&format!(
                        "Client connection {} was superseded; disconnecting old owner of session {}",
                        client_connection_id, client_session_id
                    ));
                    break;
                }
                continue;
            }
            // Forward bus events to this client
            bus_event = bus_rx.recv(), if client_subscribed => {
                match bus_event {
                    Ok(BusEvent::ModelsUpdated) => {
                        let Some(event) = try_available_models_updated_event(&agent) else {
                            crate::logging::info(&format!(
                                "Skipping ModelsUpdated push for busy connection {}",
                                client_connection_id
                            ));
                            continue;
                        };
                        // Compare on an age-insensitive key: route details carry
                        // cosmetic "12m ago" cache ages that tick on their own,
                        // and a raw byte compare treated that drift as a real
                        // catalog change, fanning a full repaint out to every
                        // connected client.
                        let dedup_key = available_models_dedup_key(&event);
                        if last_available_models_snapshot.as_ref() == Some(&dedup_key) {
                            continue;
                        }
                        let encoded_len = crate::protocol::encode_event(&event).len();
                        if encoded_len > MAX_LIVE_AVAILABLE_MODELS_UPDATE_BYTES {
                            // Don't drop the catalog update entirely: clients still
                            // need fresh model names for the picker. Strip the heavy
                            // route expansion and ship a names-only snapshot; the TUI
                            // rebuilds fallback routes for missing models locally.
                            let slim_event = names_only_available_models_event(&event);
                            let slim_encoded =
                                slim_event.as_ref().map(crate::protocol::encode_event);
                            match (slim_event, slim_encoded) {
                                (Some(slim_event), Some(slim_encoded))
                                    if slim_encoded.len()
                                        <= MAX_LIVE_AVAILABLE_MODELS_UPDATE_BYTES =>
                                {
                                    crate::logging::info(&format!(
                                        "Downgrading oversized bus AvailableModelsUpdated frame to names-only for connection {} ({} -> {} bytes)",
                                        client_connection_id,
                                        encoded_len,
                                        slim_encoded.len()
                                    ));
                                    let _ = client_event_tx.send(slim_event);
                                }
                                _ => {
                                    crate::logging::warn(&format!(
                                        "Skipping oversized bus AvailableModelsUpdated frame for connection {} ({} bytes)",
                                        client_connection_id, encoded_len
                                    ));
                                }
                            }
                            last_available_models_snapshot = Some(dedup_key);
                            continue;
                        }
                        let _ = client_event_tx.send(event);
                        last_available_models_snapshot = Some(dedup_key);
                    }
                    Ok(BusEvent::BatchProgress(progress)) => {
                        if progress.session_id == client_session_id {
                            let _ = client_event_tx.send(ServerEvent::BatchProgress { progress });
                        }
                    }
                    Ok(BusEvent::SidePanelUpdated(update)) => {
                        if update.session_id == client_session_id {
                            let _ = client_event_tx.send(ServerEvent::SidePanelState {
                                snapshot: update.snapshot,
                            });
                        }
                    }
                    _ => {}
                }
                continue;
            }
            // Handle client debug commands from debug socket
            debug_cmd = debug_cmd_rx.recv() => {
                if let Some((request_id, command)) = debug_cmd
                    && client_event_tx
                        .send(ServerEvent::ClientDebugRequest {
                            id: request_id,
                            command,
                        })
                        .is_err()
                {
                    let _ = client_debug_response_tx.send((
                        request_id,
                        "No TUI client connected".to_string(),
                    ));
                }
                continue;
            }
            }

            match decode_request(&line) {
                Ok(r) => r,
                Err(e) => {
                    let event = ServerEvent::Error {
                        id: 0,
                        message: format!("Invalid request: {}", e),
                        retry_after_secs: None,
                    };
                    let json = encode_event(&event);
                    let mut w = writer.lock().await;
                    if super::client_writer::write_bytes(&mut *w,json.as_bytes()).await.is_err() {
                        break;
                    }
                    continue;
                }
            }
        };
        let mut processing_message_id = sessions.processing(&client_session_id);
        let resources = sessions.resources(&client_session_id, &agent)?;
        provider = resources.provider;
        registry = resources.registry;
        let mut client_is_processing = processing_message_id.is_some();
        let mut processing_session_id = processing_message_id.map(|_| client_session_id.clone());
        let request_decoded_at = Instant::now();
        let request_id = request.id();
        let request_kind = protocol_type_from_line(&line);
        let request_lifecycle_logged = !request_type_is_read_only(&request_kind);
        let request_lifecycle_start = Instant::now();
        let _request_watchdog = RequestHandlerWatchdog::spawn(RequestHandlerWatchdogContext {
            request_id,
            request_kind: request_kind.clone(),
            client_session_id: client_session_id.clone(),
            client_connection_id: client_connection_id.clone(),
            client_instance_id: current_client_instance_id.clone(),
            client_is_processing,
            message_id: processing_message_id,
            processing_session_id: processing_session_id.clone(),
            line_bytes: line.len(),
            lifecycle_logged: request_lifecycle_logged,
        });
        if request_lifecycle_logged {
            let mut fields = server_request_lifecycle_fields(ServerRequestLifecycleFields {
                phase: "received",
                request_id,
                request_kind: &request_kind,
                client_session_id: &client_session_id,
                client_connection_id: &client_connection_id,
                client_instance_id: current_client_instance_id.as_deref(),
                client_is_processing,
                message_id: processing_message_id,
                processing_session_id: processing_session_id.as_deref(),
                line_bytes: line.len(),
            });
            fields.extend(request_payload_summary(&request_kind, &line));
            crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
        }
        if let Some(fields) = interrupt_request_log_fields(
            &request,
            &client_session_id,
            client_is_processing,
            processing_message_id,
            sessions.processing(&client_session_id).is_some(),
            line.len(),
        ) {
            crate::logging::info(&format!("SERVER_INTERRUPT_REQUEST_DECODED {}", fields));
        }

        // A cancellation request must never be gated on writing an Ack to the client.
        // The normal Ack path takes the shared outbound writer before dispatching the
        // request. During heavy streaming, history replay, or client-side backpressure,
        // that writer can be busy long enough that an already-decoded cancel would sit
        // behind outbound bytes instead of signalling the agent's lock-free cancel
        // handle. Queue the Ack through the event channel and signal cancellation first.
        if let Request::Cancel { id } = request {
            let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_PRE_ACK_DISPATCH id={} session={} ack_queued={} decoded_to_dispatch_ms={}",
                id,
                client_session_id,
                ack_queued,
                request_decoded_at.elapsed().as_millis()
            ));
            let cancel_dispatch_start = Instant::now();
            cancel_processing_message(
                &mut ProcessingState {
                    client_is_processing: &mut client_is_processing,
                    message_id: &mut processing_message_id,
                    session_id: &mut processing_session_id,
                },
                &session_control,
                &client_event_tx,
                &sessions,
                &SwarmStatusRefs {
                    members: &swarm_members,
                    swarms_by_id: &swarms_by_id,
                    event_history: &event_history,
                    event_counter: &event_counter,
                    event_tx: &swarm_event_tx,
                },
                Some(id),
                Some(request_decoded_at),
            )
            .await;
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_PRE_ACK_DONE id={} session={} dispatch_ms={} total_since_decode_ms={}",
                id,
                client_session_id,
                cancel_dispatch_start.elapsed().as_millis(),
                request_decoded_at.elapsed().as_millis()
            ));
            if !client_is_processing {
                let mut connections = client_connections.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id) {
                    info.is_processing = false;
                    info.current_tool_name = None;
                }
            }
            if request_lifecycle_logged {
                log_request_lifecycle_handled(
                    ServerRequestLifecycleFields {
                        phase: "handled",
                        request_id,
                        request_kind: &request_kind,
                        client_session_id: &client_session_id,
                        client_connection_id: &client_connection_id,
                        client_instance_id: current_client_instance_id.as_deref(),
                        client_is_processing,
                        message_id: processing_message_id,
                        processing_session_id: processing_session_id.as_deref(),
                        line_bytes: line.len(),
                    },
                    request_lifecycle_start,
                    request_decoded_at,
                );
            }
            continue;
        }

        // Send ack
        let ack = ServerEvent::Ack { id: request.id() };
        let json = encode_event(&ack);
        {
            let ack_start = Instant::now();
            let mut w = writer.lock().await;
            if super::client_writer::write_bytes(&mut *w,json.as_bytes()).await.is_err() {
                if request_lifecycle_logged {
                    let mut fields =
                        server_request_lifecycle_fields(ServerRequestLifecycleFields {
                            phase: "ack_write_failed",
                            request_id,
                            request_kind: &request_kind,
                            client_session_id: &client_session_id,
                            client_connection_id: &client_connection_id,
                            client_instance_id: current_client_instance_id.as_deref(),
                            client_is_processing,
                            message_id: processing_message_id,
                            processing_session_id: processing_session_id.as_deref(),
                            line_bytes: line.len(),
                        });
                    fields.push((
                        "ack_write_ms".to_string(),
                        ack_start.elapsed().as_millis().to_string(),
                    ));
                    crate::logging::event_warn("SERVER_REQUEST_LIFECYCLE", fields);
                }
                break;
            }
            if request_lifecycle_logged {
                let mut fields = server_request_lifecycle_fields(ServerRequestLifecycleFields {
                    phase: "acked",
                    request_id,
                    request_kind: &request_kind,
                    client_session_id: &client_session_id,
                    client_connection_id: &client_connection_id,
                    client_instance_id: current_client_instance_id.as_deref(),
                    client_is_processing,
                    message_id: processing_message_id,
                    processing_session_id: processing_session_id.as_deref(),
                    line_bytes: line.len(),
                });
                fields.push((
                    "ack_write_ms".to_string(),
                    ack_start.elapsed().as_millis().to_string(),
                ));
                fields.push((
                    "since_decode_ms".to_string(),
                    request_decoded_at.elapsed().as_millis().to_string(),
                ));
                crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
            }
        }

        if let Some(error) = unavailable_swarm_response(&request) {
            let _ = client_event_tx.send(error);
            continue;
        }

        let (request, queued_messages) = match request {
            Request::QueuedMessages {
                id,
                entries,
                system_reminder,
                observe_startup_context,
            } => (
                Request::Message {
                    id,
                    content: String::new(),
                    images: Vec::new(),
                    system_reminder,
                    no_reply: false,
                    observe_startup_context,
                    activate_skill: None,
                },
                Some(entries),
            ),
            request => (request, None),
        };
        match request {
            Request::PrimaryStreamSubscribe{id} => {
                client_event_tx.enable_primary_stream();
                let _=client_event_tx.send(ServerEvent::PrimaryStreamCapabilities{id,version:1});
            },
            Request::QueuedMessages { .. } => unreachable!("queued request normalized above"),
            Request::DelegationProbe { id } | Request::DelegationExecute { id, .. } => {
                let _ = client_event_tx.send(ServerEvent::Error { id, message:"Hosted delegation uses a dedicated capability-checked connection and does not take over an attached Session.".into(), retry_after_secs:None });
            }
            Request::Message {
                id,
                content,
                images,
                system_reminder,
                no_reply,
                observe_startup_context,
                activate_skill,
            } => {
                if no_reply {
                    if activate_skill.is_some() {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: "Context-only messages cannot activate a skill.".to_string(),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                    append_context_message(
                        id,
                        &content,
                        images,
                        &client_session_id,
                        client_is_processing,
                        &agent,
                        &client_event_tx,
                    )
                    .await;
                    continue;
                }
                start_processing_message(
                    ProcessingMessage {
                        queued_messages,
                        id,
                        content,
                        images,
                        system_reminder,
                        observe_startup_context,
                        activate_skill,
                    },
                    &client_session_id,
                    &mut ProcessingState {
                        client_is_processing: &mut client_is_processing,
                        message_id: &mut processing_message_id,
                        session_id: &mut processing_session_id,
                    },
                    &agent,
                    &client_event_tx,
                    &sessions,
                    active_terminal_env.clone(),
                    &startup_context,
                    &SwarmStatusRefs {
                        members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        event_tx: &swarm_event_tx,
                    },
                )
                .await;
            }

            Request::Cancel { id } => {
                cancel_processing_message(
                    &mut ProcessingState {
                        client_is_processing: &mut client_is_processing,
                        message_id: &mut processing_message_id,
                        session_id: &mut processing_session_id,
                    },
                    &session_control,
                    &client_event_tx,
                    &sessions,
                    &SwarmStatusRefs {
                        members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        event_tx: &swarm_event_tx,
                    },
                    Some(id),
                    Some(request_decoded_at),
                )
                .await;
                if !client_is_processing {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.is_processing = false;
                        info.current_tool_name = None;
                    }
                }
            }

            Request::SoftInterrupt {
                id,
                content,
                images,
                urgent,
            } => {
                queue_soft_interrupt(
                    id,
                    content,
                    images,
                    urgent,
                    SoftInterruptSource::User,
                    &session_control,
                    &client_event_tx,
                );
            }

            Request::CancelSoftInterrupts { id } => {
                clear_soft_interrupts(id, &client_session_id, &session_control, &client_event_tx);
            }

            Request::BackgroundTool { id } => {
                move_tool_to_background(id, &session_control, &client_event_tx);
            }

            Request::Clear { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "clear",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                agent = crate::hooks::with_client_terminal_env(
                    active_terminal_env.clone(),
                    handle_clear_session(id,&mut client_session_id,&client_connection_id,&agent,&instruction_repositories,&sessions,&startup_context,&mcp_pool,&shutdown_signals,&soft_interrupt_queues,&client_connections,&swarm_members,&swarms_by_id,&event_history,&event_counter,&swarm_event_tx,&client_event_tx),
                ).await;
                session_control = refresh_session_control_handle(
                    &client_session_id,
                    &agent,
                    &shutdown_signals,
                    &soft_interrupt_queues,
                )
                .await;
            }

            Request::ActivateSkill { id, skill } => {
                let Some(mut agent_guard) = try_lock_idle_agent_for_request(
                    id,
                    "activate_skill",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) else {
                    continue;
                };
                match agent_guard.activate_skill(&skill) {
                    Ok(activation) => {
                        let event = ServerEvent::SkillActivated {
                            id,
                            skill_id: activation.skill_id,
                            description: activation.description,
                            source: activation.source.kind.to_string(),
                        };
                        drop(agent_guard);
                        let _ = fanout_live_client_event(&swarm_members, &client_session_id, event)
                            .await;
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Skill activation failed: {error}"),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::Rewind { id, message_index } => {
                if client_is_processing {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message: "Cannot rewind while a turn is processing.".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }

                let rewind_result = {
                    let mut agent_guard = agent.lock().await;
                    agent_guard.rewind_to_message(message_index)
                };

                match rewind_result {
                    Ok(removed) => {
                        context_transactions.invalidate_session_drafts(
                            &client_session_id,
                            "conversation rewind replaced authoritative history",
                        );
                        crate::logging::info(&format!(
                            "Rewound session {} to message {} (removed {})",
                            client_session_id, message_index, removed
                        ));
                        if handle_get_history(
                            id,
                            &client_session_id,
                            client_is_processing,
                            &agent,
                            &startup_context,
                            &provider,
                            &sessions,
                            &client_connections,
                            &client_count,
                            &writer,
                            &server_name,
                            &server_icon,
                            None,
                    Some(&client_event_tx),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        // The truncated History replaces the client transcript
                        // (dropping the inline plan graph); re-send the plan so
                        // the diagram comes back.
                        send_swarm_plan_to_session(
                            &client_session_id,
                            &swarm_members,
                            &swarm_plans,
                        )
                        .await;
                    }
                    Err(message) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::RewindUndo { id } => {
                if client_is_processing {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message: "Cannot undo rewind while a turn is processing.".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }

                let undo_result = {
                    let mut agent_guard = agent.lock().await;
                    agent_guard.undo_rewind()
                };

                match undo_result {
                    Ok(restored) => {
                        context_transactions.invalidate_session_drafts(
                            &client_session_id,
                            "conversation rewind undo replaced authoritative history",
                        );
                        crate::logging::info(&format!(
                            "Undid rewind for session {} (restored {})",
                            client_session_id, restored
                        ));
                        if handle_get_history(
                            id,
                            &client_session_id,
                            client_is_processing,
                            &agent,
                            &startup_context,
                            &provider,
                            &sessions,
                            &client_connections,
                            &client_count,
                            &writer,
                            &server_name,
                            &server_icon,
                            None,
                            Some(&client_event_tx),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        // Same as rewind: restore the inline plan graph after
                        // the transcript replacement.
                        send_swarm_plan_to_session(
                            &client_session_id,
                            &swarm_members,
                            &swarm_plans,
                        )
                        .await;
                    }
                    Err(message) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::Ping { id } => {
                let json = encode_event(&ServerEvent::Pong { id });
                let mut w = writer.lock().await;
                if super::client_writer::write_bytes(&mut *w,json.as_bytes()).await.is_err() {
                    break;
                }
            }

            Request::GetState { id } => {
                if handle_get_state(
                    id,
                    &client_session_id,
                    client_is_processing,
                    &sessions,
                    &writer,
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            Request::Subscribe {
                id,
                working_dir: subscribe_working_dir,
                selfdev,
                mut target_session_id,
                agent: requested_agent,
                startup_context_caller,
                client_instance_id,
                client_has_local_history,
                allow_session_takeover,
                terminal_env,
            } => {
                if let Err(message) =
                    required_subscribe_working_dir(subscribe_working_dir.as_deref())
                {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    });
                    continue;
                }
                let mut fresh_runtime = None;
                if client_subscribed
                    && startup_context_caller
                        == Some(crate::protocol::StartupContextPrimaryCaller::HarnessApiCreate)
                {
                    if target_session_id.is_some() {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: "Harness creation cannot also name an attach target.".into(),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                    if reject_if_agent_busy_for_request(
                        id,
                        "create a new session",
                        &client_session_id,
                        client_is_processing,
                        &agent,
                        &client_event_tx,
                    ) {
                        continue;
                    }
                    let selection =
                        match crate::instruction::AgentSelection::parse(requested_agent.as_deref())
                        {
                            Ok(selection) => selection,
                            Err(error) => {
                                let _ = client_event_tx.send(ServerEvent::Error {
                                    id,
                                    message: format!("Invalid initial agent selection: {error}"),
                                    retry_after_secs: None,
                                });
                                continue;
                            }
                        };
                    let next_provider = provider_template.fork_for_new_session();
                    let next_registry = Registry::new_for_shared_session(
                        next_provider.clone(),
                        mcp_pool.clone(),
                        (*instruction_repositories).clone(),
                    )
                    .await?;
                    let is_selfdev = selfdev.unwrap_or(false);
                    let prepared =
                        crate::hooks::with_client_terminal_env(terminal_env.clone(), async {
                            Agent::new_with_startup_context_and_agent_with_repositories(
                                Arc::clone(&next_provider),
                                next_registry.clone(),
                                subscribe_working_dir.as_deref(),
                                crate::agent::StartupContextActivation::primary(
                                    crate::agent::StartupContextCaller::HarnessApi,
                                ),
                                selection,
                                is_selfdev,
                                (*instruction_repositories).clone(),
                            )
                        })
                        .await;
                    let (mut prepared, _) = match prepared {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let _ = client_event_tx.send(ServerEvent::StartupContextFailed {
                                id,
                                failure: super::startup_context::primary_activation_failure(&error),
                            });
                            continue;
                        }
                    };
                    let next_id = prepared.session_id().to_string();
                    let next_name = prepared.session_short_name().map(str::to_string);
                    let next_stop = prepared.graceful_shutdown_signal();
                    let next_queue = prepared.soft_interrupt_queue();
                    let next_background = prepared.background_tool_signal();

                    // Publish and claim under the attachment owner's lock order.
                    // Recheck idle while retaining the old Agent guard through
                    // the claim, so another turn cannot race the handoff. Never
                    // replace the old shared Agent as /clear would do.
                    let mut connections = client_connections.write().await;
                    let mut live_sessions = sessions.write().await;
                    let idle = agent.try_lock();
                    let unchanged_attachment = idle
                        .as_ref()
                        .is_ok_and(|current| current.session_id() == client_session_id.as_str())
                        && connections
                            .get(&client_connection_id)
                            .is_some_and(|connection| connection.session_id == client_session_id);
                    if !unchanged_attachment {
                        drop(idle);
                        drop(live_sessions);
                        drop(connections);
                        prepared.mark_closed();
                        crate::tool::clear_session_tool_policy(&next_id);
                        let cleanup = crate::session::remove_unpublished_session(&next_id)
                            .err()
                            .map(|error| format!("; unpublished session cleanup failed: {error}"))
                            .unwrap_or_default();
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Session attachment changed or became busy during creation; the previous session was retained{cleanup}"),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                    live_sessions.insert(next_id.clone(), Arc::new(Mutex::new(prepared)));
                    if let Some(connection) = connections.get_mut(&client_connection_id) {
                        connection.session_id = next_id.clone();
                        connection.last_seen = Instant::now();
                    }
                    drop(idle);
                    drop(live_sessions);
                    drop(connections);
                    shutdown_signals
                        .write()
                        .await
                        .insert(next_id.clone(), next_stop);
                    register_session_interrupt_queue(&soft_interrupt_queues, &next_id, next_queue)
                        .await;
                    super::register_background_tool_signal(&next_id, next_background);
                    target_session_id = Some(next_id);
                    fresh_runtime = Some(FreshPrimaryRuntime {
                        provider: next_provider,
                        friendly_name: next_name,
                        is_selfdev,
                    });
                }

                // Every Subscribe carries an authoritative snapshot. An empty
                // snapshot must clear terminal vars inherited by the daemon
                // rather than retaining a prior pane's values.
                active_terminal_env = terminal_env;
                current_client_instance_id = client_instance_id.clone();
                {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.client_instance_id = client_instance_id.clone();
                        info.terminal_env = active_terminal_env.clone();
                    }
                }
                if let Some(target_session_id) = target_session_id {
                    if fresh_runtime.is_some() || crate::session::session_exists(&target_session_id)
                    {
                        let pre_resume_session_id = client_session_id.clone();
                        let attach_provider = fresh_runtime
                            .as_ref()
                            .map(|fresh| &fresh.provider)
                            .unwrap_or(&provider);
                        agent = crate::hooks::with_client_terminal_env(
                            active_terminal_env.clone(),
                            handle_resume_session(
                                id,
                                target_session_id.clone(),
                                client_instance_id.as_deref(),
                                client_has_local_history,
                                allow_session_takeover,
                                &mut client_selfdev,
                                &mut client_session_id,
                                &client_connection_id,
                                &agent,
                                &startup_context,
                                attach_provider,
                                &instruction_repositories,
                            initial_restore_status.take(),
                                &sessions,
                                &shutdown_signals,
                                &soft_interrupt_queues,
                                &client_connections,
                                &client_debug_state,
                                &swarm_members,
                                &swarms_by_id,
                                &file_touch,
                                &channel_subscriptions,
                                &channel_subscriptions_by_session,
                                &swarm_plans,
                                &swarm_coordinators,
                                &client_count,
                                &writer,
                                &server_name,
                                &server_icon,
                                &client_event_tx,
                                &mcp_pool,
                                &event_history,
                                &event_counter,
                                &swarm_event_tx,
                            ),
                        )
                        .await?;
                        session_control = refresh_session_control_handle(
                            &client_session_id,
                            &agent,
                            &shutdown_signals,
                            &soft_interrupt_queues,
                        )
                        .await;
                        if client_session_id == target_session_id {
                            if let Some(fresh) = fresh_runtime.take() {
                                friendly_name = fresh.friendly_name;
                                client_selfdev = fresh.is_selfdev;
                                client_primary_startup_activated = true;
                                swarm_enabled = crate::config::config().features.swarm;
                                *global_session_id.write().await = client_session_id.clone();
                            }
                            let resources = sessions.resources(&client_session_id, &agent)?;
                            registry = resources.registry;
                            handle_subscribe(
                                id,
                                subscribe_working_dir,
                                selfdev,
                                false,
                                &mut client_selfdev,
                                &client_session_id,
                                &client_connection_id,
                                &friendly_name,
                                &agent,
                                &registry,
                                swarm_enabled,
                                &swarm_members,
                                &swarms_by_id,
                                &channel_subscriptions,
                                &channel_subscriptions_by_session,
                                &swarm_plans,
                                &swarm_coordinators,
                                &client_event_tx,
                                &mcp_pool,
                                &event_history,
                                &event_counter,
                                &swarm_event_tx,
                            )
                            .await;
                            if let Some(snapshot) = try_available_models_snapshot(&agent) {
                                last_available_models_snapshot = Some(snapshot);
                            }
                        } else {
                            crate::logging::warn(&format!(
                                "Target-aware subscribe failed to bind {} from temporary {}; closing temporary client connection {}",
                                target_session_id, pre_resume_session_id, client_connection_id
                            ));
                            break;
                        }
                    } else {
                        if startup_context_caller
                            == Some(crate::protocol::StartupContextPrimaryCaller::HarnessApiAttach)
                        {
                            let _ = client_event_tx.send(ServerEvent::Error {
                                id,
                                message: "Target session became unavailable; attach did not create a replacement session"
                                    .to_string(),
                                retry_after_secs: None,
                            });
                            break;
                        }
                        if !client_primary_startup_activated {
                            let activation = {
                                let mut agent_guard = agent.lock().await;
                                agent_guard.activate_startup_context(
                                    crate::agent::StartupContextActivation::primary(
                                        initial_startup_caller,
                                    ),
                                )
                            };
                            if let Err(error) = activation {
                                let _ = client_event_tx.send(ServerEvent::StartupContextFailed {
                                    id,
                                    failure: super::startup_context::primary_activation_failure(
                                        &error,
                                    ),
                                });
                                break;
                            }
                            client_primary_startup_activated = true;
                        }
                        handle_subscribe(
                            id,
                            subscribe_working_dir,
                            selfdev,
                            true,
                            &mut client_selfdev,
                            &client_session_id,
                            &client_connection_id,
                            &friendly_name,
                            &agent,
                            &registry,
                            swarm_enabled,
                            &swarm_members,
                            &swarms_by_id,
                            &channel_subscriptions,
                            &channel_subscriptions_by_session,
                            &swarm_plans,
                            &swarm_coordinators,
                            &client_event_tx,
                            &mcp_pool,
                            &event_history,
                            &event_counter,
                            &swarm_event_tx,
                        )
                        .await;
                    }
                } else {
                    handle_subscribe(
                        id,
                        subscribe_working_dir,
                        selfdev,
                        true,
                        &mut client_selfdev,
                        &client_session_id,
                        &client_connection_id,
                        &friendly_name,
                        &agent,
                        &registry,
                        swarm_enabled,
                        &swarm_members,
                        &swarms_by_id,
                        &channel_subscriptions,
                        &channel_subscriptions_by_session,
                        &swarm_plans,
                        &swarm_coordinators,
                        &client_event_tx,
                        &mcp_pool,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                    )
                    .await;
                    if let Some(snapshot) = try_available_models_snapshot(&agent) {
                        last_available_models_snapshot = Some(snapshot);
                    }
                }
                client_subscribed = true;
            }

            Request::GetHistory { id } => {
                if handle_get_history(
                    id,
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &startup_context,
                    &provider,
                    &sessions,
                    &client_connections,
                    &client_count,
                    &writer,
                    &server_name,
                    &server_icon,
                    None,
                    Some(&client_event_tx),
                )
                .await
                .is_err()
                {
                    break;
                }
                // Follow the History payload with the current swarm plan: a
                // session-changing History clears the client's plan snapshot
                // (and the inline plan graph), so re-send it afterwards
                // instead of leaving the graph blank until the next plan
                // mutation broadcast.
                send_swarm_plan_to_session(&client_session_id, &swarm_members, &swarm_plans).await;
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::GetModelCatalog { id } => {
                if handle_get_model_catalog(
                    id,
                    &client_session_id,
                    &agent,
                    &startup_context,
                    &provider,
                    &writer,
                )
                .await
                .is_err()
                {
                    break;
                }
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::GetCompactedHistory {
                id,
                visible_messages,
            } => {
                if handle_get_compacted_history(
                    id,
                    &client_session_id,
                    &agent,
                    &writer,
                    visible_messages,
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            Request::GetStartupContextStatus {
                id,
                file_page_start,
                file_page_size,
                issue_page_start,
                issue_page_size,
            } => match startup_context_session_snapshot(&agent, &client_session_id) {
                Ok(session) => {
                    let snapshot = startup_context
                        .status_snapshot(
                            session,
                            file_page_start,
                            file_page_size,
                            issue_page_start,
                            issue_page_size,
                        )
                        .await;
                    super::startup_context::emit_checked(
                        &client_event_tx,
                        id,
                        crate::protocol::StartupContextOperation::Status,
                        ServerEvent::StartupContextStatus {
                            id,
                            snapshot,
                            action_required: None,
                        },
                    );
                }
                Err(failure) => {
                    let _ = client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                }
            },

            Request::OpenStartupContextEditor { id } => {
                match startup_context_session_snapshot(&agent, &client_session_id) {
                    Ok(session) => match session.working_dir {
                        Some(working_dir) => match startup_context
                            .open_editor(
                                client_session_id.clone(),
                                client_connection_id.clone(),
                                working_dir,
                            )
                            .await
                        {
                            Ok(super::startup_context::OpenEditorOutcome::Opened(editor)) => {
                                let release = super::startup_context::lease_request(
                                    editor.lease.lease_id.clone(),
                                    editor.project.key_digest.clone(),
                                    None,
                                    client_session_id.clone(),
                                    client_connection_id.clone(),
                                );
                                if !super::startup_context::emit_checked(
                                    &client_event_tx,
                                    id,
                                    crate::protocol::StartupContextOperation::OpenEditor,
                                    ServerEvent::StartupContextEditorOpened { id, editor },
                                ) {
                                    let _ = startup_context.close_editor(release);
                                }
                            }
                            Ok(super::startup_context::OpenEditorOutcome::Busy {
                                project,
                                owner,
                            }) => {
                                super::startup_context::emit_checked(
                                    &client_event_tx,
                                    id,
                                    crate::protocol::StartupContextOperation::OpenEditor,
                                    ServerEvent::StartupContextEditorBusy { id, project, owner },
                                );
                            }
                            Err(failure) => {
                                let _ = client_event_tx
                                    .send(ServerEvent::StartupContextFailed { id, failure });
                            }
                        },
                        None => {
                            let _ = client_event_tx.send(ServerEvent::StartupContextFailed {
                                id,
                                failure: super::startup_context::failure(
                                    crate::protocol::StartupContextOperation::OpenEditor,
                                    crate::protocol::StartupContextFailureKind::ProjectIdentity,
                                    "session has no bound working directory",
                                    false,
                                ),
                            });
                        }
                    },
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::RenewStartupContextEditorLease {
                id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context.renew_lease(request).await {
                    Ok(lease) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::RenewLease,
                            ServerEvent::StartupContextEditorLeaseRenewed { id, lease },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::CloseStartupContextEditor {
                id,
                lease_id,
                project_key_digest,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    None,
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context.close_editor(request) {
                    Ok(lease_id) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::CloseEditor,
                            ServerEvent::StartupContextEditorClosed { id, lease_id },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::ListStartupContextDirectory {
                id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
                directory,
                page_start,
                page_size,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context
                    .list_directory(request, directory, page_start, page_size)
                    .await
                {
                    Ok(page) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::ListDirectory,
                            ServerEvent::StartupContextDirectoryPage { id, page },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::SearchStartupContextFiles {
                id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
                query,
                max_results,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                if let Err(failure) = startup_context.start_search(
                    id,
                    request,
                    query,
                    max_results,
                    client_event_tx.clone(),
                ) {
                    let _ = client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                }
            }

            Request::CancelStartupContextSearch {
                id,
                search_request_id,
            } => {
                let was_active =
                    startup_context.cancel_search(&client_connection_id, search_request_id);
                super::startup_context::emit_checked(
                    &client_event_tx,
                    id,
                    crate::protocol::StartupContextOperation::CancelSearch,
                    ServerEvent::StartupContextSearchCanceled {
                        id,
                        search_request_id,
                        was_active,
                    },
                );
            }

            Request::PreviewStartupContextFile {
                id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
                path,
                start_char,
                max_chars,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context
                    .preview_file(request, path, start_char, max_chars)
                    .await
                {
                    Ok(preview) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::PreviewFile,
                            ServerEvent::StartupContextFilePreview { id, preview },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::GetStartupContextFileDetail {
                id,
                batch_id,
                spec_id,
                message_id,
                expected_sha256,
                start_char,
                max_chars,
            } => match startup_context_file_detail(
                &startup_context,
                &agent,
                &client_session_id,
                &batch_id,
                &spec_id,
                &message_id,
                &expected_sha256,
                start_char,
                max_chars,
            )
            .await
            {
                Ok(detail) => {
                    super::startup_context::emit_checked(
                        &client_event_tx,
                        id,
                        crate::protocol::StartupContextOperation::FileDetail,
                        ServerEvent::StartupContextFileDetail { id, detail },
                    );
                }
                Err(failure) => {
                    let _ = client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                }
            },

            Request::PreviewStartupContextSelection {
                id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
                selection,
            } => {
                let request = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context.preview_selection(request, selection).await {
                    Ok(preview) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::PreviewSelection,
                            ServerEvent::StartupContextSelectionPreview { id, preview },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::ApplyStartupContextSelection {
                id,
                operation_id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
                selection,
                save_project_default,
            } => {
                let lease = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                let request = super::startup_context::apply::ApplySelectionRequest {
                    lease,
                    operation_id,
                    selection,
                    save_project_default,
                };
                match startup_context
                    .apply_selection(request, Arc::clone(&agent), client_is_processing)
                    .await
                {
                    Ok(status) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::ApplySelection,
                            ServerEvent::StartupContextApplyStatus { id, status },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::CancelStartupContextApply {
                id,
                operation_id,
                lease_id,
                project_key_digest,
                expected_plan_revision,
            } => {
                let lease = super::startup_context::lease_request(
                    lease_id,
                    project_key_digest,
                    Some(expected_plan_revision),
                    client_session_id.clone(),
                    client_connection_id.clone(),
                );
                match startup_context.cancel_apply(lease, &operation_id) {
                    Ok(status) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::CancelApply,
                            ServerEvent::StartupContextApplyStatus { id, status },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            Request::GetStartupContextApplyStatus { id, operation_id } => {
                match startup_context.apply_status(&client_session_id, &operation_id) {
                    Ok(status) => {
                        super::startup_context::emit_checked(
                            &client_event_tx,
                            id,
                            crate::protocol::StartupContextOperation::ApplyStatus,
                            ServerEvent::StartupContextApplyStatus { id, status },
                        );
                    }
                    Err(failure) => {
                        let _ =
                            client_event_tx.send(ServerEvent::StartupContextFailed { id, failure });
                    }
                }
            }

            request @ (Request::GetContextEditorSnapshot { .. }
            | Request::GetContextMessageDetail { .. }
            | Request::PreviewContextRanges { .. }
            | Request::PreviewContextCuratorPlan { .. }
            | Request::SaveContextCuratorDefault { .. }
            | Request::PrepareContextDraft { .. }
            | Request::CancelContextDraft { .. }
            | Request::GetContextDraftStatus { .. }
            | Request::PreviewContextDraftSelection { .. }
            | Request::ApplyContextDraft { .. }
            | Request::ListContextTransactions { .. }
            | Request::GetContextTransactionDetail { .. }
            | Request::RevertContextTransaction { .. }
            | Request::ReapplyContextTransaction { .. }) => {
                super::context_control::dispatch_editor_request(
                    request,
                    &client_session_id,
                    &agent,
                    &context_transactions,
                    client_is_processing,
                    &client_event_tx,
                );
            }

            Request::SetContextEmergencyPolicy { id, policy } => {
                handle_set_context_emergency_policy(
                    id,
                    policy,
                    &agent,
                    &context_transactions,
                    client_is_processing,
                    &client_event_tx,
                );
            }

            Request::DebugCommand { id, .. } => {
                let _ = client_event_tx.send(ServerEvent::Error {
                    id,
                    message: "debug_command is only supported on the debug socket".to_string(),
                    retry_after_secs: None,
                });
            }

            Request::Reload { id, force } => {
                handle_reload(
                    id,
                    force,
                    &client_session_id,
                    &agent,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::ResumeSession {
                id,
                session_id,
                client_instance_id,
                client_has_local_history,
                allow_session_takeover,
            } => {
                let previous_session_id = client_session_id.clone();
                current_client_instance_id = client_instance_id.clone();
                {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.client_instance_id = client_instance_id.clone();
                    }
                }
                agent = crate::hooks::with_client_terminal_env(
                    active_terminal_env.clone(),
                    handle_resume_session(
                        id,
                        session_id,
                        client_instance_id.as_deref(),
                        client_has_local_history,
                        allow_session_takeover,
                        &mut client_selfdev,
                        &mut client_session_id,
                        &client_connection_id,
                        &agent,
                        &startup_context,
                        &provider,
                        &instruction_repositories,
                    None,
                        &sessions,
                        &shutdown_signals,
                        &soft_interrupt_queues,
                        &client_connections,
                        &client_debug_state,
                        &swarm_members,
                        &swarms_by_id,
                        &file_touch,
                        &channel_subscriptions,
                        &channel_subscriptions_by_session,
                        &swarm_plans,
                        &swarm_coordinators,
                        &client_count,
                        &writer,
                        &server_name,
                        &server_icon,
                        &client_event_tx,
                        &mcp_pool,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                    ),
                )
                .await?;
                if client_session_id != previous_session_id {
                    startup_context.release_connection(&client_connection_id);
                }
                session_control = refresh_session_control_handle(
                    &client_session_id,
                    &agent,
                    &shutdown_signals,
                    &soft_interrupt_queues,
                )
                .await;
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::ResumeAllSessions { id } => {
                super::client_actions::handle_resume_all_sessions(
                    id,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &client_event_tx,
                )
                .await;
            }

            Request::CycleModel { id, direction } => {
                handle_cycle_model(
                    id,
                    direction,
                    &agent,
                    &context_transactions,
                    &client_event_tx,
                )
                .await;
            }

            Request::RefreshModels { id } => {
                handle_refresh_models(id, &provider, &agent, &client_event_tx).await;
            }

            Request::SetPremiumMode { id, mode } => {
                handle_set_premium_mode(id, mode, &agent, &client_event_tx).await;
            }

            Request::SetModel { id, model } => {
                handle_set_model(id, model, &agent, &context_transactions, &client_event_tx).await;
            }

            Request::SetRoute { id, selection } => {
                handle_set_route(
                    id,
                    selection,
                    &agent,
                    &context_transactions,
                    &client_event_tx,
                )
                .await;
            }

            Request::SetSubagentModel { id, model } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "set_subagent_model",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                handle_set_subagent_model(id, model, &agent, &client_event_tx).await;
            }

            Request::RunSubagent {
                id,
                prompt,
                subagent_type,
                model,
                session_id,
            } => {
                handle_run_subagent(
                    id,
                    prompt,
                    subagent_type,
                    model,
                    session_id,
                    &agent,
                    &client_event_tx,
                );
            }

            Request::SetReasoningEffort {
                id,
                effort,
                target_session_id,
            } => {
                if let Some(target_session_id) = target_session_id {
                    let target_agent = { sessions.read().await.get(&target_session_id).cloned() };
                    if let Some(target_agent) = target_agent {
                        handle_set_reasoning_effort(id, effort, &target_agent, &client_event_tx)
                            .await;
                    } else {
                        let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                            id,
                            effort: None,
                            error: Some(format!("target session not found: {target_session_id}")),
                        });
                    }
                } else {
                    handle_set_reasoning_effort(id, effort, &agent, &client_event_tx).await;
                }
            }

            Request::SetServiceTier { id, service_tier } => {
                handle_set_service_tier(id, service_tier, &agent, &client_event_tx).await;
            }

            Request::SetTransport { id, transport } => {
                handle_set_transport(id, transport, &agent, &client_event_tx).await;
            }

            Request::LegacyContextCommand { id, command } => {
                let request = match command {
                    crate::protocol::LegacyContextCommand::Compact => {
                        crate::protocol::ContextRequestKind::LegacyCompact
                    }
                    crate::protocol::LegacyContextCommand::SetCompactionMode => {
                        crate::protocol::ContextRequestKind::LegacySetCompactionMode
                    }
                };
                reject_legacy_context_request(id, request, &client_event_tx);
            }

            Request::RenameSession { id, title } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "rename_session",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                handle_rename_session(
                    id,
                    title,
                    &agent,
                    &client_session_id,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::NotifyAuthChanged {
                id,
                provider: provider_hint,
                auth,
                prefer_strongest,
            } => {
                handle_notify_auth_changed(
                    id,
                    provider_hint,
                    auth,
                    prefer_strongest,
                    &provider,
                    &provider_template,
                    &sessions,
                    &client_session_id,
                    &agent,
                    &client_event_tx,
                )
                .await;
            }

            Request::SwitchAnthropicAccount { id, label } => {
                handle_switch_anthropic_account(id, label, &agent, &client_event_tx).await;
            }

            Request::SwitchOpenAiAccount { id, label } => {
                handle_switch_openai_account(id, label, &agent, &client_event_tx).await;
            }

            Request::SetFeature {
                id,
                feature,
                enabled,
            } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "set_feature",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                handle_set_feature(
                    id,
                    feature,
                    enabled,
                    &agent,
                    &client_session_id,
                    &friendly_name,
                    &mut swarm_enabled,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &swarm_plans,
                    &client_event_tx,
                )
                .await;
            }

            Request::SetAgent {
                id,
                agent: selection,
                replace,
            } => {
                let Some(mut agent_guard) = try_lock_idle_agent_for_request(
                    id,
                    "set_agent",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) else {
                    continue;
                };
                let selection = match crate::instruction::AgentSelection::parse(Some(&selection)) {
                    Ok(selection) => selection,
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Invalid agent selection: {error}"),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                };
                let mode = if replace {
                    crate::agent::AgentProfileChangeMode::ReplaceSystem
                } else {
                    crate::agent::AgentProfileChangeMode::Ordinary
                };
                match agent_guard.change_primary_agent(selection, mode).await {
                    Ok(outcome) => {
                        let active = outcome.agent().clone();
                        let (change, message_id) = match outcome {
                            crate::agent::AgentProfileChangeOutcome::NoChange { .. } => (
                                crate::protocol::AgentProfileChangeKind::NoChange,
                                agent_guard
                                    .active_transition_message_id()
                                    .map(str::to_string),
                            ),
                            crate::agent::AgentProfileChangeOutcome::Provisional { .. } => {
                                (crate::protocol::AgentProfileChangeKind::Provisional, None)
                            }
                            crate::agent::AgentProfileChangeOutcome::Appended {
                                message_id,
                                ..
                            } => (
                                crate::protocol::AgentProfileChangeKind::Appended,
                                Some(message_id),
                            ),
                            crate::agent::AgentProfileChangeOutcome::Replaced {
                                audit_message_id,
                                ..
                            } => (
                                crate::protocol::AgentProfileChangeKind::Replaced,
                                audit_message_id,
                            ),
                        };
                        let message_content = message_id.as_deref().and_then(|message_id| {
                            agent_guard
                                .messages()
                                .iter()
                                .find(|message| message.id == message_id)
                                .and_then(|message| {
                                    message.content.iter().find_map(|block| match block {
                                        crate::message::ContentBlock::Text { text, .. } => {
                                            Some(text.clone())
                                        }
                                        _ => None,
                                    })
                                })
                        });
                        if matches!(
                            change,
                            crate::protocol::AgentProfileChangeKind::Appended
                                | crate::protocol::AgentProfileChangeKind::Replaced
                        ) {
                            context_transactions.invalidate_session_drafts(
                                agent_guard.session_id(),
                                "agent profile change replaced authoritative prompt or history",
                            );
                        }
                        let event = ServerEvent::AgentSelected {
                            id,
                            agent_id: active.id,
                            display_name: active.display_name,
                            scope: active.scope.to_string(),
                            change,
                            message_id,
                            message_content,
                            active_skill_id: agent_guard.active_skill_id().map(str::to_string),
                        };
                        drop(agent_guard);
                        let _ = fanout_live_client_event(&swarm_members, &client_session_id, event)
                            .await;
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Agent selection failed: {error}"),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            /*
             * SetAgent deliberately retains its nonblocking Agent guard for the
             * whole transaction above. Do not replace it with the generic
             * check-then-lock helper: that would reintroduce a race with turn
             * acquisition.
             */
            Request::RenderWorkflowPrompt { id, workflow } => {
                let working_dir = match agent.try_lock() {
                    Ok(current) => Ok(current.working_dir().map(str::to_string)),
                    Err(_) => Session::load(&client_session_id)
                        .map(|session| session.working_dir)
                        .map_err(|error| error.to_string()),
                };
                let result = working_dir.and_then(|working_dir| {
                    crate::workflow::render_prompt(
                        instruction_repositories.as_ref(),
                        working_dir.as_deref().map(Path::new),
                        &workflow,
                    )
                    .map_err(|error| error.to_string())
                });
                match result {
                    Ok(content) => {
                        let _ = client_event_tx
                            .send(ServerEvent::WorkflowPromptRendered { id, content });
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Workflow instruction rendering failed: {error}"),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::InspectInstructions { id, request } => {
                use crate::instruction::inspection::{
                    InspectionContext, InstructionInspectionFailure,
                };
                let session_id = client_session_id.clone();
                let provider = agent
                    .try_lock()
                    .map(|current| current.provider_handle())
                    .unwrap_or_else(|_| Arc::clone(&provider_template));
                let captured = if matches!(
                    &request,
                    crate::protocol::InstructionInspectionRequest::Open { .. }
                ) {
                    agent.try_lock().ok().map(|current| {
                        InspectionContext::from_session(
                            current.startup_context_session(),
                            provider.as_ref(),
                            current.is_canary(),
                        )
                    })
                } else {
                    None
                };
                let receiver = instruction_inspection.submit(
                    instruction_repositories.as_ref().clone(),
                    client_session_id.clone(),
                    move || {
                        if let Some(context) = captured {
                            return Ok(context);
                        }
                        let session = Session::load(&session_id).map_err(|error| {
                            InstructionInspectionFailure {
                                operation: "capture session".into(),
                                detail: error.to_string(),
                                refresh_required: true,
                            }
                        })?;
                        Ok(InspectionContext::from_session(
                            &session,
                            provider.as_ref(),
                            session.is_canary,
                        ))
                    },
                    request,
                );
                let events = client_event_tx.clone();
                tokio::spawn(async move {
                    if let Ok(reply) = receiver.await {
                        let _ = events.send(ServerEvent::InstructionInspection {
                            id,
                            reply: Box::new(reply),
                        });
                    }
                });
            }

            Request::ManageInstructions { id, request } => {
                use crate::instruction::inspection::InspectionContext;
                let provider = agent
                    .try_lock()
                    .map(|current| current.provider_handle())
                    .unwrap_or_else(|_| Arc::clone(&provider_template));
                let captured = agent.try_lock().ok().map(|current| {
                    InspectionContext::from_session(
                        current.startup_context_session(),
                        provider.as_ref(),
                        current.is_canary(),
                    )
                });
                let worker = instruction_management.clone();
                let resolver = instruction_inspection.target_resolver();
                let repositories = instruction_repositories.as_ref().clone();
                let session_id = client_session_id.clone();
                let events = client_event_tx.clone();
                let failure_session = client_session_id.clone();
                tokio::spawn(async move {
                    let context = tokio::task::spawn_blocking(move || match captured {
                        Some(context) => Ok(context),
                        None => Session::load(&session_id).map(|session| {
                            InspectionContext::from_session(
                                &session,
                                provider.as_ref(),
                                session.is_canary,
                            )
                        }),
                    })
                    .await;
                    match context {
                        Ok(Ok(context)) => {
                            let failure = match worker
                                .submit(repositories, context, resolver, *request)
                                .await
                            {
                                Ok(reply) => crate::protocol::emit_instruction_management_reply(
                                    id,
                                    reply,
                                    |event| {
                                        let _ = events.send(event);
                                    },
                                )
                                .err()
                                .map(|error| error.to_string()),
                                Err(error) => Some(format!(
                                    "Manager worker stopped before returning an outcome: {error}"
                                )),
                            };
                            if let Some(error) = failure {
                                let _ = events.send(ServerEvent::InstructionManagement { id, reply: Box::new(crate::protocol::InstructionManagementReply {
                                        session_id: failure_session.clone(),
                                        result: crate::protocol::InstructionManagementResult::Failed(crate::protocol::InstructionManagementFailure { operation: "deliver manager outcome".into(), detail: format!("{error}. Recover its retained draft or receipt before retrying; no rollback is implied."), draft: None, source_unchanged: false }),
                                    }) });
                            }
                        }
                        error => {
                            let detail = match error {
                                Ok(Err(error)) => error.to_string(),
                                Err(error) => error.to_string(),
                                _ => String::new(),
                            };
                            let _ = events.send(ServerEvent::InstructionManagement {
                                id,
                                reply: Box::new(crate::protocol::InstructionManagementReply {
                                    session_id: failure_session,
                                    result: crate::protocol::InstructionManagementResult::Failed(
                                        crate::protocol::InstructionManagementFailure {
                                            operation: "capture manager session".into(),
                                            detail,
                                            draft: None,
                                            source_unchanged: true,
                                        },
                                    ),
                                }),
                            });
                        }
                    }
                });
            }

            Request::GetAgentCatalog { id } => {
                let catalog = match agent.try_lock() {
                    Ok(agent_guard) => agent_guard
                        .list_primary_agents()
                        .map(|entries| (entries, agent_guard.active_agent().cloned()))
                        .map_err(|error| error.to_string()),
                    Err(_) => Session::load(&client_session_id)
                        .map_err(|error| error.to_string())
                        .and_then(|session| {
                            let working_dir = session.working_dir.as_deref().map(Path::new);
                            crate::instruction::SystemPromptComposer::from_repository_service(
                                instruction_repositories.as_ref().clone(),
                            )
                            .list_primary_agents(working_dir)
                            .map(|entries| (entries, session.active_agent().cloned()))
                            .map_err(|error| error.to_string())
                        }),
                };
                match catalog {
                    Ok((entries, active)) => {
                        let agents = entries
                            .into_iter()
                            .map(|entry| crate::protocol::AgentProfileSummary {
                                active: active
                                    .as_ref()
                                    .is_some_and(|current| current == &entry.agent),
                                agent_id: entry.agent.id,
                                display_name: entry.agent.display_name,
                                scope: entry.agent.scope.to_string(),
                                description: entry.description,
                            })
                            .collect();
                        let _ = client_event_tx.send(ServerEvent::AgentCatalog { id, agents });
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Agent catalog failed: {error}"),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::GetAgentStatus {
                id,
                include_instructions,
            } => {
                let status = match agent.try_lock() {
                    Ok(agent_guard) => Ok((
                        agent_guard.active_agent().cloned(),
                        agent_guard.first_provider_dispatch_at().is_some(),
                        agent_guard
                            .active_transition_message_id()
                            .map(str::to_string),
                        include_instructions
                            .then(|| agent_guard.system_prompt_text().map(str::to_string))
                            .flatten(),
                        include_instructions
                            .then(|| agent_guard.active_skill_text().map(str::to_string))
                            .flatten(),
                    )),
                    Err(_) => Session::load(&client_session_id)
                        .map(|session| {
                            (
                                session.active_agent().cloned(),
                                session.first_provider_dispatch_at().is_some(),
                                session.active_transition_message_id().map(str::to_string),
                                include_instructions
                                    .then(|| session.system_prompt_text().map(str::to_string))
                                    .flatten(),
                                include_instructions
                                    .then(|| {
                                        session
                                            .active_skill
                                            .as_ref()
                                            .map(|skill| skill.rendered_text.clone())
                                    })
                                    .flatten(),
                            )
                        })
                        .map_err(|error| error.to_string()),
                };
                let (
                    active,
                    first_provider_dispatched,
                    active_transition_message_id,
                    system_prompt,
                    active_skill,
                ) = match status {
                    Ok(status) => status,
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Agent inspection failed: {error}"),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                };
                let Some(active) = active else {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message: "Session has no active primary agent.".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                };
                let _ = client_event_tx.send(ServerEvent::AgentStatus {
                    id,
                    agent_id: active.id,
                    display_name: active.display_name,
                    scope: active.scope.to_string(),
                    first_provider_dispatched,
                    active_transition_message_id,
                    system_prompt,
                    active_skill,
                });
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::Execution { id, request } => {
                let result = match crate::storage::jcode_dir() {
                    Ok(root) => {
                        crate::execution::inspection::inspect(&root, &client_session_id, request)
                            .await
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(response) => {
                        let _ =
                            client_event_tx.send(ServerEvent::ExecutionResponse { id, response });
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: format!("Execution inspection/control failed: {error:#}"),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::TaskMonitorProbe { id } => {
                let _ = client_event_tx.send(ServerEvent::TaskMonitorCapabilities {
                    id,
                    version: 1,
                    child_context: true,
                });
            }
            Request::PrimaryLaunchProbe{id}=>{
                let _=client_event_tx.send(ServerEvent::PrimaryLaunchCapabilities{id,version:1,enabled:crate::primary::launch_enabled()});
            }
            Request::PrimaryLaunch{id,request}=>{
                let host=sessions.clone();let provider=provider_template.clone();let pool=mcp_pool.clone();let repositories=(*instruction_repositories).clone();let tx=client_event_tx.clone();
                inspection_requests.spawn(async move {
                    let response=host.request_launch(*request,provider,pool,repositories).await;
                    let _=tx.send(ServerEvent::PrimaryLaunchResponse{id,response:Box::new(response)});
                });
            }
            Request::WorkspaceProbe { id } => {
                let _ = client_event_tx.send(ServerEvent::WorkspaceCapabilities {
                    id,
                    catalog_version: 1,
                    managed_rollout: false,
                });
            }
            Request::Workspace { id, request } => {
                let event_tx = client_event_tx.clone();
                inspection_requests.spawn(async move {
                    let response = crate::workspace::dispatch(*request).await;
                    let _ = event_tx.send(ServerEvent::WorkspaceResponse {
                        id,
                        response: Box::new(response),
                    });
                });
            }
            Request::TaskMonitor { id, request } => {
                let session = client_session_id.clone();
                let event_tx = client_event_tx.clone();
                inspection_requests.spawn(async move {
                    let result = match crate::storage::jcode_dir() {
                        Ok(root) => {
                            crate::execution::task_monitor::inspect(&root, &session, request).await
                        }
                        Err(error) => Err(error),
                    };
                    let event = match result {
                        Ok(response) => ServerEvent::TaskMonitorResponse { id, response },
                        Err(error) => ServerEvent::Error {
                            id,
                            message: format!("Task monitor: {error:#}"),
                            retry_after_secs: None,
                        },
                    };
                    let _ = event_tx.send(event);
                });
            }
            Request::ChildContext {
                id,
                child_id,
                request,
            } => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let target = child_id.clone();
                let destination = client_event_tx.clone();
                inspection_requests.spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if destination
                            .send(ServerEvent::ChildContextResponse {
                                id,
                                child_id: target.clone(),
                                event: Box::new(event),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                let service = context_transactions.clone();
                let repositories = (*instruction_repositories).clone();
                inspection_requests.spawn(async move {
                    let result = if request.id() != id {
                        Err(anyhow::anyhow!("Child context correlation mismatch"))
                    } else {
                        super::child_context::handle(
                            child_id,
                            *request,
                            service,
                            repositories,
                            tx.clone().into(),
                        )
                        .await
                    };
                    if let Err(error) = result {
                        let _ = tx.send(ServerEvent::Error {
                            id,
                            message: format!("Child context: {error:#}"),
                            retry_after_secs: None,
                        });
                    }
                });
            }

            Request::SessionInspection { id, request } => {
                let origin = crate::storage::jcode_dir();
                let session = client_session_id.clone();
                let client_event_tx = client_event_tx.clone();
                inspection_requests.spawn(async move {
                    let result = match origin {
                        Ok(root) => {
                            crate::session_inspection::human_inspection(root, session, request)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(response) => {
                            let _ = client_event_tx
                                .send(ServerEvent::SessionInspectionResponse { id, response });
                            let _ = client_event_tx.send(ServerEvent::Done { id });
                        }
                        Err(error) => {
                            let _ = client_event_tx.send(ServerEvent::Error {
                                id,
                                message: format!("Session inspection failed: {error:#}"),
                                retry_after_secs: None,
                            });
                        }
                    }
                });
            }
            Request::OutputCleanup { id, request } => {
                let origin = crate::storage::jcode_dir();
                let session = client_session_id.clone();
                let client_event_tx = client_event_tx.clone();
                inspection_requests.spawn(async move {
                    let result = match origin {
                        Ok(root) => {
                            crate::session_inspection::human_cleanup(root, session, request).await
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(response) => {
                            let _ = client_event_tx
                                .send(ServerEvent::OutputCleanupResponse { id, response });
                            let _ = client_event_tx.send(ServerEvent::Done { id });
                        }
                        Err(error) => {
                            let _ = client_event_tx.send(ServerEvent::Error {
                                id,
                                message: format!("Output cleanup failed: {error:#}"),
                                retry_after_secs: None,
                            });
                        }
                    }
                });
            }

            Request::Split { id } => {
                handle_split(
                    id,
                    &client_session_id,
                    &instruction_repositories,
                    None,
                    &client_event_tx,
                )
                .await;
            }
            Request::SplitWithWorkflow { id, workflow } => {
                handle_split(
                    id,
                    &client_session_id,
                    &instruction_repositories,
                    Some(&workflow),
                    &client_event_tx,
                )
                .await;
            }

            Request::Transfer { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "transfer",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                handle_transfer(
                    id,
                    &client_session_id,
                    &agent,
                    &instruction_repositories,
                    &client_event_tx,
                )
                .await;
            }

            Request::TriggerMemoryExtraction { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "trigger_memory_extraction",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ) {
                    continue;
                }
                handle_trigger_memory_extraction(id, &agent, &client_event_tx).await;
            }

            // Agent-to-agent communication
            Request::AgentRegister { id, .. } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::StdinResponse {
                id,
                request_id,
                input,
            } => {
                handle_stdin_response(id, request_id, input, &client_session_id, &sessions, &client_event_tx)
                    .await;
            }

            Request::AgentTask { id, task, .. } => {
                start_processing_message(
                    ProcessingMessage {
                        queued_messages: None,
                        id,
                        content: task,
                        images: Vec::new(),
                        system_reminder: None,
                        observe_startup_context: true,
                        activate_skill: None,
                    },
                    &client_session_id,
                    &mut ProcessingState {
                        client_is_processing: &mut client_is_processing,
                        message_id: &mut processing_message_id,
                        session_id: &mut processing_session_id,
                    },
                    &agent,
                    &client_event_tx,
                    &sessions,
                    active_terminal_env.clone(),
                    &startup_context,
                    &SwarmStatusRefs {
                        members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        event_tx: &swarm_event_tx,
                    },
                )
                .await;
            }

            Request::AgentCapabilities { id } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::AgentContext { id } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::NotifySession {
                id,
                session_id,
                message,
                unattended_context,
            } => {
                handle_notify_session(
                    id,
                    session_id,
                    message,
                    unattended_context,
                    NotifySessionContext {
                        sessions: &sessions,
                        soft_interrupt_queues: &soft_interrupt_queues,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        swarm_event_tx: &swarm_event_tx,
                        client_event_tx: &client_event_tx,
                    },
                )
                .await;
            }

            Request::Transcript {
                id,
                text,
                mode,
                session_id,
            } => {
                match super::debug::inject_transcript(
                    id,
                    text,
                    mode,
                    session_id,
                    &client_connections,
                    &client_debug_state,
                    &swarm_members,
                )
                .await
                {
                    Ok(event) => {
                        let _ = client_event_tx.send(event);
                    }
                    Err(error) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message: error.to_string(),
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::InputShell { id, command } => {
                handle_input_shell(id, command, &agent, &client_event_tx);
            }

            // === Agent communication ===
            Request::CommShare {
                id,
                session_id: req_session_id,
                key,
                value,
                append,
            } => {
                handle_comm_share(
                    id,
                    req_session_id,
                    key,
                    value,
                    append,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommRead {
                id,
                session_id: req_session_id,
                key,
            } => {
                handle_comm_read(
                    id,
                    req_session_id,
                    key,
                    &client_event_tx,
                    &swarm_members,
                    &shared_context,
                )
                .await;
            }

            Request::CommMessage {
                id,
                from_session,
                message,
                to_session,
                channel,
                delivery,
                wake,
                tldr,
            } => {
                handle_comm_message(
                    id,
                    from_session,
                    message,
                    to_session,
                    channel,
                    delivery,
                    wake,
                    tldr,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &swarm_members,
                    &swarms_by_id,
                    &channel_subscriptions,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &client_connections,
                )
                .await;
            }

            Request::CommList {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_list(
                    id,
                    req_session_id,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &file_touch,
                    &sessions,
                    &client_connections,
                )
                .await;
            }

            Request::CommListChannels {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_list_channels(
                    id,
                    req_session_id,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                )
                .await;
            }

            Request::CommChannelMembers {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_channel_members(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                )
                .await;
            }

            Request::CommProposePlan {
                id,
                session_id: req_session_id,
                items,
            } => {
                handle_comm_propose_plan(
                    id,
                    req_session_id,
                    items,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &swarm_plans,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommApprovePlan {
                id,
                session_id: req_session_id,
                proposer_session,
            } => {
                handle_comm_approve_plan(
                    id,
                    req_session_id,
                    proposer_session,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &swarm_plans,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommRejectPlan {
                id,
                session_id: req_session_id,
                proposer_session,
                reason,
            } => {
                handle_comm_reject_plan(
                    id,
                    req_session_id,
                    proposer_session,
                    reason,
                    &client_event_tx,
                    &swarm_members,
                    &shared_context,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSeedGraph {
                id,
                session_id: req_session_id,
                mode,
                nodes,
            } => {
                super::comm_graph::handle_comm_seed_graph(
                    id,
                    req_session_id,
                    mode,
                    nodes,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommExpandNode {
                id,
                session_id: req_session_id,
                node_id,
                children,
            } => {
                super::comm_graph::handle_comm_expand_node(
                    id,
                    req_session_id,
                    node_id,
                    children,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommCompleteNode {
                id,
                session_id: req_session_id,
                node_id,
                artifact_json,
            } => {
                super::comm_graph::handle_comm_complete_node(
                    id,
                    req_session_id,
                    node_id,
                    artifact_json,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommInjectGap {
                id,
                session_id: req_session_id,
                gate_id,
                nodes,
            } => {
                super::comm_graph::handle_comm_inject_gap(
                    id,
                    req_session_id,
                    gate_id,
                    nodes,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommSpawn {
                id,
                session_id: req_session_id,
                working_dir,
                initial_message,
                request_nonce,
                spawn_mode,
                model,
                effort,
                label,
            } => {
                let spawn_mode = match parse_swarm_spawn_mode(id, spawn_mode, &client_event_tx) {
                    Some(spawn_mode) => spawn_mode,
                    None => return Ok(()),
                };
                handle_comm_spawn(
                    id,
                    req_session_id,
                    working_dir,
                    initial_message,
                    request_nonce,
                    spawn_mode,
                    model,
                    effort,
                    label,
                    &client_event_tx,
                    &sessions,
                    &global_session_id,
                    &provider_template,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &mcp_pool,
                    &soft_interrupt_queues,
                    &swarm_mutation_runtime,
                    &client_connections,
                )
                .await;
            }

            Request::CommListModels {
                id,
                session_id: req_session_id,
            } => {
                super::comm_session::handle_comm_list_models(
                    id,
                    &req_session_id,
                    &sessions,
                    &provider_template,
                    |event| {
                        let _ = client_event_tx.send(event);
                    },
                )
                .await;
            }

            Request::CommStop {
                id,
                session_id: req_session_id,
                target_session,
                force,
            } => {
                handle_comm_stop(
                    id,
                    req_session_id,
                    target_session,
                    force.unwrap_or(false),
                    &client_event_tx,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &soft_interrupt_queues,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommAssignRole {
                id,
                session_id: req_session_id,
                target_session,
                role,
            } => {
                handle_comm_assign_role(
                    id,
                    req_session_id,
                    target_session,
                    role,
                    &client_event_tx,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSummary {
                id,
                session_id: req_session_id,
                target_session,
                limit,
            } => {
                handle_comm_summary(
                    id,
                    req_session_id,
                    target_session,
                    limit,
                    &sessions,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommStatus {
                id,
                session_id: req_session_id,
                target_session,
            } => {
                handle_comm_status(
                    id,
                    req_session_id,
                    target_session,
                    &sessions,
                    &swarm_members,
                    &client_connections,
                    &file_touch,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommReport {
                id,
                session_id: req_session_id,
                status,
                message,
                validation,
                follow_up,
                tldr,
            } => {
                let status = status.unwrap_or_else(|| "ready".to_string());
                let report = format_structured_completion_report(
                    &message,
                    validation.as_deref(),
                    follow_up.as_deref(),
                );
                let detail = Some(truncate_detail(&message, 160));
                update_member_status_with_report_tldr(
                    &req_session_id,
                    &status,
                    detail,
                    Some(report),
                    tldr,
                    &swarm_members,
                    &swarms_by_id,
                    Some(&event_history),
                    Some(&event_counter),
                    Some(&swarm_event_tx),
                )
                .await;
                let _ = client_event_tx.send(ServerEvent::CommReportResponse {
                    id,
                    status,
                    message: "Report recorded and delivered to the coordinator when applicable."
                        .to_string(),
                });
            }

            Request::CommPlanStatus {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_plan_status(
                    id,
                    req_session_id,
                    &swarm_members,
                    &swarm_plans,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommReadContext {
                id,
                session_id: req_session_id,
                target_session,
            } => {
                handle_comm_read_context(
                    id,
                    req_session_id,
                    target_session,
                    &sessions,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommResyncPlan {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_resync_plan(
                    id,
                    req_session_id,
                    &CommResyncPlanContext {
                        client_event_tx: &client_event_tx,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        swarm_plans: &swarm_plans,
                        swarm_coordinators: &swarm_coordinators,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        swarm_event_tx: &swarm_event_tx,
                    },
                )
                .await;
            }

            Request::CommAssignTask {
                id,
                session_id: req_session_id,
                target_session,
                task_id,
                message,
            } => {
                handle_comm_assign_task(
                    id,
                    req_session_id,
                    target_session,
                    task_id,
                    message,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommAssignNext {
                id,
                session_id: req_session_id,
                target_session,
                working_dir,
                prefer_spawn,
                spawn_if_needed,
                message,
                model,
                effort,
            } => {
                handle_comm_assign_next(
                    id,
                    req_session_id,
                    target_session,
                    working_dir,
                    prefer_spawn,
                    spawn_if_needed,
                    message,
                    model,
                    effort,
                    &client_event_tx,
                    &sessions,
                    &global_session_id,
                    &provider_template,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &mcp_pool,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommTaskControl {
                id,
                session_id: req_session_id,
                action,
                task_id,
                target_session,
                message,
            } => {
                handle_comm_task_control(
                    id,
                    req_session_id,
                    action,
                    task_id,
                    target_session,
                    message,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSubscribeChannel {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_subscribe_channel(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommUnsubscribeChannel {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_unsubscribe_channel(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommAwaitMembers {
                id,
                session_id: req_session_id,
                target_status,
                session_ids: requested_ids,
                mode,
                timeout_secs,
                background,
                notify,
                wake,
            } => {
                handle_comm_await_members(
                    id,
                    req_session_id,
                    target_status,
                    requested_ids,
                    mode,
                    timeout_secs,
                    background,
                    notify,
                    wake,
                    CommAwaitMembersContext {
                        client_event_tx: &client_event_tx,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        swarm_event_tx: &swarm_event_tx,
                        await_members_runtime: &await_members_runtime,
                    },
                )
                .await;
            }

            // These are handled via channels, not direct requests from TUI
            Request::ClientDebugCommand { id, .. } => {
                handle_client_debug_command(id, &client_event_tx).await;
            }
            Request::ClientDebugResponse { id, output } => {
                handle_client_debug_response(id, output, &client_debug_response_tx);
            }
        }
        if request_lifecycle_logged {
            log_request_lifecycle_handled(
                ServerRequestLifecycleFields {
                    phase: "handled",
                    request_id,
                    request_kind: &request_kind,
                    client_session_id: &client_session_id,
                    client_connection_id: &client_connection_id,
                    client_instance_id: current_client_instance_id.as_deref(),
                    client_is_processing,
                    message_id: processing_message_id,
                    processing_session_id: processing_session_id.as_deref(),
                    line_bytes: line.len(),
                },
                request_lifecycle_start,
                request_decoded_at,
            );
        }
    }

    Ok(())
    }.await;

    // Dropping an inspection waiter requests Stop through the common execution
    // owner. Confirmed cleanup remains a durable transaction if the client leaves.
    drop(inspection_requests);
    crate::hooks::with_client_terminal_env(
        active_terminal_env,
        cleanup_client_connection(
            super::client_disconnect_cleanup::DepartingClient {
                session_id: &client_session_id,
                debug_id: &client_debug_id,
                connection_id: &client_connection_id,
            },
            event_handle,
            &swarm_members,
            &client_debug_state,
            &client_connections,
            &startup_context,
        ),
    )
    .await?;
    client_result
}

fn startup_context_session_snapshot(
    agent: &Arc<Mutex<Agent>>,
    session_id: &str,
) -> Result<
    super::startup_context::StartupContextSessionSnapshot,
    crate::protocol::StartupContextFailure,
> {
    if let Ok(agent) = agent.try_lock() {
        return Ok(
            super::startup_context::StartupContextSessionSnapshot::from_session(
                agent.startup_context_session(),
            ),
        );
    }
    let session = Session::load_startup_stub(session_id).map_err(|error| {
        super::startup_context::failure(
            crate::protocol::StartupContextOperation::Status,
            crate::protocol::StartupContextFailureKind::Io,
            format!("could not load Startup Context session metadata: {error}"),
            true,
        )
    })?;
    Ok(super::startup_context::StartupContextSessionSnapshot::from_session(&session))
}

#[allow(clippy::too_many_arguments)]
async fn startup_context_file_detail(
    coordinator: &super::startup_context::StartupContextCoordinator,
    agent: &Arc<Mutex<Agent>>,
    session_id: &str,
    batch_id: &str,
    spec_id: &str,
    message_id: &str,
    expected_sha256: &str,
    start_char: usize,
    max_chars: Option<usize>,
) -> Result<crate::protocol::StartupContextFileDetail, crate::protocol::StartupContextFailure> {
    if let Ok(agent) = agent.try_lock() {
        return coordinator.file_detail(
            agent.startup_context_session(),
            super::startup_context::FileDetailRequest {
                batch_id,
                spec_id,
                message_id,
                expected_sha256,
                start_char,
                max_chars,
            },
        );
    }
    let coordinator = coordinator.clone();
    let session_id = session_id.to_string();
    let batch_id = batch_id.to_string();
    let spec_id = spec_id.to_string();
    let message_id = message_id.to_string();
    let expected_sha256 = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        let session = Session::load_for_remote_startup(&session_id).map_err(|error| {
            super::startup_context::failure(
                crate::protocol::StartupContextOperation::FileDetail,
                crate::protocol::StartupContextFailureKind::Io,
                format!("could not load authoritative Startup Context history: {error}"),
                true,
            )
        })?;
        coordinator.file_detail(
            &session,
            super::startup_context::FileDetailRequest {
                batch_id: &batch_id,
                spec_id: &spec_id,
                message_id: &message_id,
                expected_sha256: &expected_sha256,
                start_char,
                max_chars,
            },
        )
    })
    .await
    .map_err(|error| {
        super::startup_context::failure(
            crate::protocol::StartupContextOperation::FileDetail,
            crate::protocol::StartupContextFailureKind::Internal,
            format!("Startup Context detail task failed: {error}"),
            true,
        )
    })?
}

async fn append_context_message(
    id: u64,
    content: &str,
    images: Vec<(String, String)>,
    client_session_id: &str,
    client_is_processing: bool,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    let Ok(mut agent) = agent.try_lock() else {
        send_agent_busy_error(
            id,
            "context_message",
            client_session_id,
            client_is_processing,
            client_event_tx,
        );
        return;
    };
    let result = agent.append_user_context_message(content, images);
    let event = match result {
        Ok(()) => ServerEvent::ContextMessageAdded { id },
        Err(error) => ServerEvent::Error {
            id,
            message: crate::util::format_error_chain(&error),
            retry_after_secs: None,
        },
    };
    let _ = client_event_tx.send(event);
}

#[allow(clippy::too_many_arguments)]
async fn start_processing_message(
    message: ProcessingMessage,
    client_session_id: &str,
    state: &mut ProcessingState<'_>,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &crate::client_delivery::ClientEventSender,
    host: &SessionAgents,
    client_terminal_env: Vec<(String, String)>,
    startup_context: &Arc<super::startup_context::StartupContextCoordinator>,
    swarm: &SwarmStatusRefs<'_>,
) {
    let id = message.id;
    if server_reload_starting() {
        let _ = client_event_tx.send(ServerEvent::Reloading { new_socket: None });
        return;
    }
    let mut admission = match host.admit(client_session_id, id, agent.clone()) {
        Ok(admission) => admission,
        Err(error) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: error.to_string(),
                retry_after_secs: None,
            });
            return;
        }
    };
    let output = super::primary_output::PrimaryOutput::new(
        client_session_id.to_string(),
        host.presentation(client_session_id),
        Arc::clone(swarm.members),
        Some(client_event_tx.clone()),
    );
    let tx = output.tx.clone();
    if let Some(skill) = message.activate_skill.as_deref() {
        match admission.agent.activate_skill(skill) {
            Ok(activation) => {
                let _ = tx.send(ServerEvent::SkillActivated {
                    id,
                    skill_id: activation.skill_id,
                    description: activation.description,
                    source: activation.source.kind.to_string(),
                });
            }
            Err(error) => {
                let _ = client_event_tx.send(ServerEvent::Error {
                    id,
                    message: format!("Skill activation failed before turn acceptance: {error}"),
                    retry_after_secs: None,
                });
                return;
            }
        }
    }
    *state.client_is_processing = true;
    *state.message_id = Some(id);
    *state.session_id = Some(client_session_id.to_string());
    if let Some(reminder) = message.system_reminder.as_deref()
        && let Err(error) = super::reload_recovery::mark_delivered_if_matching_continuation(
            client_session_id,
            reminder,
            "client_message_accepted",
        )
    {
        crate::logging::warn(&format!(
            "Failed to mark reload recovery intent delivered: {error}"
        ));
    }
    update_member_status(
        client_session_id,
        "running",
        Some(truncate_detail(&message.content, 120)),
        swarm.members,
        swarm.swarms_by_id,
        Some(swarm.event_history),
        Some(swarm.event_counter),
        Some(swarm.event_tx),
    )
    .await;
    let status = super::live_turn::LiveTurnSwarmContext::new(
        swarm.members,
        swarm.swarms_by_id,
        swarm.event_history,
        swarm.event_counter,
        swarm.event_tx,
    );
    let session = client_session_id.to_string();
    let startup_context = startup_context.clone();
    let terminal_tx = tx.clone();
    let snapshot_agent = agent.clone();
    let stdin = host.stdin(client_session_id, || {
        super::primary_stdin::PrimaryStdin::new(client_session_id.into(), swarm.members.clone())
    });
    admission.agent.set_stdin_request_tx(stdin.sender());
    admission.agent.primary_presentation = Some(host.presentation(client_session_id));
    host.start(
        admission,
        move |mut agent| async move {
            let start = agent.message_count();
            crate::hooks::with_client_terminal_env(
                client_terminal_env,
                process_admitted_message(&mut agent, startup_context, message, tx),
            )
            .await?;
            Ok(agent.latest_assistant_text_after(start))
        },
        move |outcome| async move {
            status.complete(&session, id, outcome, &terminal_tx).await;
            output.finish(&snapshot_agent).await;
        },
    );
}

async fn cancel_processing_message(
    state: &mut ProcessingState<'_>,
    session_control: &SessionControlHandle,
    client_event_tx: &crate::client_delivery::ClientEventSender,
    host: &SessionAgents,
    _swarm: &SwarmStatusRefs<'_>,
    request_id: Option<u64>,
    _request_decoded_at: Option<Instant>,
) {
    let session = &session_control.session_id;
    match host.stop(session).await {
        Ok(true) => {}
        Ok(false) => {
            // Compatibility control for an explicitly internal, non-hosted turn.
            if crate::turn_cancel_registry::has_active_turn(session) {
                let epoch = session_control.request_cancel();
                let control = session_control.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    control.reset_cancel_if_epoch(epoch);
                });
                let _ = client_event_tx.send(ServerEvent::Interrupted);
                if let Some(id) = *state.message_id {
                    let _ = client_event_tx.send(ServerEvent::Done { id });
                }
            } else {
                let _ = client_event_tx.send(ServerEvent::Interrupted);
            }
        }
        Err(error) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id: request_id.unwrap_or(0),
                message: error.to_string(),
                retry_after_secs: None,
            });
        }
    }
    *state.message_id = host.processing(session);
    *state.client_is_processing = state.message_id.is_some();
    *state.session_id = state.message_id.map(|_| session.clone());
}

fn try_available_models_snapshot(agent: &Arc<Mutex<Agent>>) -> Option<String> {
    let event = try_available_models_updated_event(agent)?;
    Some(available_models_dedup_key(&event))
}

/// Build a names-only copy of an `AvailableModelsUpdated` event by dropping the
/// per-model route expansion. Used when the fully-routed frame exceeds the live
/// update size cap so clients still receive fresh model names.
fn names_only_available_models_event(event: &ServerEvent) -> Option<ServerEvent> {
    let ServerEvent::AvailableModelsUpdated {
        provider_name,
        provider_model,
        available_models,
        ..
    } = event
    else {
        return None;
    };
    Some(ServerEvent::AvailableModelsUpdated {
        provider_name: provider_name.clone(),
        provider_model: provider_model.clone(),
        available_models: available_models.clone(),
        available_model_routes: Vec::new(),
    })
}

fn queue_soft_interrupt(
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    source: SoftInterruptSource,
    session_control: &SessionControlHandle,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    let content_bytes = content.len();
    let content_chars = content.chars().count();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_REQUEST id={} session={} source={:?} urgent={} content_bytes={} content_chars={}",
        id, session_control.session_id, source, urgent, content_bytes, content_chars
    ));
    let queued = session_control.queue_soft_interrupt(content, images, urgent, source);
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_RESULT id={} session={} queued={} ack_queued={}",
        id, session_control.session_id, queued, ack_queued
    ));
}

fn clear_soft_interrupts(
    id: u64,
    session_id: &str,
    session_control: &SessionControlHandle,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_REQUEST id={} session={} control_session={}",
        id, session_id, session_control.session_id
    ));
    session_control.clear_soft_interrupts();
    let persisted_clear = match crate::soft_interrupt_store::clear(session_id) {
        Ok(()) => true,
        Err(err) => {
            crate::logging::warn(&format!(
                "SERVER_SOFT_INTERRUPT_CLEAR_PERSISTED_FAILED id={} session={} error={}",
                id, session_id, err
            ));
            false
        }
    };
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_RESULT id={} session={} persisted_clear={} ack_queued={}",
        id, session_id, persisted_clear, ack_queued
    ));
}

fn move_tool_to_background(
    id: u64,
    session_control: &SessionControlHandle,
    client_event_tx: &crate::client_delivery::ClientEventSender,
) {
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_REQUEST id={} session={}",
        id, session_control.session_id
    ));
    let signalled = session_control.request_background_current_tool();
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_RESULT id={} session={} signalled={} ack_queued={}",
        id, session_control.session_id, signalled, ack_queued
    ));
}

/// Process a message and stream events (mpsc channel - per-client)
pub(super) async fn process_message_streaming_mpsc(
    agent: Arc<Mutex<Agent>>,
    content: &str,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let mut agent = agent.lock().await;
    let session_id = agent.session_id().to_string();
    let result = agent
        .run_once_streaming_mpsc(content, images, system_reminder, event_tx)
        .await;
    if result.is_ok() {
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "turn_completed",
                "message_turn_finished",
            )
            .with_session_id(session_id)
            .force_attribution(),
        );
        crate::process_memory::release_retained_heap_debounced(
            "server_turn_completed",
            std::time::Duration::from_secs(30),
        );
    }
    result
}

async fn process_admitted_message(
    agent: &mut Agent,
    startup_context: Arc<super::startup_context::StartupContextCoordinator>,
    message: ProcessingMessage,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let ProcessingMessage {
        id: request_id,
        queued_messages,
        content,
        images,
        system_reminder,
        observe_startup_context,
        activate_skill: _,
    } = message;
    let session_id = agent.session_id().to_string();
    emit_startup_apply_drain_events(&startup_context, agent, &event_tx);
    match observe_startup_context
        .then(|| startup_context.observe_before_user_turn(agent))
        .transpose()
    {
        Ok(None) => {}
        Ok(Some(outcome)) if outcome.receipt_changed => {
            crate::logging::info(&format!(
                "STARTUP_CONTEXT_STALE_OBSERVED session={} observed_files={} stale_files={} markers_appended={}",
                session_id,
                outcome.observed_file_count,
                outcome.stale_file_count,
                outcome.marker_count
            ));
        }
        Ok(Some(_)) => {}
        Err(error) => {
            crate::logging::warn(&format!(
                "STARTUP_CONTEXT_STALE_OBSERVATION_FAILED session={} error={}",
                session_id, error
            ));
            let _ = event_tx.send(ServerEvent::StartupContextFailed {
                id: 0,
                failure: super::startup_context::failure(
                    crate::protocol::StartupContextOperation::Observe,
                    crate::protocol::StartupContextFailureKind::Io,
                    format!(
                        "Could not save the latest Startup Context file observation. The turn will continue without claiming that a stale marker was delivered: {error}"
                    ),
                    true,
                ),
            });
        }
    }
    let result = if let Some(entries) = queued_messages {
        agent
            .run_queued_streaming_mpsc(
                Some(request_id),
                &entries,
                system_reminder,
                event_tx.clone(),
            )
            .await
    } else {
        agent
            .run_once_streaming_mpsc_correlated(
                request_id,
                &content,
                images,
                system_reminder,
                event_tx.clone(),
            )
            .await
    };
    emit_startup_apply_drain_events(&startup_context, agent, &event_tx);
    let startup_action = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<crate::agent::StartupContextActionRequiredError>())
        .map(|error| error.action.clone());
    let startup_session = startup_action.as_ref().map(|_| {
        super::startup_context::StartupContextSessionSnapshot::from_session(
            agent.startup_context_session(),
        )
    });
    if let (Some(action_required), Some(session)) = (startup_action, startup_session) {
        let snapshot = startup_context
            .status_snapshot(
                session,
                0,
                Some(crate::protocol::STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE),
                0,
                Some(crate::protocol::STARTUP_CONTEXT_STATUS_MAX_PAGE_SIZE),
            )
            .await;
        super::startup_context::emit_checked(
            &event_tx.clone().into(),
            request_id,
            crate::protocol::StartupContextOperation::Status,
            ServerEvent::StartupContextStatus {
                id: request_id,
                snapshot,
                action_required: Some(action_required),
            },
        );
    }
    if result.is_ok() {
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "turn_completed",
                "message_turn_finished",
            )
            .with_session_id(session_id)
            .force_attribution(),
        );
        crate::process_memory::release_retained_heap_debounced(
            "server_turn_completed",
            std::time::Duration::from_secs(30),
        );
    }
    result
}

fn emit_startup_apply_drain_events(
    startup_context: &super::startup_context::StartupContextCoordinator,
    agent: &mut Agent,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) {
    for status in startup_context.drain_pending_for_agent(agent) {
        super::startup_context::emit_checked(
            &event_tx.clone().into(),
            0,
            crate::protocol::StartupContextOperation::ApplySelection,
            ServerEvent::StartupContextApplyStatus { id: 0, status },
        );
    }
}

#[cfg(test)]
#[path = "client_lifecycle_tests.rs"]
mod tests;
