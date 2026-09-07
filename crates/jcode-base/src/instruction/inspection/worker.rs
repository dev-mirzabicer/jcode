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

/// Carries inspection authority into the mutation worker without accepting a
/// client-supplied filesystem path or retaining the inspector's content there.
#[derive(Clone)]
pub struct InstructionTargetResolver {
    state: Arc<Mutex<Option<InstructionInspector>>>,
}

pub(crate) struct ResolvedManagementTarget {
    pub context: InspectionContext,
    pub repository: Option<InstructionRepositoryRef>,
    pub path: Option<PathBuf>,
    pub row: Option<InstructionRow>,
    pub resource: Option<InstructionResourceRef>,
}

impl InstructionTargetResolver {
    pub(crate) fn resolve(
        &self,
        session: &str,
        snapshot: &str,
        target: &InstructionInspectionTarget,
    ) -> Result<ResolvedManagementTarget> {
        let state = self
            .state
            .lock()
            .map_err(|_| fail("resolve edit target", "Inspector failed; reopen it"))?;
        let inspector = state
            .as_ref()
            .filter(|value| value.context.session_id == session)
            .ok_or_else(|| {
                fail(
                    "resolve edit target",
                    "Open instruction inspection for this session first",
                )
            })?;
        inspector.check_snapshot(snapshot)?;
        let (repository, path, row, resource) = match target {
            InstructionInspectionTarget::Resource(key) => {
                let resource = inspector
                    .resources
                    .get(key)
                    .ok_or_else(|| fail("resolve edit target", "Resource selection expired"))?;
                (
                    inspector
                        .stores
                        .get(&resource.row.repository)
                        .and_then(|store| store.reference.clone()),
                    Some(resource.path.clone()),
                    Some(resource.row.clone()),
                    resource.managed.clone(),
                )
            }
            InstructionInspectionTarget::Repository(key) => {
                let store = inspector
                    .stores
                    .get(key)
                    .ok_or_else(|| fail("resolve edit target", "Repository selection expired"))?;
                (store.reference.clone(), None, None, None)
            }
            InstructionInspectionTarget::Session => (None, None, None, None),
        };
        Ok(ResolvedManagementTarget {
            context: inspector.context.clone(),
            repository,
            path,
            row,
            resource,
        })
    }
}

impl InspectionWorker {
    pub fn target_resolver(&self) -> InstructionTargetResolver {
        InstructionTargetResolver {
            state: Arc::clone(&self.state),
        }
    }
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
