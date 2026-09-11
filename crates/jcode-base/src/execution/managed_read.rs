use super::ExecutionStore;
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
        let file = store.open_output_file(&id)?;
        ensure!(
            file.metadata()?.len() >= record.output_bytes,
            "Managed output lost committed bytes"
        );
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
