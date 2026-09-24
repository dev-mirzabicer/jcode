use super::*;

#[derive(Serialize, Deserialize)]
pub(super) struct LfsObject {
    pub oid: String,
    pub size: u64,
    #[serde(with = "jcode_workspace_types::filesystem_path")]
    pub source: PathBuf,
    pub preserved: PathBuf,
    pub restored: PathBuf,
}

pub(super) async fn preserve(
    runner: &GitExecution<'_>,
    snapshot: &RepositorySnapshot,
    stage: &Path,
    restored: &Path,
    archive: &archive::Archive<'_>,
) -> Result<()> {
    // The restored refs include detached/reflog history as well as every
    // original ref. Git LFS supplies its established pointer parser, offline.
    let listing = stage.join("lfs-history.json");
    runner
        .run(
            restored,
            &["lfs", "ls-files", "--all", "--json"],
            None,
            &listing,
        )
        .await?;
    let value: serde_json::Value =
        serde_json::from_reader(File::open(&listing).map_err(io)?).map_err(io)?;
    let entries = match value.get("files") {
        Some(serde_json::Value::Array(entries)) => entries.clone(),
        Some(serde_json::Value::Null) => vec![],
        _ => return Err(corrupt("Invalid Git LFS history inventory")),
    };
    let config = stage.join("lfs-storage");
    runner
        .run(
            &snapshot.root,
            &["config", "--null", "--default", "", "--get", "lfs.storage"],
            None,
            &config,
        )
        .await?;
    let bytes = std::fs::read(&config).map_err(io)?;
    let value = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| corrupt("Invalid LFS storage configuration"))?;
    if value.contains(&0) {
        return Err(corrupt("Ambiguous LFS storage configuration"));
    }
    use std::os::unix::ffi::OsStrExt;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(value));
    let objects = if path.as_os_str().is_empty() {
        snapshot.common_directory.join("lfs/objects")
    } else if path.is_absolute() {
        path.join("objects")
    } else {
        snapshot.common_directory.join(path).join("objects")
    };
    let mut preserved = BTreeMap::new();
    for entry in entries {
        let oid = entry
            .get("oid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| corrupt("LFS object ID missing"))?;
        let size = entry
            .get("size")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| corrupt("LFS object size missing"))?;
        if oid.len() != 64
            || !oid.bytes().all(|c| c.is_ascii_hexdigit())
            || entry.get("oid_type").and_then(|v| v.as_str()) != Some("sha256")
        {
            return Err(corrupt("Unsupported LFS object identity"));
        }
        if let Some(previous) = preserved.get(oid) {
            let previous: &LfsObject = previous;
            if previous.size != size {
                return Err(corrupt("LFS object has contradictory sizes"));
            }
            continue;
        }
        let suffix = PathBuf::from(&oid[..2]).join(&oid[2..4]).join(oid);
        let source = objects.join(&suffix);
        let metadata = std::fs::symlink_metadata(&source).map_err(|e| {
            issue(
                IssueCode::PreservationIncomplete,
                format!("Required historical LFS object {oid} is unavailable: {e}"),
            )
        })?;
        let witness = inventory::Witness::of(&metadata)?;
        if !metadata.is_file()
            || metadata.len() != size
            || inventory::hash_file(&source, &witness)? != oid
        {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                format!("Historical LFS payload {oid} differs from its pointer"),
            ));
        }
        let item = inventory::Item {
            entry: CloseoutEntry {
                id: oid.into(),
                path: suffix.clone(),
                kind: CloseoutEntryKind::File,
                bytes: size,
                sha256: Some(oid.into()),
                link_target: None,
                links: witness.links,
                mode: witness.mode,
                facts: vec![],
                blockers: vec![],
            },
            witness: Some(witness),
        };
        let destination = stage.join("lfs").join(oid);
        files::copy_entry(archive, &item, &source, &destination)?;
        let target = restored.join("lfs/objects").join(suffix);
        files::copy_entry(archive, &item, &destination, &target)?;
        files::verify_copy(&item, &target)?;
        preserved.insert(
            oid.to_owned(),
            LfsObject {
                oid: oid.into(),
                size,
                source,
                preserved: destination,
                restored: target,
            },
        );
    }
    archive.json(&stage.join("lfs.json"), &preserved)
}
