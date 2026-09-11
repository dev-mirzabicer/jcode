use super::{ExecutionStore, RunRecord};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path};

pub(super) struct ManagedRead {
    pub id: String,
    pub length: u64,
    pub running: bool,
    file: File,
    store: ExecutionStore,
    position: u64,
    chunk_start: u64,
    chunk: Vec<u8>,
}
impl ManagedRead {
    pub fn is_path(root: &Path, path: &Path) -> bool {
        path.strip_prefix(root.join("execution/outputs")).is_ok_and(|relative| {
            let parts:Vec<_>=relative.components().collect();
            matches!(parts.as_slice(),[Component::Normal(_),Component::Normal(file)] if *file==std::ffi::OsStr::new("output.txt"))
        })
    }
    pub fn open(root: &Path, path: &Path) -> Result<Option<Self>> {
        Self::open_with_stop(root, path, None)
    }
    pub fn open_with_stop(
        root: &Path,
        path: &Path,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<Option<Self>> {
        ensure!(
            !stop.is_some_and(|signal| signal.is_set()),
            "Managed output read cancelled"
        );
        let directory = root.join("execution/outputs");
        let Ok(relative) = path.strip_prefix(&directory) else {
            return Ok(None);
        };
        let components: Vec<_> = relative.components().collect();
        let [Component::Normal(id), Component::Normal(file)] = components.as_slice() else {
            return Ok(None);
        };
        if *file != std::ffi::OsStr::new("output.txt") {
            return Ok(None);
        }
        let id = id
            .to_str()
            .context("Invalid managed output identity")?
            .to_string();
        ensure!(
            id.len() == 68
                && id.starts_with("run-")
                && id[4..].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid managed output identity"
        );
        let store = ExecutionStore::open(root)?;
        let record = store
            .inspect(&id)?
            .context("Managed output is unavailable")?;
        ensure!(
            record.output_path.as_deref() == Some(path),
            "Managed source does not match its recorded output path"
        );
        let mut file = store.open_output_file(&id)?;
        ensure!(
            file.metadata()?.len() >= record.output_bytes,
            "Managed output lost committed bytes"
        );
        ensure_legacy_index(&store, &record, &mut file, stop)?;
        Ok(Some(Self {
            id,
            length: record.output_bytes,
            running: !record.state.terminal(),
            file,
            store,
            position: 0,
            chunk_start: 0,
            chunk: Vec::new(),
        }))
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }
    fn load_chunk(&mut self) -> Result<()> {
        let (start,end,digest):(i64,i64,String)=self.store.connection()?.query_row("SELECT start_byte,end_byte,sha256 FROM output_chunks WHERE run_id=?1 AND start_byte<=?2 ORDER BY start_byte DESC LIMIT 1",params![self.id,i64::try_from(self.position)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
            .context("Managed output has no verified chunk at this position; legacy acquisition must be imported explicitly")?;
        let start = u64::try_from(start)?;
        let end = u64::try_from(end)?;
        ensure!(
            start <= self.position && self.position < end && end <= self.length,
            "Committed output chunk coverage is inconsistent"
        );
        ensure!(
            end - start <= 256 * 1024,
            "Unexpected managed output chunk size"
        );
        self.file.seek(SeekFrom::Start(start))?;
        self.chunk.resize(usize::try_from(end - start)?, 0);
        self.file.read_exact(&mut self.chunk)?;
        ensure!(
            digest == format!("{:x}", Sha256::digest(&self.chunk)),
            "Managed output bytes changed after capture"
        );
        self.chunk_start = start;
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct LegacyIntegrity {
    schema: u32,
    invocation_id: String,
    text_sha256: Option<String>,
    source: jcode_tool_types::OutputSource,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct IndexedChunk {
    start: u64,
    end: u64,
    digest: String,
}

fn ensure_legacy_index(
    store: &ExecutionStore,
    record: &RunRecord,
    file: &mut File,
    stop: Option<&jcode_agent_runtime::InterruptSignal>,
) -> Result<()> {
    if record.output_bytes == 0 {
        return Ok(());
    }
    let has_chunks = |connection: &rusqlite::Connection| -> Result<bool> {
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM output_chunks WHERE run_id=?1)",
            [&record.id],
            |row| row.get(0),
        )?)
    };
    if has_chunks(&store.connection()?)? {
        return Ok(());
    }
    ensure!(
        record.state.terminal(),
        "Legacy live output has no committed chunk index; wait for its owning runtime to finish"
    );
    let _lease = super::storage::output_lease(store, &record.id)?;
    if has_chunks(&store.connection()?)? {
        return Ok(());
    }
    let manifest = record
        .result_path
        .as_ref()
        .context("Legacy output has no integrity receipt; original completeness is unknown")?;
    ensure!(
        std::fs::symlink_metadata(manifest)?.is_file(),
        "Legacy output receipt changed type"
    );
    let identity: LegacyIntegrity = crate::storage::read_json(manifest)?;
    ensure!(
        identity.schema == 1 && identity.invocation_id == record.id,
        "Legacy output receipt identity mismatch"
    );
    ensure!(
        matches!(&identity.source,jcode_tool_types::OutputSource::Retained(reference) if reference.invocation_id==record.id && Some(&reference.path)==record.output_path.as_ref() && reference.bytes==record.output_bytes),
        "Legacy receipt does not describe this committed output"
    );
    let expected = identity
        .text_sha256
        .context("Legacy output has no original digest; do not fabricate an intact result")?;
    // Stage only bounded chunk metadata, not another copy of the source body.
    // No database write transaction is held during the potentially long scan.
    let directory = store.root().join("index-imports");
    crate::storage::ensure_dir(&directory)?;
    ensure!(
        std::fs::symlink_metadata(&directory)?.is_dir(),
        "Index staging directory changed type"
    );
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    jcode_core::fs::set_permissions_owner_only(staged.path())?;
    let mut position = 0u64;
    let mut total = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    file.seek(SeekFrom::Start(0))?;
    while position < record.output_bytes {
        ensure!(
            !stop.is_some_and(|signal| signal.is_set()),
            "Legacy read-index import cancelled before publication"
        );
        let n = (record.output_bytes - position).min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..n])?;
        total.update(&buffer[..n]);
        let chunk = IndexedChunk {
            start: position,
            end: position + n as u64,
            digest: format!("{:x}", Sha256::digest(&buffer[..n])),
        };
        serde_json::to_writer(staged.as_file_mut(), &chunk)?;
        std::io::Write::write_all(staged.as_file_mut(), b"\n")?;
        position = chunk.end;
    }
    ensure!(
        expected == format!("{:x}", total.finalize()),
        "Legacy output changed since its original capture; no read index was published"
    );
    staged.as_file_mut().seek(SeekFrom::Start(0))?;
    ensure!(
        !stop.is_some_and(|signal| signal.is_set()),
        "Legacy read-index import cancelled before publication"
    );
    let mut connection = store.connection()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if !has_chunks(&transaction)? {
        let unchanged:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND output_bytes=?3 AND state=?4)",params![record.id,record.owner,i64::try_from(record.output_bytes)?,record.state.as_str()],|row|row.get(0))?;
        ensure!(unchanged, "Legacy execution changed during index import");
        use std::io::BufRead;
        let reader = std::io::BufReader::new(staged.as_file_mut());
        let mut insert = transaction.prepare(
            "INSERT INTO output_chunks(run_id,start_byte,end_byte,sha256) VALUES (?1,?2,?3,?4)",
        )?;
        for line in reader.lines() {
            let chunk: IndexedChunk = serde_json::from_str(&line?)?;
            insert.execute(params![
                record.id,
                i64::try_from(chunk.start)?,
                i64::try_from(chunk.end)?,
                chunk.digest
            ])?;
        }
    }
    transaction.commit()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}
impl Read for ManagedRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        if self.position < self.chunk_start
            || self.position >= self.chunk_start + self.chunk.len() as u64
        {
            self.load_chunk().map_err(io::Error::other)?;
        }
        let offset = (self.position - self.chunk_start) as usize;
        let count = buffer.len().min(self.chunk.len() - offset);
        buffer[..count].copy_from_slice(&self.chunk[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for ManagedRead {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
        };
        self.position = u64::try_from(next).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid managed output offset")
        })?;
        Ok(self.position)
    }
}
