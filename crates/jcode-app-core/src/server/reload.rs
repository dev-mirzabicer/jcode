use crate::server::reload_recovery::ReloadRecoveryRole;
use crate::tool::selfdev::ReloadContext;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

type SessionAgents = Arc<crate::primary::PrimaryHost>;

/// Bound for interrupting turns and publishing their checkpoints before exec.
const RELOAD_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(30);

struct ReloadAdmission {
    reservation: Option<crate::runtime_lifecycle::admission::ReloadReservation>,
    sessions: std::sync::Weak<crate::primary::PrimaryHost>,
}
impl Drop for ReloadAdmission {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            drop(reservation);
            if let Some(host) = self.sessions.upgrade() {
                let resume = host.clone();
                host.retain_delivery(async move {
                    if let Err(error) = resume.resume_deferred_inputs().await {
                        crate::logging::warn(&format!(
                            "Input retained after failed reload: {error:#}"
                        ));
                    }
                });
            }
        }
    }
}

fn prepare_server_exec(cmd: &mut std::process::Command, socket_path: &std::path::Path) {
    // The replacement daemon must own the published socket paths. Unlink them
    // before exec so we never inherit a stale on-disk endpoint through reload.
    crate::server::cleanup_socket_pair(socket_path);
    cmd.env_remove("JCODE_READY_FD");

    // The shared daemon may have inherited stderr from the client process that
    // originally spawned it. Once that client exits, later reload execs can hit
    // SIGPIPE during boot when they emit provider/model notices to stderr,
    // killing the replacement server before it binds the socket. The daemon
    // logs to the file logger, so detach stdio for exec-based reloads.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
}

async fn receive_reload_signal(
    rx: &mut watch::Receiver<Option<crate::server::ReloadSignal>>,
    last_request_id: &mut Option<String>,
) -> Option<crate::server::ReloadSignal> {
    // The reload watch channel keeps holding the last `Some(signal)` after it is
    // sent (it is never reset to `None`), so `borrow_and_update` would keep
    // handing back the same signal on every loop iteration. In production the
    // caller exec()s/exits after the first one, but a test-session listener just
    // `continue`s -- which previously turned into a hot busy-loop that starved
    // the runtime (single-threaded #[tokio::test]) and hung the reload e2e tests.
    // Dedupe by request_id so each distinct signal is delivered exactly once.
    loop {
        if let Some(signal) = rx.borrow_and_update().clone()
            && last_request_id.as_deref() != Some(signal.request_id.as_str())
        {
            *last_request_id = Some(signal.request_id.clone());
            return Some(signal);
        }

        if rx.changed().await.is_err() {
            return None;
        }
    }
}

pub(super) async fn await_reload_signal(
    sessions: Arc<crate::primary::PrimaryHost>,
    lifecycle: Option<std::sync::Weak<super::shutdown::RuntimeLifecycle>>,
) {
    let mut rx = super::reload_state::reload_signal().1.clone();
    // Treat any signal already sitting in the (process-global) reload channel as
    // already handled: a server should only react to reload signals issued after
    // it started listening, never to a stale one left over from a previous run.
    // Without this, in-process e2e servers (which share the global channel) would
    // each immediately re-process the last test's reload signal on startup.
    let mut last_request_id: Option<String> = rx
        .borrow_and_update()
        .as_ref()
        .map(|signal| signal.request_id.clone());

    loop {
        let signal = match receive_reload_signal(&mut rx, &mut last_request_id).await {
            Some(signal) => signal,
            None => return,
        };

        let reservation = crate::runtime_lifecycle::admission::current_runtime()
            .and_then(|runtime| runtime.map(|runtime| runtime.reserve_reload()).transpose());
        let admission = match reservation {
            Ok(reservation) => ReloadAdmission {
                reservation,
                sessions: Arc::downgrade(&sessions),
            },
            Err(error) => {
                crate::server::write_reload_state(
                    &signal.request_id,
                    &signal.hash,
                    crate::server::ReloadPhase::Failed,
                    signal.triggering_session.clone(),
                );
                crate::logging::error(&format!("Reload refused before effects: {error:#}"));
                continue;
            }
        };

        crate::logging::info(&format!(
            "Server: reload signal received via channel request={} hash={} triggering_session={:?} prefer_selfdev_binary={}",
            signal.request_id, signal.hash, signal.triggering_session, signal.prefer_selfdev_binary
        ));
        super::reload_trace::record_value(
            &signal.request_id,
            "signal_received",
            serde_json::json!({
                "hash": signal.hash,
                "triggering_session": signal.triggering_session,
                "prefer_selfdev_binary": signal.prefer_selfdev_binary,
            }),
        );
        let reload_started = Instant::now();
        crate::server::write_reload_state(
            &signal.request_id,
            &signal.hash,
            crate::server::ReloadPhase::Starting,
            signal.triggering_session.clone(),
        );
        // Acknowledge before quiescence: the selfdev initiator's tool returns on
        // this acknowledgement, so its own turn can then reach a safe boundary.
        super::acknowledge_reload_signal(&signal);

        if std::env::var("JCODE_TEST_SESSION")
            .map(|value| {
                let trimmed = value.trim();
                !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
        {
            crate::logging::info(
                "Server: JCODE_TEST_SESSION set, skipping process exec for reload test",
            );
            continue;
        }

        // The replacement is validated before any owned work is interrupted.
        let target = match validated_reload_target(signal.prefer_selfdev_binary).await {
            Ok(target) => target,
            Err(error) => {
                fail_reload(&signal, &sessions, admission, &error).await;
                continue;
            }
        };

        let fenced = admission.reservation.is_some();
        if let Err(error) = quiesce_for_reload(&signal, &sessions, fenced).await {
            fail_reload(&signal, &sessions, admission, &error).await;
            continue;
        }
        if let Err(error) = persist_reload_continuations(&signal, &sessions) {
            fail_reload(&signal, &sessions, admission, &error).await;
            continue;
        }
        if let Some(lifecycle) = lifecycle.as_ref().and_then(std::sync::Weak::upgrade)
            && let Err(error) = lifecycle.record_reload_handoff(&signal.request_id)
        {
            fail_reload(&signal, &sessions, admission, &error).await;
            continue;
        }
        crate::logging::info(&format!(
            "Server: verified reload quiescence request={} after {}ms state={}",
            signal.request_id,
            reload_started.elapsed().as_millis(),
            crate::server::reload_state_summary(Duration::from_secs(60))
        ));
        super::reload_trace::record_value(
            &signal.request_id,
            "quiescence_complete",
            serde_json::json!({ "elapsed_ms": reload_started.elapsed().as_millis() }),
        );

        let socket = super::socket_path();
        let (binary, label) = target;
        crate::logging::info(&format!(
            "Server: exec'ing into {} binary {:?} (socket: {:?}, prep={}ms)",
            label,
            binary,
            socket,
            reload_started.elapsed().as_millis(),
        ));
        super::reload_trace::record_value(
            &signal.request_id,
            "exec_start",
            serde_json::json!({
                "binary_label": label,
                "binary": binary,
                "socket": socket,
                "elapsed_ms": reload_started.elapsed().as_millis(),
            }),
        );
        let error = replace_runtime_process(&binary, &socket);
        crate::server::write_reload_state(
            &signal.request_id,
            &signal.hash,
            crate::server::ReloadPhase::Failed,
            Some(error.to_string()),
        );
        crate::logging::error(&format!(
            "Failed to exec into {} {:?}: {}. Interrupted turns and their continuations remain durable for the next start.",
            label, binary, error
        ));
        // The published sockets were already released for the replacement, so
        // this image cannot keep serving. Exit nonzero for supervision.
        std::process::exit(42);
    }
}

/// Replace this process image with the runtime binary serving `socket`. Only
/// returns on failure. The verified handoff must already be durable.
pub(super) fn replace_runtime_process(
    binary: &std::path::Path,
    socket: &std::path::Path,
) -> std::io::Error {
    let arguments = [
        std::ffi::OsString::from("serve"),
        "--socket".into(),
        socket.as_os_str().into(),
    ];
    replace_runtime_process_with(binary, socket, arguments)
}

/// Replace the process image with `binary` and exactly `arguments`.
pub(super) fn replace_runtime_process_with(
    binary: &std::path::Path,
    socket: &std::path::Path,
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> std::io::Error {
    let mut cmd = std::process::Command::new(binary);
    cmd.args(arguments);
    // Auto provider detection is dominated by credential-file probes.
    // The replacement process is the same trusted daemon with the same
    // environment, so carry the already-resolved, non-secret status
    // snapshot across exec instead of repeating those probes while the
    // socket is unavailable. Provider credentials themselves are still
    // loaded normally when their runtimes are constructed or used.
    if let Ok(auth_status) = serde_json::to_string(&crate::auth::AuthStatus::check_fast()) {
        cmd.env("JCODE_RELOAD_AUTH_STATUS", auth_status);
    }
    prepare_server_exec(&mut cmd, socket);
    crate::platform::replace_process(&mut cmd)
}

/// Resolve the replacement binary and prove it executes and reports its
/// version before any owned work is interrupted.
pub(super) async fn validated_reload_target(
    prefer_selfdev: bool,
) -> anyhow::Result<(std::path::PathBuf, &'static str)> {
    let (binary, label) = super::reload_exec_target(prefer_selfdev)
        .ok_or_else(|| anyhow::anyhow!("no reloadable binary found"))?;
    anyhow::ensure!(binary.exists(), "missing binary: {}", binary.display());
    let probe = binary.clone();
    tokio::task::spawn_blocking(move || crate::build::smoke_test_binary(&probe))
        .await?
        .map_err(|error| anyhow::anyhow!("replacement binary failed its smoke test: {error:#}"))?;
    Ok((binary, label))
}

/// Interrupt every primary turn at a reload boundary and wait for actual
/// terminal persistence, child and background quiescence and Session
/// checkpoints. A deadline is a failed reload, never permission to exec.
async fn quiesce_for_reload(
    signal: &crate::server::ReloadSignal,
    sessions: &SessionAgents,
    fenced: bool,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let interrupted = sessions.processing_sessions();
    super::reload_trace::record_value(
        &signal.request_id,
        "primary_turns_interrupting",
        serde_json::json!({ "sessions": interrupted }),
    );
    sessions.retain_interrupted_turns(true);
    tokio::time::timeout(RELOAD_QUIESCENCE_TIMEOUT, async {
        let (children, primaries) = tokio::join!(
            crate::execution::await_delegations_for_reload(RELOAD_QUIESCENCE_TIMEOUT),
            sessions.interrupt_runtime_with_cause(jcode_tool_types::StopCause::ReloadQuiescence),
        );
        children?;
        primaries?;
        let mut revision = sessions.subscribe();
        while !sessions.processing_sessions().is_empty() {
            revision
                .changed()
                .await
                .context("Primary host ended during reload quiescence")?;
        }
        // Finalize in-process background tasks (selfdev builds/tests, bash
        // tasks) before exec replaces this process image. exec runs no
        // destructors, so their children would otherwise leak and their
        // status files would read Running until a later orphan sweep.
        let aborted = crate::background::global()
            .abort_live_tasks_for_reload()
            .await
            .context("Owned background work could not be safely stopped")?;
        if aborted > 0 {
            crate::logging::info(&format!(
                "Server: finalized {aborted} in-process background task(s) before reload exec"
            ));
        }
        crate::delegation::release_idle_runtimes().context("Idle child runtime cleanup")?;
        if !fenced {
            // Only hosts without a runtime lifecycle owner (non-Unix) reach
            // this: there is no admission fence to make a checkpoint exact.
            // Their turns already settled above and saved at completion.
            crate::logging::warn(
                "Reload has no runtime admission fence; Session checkpoints rely on turn completion saves",
            );
            return Ok(());
        }
        sessions
            .checkpoint_runtime()
            .await
            .context("Primary Session checkpoint before reload")
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Reload quiescence deadline of {}s elapsed before owned work and checkpoints completed",
            RELOAD_QUIESCENCE_TIMEOUT.as_secs()
        )
    })?
}

/// One planned continuation per turn this reload actually interrupted. The
/// selfdev initiator keeps its exact reload-context directive when it belongs
/// to this reload; other turns receive the interrupted-turn continuation.
fn persist_reload_continuations(
    signal: &crate::server::ReloadSignal,
    sessions: &SessionAgents,
) -> anyhow::Result<()> {
    for record in sessions.retained_turn_records()? {
        let initiator = signal.triggering_session.as_deref() == Some(record.session.as_str());
        let context = initiator
            .then(|| {
                ReloadContext::peek_for_session(&record.session)
                    .ok()
                    .flatten()
            })
            .flatten()
            .filter(|context| context.version_after == signal.hash);
        let Some(directive) = ReloadContext::recovery_directive_for_session(
            &record.session,
            context.as_ref(),
            true,
            None,
        ) else {
            continue;
        };
        super::reload_recovery::persist_intent(
            &signal.request_id,
            &record.session,
            if context.is_some() {
                ReloadRecoveryRole::Initiator
            } else {
                ReloadRecoveryRole::InterruptedPeer
            },
            directive,
            if context.is_some() {
                "selfdev initiator interrupted by verified planned reload"
            } else {
                "turn interrupted by verified planned reload"
            },
        )?;
        if context.is_some() {
            // Consumed into the durable intent; it must not seed a later reload.
            let _ = ReloadContext::load_for_session(&record.session);
        }
        super::reload_trace::record_value(
            &signal.request_id,
            "intent_persisted",
            serde_json::json!({ "session_id": record.session, "initiator": context.is_some() }),
        );
    }
    Ok(())
}

/// The replacement did not happen and this incarnation keeps serving. Release
/// the fence first, then continue exactly the turns this attempt interrupted,
/// through the same durable input path a replacement would have used.
async fn fail_reload(
    signal: &crate::server::ReloadSignal,
    sessions: &SessionAgents,
    admission: ReloadAdmission,
    error: &anyhow::Error,
) {
    crate::server::write_reload_state(
        &signal.request_id,
        &signal.hash,
        crate::server::ReloadPhase::Failed,
        Some(format!("{error:#}")),
    );
    crate::logging::error(&format!("Reload did not replace the runtime: {error:#}"));
    crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(
        crate::bus::UiActivity::background(
            signal.triggering_session.clone(),
            format!("Reload did not replace the server: {error:#}"),
            Some("Reload failed; runtime kept running".to_string()),
        ),
    ));
    sessions.retain_interrupted_turns(false);
    if let Err(persist) = persist_reload_continuations(signal, sessions) {
        crate::logging::error(&format!(
            "Interrupted turns keep their durable records; continuation needs inspection: {persist:#}"
        ));
    } else if let Err(settle) = sessions.settle_retained_turn_records() {
        crate::logging::error(&format!(
            "Interrupted turn records could not be settled after failed reload: {settle:#}"
        ));
    }
    drop(admission);
    if let Ok(swarm) = sessions.input_delivery_context() {
        let delivered = super::supervision::deliver_planned_continuations(sessions, swarm).await;
        crate::logging::info(&format!(
            "Failed reload continued {delivered} interrupted turn(s) in the running runtime"
        ));
    }
}

/// Failed-reload handling without a runtime admission reservation, for
/// mechanism tests of the local continuation path.
#[cfg(test)]
pub(super) async fn fail_reload_for_test(
    signal: &crate::server::ReloadSignal,
    sessions: &SessionAgents,
    error: &anyhow::Error,
) {
    let admission = ReloadAdmission {
        reservation: None,
        sessions: Arc::downgrade(sessions),
    };
    fail_reload(signal, sessions, admission, error).await;
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod reload_tests;
