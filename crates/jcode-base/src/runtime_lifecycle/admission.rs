//! In-process admission shared by primary, child and producer owners. A scope
//! is an unforgeable live capability, never derived from a caller's Session ID.
use super::RuntimeStopOwner;
use anyhow::{Context, Result, ensure};
use jcode_workspace_types::{OperationId, RequestId, ReviewId, Revision, runtime::*};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

static RUNTIMES: LazyLock<Mutex<BTreeMap<PathBuf, Weak<RuntimeAdmission>>>> =
    LazyLock::new(Default::default);
tokio::task_local! { static SCOPE: WorkPermit; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Running,
    Draining,
    Stopping,
}

struct State {
    mode: Mode,
    work: BTreeMap<String, RuntimeWork>,
}

pub struct RuntimeAdmission {
    state: Mutex<State>,
    identity: String,
}

/// The caller retains this registration for the runtime's complete lifetime.
/// There is no process-global default that could gate an unrelated local Agent.
pub struct RuntimeRegistration {
    root: PathBuf,
    admission: Arc<RuntimeAdmission>,
}
impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state.mode = Mode::Stopping;
        if !state.work.is_empty() {
            return;
        }
        let mut runtimes = RUNTIMES.lock().unwrap_or_else(|p| p.into_inner());
        if runtimes
            .get(&self.root)
            .and_then(Weak::upgrade)
            .is_some_and(|current| Arc::ptr_eq(&current, &self.admission))
        {
            runtimes.remove(&self.root);
        }
    }
}

#[derive(Clone)]
pub struct WorkPermit(Arc<Permit>);
struct Permit {
    admission: Arc<RuntimeAdmission>,
    key: String,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.admission
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .work
            .remove(&self.key);
    }
}

impl RuntimeAdmission {
    pub fn register(root: &Path, identity: &str) -> Result<RuntimeRegistration> {
        let root = root.canonicalize()?;
        let mut runtimes = RUNTIMES
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission registry is poisoned"))?;
        ensure!(
            runtimes.get(&root).and_then(Weak::upgrade).is_none(),
            "A runtime admission owner already uses this state namespace"
        );
        let admission = Arc::new(Self {
            identity: identity.into(),
            state: Mutex::new(State {
                mode: Mode::Running,
                work: BTreeMap::new(),
            }),
        });
        runtimes.insert(root.clone(), Arc::downgrade(&admission));
        Ok(RuntimeRegistration { root, admission })
    }

    pub fn for_root(root: &Path) -> Result<Option<Arc<Self>>> {
        let mut runtimes = RUNTIMES
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission registry is poisoned"))?;
        runtimes.retain(|_, runtime| runtime.strong_count() > 0);
        if runtimes.is_empty() {
            return Ok(None);
        }
        let root = root.canonicalize()?;
        Ok(runtimes.get(&root).and_then(Weak::upgrade))
    }

    pub fn accepts_input(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.mode == Mode::Running)
    }

    /// Serialize the complete synchronous append/checkpoint boundary with Begin,
    /// not a check that releases the fence before appending new user input.
    pub fn input_boundary<T>(&self, append: impl FnOnce() -> T) -> Option<T> {
        let state = self.state.lock().ok()?;
        if state.mode != Mode::Running {
            return None;
        }
        Some(append())
    }

    pub fn independent(
        self: &Arc<Self>,
        kind: RuntimeWorkKind,
        id: String,
        session: Option<String>,
    ) -> Result<WorkPermit> {
        self.admit(kind, id, session, false)
    }

    pub fn causal(
        self: &Arc<Self>,
        kind: RuntimeWorkKind,
        id: String,
        session: Option<String>,
    ) -> Result<WorkPermit> {
        self.admit(kind, id, session, true)
    }

    fn admit(
        self: &Arc<Self>,
        kind: RuntimeWorkKind,
        id: String,
        session: Option<String>,
        causal: bool,
    ) -> Result<WorkPermit> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?;
        let parent = current_scope();
        let admitted_parent = causal
            && parent.as_ref().is_some_and(|parent| {
                Arc::ptr_eq(&parent.0.admission, self) && state.work.contains_key(&parent.0.key)
            });
        ensure!(
            state.mode == Mode::Running || (state.mode == Mode::Draining && admitted_parent),
            "Runtime shutdown defers new independent work"
        );
        ensure!(
            !state.work.contains_key(&id),
            "Runtime work already has an admission owner"
        );
        state.work.insert(
            id.clone(),
            RuntimeWork {
                id: id.clone(),
                owner: self.identity.clone(),
                session,
                kind,
                supported_survivor: false,
            },
        );
        Ok(WorkPermit(Arc::new(Permit {
            admission: self.clone(),
            key: id,
        })))
    }

    pub fn work(&self) -> Result<Vec<RuntimeWork>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?
            .work
            .values()
            .cloned()
            .collect())
    }

    pub fn review(
        &self,
        owner: &RuntimeStopOwner,
        options: ShutdownOptions,
        observed: Vec<RuntimeWork>,
    ) -> Result<ShutdownReview> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?;
        ensure!(
            state.mode == Mode::Running,
            "Runtime shutdown already fences admission"
        );
        owner.review(options, merge_work(&state, observed))
    }

    pub fn begin(
        &self,
        owner: &RuntimeStopOwner,
        request: RequestId,
        review: ReviewId,
        observed: Vec<RuntimeWork>,
    ) -> Result<ShutdownOperation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?;
        let result = owner.begin(request, review, merge_work(&state, observed));
        // Even an error can follow atomic publication (e.g. directory fsync).
        // Reconcile authority while still holding the fence; uncertainty closes
        // admission rather than pretending the durable write was rolled back.
        self.reconcile(&mut state, owner)?;
        result
    }

    pub fn cancel_wait(
        &self,
        owner: &RuntimeStopOwner,
        operation: OperationId,
        expected: Revision,
    ) -> Result<ShutdownOperation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?;
        let result = owner.cancel_wait(operation, expected);
        self.reconcile(&mut state, owner)?;
        result
    }

    /// Final drain observation and phase change exclude concurrent admission.
    /// Independent surviving workers have already released their proxy permits.
    pub fn enter_stopping(
        &self,
        owner: &RuntimeStopOwner,
        operation: OperationId,
        expected: Revision,
    ) -> Result<ShutdownOperation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime admission is poisoned"))?;
        ensure!(
            state.mode == Mode::Draining && state.work.is_empty(),
            "Runtime still has admitted work"
        );
        let result = owner.enter_stopping(operation, expected);
        self.reconcile(&mut state, owner)?;
        result
    }

    fn reconcile(&self, state: &mut State, owner: &RuntimeStopOwner) -> Result<()> {
        state.mode = Mode::Stopping;
        let status = owner.status()?;
        ensure!(
            owner.identity() == self.identity,
            "Runtime admission and durable owner disagree"
        );
        state.mode = match status.operation.map(|op| op.phase) {
            Some(ShutdownPhase::WaitingForCurrent) => Mode::Draining,
            _ if !status.desired_stopped => Mode::Running,
            _ => Mode::Stopping,
        };
        Ok(())
    }
}

impl RuntimeRegistration {
    pub fn admission(&self) -> &Arc<RuntimeAdmission> {
        &self.admission
    }
}

fn merge_work(state: &State, observed: Vec<RuntimeWork>) -> Vec<RuntimeWork> {
    let mut work = state.work.clone();
    // Execution owners supply the current physical/control capability for their
    // already-counted admission. Pending preparations remain visible too.
    for item in observed {
        work.insert(item.id.clone(), item);
    }
    work.into_values().collect()
}

pub fn current_scope() -> Option<WorkPermit> {
    SCOPE.try_with(Clone::clone).ok()
}

pub async fn scope<T>(permit: Option<WorkPermit>, future: impl Future<Output = T>) -> T {
    match permit {
        Some(permit) => SCOPE.scope(permit, future).await,
        None => future.await,
    }
}

pub fn current_runtime() -> Result<Option<Arc<RuntimeAdmission>>> {
    RuntimeAdmission::for_root(
        &crate::storage::jcode_dir().context("Runtime state namespace is unavailable")?,
    )
}

pub fn input_allowed() -> bool {
    current_runtime().is_ok_and(|runtime| runtime.is_none_or(|runtime| runtime.accepts_input()))
}

#[cfg(test)]
mod tests;
