use super::{ExecutionStore, RunRecord, RunState};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use jcode_tool_types::presentation::select_prefix;
use jcode_tool_types::{OutputReference, OutputSource, ToolImage, ToolOutput};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct ImagePart {
    media_type: String,
    label: Option<String>,
    file: String,
    raw_base64: bool,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    invocation_id: String,
    title: Option<String>,
    metadata: Option<serde_json::Value>,
    images: Vec<ImagePart>,
    source: OutputSource,
}

impl ExecutionStore {
    /// Capture precedes terminal publication, presentation and context guarding.
    /// A source read records only its position receipt, never its full source file.
    pub fn retain(
        &self,
        mut record: RunRecord,
        output: ToolOutput,
        state: RunState,
    ) -> Result<ToolOutput> {
        ensure!(
            state.terminal(),
            "Retention requires an actual terminal outcome"
        );
        let actual = self.inspect(&record.id)?.context("Unknown invocation")?;
        ensure!(
            actual.owner == record.owner && actual.state == RunState::Running,
            "Only the running invocation owner can publish output"
        );
        let directory = self.root().join("outputs").join(&record.id);
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            !std::fs::symlink_metadata(&directory)?
                .file_type()
                .is_symlink(),
            "New output directory may not be a symlink"
        );
        let mut images = Vec::new();
        for (index, image) in output.images.iter().enumerate() {
            let (bytes, raw_base64) =
                match base64::engine::general_purpose::STANDARD.decode(&image.data) {
                    Ok(bytes) => (bytes, false),
                    Err(_) => (image.data.as_bytes().to_vec(), true),
                };
            let file = format!(
                "image-{index}.{}",
                if raw_base64 { "base64" } else { "bin" }
            );
            durable_file(&directory.join(&file), &bytes)?;
            images.push(ImagePart {
                media_type: image.media_type.clone(),
                label: image.label.clone(),
                file,
                raw_base64,
            });
        }
        let mut output = output;
        if matches!(output.source, OutputSource::Inline) {
            let path = directory.join("output.txt");
            durable_file(&path, output.output.as_bytes())?;
            record.output_bytes = output.output.len() as u64;
            record.output_path = Some(path.clone());
            record.complete = true;
            output.source = OutputSource::Retained(OutputReference {
                invocation_id: record.id.clone(),
                path,
                bytes: record.output_bytes,
                complete: true,
            });
        } else if let OutputSource::Retained(reference) = &output.source {
            // A producer cannot manufacture an arbitrary path as a retention receipt.
            ensure!(
                reference.invocation_id == record.id
                    && actual.output_path.as_ref() == Some(&reference.path),
                "Retained output does not belong to this invocation"
            );
            record.output_path = actual.output_path;
            record.output_bytes = actual.output_bytes;
            record.complete = actual.complete;
        }
        let manifest = Manifest {
            schema: 1,
            invocation_id: record.id.clone(),
            title: output.title.clone(),
            metadata: output.metadata.clone(),
            images,
            source: output.source.clone(),
        };
        let manifest_path = directory.join("manifest.json");
        crate::storage::write_json_secret(&manifest_path, &manifest)?;
        record.result_path = Some(manifest_path);
        record.state = state;
        self.finish(&record)?;
        Ok(output)
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
        output.title = manifest.title;
        output.metadata = manifest.metadata;
        output.source = manifest.source;
        for part in manifest.images {
            ensure!(
                Path::new(&part.file).components().count() == 1 && !part.file.starts_with('.'),
                "Invalid retained media path"
            );
            let bytes = std::fs::read(directory.join(part.file))
                .context("Retained media is unavailable")?;
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
                (&mut file).take(bytes as u64).read_to_end(&mut buffer)?;
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
            OutputSource::Inline => anyhow::bail!("Invalid unretained result manifest"),
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
    format!(
        "\n[Retained output: {} ({} bytes, {} bytes shown). Read this file for more; do not repeat the operation. Run: {}]",
        reference.path.display(),
        reference.bytes,
        delivered_bytes,
        reference.invocation_id
    )
}

fn durable_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().context("Output path has no directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    jcode_core::fs::set_permissions_owner_only(temporary.path())?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
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
}
