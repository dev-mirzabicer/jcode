//! Lossless framing of the Anthropic SSE response body (INT-01/WP-06 R17).
//!
//! The body arrives as arbitrary byte chunks. A chunk boundary can fall inside
//! a multi-byte UTF-8 character, inside a line, or between the `\r` and `\n`
//! of a CRLF line ending. The decoder keeps raw bytes until a line is
//! complete, decodes each complete line as strict UTF-8 (a line ending byte
//! never occurs inside a multi-byte sequence), and dispatches an event at each
//! blank line, following the Server-Sent Events grammar: lines end with CRLF,
//! LF or CR; `data:` lines of one event are joined with `\n`; comment lines
//! start with `:`.

use anyhow::{Result, bail};

/// One dispatched server-sent event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) event_type: String,
    pub(crate) data: String,
}

/// Incremental decoder from body bytes to complete events.
#[derive(Default)]
pub(crate) struct SseDecoder {
    /// Bytes of the line in progress.
    line: Vec<u8>,
    /// The previous chunk ended on `\r`: a leading `\n` in the next chunk
    /// completes that CRLF and is not a second line ending.
    after_cr: bool,
    event_type: String,
    data: Option<String>,
}

impl SseDecoder {
    /// Feed one network chunk; returns every event it completed.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>> {
        let mut events = Vec::new();
        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' => self.end_line(&mut events)?,
                b'\r' => {
                    self.end_line(&mut events)?;
                    self.after_cr = true;
                }
                other => self.line.push(other),
            }
        }
        Ok(events)
    }

    /// The body ended. A partial line, or an event whose closing blank line
    /// never arrived, means the body was cut off.
    pub(crate) fn finish(&mut self) -> Result<()> {
        if !self.line.iter().all(u8::is_ascii_whitespace) {
            bail!(
                "{} body ended inside an unterminated line",
                crate::STREAM_INCOMPLETE
            );
        }
        if self.data.is_some() || !self.event_type.is_empty() {
            bail!(
                "{} body ended inside an event (`{}`) without its closing blank line",
                crate::STREAM_INCOMPLETE,
                self.event_type
            );
        }
        Ok(())
    }

    fn end_line(&mut self, events: &mut Vec<SseEvent>) -> Result<()> {
        let bytes = std::mem::take(&mut self.line);
        if bytes.is_empty() {
            // A blank line dispatches the event in progress, if any.
            let data = self.data.take();
            let event_type = std::mem::take(&mut self.event_type);
            if data.is_some() || !event_type.is_empty() {
                events.push(SseEvent {
                    event_type,
                    data: data.unwrap_or_default(),
                });
            }
            return Ok(());
        }
        let line = match String::from_utf8(bytes) {
            Ok(line) => line,
            Err(error) => bail!(
                "{} stream carried a line that is not valid UTF-8 ({error})",
                crate::STREAM_INCOMPLETE
            ),
        };
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event_type = value.to_string(),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            // `id`, `retry` and unknown fields carry nothing jcode uses.
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_in_pieces(body: &[u8], split: usize) -> Vec<SseEvent> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for piece in body.chunks(split.max(1)) {
            events.extend(decoder.push(piece).expect("valid body"));
        }
        decoder.finish().expect("complete body");
        events
    }

    #[test]
    fn every_byte_split_decodes_multibyte_text_exactly() {
        let body = "event: content_block_delta\ndata: {\"text\":\"café — 漢字 🦀\"}\n\n";
        let whole = decode_in_pieces(body.as_bytes(), body.len());
        for split in 1..body.len() {
            assert_eq!(
                decode_in_pieces(body.as_bytes(), split),
                whole,
                "split {split}"
            );
        }
        assert_eq!(whole[0].data, "{\"text\":\"café — 漢字 🦀\"}");
    }

    #[test]
    fn crlf_cr_and_lf_line_endings_frame_the_same_events() {
        let lf = "event: ping\ndata: {}\n\nevent: message_stop\ndata: {\"a\":1}\n\n";
        let expected = decode_in_pieces(lf.as_bytes(), lf.len());
        for ending in ["\r\n", "\r"] {
            let body = lf.replace('\n', ending);
            for split in 1..body.len() {
                assert_eq!(
                    decode_in_pieces(body.as_bytes(), split),
                    expected,
                    "ending {ending:?} split {split}"
                );
            }
        }
    }

    #[test]
    fn data_lines_join_and_comments_are_ignored() {
        let body = ": keepalive\nevent: x\ndata: a\ndata:b\n\n";
        assert_eq!(
            decode_in_pieces(body.as_bytes(), 3),
            vec![SseEvent {
                event_type: "x".to_string(),
                data: "a\nb".to_string()
            }]
        );
    }

    #[test]
    fn a_cut_off_body_is_incomplete() {
        for body in ["event: x\ndata: {\"partial", "event: x\ndata: {}\n"] {
            let mut decoder = SseDecoder::default();
            decoder.push(body.as_bytes()).unwrap();
            let error = decoder.finish().unwrap_err().to_string();
            assert!(error.contains(crate::STREAM_INCOMPLETE), "{error}");
        }
        let mut decoder = SseDecoder::default();
        decoder.push(b"event: x\ndata: {}\n\n  \n").unwrap();
        decoder.finish().unwrap();
    }

    #[test]
    fn invalid_utf8_is_a_fault_not_a_replacement_character() {
        let mut decoder = SseDecoder::default();
        let error = decoder.push(b"data: \xff\xfe\n\n").unwrap_err().to_string();
        assert!(error.contains("not valid UTF-8"), "{error}");
    }
}
