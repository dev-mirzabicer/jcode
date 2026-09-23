use super::*;
use jcode_tool_core::OutputCapture;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PreservationManifest {
    pub operation: OperationId,
    pub inventory_digest: String,
    pub decisions_digest: String,
    pub files: PathBuf,
    pub files_digest: String,
    pub bundles: Vec<(PathBuf, String)>,
    pub git_manifests: Vec<(PathBuf, String)>,
    pub references: (PathBuf, String),
}

impl WorkspaceService {
    /// Preservation success is not a removal authorization. The final closing
    /// gate and current-work review must still establish quiescence.
    pub async fn preserve_closeout(
        &self,
        operation: OperationId,
        expected: Revision,
        capture: &dyn OutputCapture,
    ) -> Result<CloseoutRecord> {
        let _operation = self.closeout_lease(operation)?;
        let mut stored = { load(&self.connection()?, operation)? };
        require_current(&self.connection()?, &stored, expected)?;
        inventory::require_preparation(&stored)?;
        let history = stored.history.as_ref().ok_or_else(|| {
            issue(
                IssueCode::IncompleteCapture,
                "Refresh the complete checkout inventory before preservation",
            )
        })?;
        if Some(backup::file_digest(history)?) != stored.history_digest {
            return Err(corrupt("Closeout Git inventory integrity changed"));
        }
        let snapshots: Vec<git::RepositorySnapshot> = storage::read_json(history)?;
        let references = stored.references.clone().ok_or_else(|| {
            issue(
                IssueCode::IncompleteCapture,
                "Refresh checkout references before preservation",
            )
        })?;
        if backup::file_digest(&references.0)? != references.1 {
            return Err(corrupt("Closeout reference inventory integrity changed"));
        }
        let _root = self.acquire_binding(&stored.binding)?;
        let destination = self
            .resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        if destination.relocated {
            return Err(issue(
                IssueCode::OfflineVolume,
                "Preservation destination moved; review its binding before proceeding",
            ));
        }
        let destination_archive = archive::Archive::open(&destination.path)?;
        self.resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        destination_archive.verify()?;
        inventory::verify_source(&stored)?;
        let directory = stored
            .record
            .preservation_directory
            .join(format!("capture-{}", RequestId::new()));
        let archive = destination_archive.subtree(&directory)?;
        {
            let mut connection = self.connection()?;
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
            stored.record.stage = CloseoutStage::Preserving;
            stored.record.preservation_digest = None;
            stored.preservation = None;
            stored.final_approval = None;
            stored.no_loss = None;
            save(&transaction, &stored)?;
            transaction.commit().map_err(io)?;
        }
        let revision = stored.record.revision;
        let mut bundles = Vec::new();
        let mut git_manifests = Vec::new();
        for (index, snapshot) in snapshots.iter().enumerate() {
            let path = git::preserve(
                self,
                operation,
                snapshot,
                &directory.join(format!("git-{index}")),
                capture,
                &archive,
            )
            .await?;
            bundles.push((path.clone(), backup::file_digest(&path)?));
            for name in ["lfs.json", "verified-refs.json", "administration.json"] {
                let manifest = path
                    .parent()
                    .ok_or_else(|| corrupt("Git preservation has no directory"))?
                    .join(name);
                if manifest.try_exists().map_err(io)? {
                    git_manifests.push((manifest.clone(), backup::file_digest(&manifest)?));
                }
            }
        }
        let files = files::preserve_in(&stored, &directory, &archive)?;
        self.resolver
            .resolve_directory(&stored.destination)
            .map_err(io)?;
        self.resolver
            .resolve_directory(&stored.binding)
            .map_err(io)?;
        inventory::verify_source(&stored)?;
        let manifest = PreservationManifest {
            operation,
            inventory_digest: stored
                .record
                .inventory_digest
                .clone()
                .ok_or_else(|| corrupt("Missing inventory identity"))?,
            decisions_digest: digest(encode(&stored.decisions)?.as_bytes()),
            files_digest: backup::file_digest(&files)?,
            files,
            bundles,
            git_manifests,
            references,
        };
        let path = directory.join("manifest.json");
        archive.json(&path, &manifest)?;
        let hash = backup::file_digest(&path)?;
        self.checkpoint("closeout_preservation_written")?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(&transaction, &load(&transaction, operation)?, revision)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        stored.record.preservation_digest = Some(hash);
        stored.record.stage = CloseoutStage::NeedsDecision;
        stored.preservation = Some(path);
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }
}
