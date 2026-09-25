//! Runtime-owned execution inspection and control. This is not an execution
//! engine: original supervisors, native workers and retained receipts stay owners.
use super::*;
use crate::runtime_lifecycle::RuntimeStopOwner;
use crate::workspace::runtime::{RuntimeWork, RuntimeWorkKind};
use std::collections::BTreeMap;
use std::time::Duration;

pub struct OwnedExecutions {
    store: ExecutionStore,
    runtime: Arc<runtime::RuntimeHandle>,
    namespace: String,
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;

impl OwnedExecutions {
    pub fn is_background(&self, id: &str) -> Result<bool> {
        Ok(self.owned(id)?.0.background)
    }
    pub async fn bind(root: &std::path::Path, lifecycle: &RuntimeStopOwner) -> Result<Self> {
        let root = root.to_path_buf();
        let store = tokio::task::spawn_blocking(move || ExecutionStore::open(&root)).await??;
        let runtime = runtime::ensure_running(&store).await?;
        lifecycle.bind_execution_owner(&store, &runtime.endpoint.id)?;
        Ok(Self {
            store,
            runtime,
            namespace: lifecycle.namespace().to_owned(),
        })
    }

    fn origin(&self, record: &RunRecord) -> Result<Option<(String, bool)>> {
        let transfer = self.store.command_ownership(&record.id)?;
        let (origin, native) = match transfer {
            Some(transfer) => (transfer.parent, true),
            None => (record.owner.clone(), false),
        };
        if self.store.runtime_namespace(&origin)?.as_deref() != Some(&self.namespace) {
            return Ok(None);
        }
        if origin != self.runtime.endpoint.id {
            let endpoint = self
                .store
                .runtime_endpoint(&origin)?
                .context("Previous execution owner metadata is unavailable")?;
            ensure!(
                !endpoint.has_live_lease()?,
                "Previous runtime owner {origin} is still live; its work was not acquired"
            );
        }
        Ok(Some((origin, native)))
    }

    fn owned(&self, id: &str) -> Result<(RunRecord, RuntimeWork)> {
        let record = self
            .store
            .inspect(id)?
            .context("Runtime execution is unavailable")?;
        let (origin, native) = self
            .origin(&record)?
            .context("Execution is not owned by this runtime namespace")?;
        let view = RuntimeWork {
            id: id.into(),
            owner: origin,
            session: Some(record.session_id.clone()),
            kind: RuntimeWorkKind::Execution,
            supported_survivor: native,
        };
        Ok((record, view))
    }

    pub async fn inventory(&self) -> Result<Vec<RuntimeWork>> {
        let store = self.store.clone();
        let records = tokio::task::spawn_blocking(move || store.unresolved_runs()).await??;
        let mut work = BTreeMap::new();
        for record in records {
            if let Some((origin, native)) = self.origin(&record)? {
                work.insert(
                    record.id.clone(),
                    RuntimeWork {
                        id: record.id,
                        owner: origin,
                        session: Some(record.session_id),
                        kind: RuntimeWorkKind::Execution,
                        supported_survivor: native,
                    },
                );
            }
        }
        // Setup is live before its durable row/handoff exists. Keep it visible,
        // including its capability, rather than cancelling a pending native
        // command because the worker has not yet claimed it.
        let live = LIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|run| {
                run.store.root() == self.store.root()
                    && run.runtime.endpoint.id == self.runtime.endpoint.id
            })
            .cloned()
            .collect::<Vec<_>>();
        for run in live {
            if run.result.borrow().is_some() {
                continue;
            }
            let id = run.invocation.id();
            if self.store.inspect(&id)?.is_none() || run.owns_execution.load(Ordering::SeqCst) {
                let entry = work.entry(id.clone()).or_insert_with(|| RuntimeWork {
                    id,
                    owner: run.runtime.endpoint.id.clone(),
                    session: Some(run.invocation.session_id.clone()),
                    kind: RuntimeWorkKind::Execution,
                    supported_survivor: false,
                });
                entry.supported_survivor |= run.native_command;
            }
        }
        Ok(work.into_values().collect())
    }

    /// Promotion is a verified handoff, not cancellation. The original caller
    /// receives the same durable acceptance while its worker retains output and
    /// its original cwd/root lease. Failure never substitutes Stop or a retry
    /// of the command. The caller journals partial successful handoffs.
    pub async fn preserve(&self, id: &str, timeout: Duration) -> Result<Option<RuntimeWork>> {
        tokio::time::timeout(timeout, self.preserve_inner(id))
            .await
            .context(
                "Native preservation is still pending; no destructive fallback was performed",
            )?
    }

    async fn preserve_inner(&self, id: &str) -> Result<Option<RuntimeWork>> {
        loop {
            let live = LIVE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&(self.store.root().to_path_buf(), id.into()))
                .cloned();
            let Some(record) = self.store.inspect(id)? else {
                ensure!(
                    live.as_ref()
                        .is_some_and(|run| run.runtime.endpoint.id == self.runtime.endpoint.id
                            && run.native_command),
                    "No owned native preparation exists"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let (record, mut view) = self.owned(&record.id)?;
            if record.state.terminal() {
                return Ok(None);
            }
            ensure!(
                record.stop_cause.is_none(),
                "Native command is already stopping; preservation was not promised"
            );
            let transfer = self.store.command_ownership(id)?;
            if transfer.is_none() {
                ensure!(
                    live.as_ref().is_some_and(
                        |run| run.native_command && run.owns_execution.load(Ordering::SeqCst)
                    ),
                    "This execution cannot survive runtime shutdown"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            let transfer = transfer.expect("checked transfer");
            let Some(worker) = transfer.worker.filter(|_| transfer.process.is_some()) else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            ensure!(
                worker == record.owner,
                "Native capture ownership changed unexpectedly"
            );
            let endpoint = self
                .store
                .runtime_endpoint(&worker)?
                .context("Native worker control is unavailable")?;
            let reply = match control_transport::exchange(
                &endpoint,
                id,
                ControlOperation::Background,
            )
            .await
            {
                Ok(reply) => reply,
                Err(error) => {
                    if self
                        .store
                        .inspect(id)?
                        .is_some_and(|record| record.state.terminal())
                    {
                        return Ok(None);
                    }
                    return Err(error);
                }
            };
            ensure!(
                !matches!(
                    reply,
                    ControlReply::Unavailable { .. } | ControlReply::OwnerChanged
                ),
                "Native worker did not acknowledge preservation: {reply:?}"
            );
            let current = self
                .store
                .inspect(id)?
                .context("Preserved execution disappeared")?;
            if current.state.terminal() {
                return Ok(None);
            }
            ensure!(
                current.owner == worker && current.background && current.stop_cause.is_none(),
                "Native preservation did not commit at the expected owner"
            );
            ensure!(
                endpoint.has_live_lease()?,
                "Native worker ownership ended before handoff proof"
            );
            if let Some(run) = live {
                ensure!(
                    run.native_command && run.runtime.endpoint.id == transfer.parent,
                    "Native proxy and transfer ownership disagree"
                );
                observe_native_background(&self.store, id);
                run.release_native.store(true, Ordering::SeqCst);
                let mut result = run.result.clone();
                loop {
                    if let Some(completion) = result.borrow_and_update().clone() {
                        ensure!(
                            !completion.failed,
                            "Native proxy could not publish its handoff: {}",
                            completion.output.output
                        );
                        break;
                    }
                    result
                        .changed()
                        .await
                        .context("Native proxy ended without a handoff receipt")?;
                }
            }
            let current = self
                .store
                .inspect(id)?
                .context("Native handoff lost its retained record")?;
            if current.state.terminal() {
                return Ok(None);
            }
            ensure!(
                current.background
                    && current.owner == worker
                    && self.store.acceptance_result(id)?.is_some(),
                "Native handoff lacks durable acceptance"
            );
            view.supported_survivor = true;
            return Ok(Some(view));
        }
    }

    /// Wait for actual terminal publication. Stop acknowledgement and timeout
    /// do not mean quiescence. Force targets only proven owned native groups.
    pub async fn stop(&self, id: &str, force: bool, timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, async {
            let (record, _) = self.owned(id)?;
            if record.state.terminal() { return self.require_terminal(id); }
            if record.tool == "workspace_clone" {
                let request = record.message_id.parse()?;
                let workspace = crate::workspace::WorkspaceService::new(&crate::storage::durable_state_dir());
                workspace.request_clone_cancel(request)?;
                if force {
                    ensure!(record.owner == self.runtime.endpoint.id, "A prior unavailable clone owner cannot be force-controlled by a new runtime");
                    self.store.force_native_processes(id, &record.owner, &InterruptSignal::new()).await?;
                }
                loop {
                    if self.store.inspect(id)?.is_some_and(|record| record.state.terminal()) { break; }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            } else {
                let action = if force { ControlOperation::ForceStop } else { ControlOperation::Stop { cause: StopCause::RuntimeShutdown } };
                let reply = runtime::control_in_store(&self.store, id, action).await?;
                ensure!(!matches!(reply, ControlReply::Unavailable { .. } | ControlReply::OwnerChanged), "Owned execution stop was not accepted: {reply:?}");
                let reply = runtime::control_in_store(&self.store, id, ControlOperation::Wait).await?;
                ensure!(matches!(reply, ControlReply::Snapshot { ref record } if record.state.terminal()), "Execution has not published a terminal receipt");
            }
            self.require_terminal(id)
        }).await.context("Runtime shutdown is blocked waiting for actual execution quiescence")?
    }

    fn require_terminal(&self, id: &str) -> Result<()> {
        let (record, _) = self.owned(id)?;
        ensure!(
            record.state.terminal(),
            "Owned execution remains nonterminal"
        );
        // The execution owner guarantees sealing before terminal publication.
        // If there is a capture, require its actual complete readable receipt.
        if record.output_path.is_some() {
            self.store
                .result(&record, NonZeroUsize::new(1).expect("nonzero"))?;
        }
        Ok(())
    }
}
