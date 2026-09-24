//! Verified descriptor-relative native text mutations. This is not an OS sandbox.
use anyhow::{Context, Result, ensure};
use jcode_tool_core::native_files::{NativeFilePermit, NativeFilePlan};
use std::path::{Path, PathBuf};

/// Resolve actual filesystem aliases before evaluating scope. Missing suffixes are
/// appended only to a successfully canonicalized existing ancestor.
pub fn resolve_target(path: &Path) -> Result<PathBuf> {
    resolve_inner(path, &mut std::collections::BTreeSet::new())
}
fn resolve_inner(path: &Path, links: &mut std::collections::BTreeSet<PathBuf>) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Native mutation requires an absolute bound path"
    );
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .context("Resolve traversal through missing directories before mutation")?
                        .to_os_string(),
                );
                ancestor = ancestor
                    .parent()
                    .context("Mutation has no existing ancestor")?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let mut resolved = if std::fs::symlink_metadata(ancestor)?
        .file_type()
        .is_symlink()
    {
        ensure!(
            links.insert(ancestor.to_path_buf()),
            "Mutation target has a symlink loop"
        );
        let target = std::fs::read_link(ancestor)?;
        let target = if target.is_absolute() {
            target
        } else {
            ancestor
                .parent()
                .context("Symlink has no parent")?
                .join(target)
        };
        resolve_inner(&target, links)?
    } else {
        ancestor
            .canonicalize()
            .with_context(|| format!("Resolve mutation target {}", path.display()))?
    };
    for part in missing.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// The entry unlinked by remove_file, unlike the referent read or written through
/// a leaf symlink. Ancestor aliases still resolve to their real directory.
pub fn resolve_removal_entry(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Removal requires an absolute bound path"
    );
    let parent = path.parent().context("Removal has no parent")?;
    let name = path
        .file_name()
        .context("Removal requires an explicit file entry")?;
    Ok(resolve_inner(parent, &mut std::collections::BTreeSet::new())?.join(name))
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{CString, OsStr};
    use std::fs::{File, Metadata, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    };
    use std::time::SystemTime;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
        birth: SystemTime,
    }
    impl Identity {
        fn of(meta: &Metadata) -> Result<Self> {
            Ok(Self {
                device: meta.dev(),
                inode: meta.ino(),
                birth: meta.created()?,
            })
        }
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Witness {
        identity: Identity,
        length: u64,
        modified: SystemTime,
    }
    impl Witness {
        fn of(meta: &Metadata) -> Result<Self> {
            ensure!(
                meta.is_file(),
                "Native file mutation requires a regular file"
            );
            ensure!(
                meta.nlink() == 1,
                "Native mutation refuses a multiply-linked file; resolve its other aliases before writing"
            );
            Ok(Self {
                identity: Identity::of(meta)?,
                length: meta.len(),
                modified: meta.modified()?,
            })
        }
    }
    pub struct Directory {
        path: PathBuf,
        file: File,
        identity: Identity,
        ancestors: Vec<(PathBuf, File, Identity)>,
    }
    impl Directory {
        pub fn verify_volume(&self, path: &Path) -> Result<()> {
            let metadata = std::fs::symlink_metadata(path)?;
            ensure!(
                metadata.dev() == self.identity.device,
                "A nested volume or retargeted ancestor at {} requires explicit location selection",
                path.display()
            );
            Ok(())
        }
        pub fn open(path: PathBuf) -> Result<Self> {
            let before = std::fs::symlink_metadata(&path)?;
            ensure!(
                before.is_dir(),
                "Mutation ancestor is not a real directory: {}",
                path.display()
            );
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            let identity = Identity::of(&before)?;
            ensure!(
                Identity::of(&file.metadata()?)? == identity,
                "Mutation ancestor changed while opening"
            );
            let result = Self {
                path,
                file,
                identity,
                ancestors: Vec::new(),
            };
            result.verify()?;
            Ok(result)
        }
        pub fn verify(&self) -> Result<()> {
            for (path, file, identity) in &self.ancestors {
                let metadata = std::fs::symlink_metadata(path)?;
                ensure!(
                    metadata.is_dir()
                        && Identity::of(&metadata)? == *identity
                        && Identity::of(&file.metadata()?)? == *identity,
                    "Preservation ancestor changed: {}",
                    path.display()
                );
            }
            let current = std::fs::symlink_metadata(&self.path)?;
            ensure!(
                current.is_dir()
                    && Identity::of(&current)? == self.identity
                    && Identity::of(&self.file.metadata()?)? == self.identity,
                "Mutation ancestor changed: {}",
                self.path.display()
            );
            Ok(())
        }
        fn child(&self, name: &OsStr, create: bool) -> Result<(Self, bool)> {
            self.child_with_mode(name, create, 0o777)
        }
        fn child_with_mode(
            &self,
            name: &OsStr,
            create: bool,
            mode: libc::mode_t,
        ) -> Result<(Self, bool)> {
            self.verify()?;
            let name = component(name)?;
            let mut created = false;
            // SAFETY: parent is a retained directory descriptor and name is one NUL-free component.
            let mut fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0
                && create
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                // SAFETY: same retained descriptor/component, default directory mode respects umask.
                if unsafe { libc::mkdirat(self.file.as_raw_fd(), name.as_ptr(), mode) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error.into());
                    }
                } else {
                    created = true;
                    self.file.sync_all()?;
                }
                // SAFETY: the new or competing component is opened without following links.
                fd = unsafe {
                    libc::openat(
                        self.file.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
            ensure!(
                fd >= 0,
                "Open mutation ancestor: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: successful openat returned a fresh descriptor owned by this value.
            let file = unsafe { File::from_raw_fd(fd) };
            let path = self.path.join(OsStr::from_bytes(name.as_bytes()));
            let identity = Identity::of(&file.metadata()?)?;
            let value = Self {
                path,
                file,
                identity,
                ancestors: Vec::new(),
            };
            value.verify()?;
            Ok((value, created))
        }

        /// Traverse only real directories on this filesystem. Creation is
        /// relative to retained descriptors, never recursive path-based mkdir.
        pub fn preservation_directory(&self, relative: &Path, create: bool) -> Result<Self> {
            self.verify()?;
            let ancestors = self
                .ancestors
                .iter()
                .map(|(path, file, identity)| {
                    Ok((path.clone(), file.try_clone()?, identity.clone()))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut current = Self {
                path: self.path.clone(),
                file: self.file.try_clone()?,
                identity: self.identity.clone(),
                ancestors,
            };
            for part in relative.components() {
                let std::path::Component::Normal(name) = part else {
                    anyhow::bail!("Preservation path must be relative without traversal");
                };
                let (mut next, _) = current.child_with_mode(name, create, 0o700)?;
                ensure!(
                    next.identity.device == self.identity.device,
                    "Preservation crosses a nested filesystem"
                );
                next.ancestors = current.ancestors;
                next.ancestors
                    .push((current.path, current.file, current.identity));
                current = next;
            }
            self.verify()?;
            Ok(current)
        }

        pub fn create_preserved_file(&self, name: &OsStr) -> Result<File> {
            self.verify()?;
            let name = component(name)?;
            // SAFETY: one validated component relative to a retained directory.
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            ensure!(
                fd >= 0,
                "Create preserved file: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: openat returned a fresh owned descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            self.file.sync_all()?;
            self.verify()?;
            Ok(file)
        }

        /// Capture an entry into an owned holding directory without replacing
        /// anything. Callers verify the captured entry before any unlink.
        #[cfg(target_os = "macos")]
        pub fn capture_entry(&self, name: &OsStr, holding: &Self, captured: &OsStr) -> Result<()> {
            self.verify()?;
            holding.verify()?;
            ensure!(
                self.identity.device == holding.identity.device,
                "Capture crosses filesystems"
            );
            let name = component(name)?;
            let captured = component(captured)?;
            // SAFETY: both directories remain owned; both names are single
            // NUL-free components. RENAME_EXCL never replaces a destination.
            if unsafe {
                libc::renameatx_np(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    holding.file.as_raw_fd(),
                    captured.as_ptr(),
                    libc::RENAME_EXCL,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.file.sync_all()?;
            holding.file.sync_all()?;
            self.verify()?;
            holding.verify()
        }

        /// Remove only the verified entry in an owned holding directory. A
        /// nonempty directory is rejected by the kernel, never traversed here.
        #[cfg(target_os = "macos")]
        pub fn unlink_captured_entry(&self, name: &OsStr, expected: &Metadata) -> Result<()> {
            self.verify()?;
            let entry = self.open_preserved_entry(name, expected.file_type().is_symlink())?;
            let current = entry.metadata()?;
            ensure!(
                Identity::of(&current)? == Identity::of(expected)?
                    && current.mode() == expected.mode()
                    && current.len() == expected.len()
                    && current.modified()? == expected.modified()?
                    && current.ctime() == expected.ctime()
                    && current.ctime_nsec() == expected.ctime_nsec(),
                "Captured entry changed before removal"
            );
            ensure!(
                current.dev() == self.identity.device,
                "Removal crosses a mounted filesystem"
            );
            let name = component(name)?;
            let flags = if current.is_dir() {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            // SAFETY: verified descriptor and one component. AT_REMOVEDIR only
            // accepts an empty directory; ordinary unlink never follows links.
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), flags) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            self.file.sync_all()?;
            self.verify()
        }

        pub fn open_preserved_entry(&self, name: &OsStr, symlink: bool) -> Result<File> {
            self.verify()?;
            let name = component(name)?;
            let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
            #[cfg(target_os = "macos")]
            if symlink {
                flags = (flags & !libc::O_NOFOLLOW) | libc::O_SYMLINK;
            }
            #[cfg(not(target_os = "macos"))]
            ensure!(
                !symlink,
                "Symlink metadata handles require the native macOS adapter"
            );
            // SAFETY: retained parent, validated component and read-only flags.
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
            ensure!(
                fd >= 0,
                "Open preserved entry: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: openat returned a fresh owned descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            ensure!(
                !symlink || file.metadata()?.file_type().is_symlink(),
                "Preserved symlink changed type before metadata access"
            );
            self.verify()?;
            Ok(file)
        }

        pub fn create_preserved_symlink(&self, name: &OsStr, target: &Path) -> Result<()> {
            self.verify()?;
            let name = component(name)?;
            let target = CString::new(target.as_os_str().as_bytes())?;
            // SAFETY: target is inert NUL-terminated link text, not traversed.
            ensure!(
                unsafe { libc::symlinkat(target.as_ptr(), self.file.as_raw_fd(), name.as_ptr()) }
                    == 0,
                "Create preserved symlink: {}",
                std::io::Error::last_os_error()
            );
            self.file.sync_all()?;
            self.verify()
        }

        pub fn link_preserved_file(
            &self,
            name: &OsStr,
            source: &Self,
            source_name: &OsStr,
        ) -> Result<()> {
            self.verify()?;
            source.verify()?;
            let name = component(name)?;
            let source_name = component(source_name)?;
            // SAFETY: both directories and component strings remain owned.
            ensure!(
                unsafe {
                    libc::linkat(
                        source.file.as_raw_fd(),
                        source_name.as_ptr(),
                        self.file.as_raw_fd(),
                        name.as_ptr(),
                        0,
                    )
                } == 0,
                "Link preserved file: {}",
                std::io::Error::last_os_error()
            );
            self.file.sync_all()?;
            source.verify()?;
            self.verify()
        }

        /// Used for native metadata copy and child fchdir, without reopening a
        /// possibly replaced pathname. The descriptor stays owned by this guard.
        pub fn preservation_handle(&self) -> Result<&File> {
            self.verify()?;
            Ok(&self.file)
        }

        #[cfg(target_os = "macos")]
        pub fn volume_ownership_enforced(&self) -> Result<Option<bool>> {
            self.verify()?;
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: fstatfs initializes the supplied structure on success;
            // the descriptor remains owned and this observation changes nothing.
            ensure!(
                unsafe { libc::fstatfs(self.file.as_raw_fd(), filesystem.as_mut_ptr()) } == 0,
                "Inspect filesystem ownership: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: the successful call above initialized the entire value.
            let filesystem = unsafe { filesystem.assume_init() };
            self.verify()?;
            Ok(Some(
                u64::from(filesystem.f_flags) & (libc::MNT_IGNORE_OWNERSHIP as u64) == 0,
            ))
        }
        #[cfg(not(target_os = "macos"))]
        pub fn volume_ownership_enforced(&self) -> Result<Option<bool>> {
            Ok(None)
        }

        #[cfg(target_os = "macos")]
        pub fn copy_preserved_metadata(&mut self, source: &File) -> Result<()> {
            self.verify()?;
            // SAFETY: both descriptors remain owned. COPYFILE_METADATA changes
            // attributes, including birth time, but cannot replace this inode.
            ensure!(
                unsafe {
                    libc::fcopyfile(
                        source.as_raw_fd(),
                        self.file.as_raw_fd(),
                        std::ptr::null_mut(),
                        libc::COPYFILE_METADATA,
                    )
                } == 0,
                "Copy directory metadata: {}",
                std::io::Error::last_os_error()
            );
            let after = Identity::of(&self.file.metadata()?)?;
            ensure!(
                after.device == self.identity.device && after.inode == self.identity.inode,
                "Directory identity changed during metadata copy"
            );
            self.identity = after;
            self.verify()?;
            self.file.sync_all()?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        pub fn copy_preserved_metadata(&mut self, _: &File) -> Result<()> {
            anyhow::bail!("Complete preservation metadata requires macOS")
        }
    }
    struct Target {
        path: PathBuf,
        directories: Vec<Directory>,
        witness: Option<Witness>,
    }
    impl Target {
        fn prepare(path: PathBuf) -> Result<Self> {
            let witness = witness(&path)?;
            let mut ancestor = path.parent().context("Mutation has no parent")?;
            while !ancestor.try_exists()? {
                ancestor = ancestor.parent().context("Missing mutation ancestor")?;
            }
            let directory = Directory::open(ancestor.to_path_buf())?;
            Ok(Self {
                path,
                directories: vec![directory],
                witness,
            })
        }
        fn verify(&self) -> Result<()> {
            for directory in &self.directories {
                directory.verify()?;
            }
            ensure!(
                witness(&self.path)? == self.witness,
                "Mutation target changed since admission/read: {}",
                self.path.display()
            );
            Ok(())
        }
        fn parent(&mut self, create: bool, created: &mut Vec<PathBuf>) -> Result<&Directory> {
            self.verify()?;
            let parent = self.path.parent().context("Mutation has no parent")?;
            loop {
                let last = self.directories.last().context("Missing pinned ancestor")?;
                let suffix = parent.strip_prefix(&last.path)?;
                let Some(next) = suffix.components().next() else {
                    break;
                };
                let (child, was_created) = last.child(next.as_os_str(), create)?;
                if was_created {
                    created.push(child.path.clone());
                }
                self.directories.push(child);
            }
            self.directories.last().context("Missing pinned parent")
        }
        fn open(&mut self, write: bool, created: &mut Vec<PathBuf>) -> Result<File> {
            let expected = self.witness.clone();
            let leaf = component(self.path.file_name().context("Missing file name")?)?;
            let parent = self.parent(write, created)?;
            let flags = libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | if write {
                    libc::O_WRONLY
                } else {
                    libc::O_RDONLY
                }
                | if write && expected.is_none() {
                    libc::O_CREAT | libc::O_EXCL
                } else {
                    0
                };
            // SAFETY: descriptor is held, component is validated, and flags never truncate before verification.
            let fd = unsafe { libc::openat(parent.file.as_raw_fd(), leaf.as_ptr(), flags, 0o666) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: this value exclusively owns the descriptor returned by openat.
            let file = unsafe { File::from_raw_fd(fd) };
            let actual = Witness::of(&file.metadata()?)?;
            if let Some(expected) = expected {
                ensure!(actual == expected, "Opened file differs from admitted file");
            }
            ensure!(
                witness(&self.path)? == Some(actual.clone()),
                "File path changed while opening"
            );
            self.witness = Some(actual);
            Ok(file)
        }
    }
    fn component(name: &OsStr) -> Result<CString> {
        let bytes = name.as_bytes();
        ensure!(
            !bytes.is_empty() && bytes != b"." && bytes != b".." && !bytes.contains(&b'/'),
            "Invalid native file component"
        );
        Ok(CString::new(bytes)?)
    }
    fn witness(path: &Path) -> Result<Option<Witness>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(Witness::of(&meta)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    struct LinkEntry {
        path: PathBuf,
        parent: Directory,
        identity: Identity,
        target: PathBuf,
    }
    impl LinkEntry {
        fn open(input: &Path) -> Result<Self> {
            let path = resolve_removal_entry(input)?;
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_type().is_symlink(),
                "Removal link changed during admission"
            );
            let value = Self {
                parent: Directory::open(path.parent().context("Missing removal parent")?.into())?,
                identity: Identity::of(&metadata)?,
                target: std::fs::read_link(&path)?,
                path,
            };
            value.verify(input)?;
            Ok(value)
        }
        fn verify(&self, input: &Path) -> Result<()> {
            self.parent.verify()?;
            let metadata = std::fs::symlink_metadata(&self.path)?;
            ensure!(
                resolve_removal_entry(input)? == self.path
                    && metadata.file_type().is_symlink()
                    && Identity::of(&metadata)? == self.identity
                    && std::fs::read_link(&self.path)? == self.target,
                "Symlink entry changed during native mutation"
            );
            Ok(())
        }
        fn unlink(&self) -> Result<()> {
            let leaf = component(self.path.file_name().context("Missing symlink entry")?)?;
            // SAFETY: parent and link entry were pinned and verified. This unlinks
            // the link itself, never the followed data-file descriptor.
            if unsafe { libc::unlinkat(self.parent.file.as_raw_fd(), leaf.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            self.parent.file.sync_all()?;
            Ok(())
        }
    }
    pub struct VerifiedFiles {
        aliases: BTreeMap<PathBuf, PathBuf>,
        targets: BTreeMap<PathBuf, Target>,
        created_directories: Vec<PathBuf>,
        _file_leases: Vec<File>,
        removals: BTreeSet<PathBuf>,
        links: BTreeMap<PathBuf, LinkEntry>,
        directory_references: BTreeMap<PathBuf, Directory>,
    }
    impl VerifiedFiles {
        /// Expected resolved targets come from the authorization owner. Resolution
        /// is repeated here so a retarget between policy and pinning cannot rebind it.
        pub fn acquire(paths: &[(PathBuf, PathBuf)]) -> Result<Self> {
            Self::acquire_with_removals(
                paths,
                &paths
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>(),
            )
        }
        pub fn acquire_with_removals(
            paths: &[(PathBuf, PathBuf)],
            removals: &[PathBuf],
        ) -> Result<Self> {
            let plan = NativeFilePlan::new(
                paths.iter().map(|(p, _)| p.clone()).collect(),
                removals.to_vec(),
            );
            Self::acquire_plan(paths, &plan)
        }
        pub fn acquire_plan(paths: &[(PathBuf, PathBuf)], plan: &NativeFilePlan) -> Result<Self> {
            let removals = plan.removals();
            ensure!(
                paths.iter().map(|(p, _)| p).collect::<BTreeSet<_>>()
                    == plan.paths().iter().collect(),
                "Native plan and admitted targets differ"
            );
            let mut value = Self {
                aliases: BTreeMap::new(),
                targets: BTreeMap::new(),
                created_directories: Vec::new(),
                _file_leases: Vec::new(),
                removals: removals.iter().cloned().collect(),
                links: BTreeMap::new(),
                directory_references: BTreeMap::new(),
            };
            for (input, resolved) in paths {
                ensure!(
                    resolve_target(input)? == *resolved,
                    "Mutation target changed during admission"
                );
                let link =
                    std::fs::symlink_metadata(input).is_ok_and(|m| m.file_type().is_symlink());
                if link
                    && !plan.requires_file(input)
                    && removals.contains(input)
                    && resolved.is_dir()
                {
                    value
                        .directory_references
                        .insert(resolved.clone(), Directory::open(resolved.clone())?);
                } else if !value.targets.contains_key(resolved) {
                    value
                        .targets
                        .insert(resolved.clone(), Target::prepare(resolved.clone())?);
                }
                value.aliases.insert(input.clone(), resolved.clone());
                if std::fs::symlink_metadata(input)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    value.links.insert(input.clone(), LinkEntry::open(input)?);
                }
            }
            let mut lock_paths: BTreeSet<_> = value.targets.keys().cloned().collect();
            lock_paths.extend(value.directory_references.keys().cloned());
            for removal in removals {
                ensure!(
                    value.aliases.contains_key(removal),
                    "Removal was not an admitted destination"
                );
                lock_paths.insert(resolve_removal_entry(removal)?);
            }
            for path in lock_paths {
                value._file_leases.push(file_lease(&path)?);
            }
            value.verify_all()?;
            Ok(value)
        }
        fn key(&self, path: &Path) -> Result<PathBuf> {
            self.aliases
                .get(path)
                .cloned()
                .context("File was not included in native mutation admission")
        }
        pub fn verify_all(&self) -> Result<()> {
            for (input, link) in &self.links {
                link.verify(input)?;
            }
            for (input, resolved) in &self.aliases {
                ensure!(
                    resolve_target(input)? == *resolved,
                    "Native mutation alias was retargeted: {}",
                    input.display()
                );
            }
            for directory in self.directory_references.values() {
                directory.verify()?;
            }
            for target in self.targets.values() {
                target.verify()?;
            }
            Ok(())
        }
    }
    impl NativeFilePermit for VerifiedFiles {
        fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
            self.verify_all()?;
            let key = self.key(path)?;
            let target = self
                .targets
                .get_mut(&key)
                .context("Missing admitted file")?;
            if target.witness.is_none() {
                return Ok(None);
            }
            let mut file = target.open(false, &mut self.created_directories)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            ensure!(
                Some(Witness::of(&file.metadata()?)?) == target.witness,
                "File changed while reading"
            );
            self.verify_all()?;
            Ok(Some(bytes))
        }
        fn write(&mut self, path: &Path, contents: &[u8]) -> Result<()> {
            self.verify_all()?;
            let key = self.key(path)?;
            let target = self
                .targets
                .get_mut(&key)
                .context("Missing admitted file")?;
            let result = (|| -> Result<()> {
                let mut file = target.open(true, &mut self.created_directories)?;
                // Keep the original inode, permissions, ACLs and extended attributes.
                file.set_len(0)?;
                let result = file.write_all(contents).and_then(|_| file.sync_all());
                target.witness = Some(Witness::of(&file.metadata()?)?);
                result?;
                target
                    .directories
                    .last()
                    .context("Missing parent")?
                    .file
                    .sync_all()?;
                Ok(())
            })();
            result.with_context(|| {
                format!(
                    "Native write {}; created parents: {:?}. Prior effects are not rolled back",
                    path.display(),
                    self.created_directories
                )
            })
        }
        fn remove(&mut self, path: &Path) -> Result<()> {
            self.verify_all()?;
            ensure!(
                self.removals.contains(path),
                "Removal was not included in native admission"
            );
            if let Some(link) = self.links.get(path) {
                let identity = link.identity.clone();
                link.unlink()?;
                let affected = self
                    .links
                    .iter()
                    .filter(|(_, link)| link.identity == identity)
                    .map(|(input, link)| (input.clone(), link.path.clone()))
                    .collect::<Vec<_>>();
                for (input, entry) in affected {
                    self.links.remove(&input);
                    self.aliases.insert(input, entry.clone());
                    self.targets.insert(entry.clone(), Target::prepare(entry)?);
                }
                return Ok(());
            }
            let key = self.key(path)?;
            let target = self
                .targets
                .get_mut(&key)
                .context("Missing admitted file")?;
            ensure!(target.witness.is_some(), "File does not exist");
            let leaf = component(target.path.file_name().context("Missing leaf")?)?;
            let parent = target.parent(false, &mut self.created_directories)?;
            // SAFETY: both parent and exact leaf have been verified; unlinkat does not follow a leaf link.
            if unsafe { libc::unlinkat(parent.file.as_raw_fd(), leaf.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            parent.file.sync_all()?;
            target.witness = None;
            Ok(())
        }
        fn same_file(&self, first: &Path, second: &Path) -> Result<bool> {
            let first = self.key(first)?;
            let second = self.key(second)?;
            if first == second {
                return Ok(true);
            }
            let a = self.targets.get(&first).and_then(|t| t.witness.as_ref());
            let b = self.targets.get(&second).and_then(|t| t.witness.as_ref());
            Ok(matches!((a,b), (Some(a), Some(b)) if a.identity == b.identity))
        }
        fn copy_metadata(&mut self, source: &Path, destination: &Path) -> Result<()> {
            self.verify_all()?;
            if self.same_file(source, destination)? {
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            {
                let key = self.key(source)?;
                let source = self
                    .targets
                    .get_mut(&key)
                    .context("Missing source")?
                    .open(false, &mut self.created_directories)?;
                let key = self.key(destination)?;
                let target = self.targets.get_mut(&key).context("Missing destination")?;
                let destination = target.open(true, &mut self.created_directories)?;
                // SAFETY: verified regular file descriptors remain owned across fcopyfile.
                if unsafe {
                    libc::fcopyfile(
                        source.as_raw_fd(),
                        destination.as_raw_fd(),
                        std::ptr::null_mut(),
                        libc::COPYFILE_METADATA,
                    )
                } != 0
                {
                    return Err(std::io::Error::last_os_error()).context("Destination bytes were written but metadata copy failed; source was retained");
                }
                destination.sync_all()?;
                target.witness = Some(Witness::of(&destination.metadata()?)?);
                Ok(())
            }
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!(
                "Complete native move metadata preservation is unsupported on this platform; source was retained"
            )
        }
    }

    fn file_lease(path: &Path) -> Result<File> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::DirBuilderExt;
        // SAFETY: geteuid takes no pointers and only observes process identity.
        let uid = unsafe { libc::geteuid() };
        let root = PathBuf::from(format!("/tmp/jcode-native-file-leases-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&root)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
            "Native file lease directory has a foreign identity or permissions"
        );
        let directory = Directory::open(root.canonicalize()?)?;
        let name = CString::new(format!("{:x}", Sha256::digest(path.as_os_str().as_bytes())))?;
        // SAFETY: pinned owner-only directory, private regular leaf with no-follow.
        let fd = unsafe {
            libc::openat(
                directory.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: owns the fresh openat descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == uid
                && metadata.mode() & 0o077 == 0,
            "Invalid native file lease"
        );
        file.try_lock()
            .context("Native mutation of this exact file is already in flight")?;
        Ok(file)
    }
}
#[cfg(unix)]
pub use platform::{Directory as VerifiedDirectory, VerifiedFiles};

#[cfg(not(unix))]
pub struct VerifiedFiles;
#[cfg(not(unix))]
pub struct VerifiedDirectory;
#[cfg(not(unix))]
impl VerifiedDirectory {
    pub fn volume_ownership_enforced(&self) -> Result<Option<bool>> {
        Ok(None)
    }
    pub fn copy_preserved_metadata(&mut self, _: &std::fs::File) -> Result<()> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn preservation_directory(&self, _: &Path, _: bool) -> Result<Self> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn create_preserved_file(&self, _: &std::ffi::OsStr) -> Result<std::fs::File> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn open_preserved_entry(&self, _: &std::ffi::OsStr, _: bool) -> Result<std::fs::File> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn create_preserved_symlink(&self, _: &std::ffi::OsStr, _: &Path) -> Result<()> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn link_preserved_file(
        &self,
        _: &std::ffi::OsStr,
        _: &Self,
        _: &std::ffi::OsStr,
    ) -> Result<()> {
        anyhow::bail!("Native preservation unsupported")
    }
    pub fn preservation_handle(&self) -> Result<&std::fs::File> {
        anyhow::bail!("Native preservation unsupported")
    }

    pub fn verify_volume(&self, _: &Path) -> Result<()> {
        anyhow::bail!("Verified directory handles are unsupported on this platform")
    }
    pub fn open(_: PathBuf) -> Result<Self> {
        anyhow::bail!("Verified directory handles are unsupported on this platform")
    }
    pub fn verify(&self) -> Result<()> {
        anyhow::bail!("Verified directory handles are unsupported on this platform")
    }
}
#[cfg(not(unix))]
impl VerifiedFiles {
    pub fn acquire_plan(_: &[(PathBuf, PathBuf)], _: &NativeFilePlan) -> Result<Self> {
        anyhow::bail!("Verified native mutations are unsupported on this platform")
    }
    pub fn acquire_with_removals(_: &[(PathBuf, PathBuf)], _: &[PathBuf]) -> Result<Self> {
        anyhow::bail!("Verified native mutations are unsupported on this platform")
    }
    pub fn acquire(_: &[(PathBuf, PathBuf)]) -> Result<Self> {
        anyhow::bail!("Verified native mutations are unsupported on this platform")
    }
}
#[cfg(not(unix))]
impl NativeFilePermit for VerifiedFiles {
    fn read(&mut self, _: &Path) -> Result<Option<Vec<u8>>> {
        anyhow::bail!("Unsupported native mutation")
    }
    fn write(&mut self, _: &Path, _: &[u8]) -> Result<()> {
        anyhow::bail!("Unsupported native mutation")
    }
    fn remove(&mut self, _: &Path) -> Result<()> {
        anyhow::bail!("Unsupported native mutation")
    }
    fn same_file(&self, _: &Path, _: &Path) -> Result<bool> {
        anyhow::bail!("Unsupported native mutation")
    }
    fn copy_metadata(&mut self, _: &Path, _: &Path) -> Result<()> {
        anyhow::bail!("Unsupported native mutation")
    }
}

include!("native_files_tests.rs");
