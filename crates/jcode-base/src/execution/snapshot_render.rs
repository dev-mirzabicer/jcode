//! Streaming presentation of immutable snapshots. No whole-transcript/output
//! aggregation is required by the production inspection producer.
use super::*;
use std::io::Write;

fn field(writer: &mut impl Write, name: &str, value: &impl Serialize) -> Result<()> {
    writer.write_all(b",\n")?;
    serde_json::to_writer(&mut *writer, name)?;
    writer.write_all(b":")?;
    serde_json::to_writer(writer, value)?;
    Ok(())
}
fn begin(writer: &mut impl Write, header: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(header)?;
    ensure!(
        bytes.first() == Some(&b'{') && bytes.last() == Some(&b'}'),
        "Invalid inspection header"
    );
    writer.write_all(&bytes[..bytes.len() - 1])?;
    Ok(())
}
fn array_entry(writer: &mut impl Write, first: &mut bool, value: &impl Serialize) -> Result<()> {
    if !*first {
        writer.write_all(b",\n")?;
    }
    *first = false;
    serde_json::to_writer(writer, value)?;
    Ok(())
}
fn text_result(render: impl FnOnce(&mut Vec<u8>) -> Result<()>) -> Result<String> {
    let mut output = Vec::new();
    render(&mut output)?;
    Ok(String::from_utf8(output)?)
}

/// Escape bounded complete UTF-8 windows inside one JSON string. This preserves
/// exact content without materializing a large retained output in memory.
fn json_string(writer: &mut impl Write, mut reader: impl Read) -> Result<()> {
    writer.write_all(b"\"")?;
    let mut buffer = [0; 64 * 1024];
    let mut pending = Vec::with_capacity(buffer.len() + 4);
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            ensure!(
                pending.is_empty(),
                "Captured text ends inside a UTF-8 scalar"
            );
            break;
        }
        pending.extend_from_slice(&buffer[..count]);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(error.into()),
        };
        if valid > 0 {
            let escaped = serde_json::to_vec(std::str::from_utf8(&pending[..valid])?)?;
            writer.write_all(&escaped[1..escaped.len() - 1])?;
            pending.drain(..valid);
        }
    }
    writer.write_all(b"\"")?;
    Ok(())
}

impl SnapshotRead {
    fn blob<T: DeserializeOwned>(&self, digest: &str) -> Result<T> {
        self.store
            .inspection_blob_with_stop(digest, self.stop.as_ref())
    }
    pub fn target(&self) -> &str {
        &self.manifest.target
    }
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
    pub fn outline(&self) -> Result<String> {
        text_result(|writer| self.write_outline(writer))
    }
    pub fn transcript(&self, range: Option<&TranscriptRange>, raw: bool) -> Result<String> {
        text_result(|writer| self.write_transcript(writer, range, raw))
    }
    pub fn expand_tool(&self, reference: &str) -> Result<String> {
        text_result(|writer| self.write_tool_expansion(writer, reference))
    }

    pub fn write_outline(&self, mut writer: impl Write) -> Result<()> {
        begin(
            &mut writer,
            &serde_json::json!({"snapshot_id":self.manifest.id,"target":self.manifest.target,"captured_at":self.manifest.captured_at,"context_revision":self.manifest.context_revision,"message_count":self.manifest.messages.len()}),
        )?;
        writer.write_all(b",\n\"messages\":[")?;
        let mut first = true;
        for projected in &self.manifest.projected {
            let message: Message = self.blob(&projected.digest)?;
            let mut blocks = Vec::new();
            for block in &message.content {
                blocks.push(match block {
                    ContentBlock::Text{text,..}=>serde_json::json!({"type":"text","text":text}),
                    ContentBlock::ToolUse{id,name,input,..}=>{
                        let source_id=&self.manifest.messages[projected.start-1].id;
                        let tool=self.manifest.tools.iter().find(|tool|tool.message_id==*source_id && tool.provider_id==*id);
                        serde_json::json!({"type":"tool_use","name":name,"intent":input.get("intent").or_else(||input.get("description")),"tool_use_id":tool.map(|tool|&tool.reference),"state":tool.and_then(|tool|tool.run.as_ref()).map(|run|run.state)})
                    },
                    ContentBlock::ToolResult{tool_use_id,is_error,..}=>serde_json::json!({"type":"tool_result","provider_tool_use_id":tool_use_id,"is_error":is_error,"detail":"expand the snapshot-bound tool reference"}),
                    ContentBlock::Reasoning{text}|ContentBlock::ReasoningTrace{text}=>serde_json::json!({"type":"reasoning","characters":text.chars().count(),"detail":"read transcript for stored content"}),
                    ContentBlock::AnthropicThinking{thinking,..}=>serde_json::json!({"type":"signed_reasoning","characters":thinking.chars().count()}),
                    ContentBlock::OpenAIReasoning{summary,encrypted_content,..}=>serde_json::json!({"type":"reasoning","summary":summary,"encrypted_payload_present":encrypted_content.is_some()}),
                    ContentBlock::Image{media_type,..}=>serde_json::json!({"type":"image","media_type":media_type,"detail":"stored payload available in transcript"}),
                    ContentBlock::OpenAICompaction{..}=>serde_json::json!({"type":"encrypted_compaction","text_unavailable":true}),
                });
            }
            array_entry(
                &mut writer,
                &mut first,
                &serde_json::json!({"source_range":{"start":projected.start,"end":projected.end},"summary":projected.summary,"role":message.role,"blocks":blocks}),
            )?;
        }
        writer.write_all(b"],\n\"tools\":[")?;
        first = true;
        for tool in &self.manifest.tools {
            array_entry(
                &mut writer,
                &mut first,
                &serde_json::json!({"tool_use_id":tool.reference,"message_id":tool.message_id,"provider_id":tool.provider_id,"name":tool.name,"run":tool.run}),
            )?;
        }
        writer.write_all(b"]")?;
        let state: crate::session::StoredContextViewState =
            self.blob(&self.manifest.context_digest)?;
        let markers:Vec<_>=state.active_transactions().flat_map(|transaction|transaction.operations.iter().enumerate().map(move |(index,operation)|{
            let detail=match operation {
                jcode_session_types::StoredContextOperation::RangeSummary(summary)=>serde_json::json!({"kind":"range_summary","source_range":summary.source_range}),
                jcode_session_types::StoredContextOperation::ReasoningSuppression(suppression)=>serde_json::json!({"kind":"reasoning_suppression","targets":suppression.targets}),
                jcode_session_types::StoredContextOperation::ToolResultDistillation(distillation)=>serde_json::json!({"kind":"tool_result_distillation","target":distillation.target}),
            };
            serde_json::json!({"transaction_id":transaction.id,"operation_index":index,"detail":detail})
        })).collect();
        field(&mut writer, "context_transformations", &markers)?;
        writer.write_all(b"}\n")?;
        Ok(())
    }

    pub fn write_transcript(
        &self,
        mut writer: impl Write,
        range: Option<&TranscriptRange>,
        raw: bool,
    ) -> Result<()> {
        let count = self.manifest.messages.len();
        let (start, end) = range
            .map(|range| (range.start, range.end))
            .unwrap_or((1, count));
        ensure!(
            (count == 0 && range.is_none()) || (start > 0 && start <= end && end <= count),
            "Transcript range is outside the captured source message positions"
        );
        begin(
            &mut writer,
            &serde_json::json!({"snapshot_id":self.manifest.id,"raw":raw}),
        )?;
        writer.write_all(b",\n\"messages\":[")?;
        let mut first = true;
        if raw {
            for (index, source) in self.manifest.messages.iter().enumerate() {
                if index + 1 >= start && index < end {
                    let message: StoredMessage = self.blob(&source.digest)?;
                    array_entry(
                        &mut writer,
                        &mut first,
                        &serde_json::json!({"source_range":{"start":index+1,"end":index+1},"message":message}),
                    )?;
                }
            }
        } else {
            for projected in &self.manifest.projected {
                if projected.start <= end && projected.end >= start {
                    let message: Message = self.blob(&projected.digest)?;
                    array_entry(
                        &mut writer,
                        &mut first,
                        &serde_json::json!({"source_range":{"start":projected.start,"end":projected.end},"summary":projected.summary,"message":message}),
                    )?;
                }
            }
        }
        writer.write_all(b"]")?;
        field(
            &mut writer,
            "active_instructions",
            &self.blob::<serde_json::Value>(&self.manifest.instructions_digest)?,
        )?;
        writer.write_all(b"}\n")?;
        Ok(())
    }

    pub fn write_tool_expansion(&self, mut writer: impl Write, reference: &str) -> Result<()> {
        let tool=self.manifest.tools.iter().find(|tool|tool.reference==reference).context("Tool reference does not belong to this snapshot; use its outline reference, not an ambiguous provider ID")?;
        begin(
            &mut writer,
            &serde_json::json!({"snapshot_id":self.manifest.id,"tool_use_id":tool.reference,"provider_id":tool.provider_id,"name":tool.name,"as_of_run":tool.run}),
        )?;
        field(
            &mut writer,
            "input",
            &self.blob::<serde_json::Value>(&tool.input_digest)?,
        )?;
        if let Some(run) = &tool.run {
            let invocation = self.store.invocation_input(&run.id)?;
            ensure!(
                invocation.session_id == self.manifest.target
                    && invocation.message_id == tool.message_id,
                "Recorded execution input does not belong to this snapshot's tool scope"
            );
            field(&mut writer, "recorded_invocation_input", &invocation.input)?;
        }
        writer.write_all(b",\n\"retained_output\":")?;
        let available = if let Some(run) = &tool.run
            && let Some(path) = &run.output_path
        {
            let root = self
                .store
                .root()
                .parent()
                .context("Missing execution namespace")?;
            let source = super::super::managed_read::ManagedRead::open_with_stop(
                root,
                path,
                self.stop.as_ref(),
            )?
            .context("Captured output is unavailable")?;
            ensure!(
                source.length >= run.output_bytes,
                "Captured output lost its as-of prefix"
            );
            json_string(&mut writer, source.take(run.output_bytes))?;
            true
        } else {
            writer.write_all(b"null")?;
            false
        };
        writer.write_all(b",\n\"stored_results\":[")?;
        let mut first = true;
        for digest in &tool.legacy_results {
            array_entry(&mut writer, &mut first, &self.blob::<ContentBlock>(digest)?)?;
        }
        writer.write_all(b"]")?;
        field(&mut writer, "original_output_available", &available)?;
        field(
            &mut writer,
            "legacy_notice",
            &if tool.run.is_none() {
                Some(
                    "No retained execution identity. Stored results are exact received history, not proof of complete original producer output.",
                )
            } else {
                None
            },
        )?;
        writer.write_all(b"}\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_json_text_preserves_scalar_boundaries_quotes_and_line_endings() -> Result<()> {
        let text = format!("{}\"\\\r\n\tEND", "α🦀".repeat(40_000));
        let mut rendered = Vec::new();
        json_string(&mut rendered, text.as_bytes())?;
        assert_eq!(serde_json::from_slice::<String>(&rendered)?, text);
        Ok(())
    }
}
