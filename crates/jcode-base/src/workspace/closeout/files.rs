//! File preservation is separate from Git history: bundles cannot retain an
//! index, LFS payload, ignored document, symlink or filesystem metadata.
use super::*;
use inventory::{Item, Witness};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PreservedItem {
    pub item: Item,
    pub disposition: CloseoutDisposition,
    pub saved: Option<PathBuf>,
}

pub(super) fn preserve(stored: &StoredCloseout, directory: &Path) -> Result<PathBuf> {
    let tree = directory.join("files");
    let restored = directory.join("verified-restore");
    storage::private_dir(&tree)?;
    storage::private_dir(&restored)?;
    let manifest = directory.join("files.jsonl");
    let mut output = BufWriter::new(storage::private_file(&manifest, true)?);
    let mut directories = Vec::new();
    let mut hardlinks: BTreeMap<(u64, u64), (PathBuf, PathBuf)> = BTreeMap::new();
    inventory::visit(stored, |item| {
        if item.witness.is_none() {
            return Ok(());
        }
        validate_relative(&item.entry.path)?;
        let source = stored.binding.observed_path().join(&item.entry.path);
        let disposition = match stored.decisions.get(&item.entry.id) {
            Some(decision) => decision.disposition.clone(),
            None if stored.record.spec.full_archive
                || item.entry.kind == CloseoutEntryKind::Directory =>
            {
                CloseoutDisposition::Preserve
            }
            None => {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    format!("Unresolved data: {}", item.entry.path.display()),
                ));
            }
        };
        if stored.record.spec.full_archive
            && matches!(
                disposition,
                CloseoutDisposition::Redundant { .. } | CloseoutDisposition::Preserved { .. }
            )
        {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "A full archive cannot exclude entries or substitute external references; reconcile the preservation plan",
            ));
        }
        if !item.entry.blockers.is_empty() {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                format!(
                    "Structural finding remains at {}",
                    item.entry.path.display()
                ),
            ));
        }
        let saved = match &disposition {
            CloseoutDisposition::Retain { .. } => {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    format!(
                        "Retain-in-checkout disposition blocks removal: {}",
                        item.entry.path.display()
                    ),
                ));
            }
            CloseoutDisposition::Redundant { .. } => None,
            CloseoutDisposition::Preserved { path } => {
                let destination =
                    crate::location::native_files::resolve_removal_entry(path).map_err(io)?;
                if destination.starts_with(stored.binding.observed_path()) {
                    return Err(issue(
                        IssueCode::PreservationIncomplete,
                        "Preservation reference is inside the checkout",
                    ));
                }
                verify_copy(&item, &destination)?;
                // Exercise restoration from the claimed durable reference, not
                // merely the source or a caller-supplied digest.
                let test = restored.join(&item.entry.path);
                copy_entry(&item, &destination, &test)?;
                verify_copy(&item, &test)?;
                Some(destination)
            }
            CloseoutDisposition::Preserve => {
                let destination = tree.join(&item.entry.path);
                if item.entry.kind == CloseoutEntryKind::Directory {
                    storage::private_dir(&destination)?;
                    storage::private_dir(&restored.join(&item.entry.path))?;
                    directories.push((item.clone(), source.clone(), destination.clone()));
                } else {
                    let test = restored.join(&item.entry.path);
                    let key = item.witness.as_ref().map(|w| (w.device, w.inode));
                    if item.entry.kind == CloseoutEntryKind::File
                        && let Some((saved, checked)) = key.and_then(|key| hardlinks.get(&key))
                    {
                        std::fs::hard_link(saved, &destination).map_err(io)?;
                        std::fs::hard_link(checked, &test).map_err(io)?;
                    } else {
                        copy_entry(&item, &source, &destination)?;
                        copy_entry(&item, &destination, &test)?;
                        if item.entry.kind == CloseoutEntryKind::File
                            && item.entry.links > 1
                            && let Some(key) = key
                        {
                            hardlinks.insert(key, (destination.clone(), test.clone()));
                        }
                    }
                    verify_copy(&item, &destination)?;
                    verify_copy(&item, &test)?;
                }
                Some(destination)
            }
        };
        serde_json::to_writer(
            &mut output,
            &PreservedItem {
                item,
                disposition,
                saved,
            },
        )
        .map_err(io)?;
        output.write_all(b"\n").map_err(io)?;
        Ok(())
    })?;
    // Directory modes/times/ACLs are installed last so read-only source modes
    // cannot prevent the remaining children from being captured or restored.
    for (item, source, destination) in directories.into_iter().rev() {
        copy_metadata(&source, &destination, false)?;
        copy_metadata(&destination, &restored.join(&item.entry.path), false)?;
    }
    output.flush().map_err(io)?;
    output.get_ref().sync_all().map_err(io)?;
    inventory::verify_source(stored)?;
    storage::sync_dir(directory)?;
    Ok(manifest)
}

pub(super) fn verify_copy(item: &Item, path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    match item.entry.kind {
        CloseoutEntryKind::File => {
            let witness = Witness::of(&metadata)?;
            if !metadata.is_file()
                || witness.mode != item.entry.mode
                || Some(inventory::hash_file(path, &witness)?) != item.entry.sha256
            {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    format!("Preserved file differs: {}", path.display()),
                ));
            }
        }
        CloseoutEntryKind::Symlink => {
            if !metadata.file_type().is_symlink()
                || Some(std::fs::read_link(path).map_err(io)?) != item.entry.link_target
            {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    "Preserved symlink differs",
                ));
            }
        }
        CloseoutEntryKind::Directory if metadata.is_dir() => {}
        _ => {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Unsupported or changed preserved entry",
            ));
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(corrupt("Inventory contains an unsafe relative path"));
    }
    Ok(())
}

pub(super) fn copy_entry(item: &Item, source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| corrupt("Preserved file has no parent"))?;
    storage::private_dir(parent)?;
    let source_before = Witness::of(&std::fs::symlink_metadata(source).map_err(io)?)?;
    let parent_guard =
        crate::location::native_files::VerifiedDirectory::open(parent.to_path_buf()).map_err(io)?;
    match item.entry.kind {
        CloseoutEntryKind::File => {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let mut input = options.open(source).map_err(io)?;
            if !input.metadata().map_err(io)?.is_file()
                || Witness::of(&input.metadata().map_err(io)?)? != source_before
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Preservation source identity changed",
                ));
            }
            let mut output = storage::private_file(destination, true)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let count = input.read(&mut buffer).map_err(io)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(io)?;
                hasher.update(&buffer[..count]);
            }
            if Some(format!("{:x}", hasher.finalize())) != item.entry.sha256
                || Witness::of(&input.metadata().map_err(io)?)? != source_before
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Source changed during preservation",
                ));
            }
            copy_open_metadata(&input, &output)?;
            output.sync_all().map_err(io)?;
        }
        CloseoutEntryKind::Symlink => {
            let target = std::fs::read_link(source).map_err(io)?;
            if Some(&target) != item.entry.link_target.as_ref() {
                return Err(issue(IssueCode::Conflict, "Source symlink changed"));
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, destination).map_err(io)?;
            #[cfg(not(unix))]
            return Err(issue(
                IssueCode::UnsupportedCapability,
                "Native symlink preservation unavailable",
            ));
            copy_metadata(source, destination, true)?;
        }
        _ => {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Unsupported preservation entry",
            ));
        }
    }
    parent_guard.verify().map_err(io)?;
    if Witness::of(&std::fs::symlink_metadata(source).map_err(io)?)? != source_before {
        return Err(issue(
            IssueCode::Conflict,
            "Preservation source changed after copy",
        ));
    }
    storage::sync_dir(parent)
}

#[cfg(target_os = "macos")]
fn copy_open_metadata(source: &File, destination: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: both descriptors are owned, verified regular files. This uses
    // macOS's complete metadata copier, including ACLs and extended attributes.
    if unsafe {
        libc::fcopyfile(
            source.as_raw_fd(),
            destination.as_raw_fd(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA,
        )
    } != 0
    {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn copy_open_metadata(_: &File, _: &File) -> Result<()> {
    Err(issue(
        IssueCode::UnsupportedCapability,
        "Complete closeout metadata preservation requires the native macOS adapter",
    ))
}

#[cfg(target_os = "macos")]
fn copy_metadata(source: &Path, destination: &Path, symlink: bool) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(io)?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes()).map_err(io)?;
    let flags = libc::COPYFILE_METADATA | libc::COPYFILE_NOFOLLOW;
    // SAFETY: NUL-terminated paths name previously created directory or symlink
    // entries. NOFOLLOW applies to both sides; no link target is accessed.
    if unsafe {
        libc::copyfile(
            source.as_ptr(),
            destination.as_ptr(),
            std::ptr::null_mut(),
            flags,
        )
    } != 0
    {
        return Err(io(std::io::Error::last_os_error()));
    }
    if !symlink {
        File::open(Path::new(std::ffi::OsStr::from_bytes(
            destination.as_bytes(),
        )))
        .and_then(|file| file.sync_all())
        .map_err(io)?;
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn copy_metadata(_: &Path, _: &Path, _: bool) -> Result<()> {
    Err(issue(
        IssueCode::UnsupportedCapability,
        "Native metadata preservation unavailable",
    ))
}
