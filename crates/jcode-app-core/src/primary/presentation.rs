//! Disposable ordered presentation state, derived from canonical Session checkpoints.
use crate::protocol::{PrimaryStreamCursor, ServerEvent};
use crate::session::Session;
use anyhow::{Result, ensure};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};

pub(crate) struct Presentation {
    session: String,
    stream: String,
    state: Mutex<State>,
    changed: Notify,
}

#[derive(Default)]
struct State {
    sequence: u64,
    request_id: u64,
    origin: Option<String>,
    checkpoint: Option<Arc<Session>>,
    pending: HashMap<String, Arc<Session>>,
    events: Vec<(u64, ServerEvent)>,
    ready: bool,
    closed: bool,
    failure: Option<String>,
}

pub(crate) struct Snapshot {
    pub session: Arc<Session>,
    pub events: Vec<ServerEvent>,
    pub cursor: PrimaryStreamCursor,
    pub processing: bool,
}

impl Presentation {
    pub fn new(session: &str) -> Self {
        Self {
            session: session.into(),
            stream: crate::id::new_id("stream"),
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }
    }
    pub fn begin(&self, request_id: u64) {
        let mut state = self.state.lock().expect("primary presentation");
        state.request_id = request_id;
        state.origin = None;
        state.ready = false;
        state.closed = false;
        state.failure = None;
    }
    /// The marker travels in the same FIFO as its preceding and following
    /// deltas. Merely loading the latest Session beside an event cursor is racy.
    pub fn checkpoint(
        &self,
        session: &Session,
        tx: &mpsc::UnboundedSender<ServerEvent>,
        terminal: bool,
    ) -> Result<()> {
        ensure!(
            session.id == self.session,
            "Presentation checkpoint targets another primary"
        );
        let token = crate::id::new_id("checkpoint");
        let mut snapshot = session.clone();
        snapshot.release_provider_messages_cache();
        self.state
            .lock()
            .expect("primary presentation")
            .pending
            .insert(token.clone(), Arc::new(snapshot));
        if tx
            .send(ServerEvent::PrimaryCheckpoint {
                token: token.clone(),
                terminal,
            })
            .is_err()
        {
            self.state
                .lock()
                .expect("primary presentation")
                .pending
                .remove(&token);
            anyhow::bail!("Primary presentation stream is closed");
        }
        Ok(())
    }
    pub fn record(&self, event: &ServerEvent) -> Result<Option<PrimaryStreamCursor>> {
        let mut state = self.state.lock().expect("primary presentation");
        if let ServerEvent::PrimaryCheckpoint { token, terminal } = event {
            let checkpoint = state
                .pending
                .remove(token)
                .ok_or_else(|| anyhow::anyhow!("Unknown primary presentation checkpoint"))?;
            let unchanged = state
                .checkpoint
                .as_ref()
                .is_some_and(|previous| same_history(previous, &checkpoint));
            state.checkpoint = Some(checkpoint);
            if *terminal {
                if !unchanged {
                    state.events.retain(|(_, event)| recovery_event(event));
                }
            } else {
                state.events.clear();
            }
            state.ready = true;
            state.closed = *terminal;
            drop(state);
            self.changed.notify_waiters();
            return Ok(None);
        }
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Primary presentation sequence exhausted"))?;
        let sequence = state.sequence;
        match (state.events.last_mut(), event) {
            (
                Some((last, ServerEvent::TextDelta { text: previous })),
                ServerEvent::TextDelta { text },
            ) => {
                previous.push_str(text);
                *last = sequence;
            }
            _ => state.events.push((sequence, event.clone())),
        }
        Ok(Some(PrimaryStreamCursor {
            session_id: self.session.clone(),
            stream_id: self.stream.clone(),
            sequence: state.sequence,
            request_id: state.request_id,
            origin: state.origin.clone(),
        }))
    }
    pub fn fail(&self, error: String) {
        self.state.lock().expect("primary presentation").failure = Some(error);
        self.changed.notify_waiters();
    }
    pub fn set_origin(&self, origin: Option<String>) {
        self.state.lock().expect("primary presentation").origin = origin;
    }
    pub fn cursor(&self) -> PrimaryStreamCursor {
        let state = self.state.lock().expect("primary presentation");
        PrimaryStreamCursor {
            session_id: self.session.clone(),
            stream_id: self.stream.clone(),
            sequence: state.sequence,
            request_id: state.request_id,
            origin: state.origin.clone(),
        }
    }
    pub fn recovery_events(
        &self,
        current: &Session,
        seen: Option<&PrimaryStreamCursor>,
    ) -> Vec<ServerEvent> {
        let state = self.state.lock().expect("primary presentation");
        if !state.closed
            || !state
                .checkpoint
                .as_ref()
                .is_some_and(|checkpoint| same_history(checkpoint, current))
        {
            return Vec::new();
        }
        state
            .events
            .iter()
            .filter(|(sequence, event)| {
                if !recovery_event(event) {
                    return true;
                }
                !seen
                    .is_some_and(|seen| seen.stream_id == self.stream && *sequence <= seen.sequence)
            })
            .map(|(_, event)| event.clone())
            .collect()
    }
    #[cfg(test)]
    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.snapshot_since(None).await
    }
    pub async fn snapshot_since(&self, seen: Option<&PrimaryStreamCursor>) -> Result<Snapshot> {
        loop {
            let changed = self.changed.notified();
            {
                let state = self.state.lock().expect("primary presentation");
                if let Some(error) = &state.failure {
                    anyhow::bail!("Primary presentation needs resynchronization: {error}");
                }
                if state.ready {
                    return Ok(Snapshot {
                        session: state
                            .checkpoint
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("Primary checkpoint missing"))?,
                        events: state
                            .events
                            .iter()
                            .filter(|(sequence, event)| {
                                !recovery_event(event)
                                    || !seen.is_some_and(|seen| {
                                        seen.stream_id == self.stream && *sequence <= seen.sequence
                                    })
                            })
                            .map(|(_, event)| event.clone())
                            .collect(),
                        cursor: PrimaryStreamCursor {
                            session_id: self.session.clone(),
                            stream_id: self.stream.clone(),
                            sequence: state.sequence,
                            request_id: state.request_id,
                            origin: state.origin.clone(),
                        },
                        processing: !state.closed,
                    });
                }
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), changed)
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Primary is preparing its first checkpoint; retry inspection")
                })?;
        }
    }
}

fn recovery_event(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::Done { .. }
            | ServerEvent::Error { .. }
            | ServerEvent::Interrupted
            | ServerEvent::ContextActionRequired { .. }
            | ServerEvent::StartupContextFailed { .. }
            | ServerEvent::StartupContextStatus {
                action_required: Some(_),
                ..
            }
    )
}

fn same_history(a: &Session, b: &Session) -> bool {
    a.id == b.id
        && a.messages
            .iter()
            .map(|message| &message.id)
            .eq(b.messages.iter().map(|message| &message.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn checkpoint_is_published_only_in_its_event_order() {
        let mut session = Session::create_with_id("ordered-snapshot".into(), None, None);
        session.title = Some("first fixture".into());
        let original = serde_json::to_vec(&session.messages).unwrap();
        let presentation = Presentation::new(&session.id);
        let (tx, mut rx) = mpsc::unbounded_channel();
        presentation.begin(1);
        presentation.checkpoint(&session, &tx, false).unwrap();
        assert!(
            presentation
                .record(&rx.recv().await.unwrap())
                .unwrap()
                .is_none()
        );
        presentation
            .record(&ServerEvent::TextDelta {
                text: "first ".into(),
            })
            .unwrap();
        presentation
            .record(&ServerEvent::TextDelta {
                text: "second".into(),
            })
            .unwrap();
        session.title = Some("newer fixture".into());
        presentation.checkpoint(&session, &tx, false).unwrap();
        let before = presentation.snapshot().await.unwrap();
        assert_eq!(before.session.title.as_deref(), Some("first fixture"));
        assert!(
            matches!(&before.events[..],[ServerEvent::TextDelta{text}] if text=="first second")
        );
        presentation
            .record(&ServerEvent::TextDelta {
                text: " later".into(),
            })
            .unwrap();
        presentation.record(&rx.recv().await.unwrap()).unwrap();
        let after = presentation.snapshot().await.unwrap();
        assert_eq!(after.session.title.as_deref(), Some("newer fixture"));
        assert!(after.events.is_empty());
        assert!(after.cursor.sequence > before.cursor.sequence);
        assert_eq!(serde_json::to_vec(&session.messages).unwrap(), original);
        assert_eq!(before.session.title.as_deref(), Some("first fixture"));
    }
}
