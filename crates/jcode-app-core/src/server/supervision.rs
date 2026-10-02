//! Runtime supervision over the existing owners: planned continuation after a
//! verified restart or reload, trusted selection after an unexpected exit, and
//! power inhibition derived from runtime-owned work. Delivery always goes
//! through the durable primary input store and its normal admission; this
//! module adds no scheduler, queue or second input engine.
use super::live_turn::LiveTurnSwarmContext;
use super::reload_recovery::{self, ReloadRecoveryRole};
use crate::primary::PrimaryHost;
use crate::runtime_lifecycle::PlannedTransition;
use crate::runtime_lifecycle::turns::TurnRecord;
use crate::workspace::runtime::*;
use crate::workspace::{RequestId, Revision};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;

/// Mirza-approved continuation (2026-10-02) for a turn continued by explicit
/// selection after an unexpected runtime exit or external termination.
pub(crate) const SELECTED_RECOVERY_CONTINUATION: &str = "Your previous turn ended when the Jcode runtime stopped unexpectedly, and the user chose to continue it. Tools that were running may have stopped partway; check their retained results before repeating any effect. Continue from where you left off.";

/// The replacement incarnation persists one continuation intent per turn its
/// predecessor interrupted for a verified planned transition. A reload that
/// already recorded an exact directive (for example its selfdev initiator)
/// keeps it; other interrupted turns receive the existing interrupted-turn
/// continuation.
pub(crate) fn persist_planned_continuation(
    record: &TurnRecord,
    planned: PlannedTransition,
) -> Result<()> {
    let reload_id = match &planned {
        PlannedTransition::Reload { reload } => reload.clone(),
        PlannedTransition::Restart { operation } => format!("restart-{operation}"),
    };
    if let Some(existing) = reload_recovery::peek_for_session(&record.session)?
        && existing.status == reload_recovery::ReloadRecoveryStatus::Pending
        && existing.reload_id == reload_id
    {
        return Ok(());
    }
    let directive = crate::tool::selfdev::ReloadContext::recovery_directive_for_session(
        &record.session,
        None,
        true,
        None,
    )
    .context("Interrupted turn has no continuation directive")?;
    reload_recovery::persist_runtime_intent(
        &reload_id,
        &record.session,
        ReloadRecoveryRole::InterruptedPeer,
        directive,
        match planned {
            PlannedTransition::Reload { .. } => "turn interrupted by verified planned reload",
            PlannedTransition::Restart { .. } => "turn interrupted by verified planned restart",
        },
    )
}

/// Deliver every pending planned continuation through durable primary input.
/// The correlated input ID makes a repeated startup or failed-reload pass
/// idempotent; the intent is retired only after the input store accepted it.
/// A session awaiting a crash-recovery decision never receives one.
pub(crate) async fn deliver_planned_continuations(
    host: &Arc<PrimaryHost>,
    swarm: LiveTurnSwarmContext,
) -> usize {
    let records = match reload_recovery::pending_records() {
        Ok(records) => records,
        Err(error) => {
            crate::logging::error(&format!(
                "Planned continuations are retained but could not be listed: {error:#}"
            ));
            return 0;
        }
    };
    let mut delivered = 0;
    for record in records {
        // Headless members keep their established startup continuation owner.
        // Intents written before runtime-owned continuation keep their
        // original semantics (continued when a client reattaches): they carry
        // no verified evidence that the turn was interrupted by this runtime's
        // planned transition, so the runtime never wakes them on its own.
        if record.role == ReloadRecoveryRole::Headless || !record.runtime_owned {
            continue;
        }
        let superseded = match host
            .runtime_journals()
            .map(|journals| journals.recovery.unresolved(&record.session_id))
            .transpose()
        {
            Ok(items) => items.is_some_and(|items| !items.is_empty()),
            Err(error) => {
                // Unknown recovery state: neither deliver nor discard.
                crate::logging::warn(&format!(
                    "Planned continuation for {} waits: its recovery state cannot be read: {error:#}",
                    record.session_id
                ));
                continue;
            }
        };
        if superseded {
            // An unexpected exit superseded the planned transition. Its trusted
            // decision owns continuation; the stale plan is retired.
            if let Err(error) = reload_recovery::discard_for_session(&record.session_id) {
                crate::logging::warn(&format!(
                    "Stale planned continuation for {} remains on disk: {error:#}",
                    record.session_id
                ));
            }
            continue;
        }
        let mut input = jcode_session_types::PrimaryInputEnvelope::new(
            record.session_id.clone(),
            String::new(),
            jcode_session_types::PrimaryInputDelivery::NextTurn,
        );
        input.id = crate::primary_input::correlated_input_id(
            "planned-continuation",
            &format!("{}:{}", record.reload_id, record.session_id),
        );
        input.system_reminder = Some(record.directive.continuation_message.clone());
        match super::live_turn::submit_primary_input(host, input, swarm.clone()).await {
            Ok(_) => {
                delivered += 1;
                if let Err(error) = reload_recovery::mark_delivered_if_matching_continuation(
                    &record.session_id,
                    &record.directive.continuation_message,
                    "runtime_planned_continuation",
                ) {
                    crate::logging::warn(&format!(
                        "Planned continuation for {} was accepted but its intent could not be retired; a repeat is deduplicated by input ID: {error:#}",
                        record.session_id
                    ));
                }
            }
            Err(error) => crate::logging::warn(&format!(
                "Planned continuation for {} remains pending: {error:#}",
                record.session_id
            )),
        }
    }
    delivered
}

fn recovery_input_id(item: RecoveryId) -> RequestId {
    crate::primary_input::correlated_input_id("runtime-recovery", &item.to_string())
}

fn recovery_input(
    item: &RecoveryItem,
    input: RequestId,
) -> jcode_session_types::PrimaryInputEnvelope {
    let mut envelope = jcode_session_types::PrimaryInputEnvelope::new(
        item.session.clone(),
        String::new(),
        jcode_session_types::PrimaryInputDelivery::NextTurn,
    );
    envelope.id = input;
    envelope.system_reminder = Some(SELECTED_RECOVERY_CONTINUATION.into());
    envelope
}

/// Accept continuation input for Continue decisions whose acceptance did not
/// complete before an exit. Already accepted inputs are left to their owner.
pub(crate) async fn redeliver_continued(host: &Arc<PrimaryHost>, swarm: LiveTurnSwarmContext) {
    let Some(journals) = host.runtime_journals() else {
        return;
    };
    let continued = match journals.recovery.continued() {
        Ok(continued) => continued,
        Err(error) => {
            crate::logging::error(&format!(
                "Recovery decisions could not be inspected for redelivery: {error:#}"
            ));
            return;
        }
    };
    let store = crate::primary_input::PrimaryInputStore::current();
    for (item, input) in continued {
        if matches!(store.original(&item.session, input), Ok(Some(_))) {
            continue;
        }
        if let Err(error) = super::live_turn::submit_primary_input(
            host,
            recovery_input(&item, input),
            swarm.clone(),
        )
        .await
        {
            crate::logging::warn(&format!(
                "Selected recovery continuation for {} remains pending: {error:#}",
                item.session
            ));
        }
    }
}

/// Apply one trusted decision. The decision is durable before its effect, so a
/// lost reply or exit is recovered by replaying the same request.
pub(crate) async fn decide(
    host: &Arc<PrimaryHost>,
    item: RecoveryId,
    expected: Revision,
    request: RequestId,
    decision: RecoveryDecision,
) -> Result<RecoveryItem> {
    let journals = host
        .runtime_journals()
        .context("This runtime has no recovery journal")?;
    let current = journals.recovery.inspect(item)?;
    let resolution = match decision {
        RecoveryDecision::Continue => {
            ensure!(
                crate::session::Session::load_startup_stub(&current.session)
                    .and_then(|session| session.require_published_primary())
                    .is_ok(),
                "Session {} cannot be continued: it is not a published primary",
                current.session
            );
            RecoveryResolution::Continued {
                input: recovery_input_id(item),
            }
        }
        RecoveryDecision::LeaveStopped => RecoveryResolution::LeftStopped {},
    };
    let resolved = journals
        .recovery
        .resolve(item, expected, request, resolution)?;
    if let Some(RecoveryResolution::Continued { input }) = resolved
        .resolved
        .as_ref()
        .map(|resolved| &resolved.resolution)
    {
        let swarm = host.input_delivery_context()?;
        if !matches!(
            crate::primary_input::PrimaryInputStore::current().original(&resolved.session, *input),
            Ok(Some(_))
        ) {
            super::live_turn::submit_primary_input(host, recovery_input(&resolved, *input), swarm)
                .await
                .context("Continuation decision is recorded; its input will be retried at the next runtime start")?;
        }
    } else if let Ok(swarm) = host.input_delivery_context() {
        // Deferred automatic inputs may now be delivered normally.
        super::live_turn::ensure_primary_input_delivery(host, &resolved.session, swarm);
    }
    Ok(resolved)
}

/// Fresh unresolved-execution facts for presentation. Never authority.
fn recovery_executions(session: &str) -> Vec<RecoveryExecution> {
    let Ok(store) =
        crate::execution::ExecutionStore::open(&crate::storage::jcode_dir().unwrap_or_default())
    else {
        return Vec::new();
    };
    let Ok(runs) = store.unresolved_runs() else {
        return Vec::new();
    };
    runs.into_iter()
        .filter(|run| run.session_id == session)
        .map(|run| RecoveryExecution {
            live_owner: store
                .runtime_endpoint(&run.owner)
                .ok()
                .flatten()
                .is_some_and(|endpoint| endpoint.has_live_lease().unwrap_or(false)),
            id: run.id,
            tool: run.tool,
            state: format!("{:?}", run.state).to_lowercase(),
        })
        .collect()
}

pub(crate) fn recoveries(host: &PrimaryHost) -> Result<Vec<RecoveryItem>> {
    let Some(journals) = host.runtime_journals() else {
        return Ok(Vec::new());
    };
    let mut items = journals.recovery.list()?;
    for item in items.iter_mut().filter(|item| item.resolved.is_none()) {
        item.executions = recovery_executions(&item.session);
    }
    Ok(items)
}

/// Process-wide power observation published by the runtime's monitor.
static POWER: std::sync::Mutex<Option<PowerStatus>> = std::sync::Mutex::new(None);

pub(crate) fn power_status() -> PowerStatus {
    POWER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or(PowerStatus {
            enabled: crate::config::config().power.prevent_sleep_while_streaming,
            available: crate::power_inhibit::PowerInhibitor::new().is_available(),
            active: false,
            active_work: 0,
        })
}

/// Keep the machine awake while runtime-owned work exists: admitted primary
/// turns, executions, preparations and legacy background tasks. Attached
/// clients, idle sessions and human waits never count. The user switch is
/// evaluated every reconcile, so it applies without a restart.
pub(crate) fn spawn_power_monitor(
    lifecycle: std::sync::Weak<super::shutdown::RuntimeLifecycle>,
    fallback: std::sync::Weak<PrimaryHost>,
) {
    tokio::spawn(async move {
        let mut inhibitor = crate::power_inhibit::PowerInhibitor::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = None;
        loop {
            interval.tick().await;
            let active_work = if let Some(lifecycle) = lifecycle.upgrade() {
                match lifecycle.work().await {
                    Ok(work) => work.len(),
                    Err(error) => {
                        // Uncertain observation keeps the machine awake rather
                        // than releasing an assertion under unseen work.
                        crate::logging::warn(&format!(
                            "power_inhibit: runtime work observation failed: {error:#}"
                        ));
                        usize::MAX
                    }
                }
            } else if let Some(host) = fallback.upgrade() {
                host.processing_sessions().len()
            } else {
                return;
            };
            let enabled = crate::config::config().power.prevent_sleep_while_streaming;
            let wanted = enabled && active_work > 0;
            inhibitor.set_active(wanted);
            let status = PowerStatus {
                enabled,
                available: inhibitor.is_available(),
                active: inhibitor.is_held(),
                active_work: active_work.min(u32::MAX as usize),
            };
            if last.as_ref() != Some(&(status.active, status.enabled)) {
                crate::logging::info(&format!(
                    "power_inhibit: {} (enabled={}, runtime work={})",
                    if status.active { "holding" } else { "released" },
                    status.enabled,
                    status.active_work
                ));
                last = Some((status.active, status.enabled));
            }
            *POWER.lock().unwrap_or_else(|error| error.into_inner()) = Some(status);
        }
    });
}

/// The process was started by the namespaced login service.
pub(crate) fn supervised() -> bool {
    std::env::var_os(crate::runtime_service::SUPERVISED_ENV).is_some()
}
