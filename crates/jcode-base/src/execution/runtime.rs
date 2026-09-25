use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;

/// This is deliberately not serializable. Control credentials must not enter
/// ordinary tool results, metadata listings, exports or debug formatting.
#[derive(Clone)]
pub struct RuntimeEndpoint {
    pub id: String,
    pub endpoint: PathBuf,
    pub lease_path: PathBuf,
    pub protocol_version: u32,
    pub process_id: u32,
    auth_key: String,
    process_image: Option<String>,
}
impl std::fmt::Debug for RuntimeEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeEndpoint")
            .field("id", &self.id)
            .field("endpoint", &self.endpoint)
            .field("protocol_version", &self.protocol_version)
            .field("process_id", &self.process_id)
            .finish_non_exhaustive()
    }
}
impl RuntimeEndpoint {
    pub fn new(id: String, endpoint: PathBuf, lease_path: PathBuf, auth_key: String) -> Self {
        Self {
            id,
            endpoint,
            lease_path,
            auth_key,
            protocol_version: super::control_transport::VERSION,
            process_id: std::process::id(),
            process_image: Some(crate::background::runtime_instance_id().to_string()),
        }
    }
    pub fn transport_key(&self) -> &str {
        &self.auth_key
    }
    pub fn belongs_to_current_image(&self) -> bool {
        self.process_id == std::process::id()
            && self.process_image.as_deref() == Some(crate::background::runtime_instance_id())
    }
    /// Liveness of the registered control endpoint, not proof that a process
    /// has terminated. A missing endpoint never authorizes signalling its PID.
    pub fn has_live_lease(&self) -> Result<bool> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&self.lease_path)
            .context("Runtime ownership lease is unavailable")?;
        ensure!(
            file.metadata()?.is_file(),
            "Runtime lease is not a regular file"
        );
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// An unleased endpoint alone is insufficient. A stopped process, or a
    /// different verified process-image token at the same PID, supplies evidence
    /// that its old in-process tasks cannot still execute. No PID is signalled.
    #[cfg(unix)]
    pub(super) fn image_is_gone(&self, store: &ExecutionStore) -> Result<bool> {
        if self.has_live_lease()? {
            return Ok(false);
        }
        if !crate::platform::is_process_running(self.process_id) {
            return Ok(true);
        }
        let Some(image) = &self.process_image else {
            return Ok(false);
        };
        if self.process_id == std::process::id()
            && image != crate::background::runtime_instance_id()
        {
            return Ok(true);
        }
        let connection = store.connection()?;
        let mut query=connection.prepare("SELECT id FROM runtimes WHERE process_id=?1 AND process_image IS NOT NULL AND process_image<>?2")?;
        let candidates = query
            .query_map(params![self.process_id, image], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for id in candidates {
            if let Some(current) = store.runtime_endpoint(&id)?
                && current.has_live_lease()?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
impl ExecutionStore {
    /// Immutable additive ownership evidence. Keeping it outside the shared
    /// database schema lets older independent command workers finish normally.
    pub fn bind_runtime_namespace(&self, id: &str, namespace: &str) -> Result<()> {
        let endpoint = self
            .runtime_endpoint(id)?
            .context("Execution owner is not registered")?;
        ensure!(
            endpoint.belongs_to_current_image() && endpoint.has_live_lease()?,
            "Execution endpoint is not owned by this runtime image"
        );
        validate_namespace(namespace)?;
        let path = self.runtime_namespace_path(id)?;
        let binding = NamespaceBinding {
            schema: 1,
            runtime: id.into(),
            namespace: namespace.into(),
        };
        let stage = path.with_extension(format!("{}.stage", uuid::Uuid::new_v4()));
        crate::storage::write_json_secret(&stage, &binding)?;
        // hard_link is an atomic no-replace publication on this same directory.
        // A competing or previous binding is validated, never overwritten.
        let published = std::fs::hard_link(&stage, &path);
        let cleanup = std::fs::remove_file(&stage);
        match published {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("Publish execution namespace binding"),
        }
        std::fs::File::open(
            path.parent()
                .context("Missing execution namespace directory")?,
        )?
        .sync_all()?;
        cleanup.context("Remove owned execution namespace publication stage")?;
        ensure!(
            self.runtime_namespace(id)?.as_deref() == Some(namespace),
            "Execution owner already belongs to another runtime namespace"
        );
        Ok(())
    }

    pub fn runtime_namespace(&self, id: &str) -> Result<Option<String>> {
        let path = self.runtime_namespace_path(id)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Read execution namespace binding"),
        };
        ensure!(
            file.metadata()?.is_file(),
            "Execution namespace binding changed type"
        );
        let binding: NamespaceBinding = serde_json::from_reader(file)?;
        ensure!(
            binding.schema == 1 && binding.runtime == id,
            "Execution namespace binding identity changed"
        );
        validate_namespace(&binding.namespace)?;
        Ok(Some(binding.namespace))
    }

    fn runtime_namespace_path(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid runtime identity"
        );
        Ok(self
            .root()
            .join("runtimes")
            .join(format!("{id}.namespace.json")))
    }

    pub fn register_runtime(&self, runtime: &RuntimeEndpoint) -> Result<()> {
        ensure!(
            runtime.id.len() == 32 && runtime.id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid runtime identity"
        );
        ensure!(
            runtime.auth_key.len() == 64,
            "Invalid runtime control credential"
        );
        self.connection()?.execute("INSERT INTO runtimes (id,endpoint,auth_key,lease_path,protocol_version,process_id,process_image) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![runtime.id,runtime.endpoint.to_str().context("Non-UTF-8 runtime endpoint")?,runtime.auth_key,runtime.lease_path.to_str().context("Non-UTF-8 runtime lease")?,runtime.protocol_version,runtime.process_id,runtime.process_image])?;
        Ok(())
    }
    pub fn runtime_endpoint(&self, id: &str) -> Result<Option<RuntimeEndpoint>> {
        Ok(self.connection()?.query_row("SELECT endpoint,auth_key,lease_path,protocol_version,process_id,process_image FROM runtimes WHERE id=?1",[id],|row| {
            Ok(RuntimeEndpoint{id:id.to_string(),endpoint:PathBuf::from(row.get::<_,String>(0)?),auth_key:row.get(1)?,lease_path:PathBuf::from(row.get::<_,String>(2)?),protocol_version:row.get(3)?,process_id:row.get(4)?,process_image:row.get(5)?})
        }).optional()?)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceBinding {
    schema: u32,
    runtime: String,
    namespace: String,
}
fn validate_namespace(namespace: &str) -> Result<()> {
    ensure!(
        namespace.len() == 64 && namespace.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid runtime namespace"
    );
    Ok(())
}

#[cfg(test)]
mod namespace_tests {
    use super::*;

    #[test]
    fn execution_namespace_binding_is_private_immutable_and_schema_neutral() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let before: i64 = store
            .connection()?
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        let runtimes = store.root().join("runtimes");
        crate::storage::ensure_dir(&runtimes)?;
        let lease_path = runtimes.join("fixture.lease");
        let lease = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&lease_path)?;
        lease.try_lock()?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let endpoint = RuntimeEndpoint::new(
            id.clone(),
            runtimes.join("fixture.sock"),
            lease_path,
            "a".repeat(64),
        );
        store.register_runtime(&endpoint)?;
        assert!(store.runtime_namespace(&id)?.is_none());
        store.bind_runtime_namespace(&id, &"b".repeat(64))?;
        let path = store.runtime_namespace_path(&id)?;
        let bytes = std::fs::read(&path)?;
        store.bind_runtime_namespace(&id, &"b".repeat(64))?;
        assert_eq!(std::fs::read(&path)?, bytes);
        assert!(store.bind_runtime_namespace(&id, &"c".repeat(64)).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            store
                .connection()?
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            before
        );
        std::fs::write(&path, b"corrupt")?;
        assert!(store.runtime_namespace(&id).is_err());
        assert!(store.bind_runtime_namespace(&id, &"b".repeat(64)).is_err());
        assert_eq!(std::fs::read(&path)?, b"corrupt");
        drop(lease);
        assert!(store.bind_runtime_namespace(&id, &"b".repeat(64)).is_err());
        Ok(())
    }

    #[test]
    fn concurrent_namespace_claims_have_one_immutable_winner() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let runtimes = store.root().join("runtimes");
        crate::storage::ensure_dir(&runtimes)?;
        let lease_path = runtimes.join("fixture.lease");
        let lease = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&lease_path)?;
        lease.try_lock()?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        store.register_runtime(&RuntimeEndpoint::new(
            id.clone(),
            runtimes.join("fixture.sock"),
            lease_path,
            "a".repeat(64),
        ))?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let tasks = ['b', 'c']
            .into_iter()
            .map(|letter| {
                let store = store.clone();
                let id = id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.bind_runtime_namespace(&id, &letter.to_string().repeat(64))
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .filter(Result::is_ok)
                .count(),
            1
        );
        assert!(store.runtime_namespace(&id)?.is_some());
        Ok(())
    }
}
