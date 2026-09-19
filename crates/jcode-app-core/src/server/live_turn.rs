//! Server-initiated ("wake") turns for live sessions.
//!
//! Several server paths start a full conversation turn in a session without
//! that session's client sending a message: swarm DM/broadcast wake delivery,
//! background-task completion wakes, scheduled-task delivery, and post-reload
//! resume. Those turns must keep the same bookkeeping as client-initiated
//! turns, otherwise the swarm member status stays "ready/idle" while the agent
//! is actually streaming and attached TUIs never learn the turn finished.
//!
//! This module is the single shared implementation: it marks the member
//! `running` while the turn streams, flips it back to `ready` (with a
//! completion report) or `failed` at the end, and fans out a terminal
//! `Done`/`Error` event (id 0) so attached clients can settle the externally
//! started turn in their UI.

use super::{
    SwarmEvent, SwarmMember, session_event_fanout_sender, truncate_detail, update_member_status,
    update_member_status_with_report,
};
use crate::agent::Agent;
use crate::instruction::notification::Notification;
use crate::protocol::ServerEvent;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{Mutex, RwLock, broadcast};

type SessionAgents = Arc<crate::primary::PrimaryHost>;

pub(super) struct TrackedLiveTurn {
    pub(super) message: String,
    pub(super) system_reminder: Option<LiveTurnReminder>,
    pub(super) display_role: Option<crate::session::StoredDisplayRole>,
    pub(super) unattended_context:
        Option<jcode_session_types::StoredUnattendedContextAuthorization>,
    pub(super) status_detail: Option<String>,
}

pub(super) enum LiveTurnReminder {
    /// Already-rendered recovery text remains owned by the recovery subsystem.
    Rendered(String),
    Managed(Notification<'static>),
}

impl LiveTurnReminder {
    fn render(self, agent: &Agent) -> anyhow::Result<String> {
        match self {
            Self::Rendered(text) => Ok(text),
            Self::Managed(notification) => {
                Ok(notification.render(agent.working_dir().map(std::path::Path::new))?)
            }
        }
    }
}

/// Swarm bookkeeping handles needed to keep member status accurate around a
/// server-initiated turn.
#[derive(Clone)]
pub(super) struct LiveTurnSwarmContext {
    pub members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
    pub event_counter: Arc<AtomicU64>,
    pub event_tx: broadcast::Sender<SwarmEvent>,
}

impl LiveTurnSwarmContext {
    pub(super) fn new(
        members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
        swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
        event_history: &Arc<RwLock<VecDeque<SwarmEvent>>>,
        event_counter: &Arc<AtomicU64>,
        event_tx: &broadcast::Sender<SwarmEvent>,
    ) -> Self {
        Self {
            members: Arc::clone(members),
            swarms_by_id: Arc::clone(swarms_by_id),
            event_history: Arc::clone(event_history),
            event_counter: Arc::clone(event_counter),
            event_tx: event_tx.clone(),
        }
    }
}

impl LiveTurnSwarmContext {
    pub(super) async fn complete(
        &self,
        session: &str,
        id: u64,
        outcome: crate::primary::TurnOutcome,
        tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    ) {
        if let Err(error) = &outcome.result {
            let retry_after = error
                .downcast_ref::<jcode_agent_runtime::StreamError>()
                .and_then(|e| e.retry_after_secs);
            let message = error.to_string();
            let lower = message.to_lowercase();
            if retry_after.is_some() {
                crate::telemetry::record_error(crate::telemetry::ErrorCategory::RateLimited);
            } else if lower.contains("timeout") {
                crate::telemetry::record_error(crate::telemetry::ErrorCategory::ProviderTimeout);
            } else if crate::provider::error_looks_like_credential_failure(&message)
                || lower.contains("403 forbidden")
            {
                crate::telemetry::record_error(crate::telemetry::ErrorCategory::AuthFailed);
            }
        }
        let terminal = if outcome.interrupted {
            update_member_status(
                session,
                "stopped",
                Some("cancelled".into()),
                &self.members,
                &self.swarms_by_id,
                Some(&self.event_history),
                Some(&self.event_counter),
                Some(&self.event_tx),
            )
            .await;
            let _ = tx.send(ServerEvent::Interrupted);
            ServerEvent::Done { id }
        } else {
            match outcome.result {
                Ok(report) => {
                    update_member_status_with_report(
                        session,
                        "ready",
                        None,
                        report,
                        &self.members,
                        &self.swarms_by_id,
                        Some(&self.event_history),
                        Some(&self.event_counter),
                        Some(&self.event_tx),
                    )
                    .await;
                    ServerEvent::Done { id }
                }
                Err(error) => {
                    update_member_status(
                        session,
                        "failed",
                        Some(truncate_detail(&error.to_string(), 120)),
                        &self.members,
                        &self.swarms_by_id,
                        Some(&self.event_history),
                        Some(&self.event_counter),
                        Some(&self.event_tx),
                    )
                    .await;
                    ServerEvent::Error {
                        id,
                        message: crate::util::format_error_chain(&error),
                        retry_after_secs: error
                            .downcast_ref::<jcode_agent_runtime::StreamError>()
                            .and_then(|e| e.retry_after_secs),
                    }
                }
            }
        };
        // Same ordered stream as final MessageEnd. No client owns settlement.
        let _ = tx.send(terminal);
    }
}

/// Discovery only. Actual admission retains the Agent guard in PrimaryHost.
pub(super) async fn idle_live_agent(
    session_id: &str,
    sessions: &SessionAgents,
    _members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> Option<Arc<Mutex<Agent>>> {
    if sessions.processing(session_id).is_some() {
        return None;
    }
    let agent = sessions.read().await.get(session_id).cloned()?;
    let idle = agent.try_lock().is_ok();
    idle.then_some(agent)
}

pub(super) async fn spawn_tracked_live_turn(
    session_id: &str,
    agent: Arc<Mutex<Agent>>,
    host: &SessionAgents,
    turn: TrackedLiveTurn,
    swarm: LiveTurnSwarmContext,
) -> bool {
    let admission = match host.admit(session_id, 0, agent) {
        Ok(admission) => admission,
        Err(error) => {
            crate::logging::warn(&format!(
                "Primary wake not admitted for {session_id}: {error}"
            ));
            return false;
        }
    };
    update_member_status(
        session_id,
        "running",
        turn.status_detail,
        &swarm.members,
        &swarm.swarms_by_id,
        Some(&swarm.event_history),
        Some(&swarm.event_counter),
        Some(&swarm.event_tx),
    )
    .await;
    let tx = session_event_fanout_sender(session_id.to_string(), swarm.members.clone());
    let terminal_tx = tx.clone();
    let session = session_id.to_string();
    host.start(
        admission,
        move |mut agent| async move {
            let start = agent.message_count();
            let reminder = turn.system_reminder.map(|r| r.render(&agent)).transpose()?;
            if let Some(role) = turn.display_role {
                agent
                    .run_once_streaming_mpsc_with_display_role_and_unattended(
                        &turn.message,
                        vec![],
                        reminder,
                        tx,
                        Some(role),
                        turn.unattended_context,
                    )
                    .await?;
            } else {
                agent
                    .run_once_streaming_mpsc(&turn.message, vec![], reminder, tx)
                    .await?;
            }
            Ok(agent.latest_assistant_text_after(start))
        },
        move |outcome| async move {
            swarm.complete(&session, 0, outcome, &terminal_tx).await;
        },
    );
    true
}

/// Run `message` immediately as a tracked turn if the session is live and
/// idle. Returns `true` when the turn was started.
pub(super) async fn run_live_turn_if_idle(
    session_id: &str,
    message: &str,
    system_reminder: Option<LiveTurnReminder>,
    sessions: &SessionAgents,
    swarm: LiveTurnSwarmContext,
) -> bool {
    let Some(agent) = idle_live_agent(session_id, sessions, &swarm.members).await else {
        return false;
    };
    let detail = Some(truncate_detail(message, 120)).filter(|detail| !detail.is_empty());
    spawn_tracked_live_turn(
        session_id,
        agent,
        sessions,
        TrackedLiveTurn {
            message: message.to_string(),
            system_reminder,
            display_role: None,
            unattended_context: None,
            status_detail: detail,
        },
        swarm,
    )
    .await
}

pub(super) async fn run_live_system_turn_if_idle(
    session_id: &str,
    message: &str,
    unattended_context: Option<jcode_session_types::StoredUnattendedContextAuthorization>,
    sessions: &SessionAgents,
    swarm: LiveTurnSwarmContext,
) -> bool {
    let Some(agent) = idle_live_agent(session_id, sessions, &swarm.members).await else {
        return false;
    };
    let detail = Some(truncate_detail(message, 120)).filter(|detail| !detail.is_empty());
    spawn_tracked_live_turn(
        session_id,
        agent,
        sessions,
        TrackedLiveTurn {
            message: message.to_string(),
            system_reminder: None,
            display_role: Some(crate::session::StoredDisplayRole::System),
            unattended_context,
            status_detail: detail,
        },
        swarm,
    )
    .await
}
