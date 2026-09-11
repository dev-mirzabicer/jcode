//! One character-boundary policy for source reads and retained tool output.
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSizeAlias {
    VerySmall,
    Small,
    Medium,
    Large,
    VeryLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OutputSize {
    Characters(NonZeroUsize),
    Alias(OutputSizeAlias),
}

impl OutputSize {
    pub fn target(self) -> NonZeroUsize {
        match self {
            Self::Characters(value) => value,
            Self::Alias(alias) => NonZeroUsize::new(match alias {
                OutputSizeAlias::VerySmall => 10_000,
                OutputSizeAlias::Small => 20_000,
                OutputSizeAlias::Medium => 40_000,
                OutputSizeAlias::Large => 60_000,
                OutputSizeAlias::VeryLarge => 100_000,
            })
            .expect("alias targets are positive"),
        }
    }
}

pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "description": "Presentation character target or size alias. Read saved output for more.",
        "anyOf": [
            {"type": "integer", "minimum": 1},
            {"type": "string", "enum": ["very_small", "small", "medium", "large", "very_large"]}
        ]
    })
}

/// Largest useful scan window. Arithmetic does not wrap for caller-supplied targets.
pub fn scan_characters(target: NonZeroUsize) -> usize {
    ((target.get() as u128 * 13) / 10).min(usize::MAX as u128) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub bytes: usize,
    pub characters: usize,
}

/// `complete` says the input ends at the requested source's EOF/range boundary,
/// not merely the end of a producer's scan buffer. Only real EOF is a candidate.
pub fn select_prefix(text: &str, target: NonZeroUsize, complete: bool) -> Prefix {
    let nominal = target.get();
    let lower = (nominal as u128 * 7).div_ceil(10).min(usize::MAX as u128) as usize;
    let upper = scan_characters(target);
    let mut count = 0;
    let mut fallback = Prefix {
        bytes: 0,
        characters: 0,
    };
    let mut best: Option<Prefix> = None;
    for (byte, character) in text.char_indices() {
        count += 1;
        let candidate = Prefix {
            bytes: byte + character.len_utf8(),
            characters: count,
        };
        if count <= nominal {
            fallback = candidate;
        }
        if count > upper {
            break;
        }
        let boundary = character == '\n' || (complete && candidate.bytes == text.len());
        if boundary
            && count >= lower
            && best.is_none_or(|previous| {
                count.abs_diff(nominal) < previous.characters.abs_diff(nominal)
            })
        {
            best = Some(candidate);
        }
    }
    if complete && fallback.bytes == text.len() {
        fallback
    } else {
        best.unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str, n: usize) -> &str {
        &text[..select_prefix(text, NonZeroUsize::new(n).unwrap(), true).bytes]
    }

    #[test]
    fn aliases_and_raw_targets_reject_conflicting_or_invalid_shapes() {
        for (name, target) in [
            ("very_small", 10_000),
            ("small", 20_000),
            ("medium", 40_000),
            ("large", 60_000),
            ("very_large", 100_000),
        ] {
            let size: OutputSize = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(size.target().get(), target);
        }
        assert_eq!(
            serde_json::from_str::<OutputSize>("17")
                .unwrap()
                .target()
                .get(),
            17
        );
        for input in [
            "0",
            "-1",
            "1.5",
            "true",
            "{}",
            "[20,\"small\"]",
            "\"unknown\"",
        ] {
            assert!(
                serde_json::from_str::<OutputSize>(input).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn boundary_ties_prefer_shorter_and_eof_is_a_boundary() {
        assert_eq!(cut("1234567\nabc\ndefghijk", 10), "1234567\n");
        assert_eq!(cut("12345678901", 10), "12345678901");
        assert_eq!(cut("12345678901234", 10), "1234567890");
        assert_eq!(cut("tiny", 10), "tiny");
        assert_eq!(cut("", 1), "");
    }

    #[test]
    fn scan_buffer_end_is_not_eof() {
        let n = NonZeroUsize::new(10).unwrap();
        assert_eq!(select_prefix("12345678901", n, false).bytes, 10);
        assert_eq!(
            scan_characters(NonZeroUsize::new(usize::MAX).unwrap()),
            usize::MAX
        );
    }

    #[test]
    fn unicode_crlf_huge_lines_and_tiny_targets_reassemble_exactly() {
        for text in [
            "αβ🙂\r\n界e\u{301}\n".repeat(400),
            "🙂".repeat(100_000),
            "\n".repeat(100),
        ] {
            for n in [1, 2, 7, 17, 4000] {
                let mut remaining = text.as_str();
                let mut assembled = String::new();
                while !remaining.is_empty() {
                    let prefix = cut(remaining, n);
                    assert!(!prefix.is_empty());
                    assembled.push_str(prefix);
                    remaining = &remaining[prefix.len()..];
                }
                assert_eq!(assembled, text);
            }
        }
    }
}
