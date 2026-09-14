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
