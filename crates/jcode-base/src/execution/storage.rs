//! Reserve-aware output placement and journaled whole-bundle relocation.
use super::{ExecutionStore, RunRecord};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[path = "cleanup.rs"]
mod cleanup;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveConfig {
    pub mount: PathBuf,
    pub volume_uuid: String,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub local_reserve_bytes: u64,
    pub archive_reserve_bytes: u64,
    pub archive: Option<ArchiveConfig>,
}
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            local_reserve_bytes: 1024 * 1024 * 1024,
            archive_reserve_bytes: 1024 * 1024 * 1024,
            archive: None,
        }
    }
}

pub(super) trait StorageEnvironment: Send + Sync {
    fn available(&self, path: &Path) -> Result<u64>;
    fn archive(&self, config: &ArchiveConfig) -> Result<DirectoryBinding>;
    fn existing_archive(&self, config: &ArchiveConfig) -> Result<DirectoryBinding> {
        self.archive(config)
    }
    fn checkpoint(&self, _stage: &str) -> Result<()> {
        Ok(())
    }
    fn write(&self, _path: &Path, file: &mut File, bytes: &[u8]) -> std::io::Result<usize> {
        file.write(bytes)
    }
}
struct NativeEnvironment;
impl StorageEnvironment for NativeEnvironment {
    fn available(&self, path: &Path) -> Result<u64> {
        available_bytes(path)
    }
    fn archive(&self, config: &ArchiveConfig) -> Result<DirectoryBinding> {
        verified_archive(config)
    }
    fn existing_archive(&self, config: &ArchiveConfig) -> Result<DirectoryBinding> {
        resolve_archive(config, false)
    }
}

fn verified_archive(config: &ArchiveConfig) -> Result<DirectoryBinding> {
    resolve_archive(config, true)
}

fn resolve_archive(config: &ArchiveConfig, create: bool) -> Result<DirectoryBinding> {
    ensure!(
        !config.directory.as_os_str().is_empty()
            && config
                .directory
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Archive directory must be a relative path without traversal"
    );
    ensure!(
        !config.volume_uuid.is_empty()
            && config
                .volume_uuid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "Invalid archive volume UUID"
    );
    // Never create the mount itself. An absent drive must not become a local directory.
    let mount = config
        .mount
        .canonicalize()
        .context("Configured output archive is offline")?;
    let mut binding = DirectoryBinding::open(&mount)?;
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        let info = Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist"])
            .arg(&mount)
            .output()?;
        ensure!(
            info.status.success(),
            "Cannot verify output archive identity"
        );
        let mut converter = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        converter
            .stdin
            .take()
            .context("Missing plist converter input")?
            .write_all(&info.stdout)?;
        let converted = converter.wait_with_output()?;
        ensure!(
            converted.status.success(),
            "Invalid output archive identity response"
        );
        let value: serde_json::Value = serde_json::from_slice(&converted.stdout)?;
        ensure!(
            value["VolumeUUID"]
                .as_str()
                .is_some_and(|uuid| uuid.eq_ignore_ascii_case(&config.volume_uuid))
                && value["MountPoint"]
                    .as_str()
                    .is_some_and(|path| Path::new(path) == mount),
            "Output archive volume identity does not match configuration"
        );
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let device = std::fs::metadata(Path::new("/dev/disk/by-uuid").join(&config.volume_uuid))?;
        ensure!(
            device.rdev() == std::fs::metadata(&mount)?.dev(),
            "Output archive volume identity does not match configuration"
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("Verified archive-volume placement is unsupported on this platform");
    binding.verify()?;
    for component in config.directory.components() {
        let name = component
            .as_os_str()
            .to_str()
            .context("Archive directory must be UTF-8")?;
        binding = if create {
            binding.create_child(name)?
        } else {
            DirectoryBinding::open(&binding.path.join(name))?
        };
    }
    Ok(binding)
}

fn available_bytes(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: name is NUL-terminated and stats points to writable statvfs storage.
        if unsafe { libc::statvfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful statvfs initialized the entire structure.
        let stats = unsafe { stats.assume_init() };
        Ok(
            (u128::from(stats.f_bavail) * u128::from(stats.f_frsize)).min(u128::from(u64::MAX))
                as u64,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        bail!("Reserve-aware output storage is unsupported on this platform")
    }
}

fn private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(path)?.is_dir()
                && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "Unexpected output directory identity: {}",
            path.display()
        );
    } else {
        std::fs::create_dir_all(path)?;
    }
    jcode_core::fs::set_directory_permissions_owner_only(path)?;
    Ok(())
}

pub(super) struct OutputLease {
    root: PathBuf,
    id: String,
    _file: File,
}
impl OutputLease {
    pub(super) fn validate(&self, store: &ExecutionStore, id: &str) -> Result<()> {
        ensure!(
            self.root == store.root() && self.id == id,
            "Output lease belongs to a different store or invocation"
        );
        Ok(())
    }
}
pub(super) fn output_lease(store: &ExecutionStore, id: &str) -> Result<OutputLease> {
    try_output_lease(store, id)?.context("Output is owned by an active writer or storage operation")
}
pub(super) fn try_output_lease(store: &ExecutionStore, id: &str) -> Result<Option<OutputLease>> {
    ensure!(
        id.starts_with("run-") && id.len() == 68 && id[4..].bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid invocation identity"
    );
    let locks = store.root().join("locks");
    private_directory(&locks)?;
    let path = locks.join(id);
    let file = private_open(&path, false)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    Ok(Some(OutputLease {
        root: store.root().to_path_buf(),
        id: id.into(),
        _file: file,
    }))
}

fn private_open(path: &Path, truncate: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Output part must be a regular file"
    );
    Ok(file)
}

/// The directory handle pins the verified filesystem object. Path replacement
/// cannot redirect an append to a different mount or another local directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: std::time::SystemTime,
}
impl DirectoryIdentity {
    fn of(metadata: &std::fs::Metadata) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                created: metadata.created()?,
            })
        }
    }
}
pub(super) struct DirectoryBinding {
    path: PathBuf,
    directory: File,
}
impl DirectoryBinding {
    fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
        }
        let directory = options.open(path)?;
        ensure!(
            directory.metadata()?.is_dir(),
            "Output location is not a directory"
        );
        Ok(Self {
            path: path.to_path_buf(),
            directory,
        })
    }
    fn verify(&self) -> Result<()> {
        let current =
            std::fs::symlink_metadata(&self.path).context("Output location is offline")?;
        ensure!(current.is_dir(), "Output directory was replaced");
        ensure!(
            self.identity()? == DirectoryIdentity::of(&current)?,
            "Output directory or mounted volume identity changed"
        );
        Ok(())
    }
    fn identity(&self) -> Result<DirectoryIdentity> {
        DirectoryIdentity::of(&self.directory.metadata()?)
    }
    fn create_child(&self, name: &str) -> Result<Self> {
        ensure!(
            !name.is_empty()
                && Path::new(name).components().count() == 1
                && matches!(
                    Path::new(name).components().next(),
                    Some(Component::Normal(_))
                ),
            "Invalid directory leaf"
        );
        self.verify()?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let leaf = std::ffi::CString::new(name)?;
            // SAFETY: both operations are relative to the owned directory FD.
            // The leaf cannot traverse or contain NUL. A successful FD is owned once.
            let result = unsafe { libc::mkdirat(self.directory.as_raw_fd(), leaf.as_ptr(), 0o700) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(std::io::Error::last_os_error().into());
            }
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    leaf.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let directory = unsafe { File::from_raw_fd(fd) };
            self.directory.sync_all()?;
            let child = Self {
                path: self.path.join(name),
                directory,
            };
            child.verify()?;
            Ok(child)
        }
        #[cfg(not(unix))]
        {
            let path = self.path.join(name);
            private_directory(&path)?;
            Self::open(&path)
        }
    }
    fn append_file(&self, name: &str) -> Result<File> {
        self.verify()?;
        validate_part(name)?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name)?;
            // SAFETY: the directory FD is owned and the NUL-terminated leaf is
            // validated above. The successful FD is transferred to File once.
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_APPEND
                        | libc::O_CREAT
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            ensure!(
                file.metadata()?.is_file(),
                "Output part is not a regular file"
            );
            Ok(file)
        }
        #[cfg(not(unix))]
        private_open(&self.path.join(name), false)
    }

    fn publish_part(&self, temporary: &str, name: &str) -> Result<()> {
        validate_part(temporary)?;
        validate_part(name)?;
        self.verify()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let temporary = std::ffi::CString::new(temporary)?;
            let name = std::ffi::CString::new(name)?;
            // SAFETY: both validated leaf names use the same owned directory FD.
            let result = unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    temporary.as_ptr(),
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(unix))]
        std::fs::rename(self.path.join(temporary), self.path.join(name))?;
        self.directory.sync_all()?;
        Ok(())
    }

    fn read_part(&self, name: &str) -> Result<File> {
        validate_part(name)?;
        self.verify()?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name)?;
            // SAFETY: the validated leaf is opened relative to the owned FD.
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            ensure!(file.metadata()?.is_file(), "Output part changed type");
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            Ok(File::open(self.path.join(name))?)
        }
    }
    fn remove_part(&self, name: &str) -> Result<()> {
        validate_part(name)?;
        self.verify()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name)?;
            // SAFETY: unlink affects only this leaf in the still-owned directory.
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(unix))]
        std::fs::remove_file(self.path.join(name))?;
        self.directory.sync_all()?;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

fn publish_alias(alias: &Path, source: Option<&Path>, destination: &Path) -> Result<()> {
    let parent = alias.parent().context("Missing output alias parent")?;
    private_directory(parent)?;
    if let Ok(existing) = std::fs::symlink_metadata(alias) {
        ensure!(
            existing.file_type().is_symlink(),
            "Refusing to replace a non-alias output path"
        );
        let target = std::fs::read_link(alias)?;
        if target == destination {
            return sync_directory(parent);
        }
        ensure!(
            source == Some(target.as_path()),
            "Output alias changed outside the storage transaction"
        );
    }
    let temporary = parent.join(format!(".alias-{}", uuid::Uuid::new_v4().simple()));
    #[cfg(unix)]
    std::os::unix::fs::symlink(destination, &temporary)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(destination, &temporary)?;
    #[cfg(not(any(unix, windows)))]
    bail!("Stable output aliases are unsupported on this platform");
    std::fs::rename(&temporary, alias)?;
    sync_directory(parent)
}

pub(super) struct BundleStorage {
    pub store: ExecutionStore,
    pub id: String,
    pub physical: PathBuf,
    pub archived: bool,
    config: StorageConfig,
    environment: Arc<dyn StorageEnvironment>,
    _lease: OutputLease,
    binding: DirectoryBinding,
}
impl BundleStorage {
    pub fn create(
        store: ExecutionStore,
        record: &RunRecord,
        config: StorageConfig,
    ) -> Result<Self> {
        Self::create_with_environment(store, record, config, Arc::new(NativeEnvironment))
    }

    pub(super) fn create_with_environment(
        store: ExecutionStore,
        record: &RunRecord,
        config: StorageConfig,
        environment: Arc<dyn StorageEnvironment>,
    ) -> Result<Self> {
        let lease = output_lease(&store, &record.id)?;
        let local = store.root().join("data");
        private_directory(&local)?;
        let archived =
            environment.available(&local)? < config.local_reserve_bytes.saturating_add(4096);
        let parent = if archived {
            archive_namespace(&store, &config, environment.as_ref())?
        } else {
            DirectoryBinding::open(&local)?
        };
        let reserve = if archived {
            config.archive_reserve_bytes
        } else {
            config.local_reserve_bytes
        };
        ensure!(
            environment.available(&parent.path)? >= reserve.saturating_add(4096),
            "No usable storage can retain new output while preserving receipt headroom"
        );
        let physical = parent.path.join(&record.id);
        ensure!(
            !physical.exists(),
            "Output bundle already exists; recover the original invocation instead of repeating it"
        );
        let mut connection = store.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running')",
            params![record.id, record.owner],
            |row| row.get(0),
        )?;
        ensure!(owned, "Invocation is not owned and running");
        tx.execute("INSERT INTO output_allocations (id,physical,archived,owner,stage,archive_spec) VALUES (?1,?2,?3,?4,'prepared',?5)",params![record.id,physical.to_str().context("Non-UTF-8 output path")?,archived,record.owner,config.archive.as_ref().map(serde_json::to_string).transpose()?])?;
        tx.commit()?;
        complete_allocation(&store, record, &parent, archived, environment.as_ref())?;
        let binding = DirectoryBinding::open(&physical)?;
        Ok(Self {
            store,
            id: record.id.clone(),
            physical,
            archived,
            config,
            environment,
            _lease: lease,
            binding,
        })
    }

    pub fn alias(&self) -> PathBuf {
        self.store.root().join("outputs").join(&self.id)
    }

    pub fn read_part(&self, name: &str) -> Result<File> {
        self.binding.read_part(name)
    }

    fn ensure_capacity(&mut self, bytes: u64) -> Result<()> {
        self.binding.verify()?;
        let reserve = if self.archived {
            self.config.archive_reserve_bytes
        } else {
            self.config.local_reserve_bytes
        };
        if self.environment.available(&self.physical)? >= reserve.saturating_add(bytes) {
            return Ok(());
        }
        ensure!(
            !self.archived,
            "Output archive cannot retain more data; captured prefix is preserved"
        );
        self.spill()?;
        ensure!(
            self.environment.available(&self.physical)?
                >= self.config.archive_reserve_bytes.saturating_add(bytes),
            "Output archive cannot retain more data; captured prefix is preserved"
        );
        Ok(())
    }

    pub fn append(&mut self, name: &str, mut bytes: &[u8]) -> Result<()> {
        validate_part(name)?;
        while !bytes.is_empty() {
            let wanted = bytes.len().min(64 * 1024);
            self.ensure_capacity(wanted as u64)?;
            let path = self.physical.join(name);
            let mut file = self.binding.append_file(name)?;
            match self.environment.write(&path, &mut file, &bytes[..wanted]) {
                Ok(0) => bail!(
                    "Output write made no progress; captured prefix is preserved at {}",
                    self.alias().display()
                ),
                Ok(written) => {
                    bytes = &bytes[written..];
                    file.sync_data()?;
                    self.binding.directory.sync_all()?;
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOSPC) && !self.archived => {
                    drop(file);
                    self.spill()?;
                }
                Err(error) => {
                    return Err(error).context(format!(
                        "Output capture failed; retained prefix: {}",
                        self.alias().display()
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn write_file(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        validate_part(name)?;
        let temporary = format!("stage-{}", uuid::Uuid::new_v4().simple());
        // Failure leaves the partial owned file in the bundle, never in a drop-deleted tempfile.
        self.append(&temporary, bytes)?;
        if bytes.is_empty() {
            self.binding.append_file(&temporary)?.sync_all()?;
        }
        self.binding.publish_part(&temporary, name)
    }

    pub fn write_json<T: Serialize>(&mut self, name: &str, value: &T) -> Result<()> {
        validate_part(name)?;
        let temporary = format!("stage-{}", uuid::Uuid::new_v4().simple());
        {
            struct Sink<'a> {
                storage: &'a mut BundleStorage,
                name: &'a str,
            }
            impl Write for Sink<'_> {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.storage
                        .append(self.name, bytes)
                        .map_err(std::io::Error::other)?;
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut writer = std::io::BufWriter::with_capacity(
                64 * 1024,
                Sink {
                    storage: self,
                    name: &temporary,
                },
            );
            let result = serde_json::to_writer(&mut writer, value)
                .map_err(anyhow::Error::from)
                .and_then(|()| writer.flush().map_err(anyhow::Error::from));
            // BufWriter's Drop retries flushes. On a partially retained write
            // that would duplicate bytes; disassemble it without another write.
            let (_, _buffer) = writer.into_parts();
            result?;
        }
        self.binding.publish_part(&temporary, name)
    }

    fn spill(&mut self) -> Result<()> {
        let pending: bool = self.store.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM relocations WHERE id=?1 AND stage<>'complete')",
            [&self.id],
            |row| row.get(0),
        )?;
        ensure!(
            !pending,
            "Output relocation is interrupted; preserve the prefix and recover its existing journal before further writes"
        );
        let root = archive_namespace(&self.store, &self.config, self.environment.as_ref())?;
        let destination = root
            .path
            .join(format!("{}-{}", self.id, uuid::Uuid::new_v4().simple()));
        let mut manifest = MoveManifest::capture(&self.id, &self.physical, &destination)?;
        manifest.archive_spec = self.config.archive.clone();
        let required = manifest.files.iter().try_fold(0u64, |total, part| {
            total
                .checked_add(part.bytes)
                .context("Output bundle size overflow")
        })?;
        ensure!(
            self.environment.available(&root.path)?
                >= self.config.archive_reserve_bytes.saturating_add(required),
            "Archive lacks space for the captured prefix"
        );
        let journal_dir = self.store.root().join("moves");
        private_directory(&journal_dir)?;
        let journal = journal_dir.join(format!(
            "{}-{}.json",
            self.id,
            uuid::Uuid::new_v4().simple()
        ));
        crate::storage::write_json_secret(&journal, &manifest)?;
        self.store.connection()?.execute("INSERT INTO relocations (id,source,destination,stage,manifest_path) VALUES (?1,?2,?3,'copying',?4) ON CONFLICT(id) DO UPDATE SET source=excluded.source,destination=excluded.destination,stage=excluded.stage,manifest_path=excluded.manifest_path WHERE relocations.stage='complete'", params![self.id,self.physical.to_str(),destination.to_str(),journal.to_str()])?;
        complete_move(&self.store, &manifest, self.environment.as_ref())?;
        self.physical = destination;
        self.archived = true;
        self.binding = DirectoryBinding::open(&self.physical)?;
        Ok(())
    }
}

fn archive_namespace(
    store: &ExecutionStore,
    config: &StorageConfig,
    environment: &dyn StorageEnvironment,
) -> Result<DirectoryBinding> {
    let archive = config
        .archive
        .as_ref()
        .context("Output archive is not configured and local receipt headroom is exhausted")?;
    let root = environment.archive(archive)?;
    let namespace = format!(
        "{:x}",
        Sha256::digest(store.root().as_os_str().as_encoded_bytes())
    );
    root.create_child(&namespace)
}

fn complete_allocation(
    store: &ExecutionStore,
    record: &RunRecord,
    parent: &DirectoryBinding,
    archived: bool,
    environment: &dyn StorageEnvironment,
) -> Result<()> {
    let physical = parent.path.join(&record.id);
    let row: (String, bool, String, String) = store.connection()?.query_row(
        "SELECT physical,archived,owner,stage FROM output_allocations WHERE id=?1",
        [&record.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    ensure!(
        Path::new(&row.0) == physical && row.1 == archived && row.2 == record.owner,
        "Output allocation differs from its durable intent"
    );
    if row.3 == "published" {
        return Ok(());
    }
    environment.checkpoint("allocation_before_files")?;
    let binding = parent.create_child(&record.id)?;
    let identity = serde_json::to_vec(
        &serde_json::json!({"schema":1,"invocation_id":record.id,"owner":record.owner}),
    )?;
    let mut file = binding.append_file("identity.json")?;
    let mut prior = Vec::new();
    (&mut file)
        .take(identity.len() as u64 + 1)
        .read_to_end(&mut prior)?;
    ensure!(
        identity.starts_with(&prior),
        "Output directory identity conflicts with its recorded allocation"
    );
    // Recovery completes only the exact recorded identity prefix. No random
    // orphan file is adopted, removed or used to infer an operation.
    file.write_all(&identity[prior.len()..])?;
    file.sync_all()?;
    let output = binding.append_file("output.txt")?;
    ensure!(
        output.metadata()?.len() == 0,
        "Unpublished allocation unexpectedly contains producer output"
    );
    output.sync_all()?;
    binding.directory.sync_all()?;
    sync_directory(physical.parent().context("Missing output parent")?)?;
    environment.checkpoint("allocation_after_files")?;
    let alias = store.root().join("outputs").join(&record.id);
    let mut connection = store.connection()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let location: Option<(String, bool)> = tx
        .query_row(
            "SELECT physical,archived FROM output_locations WHERE id=?1",
            [&record.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((path, is_archived)) = location {
        ensure!(
            Path::new(&path) == physical && is_archived == archived,
            "Published allocation has a different location"
        );
    } else {
        tx.execute(
            "INSERT INTO output_locations (id,physical,archived,archive_spec) SELECT ?1,?2,?3,archive_spec FROM output_allocations WHERE id=?1",
            params![record.id, physical.to_str(), archived],
        )?;
    }
    ensure!(
        tx.execute(
            "UPDATE runs SET output_path=?3 WHERE id=?1 AND owner=?2",
            params![record.id, record.owner, alias.join("output.txt").to_str()]
        )? == 1,
        "Allocation lost invocation ownership"
    );
    tx.commit()?;
    environment.checkpoint("allocation_after_location")?;
    publish_alias(&alias, None, &physical)?;
    environment.checkpoint("allocation_after_alias")?;
    ensure!(
        store.connection()?.execute(
            "UPDATE output_allocations SET stage='published' WHERE id=?1 AND stage='prepared'",
            [&record.id]
        )? == 1,
        "Allocation publication lost its journal"
    );
    Ok(())
}

fn validate_part(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && Path::new(name).components().count() == 1
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && name != "."
            && name != "..",
        "Invalid output part name"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Part {
    name: String,
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct MoveManifest {
    id: String,
    source: PathBuf,
    destination: PathBuf,
    source_identity: DirectoryIdentity,
    destination_parent_identity: DirectoryIdentity,
    files: Vec<Part>,
    #[serde(default)]
    archive_spec: Option<ArchiveConfig>,
}
impl MoveManifest {
    fn capture(id: &str, source: &Path, destination: &Path) -> Result<Self> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_file(),
                "Unexpected non-file in owned output bundle"
            );
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Non-UTF-8 output part"))?;
            validate_part(&name)?;
            files.push(Part {
                name,
                bytes: entry.metadata()?.len(),
                sha256: hash_file(&entry.path())?,
            });
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        let source_identity = DirectoryBinding::open(source)?.identity()?;
        let destination_parent_identity =
            DirectoryBinding::open(destination.parent().context("Missing destination parent")?)?
                .identity()?;
        Ok(Self {
            id: id.into(),
            source: source.into(),
            destination: destination.into(),
            source_identity,
            destination_parent_identity,
            files,
            archive_spec: None,
        })
    }
}
fn hash_file(path: &Path) -> Result<String> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "Output part changed type"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    hash_reader(options.open(path)?)
}

fn hash_reader(mut file: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn complete_move(
    store: &ExecutionStore,
    manifest: &MoveManifest,
    environment: &dyn StorageEnvironment,
) -> Result<()> {
    let connection = store.connection()?;
    let (source, destination, stage): (String, String, String) = connection.query_row(
        "SELECT source,destination,stage FROM relocations WHERE id=?1",
        [&manifest.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    ensure!(
        Path::new(&source) == manifest.source && Path::new(&destination) == manifest.destination,
        "Relocation manifest differs from its durable journal"
    );
    let location = store
        .output_location(&manifest.id)?
        .context("Missing output location")?
        .0;
    ensure!(
        location == manifest.source || location == manifest.destination,
        "Relocation current location differs from both recorded endpoints"
    );
    if stage == "complete" {
        return Ok(());
    }
    let source_binding = if manifest.source.exists() {
        let binding = DirectoryBinding::open(&manifest.source)?;
        ensure!(
            binding.identity()? == manifest.source_identity,
            "Relocation source directory was replaced"
        );
        Some(binding)
    } else {
        None
    };
    let parent = DirectoryBinding::open(
        manifest
            .destination
            .parent()
            .context("Missing relocation parent")?,
    )?;
    ensure!(
        parent.identity()? == manifest.destination_parent_identity,
        "Relocation archive directory or volume changed"
    );
    environment.checkpoint("before_copy")?;
    let destination_binding = parent.create_child(
        manifest
            .destination
            .file_name()
            .and_then(|name| name.to_str())
            .context("Invalid relocation destination")?,
    )?;
    for part in &manifest.files {
        destination_binding.verify()?;
        validate_part(&part.name)?;
        let destination = manifest.destination.join(&part.name);
        if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
            ensure!(
                metadata.is_file(),
                "Relocation destination is not an owned regular part"
            );
            if metadata.len() == part.bytes && hash_file(&destination)? == part.sha256 {
                continue;
            }
        }
        let source = manifest.source.join(&part.name);
        ensure!(
            std::fs::symlink_metadata(&source)?.is_file(),
            "Relocation source changed type"
        );
        let mut input = source_binding
            .as_ref()
            .context("Relocation source is unavailable")?
            .read_part(&part.name)?;
        let mut output = destination_binding.append_file(&part.name)?;
        let previous = output.metadata()?.len();
        ensure!(
            previous <= part.bytes,
            "Unexpected oversized relocation destination; preserving it"
        );
        // Only resume a prefix that matches the recorded source. Never truncate
        // a pre-existing destination merely because it has the expected name.
        let mut remaining = previous;
        let mut source_bytes = [0u8; 64 * 1024];
        let mut destination_bytes = [0u8; 64 * 1024];
        while remaining > 0 {
            let n = remaining.min(source_bytes.len() as u64) as usize;
            input.read_exact(&mut source_bytes[..n])?;
            output.read_exact(&mut destination_bytes[..n])?;
            ensure!(
                source_bytes[..n] == destination_bytes[..n],
                "Unexpected relocation destination bytes; preserving them"
            );
            remaining -= n as u64;
        }
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        output.rewind()?;
        ensure!(
            output.metadata()?.len() == part.bytes && hash_file(&destination)? == part.sha256,
            "Output relocation verification failed"
        );
    }
    sync_directory(&manifest.destination)?;
    environment.checkpoint("after_copy")?;
    let mut connection = store.connection()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: String = tx.query_row(
        "SELECT physical FROM output_locations WHERE id=?1",
        [&manifest.id],
        |row| row.get(0),
    )?;
    ensure!(
        Path::new(&current) == manifest.source || Path::new(&current) == manifest.destination,
        "Output location changed during relocation"
    );
    if Path::new(&current) == manifest.source {
        ensure!(tx.execute("UPDATE output_locations SET physical=?2,archived=1,generation=generation+1,archive_spec=?4 WHERE id=?1 AND physical=?3",params![manifest.id,manifest.destination.to_str(),manifest.source.to_str(),manifest.archive_spec.as_ref().map(serde_json::to_string).transpose()?])? == 1,"Relocation lost location ownership");
    }
    ensure!(
        tx.execute(
            "UPDATE relocations SET stage='published' WHERE id=?1",
            [&manifest.id]
        )? == 1,
        "Relocation journal disappeared"
    );
    tx.commit()?;
    environment.checkpoint("after_publish")?;
    publish_alias(
        &store.root().join("outputs").join(&manifest.id),
        Some(&manifest.source),
        &manifest.destination,
    )?;
    environment.checkpoint("after_alias")?;
    for part in &manifest.files {
        let source = manifest.source.join(&part.name);
        if !source.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&source)?;
        ensure!(
            metadata.is_file()
                && metadata.len() == part.bytes
                && hash_file(&source)? == part.sha256,
            "Relocation source changed before cleanup; preserving it"
        );
        source_binding
            .as_ref()
            .context("Relocation source identity is unavailable")?
            .remove_part(&part.name)?;
    }
    if manifest.source.exists() {
        source_binding
            .as_ref()
            .context("Relocation source identity is unavailable")?
            .verify()?;
        std::fs::remove_dir(&manifest.source)?;
    }
    environment.checkpoint("after_delete")?;
    store.connection()?.execute(
        "UPDATE relocations SET stage='complete' WHERE id=?1",
        [&manifest.id],
    )?;
    Ok(())
}

impl ExecutionStore {
    /// Routine policy over the same placement owner used by emergency capture.
    /// A false result means the output is no longer cold/eligible or is owned.
    pub fn archive_cold_output(&self, id: &str, config: &StorageConfig, now: i64) -> Result<bool> {
        self.archive_cold_output_with_environment(id, config, now, Arc::new(NativeEnvironment))
    }

    fn archive_cold_output_with_environment(
        &self,
        id: &str,
        config: &StorageConfig,
        now: i64,
        environment: Arc<dyn StorageEnvironment>,
    ) -> Result<bool> {
        let Some(lease) = try_output_lease(self, id)? else {
            return Ok(false);
        };
        let record = self.inspect(id)?.context("Unknown cold output")?;
        if !record.state.terminal()
            || record.result_path.is_none()
            || !self.session_is_cold(&record.session_id, now)?
        {
            return Ok(false);
        }
        let Some((physical, archived)) = self.output_location(id)? else {
            return Ok(false);
        };
        let pending: Option<String> = self
            .connection()?
            .query_row(
                "SELECT manifest_path FROM relocations WHERE id=?1 AND stage<>'complete'",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(path) = pending {
            let manifest: MoveManifest = crate::storage::read_json(Path::new(&path))?;
            ensure!(manifest.id == id, "Cold relocation identity mismatch");
            let recorded_config = StorageConfig {
                archive: manifest.archive_spec.clone(),
                ..config.clone()
            };
            let root = archive_namespace(self, &recorded_config, environment.as_ref())?;
            ensure!(
                manifest.destination.parent() == Some(root.path.as_path()),
                "Cold relocation archive changed"
            );
            complete_move(self, &manifest, environment.as_ref())?;
        } else if archived {
            // Emergency spillover becomes cold only through the same idle rule.
            self.connection()?.execute("UPDATE output_locations SET cold_archived_at=COALESCE(cold_archived_at,?2) WHERE id=?1", params![id,now])?;
            return Ok(false);
        } else {
            ensure!(
                physical == self.root().join("data").join(id),
                "Cold output is outside its owned local bundle"
            );
            let binding = DirectoryBinding::open(&physical)?;
            let mut bundle = BundleStorage {
                store: self.clone(),
                id: id.into(),
                physical,
                archived: false,
                config: config.clone(),
                environment,
                _lease: lease,
                binding,
            };
            bundle.spill()?;
        }
        self.connection()?.execute("UPDATE output_locations SET cold_archived_at=COALESCE(cold_archived_at,?2) WHERE id=?1 AND archived=1", params![id,now])?;
        Ok(true)
    }

    pub(super) fn open_output_file(&self, id: &str) -> Result<File> {
        self.open_output_part(id, "output.txt")
    }

    pub(super) fn open_output_part(&self, id: &str, name: &str) -> Result<File> {
        validate_part(name)?;
        self.ensure_output_not_deleted(id)?;
        self.open_available_output_part(id, name)
    }

    pub(super) fn ensure_output_not_deleted(&self, id: &str) -> Result<()> {
        let deletion: Option<String> = self
            .connection()?
            .query_row(
                "SELECT stage FROM output_deletions WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            deletion.is_none(),
            "Output backing data was deliberately selected for deletion by confirmed human cleanup ({}); prior transcript content remains unchanged",
            deletion.unwrap_or_default()
        );
        Ok(())
    }

    fn open_available_output_part(&self, id: &str, name: &str) -> Result<File> {
        for _ in 0..2 {
            let (physical,archived,spec,generation):(String,bool,Option<String>,i64)=self.connection()?.query_row("SELECT physical,archived,archive_spec,generation FROM output_locations WHERE id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
            let physical = PathBuf::from(physical);
            let opened = (|| {
                if archived {
                    let spec: ArchiveConfig = serde_json::from_str(
                        spec.as_deref()
                            .context("Archived output has no recorded volume identity")?,
                    )?;
                    let root = resolve_archive(&spec, false)?;
                    ensure!(
                        physical.starts_with(&root.path),
                        "Archived output is outside its recorded archive root"
                    );
                } else {
                    ensure!(
                        physical == self.root().join("data").join(id),
                        "Local output location is outside its owned bundle"
                    );
                }
                let binding = DirectoryBinding::open(&physical)?;
                let identity: serde_json::Value =
                    serde_json::from_reader(binding.read_part("identity.json")?.take(16 * 1024))?;
                ensure!(
                    identity["schema"] == 1 && identity["invocation_id"] == id,
                    "Managed output identity changed"
                );
                let alias = self.root().join("outputs").join(id);
                ensure!(
                    std::fs::read_link(&alias)? == physical,
                    "Managed output alias and location disagree"
                );
                binding.read_part(name)
            })();
            if opened.is_ok() {
                return opened;
            }
            let current: i64 = self.connection()?.query_row(
                "SELECT generation FROM output_locations WHERE id=?1",
                [id],
                |row| row.get(0),
            )?;
            if current == generation {
                return opened;
            }
        }
        bail!("Output location changed while opening it; retry the read, not its producer")
    }
    /// Converge recorded publication/move operations without running a producer.
    pub fn recover_output_storage(&self, config: &StorageConfig) -> Result<usize> {
        self.recover_output_storage_with_environment(config, &NativeEnvironment)
    }

    fn recover_output_storage_with_environment(
        &self,
        config: &StorageConfig,
        environment: &dyn StorageEnvironment,
    ) -> Result<usize> {
        let connection = self.connection()?;
        let mut query = connection.prepare(
            "SELECT id,physical,archived FROM output_allocations WHERE stage='prepared'",
        )?;
        let allocations = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut recovered = 0;
        for (id, path, archived) in allocations {
            let _lease = output_lease(self, &id)?;
            let record = self.inspect(&id)?.context("Allocation has no invocation")?;
            let parent = if archived {
                archive_namespace(self, config, environment)?
            } else {
                let local = self.root().join("data");
                private_directory(&local)?;
                DirectoryBinding::open(&local)?
            };
            ensure!(
                Path::new(&path) == parent.path.join(&id),
                "Allocation is outside its recorded storage namespace"
            );
            complete_allocation(self, &record, &parent, archived, environment)?;
            recovered += 1;
        }
        let mut query = connection
            .prepare("SELECT id,manifest_path FROM relocations WHERE stage<>'complete'")?;
        let moves = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, path) in moves {
            let _lease = output_lease(self, &id)?;
            let manifest: MoveManifest = crate::storage::read_json(Path::new(&path))?;
            ensure!(manifest.id == id, "Relocation receipt identity mismatch");
            let root = archive_namespace(self, config, environment)?;
            ensure!(
                manifest.destination.parent() == Some(root.path.as_path()),
                "Relocation destination does not match verified archive"
            );
            complete_move(self, &manifest, environment)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    pub(super) fn output_location(&self, id: &str) -> Result<Option<(PathBuf, bool)>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT physical,archived FROM output_locations WHERE id=?1",
                [id],
                |row| Ok((PathBuf::from(row.get::<_, String>(0)?), row.get(1)?)),
            )
            .optional()?)
    }

    #[cfg(unix)]
    pub(super) fn recover_owned_storage(&self, id: &str, lease: &OutputLease) -> Result<()> {
        lease.validate(self, id)?;
        let connection = self.connection()?;
        let allocation:Option<(String,bool,Option<String>)>=connection.query_row("SELECT physical,archived,archive_spec FROM output_allocations WHERE id=?1 AND stage='prepared'",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        if let Some((path, archived, spec)) = allocation {
            let record = self.inspect(id)?.context("Allocation has no invocation")?;
            let parent = if archived {
                let archive =
                    spec.context("Interrupted archive allocation has no recorded volume identity")?;
                let config = StorageConfig {
                    archive: Some(serde_json::from_str(&archive)?),
                    ..Default::default()
                };
                archive_namespace(self, &config, &NativeEnvironment)?
            } else {
                let local = self.root().join("data");
                private_directory(&local)?;
                DirectoryBinding::open(&local)?
            };
            ensure!(
                Path::new(&path) == parent.path.join(id),
                "Allocation differs from its recorded namespace"
            );
            complete_allocation(self, &record, &parent, archived, &NativeEnvironment)?;
        }
        let pending: Option<String> = connection
            .query_row(
                "SELECT manifest_path FROM relocations WHERE id=?1 AND stage<>'complete'",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(path) = pending {
            let manifest: MoveManifest = crate::storage::read_json(Path::new(&path))?;
            ensure!(manifest.id == id, "Relocation identity mismatch");
            let config = StorageConfig {
                archive: Some(
                    manifest
                        .archive_spec
                        .clone()
                        .context("Interrupted relocation has no recorded volume identity")?,
                ),
                ..Default::default()
            };
            let root = archive_namespace(self, &config, &NativeEnvironment)?;
            ensure!(
                manifest.destination.parent() == Some(root.path.as_path()),
                "Relocation differs from its verified archive"
            );
            complete_move(self, &manifest, &NativeEnvironment)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
