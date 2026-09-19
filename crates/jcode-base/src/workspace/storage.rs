use super::*;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::Duration;

pub(crate) const SCHEMA: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    installation: InstallationId,
    request: RequestId,
    ready: bool,
}

/// Shared for catalog use, exclusive for replacement. Kernel lifetime, never PID/TTL.
pub struct CatalogLease {
    _file: File,
}
pub struct RootLease {
    _catalog: CatalogLease,
    _file: File,
    pub(crate) key: String,
}
impl RootLease {
    pub fn validate_binding(&self, binding: &PhysicalBinding) -> Result<()> {
        if self.key != physical_key(binding)? {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Lease belongs to a different physical root",
            ));
        }
        Ok(())
    }
}

fn sqlite_error(error: rusqlite::Error) -> Issue {
    let code = match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            IssueCode::CorruptState
        }
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            IssueCode::Busy
        }
        _ => IssueCode::Io,
    };
    issue(code, error.to_string())
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(issue(
                IssueCode::CorruptState,
                format!("Not a private directory: {}", path.display()),
            ));
        }
    } else {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path).map_err(io)?;
    }
    jcode_core::fs::set_directory_permissions_owner_only(path).map_err(io)
}
pub(crate) fn private_file(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(issue(
            IssueCode::CorruptState,
            "Lease or record is not a regular file",
        ));
    }
    Ok(file)
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path).and_then(|f| f.sync_all()).map_err(io)
}
pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().ok_or_else(|| io("Record has no parent"))?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = private_file(&temp, true)?;
    file.write_all(encode(value)?.as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    std::fs::rename(&temp, path).map_err(io)?;
    sync_dir(parent)
}
pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(corrupt)?;
    if !file.metadata().map_err(corrupt)?.is_file() {
        return Err(corrupt("Record is not regular"));
    }
    serde_json::from_reader(file).map_err(corrupt)
}
impl WorkspaceService {
    fn marker(&self) -> PathBuf {
        self.root.with_file_name("workspace-installation.json")
    }
    pub fn lease(&self, exclusive: bool) -> Result<CatalogLease> {
        let parent = self.root.parent().ok_or_else(|| io("Missing state root"))?;
        if !parent.is_dir() {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Workspace is not initialized",
            ));
        }
        let file = private_file(&parent.join("workspace.lock"), false)?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        result.map_err(|e| issue(IssueCode::Busy, format!("Workspace ownership: {e}")))?;
        Ok(CatalogLease { _file: file })
    }
    pub fn initialize(&self, request: RequestId) -> Result<CatalogStatus> {
        private_dir(self.root.parent().ok_or_else(|| io("Missing state root"))?)?;
        let _lease = self.lease(true)?;
        let marker = self.marker();
        let marker_present = match std::fs::symlink_metadata(&marker) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(corrupt(e)),
        };
        let mut installation = if marker_present {
            let known: Installation = read_json(&marker)?;
            if known.ready {
                return self.connection().and_then(|c| status(&c));
            }
            if known.request != request {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Interrupted initialization belongs to a different request",
                ));
            }
            known
        } else {
            if self.root.try_exists().map_err(io)? {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Workspace exists without installation identity. Preserve it and repair explicitly",
                ));
            }
            let new = Installation {
                installation: InstallationId::new(),
                request,
                ready: false,
            };
            atomic_json(&marker, &new)?;
            new
        };
        private_dir(&self.root)?;
        for child in ["snapshots", "exports", "reviews", "leases", "recovery"] {
            private_dir(&self.root.join(child))?;
        }
        let path = self.root.join("catalog.sqlite3");
        let mut connection = raw_connection(&path, true)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(corrupt)?;
        if version == 0 {
            let tables: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type='table'",
                    [],
                    |r| r.get(0),
                )
                .map_err(corrupt)?;
            if tables != 0 {
                return Err(corrupt("Unrecognized schema during initialization"));
            }
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(io)?;
            let transaction = connection.transaction().map_err(io)?;
            transaction
                .execute_batch(include_str!("schema.sql"))
                .map_err(io)?;
            transaction
                .execute(
                    "INSERT INTO catalog VALUES(1,?1,0)",
                    [installation.installation.to_string()],
                )
                .map_err(io)?;
            transaction.commit().map_err(io)?;
        }
        validate(&connection, installation.installation)?;
        sync_dir(&self.root)?;
        installation.ready = true;
        atomic_json(&marker, &installation)?;
        status(&connection)
    }
    pub fn acquire_root(&self, location: LocationId) -> Result<RootLease> {
        let catalog = self.lease(false)?;
        let connection = self.connection()?;
        let body: String = connection
            .query_row(
                "SELECT body FROM bindings WHERE location=?1",
                [location.to_string()],
                |r| r.get(0),
            )
            .map_err(corrupt)?;
        let bound: BoundLocation = decode(&body)?;
        self.resolver
            .resolve_directory(&bound.binding)
            .map_err(|e| issue(IssueCode::ReplacedRoot, e.to_string()))?;
        let key = physical_key(&bound.binding)?;
        // Same-user physical-root ownership spans independently named state roots.
        #[cfg(unix)]
        let shared = {
            use std::os::unix::fs::MetadataExt;
            let owner = std::fs::metadata(&self.root).map_err(io)?.uid();
            let shared = PathBuf::from(format!("/tmp/jcode-workspace-root-leases-{owner}"));
            if let Ok(meta) = std::fs::symlink_metadata(&shared)
                && (meta.uid() != owner || !meta.is_dir() || meta.file_type().is_symlink())
            {
                return Err(corrupt(
                    "Physical lease directory has a foreign owner or identity",
                ));
            }
            shared
        };
        #[cfg(not(unix))]
        let shared = std::env::temp_dir().join("jcode-workspace-root-leases");
        private_dir(&shared)?;
        let file = private_file(&shared.join(&key), false)?;
        file.try_lock().map_err(|e| {
            issue(
                IssueCode::Busy,
                format!("Physical root has a live owner: {e}"),
            )
        })?;
        Ok(RootLease {
            _catalog: catalog,
            _file: file,
            key,
        })
    }
}
pub(crate) fn physical_key(binding: &PhysicalBinding) -> Result<String> {
    Ok(digest(
        encode(&(binding.volume(), binding.root_witness()))?.as_bytes(),
    ))
}
pub(crate) fn raw_connection(path: &Path, create: bool) -> Result<Connection> {
    // SQLITE_OPEN_NOFOLLOW rejects ancestor aliases too. Resolve the existing
    // parent (for example macOS /tmp -> /private/tmp), never the database leaf.
    let path = path
        .parent()
        .ok_or_else(|| io("Database has no parent"))?
        .canonicalize()
        .map_err(io)?
        .join(
            path.file_name()
                .ok_or_else(|| io("Database has no filename"))?,
        );
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let flags = if create {
        flags | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
    } else {
        flags
    };
    // SQLite exclusively owns its descriptors. A raw close could release POSIX locks.
    let connection = Connection::open_with_flags(&path, flags).map_err(corrupt)?;
    jcode_core::fs::set_permissions_owner_only(&path).map_err(io)?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error)?;
    Ok(connection)
}
pub(crate) fn connect(root: &Path) -> Result<Connection> {
    if root.join("restore-pending.json").try_exists().map_err(io)? {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Catalog replacement is pending. Resume the exact reviewed restore request",
        ));
    }
    let marker: Installation = read_json(&root.with_file_name("workspace-installation.json"))?;
    if !marker.ready {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Initialization is incomplete",
        ));
    }
    let meta = std::fs::symlink_metadata(root).map_err(corrupt)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(corrupt("Workspace directory identity changed"));
    }
    let connection = raw_connection(
        &root
            .canonicalize()
            .map_err(corrupt)?
            .join("catalog.sqlite3"),
        false,
    )?;
    validate(&connection, marker.installation)?;
    Ok(connection)
}
pub(crate) fn validate(connection: &Connection, installation: InstallationId) -> Result<()> {
    let state = status(connection)?;
    if state.installation != installation {
        return Err(corrupt("Foreign workspace installation identity"));
    }
    Ok(())
}

pub(super) fn installation(root: &Path) -> Result<InstallationId> {
    let marker: Installation = read_json(&root.with_file_name("workspace-installation.json"))?;
    Ok(marker.installation)
}
pub(crate) fn status(connection: &Connection) -> Result<CatalogStatus> {
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(corrupt)?;
    if version != SCHEMA {
        return Err(corrupt(format!(
            "Unsupported workspace schema {version}, expected {SCHEMA}"
        )));
    }
    let (installation, revision): (String, i64) = connection
        .query_row(
            "SELECT installation,revision FROM catalog WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(corrupt)?;
    Ok(CatalogStatus {
        installation: installation.parse().map_err(corrupt)?,
        schema: version,
        revision: revision.try_into().map_err(corrupt)?,
        managed_rollout: false,
    })
}
