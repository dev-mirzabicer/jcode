use super::*;
use std::io::BufRead;

pub(super) fn hash_value<T: Serialize>(value: &T) -> Result<String> {
    Ok(digest(encode(value)?.as_bytes()))
}

/// Revalidate the actual retained content. The initial restore exercise proves
/// recoverability; a later approval must also reject damaged retained evidence.
pub(super) fn preservation(stored: &StoredCloseout) -> Result<()> {
    let path = stored.preservation.as_ref().ok_or_else(|| {
        issue(
            IssueCode::PreservationIncomplete,
            "Prepare and verify preservation before removal review",
        )
    })?;
    if Some(backup::file_digest(path)?) != stored.record.preservation_digest {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Preservation manifest integrity changed",
        ));
    }
    let manifest: super::preservation::PreservationManifest = storage::read_json(path)?;
    if manifest.operation != stored.record.operation
        || Some(&manifest.inventory_digest) != stored.record.inventory_digest.as_ref()
        || manifest.decisions_digest != hash_value(&stored.decisions)?
    {
        return Err(issue(
            IssueCode::Conflict,
            "Preservation no longer matches this inventory and disposition plan",
        ));
    }
    for (path, expected) in manifest
        .bundles
        .iter()
        .chain(&manifest.git_manifests)
        .chain(std::iter::once(&manifest.references))
    {
        if !std::fs::symlink_metadata(path).map_err(io)?.is_file()
            || backup::file_digest(path)? != *expected
        {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                format!("Retained preservation changed: {}", path.display()),
            ));
        }
    }
    verify_files(
        &manifest.files,
        &manifest.files_digest,
        stored.binding.observed_path(),
    )?;
    for (path, _) in &manifest.git_manifests {
        match path.file_name().and_then(|s| s.to_str()) {
            Some("administration.json") => {
                let (files, hash): (PathBuf, String) = storage::read_json(path)?;
                verify_files(&files, &hash, stored.binding.observed_path())?;
            }
            Some("lfs.json") => git::verify_lfs_manifest(path)?,
            Some("verified-refs.json") => {}
            _ => return Err(corrupt("Unknown preservation manifest kind")),
        }
    }
    Ok(())
}

fn verify_files(path: &Path, expected: &str, source: &Path) -> Result<()> {
    if backup::file_digest(path)? != expected {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Preserved file manifest changed",
        ));
    }
    for line in std::io::BufReader::new(std::fs::File::open(path).map_err(io)?).lines() {
        let saved: files::PreservedItem = decode(&line.map_err(io)?)?;
        if let Some(path) = saved.saved {
            let resolved =
                crate::location::native_files::resolve_removal_entry(&path).map_err(io)?;
            if resolved.starts_with(source) {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    "A preservation reference now resolves inside the checkout",
                ));
            }
            files::verify_copy(&saved.item, &resolved)?;
        } else if !matches!(saved.disposition, CloseoutDisposition::Redundant { .. }) {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Unresolved retained file disposition",
            ));
        }
    }
    Ok(())
}

pub(super) fn linked_content(
    stored: &StoredCloseout,
    references: &references::References,
) -> Result<()> {
    let mut required = std::collections::BTreeSet::new();
    for link in &references.links {
        for path in [
            crate::location::native_files::resolve_target(&link.path).map_err(io)?,
            crate::location::native_files::resolve_removal_entry(&link.path).map_err(io)?,
        ] {
            if let Ok(relative) = path.strip_prefix(stored.binding.observed_path()) {
                required.insert(relative.to_path_buf());
            }
        }
    }
    if required.is_empty() {
        return Ok(());
    }
    let manifest: super::preservation::PreservationManifest = storage::read_json(
        stored
            .preservation
            .as_ref()
            .ok_or_else(|| issue(IssueCode::PreservationIncomplete, "Preservation is missing"))?,
    )?;
    for line in std::io::BufReader::new(std::fs::File::open(manifest.files).map_err(io)?).lines() {
        let item: files::PreservedItem = decode(&line.map_err(io)?)?;
        if item.saved.is_none()
            && item
                .item
                .entry
                .path
                .ancestors()
                .any(|path| required.contains(path))
        {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                format!(
                    "Known linked data needs a verified preservation disposition: {}",
                    item.item.entry.path.display()
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn references_digest(references: &references::References) -> Result<String> {
    let mut stable = references.clone();
    // The operation itself owns this lifecycle transition. Organization,
    // binding, sessions, grants and linked data still remain in the fingerprint.
    stable.location.lifecycle = LocationLifecycle::Ready;
    stable.location.revision = 0;
    let resolved = stable
        .links
        .iter()
        .map(|link| {
            Ok((
                crate::location::native_files::resolve_target(&link.path).map_err(io)?,
                crate::location::native_files::resolve_removal_entry(&link.path).map_err(io)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    hash_value(&(stable, resolved))
}

pub(super) fn seal(stored: &StoredCloseout, review: &CloseoutReview) -> Result<String> {
    hash_value(&(
        stored.record.operation,
        &stored.record.spec,
        &stored.binding,
        &stored.destination,
        &stored.record.inventory_digest,
        &stored.record.preservation_digest,
        hash_value(&stored.decisions)?,
        &review.references_digest,
    ))
}
