use super::*;
use std::fs::{File, Metadata, OpenOptions};

/// Durable root witness, paired with a volume UUID. The device number is not
/// durable across remounts. Birth time prevents ordinary inode-reuse confusion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryWitness {
    inode: u64,
    created: std::time::SystemTime,
}
impl DirectoryWitness {
    fn of(metadata: &Metadata, path: &Path) -> Result<Self> {
        if !metadata.is_dir() {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                path,
                "expected a directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                inode: metadata.ino(),
                created: metadata.created().map_err(|e| LocationError::io(path, e))?,
            })
        }
        #[cfg(not(unix))]
        Err(LocationError::new(
            LocationIssue::Unsupported,
            path,
            "verified directory witnesses are unavailable",
        ))
    }
}

struct PinnedDirectory {
    path: PathBuf,
    file: File,
    witness: DirectoryWitness,
}
impl PinnedDirectory {
    fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(|e| LocationError::io(path, e))?;
        let witness = DirectoryWitness::of(
            &file.metadata().map_err(|e| LocationError::io(path, e))?,
            path,
        )?;
        let pinned = Self {
            path: path.into(),
            file,
            witness,
        };
        pinned.verify()?;
        Ok(pinned)
    }
    fn verify(&self) -> Result<()> {
        let metadata =
            std::fs::symlink_metadata(&self.path).map_err(|e| LocationError::io(&self.path, e))?;
        if DirectoryWitness::of(&metadata, &self.path)? != self.witness {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                &self.path,
                "directory changed during resolution",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev()
                != self
                    .file
                    .metadata()
                    .map_err(|e| LocationError::io(&self.path, e))?
                    .dev()
            {
                return Err(LocationError::new(
                    LocationIssue::WrongVolume,
                    &self.path,
                    "directory moved to a different device",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalBinding {
    volume: VolumeIdentity,
    relative_path: PathBuf,
    observed_path: PathBuf,
    root_witness: DirectoryWitness,
    generation: u64,
}
impl PhysicalBinding {
    pub fn volume(&self) -> &VolumeIdentity {
        &self.volume
    }
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
    pub fn observed_path(&self) -> &Path {
        &self.observed_path
    }
    pub fn root_witness(&self) -> &DirectoryWitness {
        &self.root_witness
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// An existing witnessed ancestor plus a not-yet-existing suffix. Saving a
/// default base does not create it. A later operation must revalidate it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathBinding {
    ancestor: PhysicalBinding,
    suffix: PathBuf,
}
impl PathBinding {
    pub fn existing_ancestor(&self) -> &PhysicalBinding {
        &self.ancestor
    }
    pub fn missing_suffix(&self) -> &Path {
        &self.suffix
    }
    pub fn volume(&self) -> &VolumeIdentity {
        self.ancestor.volume()
    }
    pub fn observed_path(&self) -> PathBuf {
        self.ancestor.observed_path.join(&self.suffix)
    }
}
#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub path: PathBuf,
    pub volume: VolumeInfo,
    /// Inspection may follow a UUID to a new mount. This is not a Session cwd update.
    pub relocated: bool,
}

pub enum CheckoutDestination<'a> {
    Default {
        home: &'a Path,
        project_component: &'a str,
        checkout_component: &'a str,
        saved_base: Option<&'a PathBinding>,
    },
    Custom(&'a Path),
}

impl LocationResolver {
    /// Create one explicit child under an existing witnessed parent. No missing
    /// ancestor or mount directory is manufactured, and no existing entry is reused.
    pub fn create_empty_child(
        &self,
        parent: &PhysicalBinding,
        leaf: &std::ffi::OsStr,
    ) -> Result<PhysicalBinding> {
        let resolved = self.resolve_directory(parent)?;
        if resolved.relocated {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                &resolved.path,
                "parent moved; review the destination again",
            ));
        }
        if !resolved.volume.writable {
            return Err(LocationError::new(
                LocationIssue::ReadOnlyVolume,
                &resolved.path,
                "selected volume is read-only",
            ));
        }
        validate_child(leaf, &resolved.path)?;
        #[cfg(target_os = "macos")]
        if std::fs::canonicalize("/Volumes").ok().as_deref() == Some(resolved.path.as_path()) {
            return Err(LocationError::new(
                LocationIssue::InvalidPath,
                &resolved.path,
                "creating a mount-point directory is not a cwd operation",
            ));
        }
        let pinned = PinnedDirectory::open(&resolved.path)?;
        self.verify_effect_parent(parent, &pinned)?;
        #[cfg(unix)]
        {
            use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
            let leaf = std::ffi::CString::new(leaf.as_bytes())
                .map_err(|e| LocationError::io(&resolved.path, e))?;
            // SAFETY: the directory descriptor remains owned, and leaf is one
            // validated NUL-terminated component. mkdirat refuses existing entries.
            if unsafe { libc::mkdirat(pinned.file.as_raw_fd(), leaf.as_ptr(), 0o700) } != 0 {
                return Err(LocationError::io(
                    &resolved.path,
                    std::io::Error::last_os_error(),
                ));
            }
            pinned
                .file
                .sync_all()
                .map_err(|e| LocationError::io(&resolved.path, e))?;
        }
        #[cfg(not(unix))]
        return Err(LocationError::new(
            LocationIssue::Unsupported,
            &resolved.path,
            "pinned directory creation is unavailable",
        ));
        pinned.verify()?;
        self.bind_directory(&resolved.path.join(leaf))
    }

    /// Publish an owned staged directory without replacing a concurrently created
    /// destination. Both names remain relative to the same pinned parent.
    pub fn publish_empty_child(
        &self,
        parent: &PhysicalBinding,
        stage: &PhysicalBinding,
        leaf: &std::ffi::OsStr,
    ) -> Result<PhysicalBinding> {
        let resolved = self.resolve_directory(parent)?;
        let source = self.resolve_directory(stage)?;
        validate_child(leaf, &resolved.path)?;
        if resolved.relocated
            || source.relocated
            || source.path.parent() != Some(resolved.path.as_path())
        {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                &resolved.path,
                "staged directory or parent moved",
            ));
        }
        let pinned = PinnedDirectory::open(&resolved.path)?;
        self.verify_effect_parent(parent, &pinned)?;
        #[cfg(target_os = "macos")]
        {
            use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
            let from = std::ffi::CString::new(source.path.file_name().unwrap().as_bytes())
                .map_err(|e| LocationError::io(&source.path, e))?;
            let to = std::ffi::CString::new(leaf.as_bytes())
                .map_err(|e| LocationError::io(&resolved.path, e))?;
            // SAFETY: owned directory descriptor and validated component names.
            // RENAME_EXCL makes a newly arrived destination an error, never deletion.
            if unsafe {
                libc::renameatx_np(
                    pinned.file.as_raw_fd(),
                    from.as_ptr(),
                    pinned.file.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_EXCL,
                )
            } != 0
            {
                return Err(LocationError::io(
                    &resolved.path,
                    std::io::Error::last_os_error(),
                ));
            }
            pinned
                .file
                .sync_all()
                .map_err(|e| LocationError::io(&resolved.path, e))?;
        }
        #[cfg(not(target_os = "macos"))]
        return Err(LocationError::new(
            LocationIssue::Unsupported,
            &resolved.path,
            "exclusive directory publication is not supported on this platform",
        ));
        pinned.verify()?;
        let binding = self.bind_directory(&resolved.path.join(leaf))?;
        if binding.volume() != stage.volume() || binding.root_witness() != stage.root_witness() {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                binding.observed_path(),
                "published directory identity changed",
            ));
        }
        Ok(binding)
    }

    fn verify_effect_parent(
        &self,
        binding: &PhysicalBinding,
        pinned: &PinnedDirectory,
    ) -> Result<()> {
        if &pinned.witness != binding.root_witness() {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                &pinned.path,
                "parent changed before filesystem effect",
            ));
        }
        if &self.containing_volume(&pinned.path)?.identity != binding.volume() {
            return Err(LocationError::new(
                LocationIssue::WrongVolume,
                &pinned.path,
                "parent volume changed before filesystem effect",
            ));
        }
        pinned.verify()
    }

    pub fn bind_directory(&self, path: &Path) -> Result<PhysicalBinding> {
        validate_absolute(path)?;
        let path = path
            .canonicalize()
            .map_err(|e| LocationError::io(path, e))?;
        let pinned = PinnedDirectory::open(&path)?;
        let volume = self.containing_volume(&path)?;
        // A verified mount-relative spelling is necessary for identity-based
        // remount resolution, including macOS's /Users Data-volume firmlink.
        let relative = relative_to_volume(&path, &volume.mount, &pinned.witness)?;
        pinned.verify()?;
        Ok(PhysicalBinding {
            volume: volume.identity,
            relative_path: relative,
            observed_path: path,
            root_witness: pinned.witness,
            generation: 1,
        })
    }

    pub fn resolve_directory(&self, binding: &PhysicalBinding) -> Result<ResolvedPath> {
        validate_relative(&binding.relative_path)?;
        validate_absolute(&binding.observed_path)?;
        if binding.generation == 0 {
            return Err(LocationError::new(
                LocationIssue::InvalidPath,
                &binding.observed_path,
                "binding has no generation",
            ));
        }
        let volume = self.volume(&binding.volume)?;
        let physical = volume.mount.join(&binding.relative_path);
        let pinned = PinnedDirectory::open(&physical)?;
        if pinned.witness != binding.root_witness {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                &physical,
                "root identity changed; explicit rebind is required",
            ));
        }
        require_volume(
            &binding.volume,
            &self.containing_volume(&physical)?,
            &physical,
        )?;
        // Preserve the ordinary canonical spelling when it still names exactly
        // this root. A missing old mount is never created as a local stand-in.
        let path = match PinnedDirectory::open(&binding.observed_path) {
            Ok(old) if old.witness == pinned.witness => {
                require_volume(
                    &binding.volume,
                    &self.containing_volume(&old.path)?,
                    &old.path,
                )?;
                old.verify()?;
                binding.observed_path.clone()
            }
            _ => physical,
        };
        pinned.verify()?;
        Ok(ResolvedPath {
            relocated: path != binding.observed_path,
            path,
            volume,
        })
    }

    pub fn bind_path(&self, path: &Path) -> Result<PathBinding> {
        validate_absolute(path)?;
        let (ancestor, suffix) = existing_ancestor(path)?;
        let ancestor = self.bind_directory(&ancestor)?;
        Ok(PathBinding { ancestor, suffix })
    }

    pub fn resolve_path(&self, binding: &PathBinding) -> Result<ResolvedPath> {
        validate_relative(&binding.suffix)?;
        let mut resolved = self.resolve_directory(&binding.ancestor)?;
        for component in binding.suffix.components() {
            resolved.path.push(component.as_os_str());
            match std::fs::symlink_metadata(&resolved.path) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(LocationError::new(
                        LocationIssue::ReplacedRoot,
                        &resolved.path,
                        "a previously missing directory became a file or symlink; review the path again",
                    ));
                }
                Ok(_) => require_volume(
                    binding.volume(),
                    &self.containing_volume(&resolved.path)?,
                    &resolved.path,
                )?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(LocationError::io(&resolved.path, e)),
            }
        }
        Ok(resolved)
    }

    /// Returns a complete, revalidatable destination, without making directories.
    /// The chosen UUID is mandatory even for a custom absolute path.
    pub fn checkout_destination(
        &self,
        selected: &VolumeIdentity,
        destination: CheckoutDestination<'_>,
    ) -> Result<PathBinding> {
        let volume = self.volume(selected)?;
        require_writable(&volume)?;
        let path = match destination {
            CheckoutDestination::Custom(path) => path.to_path_buf(),
            CheckoutDestination::Default {
                home,
                project_component,
                checkout_component,
                saved_base,
            } => {
                validate_leaf(project_component)?;
                validate_leaf(checkout_component)?;
                let base = if let Some(binding) = saved_base {
                    require_volume(
                        selected,
                        &self.resolve_path(binding)?.volume,
                        &binding.observed_path(),
                    )?;
                    self.resolve_path(binding)?.path
                } else if volume.internal {
                    validate_absolute(home)?;
                    home.join("jcode-checkouts")
                } else {
                    volume.mount.join("jcode-checkouts")
                };
                base.join(project_component).join(checkout_component)
            }
        };
        let binding = self.bind_path(&path)?;
        if binding.volume() != selected {
            return Err(LocationError::new(
                LocationIssue::WrongVolume,
                &path,
                "destination is not on the selected volume",
            ));
        }
        self.resolve_destination(&binding)?;
        Ok(binding)
    }

    /// Read-only preflight, not a filesystem lease. Clone publication must still
    /// acquire its own operation lease and check at the effect boundary.
    pub fn resolve_destination(&self, binding: &PathBinding) -> Result<ResolvedPath> {
        let resolved = self.resolve_path(binding)?;
        require_writable(&resolved.volume)?;
        match std::fs::symlink_metadata(&resolved.path) {
            Ok(_) => Err(LocationError::new(
                LocationIssue::AlreadyExists,
                &resolved.path,
                "destination already exists; adoption is a separate operation",
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
            Err(e) => Err(LocationError::io(&resolved.path, e)),
        }
    }
}

fn validate_child(leaf: &std::ffi::OsStr, parent: &Path) -> Result<()> {
    let path = Path::new(leaf);
    if path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(LocationError::new(
            LocationIssue::InvalidPath,
            parent,
            "expected one directory component",
        ));
    }
    Ok(())
}

fn require_volume(expected: &VolumeIdentity, actual: &VolumeInfo, path: &Path) -> Result<()> {
    if expected != &actual.identity {
        return Err(LocationError::new(
            LocationIssue::WrongVolume,
            path,
            "volume UUID changed or path crosses onto another volume",
        ));
    }
    Ok(())
}
fn require_writable(volume: &VolumeInfo) -> Result<()> {
    if !volume.writable {
        return Err(LocationError::new(
            LocationIssue::ReadOnlyVolume,
            &volume.mount,
            "selected volume is read-only",
        ));
    }
    Ok(())
}
fn validate_leaf(value: &str) -> Result<()> {
    let path = Path::new(value);
    validate_relative(path)?;
    if path.components().count() != 1 || value.contains('\0') {
        return Err(LocationError::new(
            LocationIssue::InvalidPath,
            path,
            "expected one nonempty filesystem name, not a path",
        ));
    }
    Ok(())
}
fn existing_ancestor(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => {
                // Canonicalization rejects dangling symlinks instead of walking
                // past them to an ancestor and manufacturing a usable path.
                let canonical = existing
                    .canonicalize()
                    .map_err(|e| LocationError::io(existing, e))?;
                let mut suffix = PathBuf::new();
                for component in missing.into_iter().rev() {
                    suffix.push(component);
                }
                return Ok((canonical, suffix));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .ok_or_else(|| LocationError::io(existing, "no existing ancestor"))?,
                );
                existing = existing
                    .parent()
                    .ok_or_else(|| LocationError::io(existing, "no parent"))?;
            }
            Err(e) => return Err(LocationError::io(existing, e)),
        }
    }
}
fn relative_to_volume(path: &Path, mount: &Path, witness: &DirectoryWitness) -> Result<PathBuf> {
    if let Ok(relative) = path.strip_prefix(mount) {
        return Ok(relative.into());
    }
    // APFS firmlinks aren't symbolic links and canonicalize keeps /Users. Do
    // not guess a mapping: prove the volume-root spelling names the same inode
    // and birth identity, then retain only its relative suffix.
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|e| LocationError::io(path, e))?;
    let candidate = mount.join(relative);
    let pinned = PinnedDirectory::open(&candidate)?;
    if &pinned.witness != witness {
        return Err(LocationError::new(
            LocationIssue::WrongVolume,
            path,
            "cannot establish a volume-relative physical path",
        ));
    }
    Ok(relative.into())
}

#[cfg(all(test, target_os = "macos"))]
mod effect_tests {
    use super::*;
    #[test]
    fn effect_parent_rejects_a_new_directory_opened_after_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let resolver = LocationResolver::new();
        let bound = resolver.bind_directory(&parent).unwrap();
        std::fs::rename(&parent, temp.path().join("original")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        let replacement = PinnedDirectory::open(&parent.canonicalize().unwrap()).unwrap();
        assert_eq!(
            resolver
                .verify_effect_parent(&bound, &replacement)
                .unwrap_err()
                .kind,
            LocationIssue::ReplacedRoot
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
    }
}
