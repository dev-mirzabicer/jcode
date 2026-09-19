//! Client delivery is expendable. Session and execution storage retain the work.
use crate::protocol::{ServerEvent, encode_event};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_QUEUED_EVENTS: usize = 256;
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct DeliveryClosed;
impl std::fmt::Display for DeliveryClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client delivery closed; reconnect and inspect retained state")
    }
}
impl std::error::Error for DeliveryClosed {}

#[derive(Clone)]
pub struct ClientEventSender {
    route: Route,
    binding: Option<Binding>,
}

#[derive(Clone)]
enum Route {
    Local(mpsc::UnboundedSender<ServerEvent>),
    Client {
        sender: mpsc::Sender<ClientEvent>,
        state: Arc<State>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    session: String,
    generation: u64,
}

struct State {
    metadata: Mutex<Metadata>,
    disconnected: CancellationToken,
    byte_limit: usize,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct Metadata {
    target: Option<Binding>,
    bytes: usize,
    paused: bool,
}

pub(crate) struct ClientEvent {
    pub event: ServerEvent,
    pub json: String,
    binding: Option<Binding>,
    state: Arc<State>,
}

impl ClientEvent {
    pub async fn ready(&self) {
        loop {
            let changed = self.state.changed.notified();
            if !self.paused() || !self.is_current() {
                return;
            }
            changed.await;
        }
    }
    pub fn paused(&self) -> bool {
        self.state.metadata.lock().expect("client delivery").paused
    }
    /// Recheck while holding the socket writer so a navigation barrier cannot
    /// be overtaken by a previously dequeued event.
    pub fn is_current(&self) -> bool {
        self.binding.is_none()
            || self.binding == self.state.metadata.lock().expect("client delivery").target
    }
}

impl Drop for ClientEvent {
    fn drop(&mut self) {
        let mut metadata = self.state.metadata.lock().expect("client delivery");
        metadata.bytes = metadata.bytes.saturating_sub(self.json.len());
    }
}

impl From<mpsc::UnboundedSender<ServerEvent>> for ClientEventSender {
    /// Existing in-process consumers have no socket or disconnect policy.
    fn from(sender: mpsc::UnboundedSender<ServerEvent>) -> Self {
        Self {
            route: Route::Local(sender),
            binding: None,
        }
    }
}

/// In-process event observers. Network connections use `bounded_client`.
pub fn local_event_channel() -> (ClientEventSender, mpsc::UnboundedReceiver<ServerEvent>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (sender.into(), receiver)
}

impl ClientEventSender {
    pub(crate) async fn begin_snapshot(
        &self,
        session: &str,
        writer: &Arc<tokio::sync::Mutex<crate::transport::WriteHalf>>,
    ) -> Self {
        if matches!(self.route, Route::Client { .. }) {
            let _writer = writer.lock().await;
            self.retarget(session);
            self.pause_snapshot();
        }
        self.for_session(session)
    }
    pub(crate) fn bounded_client() -> (Self, mpsc::Receiver<ClientEvent>, CancellationToken) {
        Self::bounded(MAX_QUEUED_EVENTS, MAX_QUEUED_BYTES)
    }

    fn bounded(
        capacity: usize,
        byte_limit: usize,
    ) -> (Self, mpsc::Receiver<ClientEvent>, CancellationToken) {
        let (sender, receiver) = mpsc::channel(capacity);
        let disconnected = CancellationToken::new();
        let state = Arc::new(State {
            metadata: Mutex::new(Metadata::default()),
            disconnected: disconnected.clone(),
            byte_limit,
            changed: tokio::sync::Notify::new(),
        });
        // The budget belongs to the queue, not to each sender clone.
        let route = Route::Client { sender, state };
        let value = Self {
            route,
            binding: None,
        };
        (value, receiver, disconnected)
    }

    pub fn is_closed(&self) -> bool {
        match &self.route {
            Route::Local(sender) => sender.is_closed(),
            Route::Client { sender, state } => {
                sender.is_closed() || state.disconnected.is_cancelled()
            }
        }
    }

    pub(crate) fn retarget(&self, session: &str) {
        if let Route::Client { state, .. } = &self.route {
            let mut metadata = state.metadata.lock().expect("client delivery");
            let generation = metadata
                .target
                .as_ref()
                .map_or(1, |target| target.generation.wrapping_add(1));
            metadata.target = Some(Binding {
                session: session.into(),
                generation,
            });
            drop(metadata);
            state.changed.notify_waiters();
        }
    }

    pub(crate) fn pause_snapshot(&self) {
        if let Route::Client { state, .. } = &self.route {
            state.metadata.lock().expect("client delivery").paused = true;
        }
    }
    pub(crate) fn finish_snapshot(&self) {
        if let Route::Client { state, .. } = &self.route {
            state.metadata.lock().expect("client delivery").paused = false;
            state.changed.notify_waiters();
        }
    }
    pub(crate) fn for_session(&self, session: &str) -> Self {
        let binding = match &self.route {
            Route::Client { state, .. } => Some(Binding {
                session: session.into(),
                generation: state
                    .metadata
                    .lock()
                    .expect("client delivery")
                    .target
                    .as_ref()
                    .map_or(0, |target| target.generation),
            }),
            Route::Local(_) => None,
        };
        Self {
            route: self.route.clone(),
            binding,
        }
    }

    pub fn send(&self, event: ServerEvent) -> Result<(), DeliveryClosed> {
        let Route::Client { sender, state } = &self.route else {
            let Route::Local(sender) = &self.route else {
                unreachable!()
            };
            return sender.send(event).map_err(|_| DeliveryClosed);
        };
        if self.is_closed() {
            return Err(DeliveryClosed);
        }
        let json = encode_event(&event);
        let mut metadata = state.metadata.lock().expect("client delivery");
        if self.binding.is_some() && self.binding != metadata.target {
            return Ok(());
        }
        // One complete oversized event is allowed in an otherwise empty queue.
        // Never truncate a response to fit the delivery budget.
        if metadata.bytes != 0 && metadata.bytes.saturating_add(json.len()) > state.byte_limit {
            state.disconnected.cancel();
            return Err(DeliveryClosed);
        }
        metadata.bytes += json.len();
        let queued = ClientEvent {
            event,
            json,
            binding: self.binding.clone(),
            state: state.clone(),
        };
        let result = sender.try_send(queued);
        drop(metadata);
        if let Err(error) = result {
            state.disconnected.cancel();
            // The retained authoritative result is unaffected by disconnect.
            drop(error);
            return Err(DeliveryClosed);
        }
        Ok(())
    }
}

impl std::fmt::Debug for ClientEventSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientEventSender")
            .field("binding", &self.binding)
            .field("closed", &self.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slow_client_disconnects_at_the_queue_bound() {
        let (sender, mut receiver, disconnected) = ClientEventSender::bounded(2, 1024);
        sender.send(ServerEvent::Done { id: 1 }).unwrap();
        sender.send(ServerEvent::Done { id: 2 }).unwrap();
        assert!(sender.send(ServerEvent::Done { id: 3 }).is_err());
        assert!(disconnected.is_cancelled());
        assert!(sender.is_closed());
        assert_eq!(receiver.len(), 2);
        assert!(matches!(
            receiver.recv().await.unwrap().event,
            ServerEvent::Done { id: 1 }
        ));
    }

    #[tokio::test]
    async fn byte_budget_is_shared_and_complete_oversized_frames_are_not_truncated() {
        let (sender, mut receiver, disconnected) = ClientEventSender::bounded(10, 64);
        let text = "synthetic".repeat(1024);
        sender
            .send(ServerEvent::TextDelta { text: text.clone() })
            .unwrap();
        let frame = receiver.recv().await.unwrap();
        let decoded: ServerEvent = serde_json::from_str(&frame.json).unwrap();
        assert!(matches!(decoded, ServerEvent::TextDelta { text: actual } if actual == text));
        drop(frame);
        assert!(!disconnected.is_cancelled());
        sender.send(ServerEvent::Done { id: 1 }).unwrap();
        sender.clone().send(ServerEvent::Done { id: 2 }).unwrap();
        assert!(sender.send(ServerEvent::Done { id: 3 }).is_err());
        assert!(disconnected.is_cancelled());
    }

    #[tokio::test]
    async fn queued_and_late_old_target_events_do_not_follow_navigation() {
        let (sender, mut receiver, disconnected) = ClientEventSender::bounded(8, 1024);
        sender.retarget("first");
        let first = sender.for_session("first");
        first.send(ServerEvent::Done { id: 1 }).unwrap();
        let queued = receiver.recv().await.unwrap();
        assert!(queued.is_current());
        sender.retarget("second");
        assert!(!queued.is_current());
        first.send(ServerEvent::Done { id: 2 }).unwrap();
        assert!(receiver.try_recv().is_err());
        sender
            .for_session("first")
            .send(ServerEvent::Done { id: 3 })
            .unwrap();
        assert!(receiver.try_recv().is_err());
        sender
            .for_session("second")
            .send(ServerEvent::Done { id: 4 })
            .unwrap();
        assert!(receiver.recv().await.unwrap().is_current());
        assert!(!disconnected.is_cancelled());
    }

    #[tokio::test]
    async fn snapshot_barrier_holds_output_until_publication() {
        let (sender, mut receiver, _) = ClientEventSender::bounded(8, 1024);
        sender.retarget("target");
        sender.pause_snapshot();
        sender
            .for_session("target")
            .send(ServerEvent::Done { id: 1 })
            .unwrap();
        let frame = receiver.recv().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), frame.ready())
                .await
                .is_err()
        );
        sender.finish_snapshot();
        tokio::time::timeout(std::time::Duration::from_secs(1), frame.ready())
            .await
            .unwrap();
        assert!(frame.is_current());
    }
}
