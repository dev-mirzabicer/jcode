//! Primary execution ownership. Connections hold subscriptions, never turn tasks.
use crate::agent::Agent;
use anyhow::{Context, Result, ensure};
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, watch};
use tokio::task::{AbortHandle, JoinSet};

mod launch;
mod location;
mod new_context;
mod placement;
pub use placement::{place_process_primary, require_process_primary_placed};
mod shutdown;
pub use new_context::{
    prepare_local_clear_session, prepare_split_session, prepare_transfer_session,
};
mod transport;
pub use transport::{configured_launch, launch_enabled, launch_local_request};
pub(crate) mod presentation;
pub use launch::{PrimaryLauncher, PrimaryRegistryMode};

type Agents = HashMap<String, Arc<Mutex<Agent>>>;

pub struct PrimaryHost {
    runtime_admission:
        std::sync::OnceLock<Arc<crate::runtime_lifecycle::admission::RuntimeAdmission>>,
    input_restore: std::sync::OnceLock<InputRestore>,
    startup_context:
        std::sync::OnceLock<Arc<crate::server::startup_context::StartupContextCoordinator>>,
    ownership_id: u64,
    agents: RwLock<Agents>,
    owners: StdMutex<HashMap<String, Arc<PrimaryLease>>>,
    turns: StdMutex<HashMap<String, Arc<TurnControl>>>,
    tasks: StdMutex<JoinSet<()>>,
    revision: watch::Sender<u64>,
    restoring: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    provisional: StdMutex<HashSet<String>>,
    resources: StdMutex<HashMap<String, PrimaryResources>>,
    accepting: AtomicBool,
    input_drains: StdMutex<HashSet<String>>,
    input_events: StdMutex<Option<crate::server::LiveTurnSwarmContext>>,
    stdin: StdMutex<HashMap<String, Arc<crate::server::primary_stdin::PrimaryStdin>>>,
    presentations: StdMutex<HashMap<String, Arc<presentation::Presentation>>>,
    checkpoint: StdMutex<Option<shutdown::Checkpoint>>,
    journals: std::sync::OnceLock<RuntimeJournals>,
    /// Set by a planned restart, reload or external-signal exit before it
    /// interrupts turns. Their admission records then survive as evidence for
    /// the next incarnation instead of settling as ordinary interruptions.
    retain_interrupted: AtomicBool,
}

/// Durable turn admission and recovery records of the bound runtime namespace.
/// Hosts without a runtime (tests, process-owned callers) keep neither.
#[derive(Clone)]
pub(crate) struct RuntimeJournals {
    pub turns: crate::runtime_lifecycle::turns::TurnJournal,
    pub recovery: crate::runtime_lifecycle::recovery::RecoveryStore,
}

struct InputRestore {
    provider: Arc<dyn crate::provider::Provider>,
    pool: Arc<tokio::sync::OnceCell<Arc<crate::mcp::SharedMcpPool>>>,
    repositories: Arc<crate::instruction::InstructionRepositoryService>,
}

#[derive(Clone)]
pub(crate) struct PrimaryResources {
    pub provider: Arc<dyn crate::provider::Provider>,
    pub registry: crate::tool::Registry,
    pub interrupts: crate::agent::SoftInterruptQueue,
    pub background: InterruptSignal,
    pub shutdown: InterruptSignal,
}
impl PrimaryResources {
    fn from_agent(agent: &Agent) -> Self {
        Self {
            provider: agent.provider_handle(),
            registry: agent.registry(),
            interrupts: agent.soft_interrupt_queue(),
            background: agent.background_tool_signal(),
            shutdown: agent.graceful_shutdown_signal(),
        }
    }
}

struct TurnControl {
    request_id: u64,
    cancel: InterruptSignal,
    abort: StdMutex<Option<AbortHandle>>,
    finished: watch::Sender<bool>,
    stopping: AtomicBool,
}

struct TurnBody(tokio::task::JoinHandle<Result<Option<String>>>);
impl Drop for TurnBody {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) struct Admission {
    pub agent: OwnedMutexGuard<Agent>,
    reservation: Reservation,
}

struct Reservation {
    session: String,
    host: std::sync::Weak<PrimaryHost>,
    control: Arc<TurnControl>,
    started: bool,
    permit: Option<crate::runtime_lifecycle::admission::WorkPermit>,
    turn: Option<crate::runtime_lifecycle::turns::TurnRecord>,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.started
            && let Some(host) = self.host.upgrade()
        {
            host.settle_turn_record(self.turn.take());
            host.turns
                .lock()
                .expect("primary turns")
                .remove(&self.session);
            self.control.finished.send_replace(true);
            host.revision.send_modify(|r| *r = r.wrapping_add(1));
        }
    }
}

pub(crate) struct TurnOutcome {
    pub result: Result<Option<String>>,
    pub interrupted: bool,
}

impl Default for PrimaryHost {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

impl PrimaryHost {
    pub fn new(agents: Agents) -> Self {
        static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let resources = agents
            .iter()
            .filter_map(|(id, agent)| {
                agent
                    .try_lock()
                    .ok()
                    .map(|agent| (id.clone(), PrimaryResources::from_agent(&agent)))
            })
            .collect();
        Self {
            input_restore: std::sync::OnceLock::new(),
            runtime_admission: std::sync::OnceLock::new(),
            startup_context: std::sync::OnceLock::new(),
            ownership_id: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
            agents: RwLock::new(agents),
            owners: StdMutex::new(HashMap::new()),
            turns: StdMutex::new(HashMap::new()),
            tasks: StdMutex::new(JoinSet::new()),
            revision: watch::channel(0).0,
            restoring: Mutex::new(HashMap::new()),
            provisional: StdMutex::new(HashSet::new()),
            resources: StdMutex::new(resources),
            accepting: AtomicBool::new(true),
            input_drains: StdMutex::new(HashSet::new()),
            input_events: StdMutex::new(None),
            stdin: StdMutex::new(HashMap::new()),
            presentations: StdMutex::new(HashMap::new()),
            checkpoint: StdMutex::new(None),
            journals: std::sync::OnceLock::new(),
            retain_interrupted: AtomicBool::new(false),
        }
    }

    /// Keep interrupted turns' admission records for the next incarnation.
    /// Only a transition that ends this process image may set this, and it
    /// is cleared if that transition fails and this incarnation continues.
    pub(crate) fn retain_interrupted_turns(&self, retain: bool) {
        self.retain_interrupted.store(retain, Ordering::SeqCst);
    }

    pub(crate) fn bind_runtime_journals(&self, journals: RuntimeJournals) -> Result<()> {
        ensure!(
            self.journals.set(journals).is_ok(),
            "Primary host already has runtime journals"
        );
        Ok(())
    }

    pub(crate) fn runtime_journals(&self) -> Option<&RuntimeJournals> {
        self.journals.get()
    }

    /// Settle a turn's admission record after its terminal outcome persisted.
    /// A failure leaves evidence that the next runtime presents for review,
    /// never a silent continuation.
    fn settle_turn_record(&self, record: Option<crate::runtime_lifecycle::turns::TurnRecord>) {
        if let (Some(record), Some(journals)) = (record, self.journals.get())
            && let Err(error) = journals.turns.remove(&record)
        {
            crate::logging::error(&format!(
                "Primary turn record for {} could not be settled; a later runtime will present it for recovery review: {error:#}",
                record.session
            ));
        }
    }

    /// Sessions whose turns this host currently owns.
    pub(crate) fn processing_sessions(&self) -> Vec<String> {
        let mut sessions = self
            .turns
            .lock()
            .expect("primary turns")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort();
        sessions
    }

    // Existing runtime inspection and new-context coordinators share this same
    // registry. These guards are crate-private, not a public launch protocol.
    pub(crate) async fn read(&self) -> RwLockReadGuard<'_, Agents> {
        self.agents.read().await
    }
    pub(crate) async fn write(&self) -> RwLockWriteGuard<'_, Agents> {
        self.agents.write().await
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub(crate) fn stdin(
        &self,
        session: &str,
        create: impl FnOnce() -> crate::server::primary_stdin::PrimaryStdin,
    ) -> Arc<crate::server::primary_stdin::PrimaryStdin> {
        self.stdin
            .lock()
            .expect("primary stdin owners")
            .entry(session.into())
            .or_insert_with(|| Arc::new(create()))
            .clone()
    }
    pub(crate) fn pending_stdin(&self, session: &str) -> Vec<crate::protocol::ServerEvent> {
        self.stdin
            .lock()
            .expect("primary stdin owners")
            .get(session)
            .map(|input| input.pending())
            .unwrap_or_default()
    }
    pub(crate) fn respond_stdin(&self, session: &str, request: &str, input: String) -> Result<()> {
        self.stdin
            .lock()
            .expect("primary stdin owners")
            .get(session)
            .ok_or_else(|| anyhow::anyhow!("Primary has no pending input"))?
            .respond(request, input)
    }

    pub(crate) fn presentation(&self, session: &str) -> Arc<presentation::Presentation> {
        self.presentations
            .lock()
            .expect("primary presentations")
            .entry(session.into())
            .or_insert_with(|| Arc::new(presentation::Presentation::new(session)))
            .clone()
    }

    pub(crate) fn resources(
        &self,
        session: &str,
        agent: &Arc<Mutex<Agent>>,
    ) -> Result<PrimaryResources> {
        let mut resources = self.resources.lock().expect("primary resources");
        if let Ok(agent) = agent.try_lock() {
            ensure!(
                agent.session_id() == session,
                "Primary resource identity changed"
            );
            let value = PrimaryResources::from_agent(&agent);
            resources.insert(session.into(), value.clone());
            return Ok(value);
        }
        resources
            .get(session)
            .cloned()
            .context("Primary resources are unavailable during unregistered work")
    }

    #[cfg(test)]
    pub(crate) fn mark_provisional(&self, session: &str) {
        self.provisional
            .lock()
            .expect("primary placeholders")
            .insert(session.to_string());
    }
    pub(crate) fn is_provisional(&self, session: &str) -> bool {
        self.provisional
            .lock()
            .expect("primary placeholders")
            .contains(session)
    }
    pub(crate) fn release_provisional(&self, session: &str) {
        self.provisional
            .lock()
            .expect("primary placeholders")
            .remove(session);
        self.owners.lock().expect("primary owners").remove(session);
        self.resources
            .lock()
            .expect("primary resources")
            .remove(session);
    }

    pub(crate) async fn restore(
        &self,
        session: &str,
        provider: &Arc<dyn crate::provider::Provider>,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        repositories: &crate::instruction::InstructionRepositoryService,
    ) -> Result<Option<crate::session::SessionStatus>> {
        self.restore_mode(session, provider, pool, repositories, false)
            .await
    }

    pub(crate) fn configure_input_restore(
        &self,
        provider: Arc<dyn crate::provider::Provider>,
        pool: Arc<tokio::sync::OnceCell<Arc<crate::mcp::SharedMcpPool>>>,
        repositories: Arc<crate::instruction::InstructionRepositoryService>,
    ) {
        self.input_restore.get_or_init(|| InputRestore {
            provider,
            pool,
            repositories,
        });
    }

    pub(crate) async fn restore_input_recipient(&self, session: &str) -> Result<()> {
        if self.read().await.contains_key(session) {
            return Ok(());
        }
        let context = self
            .input_restore
            .get()
            .context("Cold primary input needs the runtime restore factory")?;
        let pool = crate::server::get_shared_mcp_pool(&context.pool).await;
        self.restore(session, &context.provider, &pool, &context.repositories)
            .await?;
        Ok(())
    }

    pub(crate) async fn restore_for_location_repair(
        &self,
        session: &str,
        provider: &Arc<dyn crate::provider::Provider>,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        repositories: &crate::instruction::InstructionRepositoryService,
    ) -> Result<Option<crate::session::SessionStatus>> {
        self.restore_mode(session, provider, pool, repositories, true)
            .await
    }

    async fn restore_mode(
        &self,
        session: &str,
        provider: &Arc<dyn crate::provider::Provider>,
        pool: &Arc<crate::mcp::SharedMcpPool>,
        repositories: &crate::instruction::InstructionRepositoryService,
        location_repair: bool,
    ) -> Result<Option<crate::session::SessionStatus>> {
        let gate = self
            .restoring
            .lock()
            .await
            .entry(session.to_string())
            .or_default()
            .clone();
        let _gate = gate.lock().await;
        if self.read().await.contains_key(session) {
            return Ok(None);
        }
        let permit = crate::runtime_lifecycle::admission::preparation(
            "primary-restore",
            Some(session.into()),
        )?;
        let (owner, acquired) = self.claim_tracked(session)?;
        let release = || {
            if acquired {
                self.owners.lock().expect("primary owners").remove(session);
            }
        };
        let result: Result<_> = crate::runtime_lifecycle::admission::scope(permit.clone(), async {
            let stored = crate::session::Session::load_startup_stub(session)?;
            if location_repair {
                stored.require_primary_publication()?;
            } else {
                stored.require_published_primary()?;
            }
            let previous = stored.status;
            let provider = provider.fork_for_new_session();
            let registry = crate::tool::Registry::new_for_shared_session(
                provider.clone(),
                pool.clone(),
                repositories.clone(),
            )
            .await?;
            // Boxed: the Agent stays alive across the registration await, and
            // its inline size would otherwise enlarge every caller's future.
            let agent = Box::new(if location_repair {
                Agent::restore_primary_for_location_repair(
                    session,
                    provider,
                    registry,
                    repositories.clone(),
                    owner,
                )
            } else {
                Agent::restore_primary(session, provider, registry, repositories.clone(), owner)
            }?);
            // Same tool surface as launch and new contexts. A restored primary
            // (reload continuation, detached input, recovery) may make its first
            // request before any client subscribes; without this its frozen tool
            // set would lose MCP tools and regain them on attach.
            register_restored_mcp(&agent, pool, session).await;
            Ok((*agent, previous))
        })
        .await;
        match result {
            Ok((agent, previous)) => {
                if !crate::runtime_lifecycle::admission::sync_scope(permit.clone(), || {
                    self.accepts_prepared_work()
                }) {
                    release();
                    anyhow::bail!(
                        "Runtime stopped before restored primary publication; saved Session and input remain retained"
                    );
                }
                self.resources
                    .lock()
                    .expect("primary resources")
                    .insert(session.to_string(), PrimaryResources::from_agent(&agent));
                self.write()
                    .await
                    .insert(session.to_string(), Arc::new(Mutex::new(agent)));
                Ok(Some(previous))
            }
            Err(error) => {
                release();
                Err(error)
            }
        }
    }

    pub(crate) fn accepts_input(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
            && self
                .runtime_admission()
                .is_ok_and(|runtime| runtime.is_none_or(|runtime| runtime.accepts_input()))
    }

    pub(crate) fn accepts_prepared_work(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
            && self
                .runtime_admission()
                .is_ok_and(|runtime| runtime.is_none_or(|runtime| runtime.permits_current_work()))
    }

    pub(crate) fn bind_runtime_admission(
        &self,
        gate: Arc<crate::runtime_lifecycle::admission::RuntimeAdmission>,
    ) -> Result<()> {
        let bound = self.runtime_admission.get_or_init(|| gate.clone());
        ensure!(
            Arc::ptr_eq(bound, &gate),
            "Primary host belongs to another runtime admission owner"
        );
        Ok(())
    }

    fn runtime_admission(
        &self,
    ) -> Result<Option<Arc<crate::runtime_lifecycle::admission::RuntimeAdmission>>> {
        match self.runtime_admission.get() {
            Some(gate) => Ok(Some(gate.clone())),
            None => crate::runtime_lifecycle::admission::current_runtime(),
        }
    }

    pub(crate) fn configure_startup_context(
        &self,
        coordinator: Arc<crate::server::startup_context::StartupContextCoordinator>,
    ) {
        assert!(
            self.startup_context.set(coordinator).is_ok(),
            "Primary startup coordinator already configured"
        );
    }
    pub(crate) fn startup_context(
        &self,
    ) -> Option<Arc<crate::server::startup_context::StartupContextCoordinator>> {
        self.startup_context.get().cloned()
    }

    pub(crate) fn configure_input_delivery(&self, context: crate::server::LiveTurnSwarmContext) {
        self.input_events
            .lock()
            .expect("primary input event context")
            .get_or_insert(context);
    }

    pub(crate) fn input_delivery_context(&self) -> Result<crate::server::LiveTurnSwarmContext> {
        self.input_events
            .lock()
            .expect("primary input event context")
            .clone()
            .context("Primary input delivery is not attached to a runtime")
    }

    pub(crate) fn input_drain(self: &Arc<Self>, session: &str) -> Option<PrimaryInputDrain> {
        self.input_drains
            .lock()
            .expect("primary input drains")
            .insert(session.into())
            .then(|| PrimaryInputDrain {
                host: Arc::downgrade(self),
                session: session.into(),
                finished: false,
                runtime_deferred: false,
            })
    }

    pub(crate) fn retain_delivery(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().expect("primary tasks");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task);
    }

    pub(crate) fn processing(&self, session: &str) -> Option<u64> {
        self.turns
            .lock()
            .expect("primary turns")
            .get(session)
            .map(|t| t.request_id)
    }

    /// Retain kernel ownership until this runtime releases the primary. A PID,
    /// socket, client count or elapsed timeout never authorizes another writer.
    #[cfg(test)]
    pub(crate) fn own(&self, session: &str) -> Result<()> {
        self.claim(session).map(|_| ())
    }
    pub(crate) fn claim(&self, session: &str) -> Result<Arc<PrimaryLease>> {
        self.claim_tracked(session).map(|(owner, _)| owner)
    }

    /// Claim the owner lease and report whether this call acquired it. Only
    /// the acquiring caller may release a failed claim: another holder (for
    /// example the input drain) keeps its lease, and removing its map entry
    /// would make the next restore in this runtime contend with itself.
    fn claim_tracked(&self, session: &str) -> Result<(Arc<PrimaryLease>, bool)> {
        let mut owners = self.owners.lock().expect("primary owners");
        if let Some(owner) = owners.get(session) {
            return Ok((owner.clone(), false));
        }
        let owner = Arc::new(PrimaryLease::acquire(session)?);
        owner.host.store(self.ownership_id, Ordering::Release);
        owners.insert(session.into(), owner.clone());
        Ok((owner, true))
    }
    pub(crate) fn adopt_owner(&self, agent: &Agent) -> Result<Arc<PrimaryLease>> {
        let session = agent.session_id();
        if let Some(owner) = &agent.primary_owner {
            ensure!(
                owner.session == session,
                "Primary ownership identity changed"
            );
            let claimed = owner.host.compare_exchange(
                0,
                self.ownership_id,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            ensure!(
                claimed.is_ok() || claimed == Err(self.ownership_id),
                "Primary is owned by another runtime"
            );
            self.owners
                .lock()
                .expect("primary owners")
                .insert(session.into(), owner.clone());
            Ok(owner.clone())
        } else {
            self.claim(session)
        }
    }

    pub(crate) fn admit(
        self: &Arc<Self>,
        session: &str,
        request_id: u64,
        agent: Arc<Mutex<Agent>>,
    ) -> Result<Admission> {
        let permit = self
            .runtime_admission()?
            .map(|runtime| {
                runtime.independent(
                    crate::workspace::runtime::RuntimeWorkKind::PrimaryTurn,
                    format!("primary:{session}:{}", uuid::Uuid::new_v4()),
                    Some(session.to_owned()),
                )
            })
            .transpose()?;
        let mut turns = self.turns.lock().expect("primary turns");
        ensure!(
            self.accepting.load(Ordering::Acquire),
            "Primary runtime is stopping"
        );
        ensure!(!turns.contains_key(session), "Already processing a message");
        *self.checkpoint.lock().expect("runtime checkpoint") = None;
        let mut agent = agent
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("Primary is busy"))?;
        ensure!(
            agent.session_id() == session,
            "Primary identity changed before admission"
        );
        agent.primary_owner = Some(self.adopt_owner(&agent)?);
        // Durable admission evidence precedes any provider dispatch. Without it
        // an unexpected exit could not be presented for selected recovery.
        let turn = self
            .journals
            .get()
            .map(|journals| journals.turns.begin(session))
            .transpose()
            .context("Primary turn admission could not be recorded")?;
        let presentation = self.presentation(session);
        presentation.begin(request_id);
        let control = Arc::new(TurnControl {
            // The stable Registry/provider objects stay inspectable while the
            // Agent guard is held by inference.
            request_id,
            cancel: agent.graceful_shutdown_signal(),
            abort: StdMutex::new(None),
            finished: watch::channel(false).0,
            stopping: AtomicBool::new(false),
        });
        self.resources
            .lock()
            .expect("primary resources")
            .insert(session.to_string(), PrimaryResources::from_agent(&agent));
        turns.insert(session.to_string(), control.clone());
        self.revision.send_modify(|r| *r = r.wrapping_add(1));
        Ok(Admission {
            agent,
            reservation: Reservation {
                session: session.to_string(),
                host: Arc::downgrade(self),
                control,
                started: false,
                permit,
                turn,
            },
        })
    }

    /// The retained supervisor always settles the result, even when Stop aborts
    /// the body after cooperative cancellation. Client delivery is not involved.
    pub(crate) fn start<F, Fut, C, Done>(
        self: &Arc<Self>,
        admission: Admission,
        body: F,
        complete: C,
    ) where
        F: FnOnce(OwnedMutexGuard<Agent>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<String>>> + Send + 'static,
        C: FnOnce(TurnOutcome) -> Done + Send + 'static,
        Done: Future<Output = ()> + Send + 'static,
    {
        let Admission {
            agent,
            mut reservation,
        } = admission;
        let session = reservation.session.clone();
        let control = reservation.control.clone();
        let starting = control.clone();
        let permit = reservation.permit.clone();
        let turn_record = reservation.turn.take();
        let mut task = TurnBody(tokio::spawn(crate::runtime_lifecycle::admission::scope(
            permit,
            async move {
                ensure!(
                    !starting.stopping.load(Ordering::Acquire),
                    "Primary stopped before provider dispatch"
                );
                body(agent).await
            },
        )));
        *control.abort.lock().expect("primary body") = Some(task.0.abort_handle());
        reservation.started = true;
        let host = Arc::downgrade(self);
        let mut tasks = self.tasks.lock().expect("primary tasks");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let _reservation = reservation;
            let mut result = match (&mut task.0).await {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!("Primary turn ended: {error}")),
            };
            let mut interrupted =
                control.stopping.load(Ordering::Acquire) || control.cancel.is_set();
            let epoch = control.cancel.epoch();
            // Aborting the Agent only ends its waiter. The shared execution
            // owner still has to drain and seal foreground results. Reload
            // has its existing handoff/quiescence owner (including selfdev).
            if interrupted
                && control.cancel.stop_cause()
                    != Some(jcode_tool_types::StopCause::ReloadQuiescence)
                && let Err(error) = crate::execution::await_primary_foreground(&session).await
            {
                interrupted = false;
                result = Err(error.context("Primary Stop could not verify foreground completion"));
            }

            complete(TurnOutcome {
                result,
                interrupted,
            })
            .await;
            control.cancel.reset_if_epoch(epoch);
            let Some(host) = host.upgrade() else {
                return;
            };
            if interrupted && host.retain_interrupted.load(Ordering::SeqCst) {
                // The replacement incarnation classifies this exact record.
                drop(turn_record);
            } else {
                host.settle_turn_record(turn_record);
            }
            host.turns.lock().expect("primary turns").remove(&session);
            control.finished.send_replace(true);
            host.revision.send_modify(|r| *r = r.wrapping_add(1));
        });
    }

    pub(crate) async fn stop(&self, session: &str) -> Result<bool> {
        self.stop_with_cause(session, jcode_tool_types::StopCause::HumanCancellation)
            .await
    }

    async fn stop_with_cause(
        &self,
        session: &str,
        cause: jcode_tool_types::StopCause,
    ) -> Result<bool> {
        let control = self
            .turns
            .lock()
            .expect("primary turns")
            .get(session)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        let mut finished = control.finished.subscribe();
        control.stopping.store(true, Ordering::Release);
        control.cancel.fire_with_cause(cause);
        if tokio::time::timeout(
            std::time::Duration::from_millis(500),
            finished.wait_for(|done| *done),
        )
        .await
        .is_err()
        {
            if let Some(abort) = control.abort.lock().expect("primary body").as_ref() {
                abort.abort();
            }
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                finished.wait_for(|done| *done),
            )
            .await
            .context("Primary Stop is still waiting for terminal persistence")??;
        }
        Ok(true)
    }

    pub(crate) async fn wait_idle(&self, session: &str) -> Result<()> {
        let mut revision = self.subscribe();
        while self.processing(session).is_some() {
            revision.changed().await?;
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.accepting.store(false, Ordering::Release);
        let sessions: Vec<_> = self
            .turns
            .lock()
            .expect("primary turns")
            .keys()
            .cloned()
            .collect();
        for session in sessions {
            self.stop(&session).await?;
            self.wait_idle(&session).await?;
        }
        let mut tasks = std::mem::take(&mut *self.tasks.lock().expect("primary tasks"));
        while let Some(result) = tasks.join_next().await {
            result?;
        }
        let inputs = self
            .stdin
            .lock()
            .expect("primary stdin owners")
            .drain()
            .map(|(_, input)| input)
            .collect::<Vec<_>>();
        for input in inputs {
            input.shutdown().await;
        }
        Ok(())
    }
}

pub(crate) struct PrimaryInputDrain {
    host: std::sync::Weak<PrimaryHost>,
    session: String,
    finished: bool,
    runtime_deferred: bool,
}
impl PrimaryInputDrain {
    pub(crate) fn defer_for_runtime(&mut self) {
        self.runtime_deferred = true;
    }
    /// Admission persists before checking this same gate. Recheck durable
    /// emptiness while retiring the worker so a concurrent acceptance cannot
    /// mistake a departing worker for an active delivery owner.
    pub(crate) fn finish_if_empty(&mut self) -> Result<bool> {
        let Some(host) = self.host.upgrade() else {
            return Ok(true);
        };
        let mut drains = host.input_drains.lock().expect("primary input drains");
        if !crate::primary_input::PrimaryInputStore::current()
            .pending(&self.session)?
            .is_empty()
        {
            return Ok(false);
        }
        drains.remove(&self.session);
        self.finished = true;
        Ok(true)
    }
}
impl Drop for PrimaryInputDrain {
    fn drop(&mut self) {
        if !self.finished
            && let Some(host) = self.host.upgrade()
        {
            host.input_drains
                .lock()
                .expect("primary input drains")
                .remove(&self.session);
            // Cancel can race a drain that already observed the fence. If its
            // old registration made Cancel's kick a no-op, restart only this
            // explicitly deferred drain after retiring that registration.
            if self.runtime_deferred
                && host.accepts_input()
                && let Ok(context) = host.input_delivery_context()
            {
                crate::server::ensure_primary_input_delivery(&host, &self.session, context);
            }
        }
    }
}

/// Shared by hosted and process-owned primaries. The kernel, not a PID or
/// attachment, decides whether another live writer can adopt a Session.
#[derive(Debug)]
pub(crate) struct PrimaryLease {
    pub session: String,
    host: std::sync::atomic::AtomicU64,
    _file: File,
}
impl PrimaryLease {
    pub(crate) fn validate_identity(session: &str) -> Result<()> {
        ensure!(
            !session.is_empty()
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid primary Session identity"
        );
        Ok(())
    }

    pub(crate) fn acquire(session: &str) -> Result<Self> {
        Self::validate_identity(session)?;
        let path = crate::session::session_path(session)?;
        let parent = path.parent().context("Session has no storage parent")?;
        crate::storage::ensure_dir(parent)?;
        let coordination = parent.canonicalize()?.join(".writers");
        crate::storage::ensure_dir(&coordination)?;
        let metadata = std::fs::symlink_metadata(&coordination)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Primary coordination directory identity changed"
        );
        let path = coordination.join(format!("primary-{session}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Primary owner lease is not a regular file"
        );
        file.try_lock()
            .map_err(|e| anyhow::anyhow!("Primary {session} is owned by another runtime: {e}"))?;
        Ok(Self {
            session: session.into(),
            host: std::sync::atomic::AtomicU64::new(0),
            _file: file,
        })
    }
}

impl Drop for PrimaryHost {
    fn drop(&mut self) {
        for owner in self.owners.get_mut().expect("primary owners").values() {
            let _ = owner.host.compare_exchange(
                self.ownership_id,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

pub(crate) fn scope_rejection(
    id: u64,
    source: &str,
    error: &anyhow::Error,
) -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::ScopedContextRejected {
        id,
        source_session: source.into(),
        issue: error
            .downcast_ref::<crate::workspace::Issue>()
            .cloned()
            .unwrap_or_else(|| crate::workspace::Issue {
                code: crate::workspace::IssueCode::RecoveryRequired,
                detail: format!("{error:#}"),
            }),
    }
}

/// Restore is awaited inside client tasks whose stacks are close to their
/// limit in unoptimized builds. Keeping the registration future's concrete type
/// in this non-inlined function means restore's state holds only a pointer.
#[inline(never)]
fn register_restored_mcp(
    agent: &Agent,
    pool: &Arc<crate::mcp::SharedMcpPool>,
    session: &str,
) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    let registry = agent.registry();
    let pool = pool.clone();
    let session = session.to_string();
    let cwd = agent.working_dir().map(std::path::PathBuf::from);
    Box::pin(async move {
        registry
            .register_mcp_tools_for_dir(None, Some(pool), Some(session), cwd)
            .await;
    })
}
