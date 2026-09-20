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
mod new_context;
mod transport;
pub use transport::{configured_launch, launch_enabled, launch_local_request};
pub(crate) mod presentation;
pub use launch::{PrimaryLauncher, PrimaryRegistryMode};

type Agents = HashMap<String, Arc<Mutex<Agent>>>;

pub struct PrimaryHost {
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
    stdin: StdMutex<HashMap<String, Arc<crate::server::primary_stdin::PrimaryStdin>>>,
    presentations: StdMutex<HashMap<String, Arc<presentation::Presentation>>>,
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
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.started
            && let Some(host) = self.host.upgrade()
        {
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
            stdin: StdMutex::new(HashMap::new()),
            presentations: StdMutex::new(HashMap::new()),
        }
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
        let owner = self.claim(session)?;
        let result = async {
            let stored = crate::session::Session::load_startup_stub(session)?;
            stored.require_published_primary()?;
            let previous = stored.status;
            let provider = provider.fork_for_new_session();
            let registry = crate::tool::Registry::new_for_shared_session(
                provider.clone(),
                pool.clone(),
                repositories.clone(),
            )
            .await?;
            Agent::restore_primary(session, provider, registry, repositories.clone(), owner)
                .map(|agent| (agent, previous))
        }
        .await;
        match result {
            Ok((agent, previous)) => {
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
                self.owners.lock().expect("primary owners").remove(session);
                Err(error)
            }
        }
    }

    pub(crate) fn accepts_input(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
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
        let mut owners = self.owners.lock().expect("primary owners");
        if let Some(owner) = owners.get(session) {
            return Ok(owner.clone());
        }
        let owner = Arc::new(PrimaryLease::acquire(session)?);
        owner.host.store(self.ownership_id, Ordering::Release);
        owners.insert(session.into(), owner.clone());
        Ok(owner)
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
        let mut turns = self.turns.lock().expect("primary turns");
        ensure!(
            self.accepting.load(Ordering::Acquire),
            "Primary runtime is stopping"
        );
        ensure!(!turns.contains_key(session), "Already processing a message");
        let mut agent = agent
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("Primary is busy"))?;
        ensure!(
            agent.session_id() == session,
            "Primary identity changed before admission"
        );
        agent.primary_owner = Some(self.adopt_owner(&agent)?);
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
        let mut task = TurnBody(tokio::spawn(async move {
            ensure!(
                !starting.stopping.load(Ordering::Acquire),
                "Primary stopped before provider dispatch"
            );
            body(agent).await
        }));
        *control.abort.lock().expect("primary body") = Some(task.0.abort_handle());
        reservation.started = true;
        let host = Arc::downgrade(self);
        let mut tasks = self.tasks.lock().expect("primary tasks");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let result = match (&mut task.0).await {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!("Primary turn ended: {error}")),
            };
            let interrupted = control.stopping.load(Ordering::Acquire) || control.cancel.is_set();
            let epoch = control.cancel.epoch();
            complete(TurnOutcome {
                result,
                interrupted,
            })
            .await;
            control.cancel.reset_if_epoch(epoch);
            let Some(host) = host.upgrade() else {
                return;
            };
            host.turns.lock().expect("primary turns").remove(&session);
            control.finished.send_replace(true);
            host.revision.send_modify(|r| *r = r.wrapping_add(1));
        });
    }

    pub(crate) async fn stop(&self, session: &str) -> Result<bool> {
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
        control.cancel.fire();
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

/// Shared by hosted and process-owned primaries. The kernel, not a PID or
/// attachment, decides whether another live writer can adopt a Session.
#[derive(Debug)]
pub(crate) struct PrimaryLease {
    pub session: String,
    host: std::sync::atomic::AtomicU64,
    _file: File,
}
impl PrimaryLease {
    pub(crate) fn acquire(session: &str) -> Result<Self> {
        ensure!(
            !session.is_empty()
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid primary Session identity"
        );
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
