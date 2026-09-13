use super::{ExecutionStore, RunRecord, RunState};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use jcode_tool_types::presentation::select_prefix;
use jcode_tool_types::{OutputReference, OutputSource, ToolImage, ToolOutput};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(super) struct ImagePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded_file: Option<String>,
    pub(super) media_type: String,
    pub(super) label: Option<String>,
    pub(super) file: String,
    pub(super) raw_base64: bool,
    #[serde(default)]
    pub(super) integrity: Option<PartIntegrity>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    #[serde(default)]
    pub superseded: bool,
    #[serde(default)]
    pub provider_receipt: Option<jcode_tool_types::ProviderReceiptReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_exit: Option<jcode_tool_types::ProcessExit>,
    pub(super) schema: u32,
    pub(super) invocation_id: String,
    pub(super) title: Option<String>,
    pub(super) metadata: Option<serde_json::Value>,
    pub(super) images: Vec<ImagePart>,
    #[serde(default)]
    pub(super) resources: Vec<ResourcePart>,
    #[serde(default)]
    pub(super) parts: Vec<StoredPart>,
    pub(super) source: OutputSource,
    #[serde(default)]
    pub(super) outcome: Option<RunState>,
    #[serde(default)]
    pub(super) text_sha256: Option<String>,
}

// Ignore unbounded result metadata while locating declared parts. It remains
// retrievable as exact bytes in manifest.json, not allocated by this index view.
#[derive(Deserialize)]
pub(super) struct PartIndex {
    pub schema: u32,
    pub invocation_id: String,
    #[serde(default)]
    pub images: Vec<ImagePart>,
    #[serde(default)]
    pub resources: Vec<ResourcePart>,
    #[serde(default)]
    pub parts: Vec<StoredPart>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredPart {
    pub file: String,
    pub integrity: PartIntegrity,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ResourcePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded_file: Option<String>,
    pub uri: String,
    pub media_type: Option<String>,
    pub file: String,
    #[serde(default)]
    pub integrity: Option<PartIntegrity>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PartIntegrity {
    pub(super) bytes: u64,
    pub(super) sha256: String,
}
impl PartIntegrity {
    pub(super) fn read(mut reader: impl Read) -> Result<Self> {
        let mut bytes = 0u64;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes = bytes.checked_add(n as u64).context("Part size overflow")?;
            digest.update(&buffer[..n]);
        }
        Ok(Self {
            bytes,
            sha256: format!("{:x}", digest.finalize()),
        })
    }
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }
    fn verify(&self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.bytes == bytes.len() as u64
                && self.sha256 == format!("{:x}", Sha256::digest(bytes)),
            "Retained media/resource integrity mismatch"
        );
        Ok(())
    }
    fn verify_file(&self, path: &Path) -> Result<()> {
        ensure!(
            std::fs::symlink_metadata(path)?.is_file(),
            "Retained part changed type"
        );
        let mut file = File::open(path)?;
        ensure!(
            file.metadata()?.len() == self.bytes,
            "Retained part length changed"
        );
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        ensure!(
            self.sha256 == format!("{:x}", digest.finalize()),
            "Retained part digest changed"
        );
        Ok(())
    }
}

impl ExecutionStore {
    /// Resolve only a canonical retained image reference, never a generic .bin
    /// file by guesswork. Selection itself performs no filesystem acquisition.
    pub fn retained_image_selector(root: &Path, path: &Path) -> Option<(String, usize)> {
        let relative = path.strip_prefix(root.join("execution/outputs")).ok()?;
        let mut components = relative.components();
        let std::path::Component::Normal(id) = components.next()? else {
            return None;
        };
        let std::path::Component::Normal(file) = components.next()? else {
            return None;
        };
        if components.next().is_some() {
            return None;
        }
        let id = id.to_str()?;
        let file = file.to_str()?;
        if id.len() != 68
            || !id.starts_with("run-")
            || !id[4..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        let index: usize = file
            .strip_prefix("image-")?
            .strip_suffix(".bin")?
            .parse()
            .ok()?;
        if file != format!("image-{index}.bin") {
            return None;
        }
        Some((id.into(), index))
    }

    /// Read one atomic image with its declared media type and original integrity.
    /// Both aliases and physical archive identity are checked by the storage owner.
    pub fn retained_image(&self, path: &Path, max_bytes: u64) -> Result<(Vec<u8>, String)> {
        let root = self.root().parent().context("Invalid execution root")?;
        let (id, index) =
            Self::retained_image_selector(root, path).context("Invalid retained image path")?;
        let record = self
            .inspect(&id)?
            .context("Retained image invocation is unavailable")?;
        ensure!(
            record.state.terminal(),
            "Image capture is not sealed; wait for the original operation"
        );
        let expected = self.root().join("outputs").join(&id);
        ensure!(
            record.result_path.as_ref() == Some(&expected.join("manifest.json")),
            "Retained image has no canonical bundle manifest"
        );
        let manifest: PartIndex = serde_json::from_reader(std::io::BufReader::new(
            self.open_output_part(&id, "manifest.json")?,
        ))?;
        ensure!(
            manifest.schema == 1 && manifest.invocation_id == id,
            "Retained image manifest identity changed"
        );
        let part = manifest
            .images
            .get(index)
            .context("Retained image index is unavailable")?;
        ensure!(
            part.media_type.starts_with("image/"),
            "Retained part is not declared as image media"
        );
        let binary = format!("image-{index}.bin");
        ensure!(
            part.decoded_file
                .as_ref()
                .is_none_or(|file| file == &binary),
            "Retained decoded image descriptor changed"
        );
        let mut file = self.open_output_part(&id, &binary)?;
        ensure!(
            file.metadata()?.len() <= max_bytes,
            "Retained image exceeds the atomic vision limit; no pixels were acquired"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= max_bytes,
            "Retained image grew beyond the atomic vision limit"
        );
        let integrity = part
            .integrity
            .as_ref()
            .context("Retained image has no original integrity receipt")?;
        if part.raw_base64 {
            ensure!(
                part.file == format!("image-{index}.base64"),
                "Retained image encoding path changed"
            );
            let limit = max_bytes
                .saturating_add(2)
                .saturating_div(3)
                .saturating_mul(4);
            ensure!(
                integrity.bytes <= limit,
                "Retained image encoding exceeds the atomic vision limit"
            );
            let mut original = Vec::new();
            self.open_output_part(&id, &part.file)?
                .take(limit.saturating_add(1))
                .read_to_end(&mut original)?;
            integrity.verify(&original)?;
            ensure!(
                base64::engine::general_purpose::STANDARD.decode(&original)? == bytes,
                "Retained decoded image differs from the original captured bytes"
            );
        } else {
            ensure!(part.file == binary, "Retained image byte path changed");
            integrity.verify(&bytes)?;
        }
        Ok((bytes, part.media_type.clone()))
    }

    /// Recover only an already-sealed, identity-checked output. Missing or old
    /// manifests do not authorize repeating an operation with uncertain effects.
    pub fn recover_terminal_output(&self, id: &str) -> Result<RunRecord> {
        let lease = super::storage::output_lease(self, id)?;
        self.recover_terminal_under_lease(id, &lease)
    }
    pub(super) fn terminal_witness(&self, id: &str) -> Result<Option<std::path::PathBuf>> {
        let local = self.root().join("receipts").join(format!("{id}.json"));
        if local.try_exists()? {
            return Ok(Some(local));
        }
        let bundle = self.root().join("outputs").join(id).join("manifest.json");
        Ok(bundle.try_exists()?.then_some(bundle))
    }
    pub(super) fn recover_terminal_under_lease(
        &self,
        id: &str,
        lease: &super::storage::OutputLease,
    ) -> Result<RunRecord> {
        lease.validate(self, id)?;
        let mut record = self.inspect(id)?.context("Unknown invocation")?;
        if record.state.terminal() {
            return Ok(record);
        }
        let manifest_path = self.terminal_witness(id)?.context(
            "No sealed output is available; do not automatically repeat the original operation",
        )?;
        let manifest: Manifest = crate::storage::read_json(&manifest_path).context(
            "No sealed output is available; do not automatically repeat the original operation",
        )?;
        ensure!(
            manifest.schema == 1 && manifest.invocation_id == id,
            "Terminal output identity mismatch"
        );
        let outcome = manifest
            .outcome
            .context("Legacy output has no proven terminal outcome")?;
        ensure!(
            outcome.terminal(),
            "Output manifest has no terminal outcome"
        );
        if let OutputSource::Retained(reference) = &manifest.source {
            let expected = self.root().join("outputs").join(id).join("output.txt");
            ensure!(
                reference.invocation_id == id
                    && reference.path == expected
                    && record.output_path.as_ref() == Some(&expected),
                "Terminal output reference differs from its invocation"
            );
            if reference.complete {
                let mut file = File::open(&expected).context("Sealed output is offline")?;
                ensure!(
                    file.metadata()?.len() == reference.bytes,
                    "Sealed output length changed"
                );
                let mut hash = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buffer[..n]);
                }
                ensure!(
                    manifest.text_sha256.as_deref()
                        == Some(format!("{:x}", hash.finalize()).as_str()),
                    "Sealed output digest changed"
                );
                for image in &manifest.images {
                    let part = Path::new(&image.file);
                    ensure!(
                        part.components().count() == 1 && !image.file.starts_with('.'),
                        "Invalid retained media path"
                    );
                    if let Some(integrity) = &image.integrity {
                        integrity.verify_file(&expected.parent().unwrap().join(part))?;
                    }
                    ensure!(
                        std::fs::symlink_metadata(expected.parent().unwrap().join(part))?.is_file(),
                        "Sealed media is unavailable or changed type"
                    );
                }
                for resource in &manifest.resources {
                    let part = Path::new(&resource.file);
                    ensure!(
                        part.components().count() == 1 && !resource.file.starts_with('.'),
                        "Invalid retained resource path"
                    );
                    if let Some(integrity) = &resource.integrity {
                        integrity.verify_file(&expected.parent().unwrap().join(part))?;
                    }
                    ensure!(
                        std::fs::symlink_metadata(expected.parent().unwrap().join(part))?.is_file(),
                        "Sealed resource is unavailable or changed type"
                    );
                }
                for part in &manifest.parts {
                    ensure!(
                        Path::new(&part.file).components().count() == 1
                            && !part.file.starts_with('.'),
                        "Invalid retained part name"
                    );
                    part.integrity
                        .verify_file(&expected.parent().unwrap().join(&part.file))?;
                }
            } else {
                ensure!(
                    outcome != RunState::Completed,
                    "Incomplete capture cannot recover as completed"
                );
            }
            record.output_bytes = reference.bytes;
            record.complete = reference.complete;
        } else {
            ensure!(
                matches!(manifest.source, OutputSource::ReadPage(_)),
                "Unretained output cannot be recovered"
            );
        }
        record.state = outcome;
        record.process_exit = manifest.process_exit;
        record.superseded = manifest.superseded;
        record.result_path = Some(manifest_path);
        self.finish(&record)?;
        Ok(record)
    }

    /// Capture precedes terminal publication, presentation and context guarding.
    /// A source read records only its position receipt, never its full source file.
    pub fn retain(
        &self,
        mut record: RunRecord,
        output: ToolOutput,
        state: RunState,
    ) -> Result<ToolOutput> {
        if matches!(output.source, OutputSource::ReadPage(_)) {
            ensure!(
                state.terminal(),
                "Source-read receipt requires a terminal outcome"
            );
            let actual = self.inspect(&record.id)?.context("Unknown invocation")?;
            ensure!(
                actual.owner == record.owner && actual.state == RunState::Running,
                "Source read is not owned and running"
            );
            ensure!(
                output.images.is_empty() && output.resources.is_empty(),
                "Atomic media requires a media-read receipt, not a text read point"
            );
            let directory = self.root().join("receipts");
            crate::storage::ensure_dir(&directory)?;
            let path = directory.join(format!("{}.json", record.id));
            let manifest = Manifest {
                process_exit: output.process_exit.clone(),
                superseded: output.superseded,
                provider_receipt: output.provider_receipt.clone(),
                schema: 1,
                invocation_id: record.id.clone(),
                title: output.title.clone(),
                metadata: output.metadata.clone(),
                images: Vec::new(),
                resources: Vec::new(),
                parts: Vec::new(),
                source: output.source.clone(),
                outcome: Some(state),
                text_sha256: None,
            };
            crate::storage::write_json_secret(&path, &manifest)?;
            record.result_path = Some(path);
            record.state = state;
            self.finish(&record)?;
            return Ok(output);
        }
        let capture = super::Capture::create(
            self.clone(),
            record,
            crate::config::config().output.storage.clone(),
        )?;
        capture.seal(output, state)
    }

    /// Read a prior terminal outcome. Never call the original producer here.
    pub fn result(&self, record: &RunRecord, target: NonZeroUsize) -> Result<ToolOutput> {
        self.ensure_output_not_deleted(&record.id)?;
        ensure!(
            record.state.terminal(),
            "Invocation is still active; inspect or wait rather than reexecuting"
        );
        if record.state == RunState::Interrupted && record.result_path.is_none() {
            return self.interrupted_result(record, target);
        }
        let manifest_path = record.result_path.as_ref().context("Invocation was interrupted before a retained result was published; do not automatically repeat its effects")?;
        let manifest: Manifest = crate::storage::read_json(manifest_path)
            .context("Retained result is offline or unavailable")?;
        ensure!(
            manifest.schema == 1 && manifest.invocation_id == record.id,
            "Retained result identity mismatch"
        );
        let directory = manifest_path
            .parent()
            .context("Invalid output manifest path")?;
        let mut output = ToolOutput::new("");
        output.process_exit = record.process_exit.clone();
        output.superseded = record.superseded;
        output.provider_receipt = manifest.provider_receipt.clone();
        output.is_error = record.state != RunState::Completed;
        output.title = manifest.title;
        output.metadata = manifest.metadata;
        output.source = manifest.source;
        if let OutputSource::Retained(reference) = &mut output.source {
            reference.manifest_path = record.result_path.clone();
        }
        for part in manifest.resources {
            ensure!(
                Path::new(&part.file).components().count() == 1 && !part.file.starts_with('.'),
                "Invalid retained resource path"
            );
            let bytes = std::fs::read(directory.join(part.file))
                .context("Retained resource is unavailable")?;
            if let Some(integrity) = part.integrity {
                integrity.verify(&bytes)?;
            }
            output.resources.push(jcode_tool_types::ToolResource {
                uri: part.uri,
                media_type: part.media_type,
                data: String::from_utf8(bytes)?,
            });
        }
        for part in manifest.images {
            ensure!(
                Path::new(&part.file).components().count() == 1 && !part.file.starts_with('.'),
                "Invalid retained media path"
            );
            let bytes = std::fs::read(directory.join(part.file))
                .context("Retained media is unavailable")?;
            if let Some(integrity) = part.integrity {
                integrity.verify(&bytes)?;
            }
            let data = if part.raw_base64 {
                String::from_utf8(bytes)?
            } else {
                base64::engine::general_purpose::STANDARD.encode(bytes)
            };
            output.images.push(ToolImage {
                media_type: part.media_type,
                data,
                label: part.label,
            });
        }
        let mut continuation = None;
        match &output.source {
            OutputSource::ReadPage(page) => {
                output.output = format!(
                    "Source read receipt retained. Read file_path=\"{}\", read_point=\"{}\" to retrieve the undelivered page. The original file was not archived.",
                    page.path.display(),
                    page.retry_point
                );
            }
            OutputSource::Retained(reference) => {
                ensure!(
                    record.output_path.as_ref() == Some(&reference.path),
                    "Output reference differs from invocation metadata"
                );
                let window = jcode_tool_types::presentation::scan_characters(target);
                let bytes = window.saturating_add(1).saturating_mul(4);
                let state_root = self
                    .root()
                    .parent()
                    .context("Missing execution state root")?;
                let mut file = super::managed_read::ManagedRead::open(state_root, &reference.path)?
                    .context(
                        "Retained output is offline or unavailable; no operation was repeated",
                    )?;
                let mut buffer = Vec::new();
                (&mut file)
                    .take((bytes as u64).min(reference.bytes))
                    .read_to_end(&mut buffer)?;
                // A bounded byte window can end in the middle of a scalar.
                let text = match std::str::from_utf8(&buffer) {
                    Ok(text) => text,
                    Err(error) if error.error_len().is_none() => {
                        std::str::from_utf8(&buffer[..error.valid_up_to()])?
                    }
                    Err(error) => return Err(error.into()),
                };
                let complete = buffer.len() as u64 == reference.bytes;
                let prefix = select_prefix(text, target, complete);
                output.output = text[..prefix.bytes].to_string();
                let point = if (prefix.bytes as u64) < reference.bytes {
                    Some(
                        super::reader::SourceReader::new(state_root)
                            .output_continuation(&reference.path, &output.output)?,
                    )
                } else {
                    None
                };
                output
                    .output
                    .push_str(&retained_notice(reference, prefix.bytes as u64));
                if let Some(point) = &point {
                    output.output.push_str(&format!(
                        "\n[Continue exactly: read the same file_path with read_point=\"{point}\".]"
                    ));
                }
                continuation = point;
            }
            OutputSource::Inline | OutputSource::Acceptance(_) | OutputSource::Unavailable(_) => {
                anyhow::bail!("Invalid unretained result manifest")
            }
        }
        if let OutputSource::Retained(reference) = &mut output.source {
            reference.continuation = continuation;
        }
        Ok(output)
    }
}

pub fn present(mut output: ToolOutput, target: NonZeroUsize) -> ToolOutput {
    if let OutputSource::Unavailable(reference) = &output.source {
        let prefix = select_prefix(&output.output, target, true);
        output.output.truncate(prefix.bytes);
        output.output.push_str(&format!("\n[Interruption receipt: {}. Run: {}. Do not repeat the original operation to retrieve its output.]",reference.receipt_path.display(),reference.invocation_id));
    }
    if let OutputSource::Retained(reference) = &output.source {
        let prefix = select_prefix(&output.output, target, true);
        output.output.truncate(prefix.bytes);
        output
            .output
            .push_str(&retained_notice(reference, prefix.bytes as u64));
    }
    output
}

fn retained_notice(reference: &OutputReference, delivered_bytes: u64) -> String {
    let mut notice = format!(
        "\n[Retained output: {} ({} bytes, {} bytes shown). Read this file for more; do not repeat the operation. Run: {}]",
        reference.path.display(),
        reference.bytes,
        delivered_bytes,
        reference.invocation_id
    );
    if let Some(path) = &reference.manifest_path {
        notice.push_str(&format!(
            "\n[Metadata and resource parts: {}]",
            path.display()
        ));
    }
    notice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation};

    #[test]
    fn full_text_rich_metadata_and_invalid_media_survive_presentation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let call = Invocation {
            session_id: "s".into(),
            message_id: "m".into(),
            call_path: vec!["c".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&call, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let text = format!("{}TAIL", "🙂".repeat(40_000));
        let output = ToolOutput::new(&text)
            .with_metadata(serde_json::json!({"structured": [1,2,3]}))
            .with_image("image/png", "invalid base64 retained exactly");
        let retained = store.retain(record.clone(), output, RunState::Completed)?;
        let target = NonZeroUsize::new(20).unwrap();
        assert!(!present(retained, target).output.contains("TAIL"));
        let saved = store.inspect(&record.id)?.unwrap();
        assert_eq!(
            std::fs::read_to_string(saved.output_path.as_ref().unwrap())?,
            text
        );
        let restored = store.result(&saved, target)?;
        assert_eq!(restored.images[0].data, "invalid base64 retained exactly");
        assert_eq!(
            restored.metadata.unwrap()["structured"],
            serde_json::json!([1, 2, 3])
        );
        Ok(())
    }

    #[test]
    fn changed_image_or_resource_bytes_cannot_be_returned_as_the_original() -> Result<()> {
        for part in ["image-0.base64", "resource-0.base64"] {
            let dir = tempfile::tempdir()?;
            let store = ExecutionStore::open(dir.path())?;
            let input = Invocation {
                session_id: "integrity".into(),
                message_id: "message".into(),
                call_path: vec!["call".into()],
                tool: "fixture".into(),
                input: serde_json::json!({}),
                working_dir: None,
                received_result_digest: None,
            };
            let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
                panic!()
            };
            store.start(&record.id, "owner")?;
            let mut output = ToolOutput::new("body").with_image("image/png", "YWJj");
            output.resources.push(jcode_tool_types::ToolResource {
                uri: "fixture://part".into(),
                media_type: None,
                data: "ZGVm".into(),
            });
            store.retain(record.clone(), output, RunState::Completed)?;
            let saved = store.inspect(&record.id)?.unwrap();
            std::fs::write(
                saved.output_path.as_ref().unwrap().with_file_name(part),
                b"eHl6",
            )?;
            assert!(
                store
                    .result(&saved, NonZeroUsize::new(100).unwrap())
                    .is_err(),
                "{part}"
            );
        }
        Ok(())
    }

    #[test]
    fn declared_error_result_is_not_published_as_completed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = ExecutionStore::open(dir.path())?;
        let input = Invocation {
            session_id: "error".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        store.retain(
            record.clone(),
            ToolOutput::new("producer error").with_error(true),
            RunState::Completed,
        )?;
        let saved = store.inspect(&record.id)?.unwrap();
        assert_eq!(saved.state, RunState::Failed);
        assert!(
            store
                .result(&saved, NonZeroUsize::new(100).unwrap())?
                .is_error
        );
        Ok(())
    }
}
