//! File preservation is separate from Git history: bundles cannot retain an
//! index, LFS payload, ignored document, symlink or filesystem metadata.
use super::*;
use inventory::{Item, Witness};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
mod metadata;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PreservedItem {
    pub item: Item,
    pub disposition: CloseoutDisposition,
    #[serde(default, with = "jcode_workspace_types::filesystem_path::optional")]
    pub saved: Option<PathBuf>,
    #[serde(default)]
    pub metadata_digest: Option<String>,
}

#[cfg(test)]
pub(super) fn preserve(stored: &StoredCloseout, directory: &Path) -> Result<PathBuf> {
    preserve_in(stored, directory, &archive::Archive::open(directory)?)
}

pub(super) fn preserve_in(
    stored: &StoredCloseout,
    directory: &Path,
    archive: &archive::Archive<'_>,
) -> Result<PathBuf> {
    preserve_tree(
        &inventory::source(stored)?,
        &stored.decisions,
        stored.record.spec.full_archive,
        directory,
        archive,
    )
}

pub(super) fn preserve_tree(
    source_tree: &inventory::TreeSnapshot,
    decisions: &BTreeMap<String, CloseoutDecision>,
    full_archive: bool,
    directory: &Path,
    archive: &archive::Archive<'_>,
) -> Result<PathBuf> {
    inventory::verify_tree_controlled(source_tree, archive.control())?;
    let tree = directory.join("files");
    let restored = directory.join("verified-restore");
    archive.directory(&tree)?;
    archive.directory(&restored)?;
    let manifest = directory.join("files.jsonl");
    let plan = directory.join("files-plan.jsonl");
    let mut output = BufWriter::new(archive.file(&plan)?);
    let mut directories = Vec::new();
    let mut hardlinks: BTreeMap<(u64, u64), (PathBuf, PathBuf)> = BTreeMap::new();
    inventory::visit_tree(source_tree, |item| {
        archive.check_cancelled()?;
        if item.witness.is_none() {
            return Ok(());
        }
        validate_relative(&item.entry.path)?;
        let source = source_tree.root.join(&item.entry.path);
        let disposition = match decisions.get(&item.entry.id) {
            Some(decision) => decision.disposition.clone(),
            None if full_archive || item.entry.kind == CloseoutEntryKind::Directory => {
                CloseoutDisposition::Preserve
            }
            None => {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    format!("Unresolved data: {}", item.entry.path.display()),
                ));
            }
        };
        if full_archive
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
            CloseoutDisposition::Redundant { .. } => {
                if item.entry.kind == CloseoutEntryKind::Directory {
                    archive.directory(&tree.join(&item.entry.path))?;
                    archive.directory(&restored.join(&item.entry.path))?;
                }
                None
            }
            CloseoutDisposition::Preserved { path } => {
                let destination =
                    crate::location::native_files::resolve_removal_entry(path).map_err(io)?;
                if destination.starts_with(&source_tree.root) {
                    return Err(issue(
                        IssueCode::PreservationIncomplete,
                        "Preservation reference is inside the checkout",
                    ));
                }
                verify_contents(&item, &destination, false)?;
                // Exercise restoration from the claimed durable reference, not
                // merely the source or a caller-supplied digest.
                let test = restored.join(&item.entry.path);
                if item.entry.kind == CloseoutEntryKind::Directory {
                    archive.directory(&test)?;
                    let local = tree.join(&item.entry.path);
                    archive.directory(&local)?;
                    directories.push((item.clone(), source.clone(), local));
                } else {
                    let key = item.witness.as_ref().map(|w| (w.device, w.inode));
                    if item.entry.kind == CloseoutEntryKind::File
                        && let Some((_, checked)) = key.and_then(|key| hardlinks.get(&key))
                    {
                        link_entry(archive, checked, &test)?;
                    } else {
                        copy_entry(archive, &item, &destination, &test)?;
                    }
                    // The reference proves content, not the source's ACLs,
                    // attributes or timestamps. Keep the already-exercised
                    // restoration as durable evidence with original metadata.
                    copy_metadata(archive, &source, &test, item.entry.kind)?;
                    if item.entry.kind == CloseoutEntryKind::File
                        && item.entry.links > 1
                        && let Some(key) = key
                    {
                        hardlinks
                            .entry(key)
                            .or_insert_with(|| (test.clone(), test.clone()));
                    }
                }
                verify_copy(&item, &test)?;
                Some(test)
            }
            CloseoutDisposition::Preserve => {
                let destination = tree.join(&item.entry.path);
                if item.entry.kind == CloseoutEntryKind::Directory {
                    archive.directory(&destination)?;
                    archive.directory(&restored.join(&item.entry.path))?;
                    directories.push((item.clone(), source.clone(), destination.clone()));
                } else {
                    let test = restored.join(&item.entry.path);
                    let key = item.witness.as_ref().map(|w| (w.device, w.inode));
                    if item.entry.kind == CloseoutEntryKind::File
                        && let Some((saved, checked)) = key.and_then(|key| hardlinks.get(&key))
                    {
                        link_entry(archive, saved, &destination)?;
                        link_entry(archive, checked, &test)?;
                    } else {
                        copy_entry(archive, &item, &source, &destination)?;
                        copy_entry(archive, &item, &destination, &test)?;
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
                metadata_digest: None,
            },
        )
        .map_err(io)?;
        output.write_all(b"\n").map_err(io)?;
        Ok(())
    })?;
    // Directory modes/times/ACLs are installed last so read-only source modes
    // cannot prevent the remaining children from being captured or restored.
    for (item, source, destination) in directories.into_iter().rev() {
        copy_metadata(archive, &source, &destination, CloseoutEntryKind::Directory)?;
        copy_metadata(
            archive,
            &destination,
            &restored.join(&item.entry.path),
            CloseoutEntryKind::Directory,
        )?;
    }
    output.flush().map_err(io)?;
    output.get_ref().sync_all().map_err(io)?;
    let plan_guard = archive.existing_directory(directory)?;
    let input = plan_guard
        .open_preserved_entry(std::ffi::OsStr::new("files-plan.jsonl"), false)
        .map_err(io)?;
    let mut sealed = BufWriter::new(archive.file(&manifest)?);
    for line in BufReader::new(input).lines() {
        archive.check_cancelled()?;
        let mut item: PreservedItem = decode(&line.map_err(io)?)?;
        if let Some(saved) = &item.saved {
            item.metadata_digest = Some(metadata::fingerprint(saved, item.item.entry.kind)?);
        }
        serde_json::to_writer(&mut sealed, &item).map_err(io)?;
        sealed.write_all(b"\n").map_err(io)?;
    }
    sealed.flush().map_err(io)?;
    sealed.get_ref().sync_all().map_err(io)?;
    plan_guard.verify().map_err(io)?;
    inventory::verify_tree_controlled(source_tree, archive.control())?;
    storage::sync_dir(directory)?;
    archive.verify()?;
    Ok(manifest)
}

pub(super) fn verify_copy(item: &Item, path: &Path) -> Result<()> {
    verify_contents(item, path, true)
}

pub(super) fn verify_saved(item: &PreservedItem, path: &Path) -> Result<()> {
    verify_copy(&item.item, path)?;
    if item.metadata_digest.as_ref() != Some(&metadata::fingerprint(path, item.item.entry.kind)?) {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Preserved metadata differs or has no verified receipt; prepare preservation again",
        ));
    }
    Ok(())
}

fn verify_contents(item: &Item, path: &Path, enforce_mode: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    match item.entry.kind {
        CloseoutEntryKind::File => {
            let witness = Witness::of(&metadata)?;
            if !metadata.is_file()
                || (enforce_mode && witness.mode != item.entry.mode)
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

pub(super) fn copy_entry(
    archive: &archive::Archive<'_>,
    item: &Item,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| corrupt("Preserved file has no parent"))?;
    let source_before = Witness::of(&std::fs::symlink_metadata(source).map_err(io)?)?;
    let parent_guard = archive.directory(parent)?;
    let source_parent = crate::location::native_files::VerifiedDirectory::open(
        source
            .parent()
            .ok_or_else(|| corrupt("Source has no parent"))?
            .into(),
    )
    .map_err(io)?;
    let source_name = source
        .file_name()
        .ok_or_else(|| corrupt("Source has no leaf"))?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| corrupt("Destination has no leaf"))?;
    match item.entry.kind {
        CloseoutEntryKind::File => {
            let mut input = source_parent
                .open_preserved_entry(source_name, false)
                .map_err(io)?;
            if !input.metadata().map_err(io)?.is_file()
                || Witness::of(&input.metadata().map_err(io)?)? != source_before
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Preservation source identity changed",
                ));
            }
            let mut output = parent_guard
                .create_preserved_file(destination_name)
                .map_err(io)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                archive.check_cancelled()?;
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
            parent_guard
                .create_preserved_symlink(destination_name, &target)
                .map_err(io)?;
            copy_metadata(archive, source, destination, CloseoutEntryKind::Symlink)?;
        }
        _ => {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Unsupported preservation entry",
            ));
        }
    }
    parent_guard.verify().map_err(io)?;
    source_parent.verify().map_err(io)?;
    if Witness::of(&std::fs::symlink_metadata(source).map_err(io)?)? != source_before {
        return Err(issue(
            IssueCode::Conflict,
            "Preservation source changed after copy",
        ));
    }
    archive.verify()
}

fn link_entry(archive: &archive::Archive<'_>, source: &Path, destination: &Path) -> Result<()> {
    let from = archive.directory(
        source
            .parent()
            .ok_or_else(|| corrupt("Link source parent missing"))?,
    )?;
    let to = archive.directory(
        destination
            .parent()
            .ok_or_else(|| corrupt("Link destination parent missing"))?,
    )?;
    to.link_preserved_file(
        destination
            .file_name()
            .ok_or_else(|| corrupt("Link destination missing"))?,
        &from,
        source
            .file_name()
            .ok_or_else(|| corrupt("Link source missing"))?,
    )
    .map_err(io)
}

#[cfg(target_os = "macos")]
fn copy_open_metadata(source: &File, destination: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: both descriptors are owned, verified filesystem entries. This uses
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

fn copy_metadata(
    archive: &archive::Archive<'_>,
    source: &Path,
    destination: &Path,
    kind: CloseoutEntryKind,
) -> Result<()> {
    use crate::location::native_files::VerifiedDirectory;
    if kind != CloseoutEntryKind::Directory {
        let from = VerifiedDirectory::open(
            source
                .parent()
                .ok_or_else(|| corrupt("Metadata source parent missing"))?
                .into(),
        )
        .map_err(io)?;
        let to = archive.directory(
            destination
                .parent()
                .ok_or_else(|| corrupt("Metadata destination parent missing"))?,
        )?;
        let input = from
            .open_preserved_entry(
                source
                    .file_name()
                    .ok_or_else(|| corrupt("Metadata source missing"))?,
                kind == CloseoutEntryKind::Symlink,
            )
            .map_err(io)?;
        let output = to
            .open_preserved_entry(
                destination
                    .file_name()
                    .ok_or_else(|| corrupt("Metadata destination missing"))?,
                kind == CloseoutEntryKind::Symlink,
            )
            .map_err(io)?;
        copy_open_metadata(&input, &output)?;
        output.sync_all().map_err(io)?;
        from.verify().map_err(io)?;
        to.verify().map_err(io)?;
    } else {
        let from = VerifiedDirectory::open(source.into()).map_err(io)?;
        let mut to = archive.existing_directory(destination)?;
        to.copy_preserved_metadata(from.preservation_handle().map_err(io)?)
            .map_err(io)?;
        from.verify().map_err(io)?;
    }
    archive.verify()
}
