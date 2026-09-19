use super::*;
use std::fs::File;
use std::io::Read;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct BackupIntent {
    name: String,
    id: SnapshotId,
}
#[derive(Serialize, Deserialize)]
struct SnapshotPublication {
    snapshot: Snapshot,
    stage: PathBuf,
}

pub(super) fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(io)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(io)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(super) fn integrity(connection: &Connection) -> Result<()> {
    validate_schema(connection)?;
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(corrupt)?;
    if check != "ok" {
        return Err(corrupt(format!("Catalog integrity check failed: {check}")));
    }
    let broken: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .map_err(corrupt)?;
    if broken != 0 {
        return Err(corrupt("Catalog foreign-key references are broken"));
    }
    storage::status(connection)?;
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<()> {
    fn definitions(connection: &Connection) -> Result<Vec<(String, String, String)>> {
        let mut statement=connection.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name").map_err(corrupt)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(corrupt)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(corrupt)
    }
    let reference = Connection::open_in_memory().map_err(io)?;
    reference
        .execute_batch(include_str!("schema.sql"))
        .map_err(io)?;
    if definitions(connection)? != definitions(&reference)? {
        return Err(corrupt(
            "Catalog schema definitions differ from the supported version",
        ));
    }
    Ok(())
}
pub(super) fn copy_database(source: &Connection, destination: &Path) -> Result<()> {
    if destination.try_exists().map_err(io)? {
        return Err(issue(
            IssueCode::Conflict,
            "Snapshot destination already exists",
        ));
    }
    let mut target = storage::raw_connection(destination, true)?;
    {
        let backup = rusqlite::backup::Backup::new(source, &mut target).map_err(io)?;
        backup
            .run_to_completion(128, Duration::from_millis(5), None)
            .map_err(io)?;
    }
    target
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(io)?;
    integrity(&target)?;
    drop(target);
    File::open(destination)
        .and_then(|f| f.sync_all())
        .map_err(io)?;
    storage::sync_dir(
        destination
            .parent()
            .ok_or_else(|| io("Missing snapshot parent"))?,
    )
}

impl WorkspaceService {
    pub fn backup(&self, request: RequestId, name: String) -> Result<Snapshot> {
        let _lease = self.lease(false)?;
        let lock = storage::private_file(&self.root.join("snapshots.lock"), false)?;
        lock.lock().map_err(io)?;
        let receipt = self
            .root
            .join("snapshots")
            .join(format!("request-{request}.json"));
        if receipt.try_exists().map_err(io)? {
            let existing: BackupIntent = storage::read_json(&receipt)?;
            if existing.name != name {
                return Err(issue(
                    IssueCode::Conflict,
                    "Backup request already used with another name",
                ));
            }
            return self.publish_snapshot(&self.connection()?, existing.id, name, false);
        }
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(issue(
                IssueCode::InvalidInput,
                "Backup name must be nonempty text",
            ));
        }
        let id = SnapshotId::new();
        storage::atomic_json(
            &receipt,
            &BackupIntent {
                name: name.clone(),
                id,
            },
        )?;
        self.publish_snapshot(&self.connection()?, id, name, false)
    }
    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let _lease = self.lease(false)?;
        self.snapshots_locked()
    }
    pub(super) fn snapshots_locked(&self) -> Result<Vec<Snapshot>> {
        let mut snapshots = vec![];
        for entry in std::fs::read_dir(self.root.join("snapshots")).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if path
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.starts_with("snapshot-") && v.ends_with(".json"))
            {
                snapshots.push(storage::read_json::<Snapshot>(&path)?);
            }
        }
        snapshots.sort_by(|a, b| b.revision.cmp(&a.revision).then_with(|| b.id.cmp(&a.id)));
        Ok(snapshots)
    }
    pub(super) fn snapshot_locked(
        &self,
        connection: &Connection,
        name: String,
        automatic: bool,
    ) -> Result<Snapshot> {
        let id = SnapshotId::new();
        self.publish_snapshot(connection, id, name, automatic)
    }
    fn publish_snapshot(
        &self,
        connection: &Connection,
        id: SnapshotId,
        name: String,
        automatic: bool,
    ) -> Result<Snapshot> {
        let manifest = self
            .root
            .join("snapshots")
            .join(format!("snapshot-{id}.json"));
        if manifest.try_exists().map_err(io)? {
            let snapshot: Snapshot = storage::read_json(&manifest)?;
            drop(self.verify_snapshot(&snapshot)?);
            return Ok(snapshot);
        }
        let path = self.root.join("snapshots").join(format!("{id}.sqlite3"));
        let pending = self
            .root
            .join("snapshots")
            .join(format!("pending-{id}.json"));
        let publication = if pending.try_exists().map_err(io)? {
            storage::read_json::<SnapshotPublication>(&pending)?
        } else {
            let stage = self
                .root
                .join("snapshots")
                .join(format!("stage-{}.sqlite3", SnapshotId::new()));
            copy_database(connection, &stage)?;
            let saved = storage::raw_connection(&stage, false)?;
            let state = storage::status(&saved)?;
            drop(saved);
            let snapshot = Snapshot {
                id,
                name: name.clone(),
                path: path.clone(),
                sha256: file_digest(&stage)?,
                revision: state.revision,
                automatic,
            };
            let publication = SnapshotPublication { snapshot, stage };
            storage::atomic_json(&pending, &publication)?;
            self.checkpoint("backup_prepared")?;
            publication
        };
        if publication.snapshot.id != id
            || publication.snapshot.name != name
            || publication.snapshot.path != path
            || publication.stage.parent() != path.parent()
        {
            return Err(corrupt("Snapshot publication identity mismatch"));
        }
        if !path.try_exists().map_err(io)? {
            if file_digest(&publication.stage)? != publication.snapshot.sha256 {
                return Err(corrupt("Snapshot stage checksum mismatch"));
            }
            std::fs::rename(&publication.stage, &path).map_err(io)?;
            storage::sync_dir(path.parent().ok_or_else(|| io("Snapshot has no parent"))?)?;
        }
        self.checkpoint("backup_published")?;
        drop(self.verify_snapshot(&publication.snapshot)?);
        storage::atomic_json(&manifest, &publication.snapshot)?;
        self.checkpoint("backup_receipt")?;
        std::fs::remove_file(&pending).map_err(io)?;
        storage::sync_dir(path.parent().ok_or_else(|| io("Snapshot has no parent"))?)?;
        Ok(publication.snapshot)
    }
    pub(super) fn verify_snapshot(&self, snapshot: &Snapshot) -> Result<Connection> {
        let expected = self
            .root
            .join("snapshots")
            .join(format!("{}.sqlite3", snapshot.id));
        if snapshot.path != expected {
            return Err(corrupt("Snapshot path is not owned by this catalog"));
        }
        let meta = std::fs::symlink_metadata(&expected).map_err(corrupt)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(corrupt("Snapshot file identity changed"));
        }
        if file_digest(&expected)? != snapshot.sha256 {
            return Err(corrupt("Snapshot checksum mismatch"));
        }
        let connection = storage::raw_connection(&expected, false)?;
        integrity(&connection)?;
        if storage::status(&connection)?.revision != snapshot.revision {
            return Err(corrupt("Snapshot revision mismatch"));
        }
        Ok(connection)
    }
    pub(super) fn automatic_backup(&self) -> Result<()> {
        let lock = storage::private_file(&self.root.join("snapshots.lock"), false)?;
        lock.lock().map_err(io)?;
        self.snapshot_locked(
            &self.connection()?,
            "Automatic catalog snapshot".into(),
            true,
        )?;
        // Only verified, owned automatic snapshots may be reclaimed. Named backups never expire.
        for old in self
            .snapshots_locked()?
            .into_iter()
            .filter(|s| s.automatic)
            .skip(10)
        {
            drop(self.verify_snapshot(&old)?);
            std::fs::remove_file(&old.path).map_err(io)?;
            std::fs::remove_file(
                self.root
                    .join("snapshots")
                    .join(format!("snapshot-{}.json", old.id)),
            )
            .map_err(io)?;
        }
        storage::sync_dir(&self.root.join("snapshots"))
    }
    pub(super) fn after_mutation(&self, mut receipt: Receipt) -> Result<Receipt> {
        if let Err(error) = self.automatic_backup() {
            receipt.issues.push(issue(
                IssueCode::BackupFailed,
                format!(
                    "Mutation committed at revision {}, automatic backup failed: {error}",
                    receipt.revision
                ),
            ));
            self.connection()?
                .execute(
                    "UPDATE receipts SET body=?1 WHERE request=?2",
                    rusqlite::params![encode(&receipt)?, receipt.request.to_string()],
                )
                .map_err(io)?;
        }
        Ok(receipt)
    }
}
