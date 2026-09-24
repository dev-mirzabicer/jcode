//! Validate retained journals without inferring unrecorded filesystem effects.
use super::*;

impl WorkspaceService {
    pub(super) fn verify_recovery_remainder(&self, stored: &StoredCloseout) -> Result<()> {
        let removal = stored.removal.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "No removal journal exists; restart preparation or retain files",
            )
        })?;
        if self
            .resolver
            .resolve_directory(&removal.parent)
            .map_err(io)?
            .relocated
        {
            return Err(issue(IssueCode::ReplacedRoot, "Removal parent relocated"));
        }
        let source = stored.binding.observed_path();
        if present(source)? {
            if removal.quarantined.is_some()
                || stored.record.removed_entries != 0
                || removal.pending.is_some()
                || removal.root_removed
                || present(&removal.quarantine)?
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Original path is occupied after removal began; its contents will not be adopted",
                ));
            }
            self.verify_closeout_evidence(stored)?;
            let dependencies = git::dependent_worktrees(stored)?;
            if let Some(issue) = dependencies.into_iter().next() {
                return Err(issue);
            }
            return Ok(());
        }
        if present(&removal.quarantine)? {
            let actual = self
                .resolver
                .bind_directory(&removal.quarantine)
                .map_err(io)?;
            if actual.volume() != stored.binding.volume()
                || actual.root_witness() != stored.binding.root_witness()
            {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Quarantine is not the original checkout root",
                ));
            }
        } else if !removal.root_removed
            && !removal
                .pending
                .as_ref()
                .is_some_and(|p| p.item.entry.path.as_os_str().is_empty())
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Quarantine disappeared without a journaled root effect",
            ));
        }
        if let Some(binding) = &removal.holding_binding
            && present(&removal.holding)?
            && self
                .resolver
                .resolve_directory(binding)
                .map_err(io)?
                .relocated
        {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Holding directory relocated",
            ));
        }
        if let Some(worktree) = &removal.worktree
            && !worktree.complete
        {
            if self
                .resolver
                .resolve_directory(&worktree.common)
                .map_err(io)?
                .relocated
            {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Shared Git repository moved",
                ));
            }
            if present(worktree.administration.observed_path())? {
                if self
                    .resolver
                    .resolve_directory(&worktree.administration)
                    .map_err(io)?
                    .relocated
                {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Worktree administration moved",
                    ));
                }
                inventory::verify_tree(&worktree.snapshot)?;
            } else if !worktree.started {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Worktree administration has an unrecorded disappearance",
                ));
            }
        }
        let tree = inventory::source(stored)?;
        if backup::file_digest(&tree.manifest)? != tree.digest {
            return Err(corrupt("Recovery inventory integrity changed"));
        }
        let slot = removal.holding.join("entry");
        let mut index = 0;
        let mut expected_present = 0;
        visit_reverse(&tree.manifest, |item| {
            let Some(witness) = &item.witness else {
                return Ok(());
            };
            index += 1;
            let original = removal.quarantine.join(&item.entry.path);
            if index <= stored.record.removed_entries {
                if present(&original)? {
                    return Err(issue(
                        IssueCode::Conflict,
                        "New data occupies an already-removed entry",
                    ));
                }
                return Ok(());
            }
            let pending = removal
                .pending
                .as_ref()
                .filter(|p| p.item.entry.id == item.entry.id);
            if index == stored.record.removed_entries + 1
                && removal.pending.is_some()
                && pending.is_none()
            {
                return Err(corrupt("Pending entry differs from the removal cursor"));
            }
            if let Some(pending) = pending {
                if verification::hash_value(&pending.item)? != verification::hash_value(&item)? {
                    return Err(corrupt("Pending inventory witness changed"));
                }
                if present(&slot)? {
                    if present(&original)? {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Both captured and source entries exist",
                        ));
                    }
                    verify_entry(&item, &slot, true)?;
                    expected_present += 1;
                    return Ok(());
                }
                if !present(&original)? && pending.captured {
                    return Ok(());
                }
            }
            if !present(&original)? {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    format!(
                        "No receipt explains missing entry {}; retain files or repair the journal",
                        item.entry.path.display()
                    ),
                ));
            }
            let metadata = std::fs::symlink_metadata(&original).map_err(io)?;
            let actual = Witness::of(&metadata)?;
            if item.entry.kind == CloseoutEntryKind::Directory {
                if actual.inode != witness.inode
                    || actual.device != witness.device
                    || actual.created != witness.created
                    || actual.mode != witness.mode
                    || actual.owner != witness.owner
                {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Remaining directory identity changed",
                    ));
                }
            } else {
                verify_entry(&item, &original, false)?;
            }
            expected_present += 1;
            Ok(())
        })?;
        if stored.record.removed_entries > index {
            return Err(corrupt("Removal progress exceeds inventory"));
        }
        let device = if present(&removal.quarantine)? {
            Some(Witness::of(&std::fs::symlink_metadata(&removal.quarantine).map_err(io)?)?.device)
        } else {
            removal
                .pending
                .as_ref()
                .and_then(|p| p.item.witness.as_ref())
                .map(|w| w.device)
        };
        let mut actual_present = 0;
        for path in [&removal.quarantine, &slot] {
            if present(path)? {
                actual_present += count_tree(path, device)?;
            }
        }
        if present(&removal.holding)? {
            for entry in std::fs::read_dir(&removal.holding).map_err(io)? {
                if entry.map_err(io)?.file_name() != "entry" {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Unexpected data in holding directory",
                    ));
                }
            }
        }
        if actual_present != expected_present {
            return Err(issue(
                IssueCode::Conflict,
                "Remaining tree contains unreviewed entries",
            ));
        }
        Ok(())
    }
}

fn count_tree(path: &Path, device: Option<u64>) -> Result<u64> {
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    let witness = Witness::of(&metadata)?;
    if device.is_some_and(|device| witness.device != device) {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Recovery tree crosses a mounted filesystem",
        ));
    }
    let mut count = 1;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(io)? {
            count += count_tree(&entry.map_err(io)?.path(), device)?;
        }
    }
    if Witness::of(&std::fs::symlink_metadata(path).map_err(io)?)? != witness {
        return Err(issue(
            IssueCode::Conflict,
            "Recovery tree changed while inspected",
        ));
    }
    Ok(count)
}
