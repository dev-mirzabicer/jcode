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
    pub(super) media_type: String,
    pub(super) label: Option<String>,
    pub(super) file: String,
    pub(super) raw_base64: bool,
    #[serde(default)]
    pub(super) integrity: Option<PartIntegrity>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    pub(super) schema: u32,
    pub(super) invocation_id: String,
    pub(super) title: Option<String>,
    pub(super) metadata: Option<serde_json::Value>,
    pub(super) images: Vec<ImagePart>,
    #[serde(default)]
    pub(super) resources: Vec<ResourcePart>,
    pub(super) source: OutputSource,
    #[serde(default)]
    pub(super) outcome: Option<RunState>,
    #[serde(default)]
    pub(super) text_sha256: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ResourcePart {
    pub uri: String,
    pub media_type: Option<String>,
    pub file: String,
    #[serde(default)]
    pub integrity: Option<PartIntegrity>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PartIntegrity {
    bytes: u64,
    sha256: String,
}
impl PartIntegrity {
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
    /// Recover only an already-sealed, identity-checked output. Missing or old
    /// manifests do not authorize repeating an operation with uncertain effects.
    pub fn recover_terminal_output(&self, id: &str) -> Result<RunRecord> {
        let _lease = super::storage::output_lease(self, id)?;
        let mut record = self.inspect(id)?.context("Unknown invocation")?;
        if record.state.terminal() {
            return Ok(record);
        }
        let local = self.root().join("receipts").join(format!("{id}.json"));
        let manifest_path = if local.exists() {
            local
        } else {
            self.root().join("outputs").join(id).join("manifest.json")
        };
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
                schema: 1,
                invocation_id: record.id.clone(),
                title: output.title.clone(),
                metadata: output.metadata.clone(),
                images: Vec::new(),
                resources: Vec::new(),
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
        ensure!(
            record.state.terminal(),
            "Invocation is still active; inspect or wait rather than reexecuting"
        );
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
                let mut file = File::open(&reference.path).context(
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
                output
                    .output
                    .push_str(&retained_notice(reference, prefix.bytes as u64));
            }
            OutputSource::Inline | OutputSource::Acceptance(_) => {
                anyhow::bail!("Invalid unretained result manifest")
            }
        }
        Ok(output)
    }
}

pub fn present(mut output: ToolOutput, target: NonZeroUsize) -> ToolOutput {
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
