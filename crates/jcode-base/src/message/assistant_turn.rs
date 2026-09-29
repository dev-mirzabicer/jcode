//! One owner for turning a provider stream into a stored assistant turn.
//!
//! Every turn loop (the agent's two loops, the TUI local loop, and the partial
//! checkpoints of both) feeds stream events into an [`AssistantTurnAssembler`]
//! and asks it for the stored content blocks. The assembler records segments in
//! arrival order, so what jcode stores (and later replays) is the order the
//! provider produced (INT-01 DESIGN §4.1, INV-3).
//!
//! Which reasoning blocks are kept for replay is decided by the replay kind of
//! the runtime that will receive the next request
//! ([`crate::provider::stored_reasoning_replay_kind`]):
//!
//! - Replayable blocks of that kind keep their positions. They are the only
//!   segments that split text: provider-signed Anthropic thinking, OpenAI
//!   reasoning items, and tool uses.
//! - Reasoning the runtime does not replay becomes a history-only
//!   `ReasoningTrace`, or `Reasoning` for runtimes that replay generic
//!   reasoning text. Within the stretch between two kept segments it is placed
//!   before the text, matching the live display.

use super::{ContentBlock, ToolCall};
use jcode_message_types::ReplayableReasoningBlock;
use jcode_provider_core::ContextReasoningBlockKind;

#[derive(Debug, Clone)]
enum Segment {
    Text(String),
    /// Readable reasoning streamed for display.
    Reasoning(String),
    /// A complete provider-signed block.
    Replayable(ReplayableReasoningBlock),
    /// An OpenAI Responses reasoning item.
    OpenAiReasoning(ContentBlock),
    /// A tool call, by id. Its final input comes from the caller's tool calls.
    ToolUse(String),
}

/// Accumulates one provider response into the assistant turn jcode stores.
#[derive(Debug, Clone, Default)]
pub struct AssistantTurnAssembler {
    segments: Vec<Segment>,
    /// Whether the last `Reasoning` segment is still receiving deltas.
    reasoning_open: bool,
}

impl AssistantTurnAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discard everything, for a provider retry that replays the response
    /// from the top.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A reasoning block started (`StreamEvent::ThinkingStart`).
    pub fn reasoning_started(&mut self) {
        self.segments.push(Segment::Reasoning(String::new()));
        self.reasoning_open = true;
    }

    /// Readable reasoning text (`StreamEvent::ThinkingDelta`).
    pub fn reasoning_delta(&mut self, text: &str) {
        match self.segments.last_mut() {
            Some(Segment::Reasoning(run)) if self.reasoning_open => run.push_str(text),
            _ => {
                self.segments.push(Segment::Reasoning(text.to_string()));
                self.reasoning_open = true;
            }
        }
    }

    /// The reasoning block ended (`StreamEvent::ThinkingEnd`).
    pub fn reasoning_ended(&mut self) {
        self.reasoning_open = false;
    }

    /// A complete signed block (`StreamEvent::ReplayableReasoning`). It
    /// replaces the display run that streamed the same block, so its text is
    /// stored once.
    pub fn replayable_reasoning(&mut self, block: ReplayableReasoningBlock) {
        if matches!(
            self.segments.last(),
            Some(Segment::Reasoning(run)) if run == block.readable_text()
        ) {
            self.segments.pop();
        }
        self.segments.push(Segment::Replayable(block));
        self.reasoning_open = false;
    }

    /// An OpenAI Responses reasoning item (`StreamEvent::OpenAIReasoning`).
    pub fn openai_reasoning(&mut self, item: ContentBlock) {
        self.segments.push(Segment::OpenAiReasoning(item));
        self.reasoning_open = false;
    }

    /// Visible answer text (`StreamEvent::TextDelta`, or harness-appended
    /// markers such as an interruption note).
    pub fn text(&mut self, text: &str) {
        self.reasoning_open = false;
        match self.segments.last_mut() {
            Some(Segment::Text(run)) => run.push_str(text),
            _ => self.segments.push(Segment::Text(text.to_string())),
        }
    }

    /// A tool call started (`StreamEvent::ToolUseStart`).
    pub fn tool_use_started(&mut self, id: &str) {
        self.reasoning_open = false;
        self.segments.push(Segment::ToolUse(id.to_string()));
    }

    /// All visible text, in order.
    pub fn visible_text(&self) -> String {
        self.segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether the response carried any readable reasoning.
    pub fn has_readable_reasoning(&self) -> bool {
        self.segments.iter().any(|segment| match segment {
            Segment::Reasoning(text) => !text.trim().is_empty(),
            Segment::Replayable(block) => !block.readable_text().trim().is_empty(),
            _ => false,
        })
    }

    /// Whether anything that would be stored has arrived.
    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|segment| match segment {
            Segment::Text(text) | Segment::Reasoning(text) => text.is_empty(),
            _ => false,
        })
    }

    /// Replace all visible text with `text`, at the position of the first text
    /// run (used when a tool call wrapped in text is recovered).
    pub fn replace_visible_text(&mut self, text: String) {
        let position = self
            .segments
            .iter()
            .position(|segment| matches!(segment, Segment::Text(_)))
            .unwrap_or(self.segments.len());
        let before = self.segments[..position]
            .iter()
            .filter(|segment| !matches!(segment, Segment::Text(_)))
            .count();
        self.segments
            .retain(|segment| !matches!(segment, Segment::Text(_)));
        if !text.is_empty() {
            self.segments.insert(before, Segment::Text(text));
        }
    }

    /// The stored assistant content for this response.
    ///
    /// `tool_calls` are the caller's finished calls, whose inputs and
    /// signatures are authoritative. Calls the stream never announced (for
    /// example one recovered from text) follow the streamed segments.
    pub fn content_blocks(
        &self,
        replay: Option<ContextReasoningBlockKind>,
        tool_calls: &[ToolCall],
    ) -> Vec<ContentBlock> {
        self.content_blocks_with(replay, tool_calls, |_| true)
    }

    /// [`Self::content_blocks`] keeping only the tool calls `include` accepts
    /// (a partial checkpoint keeps calls whose results were already received).
    pub fn content_blocks_with(
        &self,
        replay: Option<ContextReasoningBlockKind>,
        tool_calls: &[ToolCall],
        include: impl Fn(&ToolCall) -> bool,
    ) -> Vec<ContentBlock> {
        let keep_anthropic = replay == Some(ContextReasoningBlockKind::AnthropicThinking);
        let keep_openai = replay == Some(ContextReasoningBlockKind::OpenAiReasoning);
        let generic = replay == Some(ContextReasoningBlockKind::GenericReasoning);

        let mut gap = Gap::default();
        let mut blocks = Vec::new();
        let mut placed_tools = Vec::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(text) => gap.text.push_str(text),
                Segment::Reasoning(text) => gap.reasoning.push_str(text),
                Segment::Replayable(block) if keep_anthropic => {
                    gap.flush(&mut blocks, generic);
                    blocks.push(block.clone().into_content_block());
                }
                Segment::Replayable(block) => gap.reasoning.push_str(block.readable_text()),
                Segment::OpenAiReasoning(item) if keep_openai => {
                    gap.flush(&mut blocks, generic);
                    blocks.push(item.clone());
                }
                // Its readable summary arrived as reasoning deltas.
                Segment::OpenAiReasoning(_) => {}
                Segment::ToolUse(id) => {
                    placed_tools.push(id.as_str());
                    if let Some(call) = tool_calls.iter().find(|call| &call.id == id)
                        && include(call)
                    {
                        gap.flush(&mut blocks, generic);
                        blocks.push(tool_use_block(call));
                    }
                }
            }
        }
        gap.flush(&mut blocks, generic);
        for call in tool_calls {
            if !placed_tools.contains(&call.id.as_str()) && include(call) {
                blocks.push(tool_use_block(call));
            }
        }
        blocks
    }
}

/// Text and non-replayed reasoning between two kept segments.
#[derive(Default)]
struct Gap {
    reasoning: String,
    text: String,
}

impl Gap {
    fn flush(&mut self, blocks: &mut Vec<ContentBlock>, generic: bool) {
        let reasoning = std::mem::take(&mut self.reasoning);
        if !reasoning.trim().is_empty() {
            blocks.push(if generic {
                ContentBlock::Reasoning { text: reasoning }
            } else {
                ContentBlock::ReasoningTrace { text: reasoning }
            });
        }
        // Whitespace-only text is not an answer; providers reject empty text
        // blocks and the UI would show an empty reply.
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() {
            blocks.push(ContentBlock::Text {
                text,
                cache_control: None,
            });
        }
    }
}

fn tool_use_block(call: &ToolCall) -> ContentBlock {
    ContentBlock::ToolUse {
        id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
        thought_signature: call.thought_signature.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::AnthropicThinkingBinding;
    use serde_json::json;

    const ANTHROPIC: Option<ContextReasoningBlockKind> =
        Some(ContextReasoningBlockKind::AnthropicThinking);
    const OPENAI: Option<ContextReasoningBlockKind> =
        Some(ContextReasoningBlockKind::OpenAiReasoning);
    const GENERIC: Option<ContextReasoningBlockKind> =
        Some(ContextReasoningBlockKind::GenericReasoning);

    fn binding() -> AnthropicThinkingBinding {
        AnthropicThinkingBinding {
            model: "claude-opus-5-5".to_string(),
            prefix_digest: "anthropic-prefix-v1:test".to_string(),
            predecessor: None,
        }
    }

    fn thinking(text: &str, signature: &str) -> ReplayableReasoningBlock {
        ReplayableReasoningBlock::AnthropicThinking {
            thinking: text.to_string(),
            signature: signature.to_string(),
            binding: binding(),
        }
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: "bash".to_string(),
            input: json!({"command": "true"}),
            intent: None,
            thought_signature: Some(format!("{id}-signature")),
        }
    }

    /// Drive the assembler the way the Anthropic runtime's events do.
    fn anthropic_block(turn: &mut AssistantTurnAssembler, text: &str, signature: &str) {
        turn.reasoning_started();
        if !text.is_empty() {
            turn.reasoning_delta(text);
        }
        turn.replayable_reasoning(thinking(text, signature));
        turn.reasoning_ended();
    }

    fn kinds(blocks: &[ContentBlock]) -> Vec<String> {
        blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text, .. } => format!("text:{text}"),
                ContentBlock::Reasoning { text } => format!("reasoning:{text}"),
                ContentBlock::ReasoningTrace { text } => format!("trace:{text}"),
                ContentBlock::AnthropicThinking { signature, .. } => {
                    format!("thinking:{signature}")
                }
                ContentBlock::AnthropicRedactedThinking { data, .. } => format!("redacted:{data}"),
                ContentBlock::OpenAIReasoning { id, .. } => format!("openai:{id}"),
                ContentBlock::ToolUse { id, .. } => format!("tool:{id}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn interleaved_anthropic_turn() -> AssistantTurnAssembler {
        let mut turn = AssistantTurnAssembler::new();
        anthropic_block(&mut turn, "plan", "sig-1");
        turn.text("Looking. ");
        // A progress-update block with empty text under `display: omitted`.
        anthropic_block(&mut turn, "", "sig-2");
        turn.reasoning_started();
        turn.replayable_reasoning(ReplayableReasoningBlock::AnthropicRedactedThinking {
            data: "opaque".to_string(),
            binding: binding(),
        });
        turn.reasoning_ended();
        turn.tool_use_started("t1");
        turn
    }

    #[test]
    fn anthropic_blocks_are_stored_once_each_in_stream_order() {
        let turn = interleaved_anthropic_turn();
        let blocks = turn.content_blocks(ANTHROPIC, &[call("t1")]);
        assert_eq!(
            kinds(&blocks),
            vec![
                "thinking:sig-1",
                "text:Looking. ",
                "thinking:sig-2",
                "redacted:opaque",
                "tool:t1"
            ]
        );
        match &blocks[0] {
            ContentBlock::AnthropicThinking {
                thinking,
                binding: Some(stored),
                ..
            } => {
                assert_eq!(thinking, "plan");
                assert_eq!(stored, &binding());
            }
            other => panic!("expected bound thinking, got {other:?}"),
        }
        assert!(
            matches!(&blocks[4], ContentBlock::ToolUse { thought_signature: Some(s), .. } if s == "t1-signature")
        );
    }

    #[test]
    fn a_runtime_that_does_not_replay_thinking_keeps_it_as_history_only_text() {
        let turn = interleaved_anthropic_turn();
        let blocks = turn.content_blocks(None, &[call("t1")]);
        assert_eq!(
            kinds(&blocks),
            vec!["trace:plan", "text:Looking. ", "tool:t1"]
        );
    }

    #[test]
    fn unfinished_reasoning_stays_a_trace_even_on_a_replaying_runtime() {
        let mut turn = AssistantTurnAssembler::new();
        anthropic_block(&mut turn, "done", "sig-1");
        turn.reasoning_started();
        turn.reasoning_delta("cut off mid-block");
        let blocks = turn.content_blocks(ANTHROPIC, &[]);
        assert_eq!(
            kinds(&blocks),
            vec!["thinking:sig-1", "trace:cut off mid-block"]
        );
    }

    #[test]
    fn openai_items_keep_their_position_with_a_readable_trace_beside_them() {
        let mut turn = AssistantTurnAssembler::new();
        turn.reasoning_started();
        turn.reasoning_delta("summary");
        turn.reasoning_ended();
        turn.openai_reasoning(ContentBlock::OpenAIReasoning {
            id: "rs_1".to_string(),
            summary: vec!["summary".to_string()],
            encrypted_content: Some("enc".to_string()),
            status: None,
        });
        turn.text("answer");
        turn.tool_use_started("call_1");
        assert_eq!(
            kinds(&turn.content_blocks(OPENAI, &[call("call_1")])),
            vec!["trace:summary", "openai:rs_1", "text:answer", "tool:call_1"]
        );
        assert_eq!(
            kinds(&turn.content_blocks(None, &[call("call_1")])),
            vec!["trace:summary", "text:answer", "tool:call_1"]
        );
    }

    #[test]
    fn generic_reasoning_is_stored_as_replayable_text() {
        let mut turn = AssistantTurnAssembler::new();
        turn.reasoning_delta("first ");
        turn.reasoning_delta("second");
        turn.text("answer");
        assert_eq!(
            kinds(&turn.content_blocks(GENERIC, &[])),
            vec!["reasoning:first second", "text:answer"]
        );
    }

    #[test]
    fn whitespace_text_and_empty_reasoning_are_not_stored() {
        let mut turn = AssistantTurnAssembler::new();
        turn.reasoning_started();
        turn.reasoning_ended();
        turn.text("  \n ");
        assert!(turn.content_blocks(ANTHROPIC, &[]).is_empty());
        assert!(!turn.has_readable_reasoning());
    }

    #[test]
    fn tool_calls_follow_the_callers_final_calls() {
        let mut turn = AssistantTurnAssembler::new();
        turn.text("calling");
        turn.tool_use_started("finished");
        turn.tool_use_started("never_finished");
        let recovered = call("recovered");
        let calls = [call("finished"), recovered];
        assert_eq!(
            kinds(&turn.content_blocks(None, &calls)),
            vec!["text:calling", "tool:finished", "tool:recovered"],
            "an announced call that never finished is dropped; an unannounced call is appended"
        );
        assert_eq!(
            kinds(&turn.content_blocks_with(None, &calls, |call| call.id == "recovered")),
            vec!["text:calling", "tool:recovered"]
        );
    }

    #[test]
    fn replacing_visible_text_keeps_its_position() {
        let mut turn = AssistantTurnAssembler::new();
        anthropic_block(&mut turn, "plan", "sig-1");
        turn.text("prefix <tool_call>...</tool_call>");
        turn.replace_visible_text("prefix".to_string());
        assert_eq!(turn.visible_text(), "prefix");
        assert_eq!(
            kinds(&turn.content_blocks(ANTHROPIC, &[call("recovered")])),
            vec!["thinking:sig-1", "text:prefix", "tool:recovered"]
        );
    }

    #[test]
    fn reset_discards_a_rolled_back_attempt() {
        let mut turn = interleaved_anthropic_turn();
        assert!(!turn.is_empty());
        turn.reset();
        assert!(turn.is_empty());
        assert!(turn.content_blocks(ANTHROPIC, &[]).is_empty());
    }
}
