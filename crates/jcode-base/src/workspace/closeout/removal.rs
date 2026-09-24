//! Exactly one sequential, catalog-journaled removal owner. Filesystem effects
//! use pinned directories and an exclusive holding slot, never recursive removal.
use super::*;
use crate::location::native_files::VerifiedDirectory;
use inventory::{Item, Witness};
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "macos")]
mod recovery;
#[cfg(target_os = "macos")]
mod remainder;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Removal {
    parent: PhysicalBinding,
    quarantine: PathBuf,
    holding: PathBuf,
    holding_binding: Option<PhysicalBinding>,
    quarantined: Option<PhysicalBinding>,
    pending: Option<Pending>,
    root_removed: bool,
    worktree: Option<WorktreeRetirement>,
}
impl Removal {
    pub(super) fn control_paths(&self) -> [&Path; 2] {
        [&self.quarantine, &self.holding]
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct WorktreeRetirement {
    administration: PhysicalBinding,
    common: PhysicalBinding,
    snapshot: inventory::TreeSnapshot,
    started: bool,
    complete: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    item: Item,
    captured: bool,
}

impl WorkspaceService {
    pub fn closeout_removal_progress(
        &self,
        operation: OperationId,
        expected: Revision,
        after: u64,
        limit: u32,
    ) -> Result<CloseoutRemovalPage> {
        if !(1..=200).contains(&limit) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Removal page size must be 1 through 200",
            ));
        }
        let _catalog = self.lease(false)?;
        let stored = load(&self.connection()?, operation)?;
        require_authorization_revision(&self.connection()?, &stored, expected)?;
        let tree = inventory::source(&stored)?;
        if backup::file_digest(&tree.manifest)? != tree.digest {
            return Err(corrupt("Removal inventory changed"));
        }
        let pending = stored
            .removal
            .as_ref()
            .and_then(|r| r.pending.as_ref())
            .map(|p| CloseoutRemovalEntry {
                id: p.item.entry.id.clone(),
                path: p.item.entry.path.clone(),
                progress: CloseoutEntryProgress::Unconfirmed,
            });
        let mut total = 0;
        let mut entries = Vec::new();
        visit_reverse(&tree.manifest, |item| {
            if item.witness.is_none() {
                return Ok(());
            }
            let index = total;
            total += 1;
            if index >= after && entries.len() < limit as usize {
                let progress = if index < stored.record.removed_entries {
                    CloseoutEntryProgress::Removed
                } else if pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == item.entry.id)
                {
                    CloseoutEntryProgress::Unconfirmed
                } else {
                    CloseoutEntryProgress::NotProcessed
                };
                entries.push(CloseoutRemovalEntry {
                    id: item.entry.id,
                    path: item.entry.path,
                    progress,
                });
            }
            Ok(())
        })?;
        require_authorization_revision(
            &self.connection()?,
            &load(&self.connection()?, operation)?,
            expected,
        )?;
        let next = after.saturating_add(entries.len() as u64);
        Ok(CloseoutRemovalPage {
            operation,
            revision: expected,
            total,
            completed: stored.record.removed_entries,
            pending,
            entries,
            next: (next < total).then_some(next),
        })
    }

    pub fn closed_checkout_history(&self, location: LocationId) -> Result<CloseoutHistory> {
        let _catalog = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction().map_err(io)?;
        let Entity::Location(location) = entity(&transaction, EntityId::Location(location))? else {
            return Err(corrupt("Location history target changed kind"));
        };
        if !location.lifecycle.is_historical() {
            return Err(issue(IssueCode::InvalidIdentity, "Checkout has not closed"));
        }
        let body: String = transaction
            .query_row(
                "SELECT body FROM closed_history WHERE location=?1",
                [location.id.to_string()],
                |row| row.get(0),
            )
            .map_err(corrupt)?;
        let reference: portable::ClosedReference = decode(&body)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND kind='closeout')",
                [reference.operation.to_string()],
                |row| row.get(0),
            )
            .map_err(io)?;
        let record = if exists {
            let record = load(&transaction, reference.operation)?.record;
            if !matches!(
                record.stage,
                CloseoutStage::Closed | CloseoutStage::Retained
            ) || record.spec.location != location.id
            {
                return Err(corrupt("Closed receipt does not match its location"));
            }
            Some(record)
        } else {
            None
        }; // Portable imports retain external references, not an invented operation.
        Ok(CloseoutHistory {
            location,
            operation: reference.operation,
            record,
            preservation_paths: reference.preservation_paths,
            report: reference.report,
        })
    }

    pub(crate) fn reject_closeout_control_paths(
        &self,
        connection: &Connection,
        cwd: Option<&Path>,
        targets: &[PathBuf],
    ) -> Result<()> {
        let mut query = connection.prepare("SELECT body FROM operations WHERE kind='closeout' AND state IN ('pending','recovery_required')").map_err(io)?;
        for row in query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(io)?
        {
            let stored: StoredCloseout = decode(&row.map_err(io)?)?;
            if let Some(removal) = stored.removal {
                for root in [&removal.quarantine, &removal.holding] {
                    if cwd.is_some_and(|path| path.starts_with(root))
                        || targets
                            .iter()
                            .any(|path| path.starts_with(root) || root.starts_with(path))
                    {
                        return Err(issue(
                            IssueCode::LiveWork,
                            format!(
                                "Closeout {} owns this recovery path; use its operation controls",
                                stored.record.operation
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Invoked only over an already-authorized review. An error leaves the
    /// exact journal, retained bytes and Closing gate available for inspection.
    #[cfg(target_os = "macos")]
    pub async fn finish_closeout(
        &self,
        operation: OperationId,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<CloseoutRecord> {
        let _operation = self.closeout_lease(operation)?;
        let mut stored = load(&self.connection()?, operation)?;
        if stored.record.stage == CloseoutStage::Closed {
            return Ok(stored.record);
        }
        runtime.check_stop()?;
        let authorization = stored.record.authorization.as_ref().ok_or_else(|| {
            issue(
                IssueCode::PermissionRequired,
                "Closeout has no current removal authorization",
            )
        })?;
        let review = stored
            .review
            .as_ref()
            .ok_or_else(|| {
                issue(
                    IssueCode::PermissionRequired,
                    "Closeout has no current approved review",
                )
            })?
            .clone();
        if authorization.review != review.id
            || authorization.seal != verification::seal(&stored, &review)?
        {
            return Err(issue(
                IssueCode::Conflict,
                "Closeout authorization no longer matches its evidence",
            ));
        }
        if !matches!(
            stored.record.stage,
            CloseoutStage::Authorized | CloseoutStage::Removing | CloseoutStage::RecoveryRequired
        ) {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Closeout is not authorized for removal",
            ));
        }
        self.checkpoint("closeout_before_removal_lease")?;
        let _root = self.acquire_recorded_binding(&stored.binding)?;
        let current = load(&self.connection()?, operation)?;
        require_current(&self.connection()?, &current, stored.record.revision)?;
        if current.record.authorization != stored.record.authorization {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Removal authority changed before physical ownership was acquired",
            ));
        }
        if stored.removal.is_none() {
            self.validate_closeout_review(&stored, &review, runtime)
                .await?;
            let snapshots: Vec<git::RepositorySnapshot> = storage::read_json(
                stored
                    .history
                    .as_ref()
                    .ok_or_else(|| corrupt("Git history missing"))?,
            )?;
            let worktree = snapshots
                .iter()
                .find(|snapshot| {
                    snapshot.root == stored.binding.observed_path()
                        && snapshot.git_directory != snapshot.common_directory
                })
                .map(|snapshot| -> Result<_> {
                    Ok(WorktreeRetirement {
                        administration: self
                            .resolver
                            .bind_directory(&snapshot.git_directory)
                            .map_err(io)?,
                        common: self
                            .resolver
                            .bind_directory(&snapshot.common_directory)
                            .map_err(io)?,
                        snapshot: snapshot.administration.clone(),
                        started: false,
                        complete: false,
                    })
                })
                .transpose()?;
            let parent = self
                .resolver
                .bind_directory(
                    stored
                        .binding
                        .observed_path()
                        .parent()
                        .ok_or_else(|| corrupt("Checkout has no parent"))?,
                )
                .map_err(io)?;
            let base = parent.observed_path();
            stored.removal = Some(Removal {
                quarantine: base.join(format!(".jcode-closeout-{operation}")),
                holding: base.join(format!(".jcode-closeout-holding-{operation}")),
                parent,
                holding_binding: None,
                quarantined: None,
                pending: None,
                root_removed: false,
                worktree,
            });
            stored.record.stage = CloseoutStage::Removing;
            self.save_removal(&mut stored)?;
            self.checkpoint("closeout_removal_intent")?;
        }
        let result = self.remove_journaled(&mut stored, runtime).await;
        match result {
            Ok(()) => self.closeout_backup(stored.record),
            Err(error) => {
                // A failed write may have committed, or may have left the last
                // effect receipt pending. Never invent progress from the live
                // candidate or regress an already-published Closed outcome.
                stored = load(&self.connection()?, operation)?;
                if stored.record.stage == CloseoutStage::Closed {
                    return self.closeout_backup(stored.record);
                }
                stored.record.stage = CloseoutStage::RecoveryRequired;
                stored.record.issues = vec![error.clone()];
                self.save_removal(&mut stored)?;
                Err(issue(
                    error.code,
                    format!(
                        "Closeout {operation} retains its removal journal and {} completed entries: {}",
                        stored.record.removed_entries, error.detail
                    ),
                ))
            }
        }
    }

    fn save_removal(&self, stored: &mut StoredCloseout) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let current = load(&transaction, stored.record.operation)?;
        require_current(&transaction, &current, stored.record.revision)?;
        if current.record.authorization != stored.record.authorization {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Removal authorization changed",
            ));
        }
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        save(&transaction, stored)?;
        transaction.commit().map_err(io)
    }

    #[cfg(target_os = "macos")]
    async fn remove_journaled(
        &self,
        stored: &mut StoredCloseout,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<()> {
        verification::preservation(stored)?;
        let mut findings = Vec::new();
        runtime.observe_internal(self, stored, &mut findings)?;
        if !findings.is_empty() {
            return Err(issue(
                IssueCode::LiveWork,
                format!("Recovery paths have dependent work: {findings:?}"),
            ));
        }
        let destination = self
            .resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        if destination.relocated {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Preservation volume moved",
            ));
        }
        let removal = stored
            .removal
            .as_ref()
            .ok_or_else(|| corrupt("Removal intent missing"))?
            .clone();
        let parent = self
            .resolver
            .resolve_directory(&removal.parent)
            .map_err(io)?;
        if parent.relocated {
            return Err(issue(IssueCode::ReplacedRoot, "Removal parent moved"));
        }
        // No checkout descriptor is held during OS inspection, so an open
        // descriptor from this process is also a real blocker, not ignored.
        if removal.quarantined.is_none() {
            if present(&removal.quarantine)? {
                let binding = self
                    .resolver
                    .bind_directory(&removal.quarantine)
                    .map_err(io)?;
                if binding.root_witness() != stored.binding.root_witness()
                    || binding.volume() != stored.binding.volume()
                {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Quarantine belongs to another physical root; nothing was removed",
                    ));
                }
                stored.removal.as_mut().unwrap().quarantined = Some(binding);
                stored.record.quarantine = Some(removal.quarantine.clone());
                self.save_removal(stored)?;
            } else {
                let review = stored
                    .review
                    .as_ref()
                    .ok_or_else(|| corrupt("Approved review missing"))?;
                self.validate_closeout_review(stored, review, runtime)
                    .await?;
                runtime.check_stop()?;
                let binding = self
                    .resolver
                    .publish_empty_child(
                        &removal.parent,
                        &stored.binding,
                        removal.quarantine.file_name().unwrap(),
                    )
                    .map_err(io)?;
                self.checkpoint("closeout_quarantine_renamed")?;
                stored.removal.as_mut().unwrap().quarantined = Some(binding);
                stored.record.quarantine = Some(removal.quarantine.clone());
                self.save_removal(stored)?;
            }
        }
        if present(stored.binding.observed_path())? {
            return Err(issue(
                IssueCode::Conflict,
                "Original checkout path is occupied; its replacement is retained",
            ));
        }
        if stored.record.removed_entries == 0 && stored.removal.as_ref().unwrap().pending.is_none()
        {
            inventory::visit(stored, |item| {
                if let Some(expected) = &item.witness {
                    let path = removal.quarantine.join(&item.entry.path);
                    let mut observed = Witness::of(&std::fs::symlink_metadata(&path).map_err(io)?)?;
                    if item.entry.path.as_os_str().is_empty() {
                        observed.changed = expected.changed;
                    }
                    if observed != *expected {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Captured checkout changed before deletion",
                        ));
                    }
                    if item.entry.kind == CloseoutEntryKind::File
                        && Some(backup::file_digest(&path)?) != item.entry.sha256
                    {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Captured file changed before deletion",
                        ));
                    }
                    if item.entry.kind == CloseoutEntryKind::Symlink
                        && Some(std::fs::read_link(&path).map_err(io)?) != item.entry.link_target
                    {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Captured symlink changed before deletion",
                        ));
                    }
                }
                Ok(())
            })?;
        }
        if !stored.removal.as_ref().unwrap().root_removed {
            let quarantine_binding = stored
                .removal
                .as_ref()
                .unwrap()
                .quarantined
                .as_ref()
                .unwrap();
            // The final root can be in the pending holding slot after a crash.
            let root_in_slot = stored
                .removal
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .is_some_and(|p| p.item.entry.path.as_os_str().is_empty())
                && !present(&removal.quarantine)?;
            if !root_in_slot {
                let current = self
                    .resolver
                    .resolve_directory(quarantine_binding)
                    .map_err(io)?;
                if current.relocated {
                    return Err(issue(IssueCode::ReplacedRoot, "Quarantine moved"));
                }
                let findings = work::external_work(
                    self,
                    stored.record.operation,
                    &removal.quarantine,
                    runtime.capture,
                )
                .await?;
                if !findings.is_empty() {
                    return Err(issue(
                        IssueCode::LiveWork,
                        "Quarantine still has open files or process working directories",
                    ));
                }
            }
            runtime.check_stop()?;
            if present(&removal.holding)? {
                for entry in std::fs::read_dir(&removal.holding).map_err(io)? {
                    if entry.map_err(io)?.file_name() != "entry" {
                        return Err(issue(
                            IssueCode::Conflict,
                            "Unexpected data in holding directory is retained",
                        ));
                    }
                }
            }
            self.retire_worktree(stored, runtime).await?;
            if present(&removal.holding)?
                && !work::external_work(
                    self,
                    stored.record.operation,
                    &removal.holding,
                    runtime.capture,
                )
                .await?
                .is_empty()
            {
                return Err(issue(
                    IssueCode::LiveWork,
                    "Holding entries still have open files or process working directories",
                ));
            }
            if stored.removal.as_ref().unwrap().holding_binding.is_none() {
                // An unjournaled competing directory is never adopted by name.
                let binding = self
                    .resolver
                    .create_empty_child(&removal.parent, removal.holding.file_name().unwrap())
                    .map_err(io)?;
                self.checkpoint("closeout_holding_created")?;
                stored.removal.as_mut().unwrap().holding_binding = Some(binding);
                self.save_removal(stored)?;
            }
            let holding_binding = stored
                .removal
                .as_ref()
                .unwrap()
                .holding_binding
                .as_ref()
                .unwrap();
            if self
                .resolver
                .resolve_directory(holding_binding)
                .map_err(io)?
                .relocated
            {
                return Err(issue(IssueCode::ReplacedRoot, "Holding directory moved"));
            }
            let holding = VerifiedDirectory::open(removal.holding.clone()).map_err(io)?;
            if self
                .resolver
                .resolve_directory(holding_binding)
                .map_err(io)?
                .relocated
            {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Holding volume moved while opening",
                ));
            }
            holding.verify().map_err(io)?;
            let root_guard = if present(&removal.quarantine)? {
                let pinned = VerifiedDirectory::open(removal.quarantine.clone()).map_err(io)?;
                if self
                    .resolver
                    .resolve_directory(
                        stored
                            .removal
                            .as_ref()
                            .unwrap()
                            .quarantined
                            .as_ref()
                            .unwrap(),
                    )
                    .map_err(io)?
                    .relocated
                {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Quarantine volume moved while opening",
                    ));
                }
                pinned.verify().map_err(io)?;
                Some(pinned)
            } else {
                None
            };
            let inventory = inventory::source(stored)?;
            if backup::file_digest(&inventory.manifest)? != inventory.digest {
                return Err(corrupt("Removal inventory changed"));
            }
            let mut sequence = 0;
            visit_reverse(&inventory.manifest, |item| {
                if item.witness.is_none() {
                    return Ok(());
                }
                sequence += 1;
                if sequence <= stored.record.removed_entries {
                    return Ok(());
                }
                runtime.check_stop()?;
                self.remove_entry(stored, item, &holding, root_guard.as_ref())?;
                Ok(())
            })?;
            if sequence != stored.record.removed_entries {
                return Err(corrupt("Removal progress exceeds inventory"));
            }
        }
        self.publish_closed(stored)
    }

    #[cfg(target_os = "macos")]
    async fn retire_worktree(
        &self,
        stored: &mut StoredCloseout,
        runtime: &CloseoutRuntime<'_>,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let Some(worktree) = stored.removal.as_ref().and_then(|r| r.worktree.clone()) else {
            return Ok(());
        };
        if worktree.complete {
            return Ok(());
        }
        let _administration = self.acquire_recorded_binding(&worktree.administration)?;
        let common = self
            .resolver
            .resolve_directory(&worktree.common)
            .map_err(io)?;
        if common.relocated {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Shared Git repository moved",
            ));
        }
        let directory = self
            .root
            .join("closeout-git-retirement")
            .join(stored.record.operation.to_string());
        storage::private_dir(&directory)?;
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
            let findings = work::external_work(
                self,
                stored.record.operation,
                worktree.administration.observed_path(),
                runtime.capture,
            )
            .await?;
            if !findings.is_empty() {
                return Err(issue(
                    IssueCode::LiveWork,
                    "Worktree administration is in use",
                ));
            }
            stored
                .removal
                .as_mut()
                .unwrap()
                .worktree
                .as_mut()
                .unwrap()
                .started = true;
            self.save_removal(stored)?;
            self.checkpoint("closeout_worktree_retirement_intent")?;
            let original = stored.binding.observed_path().to_str().ok_or_else(|| {
                issue(
                    IssueCode::UnsupportedCapability,
                    "Git worktree path is not representable by this command adapter",
                )
            })?;
            // Git owns worktree registration. No force/prune operation, shared
            // common-directory recursion or guessed administrative path removal.
            git::run(
                self,
                stored.record.operation,
                &common.path,
                &["--git-dir=.", "worktree", "remove", "--", original],
                None,
                &directory.join(format!("remove-{}", RequestId::new())),
                runtime.capture,
            )
            .await?;
            self.checkpoint("closeout_worktree_retired")?;
        } else if !worktree.started {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Worktree administration disappeared before journaled retirement",
            ));
        }
        if present(worktree.administration.observed_path())? {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Git did not retire the reviewed worktree administration",
            ));
        }
        let listing = directory.join(format!("verify-{}", RequestId::new()));
        git::run(
            self,
            stored.record.operation,
            &common.path,
            &["--git-dir=.", "worktree", "list", "--porcelain", "-z"],
            None,
            &listing,
            runtime.capture,
        )
        .await?;
        if std::fs::read(listing)
            .map_err(io)?
            .split(|b| *b == 0)
            .any(|field| {
                field.strip_prefix(b"worktree ")
                    == Some(stored.binding.observed_path().as_os_str().as_bytes())
            })
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Git still lists the removed worktree",
            ));
        }
        stored
            .removal
            .as_mut()
            .unwrap()
            .worktree
            .as_mut()
            .unwrap()
            .complete = true;
        self.save_removal(stored)
    }

    #[cfg(target_os = "macos")]
    fn remove_entry(
        &self,
        stored: &mut StoredCloseout,
        item: Item,
        holding: &VerifiedDirectory,
        root_guard: Option<&VerifiedDirectory>,
    ) -> Result<()> {
        let removal = stored.removal.as_ref().unwrap().clone();
        let original = removal.quarantine.join(&item.entry.path);
        let slot = removal.holding.join("entry");
        if present(&removal.quarantine)? {
            root_guard
                .ok_or_else(|| corrupt("Quarantine lacks a retained descriptor"))?
                .verify()
                .map_err(io)?;
        }
        if let Some(pending) = &removal.pending {
            if pending.item.entry.id != item.entry.id {
                return Err(corrupt("Removal cursor does not match pending entry"));
            }
        } else {
            if present(&slot)? {
                return Err(issue(
                    IssueCode::Conflict,
                    "Unjournaled holding entry is retained",
                ));
            }
            verify_entry(&item, &original, false)?;
            stored.removal.as_mut().unwrap().pending = Some(Pending {
                item: item.clone(),
                captured: false,
            });
            self.save_removal(stored)?;
            self.checkpoint("closeout_entry_intent")?;
        }
        if present(&original)? && present(&slot)? {
            return Err(issue(
                IssueCode::Conflict,
                "Original and captured entry both exist; replacement retained",
            ));
        }
        if !present(&slot)?
            && !stored
                .removal
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .captured
        {
            verify_entry(&item, &original, false)?;
            let parent = if item.entry.path.as_os_str().is_empty() {
                VerifiedDirectory::open(removal.parent.observed_path().into()).map_err(io)?
            } else {
                let root =
                    root_guard.ok_or_else(|| corrupt("Quarantine descriptor unavailable"))?;
                root.preservation_directory(
                    item.entry.path.parent().unwrap_or(Path::new("")),
                    false,
                )
                .map_err(io)?
            };
            parent
                .capture_entry(
                    original
                        .file_name()
                        .ok_or_else(|| corrupt("Removal entry has no name"))?,
                    holding,
                    OsStr::new("entry"),
                )
                .map_err(io)?;
            self.checkpoint("closeout_entry_captured")?;
        }
        if present(&slot)? {
            let metadata = verify_entry(&item, &slot, true)?;
            stored
                .removal
                .as_mut()
                .unwrap()
                .pending
                .as_mut()
                .unwrap()
                .captured = true;
            self.save_removal(stored)?;
            holding
                .unlink_captured_entry(OsStr::new("entry"), &metadata)
                .map_err(io)?;
            self.checkpoint("closeout_entry_unlinked")?;
        } else if !stored
            .removal
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .captured
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Entry and holding slot are missing without captured ownership",
            ));
        }
        if present(&original)? {
            return Err(issue(
                IssueCode::Conflict,
                "New data appeared at the removed entry; it is retained",
            ));
        }
        stored.record.removed_entries += 1;
        stored.removal.as_mut().unwrap().pending = None;
        if item.entry.path.as_os_str().is_empty() {
            stored.removal.as_mut().unwrap().root_removed = true;
        }
        self.save_removal(stored)
    }

    #[cfg(target_os = "macos")]
    fn publish_closed(&self, stored: &mut StoredCloseout) -> Result<()> {
        let removal = stored.removal.as_ref().unwrap();
        if !removal.root_removed
            || present(stored.binding.observed_path())?
            || present(&removal.quarantine)?
        {
            return Err(issue(
                IssueCode::Conflict,
                "Checkout absence has not been established",
            ));
        }
        if present(&removal.holding)? {
            let binding = removal
                .holding_binding
                .as_ref()
                .ok_or_else(|| corrupt("Holding identity missing"))?;
            if self
                .resolver
                .resolve_directory(binding)
                .map_err(io)?
                .relocated
                || self
                    .resolver
                    .resolve_directory(&removal.parent)
                    .map_err(io)?
                    .relocated
            {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Holding directory or its volume moved before cleanup",
                ));
            }
            let parent =
                VerifiedDirectory::open(removal.parent.observed_path().into()).map_err(io)?;
            let metadata = std::fs::symlink_metadata(&removal.holding).map_err(io)?;
            parent
                .unlink_captured_entry(removal.holding.file_name().unwrap(), &metadata)
                .map_err(io)?;
        }
        self.checkpoint("closeout_removed_before_closed")?;
        let report = self.root.join("closeout-reports");
        storage::private_dir(&report)?;
        let report = report.join(format!("{}.json", stored.record.operation));
        storage::atomic_json(
            &report,
            &serde_json::json!({
                "kind": "checkout_removal_evidence", "operation": stored.record.operation,
                "observed_at": chrono::Utc::now().to_rfc3339(), "binding": stored.binding,
                "authorization": stored.record.authorization, "references": stored.references,
                "preservation_manifest": stored.preservation, "preservation_digest": stored.record.preservation_digest,
                "completed_entries": stored.record.removed_entries, "original_and_quarantine_absent": true
            }),
        )?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(
            &transaction,
            &load(&transaction, stored.record.operation)?,
            stored.record.revision,
        )?;
        let Entity::Location(mut location) = entity(
            &transaction,
            EntityId::Location(stored.record.spec.location),
        )?
        else {
            return Err(corrupt("Closed location missing"));
        };
        if location.lifecycle != LocationLifecycle::Closing {
            return Err(issue(IssueCode::Conflict, "Location closing gate changed"));
        }
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        stored.record.stage = CloseoutStage::Closed;
        stored.record.issues.clear();
        location.lifecycle = LocationLifecycle::Closed;
        location.revision = stored.record.revision;
        organization::save_entity(&transaction, &Entity::Location(location))?;
        transaction
            .execute(
                "UPDATE bindings SET live_key=NULL WHERE location=?1",
                [stored.record.spec.location.to_string()],
            )
            .map_err(io)?;
        let reference = portable::ClosedReference {
            location: stored.record.spec.location,
            operation: stored.record.operation,
            preservation_paths: vec![stored.record.preservation_directory.clone()],
            report: Some(report),
        };
        transaction
            .execute(
                "INSERT INTO closed_history VALUES(?1,?2)",
                params![reference.location.to_string(), encode(&reference)?],
            )
            .map_err(io)?;
        save(&transaction, stored)?;
        transaction.commit().map_err(io)
    }
}

fn present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io(error)),
    }
}

fn verify_entry(item: &Item, path: &Path, captured: bool) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    let current = Witness::of(&metadata)?;
    let expected = item
        .witness
        .as_ref()
        .ok_or_else(|| corrupt("Removal entry has no witness"))?;
    if current.device != expected.device
        || current.inode != expected.inode
        || current.created != expected.created
        || current.owner != expected.owner
        || current.mode != expected.mode
    {
        return Err(issue(
            IssueCode::ReplacedRoot,
            format!("Removal entry identity changed: {}", path.display()),
        ));
    }
    match item.entry.kind {
        CloseoutEntryKind::Directory => {
            if std::fs::read_dir(path).map_err(io)?.next().is_some() {
                return Err(issue(
                    IssueCode::Conflict,
                    format!("Unexpected entries remain in {}", path.display()),
                ));
            }
        }
        CloseoutEntryKind::File => {
            if current.bytes != expected.bytes
                || current.modified != expected.modified
                || current.links > expected.links
                || Some(backup::file_digest(path)?) != item.entry.sha256
            {
                return Err(issue(
                    IssueCode::Conflict,
                    "Removal file contents or aliases changed",
                ));
            }
            if !captured && expected.links == 1 && current.changed != expected.changed {
                return Err(issue(IssueCode::Conflict, "Removal file metadata changed"));
            }
        }
        CloseoutEntryKind::Symlink => {
            if Some(std::fs::read_link(path).map_err(io)?) != item.entry.link_target {
                return Err(issue(IssueCode::Conflict, "Removal symlink changed"));
            }
        }
        _ => {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Special or mounted entries cannot be removed",
            ));
        }
    }
    if Witness::of(&std::fs::symlink_metadata(path).map_err(io)?)? != current {
        return Err(issue(
            IssueCode::Conflict,
            "Removal entry changed while verifying",
        ));
    }
    Ok(metadata)
}

/// JSONL reverse traversal retains at most one line plus a fixed read buffer.
fn visit_reverse(path: &Path, mut visitor: impl FnMut(Item) -> Result<()>) -> Result<()> {
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut position = file.metadata().map_err(io)?.len();
    let mut pending = Vec::new();
    while position != 0 {
        let length = position.min(65536) as usize;
        position -= length as u64;
        file.seek(SeekFrom::Start(position)).map_err(io)?;
        let mut chunk = vec![0; length];
        file.read_exact(&mut chunk).map_err(io)?;
        chunk.extend_from_slice(&pending);
        pending = chunk;
        while let Some(index) = pending.iter().rposition(|byte| *byte == b'\n') {
            let line = pending.split_off(index + 1);
            pending.truncate(index);
            if !line.is_empty() {
                visitor(serde_json::from_slice(&line).map_err(corrupt)?)?;
            }
        }
    }
    if !pending.is_empty() {
        visitor(serde_json::from_slice(&pending).map_err(corrupt)?)?;
    }
    Ok(())
}
