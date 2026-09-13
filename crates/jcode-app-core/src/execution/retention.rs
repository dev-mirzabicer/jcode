//! Runtime hook for quiet retention. One live worker per namespace in-process;
//! the storage owner's kernel lease serializes independent Jcode runtimes.
use crate::execution::{ExecutionStore, StorageConfig};
use anyhow::Result;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

struct Worker {
    lifetime: Weak<()>,
    config: Arc<Mutex<StorageConfig>>,
}

pub(crate) async fn ensure_worker(root: PathBuf) -> Result<()> {
    let config = crate::config::config().output.storage.clone();
    let origin = root.clone();
    let (store, namespace) = tokio::task::spawn_blocking(move || {
        let store = ExecutionStore::open(&origin)?;
        let namespace = store.provider_receipt_namespace()?;
        Ok::<_, anyhow::Error>((store, namespace))
    })
    .await??;
    static WORKERS: OnceLock<Mutex<HashMap<(PathBuf, String), Worker>>> = OnceLock::new();
    let mut workers = WORKERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    workers.retain(|_, worker| worker.lifetime.strong_count() > 0);
    let key = (root.clone(), namespace.clone());
    if let Some(worker) = workers.get(&key) {
        *worker
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = config;
        return Ok(());
    }
    let lifetime = Arc::new(());
    let config = Arc::new(Mutex::new(config));
    workers.insert(
        key,
        Worker {
            lifetime: Arc::downgrade(&lifetime),
            config: config.clone(),
        },
    );
    tokio::spawn(async move {
        let _lifetime = lifetime;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            // Never recreate a deleted fixture/namespace from a delayed worker.
            if !root.join("execution/index.sqlite").is_file() {
                break;
            }
            let store = store.clone();
            let namespace = namespace.clone();
            let config = config
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let result = tokio::task::spawn_blocking(move || {
                if store.provider_receipt_namespace()? != namespace {
                    return Ok(None);
                }
                store
                    .maintain_retention(&config, chrono::Utc::now().timestamp())
                    .map(Some)
            })
            .await;
            match result {
                Ok(Ok(Some(_))) => {}
                Ok(Ok(None)) => break,
                Ok(Err(error)) => {
                    crate::logging::warn(&format!("Output retention maintenance failed: {error:#}"))
                }
                Err(error) => {
                    crate::logging::warn(&format!("Output retention worker failed: {error}"))
                }
            }
        }
    });
    Ok(())
}

/// Explicit human opening/resumption, not a status refresh or maintenance scan.
pub async fn record_session_use(session: String) -> Result<()> {
    let root = crate::storage::jcode_dir()?;
    let origin = root.clone();
    tokio::task::spawn_blocking(move || {
        ExecutionStore::open(&origin)?.touch_activity(&session, chrono::Utc::now().timestamp())
    })
    .await??;
    ensure_worker(root).await
}
