//! Hosted tool stdin belongs to a primary, not its first attached connection.
use super::SwarmMember;
use crate::{protocol::ServerEvent, tool::StdinInputRequest};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{RwLock, mpsc, oneshot};

struct Pending {
    event: ServerEvent,
    response: oneshot::Sender<String>,
}
pub(crate) struct PrimaryStdin {
    sender: mpsc::UnboundedSender<StdinInputRequest>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl PrimaryStdin {
    pub(super) fn new(session: String, members: Arc<RwLock<HashMap<String, SwarmMember>>>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<StdinInputRequest>();
        let pending = Arc::new(Mutex::new(HashMap::<String, Pending>::new()));
        let requests = pending.clone();
        let worker = tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                let event = ServerEvent::StdinRequest {
                    request_id: request.request_id.clone(),
                    prompt: request.prompt,
                    is_password: request.is_password,
                    tool_call_id: String::new(),
                };
                {
                    let mut pending = requests.lock().expect("primary stdin");
                    pending.retain(|_, request| !request.response.is_closed());
                    if pending.contains_key(&request.request_id) {
                        continue;
                    }
                    pending.insert(
                        request.request_id,
                        Pending {
                            event: event.clone(),
                            response: request.response_tx,
                        },
                    );
                }
                super::state::fanout_session_event(&members, &session, event).await;
            }
        });
        Self {
            sender,
            pending,
            worker: Mutex::new(Some(worker)),
        }
    }
    pub(crate) async fn shutdown(&self) {
        let worker = self.worker.lock().expect("primary stdin worker").take();
        if let Some(worker) = worker {
            worker.abort();
            let _ = worker.await;
        }
        self.pending.lock().expect("primary stdin").clear();
    }
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<StdinInputRequest> {
        self.sender.clone()
    }
    pub(crate) fn pending(&self) -> Vec<ServerEvent> {
        let mut pending = self.pending.lock().expect("primary stdin");
        pending.retain(|_, request| !request.response.is_closed());
        let mut entries = pending.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(id, _)| *id);
        entries
            .into_iter()
            .map(|(_, request)| request.event.clone())
            .collect()
    }
    pub(crate) fn respond(&self, id: &str, input: String) -> anyhow::Result<()> {
        let request = self
            .pending
            .lock()
            .expect("primary stdin")
            .remove(id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Input request is absent, already answered, or belongs to another primary"
                )
            })?;
        request
            .response
            .send(input)
            .map_err(|_| anyhow::anyhow!("Input request closed before delivery"))
    }
}
impl Drop for PrimaryStdin {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().expect("primary stdin worker").take() {
            worker.abort();
        }
    }
}
