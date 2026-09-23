use super::*;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Witness {
    pub inode: u64,
    pub device: u64,
    pub created: SystemTime,
    pub modified: SystemTime,
    pub changed: (i64, i64),
    pub owner: (u32, u32),
    pub bytes: u64,
    pub mode: u32,
    pub links: u64,
}

impl Witness {
    #[cfg(unix)]
    pub(super) fn of(meta: &Metadata) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            inode: meta.ino(),
            device: meta.dev(),
            created: meta.created().map_err(io)?,
            modified: meta.modified().map_err(io)?,
            changed: (meta.ctime(), meta.ctime_nsec()),
            owner: (meta.uid(), meta.gid()),
            bytes: meta.len(),
            mode: meta.mode(),
            links: meta.nlink(),
        })
    }
    #[cfg(not(unix))]
    pub(super) fn of(_meta: &Metadata) -> Result<Self> {
        Err(issue(
            IssueCode::UnsupportedCapability,
            "Closeout filesystem witnesses are unavailable on this platform",
        ))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Item {
    pub entry: CloseoutEntry,
    pub witness: Option<Witness>,
}

pub(super) struct InventoryWriter {
    file: BufWriter<File>,
    hash: Sha256,
    count: u64,
}
impl InventoryWriter {
    pub(super) fn push(&mut self, mut item: Item) -> Result<()> {
        item.entry.id = digest(encode(&item)?.as_bytes());
        let mut bytes = serde_json::to_vec(&item).map_err(io)?;
        bytes.push(b'\n');
        self.file.write_all(&bytes).map_err(io)?;
        self.hash.update(&bytes);
        self.count += 1;
        Ok(())
    }
}

impl WorkspaceService {
    /// A new observation invalidates previous dispositions and approvals. An
    /// interrupted scan never replaces the last complete inventory.
    pub fn inventory_closeout(
        &self,
        operation: OperationId,
        expected: Revision,
    ) -> Result<CloseoutRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let mut stored = load(&connection, operation)?;
        require_current(&connection, &stored, expected)?;
        require_preparation(&stored)?;
        let root = self
            .resolver
            .resolve_directory(&stored.binding)
            .map_err(io)?
            .path;
        if root != stored.binding.observed_path() {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Checkout moved; explicit binding repair is required",
            ));
        }
        let directory = self
            .root
            .join("closeout-inventories")
            .join(operation.to_string());
        storage::private_dir(&directory)?;
        let stage = directory.join(format!("scan-{}.jsonl", RequestId::new()));
        let mut writer = InventoryWriter {
            file: BufWriter::new(storage::private_file(&stage, true)?),
            hash: Sha256::new(),
            count: 0,
        };
        let metadata = std::fs::symlink_metadata(&root).map_err(io)?;
        let witness = Witness::of(&metadata)?;
        scan(&root, Path::new(""), witness.device, &mut writer)?;
        self.resolver
            .resolve_directory(&stored.binding)
            .map_err(io)?;
        writer.file.flush().map_err(io)?;
        writer.file.get_ref().sync_all().map_err(io)?;
        let hash = format!("{:x}", writer.hash.finalize());
        let count = writer.count;
        drop(writer.file);
        storage::sync_dir(&directory)?;
        self.checkpoint("closeout_inventory_written")?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&transaction, &load(&transaction, operation)?, expected)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        stored.record.inventory_digest = Some(hash);
        stored.record.inventory_entries = count;
        stored.record.stage = CloseoutStage::NeedsDecision;
        stored.record.preservation_digest = None;
        stored.inventory = Some(stage);
        stored.history = None;
        stored.history_digest = None;
        stored.decisions.clear();
        stored.final_approval = None;
        stored.no_loss = None;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }

    pub fn closeout_inventory(
        &self,
        operation: OperationId,
        expected_digest: &str,
        after: u64,
        limit: u32,
    ) -> Result<CloseoutInventoryPage> {
        if !(1..=200).contains(&limit) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Inventory page size must be 1 through 200",
            ));
        }
        let _catalog = self.lease(false)?;
        let stored = load(&self.connection()?, operation)?;
        if stored.record.inventory_digest.as_deref() != Some(expected_digest) {
            return Err(issue(
                IssueCode::Conflict,
                "Inventory changed; restart with its current digest",
            ));
        }
        let mut entries = Vec::new();
        let mut index = 0;
        visit(&stored, |item| {
            if index >= after && entries.len() < limit as usize {
                entries.push(item.entry);
            }
            index += 1;
            Ok(())
        })?;
        let end = after.saturating_add(entries.len() as u64);
        Ok(CloseoutInventoryPage {
            operation,
            digest: expected_digest.into(),
            total: index,
            entries,
            next: (end < index).then_some(end),
        })
    }

    pub fn record_closeout_disposition(
        &self,
        operation: OperationId,
        expected: Revision,
        decision: CloseoutDecision,
    ) -> Result<CloseoutRecord> {
        if decision.recorded_by.trim().is_empty() {
            return Err(issue(
                IssueCode::InvalidInput,
                "Disposition requires provenance",
            ));
        }
        if let CloseoutDisposition::Retain { reason } | CloseoutDisposition::Redundant { reason } =
            &decision.disposition
            && reason.trim().is_empty()
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Disposition requires a reason, not only a label",
            ));
        }
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let mut stored = load(&connection, operation)?;
        require_current(&connection, &stored, expected)?;
        require_preparation(&stored)?;
        let mut found = false;
        visit(&stored, |item| {
            found |= item.entry.id == decision.entry;
            Ok(())
        })?;
        if !found {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Disposition belongs to another inventory",
            ));
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&transaction, &load(&transaction, operation)?, expected)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        stored.record.stage = CloseoutStage::NeedsDecision;
        stored.decisions.insert(decision.entry.clone(), decision);
        stored.final_approval = None;
        stored.no_loss = None;
        stored.record.preservation_digest = None;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }
}

pub(super) fn require_preparation(stored: &StoredCloseout) -> Result<()> {
    if stored.record.stage == CloseoutStage::RecoveryRequired {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Closeout authority requires trusted recovery; preparation cannot reactivate it",
        ));
    }
    if matches!(
        stored.record.stage,
        CloseoutStage::Closed | CloseoutStage::Revoked | CloseoutStage::Removing
    ) {
        return Err(issue(
            IssueCode::Conflict,
            "Closeout no longer accepts preparation changes",
        ));
    }
    Ok(())
}

pub(super) fn visit(
    stored: &StoredCloseout,
    mut visitor: impl FnMut(Item) -> Result<()>,
) -> Result<()> {
    let path = stored.inventory.as_ref().ok_or_else(|| {
        issue(
            IssueCode::IncompleteCapture,
            "Closeout has no completed inventory",
        )
    })?;
    // Validate the entire retained inventory before any consumer performs effects.
    if backup::file_digest(path)? != stored.record.inventory_digest.as_deref().unwrap_or("") {
        return Err(corrupt("Closeout inventory integrity changed"));
    }
    for line in BufReader::new(File::open(path).map_err(io)?).lines() {
        visitor(decode(&line.map_err(io)?)?)?;
    }
    Ok(())
}

pub(super) fn verify_source(stored: &StoredCloseout) -> Result<()> {
    visit(stored, |item| {
        let Some(witness) = item.witness else {
            return Ok(());
        };
        let path = stored.binding.observed_path().join(&item.entry.path);
        if Witness::of(&std::fs::symlink_metadata(&path).map_err(io)?)? != witness {
            return Err(issue(
                IssueCode::Conflict,
                format!("Inventory entry changed: {}", path.display()),
            ));
        }
        if item.entry.kind == CloseoutEntryKind::File
            && Some(hash_file(&path, &witness)?) != item.entry.sha256
        {
            return Err(issue(
                IssueCode::Conflict,
                format!("Inventory file contents changed: {}", path.display()),
            ));
        }
        if item.entry.kind == CloseoutEntryKind::Symlink
            && Some(std::fs::read_link(&path).map_err(io)?) != item.entry.link_target
        {
            return Err(issue(IssueCode::Conflict, "Inventory symlink changed"));
        }
        Ok(())
    })
}

pub(super) fn append(
    stored: &mut StoredCloseout,
    entries: Vec<CloseoutEntry>,
    directory: &Path,
) -> Result<()> {
    let path = directory.join(format!("inventory-{}.jsonl", RequestId::new()));
    let mut writer = InventoryWriter {
        file: BufWriter::new(storage::private_file(&path, true)?),
        hash: Sha256::new(),
        count: 0,
    };
    visit(stored, |mut item| {
        item.entry.id.clear();
        writer.push(item)
    })?;
    for entry in entries {
        writer.push(Item {
            entry,
            witness: None,
        })?;
    }
    writer.file.flush().map_err(io)?;
    writer.file.get_ref().sync_all().map_err(io)?;
    stored.record.inventory_digest = Some(format!("{:x}", writer.hash.finalize()));
    stored.record.inventory_entries = writer.count;
    stored.inventory = Some(path);
    storage::sync_dir(directory)
}

fn scan(root: &Path, relative: &Path, device: u64, writer: &mut InventoryWriter) -> Result<()> {
    let path = root.join(relative);
    let before = std::fs::symlink_metadata(&path).map_err(io)?;
    let witness = Witness::of(&before)?;
    let kind = if witness.device != device {
        CloseoutEntryKind::Mount
    } else if before.file_type().is_symlink() {
        CloseoutEntryKind::Symlink
    } else if before.is_dir() {
        CloseoutEntryKind::Directory
    } else if before.is_file() {
        CloseoutEntryKind::File
    } else {
        CloseoutEntryKind::Special
    };
    let mut entry = CloseoutEntry {
        id: String::new(),
        path: relative.into(),
        kind,
        bytes: before.len(),
        sha256: None,
        link_target: None,
        links: witness.links,
        mode: witness.mode,
        facts: vec![],
        blockers: vec![],
    };
    match kind {
        CloseoutEntryKind::File => entry.sha256 = Some(hash_file(&path, &witness)?),
        CloseoutEntryKind::Symlink => {
            entry.link_target = Some(std::fs::read_link(&path).map_err(io)?)
        }
        CloseoutEntryKind::Mount | CloseoutEntryKind::Special => entry.blockers.push(issue(
            IssueCode::PreservationIncomplete,
            "Mount or special file requires resolution before removal",
        )),
        _ => {}
    }
    if witness.links > 1 && before.is_file() {
        entry
            .facts
            .push("multiply-linked file; other aliases are not removed".into());
    }
    if relative.components().any(|part| part.as_os_str() == ".git") {
        entry
            .facts
            .push("Git metadata, not disposable merely because the worktree is clean".into());
    }
    writer.push(Item {
        entry,
        witness: Some(witness.clone()),
    })?;
    if kind == CloseoutEntryKind::Directory {
        let mut children = std::fs::read_dir(&path)
            .map_err(io)?
            .map(|r| r.map(|e| e.file_name()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io)?;
        children.sort();
        for child in children {
            scan(root, &relative.join(child), device, writer)?;
        }
    }
    if Witness::of(&std::fs::symlink_metadata(&path).map_err(io)?)? != witness {
        return Err(issue(
            IssueCode::Conflict,
            format!("Entry changed during inventory: {}", path.display()),
        ));
    }
    Ok(())
}

pub(super) fn hash_file(path: &Path, expected: &Witness) -> Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file()
        || Witness::of(&file.metadata().map_err(io)?)? != *expected
    {
        return Err(issue(
            IssueCode::Conflict,
            "File identity changed before reading",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(io)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if Witness::of(&file.metadata().map_err(io)?)? != *expected
        || Witness::of(&std::fs::symlink_metadata(path).map_err(io)?)? != *expected
    {
        return Err(issue(IssueCode::Conflict, "File changed while reading"));
    }
    Ok(format!("{:x}", hash.finalize()))
}
