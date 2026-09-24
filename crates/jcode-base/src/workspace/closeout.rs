//! One catalog-owned closeout journal. The authorization is bound to a Location,
//! physical generation and operation. Inspection never authorizes removal.
use super::*;
use rusqlite::{TransactionBehavior, params};
use std::collections::BTreeMap;
mod actions;
pub use actions::{CLOSEOUT_EXECUTION_SESSION, CLOSEOUT_EXECUTION_TOOL};

#[cfg(unix)]
mod approval;
mod archive;
#[cfg(unix)]
mod verification;
#[cfg(unix)]
pub use work::CloseoutRuntime;
mod files;
#[cfg(unix)]
mod git;
mod inventory;
#[cfg(unix)]
mod preservation;
mod references;
mod removal;
#[cfg(all(test, target_os = "macos"))]
mod tests;
#[cfg(unix)]
mod work;

#[derive(Clone, Serialize, Deserialize)]
struct StoredCloseout {
    record: CloseoutRecord,
    installation: InstallationId,
    binding: PhysicalBinding,
    destination: PhysicalBinding,
    inventory: Option<PathBuf>,
    history: Option<PathBuf>,
    history_digest: Option<String>,
    references: Option<(PathBuf, String)>,
    preservation: Option<PathBuf>,
    decisions: BTreeMap<String, CloseoutDecision>,
    #[serde(default)]
    review: Option<CloseoutReview>,
    #[serde(default)]
    removal: Option<removal::Removal>,
}

impl WorkspaceService {
    fn fence_closeout(&self, operation: OperationId, expected: Revision) -> Result<CloseoutRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let observed = load(&connection, operation)?;
        require_current(&connection, &observed, expected)?;
        let resolved = self
            .resolver
            .resolve_directory(&observed.binding)
            .map_err(io)?;
        if resolved.relocated {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Checkout moved; review its current binding before closeout preparation",
            ));
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut stored = load(&transaction, operation)?;
        require_current(&transaction, &stored, expected)?;
        inventory::require_preparation(&stored)?;
        let Entity::Location(mut location) = entity(
            &transaction,
            EntityId::Location(stored.record.spec.location),
        )?
        else {
            return Err(corrupt("Closeout lost its checkout"));
        };
        if location.lifecycle == LocationLifecycle::Closing {
            return Ok(stored.record);
        }
        if location.lifecycle != LocationLifecycle::Ready || location.retired {
            return Err(issue(
                IssueCode::Conflict,
                "Checkout is not available for closing",
            ));
        }
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        location.lifecycle = LocationLifecycle::Closing;
        location.revision = stored.record.revision;
        organization::save_entity(&transaction, &Entity::Location(location))?;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }
    fn closeout_lease(&self, operation: OperationId) -> Result<std::fs::File> {
        let directory = self.root.join("leases");
        storage::private_dir(&directory)?;
        let file = storage::private_file(&directory.join(format!("closeout-{operation}")), false)?;
        file.try_lock().map_err(|e| {
            issue(
                IssueCode::Busy,
                format!("Closeout has an active owner: {e}"),
            )
        })?;
        Ok(file)
    }
    pub fn begin_closeout(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        expected: Revision,
        spec: CloseoutSpec,
    ) -> Result<CloseoutRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let input = digest(encode(&("begin_closeout", expected, &spec))?.as_bytes());
        if let Some(receipt) = organization::replay(&connection, request, &input)? {
            return Ok(load(&connection, receipt.operation)?.record);
        }
        organization::require_revision(&connection, expected)?;
        let Entity::Location(location) = entity(&connection, EntityId::Location(spec.location))?
        else {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Closeout needs a checkout",
            ));
        };
        if !matches!(location.kind, LocationKind::Checkout { .. })
            || location.lifecycle != LocationLifecycle::Ready
            || location.retired
            || location.binding_generation != spec.expected_generation
        {
            return Err(issue(
                IssueCode::Conflict,
                "Closeout requires the reviewed Ready checkout generation",
            ));
        }
        let binding = self.verify_writable_root(&connection, &location)?;
        let base = match &spec.preservation_directory {
            Some(path) => path.clone(),
            None => {
                let path = self.root.join("closed");
                storage::private_dir(&path)?;
                path
            }
        };
        let destination = self.resolver.bind_directory(&base).map_err(io)?;
        let preservation_volume_ownership = crate::location::native_files::VerifiedDirectory::open(
            destination.observed_path().into(),
        )
        .and_then(|directory| directory.volume_ownership_enforced())
        .map_err(io)?;
        if destination
            .observed_path()
            .starts_with(binding.observed_path())
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Preservation destination is inside the checkout being removed",
            ));
        }
        // Merely recording closeout intent does not stop existing work. An
        // exclusive physical lease is acquired at the final closing boundary.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        organization::require_revision(&transaction, expected)?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations o JOIN operation_targets t ON t.operation=o.id WHERE o.kind='closeout' AND t.target=?1 AND o.state IN ('pending','recovery_required'))",
            [location.id.to_string()], |r| r.get(0),
        ).map_err(io)?;
        if active {
            return Err(issue(
                IssueCode::Busy,
                "Checkout already has an unfinished closeout; inspect its operation",
            ));
        }
        let receipt = organization::commit_receipt(
            &transaction,
            request,
            &input,
            vec![EntityId::Location(location.id)],
        )?;
        let record = CloseoutRecord {
            operation: receipt.operation,
            request,
            spec,
            revision: receipt.revision,
            stage: CloseoutStage::Preparing,
            initiated_by: client.0.clone(),
            inventory_digest: None,
            inventory_entries: 0,
            preservation_directory: destination
                .observed_path()
                .join(location.id.to_string())
                .join(receipt.operation.to_string()),
            preservation_digest: None,
            preservation_volume_ownership,
            authorization: None,
            quarantine: None,
            removed_entries: 0,
            issues: vec![],
        };
        let stored = StoredCloseout {
            record: record.clone(),
            installation: storage::status(&transaction)?.installation,
            binding,
            destination,
            inventory: None,
            history: None,
            history_digest: None,
            references: None,
            preservation: None,
            decisions: BTreeMap::new(),
            review: None,
            removal: None,
        };
        save(&transaction, &stored)?;
        transaction
            .execute(
                "INSERT INTO operation_targets VALUES(?1,?2)",
                params![receipt.operation.to_string(), location.id.to_string()],
            )
            .map_err(io)?;
        self.checkpoint("closeout_begin_before_commit")?;
        transaction.commit().map_err(io)?;
        self.checkpoint("closeout_begin_committed")?;
        self.closeout_backup(record)
    }

    pub fn inspect_closeout(&self, operation: OperationId) -> Result<CloseoutRecord> {
        let _catalog = self.lease(false)?;
        Ok(load(&self.connection()?, operation)?.record)
    }

    /// Revocation is a human control, not an agent-supplied authorization flag.
    /// Once destructive work starts, pausing needs the removal owner rather
    /// than pretending already removed bytes can be rolled back.
    pub fn revoke_closeout(
        &self,
        client: &WorkspaceClientAuthority,
        request: RequestId,
        operation: OperationId,
        expected: Revision,
    ) -> Result<CloseoutRecord> {
        let _catalog = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let input = digest(encode(&("revoke_closeout", operation, expected))?.as_bytes());
        if organization::replay(&transaction, request, &input)?.is_some() {
            return Ok(load(&transaction, operation)?.record);
        }
        let mut stored = load(&transaction, operation)?;
        require_authorization_revision(&transaction, &stored, expected)?;
        if stored.removal.is_some()
            || matches!(
                stored.record.stage,
                CloseoutStage::Removing | CloseoutStage::Closed | CloseoutStage::Retained
            )
        {
            return Err(issue(
                IssueCode::Conflict,
                "Removal has begun; inspect its exact journaled outcome",
            ));
        }
        let receipt = organization::commit_receipt(
            &transaction,
            request,
            &input,
            vec![EntityId::Location(stored.record.spec.location)],
        )?;
        stored.record.stage = CloseoutStage::Revoked;
        stored.record.revision = receipt.revision;
        stored.record.authorization = None;
        stored.review = None;
        if let Entity::Location(mut location) = entity(
            &transaction,
            EntityId::Location(stored.record.spec.location),
        )? && location.lifecycle == LocationLifecycle::Closing
            && location.binding_generation == stored.binding.generation()
            && location.observed_path == stored.binding.observed_path()
        {
            location.lifecycle = LocationLifecycle::Ready;
            location.revision = receipt.revision;
            organization::save_entity(&transaction, &Entity::Location(location))?;
        }
        transaction
            .execute(
                "INSERT INTO operations VALUES(?1,'closeout_revocation','complete',?2)",
                params![
                    receipt.operation.to_string(),
                    encode(&(operation, &client.0))?
                ],
            )
            .map_err(io)?;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }

    fn closeout_backup(&self, mut record: CloseoutRecord) -> Result<CloseoutRecord> {
        if let Err(error) = self.automatic_backup() {
            record.issues.push(issue(
                IssueCode::BackupFailed,
                format!("Closeout mutation committed; backup failed: {error}"),
            ));
        }
        Ok(record)
    }
}

fn load(connection: &Connection, operation: OperationId) -> Result<StoredCloseout> {
    let body: Option<(String, String)> = connection
        .query_row(
            "SELECT state,body FROM operations WHERE id=?1 AND kind='closeout'",
            [operation.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    let (state, body) =
        body.ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown closeout operation"))?;
    let mut stored: StoredCloseout = decode(&body)?;
    if stored.record.operation != operation {
        return Err(corrupt("Closeout identity mismatch"));
    }
    if state == "recovery_required" && stored.record.stage != CloseoutStage::RecoveryRequired {
        // Catalog restore invalidates pending effects without rewriting their
        // historical journal. Old approval must not become fresh authority.
        stored.record.stage = CloseoutStage::RecoveryRequired;
        stored.record.spec.conditional_no_loss = false;
        stored.record.authorization = None;
        stored.review = None;
        stored.record.issues.push(issue(IssueCode::RecoveryRequired, "Catalog recovery invalidated closeout authority; a trusted client must reconcile the retained operation"));
    }
    Ok(stored)
}

pub(super) fn invalidate_restored_authority(
    connection: &Connection,
    revision: Revision,
) -> Result<()> {
    let mut query = connection.prepare("SELECT body FROM operations WHERE kind='closeout' AND state IN ('pending','recovery_required')").map_err(io)?;
    let bodies = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(io)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(io)?;
    drop(query);
    for body in bodies {
        let mut stored: StoredCloseout = decode(&body)?;
        stored.record.authorization = None;
        stored.record.spec.conditional_no_loss = false;
        stored.review = None;
        stored.record.stage = CloseoutStage::RecoveryRequired;
        stored.record.revision = revision;
        stored.record.issues.push(issue(IssueCode::RecoveryRequired, "Catalog restore invalidated closeout authority; inspect retained state before a new trusted decision"));
        save(connection, &stored)?;
    }
    Ok(())
}

fn save(connection: &Connection, stored: &StoredCloseout) -> Result<()> {
    let state = match stored.record.stage {
        CloseoutStage::Closed | CloseoutStage::Retained => "complete",
        CloseoutStage::Revoked => "failed",
        CloseoutStage::RecoveryRequired => "recovery_required",
        _ => "pending",
    };
    connection.execute("INSERT INTO operations VALUES(?1,'closeout',?2,?3) ON CONFLICT(id) DO UPDATE SET state=excluded.state,body=excluded.body", params![stored.record.operation.to_string(), state, encode(stored)?]).map_err(io)?;
    Ok(())
}

fn require_current(
    connection: &Connection,
    stored: &StoredCloseout,
    expected: Revision,
) -> Result<()> {
    require_authorization_revision(connection, stored, expected)?;
    let Entity::Location(location) =
        entity(connection, EntityId::Location(stored.record.spec.location))?
    else {
        return Err(corrupt("Closeout lost its Location"));
    };
    if location.binding_generation != stored.binding.generation()
        || location.observed_path != stored.binding.observed_path()
    {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Checkout binding changed since authorization",
        ));
    }
    Ok(())
}

fn require_authorization_revision(
    connection: &Connection,
    stored: &StoredCloseout,
    expected: Revision,
) -> Result<()> {
    if stored.record.revision != expected
        || stored.installation != storage::status(connection)?.installation
    {
        return Err(issue(
            IssueCode::Conflict,
            "Closeout revision or installation changed",
        ));
    }
    Ok(())
}
