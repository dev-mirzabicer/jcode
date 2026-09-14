use super::*;
use std::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

type RuntimeSlot = Arc<tokio::sync::Mutex<Option<std::sync::Weak<RuntimeHandle>>>>;
static RUNTIMES: LazyLock<Mutex<HashMap<PathBuf, RuntimeSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) use jcode_base::execution::control_transport::control_in_store;
#[cfg(test)]
use jcode_base::execution::control_transport::exchange;
pub use jcode_base::execution::control_transport::{ControlOperation, ControlReply, control};
use jcode_base::execution::control_transport::{MAX_REQUEST, Request, Response, VERSION};

pub(super) struct RuntimeHandle {
    pub endpoint: RuntimeEndpoint,
    listener: tokio::task::JoinHandle<()>,
    _lease: Arc<File>,
}

#[cfg(test)]
pub(super) async fn end_test_listener(runtime: &RuntimeHandle) {
    runtime.listener.abort();
    while !runtime.listener.is_finished() {
        tokio::task::yield_now().await;
    }
}
struct EndpointLifetime {
    path: PathBuf,
    _lease: Arc<File>,
    #[cfg(unix)]
    identity: (u64, u64),
}
impl Drop for EndpointLifetime {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::symlink_metadata(&self.path)
                .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
            {
                crate::transport::remove_socket(&self.path);
            }
        }
    }
}

pub(super) async fn ensure_running(store: &ExecutionStore) -> Result<Arc<RuntimeHandle>> {
    let slot = RUNTIMES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(store.root().to_path_buf())
        .or_default()
        .clone();
    let mut slot = slot.lock().await;
    if let Some(runtime) = slot
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
        .filter(|runtime| !runtime.listener.is_finished())
    {
        return Ok(runtime.clone());
    }
    let store_for_setup = store.clone();
    let (endpoint, lease) =
        tokio::task::spawn_blocking(move || prepare_endpoint(&store_for_setup)).await??;
    let lease = Arc::new(lease);
    let listener = crate::transport::bind_exclusive(&endpoint.endpoint)?;
    #[cfg(windows)]
    let mut listener = listener;
    let lifetime = EndpointLifetime {
        path: endpoint.endpoint.clone(),
        _lease: lease.clone(),
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(&endpoint.endpoint)?;
            (metadata.dev(), metadata.ino())
        },
    };
    #[cfg(unix)]
    jcode_core::fs::set_permissions_owner_only(&endpoint.endpoint)?;
    let database = store.clone();
    let registered = endpoint.clone();
    tokio::task::spawn_blocking(move || database.register_runtime(&registered)).await??;
    let server_endpoint = endpoint.clone();
    let server_store = store.clone();
    let (owner_tx, owner_rx) = oneshot::channel::<Arc<RuntimeHandle>>();
    let task = tokio::spawn(async move {
        let _lifetime = lifetime;
        let Ok(_owner) = owner_rx.await else {
            return;
        };
        let readers = Arc::new(tokio::sync::Semaphore::new(64));
        let waiters = Arc::new(tokio::sync::Semaphore::new(64));
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(permit) = readers.clone().try_acquire_owned() else {
                continue;
            };
            let endpoint = server_endpoint.clone();
            let store = server_store.clone();
            let waiters = waiters.clone();
            tokio::spawn(async move {
                let _ = serve(stream, store, endpoint, permit, waiters).await;
            });
        }
    });
    let runtime = Arc::new(RuntimeHandle {
        endpoint,
        listener: task,
        _lease: lease,
    });
    *slot = Some(Arc::downgrade(&runtime));
    let _ = owner_tx.send(runtime.clone());
    Ok(runtime)
}

fn prepare_endpoint(store: &ExecutionStore) -> Result<(RuntimeEndpoint, File)> {
    let directory = store.root().join("runtimes");
    crate::storage::ensure_dir(&directory)?;
    ensure!(
        !std::fs::symlink_metadata(&directory)?
            .file_type()
            .is_symlink(),
        "Runtime directory must not be a symlink"
    );
    let id = uuid::Uuid::new_v4().simple().to_string();
    let lease_path = directory.join(format!("{id}.lease"));
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lease = options.open(&lease_path)?;
    lease.try_lock()?;
    #[cfg(unix)]
    let user = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let user = std::process::id();
    let ipc = std::env::temp_dir().join(format!("jx-{user}"));
    crate::storage::ensure_dir(&ipc)?;
    let metadata = std::fs::symlink_metadata(&ipc)?;
    ensure!(
        metadata.is_dir(),
        "Runtime socket directory is not a regular directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            metadata.uid() == user && metadata.permissions().mode() & 0o077 == 0,
            "Runtime socket directory is not private to this user"
        );
    }
    let endpoint = ipc.join(format!("{}.sock", uuid::Uuid::new_v4().simple()));
    let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    Ok((RuntimeEndpoint::new(id, endpoint, lease_path, key), lease))
}

fn key_matches(expected: &str, provided: &str) -> bool {
    expected.len() == provided.len()
        && expected
            .bytes()
            .zip(provided.bytes())
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            == 0
}
async fn serve(
    stream: crate::transport::Stream,
    store: ExecutionStore,
    endpoint: RuntimeEndpoint,
    permit: tokio::sync::OwnedSemaphorePermit,
    waiters: Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    #[cfg(unix)]
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "Execution control peer is not this user"
    );
    let (read, mut write) = stream.into_split();
    let mut bytes = Vec::new();
    let mut reader = BufReader::new(read.take(MAX_REQUEST + 1));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_until(b'\n', &mut bytes),
    )
    .await??;
    ensure!(
        bytes.len() as u64 <= MAX_REQUEST && bytes.last() == Some(&b'\n'),
        "Invalid execution control request size"
    );
    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid execution control request"))?;
    ensure!(
        request.version == VERSION
            && request.instance == endpoint.id
            && key_matches(endpoint.transport_key(), &request.key),
        "Execution control authentication or capability mismatch"
    );
    ensure!(
        request.run_id.len() == 68
            && request.run_id.starts_with("run-")
            && request.run_id[4..].bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid execution identity"
    );
    drop(permit);
    let waiter = if matches!(request.action, ControlOperation::Wait) {
        Some(
            waiters
                .try_acquire_owned()
                .context("Execution wait capacity exhausted")?,
        )
    } else {
        None
    };
    let is_wait = matches!(request.action, ControlOperation::Wait);
    let action = dispatch(&store, &endpoint, &request.run_id, request.action);
    let outcome = if is_wait {
        tokio::select! {
            outcome=action=>outcome,
            _=reader.read_u8()=>return Ok(()),
        }
    } else {
        action.await
    };
    let reply = match outcome {
        Ok(reply) => reply,
        Err(error) => ControlReply::Unavailable {
            message: error.to_string(),
        },
    };
    drop(waiter);
    let mut bytes = serde_json::to_vec(&Response {
        version: VERSION,
        reply,
    })?;
    bytes.push(b'\n');
    tokio::time::timeout(std::time::Duration::from_secs(5), write.write_all(&bytes)).await??;
    Ok(())
}

async fn inspect(store: &ExecutionStore, id: &str) -> Result<RunRecord> {
    let store = store.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || store.inspect(&id)?.context("Unknown invocation")).await?
}
async fn dispatch(
    store: &ExecutionStore,
    endpoint: &RuntimeEndpoint,
    id: &str,
    action: ControlOperation,
) -> Result<ControlReply> {
    let live = LIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(store.root().to_path_buf(), id.to_string()))
        .cloned();
    if let Some(run) = live.filter(|run| {
        run.runtime.endpoint.id == endpoint.id && run.owns_execution.load(Ordering::SeqCst)
    }) {
        match action {
            ControlOperation::ForceStop => {
                ensure!(run.result.borrow().is_none(), "Execution already finished");
                let changed = store
                    .force_native_processes(id, &endpoint.id, &run.stop)
                    .await?;
                return Ok(ControlReply::Accepted { changed });
            }
            ControlOperation::Stop { cause } => {
                let changed = run.result.borrow().is_none();
                if changed {
                    run.stop.fire_with_cause(cause);
                }
                return Ok(ControlReply::Accepted { changed });
            }
            ControlOperation::Background => {
                ensure!(!run.stop.is_set(), "Invocation is stopping");
                let (reply, response) = oneshot::channel();
                run.commands
                    .send(Command::Background(reply))
                    .context("Execution owner unavailable")?;
                return Ok(ControlReply::Accepted {
                    changed: response.await?.map_err(anyhow::Error::msg)?,
                });
            }
            ControlOperation::Wait => {
                let mut result = run.result.clone();
                while result.borrow_and_update().is_none() {
                    result
                        .changed()
                        .await
                        .context("Execution owner ended before completion")?;
                }
            }
            ControlOperation::Inspect => {}
        }
    }
    let record = inspect(store, id).await?;
    if record.owner != endpoint.id && !record.state.terminal() {
        return Ok(ControlReply::OwnerChanged);
    }
    match action {
        ControlOperation::Inspect => Ok(ControlReply::Snapshot {
            record: Box::new(record),
        }),
        ControlOperation::Wait => {
            ensure!(
                record.state.terminal(),
                "Execution owner is unavailable or has not persisted completion"
            );
            Ok(ControlReply::Snapshot {
                record: Box::new(record),
            })
        }
        _ => Ok(ControlReply::Accepted { changed: false }),
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
