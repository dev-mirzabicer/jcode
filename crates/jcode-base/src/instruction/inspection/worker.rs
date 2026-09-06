use super::*;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Responsive connection-local ownership, shared by local and remote adapters.
/// Obsolete work cannot publish a snapshot or a reply to newer intent.
#[derive(Default)]
pub struct InspectionWorker {
    state: Arc<Mutex<Option<InstructionInspector>>>,
    cancellation: Arc<AtomicBool>,
}

impl InspectionWorker {
    pub fn submit(
        &mut self,
        repositories: InstructionRepositoryService,
        session_id: String,
        context: impl FnOnce() -> std::result::Result<InspectionContext, InstructionInspectionFailure>
        + Send
        + 'static,
        request: InstructionInspectionRequest,
    ) -> oneshot::Receiver<InstructionInspectionReply> {
        self.cancellation.store(true, Ordering::Release);
        self.cancellation = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancellation);
        let state = Arc::clone(&self.state);
        let (sender, receiver) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let result = (|| {
                canceled(&cancel)?;
                if let InstructionInspectionRequest::Open { filter } = &request {
                    let context = context()?;
                    if context.session_id != session_id {
                        return Err(fail("open", "Session context changed"));
                    }
                    let inspector = InstructionInspector::open(repositories, context, &cancel)?;
                    let snapshot = inspector.snapshot(filter);
                    let mut state = state
                        .lock()
                        .map_err(|_| fail("open", "Inspection worker failed; close and reopen"))?;
                    canceled(&cancel)?;
                    *state = Some(inspector);
                    return Ok(InstructionInspectionReply {
                        session_id: session_id.clone(),
                        snapshot: Some(snapshot.snapshot.clone()),
                        result: InstructionInspectionResult::Opened(snapshot),
                    });
                }
                let mut state = state
                    .lock()
                    .map_err(|_| fail("inspect", "Inspection worker failed; close and reopen"))?;
                canceled(&cancel)?;
                if matches!(request, InstructionInspectionRequest::Close) {
                    *state = None;
                    return Ok(InstructionInspectionReply {
                        session_id: session_id.clone(),
                        snapshot: None,
                        result: InstructionInspectionResult::Closed,
                    });
                }
                if matches!(request, InstructionInspectionRequest::Cancel) {
                    let snapshot = state
                        .as_mut()
                        .filter(|inspector| inspector.context.session_id == session_id)
                        .map(|inspector| {
                            inspector.document = None;
                            inspector.snapshot.clone()
                        });
                    return Ok(InstructionInspectionReply {
                        session_id: session_id.clone(),
                        snapshot,
                        result: InstructionInspectionResult::Canceled,
                    });
                }
                let inspector = state
                    .as_mut()
                    .filter(|inspector| inspector.context.session_id == session_id)
                    .ok_or_else(|| InstructionInspectionFailure {
                        operation: "inspect".into(),
                        detail: "Inspection is absent or belongs to another session. Refresh."
                            .into(),
                        refresh_required: true,
                    })?;
                Ok(inspector.request(request, &cancel))
            })();
            if !cancel.load(Ordering::Acquire) {
                let _ = sender.send(result.unwrap_or_else(|error| InstructionInspectionReply {
                    session_id,
                    snapshot: None,
                    result: InstructionInspectionResult::Failed(error),
                }));
            }
        });
        receiver
    }
}

impl Drop for InspectionWorker {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
    }
}
