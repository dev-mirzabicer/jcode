//! Runtime shutdown reuses primary turn, Session and input-drain ownership.
use super::*;
use jcode_tool_types::StopCause;

pub(super) type Checkpoint = watch::Receiver<Option<std::result::Result<(), String>>>;

impl PrimaryHost {
    pub(crate) async fn interrupt_runtime(&self) -> Result<()> {
        let sessions = self
            .turns
            .lock()
            .expect("primary turns")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let results = futures::future::join_all(sessions.iter().map(|session| async move {
            self.stop_with_cause(session, StopCause::RuntimeShutdown)
                .await
                .map_err(|error| format!("{session}: {error:#}"))
        }))
        .await;
        let errors = results
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        ensure!(
            errors.is_empty(),
            "Primary shutdown remains incomplete: {}",
            errors.join("; ")
        );
        Ok(())
    }

    pub(crate) async fn resume_deferred_inputs(self: &Arc<Self>) -> Result<()> {
        let sessions = self.read().await.keys().cloned().collect::<Vec<_>>();
        if sessions.is_empty() {
            return Ok(());
        }
        let context = self.input_delivery_context()?;
        for session in sessions {
            crate::server::ensure_primary_input_delivery(self, &session, context.clone());
        }
        Ok(())
    }

    /// A timed-out waiter does not discard an in-flight checkpoint or start a
    /// second writer. The retained attempt finishes through the original owners.
    pub(crate) async fn checkpoint_runtime(&self) -> Result<()> {
        ensure!(
            !self.accepts_input() && self.turns.lock().expect("primary turns").is_empty(),
            "Primary work has not quiesced"
        );
        let agents = self.read().await.values().cloned().collect::<Vec<_>>();
        let mut result = {
            let mut checkpoint = self.checkpoint.lock().expect("runtime checkpoint");
            let retry = checkpoint.as_ref().is_none_or(|result| {
                matches!(result.borrow().as_ref(), Some(Err(_)))
                    || (result.borrow().is_none() && result.has_changed().is_err())
            });
            if retry {
                let (sender, receiver) = watch::channel(None);
                let mut tasks = std::mem::take(&mut *self.tasks.lock().expect("primary tasks"));
                let inputs = self
                    .stdin
                    .lock()
                    .expect("primary stdin owners")
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                tokio::spawn(async move {
                    let mut errors = Vec::new();
                    while let Some(result) = tasks.join_next().await {
                        if let Err(error) = result {
                            errors.push(format!("Primary supervisor: {error}"));
                        }
                    }
                    for agent in agents {
                        match tokio::task::spawn_blocking(move || -> Result<()> {
                            let mut agent = agent
                                .try_lock()
                                .context("Primary is still held by another control")?;
                            agent.startup_context_session_mut().save()?;
                            Ok(())
                        })
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => errors.push(format!("Primary checkpoint: {error:#}")),
                            Err(error) => errors.push(format!("Primary checkpoint owner: {error}")),
                        }
                    }
                    for input in inputs {
                        input.shutdown().await;
                    }
                    sender.send_replace(Some(if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("; "))
                    }));
                });
                *checkpoint = Some(receiver);
            }
            checkpoint.as_ref().expect("checkpoint installed").clone()
        };
        loop {
            if let Some(outcome) = result.borrow_and_update().clone() {
                return outcome.map_err(anyhow::Error::msg);
            }
            result
                .changed()
                .await
                .context("Primary checkpoint owner ended without a receipt")?;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::runtime_lifecycle::{RuntimeStopStore, admission::RuntimeAdmission};
    use crate::workspace::{RequestId, runtime::*};

    #[test]
    fn abandoned_checkpoint_attempt_can_retry_without_reusing_a_lost_waiter() -> Result<()> {
        let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let owner = RuntimeStopStore::new(
                &crate::storage::durable_state_dir(),
                &sandbox.root().join("checkpoint.sock"),
            )?
            .claim()?;
            let registration = RuntimeAdmission::register(sandbox.root(), owner.identity())?;
            let review = registration.admission().review(
                &owner,
                ShutdownOptions {
                    strategy: StopStrategy::Interrupt,
                    independent: IndependentTasks::Stop,
                    quiescence_timeout_seconds: 1,
                },
                Vec::new(),
            )?;
            registration
                .admission()
                .begin(&owner, RequestId::new(), review.id, Vec::new())?;
            let host = PrimaryHost::default();
            let (sender, receiver) = watch::channel(None);
            *host.checkpoint.lock().unwrap() = Some(receiver);
            drop(sender);
            host.checkpoint_runtime().await?;
            assert!(matches!(
                host.checkpoint
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .as_ref(),
                Some(Ok(()))
            ));
            Ok(())
        })
    }
}
