//! Bounded client materialization of sealed, manifest-declared output parts.
use super::{
    ExecutionStore, RunRecord,
    output::{PartIndex, PartIntegrity},
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path};

struct Stoppable<'a, R> {
    inner: R,
    stop: Option<&'a jcode_agent_runtime::InterruptSignal>,
}
impl<R: Read> Read for Stoppable<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.stop.is_some_and(|signal| signal.is_set()) {
            return Err(std::io::Error::other("Retained part read cancelled"));
        }
        self.inner.read(bytes)
    }
}

impl ExecutionStore {
    pub fn read_part_page(
        &self,
        record: &RunRecord,
        name: &str,
        offset: u64,
        limit: u32,
        expected_sha256: Option<&str>,
    ) -> Result<jcode_tool_types::execution::ExecutionPartPage> {
        self.read_part_page_with_stop(record, name, offset, limit, expected_sha256, None)
    }

    pub(super) fn read_part_page_with_stop(
        &self,
        record: &RunRecord,
        name: &str,
        offset: u64,
        limit: u32,
        expected_sha256: Option<&str>,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<jcode_tool_types::execution::ExecutionPartPage> {
        ensure!(
            !stop.is_some_and(|signal| signal.is_set()),
            "Retained part read cancelled"
        );
        ensure!(
            record.id.len() == 68
                && record.id.starts_with("run-")
                && record.id[4..].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid execution identity"
        );
        let current = self
            .inspect(&record.id)?
            .context("Execution is unavailable")?;
        ensure!(
            current.state.terminal() && current.result_path == record.result_path,
            "Execution part does not match its authoritative terminal record"
        );
        ensure!(
            record.state.terminal(),
            "Output parts are not sealed; inspect the live canonical output instead"
        );
        ensure!(
            (1..=1024 * 1024).contains(&limit),
            "Part page limit must be 1 through 1048576 bytes"
        );
        let mut components = Path::new(name).components();
        ensure!(
            matches!(components.next(), Some(Component::Normal(_)))
                && components.next().is_none()
                && !name.starts_with('.'),
            "Invalid retained part selector"
        );
        ensure!(
            offset == 0 || expected_sha256.is_some(),
            "A continuation requires the prior page's SHA-256"
        );
        if let Some(hash) = expected_sha256 {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Invalid part SHA-256"
            );
        }
        let manifest = self
            .root()
            .join("outputs")
            .join(&record.id)
            .join("manifest.json");
        ensure!(
            record.result_path.as_ref() == Some(&manifest),
            "This execution has no sealed bundle manifest"
        );
        let mut manifest_file = self.open_output_part(&record.id, "manifest.json")?;
        let manifest_version = manifest_file.metadata()?;
        let index: PartIndex = serde_json::from_reader(Stoppable {
            inner: std::io::BufReader::new(&mut manifest_file),
            stop,
        })?;
        ensure!(
            index.schema == 1 && index.invocation_id == record.id,
            "Retained manifest identity changed"
        );
        let expected = if name == "manifest.json" {
            None
        } else {
            Some(self.part_integrity(&record.id, name, &index, stop)?)
        };
        let (mut file, before) = if name == "manifest.json" {
            manifest_file.seek(SeekFrom::Start(0))?;
            (manifest_file, manifest_version)
        } else {
            let file = self.open_output_part(&record.id, name)?;
            let metadata = file.metadata()?;
            (file, metadata)
        };
        ensure!(offset <= before.len(), "Part offset is beyond EOF");
        let end = offset
            .checked_add(u64::from(limit))
            .context("Part page range overflow")?
            .min(before.len());
        let mut bytes = Vec::with_capacity((end - offset) as usize);
        let mut digest = Sha256::new();
        let mut position = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            ensure!(
                !stop.is_some_and(|signal| signal.is_set()),
                "Retained part read cancelled"
            );
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let next = position
                .checked_add(count as u64)
                .context("Part byte count overflow")?;
            digest.update(&buffer[..count]);
            if position < end && next > offset {
                let from = offset.saturating_sub(position) as usize;
                let to = (end.min(next) - position) as usize;
                bytes.extend_from_slice(&buffer[from..to]);
            }
            position = next;
        }
        let after = file.metadata()?;
        ensure!(
            position == before.len()
                && after.len() == before.len()
                && after.modified()? == before.modified()?,
            "Retained part changed during read"
        );
        let actual = PartIntegrity {
            bytes: position,
            sha256: format!("{:x}", digest.finalize()),
        };
        ensure!(
            expected.as_ref().is_none_or(|value| value == &actual),
            "Retained part differs from its original integrity receipt"
        );
        ensure!(
            expected_sha256.is_none_or(|value| value == actual.sha256),
            "Part continuation is stale; bytes changed"
        );
        Ok(jcode_tool_types::execution::ExecutionPartPage {
            part: name.into(),
            offset,
            total_bytes: position,
            sha256: actual.sha256,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            next_offset: (end < position).then_some(end),
        })
    }

    fn part_integrity(
        &self,
        id: &str,
        name: &str,
        index: &PartIndex,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<PartIntegrity> {
        if let Some(part) = index.parts.iter().find(|part| part.file == name) {
            return Ok(part.integrity.clone());
        }
        for (ordinal, part) in index.images.iter().enumerate() {
            if part.file == name {
                return part
                    .integrity
                    .clone()
                    .context("Legacy image has no original integrity receipt");
            }
            if name == format!("image-{ordinal}.bin") && part.raw_base64 {
                ensure!(
                    part.file == format!("image-{ordinal}.base64")
                        && part.decoded_file.as_deref().is_none_or(|file| file == name),
                    "Image descriptor changed"
                );
                return self.decoded_part_integrity(id, &part.file, part.integrity.as_ref(), stop);
            }
        }
        for (ordinal, part) in index.resources.iter().enumerate() {
            if part.file == name {
                return part
                    .integrity
                    .clone()
                    .context("Legacy resource has no original integrity receipt");
            }
            if name == format!("resource-{ordinal}.bin") {
                ensure!(
                    part.file == format!("resource-{ordinal}.base64")
                        && part.decoded_file.as_deref().is_none_or(|file| file == name),
                    "Resource descriptor changed"
                );
                return self.decoded_part_integrity(id, &part.file, part.integrity.as_ref(), stop);
            }
        }
        bail!("Part is not declared in the retained manifest; no arbitrary server path was read")
    }

    fn decoded_part_integrity(
        &self,
        id: &str,
        name: &str,
        expected: Option<&PartIntegrity>,
        stop: Option<&jcode_agent_runtime::InterruptSignal>,
    ) -> Result<PartIntegrity> {
        let mut source = self.open_output_part(id, name)?;
        let before = source.metadata()?;
        let original = PartIntegrity::read(Stoppable {
            inner: &mut source,
            stop,
        })?;
        ensure!(
            expected == Some(&original),
            "Encoded part differs from its original integrity receipt"
        );
        source.seek(SeekFrom::Start(0))?;
        let decoded = PartIntegrity::read(base64::read::DecoderReader::new(
            Stoppable {
                inner: &mut source,
                stop,
            },
            &base64::engine::general_purpose::STANDARD,
        ))?;
        let after = source.metadata()?;
        ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "Encoded part changed during decoding"
        );
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Capture, Invocation, PreparedInvocation, inspection};
    use jcode_tool_core::{OutputCapture, OutputStream};
    use jcode_tool_types::{
        RunState, ToolOutput, ToolResource,
        execution::{ExecutionRequest, ExecutionResponse},
    };

    #[tokio::test]
    async fn public_part_reads_reassemble_bytes_and_reject_unsafe_selectors_and_changed_versions()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let invocation = Invocation {
            session_id: "parts".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&invocation, "fixture")? else {
            panic!()
        };
        store.start(&record.id, "fixture")?;
        let capture = Capture::create(store.clone(), record.clone(), Default::default())?;
        let bytes = [b"binary\xff\0\r\n".repeat(15000), b"TAIL".to_vec()].concat();
        capture.write(OutputStream::Stdout, &bytes)?;
        let mut output = ToolOutput::new("");
        output.source = jcode_tool_types::OutputSource::Retained(capture.reference()?);
        output.resources.push(ToolResource {
            uri: "fixture:binary".into(),
            media_type: Some("application/octet-stream".into()),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
        capture.seal(output, RunState::Completed)?;
        let record = store.inspect(&record.id)?.unwrap();
        for part in ["stdout.bin", "resource-0.bin"] {
            let mut offset = 0;
            let mut hash = None;
            let mut all = Vec::new();
            loop {
                let request = ExecutionRequest::ReadPart {
                    run_id: record.id.clone(),
                    part: part.into(),
                    offset: Some(offset),
                    limit: Some(7000),
                    expected_sha256: hash.clone(),
                };
                let ExecutionResponse::Part { run_id, page } =
                    inspection::inspect(root.path(), "parts", request).await?
                else {
                    panic!()
                };
                assert_eq!(run_id, record.id);
                assert_eq!(page.offset, offset);
                let block = base64::engine::general_purpose::STANDARD.decode(&page.data_base64)?;
                assert!(block.len() <= 7000);
                all.extend(block);
                hash = Some(page.sha256);
                if let Some(next) = page.next_offset {
                    offset = next
                } else {
                    break;
                }
            }
            assert_eq!(all, bytes);
        }
        let manifest = store.read_part_page(&record, "manifest.json", 0, 32, None)?;
        let stop = jcode_agent_runtime::InterruptSignal::new();
        stop.fire();
        assert!(
            store
                .read_part_page_with_stop(&record, "manifest.json", 0, 32, None, Some(&stop))
                .is_err()
        );
        let reader = crate::execution::reader::SourceReader::new(root.path());
        let request = crate::execution::reader::ReadRequest {
            path: record.result_path.clone().unwrap(),
            point: None,
            start_line: 1,
            end_line: None,
            target: std::num::NonZeroUsize::new(80).unwrap(),
            stop: None,
        };
        let first = reader.read(request)?;
        let jcode_tool_types::OutputSource::ReadPage(point) = first.source else {
            panic!()
        };
        let manifest_path = record.result_path.as_ref().unwrap();
        let mut changed: serde_json::Value = crate::storage::read_json(manifest_path)?;
        changed["metadata"] = serde_json::json!({"changed":true});
        let original_manifest = std::fs::read(manifest_path)?;
        std::fs::write(manifest_path, serde_json::to_vec(&changed)?)?;
        assert!(
            reader
                .read(crate::execution::reader::ReadRequest {
                    path: manifest_path.clone(),
                    point: point.next_point,
                    start_line: 1,
                    end_line: None,
                    target: std::num::NonZeroUsize::new(80).unwrap(),
                    stop: None
                })
                .is_err(),
            "A changed retained manifest must not accept an old read point"
        );
        std::fs::write(manifest_path, original_manifest)?;
        assert!(
            store
                .read_part_page(&record, "manifest.json", 32, 32, None)
                .is_err()
        );
        assert!(
            store
                .read_part_page(&record, "manifest.json", 32, 32, Some(&"0".repeat(64)))
                .is_err()
        );
        assert!(
            store
                .read_part_page(&record, "manifest.json", 32, 32, Some(&manifest.sha256))
                .is_ok()
        );
        for name in ["../input", "/etc/passwd", "unowned.bin", ".hidden"] {
            assert!(store.read_part_page(&record, name, 0, 32, None).is_err());
        }
        let source = record.output_path.unwrap().with_file_name("resource-0.bin");
        std::fs::write(&source, b"corrupt")?;
        let record = store.inspect(&record.id)?.unwrap();
        assert!(
            store
                .read_part_page(&record, "resource-0.bin", 0, 32, None)
                .is_err()
        );
        Ok(())
    }
}
