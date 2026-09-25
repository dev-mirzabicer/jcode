//! Reviewed runtime intent over the existing primary, tool and native owners.
//! Client connections neither drive this operation nor determine its lifetime.
use crate::background::BackgroundTaskManager;
use crate::execution::shutdown::OwnedExecutions;
use crate::primary::PrimaryHost;
use crate::runtime_lifecycle::{
    RuntimeStopOwner, RuntimeStopStore,
    admission::{RuntimeAdmission, RuntimeRegistration},
};
use crate::workspace::{OperationId, runtime::*};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, watch};

pub struct RuntimeLifecycle {
    owner: RuntimeStopOwner,
    registration: RuntimeRegistration,
    executions: OwnedExecutions,
    primaries: Arc<PrimaryHost>,
    background: BackgroundTaskManager,
    mutation: Mutex<()>,
    wake: Arc<Notify>,
    stopped: watch::Sender<Option<RuntimeExit>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeExit {
    pub operation: OperationId,
    pub forced: bool,
}

struct WakeMutation(Option<Arc<Notify>>);
impl Drop for WakeMutation {
    fn drop(&mut self) {
        if let Some(wake) = &self.0 {
            wake.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;

impl RuntimeLifecycle {
    pub async fn new(
        root: &Path,
        socket: &Path,
        primaries: Arc<PrimaryHost>,
        background: BackgroundTaskManager,
    ) -> Result<Arc<Self>> {
        let store = RuntimeStopStore::new(&crate::storage::durable_state_dir(), socket)?;
        let owner = store.claim()?;
        let registration = RuntimeAdmission::register_namespace(
            root,
            owner.identity(),
            Some(owner.namespace().into()),
        )?;
        let executions = OwnedExecutions::bind(root, &owner).await?;
        primaries.bind_runtime_admission(registration.admission().clone())?;
        let lifecycle = Arc::new(Self {
            owner,
            registration,
            executions,
            primaries,
            background,
            mutation: Mutex::new(()),
            wake: Arc::new(Notify::new()),
            stopped: watch::channel(None).0,
        });
        let weak = Arc::downgrade(&lifecycle);
        let wake = lifecycle.wake.clone();
        tokio::spawn(async move {
            loop {
                wake.notified().await;
                let Some(lifecycle) = weak.upgrade() else {
                    return;
                };
                if let Err(error) = lifecycle.drive().await {
                    // Storage failure is not successful Stop. The existing
                    // durable intent stays fenced and inspection remains live.
                    crate::logging::error(&format!("Runtime shutdown driver: {error:#}"));
                }
                if lifecycle.stopped.borrow().is_some() {
                    return;
                }
            }
        });
        Ok(lifecycle)
    }

    pub fn stopped(&self) -> watch::Receiver<Option<RuntimeExit>> {
        self.stopped.subscribe()
    }

    async fn work(&self) -> Result<Vec<RuntimeWork>> {
        let mut work = self
            .registration
            .admission()
            .work()?
            .into_iter()
            .map(|work| (work.id.clone(), work))
            .collect::<BTreeMap<_, _>>();
        for item in self.executions.inventory().await?.into_iter().chain(
            self.background
                .runtime_owned_work(self.owner.identity())
                .await,
        ) {
            work.insert(item.id.clone(), item);
        }
        Ok(work.into_values().collect())
    }

    pub async fn request(&self, request: RuntimeRequest) -> Result<RuntimeResponse> {
        let kick = matches!(
            &request,
            RuntimeRequest::Begin { .. }
                | RuntimeRequest::CancelWait { .. }
                | RuntimeRequest::Retry { .. }
                | RuntimeRequest::Force { .. }
        );
        // An error may follow durable intent publication. A cancelled or lost
        // reply cannot strand that accepted operation without its driver.
        let _wake = WakeMutation(kick.then(|| self.wake.clone()));
        let _mutation = self.mutation.lock().await;
        let response = match request {
            RuntimeRequest::Status {} => {
                let mut status = self.owner.status()?;
                status.reload_in_progress = self.registration.admission().is_reloading();
                status.work = self.work().await?;
                RuntimeResponse::Status(status)
            }
            RuntimeRequest::Inspect { operation } => {
                RuntimeResponse::Operation(self.owner.inspect(operation)?)
            }
            RuntimeRequest::Review { options } => RuntimeResponse::Review(
                self.registration
                    .admission()
                    .review(&self.owner, options, self.work().await?)?,
            ),
            RuntimeRequest::ReviewChange {
                operation,
                expected_revision,
                options,
            } => RuntimeResponse::Review(self.registration.admission().review_change(
                &self.owner,
                operation,
                expected_revision,
                options,
                self.work().await?,
            )?),
            RuntimeRequest::Begin { request, review } => {
                RuntimeResponse::Operation(self.registration.admission().begin(
                    &self.owner,
                    request,
                    review,
                    self.work().await?,
                )?)
            }
            RuntimeRequest::CancelWait {
                operation,
                expected_revision,
            } => {
                let operation = self.registration.admission().cancel_wait(
                    &self.owner,
                    operation,
                    expected_revision,
                )?;
                self.primaries.resume_deferred_inputs().await?;
                RuntimeResponse::Operation(operation)
            }
            RuntimeRequest::Retry {
                operation,
                expected_revision,
            } => {
                RuntimeResponse::Operation(self.owner.retry(operation, expected_revision, false)?)
            }
            RuntimeRequest::Force {
                operation,
                expected_revision,
            } => {
                RuntimeResponse::Operation(self.owner.retry(operation, expected_revision, true)?)
            }
        };
        // Notify retains a permit if this races the driver retiring an attempt.
        // Repeated requests never create another shutdown or provider turn.
        Ok(response)
    }

    async fn observe(
        &self,
        id: OperationId,
        mut preserved: Vec<RuntimeWork>,
        issues: Vec<String>,
    ) -> Result<ShutdownOperation> {
        let _mutation = self.mutation.lock().await;
        let current = self.owner.inspect(id)?;
        if current.phase.terminal() {
            return Ok(current);
        }
        for prior in &current.preserved {
            if !preserved.iter().any(|item| item.id == prior.id) {
                preserved.push(prior.clone());
            }
        }
        preserved.sort_by(|a, b| a.id.cmp(&b.id));
        let retained = preserved
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>();
        let remaining = self
            .work()
            .await?
            .into_iter()
            .filter(|item| !retained.contains(item.id.as_str()))
            .collect::<Vec<_>>();
        if current.remaining == remaining
            && current.preserved == preserved
            && current.issues == issues
        {
            return Ok(current);
        }
        self.owner
            .observe(id, current.revision, remaining, preserved, issues)
    }

    async fn drive(&self) -> Result<()> {
        loop {
            let Some(operation) = self.owner.status()?.operation else {
                return Ok(());
            };
            match operation.phase {
                ShutdownPhase::Forced
                    if operation.review.runtime == self.owner.identity()
                        && self.owner.status()?.desired_stopped =>
                {
                    self.registration
                        .admission()
                        .confirm_forced(&self.owner, operation.id)?;
                    self.stopped.send_replace(Some(RuntimeExit {
                        operation: operation.id,
                        forced: true,
                    }));
                    return Ok(());
                }
                ShutdownPhase::Stopped
                    if operation.review.runtime == self.owner.identity()
                        && self.owner.status()?.desired_stopped =>
                {
                    // A prior complete() can report a post-rename fsync error.
                    // Retry confirms the original receipt, not the work effects.
                    let kept = operation
                        .preserved
                        .iter()
                        .map(|work| &work.id)
                        .collect::<BTreeSet<_>>();
                    ensure!(
                        self.work()
                            .await?
                            .iter()
                            .all(|work| kept.contains(&work.id))
                            && self.registration.admission().work()?.is_empty(),
                        "Stopped receipt has outstanding runtime work"
                    );
                    self.registration
                        .admission()
                        .confirm_stopped(&self.owner, operation.id)?;
                    self.stopped.send_replace(Some(RuntimeExit {
                        operation: operation.id,
                        forced: false,
                    }));
                    return Ok(());
                }
                ShutdownPhase::WaitingForCurrent => {
                    let mut preserved = operation.preserved.clone();
                    let mut issues = Vec::new();
                    if operation.review.options.independent == IndependentTasks::KeepSupported {
                        for work in self.executions.inventory().await? {
                            if work.supported_survivor
                                && self.executions.is_background(&work.id)?
                                && !preserved.iter().any(|item| item.id == work.id)
                            {
                                match self
                                    .executions
                                    .preserve(&work.id, Duration::from_secs(5))
                                    .await
                                {
                                    Ok(Some(work)) => preserved.push(work),
                                    Ok(None) => {}
                                    Err(error) => issues.push(format!("{}: {error:#}", work.id)),
                                }
                            }
                        }
                    }
                    let observed = self.observe(operation.id, preserved, issues).await?;
                    if observed.phase != ShutdownPhase::WaitingForCurrent {
                        continue;
                    }
                    if observed.remaining.is_empty()
                        && observed.issues.is_empty()
                        && self.registration.admission().work()?.is_empty()
                    {
                        let _mutation = self.mutation.lock().await;
                        let current = self.owner.inspect(operation.id)?;
                        if current.phase == ShutdownPhase::WaitingForCurrent {
                            self.registration.admission().enter_stopping(
                                &self.owner,
                                current.id,
                                current.revision,
                            )?;
                        }
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                ShutdownPhase::Stopping => {
                    let timeout = Duration::from_secs(
                        operation.review.options.quiescence_timeout_seconds.into(),
                    );
                    let preservation_complete = std::sync::atomic::AtomicBool::new(false);
                    let result = tokio::time::timeout(
                        timeout,
                        self.quiesce(&operation, &preservation_complete),
                    )
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            let observed =
                                match self.observe(operation.id, Vec::new(), Vec::new()).await {
                                    Ok(observed) => observed,
                                    Err(error) => {
                                        self.block(
                                            operation.id,
                                            format!("Final work observation failed: {error:#}"),
                                        )
                                        .await?;
                                        return Ok(());
                                    }
                                };
                            let _mutation = self.mutation.lock().await;
                            ensure!(
                                self.registration.admission().work()?.is_empty(),
                                "Admitted work remains after quiescence"
                            );
                            let completed = self.registration.admission().complete(
                                &self.owner,
                                operation.id,
                                observed.revision,
                            )?;
                            self.stopped.send_replace(Some(RuntimeExit {
                                operation: completed.id,
                                forced: false,
                            }));
                            return Ok(());
                        }
                        failure => {
                            let issue = match failure { Ok(Err(error)) => format!("{error:#}"), Err(_) => "Shutdown deadline elapsed before owned work and checkpoints completed".into(), Ok(Ok(())) => unreachable!() };
                            self.block(operation.id, issue).await?;
                            let current = self.owner.inspect(operation.id)?;
                            if current.phase == ShutdownPhase::Blocked
                                && current.force_requested
                                && !operation.force_requested
                            {
                                let _mutation = self.mutation.lock().await;
                                self.owner.retry(current.id, current.revision, true)?;
                                continue;
                            }
                            if operation.force_requested
                                && preservation_complete.load(std::sync::atomic::Ordering::SeqCst)
                            {
                                let _mutation = self.mutation.lock().await;
                                let current = self.owner.inspect(operation.id)?;
                                let forced = self.registration.admission().force_exit(
                                    &self.owner,
                                    current.id,
                                    current.revision,
                                )?;
                                self.stopped.send_replace(Some(RuntimeExit {
                                    operation: forced.id,
                                    forced: true,
                                }));
                            }
                            return Ok(());
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    async fn block(&self, id: OperationId, mut issue: String) -> Result<()> {
        if let Err(error) = self.observe(id, Vec::new(), Vec::new()).await {
            issue.push_str(&format!("; latest work observation unavailable: {error:#}"));
        }
        let _mutation = self.mutation.lock().await;
        let current = self.owner.inspect(id)?;
        self.owner.block(id, current.revision, vec![issue])?;
        Ok(())
    }

    async fn quiesce(
        &self,
        operation: &ShutdownOperation,
        preservation_complete: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        let timeout =
            Duration::from_secs(operation.review.options.quiescence_timeout_seconds.into());
        let mut preserved = operation.preserved.clone();
        // Preservation must succeed for every eligible owner before parents are
        // interrupted. Earlier successful handoffs remain durable on failure.
        if operation.review.options.independent == IndependentTasks::KeepSupported {
            let inventory = loop {
                let inventory = self.executions.inventory().await?;
                if self
                    .registration
                    .admission()
                    .work()?
                    .iter()
                    .filter(|work| work.kind == RuntimeWorkKind::Execution)
                    .all(|work| inventory.iter().any(|known| known.id == work.id))
                {
                    break inventory;
                }
                // An admitted producer may not have published its native
                // capability yet. Never classify it as disposable by absence.
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            for work in inventory {
                if work.supported_survivor && !preserved.iter().any(|item| item.id == work.id) {
                    if let Some(work) = self.executions.preserve(&work.id, timeout).await? {
                        preserved.push(work);
                    }
                    self.observe(operation.id, preserved.clone(), Vec::new())
                        .await?;
                }
            }
        }
        preservation_complete.store(true, std::sync::atomic::Ordering::SeqCst);
        let kept = preserved
            .iter()
            .map(|work| work.id.clone())
            .collect::<BTreeSet<_>>();
        self.registration.admission().interrupt_preparations()?;
        let execution_work = self.executions.inventory().await?;
        let legacy_work = self
            .background
            .runtime_owned_work(self.owner.identity())
            .await;
        let (primary, execution, legacy) = tokio::join!(
            self.primaries.interrupt_runtime(),
            async {
                futures::future::join_all(
                    execution_work
                        .iter()
                        .filter(|work| !kept.contains(&work.id))
                        .map(|work| {
                            self.executions
                                .stop(&work.id, operation.force_requested, timeout)
                        }),
                )
                .await
            },
            async {
                futures::future::join_all(legacy_work.iter().map(|work| {
                    self.background.stop_runtime_owned(
                        work.id
                            .strip_prefix("background:")
                            .expect("legacy owner prefix"),
                    )
                }))
                .await
            },
        );
        let issues = std::iter::once(primary)
            .chain(execution)
            .chain(legacy)
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        ensure!(
            issues.is_empty(),
            "Owned shutdown remains incomplete: {}",
            issues.join("; ")
        );
        loop {
            let remaining = self
                .work()
                .await?
                .into_iter()
                .filter(|work| !kept.contains(&work.id))
                .collect::<Vec<_>>();
            if remaining.is_empty() && self.registration.admission().work()?.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.primaries
            .checkpoint_runtime()
            .await
            .context("Runtime primary checkpoint")
    }
}

impl Drop for RuntimeLifecycle {
    fn drop(&mut self) {
        self.wake.notify_one();
    }
}
