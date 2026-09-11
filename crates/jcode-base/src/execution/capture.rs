//! One owned streaming sink for complete text, separate raw streams and receipts.
use super::output::{ImagePart, Manifest, PartIntegrity, ResourcePart, StoredPart};
use super::storage::BundleStorage;
use super::{ExecutionStore, RunRecord, RunState, StorageConfig};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use jcode_tool_core::{OutputCapture, OutputStream};
use jcode_tool_types::{OutputReference, OutputSource, ToolOutput};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
    lossy: bool,
}
impl Decoder {
    fn decode(&mut self, bytes: &[u8], finish: bool) -> String {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut remaining = input.as_slice();
        let mut output = String::new();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    output.push_str(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.push_str(
                        std::str::from_utf8(&remaining[..valid]).expect("validated UTF-8 prefix"),
                    );
                    remaining = &remaining[valid..];
                    if let Some(length) = error.error_len() {
                        output.push('\u{fffd}');
                        self.lossy = true;
                        remaining = &remaining[length..];
                    } else if finish {
                        output.push('\u{fffd}');
                        self.lossy = true;
                        break;
                    } else {
                        self.pending.extend_from_slice(remaining);
                        break;
                    }
                }
            }
        }
        output
    }
}

struct CaptureState {
    storage: BundleStorage,
    record: RunRecord,
    decoders: [Decoder; 3],
    offsets: [u64; 3],
    sequence: u64,
    committed: u64,
    failed: Option<String>,
    sealed: bool,
    seal_intent: Option<([u8; 32], RunState)>,
    pending_output: Option<ToolOutput>,
    text_digest: Sha256,
    parts: std::collections::BTreeSet<String>,
}

/// Callers share the sink, not writer handles. Publication and relocation share
/// its lock, and the kernel lease prevents a second process from moving a live file.
pub struct Capture {
    state: Mutex<CaptureState>,
}
impl Capture {
    pub fn create(store: ExecutionStore, record: RunRecord, config: StorageConfig) -> Result<Self> {
        let storage = BundleStorage::create(store, &record, config)?;
        Ok(Self::from_storage(storage, record))
    }

    fn from_storage(storage: BundleStorage, record: RunRecord) -> Self {
        Self {
            state: Mutex::new(CaptureState {
                storage,
                record,
                decoders: Default::default(),
                offsets: [0; 3],
                sequence: 0,
                committed: 0,
                failed: None,
                sealed: false,
                seal_intent: None,
                pending_output: None,
                text_digest: Sha256::new(),
                parts: Default::default(),
            }),
        }
    }

    /// Seal available output and publish the terminal receipt after owned work
    /// actually stops. Capture failure overrides an otherwise successful exit.
    pub fn seal(&self, mut output: ToolOutput, outcome: RunState) -> Result<ToolOutput> {
        let outcome = if output.is_error && outcome == RunState::Completed {
            RunState::Failed
        } else {
            outcome
        };
        ensure!(
            outcome.terminal(),
            "Capture sealing requires a terminal outcome"
        );
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(!state.sealed, "Capture is already sealed");
        let digest = output_digest(&output)?;
        if let Some((previous, previous_outcome)) = state.seal_intent {
            ensure!(
                previous == digest && previous_outcome == outcome,
                "Terminal retry differs from the frozen original result"
            );
        } else {
            state.seal_intent = Some((digest, outcome));
        }
        if state.pending_output.is_none() {
            if state.failed.is_none() {
                let result = state.finish_body(&mut output);
                if let Err(error) = result {
                    state.failed = Some(format!("{error:#}"));
                }
            }
            let parts = if state.failed.is_none() {
                match state
                    .parts
                    .iter()
                    .map(|file| {
                        Ok(StoredPart {
                            file: file.clone(),
                            integrity: PartIntegrity::read(state.storage.read_part(file)?)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
                {
                    Ok(parts) => parts,
                    Err(error) => {
                        state.failed = Some(format!("{error:#}"));
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let complete = state.failed.is_none();
            let source = OutputSource::Retained(state.reference(complete));
            let manifest_path;
            if complete {
                let manifest = Manifest {
                    process_exit: output.process_exit.clone(),
                    parts,
                    schema: 1,
                    invocation_id: state.record.id.clone(),
                    title: output.title.clone(),
                    metadata: output.metadata.take(),
                    images: output
                        .images
                        .iter()
                        .enumerate()
                        .map(|(index, image)| ImagePart {
                            media_type: image.media_type.clone(),
                            label: image.label.clone(),
                            file: format!("image-{index}.base64"),
                            raw_base64: true,
                            integrity: Some(PartIntegrity::of(image.data.as_bytes())),
                        })
                        .collect(),
                    source: source.clone(),
                    resources: output
                        .resources
                        .iter()
                        .enumerate()
                        .map(|(index, part)| ResourcePart {
                            uri: part.uri.clone(),
                            media_type: part.media_type.clone(),
                            file: format!("resource-{index}.base64"),
                            integrity: Some(PartIntegrity::of(part.data.as_bytes())),
                        })
                        .collect(),
                    outcome: Some(outcome),
                    text_sha256: Some(format!("{:x}", state.text_digest.clone().finalize())),
                };
                if let Err(error) = state.storage.write_json("manifest.json", &manifest) {
                    state.failed = Some(format!("{error:#}"));
                }
                output.metadata = manifest.metadata;
            }
            if let Some(error) = &state.failed {
                // Receipt headroom is deliberately on local storage. Do not require
                // another successful write to the failed/removed archive to report it.
                let directory = state.storage.store.root().join("receipts");
                crate::storage::ensure_dir(&directory)?;
                manifest_path = directory.join(format!("{}.json", state.record.id));
                let manifest = Manifest {
                    process_exit: output.process_exit.clone(),
                    schema: 1,
                    invocation_id: state.record.id.clone(),
                    title: output.title.clone(),
                    metadata: Some(
                        serde_json::json!({"capture_error":error,"partial_bundle":state.storage.alias()}),
                    ),
                    images: Vec::new(),
                    resources: Vec::new(),
                    parts: Vec::new(),
                    source: OutputSource::Retained(state.reference(false)),
                    outcome: Some(RunState::Failed),
                    text_sha256: Some(format!("{:x}", state.text_digest.clone().finalize())),
                };
                crate::storage::write_json_secret(&manifest_path,&manifest)
                .context("Capture failed and terminal receipt could not be persisted; existing output files remain preserved")?;
            } else {
                manifest_path = state.storage.alias().join("manifest.json");
            }
            state.record.state = if state.failed.is_some() {
                RunState::Failed
            } else {
                outcome
            };
            state.record.process_exit = output.process_exit.clone();
            state.record.output_path = Some(state.storage.alias().join("output.txt"));
            state.record.output_bytes = state.committed;
            state.record.complete = state.failed.is_none();
            state.record.result_path = Some(manifest_path);
            output.source = OutputSource::Retained(state.reference(state.failed.is_none()));
            state.pending_output = Some(output);
        }
        state.storage.store.finish(&state.record)?;
        state.sealed = true;
        let output = state
            .pending_output
            .take()
            .expect("terminal output prepared before publication");
        if let Some(error) = &state.failed {
            anyhow::bail!(
                "Output capture failed. Retained prefix: {}. Run: {}. {error}",
                state.storage.alias().join("output.txt").display(),
                state.record.id
            );
        }
        Ok(output)
    }
}

impl CaptureState {
    fn reference(&self, complete: bool) -> OutputReference {
        OutputReference {
            invocation_id: self.record.id.clone(),
            path: self.storage.alias().join("output.txt"),
            bytes: self.committed,
            complete,
            continuation: None,
            manifest_path: self.record.result_path.clone(),
        }
    }

    fn write_chunk(&mut self, stream: OutputStream, bytes: &[u8]) -> Result<()> {
        let (index, name) = match stream {
            OutputStream::Text => (0, "text"),
            OutputStream::Stdout => (1, "stdout"),
            OutputStream::Stderr => (2, "stderr"),
        };
        self.storage.append(&format!("{name}.bin"), bytes)?;
        let decoded = self.decoders[index].decode(bytes, false);
        self.storage.append("output.txt", decoded.as_bytes())?;
        let event = serde_json::json!({"sequence":self.sequence,"stream":name,"offset":self.offsets[index],"bytes":bytes.len(),"rendered_bytes":decoded.len()});
        let mut event = serde_json::to_vec(&event)?;
        event.push(b'\n');
        self.storage.append("events.jsonl", &event)?;
        self.commit_rendered(decoded.as_bytes())?;
        self.offsets[index] += bytes.len() as u64;
        self.sequence += 1;
        Ok(())
    }

    fn commit_rendered(&mut self, bytes: &[u8]) -> Result<()> {
        let next = self
            .committed
            .checked_add(bytes.len() as u64)
            .context("Output byte count overflow")?;
        let mut connection = self.storage.store.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(transaction.execute("UPDATE runs SET output_bytes=?3,updated=unixepoch() WHERE id=?1 AND owner=?2 AND state='running' AND output_bytes=?4",params![self.record.id,self.record.owner,i64::try_from(next)?,i64::try_from(self.committed)?])?==1,"Output capture lost invocation ownership or committed prefix");
        if !bytes.is_empty() {
            transaction.execute(
                "INSERT INTO output_chunks(run_id,start_byte,end_byte,sha256) VALUES (?1,?2,?3,?4)",
                params![
                    self.record.id,
                    i64::try_from(self.committed)?,
                    i64::try_from(next)?,
                    format!("{:x}", Sha256::digest(bytes))
                ],
            )?;
        }
        transaction.commit()?;
        self.committed = next;
        self.text_digest.update(bytes);
        Ok(())
    }

    fn finish_body(&mut self, output: &mut ToolOutput) -> Result<()> {
        match &output.source {
            OutputSource::Inline => {
                ensure!(
                    self.sequence == 0,
                    "Streaming producers must return their captured reference rather than repeat their body"
                );
                for chunk in output.output.as_bytes().chunks(64 * 1024) {
                    self.write_chunk(OutputStream::Text, chunk)?;
                }
            }
            OutputSource::Retained(reference) => {
                ensure!(
                    reference.invocation_id == self.record.id
                        && reference.path == self.storage.alias().join("output.txt"),
                    "Producer supplied a foreign retained-output reference"
                );
            }
            OutputSource::ReadPage(_)
            | OutputSource::Acceptance(_)
            | OutputSource::Unavailable(_) => {
                anyhow::bail!("A source-read page must not enter the output capture sink")
            }
        }
        for index in 0..self.decoders.len() {
            let suffix = self.decoders[index].decode(&[], true);
            self.storage.append("output.txt", suffix.as_bytes())?;
            self.commit_rendered(suffix.as_bytes())?;
        }
        for (index, image) in output.images.iter().enumerate() {
            // Preserve exactly what arrived, including undecodable media.
            self.storage
                .write_file(&format!("image-{index}.base64"), image.data.as_bytes())?;
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&image.data) {
                self.storage
                    .write_file(&format!("image-{index}.bin"), &bytes)?;
            }
        }
        for (index, resource) in output.resources.iter().enumerate() {
            self.storage.write_file(
                &format!("resource-{index}.base64"),
                resource.data.as_bytes(),
            )?;
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&resource.data) {
                self.storage
                    .write_file(&format!("resource-{index}.bin"), &bytes)?;
            }
        }
        let streams = serde_json::json!({"ordering":"observed drain order, not producer-time order","text":{"raw":"text.bin","bytes":self.offsets[0],"lossy_rendering":self.decoders[0].lossy},"stdout":{"raw":"stdout.bin","bytes":self.offsets[1],"lossy_rendering":self.decoders[1].lossy},"stderr":{"raw":"stderr.bin","bytes":self.offsets[2],"lossy_rendering":self.decoders[2].lossy}});
        self.storage.write_json("streams.json", &streams)?;
        Ok(())
    }
}

impl OutputCapture for Capture {
    fn report_progress(
        &self,
        progress: jcode_background_types::BackgroundTaskProgress,
        checkpoint: bool,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure!(
            !state.sealed && state.seal_intent.is_none(),
            "Cannot report progress after terminal publication"
        );
        state.storage.store.record_progress(
            &state.record.id,
            &state.record.owner,
            progress,
            checkpoint,
        )
    }
    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let file = part_filename(name)?;
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            !state.sealed && state.seal_intent.is_none() && state.failed.is_none(),
            "Capture is no longer writable"
        );
        if bytes.is_empty() && state.parts.contains(&file) {
            return Ok(());
        }
        state.parts.insert(file.clone());
        let result = (|| {
            let owned: bool = state.storage.store.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running')",
                params![state.record.id, state.record.owner],
                |row| row.get(0),
            )?;
            ensure!(owned, "Raw capture part lost invocation ownership");
            if bytes.is_empty() {
                state.storage.write_file(&file, bytes)
            } else {
                state.storage.append(&file, bytes)
            }
        })();
        if let Err(error) = &result {
            state.failed = Some(format!("{error:#}"));
        }
        result
    }
    fn read_part(&self, name: &str) -> Result<jcode_tool_core::CapturedPart> {
        let file = part_filename(name)?;
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(state.parts.contains(&file), "Unknown captured part");
        Ok(jcode_tool_core::CapturedPart {
            path: state.storage.alias().join(&file),
            reader: state.storage.read_part(&file)?,
        })
    }
    fn write(&self, stream: OutputStream, bytes: &[u8]) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(!state.sealed, "Cannot append to a sealed output");
        ensure!(
            state.seal_intent.is_none(),
            "Cannot append after terminal publication has begun"
        );
        ensure!(
            state.failed.is_none(),
            "Output capture previously failed; owned work must stop"
        );
        for chunk in bytes.chunks(64 * 1024) {
            if let Err(error) = state.write_chunk(stream, chunk) {
                state.failed = Some(format!("{error:#}"));
                return Err(error);
            }
        }
        Ok(())
    }
    fn reference(&self) -> Result<OutputReference> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        Ok(state.reference(state.sealed && state.failed.is_none()))
    }
}

fn part_filename(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name.len() <= 100
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid captured part name"
    );
    Ok(format!("part-{name}.bin"))
}

pub fn output_digest(output: &ToolOutput) -> Result<[u8; 32]> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, output)?;
    Ok(writer.0.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};

    #[test]
    fn process_exit_recovers_with_terminal_receipt_and_metadata_only_status() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        let exit = jcode_tool_types::ProcessExit {
            code: Some(7),
            signal: None,
            timed_out: false,
        };
        let mut output = ToolOutput::new("failed body").with_error(true);
        output.process_exit = Some(exit.clone());
        store.connection()?.execute_batch("CREATE TRIGGER block_terminal BEFORE UPDATE OF state ON runs WHEN NEW.state='failed' BEGIN SELECT RAISE(FAIL,'injected terminal failure'); END;")?;
        assert!(capture.seal(output, RunState::Failed).is_err());
        drop(capture);
        store
            .connection()?
            .execute_batch("DROP TRIGGER block_terminal;")?;
        let recovered = store.recover_terminal_output(&record.id)?;
        assert_eq!(recovered.process_exit, Some(exit.clone()));
        let manifest = recovered.result_path.unwrap();
        std::fs::rename(&manifest, manifest.with_extension("saved"))?;
        assert_eq!(store.inspect(&record.id)?.unwrap().process_exit, Some(exit));
        Ok(())
    }

    #[test]
    fn raw_parts_preserve_chunks_and_empty_appends_without_aliasing_canonical_files() -> Result<()>
    {
        use std::io::Read;
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        capture.append_part("response", b"abc")?;
        capture.append_part("response", b"")?;
        capture.append_part("response", b"\xffdef")?;
        assert!(capture.append_part("../output.txt", b"overwrite").is_err());
        let mut part = capture.read_part("response")?;
        let mut bytes = Vec::new();
        part.reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"abc\xffdef");
        capture.seal(ToolOutput::new("selected rendering"), RunState::Completed)?;
        assert_eq!(
            std::fs::read_to_string(store.inspect(&record.id)?.unwrap().output_path.unwrap())?,
            "selected rendering"
        );
        assert!(capture.append_part("response", b"late").is_err());
        Ok(())
    }

    #[test]
    fn raw_part_requires_current_invocation_ownership() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        store.connection()?.execute(
            "UPDATE runs SET owner='another-owner' WHERE id=?1",
            [&record.id],
        )?;
        assert!(
            capture
                .append_part("response", b"not acknowledged")
                .is_err()
        );
        Ok(())
    }
    fn prepared(store: &ExecutionStore) -> Result<RunRecord> {
        let invocation = Invocation {
            session_id: "s".into(),
            message_id: "m".into(),
            call_path: vec!["c".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&invocation, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        Ok(record)
    }
    #[test]
    fn split_utf8_raw_streams_and_observed_sequence_are_retained() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        let emoji = "🙂".as_bytes();
        capture.write(OutputStream::Stdout, &emoji[..2])?;
        capture.write(OutputStream::Stderr, b"error\xff")?;
        capture.write(OutputStream::Stdout, &emoji[2..])?;
        let mut output = ToolOutput::new("");
        output.source = OutputSource::Retained(capture.reference()?);
        capture.seal(output, RunState::Cancelled)?;
        let saved = store.inspect(&record.id)?.unwrap();
        assert_eq!(saved.state, RunState::Cancelled);
        let path = saved.output_path.unwrap();
        assert_eq!(std::fs::read_to_string(&path)?, "error\u{fffd}🙂");
        assert_eq!(std::fs::read(path.with_file_name("stdout.bin"))?, emoji);
        assert_eq!(
            std::fs::read(path.with_file_name("stderr.bin"))?,
            b"error\xff"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_file_name("events.jsonl"))?
                .lines()
                .count(),
            3
        );
        assert!(capture.write(OutputStream::Text, b"late").is_err());
        Ok(())
    }

    #[test]
    fn terminal_sql_failure_retries_without_recapturing_body() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        store.connection()?.execute_batch("CREATE TRIGGER fail_terminal BEFORE UPDATE OF state ON runs WHEN NEW.state='completed' BEGIN SELECT RAISE(FAIL,'injected terminal write'); END;")?;
        let output = ToolOutput::new("one exact body");
        assert!(capture.seal(output.clone(), RunState::Completed).is_err());
        assert!(
            capture
                .seal(ToolOutput::new("different result"), RunState::Completed)
                .is_err()
        );
        store
            .connection()?
            .execute_batch("DROP TRIGGER fail_terminal;")?;
        capture.seal(output, RunState::Completed)?;
        let saved = store.inspect(&record.id)?.unwrap();
        assert_eq!(saved.state, RunState::Completed);
        assert_eq!(
            std::fs::read_to_string(saved.output_path.unwrap())?,
            "one exact body"
        );
        Ok(())
    }

    #[test]
    fn lost_invocation_owner_cannot_acknowledge_a_stream_write() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let record = prepared(&store)?;
        let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
        store
            .connection()?
            .execute("UPDATE runs SET owner='other' WHERE id=?1", [&record.id])?;
        assert!(
            capture
                .write(OutputStream::Stdout, b"not acknowledged")
                .is_err()
        );
        assert_eq!(store.inspect(&record.id)?.unwrap().owner, "other");
        Ok(())
    }

    #[test]
    fn sealed_receipt_recovers_after_owner_drop_and_rejects_corruption() -> Result<()> {
        for corrupt in [false, true] {
            let dir = tempfile::tempdir()?;
            let store = ExecutionStore::open(dir.path())?;
            let record = prepared(&store)?;
            let capture = Capture::create(store.clone(), record.clone(), StorageConfig::default())?;
            store.connection()?.execute_batch("CREATE TRIGGER fail_terminal BEFORE UPDATE OF state ON runs WHEN NEW.state='completed' BEGIN SELECT RAISE(FAIL,'injected terminal write'); END;")?;
            assert!(
                capture
                    .seal(ToolOutput::new("one exact body"), RunState::Completed)
                    .is_err()
            );
            assert!(
                store.recover_terminal_output(&record.id).is_err(),
                "live capture lease must prevent recovery"
            );
            drop(capture);
            store
                .connection()?
                .execute_batch("DROP TRIGGER fail_terminal;")?;
            if corrupt {
                std::fs::write(
                    store.inspect(&record.id)?.unwrap().output_path.unwrap(),
                    "two exact body",
                )?;
            }
            let reopened = ExecutionStore::open(dir.path())?;
            let recovered = reopened.recover_terminal_output(&record.id);
            if corrupt {
                assert!(recovered.is_err());
            } else {
                assert_eq!(recovered?.state, RunState::Completed);
                assert_eq!(
                    reopened.recover_terminal_output(&record.id)?.state,
                    RunState::Completed
                );
            }
        }
        Ok(())
    }
}
