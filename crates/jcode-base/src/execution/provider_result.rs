//! One conversion of an already-received SDK result. Original protocol data
//! remains retained even when its media schema is unknown or malformed.
use jcode_tool_types::{ToolOutput, ToolResource};
use serde_json::{Value, json};

pub fn undecodable_sdk_record(bytes: &[u8], reason: &str) -> ToolOutput {
    use base64::Engine;
    let mut output=ToolOutput::new("Provider protocol record could not be decoded. Original received bytes are retained as a resource; no tool input or outcome was invented.").with_error(true)
        .with_metadata(json!({"provider_supplied":true,"protocol_error":reason,"received_bytes":bytes.len()}));
    output.resources.push(ToolResource {
        uri: "jcode:provider-undecoded-record".into(),
        media_type: Some("application/octet-stream".into()),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    output
}

pub fn received_sdk_result(
    id: &str,
    content: String,
    is_error: bool,
    original: Option<String>,
) -> ToolOutput {
    let mut output = ToolOutput::new(content).with_error(is_error);
    let mut metadata = json!({"provider_supplied":true});
    if let Some(original) = original {
        match serde_json::from_str::<Value>(&original) {
            Ok(value) => {
                let blocks = value.pointer("/message/content").and_then(Value::as_array);
                let selected: Vec<&Value> = if let Some(blocks) = blocks {
                    blocks
                        .iter()
                        .filter(|block| {
                            block["type"] == "tool_result" && block["tool_use_id"] == id
                        })
                        .collect()
                } else if value["type"] == "tool_result" && value["tool_use_id"] == id {
                    vec![&value]
                } else {
                    Vec::new()
                };
                for result in selected {
                    if let Some(parts) = result["content"].as_array() {
                        for part in parts {
                            match part["type"].as_str() {
                                Some("image") => {
                                    let source = part.get("source").unwrap_or(part);
                                    if let (Some(data), Some(media_type)) = (
                                        source["data"].as_str(),
                                        source
                                            .get("media_type")
                                            .or_else(|| source.get("mimeType"))
                                            .and_then(Value::as_str),
                                    ) {
                                        output = output.with_image(media_type, data);
                                    }
                                }
                                Some("resource") => {
                                    let resource = &part["resource"];
                                    if let (Some(uri), Some(data)) =
                                        (resource["uri"].as_str(), resource["blob"].as_str())
                                    {
                                        output.resources.push(ToolResource {
                                            uri: uri.into(),
                                            media_type: resource["mimeType"]
                                                .as_str()
                                                .map(Into::into),
                                            data: data.into(),
                                        });
                                    }
                                }
                                _ => {} // Unknown typed data remains in the complete original record.
                            }
                        }
                    }
                }
            }
            Err(error) => {
                metadata["normalization_error"] = json!(error.to_string());
                output.is_error = true;
            }
        }
        metadata["provider_original"] = Value::String(original);
    }
    output.metadata = Some(metadata);
    output
}

/// Used only when retention itself failed, preserving the received source in
/// the authoritative failure checkpoint rather than silently dropping it.
pub fn sdk_failure_body(output: &ToolOutput) -> String {
    match output
        .metadata
        .as_ref()
        .and_then(|meta| meta["provider_original"].as_str())
    {
        Some(original) => format!("{}\n[Original provider record]\n{original}", output.output),
        None => output.output.clone(),
    }
}

pub fn tool_result_blocks(
    id: String,
    output: ToolOutput,
) -> Vec<jcode_message_types::ContentBlock> {
    use jcode_message_types::ContentBlock;
    let mut blocks = vec![ContentBlock::ToolResult {
        tool_use_id: id,
        content: output.output,
        is_error: output.is_error.then_some(true),
    }];
    for image in output.images {
        blocks.push(ContentBlock::Image {
            media_type: image.media_type,
            data: image.data,
        });
        if let Some(label) = image.label.filter(|label| !label.trim().is_empty()) {
            blocks.push(ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {label}]"
                ),
                cache_control: None,
            });
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_original_record_and_matching_rich_parts_survive_conversion() {
        let original=json!({"type":"user","vendor":{"extra":17},"message":{"content":[
            {"type":"tool_result","tool_use_id":"other","content":[{"type":"image","source":{"data":"OTHER","media_type":"image/png"}}]},
            {"type":"tool_result","tool_use_id":"ours","extra":"kept","content":[{"type":"image","source":{"type":"base64","data":"eA==","media_type":"image/png"}},{"type":"resource","resource":{"uri":"fixture:resource","mimeType":"application/octet-stream","blob":"eQ=="}},{"type":"future","data":"unknown retained"}]}
        ]}}).to_string();
        let output =
            received_sdk_result("ours", "selected text".into(), true, Some(original.clone()));
        assert_eq!(
            output.metadata.as_ref().unwrap()["provider_original"],
            original
        );
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].data, "eA==");
        assert_eq!(output.resources.len(), 1);
        assert_eq!(output.resources[0].data, "eQ==");
        assert!(sdk_failure_body(&output).ends_with(&original));
        let history = tool_result_blocks("ours".into(), output);
        assert!(matches!(
            &history[0],
            jcode_message_types::ContentBlock::ToolResult {
                is_error: Some(true),
                ..
            }
        ));
        assert!(
            matches!(&history[1],jcode_message_types::ContentBlock::Image{data,..} if data=="eA==")
        );
    }
    #[test]
    fn malformed_original_is_preserved_as_a_failed_conversion() {
        let output = received_sdk_result(
            "ours",
            "received".into(),
            false,
            Some("invalid original".into()),
        );
        assert!(output.is_error);
        assert_eq!(
            output.metadata.unwrap()["provider_original"],
            "invalid original"
        );
    }
}
