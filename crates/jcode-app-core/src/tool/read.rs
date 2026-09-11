#![cfg_attr(test, allow(clippy::items_after_test_module))]

use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use jcode_terminal_image::{ImageDisplayParams, ImageProtocol, display_image_bytes};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const DEFAULT_LIMIT: usize = 5000;

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ReadInput {
    file_path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    read_point: Option<String>,
    #[serde(default)]
    output_size: Option<jcode_tool_types::presentation::OutputSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadRangeStyle {
    OffsetLimit,
    StartEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalizedReadRange {
    offset: usize,
    limit: usize,
    style: ReadRangeStyle,
}

fn normalize_read_range(params: &ReadInput) -> Result<NormalizedReadRange> {
    let has_start_end = params.start_line.is_some() || params.end_line.is_some();
    let has_mixed_offset = match (params.start_line, params.end_line, params.offset) {
        (Some(start_line), _, Some(offset)) => {
            if start_line == 0 {
                true
            } else {
                offset.checked_add(1) != Some(start_line)
            }
        }
        (None, Some(_), Some(offset)) => offset != 0,
        _ => params.offset.is_some(),
    };

    if has_start_end && has_mixed_offset {
        return Err(anyhow::anyhow!(
            "Use either start_line/end_line (1-based) or offset (0-based), not both. `limit` may be used with either style."
        ));
    }

    if has_start_end {
        let start_line = params.start_line.unwrap_or(1);
        if start_line == 0 {
            return Err(anyhow::anyhow!(
                "start_line must be 1 or greater (it is 1-based)."
            ));
        }

        let limit = if let Some(end_line) = params.end_line {
            if end_line == 0 {
                return Err(anyhow::anyhow!(
                    "end_line must be 1 or greater (it is 1-based)."
                ));
            }
            if end_line < start_line {
                return Err(anyhow::anyhow!(
                    "end_line ({}) must be greater than or equal to start_line ({}).",
                    end_line,
                    start_line
                ));
            }
            end_line - start_line + 1
        } else {
            params.limit.unwrap_or(DEFAULT_LIMIT)
        };

        return Ok(NormalizedReadRange {
            offset: start_line - 1,
            limit,
            style: ReadRangeStyle::StartEnd,
        });
    }

    Ok(NormalizedReadRange {
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
        style: ReadRangeStyle::OffsetLimit,
    })
}

#[async_trait]
impl Tool for ReadTool {
    fn execution_policy(
        &self,
        input: &Value,
        _: &ToolContext,
    ) -> Result<jcode_tool_core::ExecutionPolicy> {
        let params: ReadInput = serde_json::from_value(input.clone())?;
        let media = is_image_file(Path::new(&params.file_path))
            || is_pdf_file(Path::new(&params.file_path));
        Ok(jcode_tool_core::ExecutionPolicy {
            capture: if media {
                jcode_tool_core::CaptureMode::Complete
            } else {
                jcode_tool_core::CaptureMode::SourceRead
            },
            cooperative_stop: true,
            ..Default::default()
        })
    }
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file. Supports text files, image files, and PDFs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "Path to a file."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based start line for text files."
                },
                "end_line": {
                    "type": "integer",
                    "description": "Inclusive upper line bound, also usable with read_point."
                },
                "read_point": {
                    "type": "string",
                    "description": "Exact continuation point from a previous read of this file. Replaces start_line/offset."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max text lines to read. Default 5000."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ReadInput = serde_json::from_value(input)?;
        anyhow::ensure!(
            params.read_point.is_none()
                || (params.start_line.is_none()
                    && params.offset.is_none()
                    && params.limit.is_none()),
            "read_point replaces start_line/offset/limit; use end_line for an upper bound"
        );
        let range = normalize_read_range(&params)?;
        anyhow::ensure!(
            range.limit > 0 && range.offset.checked_add(range.limit).is_some(),
            "Read line range must be positive and representable"
        );

        let path = ctx.resolve_path(Path::new(&params.file_path));
        let root = crate::storage::jcode_dir()?;
        let managed = crate::execution::reader::SourceReader::is_managed_path(&root, &path);

        // Check if file exists
        if !managed && !path.exists() {
            // Try to find similar files
            let suggestions = find_similar_files(&path);
            if suggestions.is_empty() {
                return Err(anyhow::anyhow!("File not found: {}", params.file_path));
            } else {
                return Err(anyhow::anyhow!(
                    "File not found: {}\nDid you mean: {}",
                    params.file_path,
                    suggestions.join(", ")
                ));
            }
        }

        // Check for image files and display in terminal if supported
        if is_image_file(&path) {
            anyhow::ensure!(
                params.read_point.is_none(),
                "Image reads are atomic; a text read point cannot select image pixels"
            );
            return tokio::task::spawn_blocking(move || {
                handle_image_file(&path, &params.file_path)
            })
            .await?;
        }

        // Check for PDF files and extract text
        if is_pdf_file(&path) {
            anyhow::ensure!(
                params.read_point.is_none(),
                "PDF continuation uses the retained derived-text file named in the prior result, not a second extraction of the original PDF"
            );
            let selected = (params.start_line.is_some()
                || params.end_line.is_some()
                || params.offset.is_some()
                || params.limit.is_some())
            .then_some(range);
            return tokio::task::spawn_blocking(move || {
                handle_pdf_file(&path, &params.file_path, selected)
            })
            .await?;
        }

        // Check for binary files
        if !managed && is_binary_file(&path) {
            return Ok(ToolOutput::new(format!(
                "Binary file detected: {}\nUse appropriate tools to handle binary files.",
                params.file_path
            )));
        }

        let target = ctx.invocation.output_target.unwrap_or_else(|| {
            crate::config::config()
                .output
                .target("read", params.output_size)
        });
        let end_line = if params.read_point.is_some() {
            params.end_line.map(|line| line as u64)
        } else {
            Some(
                range
                    .offset
                    .checked_add(range.limit)
                    .ok_or_else(|| anyhow::anyhow!("Read line range overflows"))?
                    as u64,
            )
        };
        anyhow::ensure!(range.limit > 0, "limit must be positive");
        let request = crate::execution::reader::ReadRequest {
            path: path.clone(),
            point: params.read_point,
            start_line: range.offset as u64 + 1,
            end_line,
            target,
            stop: ctx.graceful_shutdown_signal.clone(),
        };
        let mut output = tokio::task::spawn_blocking(move || {
            crate::execution::reader::SourceReader::new(&root).read(request)
        })
        .await??;
        if let jcode_tool_types::OutputSource::ReadPage(page) = &output.source
            && page.next_point.is_none()
            && page.end_byte < std::fs::metadata(&path)?.len()
        {
            let hint = match range.style {
                ReadRangeStyle::StartEnd => format!("start_line={}", page.end_line),
                ReadRangeStyle::OffsetLimit => format!("offset={}", page.end_line - 1),
            };
            output.output.push_str(&format!(
                "\n[Selected line range complete; use {hint} to continue.]"
            ));
        }
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id,
            path,
            op: FileOp::Read,
            intent: None,
            summary: Some("read source page".to_string()),
            detail: None,
        }));
        Ok(output)
    }
}

#[cfg(test)]
mod tests;

fn is_binary_file(path: &Path) -> bool {
    // Check by extension first (no I/O needed)
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        let binary_exts = [
            "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "zip", "tar", "gz", "bz2", "xz",
            "7z", "rar", "exe", "dll", "so", "dylib", "o", "a", "class", "pyc", "wasm", "mp3",
            "mp4", "avi", "mov", "mkv", "flac", "ogg", "wav",
        ];
        if binary_exts.contains(&ext.as_str()) {
            return true;
        }
    }

    // Read only the first 8KB to check for binary content (not the entire file)
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 8192];
        if let Ok(n) = file.read(&mut buf)
            && n > 0
        {
            let null_count = buf[..n].iter().filter(|&&b| b == 0).count();
            return null_count > n / 10;
        }
    }

    false
}

fn find_similar_files(path: &Path) -> Vec<String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_name().map(|s| s.to_string_lossy().to_lowercase());

    let mut suggestions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if let Some(ref target) = filename {
                // Simple similarity check
                let target_str: &str = target.as_ref();
                if name.contains(target_str) || target_str.contains(&name as &str) {
                    suggestions.push(entry.path().display().to_string());
                    if suggestions.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }

    suggestions
}

/// Check if a file is an image based on extension
fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
        )
    } else {
        false
    }
}

/// Handle reading an image file - display in terminal if supported AND return base64 for model vision
fn handle_image_file(path: &Path, file_path: &str) -> Result<ToolOutput> {
    let protocol = ImageProtocol::detect();

    const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;
    let source_size = std::fs::metadata(path)?.len();
    if source_size > MAX_IMAGE_SIZE {
        return Ok(ToolOutput::new(format!("Image: {file_path} ({source_size} bytes). Image exceeds the existing 20-MiB atomic vision limit; no pixels were sent. The source remains at the original path."))
            .with_error(true).with_metadata(json!({"source":file_path,"source_bytes":source_size,"vision_available":false})));
    }
    let (data, source_digest) = media_bytes(path, Some(MAX_IMAGE_SIZE))?;
    let file_size = data.len() as u64;

    let dimensions = get_image_dimensions_from_data(&data);

    let dim_str = dimensions
        .map(|(w, h)| format!("{}x{}", w, h))
        .unwrap_or_else(|| "unknown".to_string());

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    let mut terminal_displayed = false;
    if protocol.is_supported() {
        let params = ImageDisplayParams::from_terminal();
        match display_image_bytes(&data, path, &params) {
            Ok(true) => {
                terminal_displayed = true;
            }
            Ok(false) => {}
            Err(e) => {
                crate::logging::info(&format!("Warning: Failed to display image: {}", e));
            }
        }
    }

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "image/png",
    };

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
    let display_note = if terminal_displayed {
        "Displayed in terminal. "
    } else {
        ""
    };
    let mut output = ToolOutput::new(format!(
        "Image: {} ({})\nDimensions: {}\n{}Image attached for model vision analysis.",
        file_path, size_str, dim_str, display_note
    ))
    .with_labeled_image(media_type, b64, file_path.to_string());

    output = output.with_title(format!("📷 {}", file_path)).with_metadata(json!({"source":file_path,"source_bytes":file_size,"source_sha256":source_digest,"vision_available":true}));
    Ok(output)
}

/// Get image dimensions from raw data (duplicated from tui::image for convenience)
fn get_image_dimensions_from_data(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: check signature and parse IHDR chunk
    if data.len() > 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((width, height));
    }

    // JPEG: look for SOF0/SOF2 markers
    if data.len() > 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            // SOF0 (baseline) or SOF2 (progressive)
            if marker == 0xC0 || marker == 0xC2 {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            // Skip to next marker
            if i + 3 < data.len() {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    // GIF: parse header
    if data.len() > 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }

    None
}

/// Check if a file is a PDF based on extension
fn is_pdf_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        ext.to_string_lossy().to_lowercase() == "pdf"
    } else {
        false
    }
}

/// Read a complete media snapshot into the decoder, without archiving the source binary.
fn media_bytes(path: &Path, limit: Option<u64>) -> Result<(Vec<u8>, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let before = file.metadata()?;
    anyhow::ensure!(before.is_file(), "Media source must be a regular file");
    if let Some(limit) = limit {
        anyhow::ensure!(before.len() <= limit, "Media exceeds its atomic read limit");
    }
    let mut bytes = Vec::new();
    if let Some(limit) = limit {
        (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= limit,
            "Media grew beyond its atomic read limit"
        );
    } else {
        file.read_to_end(&mut bytes)?;
    }
    let after = file.metadata()?;
    anyhow::ensure!(
        before.len() == after.len()
            && before.modified()? == after.modified()?
            && bytes.len() as u64 == after.len(),
        "Media source changed during capture"
    );
    let digest = format!("{:x}", Sha256::digest(&bytes));
    Ok((bytes, digest))
}

#[cfg(feature = "pdf")]
fn handle_pdf_file(
    path: &Path,
    file_path: &str,
    selection: Option<NormalizedReadRange>,
) -> Result<ToolOutput> {
    let (bytes, digest) = media_bytes(path, None)?;
    let pages=match jcode_pdf::extract_pages(&bytes) {
        Ok(pages)=>pages,
        Err(error)=>return Ok(ToolOutput::new(format!("PDF text extraction failed for {file_path}: {error}. The original PDF was not copied into output storage."))
            .with_error(true).with_metadata(json!({"source":file_path,"source_bytes":bytes.len(),"source_sha256":digest,"decoder":"pdf-extract","extraction_complete":false}))),
    };
    let mut output = format!(
        "PDF: {file_path} ({} bytes)\nPages: {}\n",
        bytes.len(),
        pages.len()
    );
    for (index, page) in pages.iter().enumerate() {
        output.push_str(&format!("\n--- Page {} ---\n", index + 1));
        output.push_str(page);
        output.push('\n');
    }
    let selected =
        selection.map(|range| json!({"start_line":range.offset+1,"max_lines":range.limit}));
    if let Some(range) = selection {
        output = output
            .split_inclusive('\n')
            .skip(range.offset)
            .take(range.limit)
            .collect();
    }
    Ok(ToolOutput::new(output).with_metadata(json!({"source":file_path,"source_bytes":bytes.len(),"source_sha256":digest,"decoder":"pdf-extract","derived_text_format":1,"pages":pages.len(),"extraction_complete":true,"line_selection":selected})))
}

#[cfg(not(feature = "pdf"))]
fn handle_pdf_file(
    path: &Path,
    file_path: &str,
    _selection: Option<NormalizedReadRange>,
) -> Result<ToolOutput> {
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    Ok(ToolOutput::new(format!(
        "PDF: {} ({})\nPDF text extraction is not available in this build. Rebuild with the `pdf` feature enabled to extract text.",
        file_path, size_str
    )).with_error(true).with_metadata(json!({"source":file_path,"source_bytes":file_size,"extraction_complete":false,"supported":false})))
}
