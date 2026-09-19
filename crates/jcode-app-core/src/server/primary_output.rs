//! Ordered primary output, independent of any connection task.
use super::{SwarmMember, state};
use crate::client_delivery::ClientEventSender;
use crate::primary::presentation::Presentation;
use crate::protocol::ServerEvent;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

pub(super) struct PrimaryOutput {
    pub tx: mpsc::UnboundedSender<ServerEvent>,
    worker: Option<tokio::task::JoinHandle<()>>,
    presentation: Arc<Presentation>,
}

impl PrimaryOutput {
    pub fn new(
        session: String,
        presentation: Arc<Presentation>,
        members: Arc<RwLock<HashMap<String, SwarmMember>>>,
        fallback: Option<ClientEventSender>,
    ) -> Self {
        presentation.set_origin(
            fallback
                .as_ref()
                .and_then(|sender| sender.identity().map(str::to_string)),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let view = presentation.clone();
        let worker = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let terminal =
                    matches!(event, ServerEvent::PrimaryCheckpoint { terminal: true, .. });
                match view.record(&event) {
                    Ok(Some(cursor)) => {
                        let targets = state::session_event_targets(&members, &session).await;
                        if targets.is_empty() && !members.read().await.contains_key(&session) {
                            if let Some(fallback) = &fallback {
                                let _ = fallback.send_sequenced(event, cursor);
                            }
                        } else {
                            for target in targets {
                                let _ = target.send_sequenced(event.clone(), cursor.clone());
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        view.fail(error.to_string());
                        break;
                    }
                }
                if terminal {
                    break;
                }
            }
        });
        Self {
            tx,
            worker: Some(worker),
            presentation,
        }
    }
    /// Called only after the body and terminal event have settled. Joining this
    /// FIFO prevents a previous turn's terminal events overtaking the next turn.
    pub async fn finish(mut self, agent: &Arc<tokio::sync::Mutex<crate::agent::Agent>>) {
        let mut agent = agent.lock().await;
        let checkpoint =
            self.presentation
                .checkpoint(agent.startup_context_session(), &self.tx, true);
        agent.primary_presentation = None;
        drop(agent);
        if let Err(error) = checkpoint {
            self.presentation.fail(error.to_string());
            if let Some(worker) = &self.worker {
                worker.abort();
            }
        }
        if let Some(worker) = self.worker.take()
            && let Err(error) = worker.await
        {
            self.presentation
                .fail(format!("Primary output worker ended: {error}"));
        }
    }
}

impl Drop for PrimaryOutput {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
