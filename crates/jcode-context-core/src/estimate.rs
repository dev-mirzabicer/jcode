use jcode_message_types::{ContentBlock, Message};

pub const APPROXIMATE_CHARS_PER_TOKEN: usize = 4;
pub const ESTIMATED_IMAGE_TOKENS: usize = 1_600;
pub const ESTIMATED_MESSAGE_OVERHEAD_TOKENS: usize = 4;

fn text_tokens(text: &str) -> usize {
    text.len().div_ceil(APPROXIMATE_CHARS_PER_TOKEN)
}

fn token_overhead_chars(tokens: usize) -> usize {
    tokens.saturating_mul(APPROXIMATE_CHARS_PER_TOKEN)
}

fn saturating_sum(values: impl IntoIterator<Item = usize>) -> usize {
    values
        .into_iter()
        .fold(0usize, |total, value| total.saturating_add(value))
}

/// Estimate provider-relevant character-equivalents for one block.
///
/// This deliberately excludes history-only reasoning traces and bounds images
/// independently of their base64 transport size. The result is diagnostic
/// accounting, not a tokenizer result; token decisions must use
/// [`estimate_content_block_tokens`].
pub fn estimate_content_block_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Reasoning { text } => text.len(),
        ContentBlock::ReasoningTrace { .. } => 0,
        ContentBlock::AnthropicThinking {
            thinking,
            signature,
        } => thinking
            .len()
            .saturating_add(signature.len())
            .saturating_add(token_overhead_chars(4)),
        ContentBlock::OpenAIReasoning {
            id,
            summary,
            encrypted_content,
            status,
        } => id
            .len()
            .saturating_add(saturating_sum(summary.iter().map(String::len)))
            .saturating_add(
                encrypted_content
                    .as_deref()
                    .map(str::len)
                    .unwrap_or_default(),
            )
            .saturating_add(status.as_deref().map(str::len).unwrap_or_default())
            .saturating_add(token_overhead_chars(6)),
        ContentBlock::ToolUse {
            id,
            name,
            input,
            thought_signature,
        } => id
            .len()
            .saturating_add(name.len())
            .saturating_add(
                serde_json::to_string(input)
                    .map(|input| input.len())
                    .unwrap_or_default(),
            )
            .saturating_add(
                thought_signature
                    .as_deref()
                    .map(str::len)
                    .unwrap_or_default(),
            )
            .saturating_add(token_overhead_chars(8)),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => tool_use_id
            .len()
            .saturating_add(content.len())
            .saturating_add(token_overhead_chars(6)),
        ContentBlock::Image { .. } => {
            ESTIMATED_IMAGE_TOKENS.saturating_mul(APPROXIMATE_CHARS_PER_TOKEN)
        }
        ContentBlock::OpenAICompaction { encrypted_content } => encrypted_content
            .len()
            .saturating_add(token_overhead_chars(4)),
    }
}

/// Estimate provider-input tokens for one block. History-only traces cost zero.
pub fn estimate_content_block_tokens(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Reasoning { text } => text_tokens(text),
        ContentBlock::ReasoningTrace { .. } => 0,
        ContentBlock::AnthropicThinking {
            thinking,
            signature,
        } => text_tokens(thinking)
            .saturating_add(text_tokens(signature))
            .saturating_add(4),
        ContentBlock::OpenAIReasoning {
            id,
            summary,
            encrypted_content,
            status,
        } => {
            let summary_tokens = saturating_sum(summary.iter().map(|summary| text_tokens(summary)));
            text_tokens(id)
                .saturating_add(summary_tokens)
                .saturating_add(
                    encrypted_content
                        .as_deref()
                        .map(text_tokens)
                        .unwrap_or_default(),
                )
                .saturating_add(status.as_deref().map(text_tokens).unwrap_or_default())
                .saturating_add(6)
        }
        ContentBlock::ToolUse {
            id,
            name,
            input,
            thought_signature,
        } => text_tokens(id)
            .saturating_add(text_tokens(name))
            .saturating_add(
                serde_json::to_string(input)
                    .map(|input| text_tokens(&input))
                    .unwrap_or_default(),
            )
            .saturating_add(
                thought_signature
                    .as_deref()
                    .map(text_tokens)
                    .unwrap_or_default(),
            )
            .saturating_add(8),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => text_tokens(tool_use_id)
            .saturating_add(text_tokens(content))
            .saturating_add(6),
        ContentBlock::Image { .. } => ESTIMATED_IMAGE_TOKENS,
        ContentBlock::OpenAICompaction { encrypted_content } => {
            text_tokens(encrypted_content).saturating_add(4)
        }
    }
}

pub fn estimate_content_blocks_chars(content: &[ContentBlock]) -> usize {
    let chars = saturating_sum(content.iter().map(estimate_content_block_chars));
    if chars == 0 {
        0
    } else {
        chars.saturating_add(token_overhead_chars(ESTIMATED_MESSAGE_OVERHEAD_TOKENS))
    }
}

pub fn estimate_content_blocks_tokens(content: &[ContentBlock]) -> usize {
    let tokens = saturating_sum(content.iter().map(estimate_content_block_tokens));
    if tokens == 0 {
        0
    } else {
        tokens.saturating_add(ESTIMATED_MESSAGE_OVERHEAD_TOKENS)
    }
}

pub fn estimate_message_chars(message: &Message) -> usize {
    estimate_content_blocks_chars(&message.content)
}

pub fn estimate_message_tokens(message: &Message) -> usize {
    estimate_content_blocks_tokens(&message.content)
}

pub fn estimate_messages_chars(messages: &[Message]) -> usize {
    saturating_sum(messages.iter().map(estimate_message_chars))
}

pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    saturating_sum(messages.iter().map(estimate_message_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::Role;

    #[test]
    fn trace_only_content_costs_zero_provider_tokens() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ReasoningTrace {
                text: "very long history-only trace".repeat(1_000),
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        assert_eq!(estimate_message_tokens(&message), 0);
        assert_eq!(estimate_message_chars(&message), 0);
    }

    #[test]
    fn image_estimate_is_bounded_independently_of_base64_size() {
        let small = ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "x".repeat(10),
        };
        let large = ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "x".repeat(1_000_000),
        };
        assert_eq!(
            estimate_content_block_tokens(&small),
            ESTIMATED_IMAGE_TOKENS
        );
        assert_eq!(
            estimate_content_block_tokens(&large),
            ESTIMATED_IMAGE_TOKENS
        );
        assert_eq!(
            estimate_content_block_chars(&small),
            ESTIMATED_IMAGE_TOKENS * APPROXIMATE_CHARS_PER_TOKEN
        );
        assert_eq!(
            estimate_content_block_chars(&large),
            ESTIMATED_IMAGE_TOKENS * APPROXIMATE_CHARS_PER_TOKEN
        );
    }

    #[test]
    fn aggregate_accounting_saturates_instead_of_overflowing() {
        assert_eq!(saturating_sum([usize::MAX, 1]), usize::MAX);
        assert_eq!(saturating_sum([usize::MAX - 1, 1]), usize::MAX);
    }
}
