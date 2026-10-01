//! Prompt-cache breakpoints for a Messages request (INT-01 DESIGN §6).
//!
//! Anthropic caches a strict prefix, rendered tools, then system, then
//! messages, and a breakpoint caches everything up to and including the block
//! it sits on. Between recorded transitions jcode's requests are append-only
//! (INV-1): each request is the previous one with the model's response and
//! the new user-role content appended. The markers follow from that:
//!
//! 1. **Static prefix.** The last system block, or the last tool when there is
//!    no system. Tools render before system, so one marker covers both.
//! 2. **Newest content.** The last block of the newest message, written for
//!    the next request.
//! 3. **Previous newest.** The last block of the user (or operator system)
//!    message before the latest assistant response, which is where the previous request's newest
//!    marker sat. The request reads exactly the entry the previous request
//!    wrote, however much it appended.
//! 4. **Lookback.** A breakpoint finds an earlier entry at most
//!    [`LOOKBACK_POSITIONS`] positions back. When more than
//!    [`INTERMEDIATE_AFTER_POSITIONS`] positions separate markers 3 and 2, one
//!    more marker sits that far before the newest block. An entry written
//!    between them, by a request that did not end where rule 3 expects (for
//!    example a request whose response failed), is then still reached.
//!
//! That is at most four markers, the API's limit. Every marker carries the
//! same TTL, which keeps the rule that 1h entries precede 5m entries. Thinking
//! blocks cannot carry a marker; a marker moves back to the nearest block that
//! can.

use crate::{ApiContentBlock, ApiMessage, ApiRequest, ApiSystem, CacheControlParam};

/// How far back a breakpoint looks for an earlier cache entry, in positions.
/// A run of consecutive `tool_use` blocks, or of consecutive `tool_result`
/// blocks, is one position.
pub const LOOKBACK_POSITIONS: usize = 20;

/// The gap, in positions, between the previous request's newest block and
/// this request's newest block above which an intermediate marker is placed.
/// It leaves a margin inside [`LOOKBACK_POSITIONS`].
pub const INTERMEDIATE_AFTER_POSITIONS: usize = 15;

/// Place every prompt-cache breakpoint of `request`, replacing any marker
/// already present, so the result depends only on the request content.
pub fn place_cache_breakpoints(request: &mut ApiRequest, cache_ttl_1h: bool) {
    clear_markers(request);
    let marker = CacheControlParam::ephemeral(cache_ttl_1h);

    let system_block = match request.system.as_mut() {
        Some(ApiSystem::Blocks(blocks)) => blocks.last_mut(),
        None => None,
    };
    if let Some(block) = system_block {
        block.cache_control = Some(marker.clone());
    } else if let Some(tool) = request.tools.as_mut().and_then(|tools| tools.last_mut()) {
        tool.cache_control = Some(marker.clone());
    }

    for (message, block) in message_breakpoints(&request.messages) {
        if let Some(slot) = request.messages[message].content[block].cache_control_mut() {
            *slot = Some(marker.clone());
        }
    }
}

fn clear_markers(request: &mut ApiRequest) {
    if let Some(ApiSystem::Blocks(blocks)) = request.system.as_mut() {
        for block in blocks {
            block.cache_control = None;
        }
    }
    for tool in request.tools.iter_mut().flatten() {
        tool.cache_control = None;
    }
    for block in request
        .messages
        .iter_mut()
        .flat_map(|message| &mut message.content)
    {
        if let Some(slot) = block.cache_control_mut() {
            *slot = None;
        }
    }
}

/// One content block in request order, with its lookback position.
struct Located {
    message: usize,
    block: usize,
    position: usize,
    markable: bool,
}

/// The message blocks that carry markers 2, 3 and 4, deduplicated, as
/// `(message index, block index)`.
fn message_breakpoints(messages: &[ApiMessage]) -> Vec<(usize, usize)> {
    let blocks = locate(messages);
    let Some(newest) = last_markable(&blocks, |_| true) else {
        return Vec::new();
    };
    let mut targets = vec![newest];

    let previous = messages
        .iter()
        .rposition(|message| message.role == "assistant")
        // A user message, or a system message (an operator notice) that
        // ended the previous request.
        .and_then(|assistant| {
            messages[..assistant]
                .iter()
                .rposition(|m| m.role != "assistant")
        })
        .and_then(|user| last_markable(&blocks, |located| located.message <= user));
    if let Some(previous) = previous {
        targets.push(previous);
        let floor = blocks[previous].position;
        let newest_position = blocks[newest].position;
        if newest_position - floor > INTERMEDIATE_AFTER_POSITIONS {
            let at_or_before = newest_position - INTERMEDIATE_AFTER_POSITIONS;
            if let Some(intermediate) =
                last_markable(&blocks, |located| located.position <= at_or_before)
                    .filter(|index| blocks[*index].position > floor)
            {
                targets.push(intermediate);
            }
        }
    }

    targets.sort_unstable();
    targets.dedup();
    targets
        .into_iter()
        .map(|index| (blocks[index].message, blocks[index].block))
        .collect()
}

/// Every message block in request order. A block continues its
/// predecessor's position when both are `tool_use`, or both `tool_result`.
fn locate(messages: &[ApiMessage]) -> Vec<Located> {
    let mut located = Vec::new();
    let mut position = 0usize;
    let mut previous_kind: Option<RunKind> = None;
    for (message_index, message) in messages.iter().enumerate() {
        for (block_index, block) in message.content.iter().enumerate() {
            let kind = RunKind::of(block);
            let continues_run = kind.is_some() && kind == previous_kind;
            if !continues_run && !located.is_empty() {
                position += 1;
            }
            previous_kind = kind;
            located.push(Located {
                message: message_index,
                block: block_index,
                position,
                markable: block.accepts_cache_control(),
            });
        }
    }
    located
}

fn last_markable(blocks: &[Located], within: impl Fn(&Located) -> bool) -> Option<usize> {
    blocks
        .iter()
        .rposition(|located| located.markable && within(located))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    ToolUse,
    ToolResult,
}

impl RunKind {
    fn of(block: &ApiContentBlock) -> Option<Self> {
        match block {
            ApiContentBlock::ToolUse { .. } => Some(Self::ToolUse),
            ApiContentBlock::ToolResult { .. } => Some(Self::ToolResult),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "cache_breakpoints_tests.rs"]
mod tests;
