//! Lightweight context-budget accounting with no transformation policy.
//!
//! This tracker deliberately cannot summarize, suppress, compact, truncate, or
//! otherwise return a modified message list. It only records provider-relevant
//! estimates, the active model budget, and the latest provider-observed input.

use jcode_context_core::{
    estimate_content_blocks_chars, estimate_content_blocks_tokens, estimate_message_chars,
    estimate_message_tokens, estimate_messages_chars, estimate_messages_tokens,
};
use jcode_message_types::Message;
use jcode_session_types::StoredMessage;

/// Transitional default until provider initialization supplies the active model limit.
pub const DEFAULT_CONTEXT_TOKEN_BUDGET: usize = 200_000;

/// Existing conservative system-prompt and tool-definition allowance.
///
/// Later whole-request preflight replaces this approximation with exact request
/// components. Keeping it here preserves today's large-output guard safety while
/// separating the estimate from all compaction policy.
pub const ESTIMATED_STATIC_CONTEXT_OVERHEAD_TOKENS: usize = 18_000;

const STATIC_OVERHEAD_MINIMUM_BUDGET: usize = DEFAULT_CONTEXT_TOKEN_BUDGET / 2;

fn saturating_sum(values: impl IntoIterator<Item = usize>) -> usize {
    values
        .into_iter()
        .fold(0usize, |total, value| total.saturating_add(value))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextBudgetStats {
    pub token_budget: usize,
    pub message_count: usize,
    pub estimated_message_chars: usize,
    pub estimated_message_tokens: usize,
    pub estimated_static_overhead_tokens: usize,
    pub token_estimate: usize,
    pub observed_input_tokens: Option<u64>,
    pub effective_tokens: usize,
    pub context_usage: f32,
}

/// Policy-free accounting for the provider-facing message view.
#[derive(Clone, Debug)]
pub struct ContextBudgetTracker {
    token_budget: usize,
    observed_input_tokens: Option<u64>,
    estimated_message_tokens_at_observation: Option<usize>,
    estimated_message_chars: usize,
    estimated_message_tokens: usize,
    message_count: usize,
}

impl ContextBudgetTracker {
    pub fn new() -> Self {
        Self {
            token_budget: DEFAULT_CONTEXT_TOKEN_BUDGET,
            observed_input_tokens: None,
            estimated_message_tokens_at_observation: None,
            estimated_message_chars: 0,
            estimated_message_tokens: 0,
            message_count: 0,
        }
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.token_budget = budget;
        self
    }

    /// Change only the model budget. Recorded messages are left untouched.
    pub fn set_budget(&mut self, budget: usize) {
        self.token_budget = budget;
    }

    pub fn token_budget(&self) -> usize {
        self.token_budget
    }

    /// Clear message and provider-observation accounting while preserving the
    /// configured model budget.
    pub fn reset(&mut self) {
        self.observed_input_tokens = None;
        self.estimated_message_tokens_at_observation = None;
        self.estimated_message_chars = 0;
        self.estimated_message_tokens = 0;
        self.message_count = 0;
    }

    pub fn clear_observed_input_tokens(&mut self) {
        self.observed_input_tokens = None;
        self.estimated_message_tokens_at_observation = None;
    }

    pub fn update_observed_input_tokens(&mut self, tokens: u64) {
        self.observed_input_tokens = Some(tokens);
        self.estimated_message_tokens_at_observation = Some(self.estimated_message_tokens);
    }

    pub fn observed_input_tokens(&self) -> Option<u64> {
        self.observed_input_tokens
    }

    /// Replace accounting from a complete provider-facing message view.
    /// Historical replacement invalidates any observation from the old view.
    pub fn seed_messages(&mut self, messages: &[Message]) {
        self.estimated_message_chars = estimate_messages_chars(messages);
        self.estimated_message_tokens = estimate_messages_tokens(messages);
        self.message_count = messages.len();
        self.observed_input_tokens = None;
        self.estimated_message_tokens_at_observation = None;
    }

    /// Replace accounting from authoritative stored messages before projection.
    pub fn seed_stored_messages(&mut self, messages: &[StoredMessage]) {
        self.estimated_message_chars = saturating_sum(
            messages
                .iter()
                .map(|message| estimate_content_blocks_chars(&message.content)),
        );
        self.estimated_message_tokens = saturating_sum(
            messages
                .iter()
                .map(|message| estimate_content_blocks_tokens(&message.content)),
        );
        self.message_count = messages.len();
        self.observed_input_tokens = None;
        self.estimated_message_tokens_at_observation = None;
    }

    pub fn record_message(&mut self, message: &Message) {
        self.estimated_message_chars = self
            .estimated_message_chars
            .saturating_add(estimate_message_chars(message));
        self.estimated_message_tokens = self
            .estimated_message_tokens
            .saturating_add(estimate_message_tokens(message));
        self.message_count = self.message_count.saturating_add(1);
    }

    pub fn record_stored_message(&mut self, message: &StoredMessage) {
        self.estimated_message_chars = self
            .estimated_message_chars
            .saturating_add(estimate_content_blocks_chars(&message.content));
        self.estimated_message_tokens = self
            .estimated_message_tokens
            .saturating_add(estimate_content_blocks_tokens(&message.content));
        self.message_count = self.message_count.saturating_add(1);
    }

    pub fn message_count(&self) -> usize {
        self.message_count
    }

    pub fn estimated_message_chars(&self) -> usize {
        self.estimated_message_chars
    }

    pub fn estimated_message_tokens(&self) -> usize {
        self.estimated_message_tokens
    }

    pub fn estimated_static_overhead_tokens(&self) -> usize {
        if self.token_budget >= STATIC_OVERHEAD_MINIMUM_BUDGET {
            ESTIMATED_STATIC_CONTEXT_OVERHEAD_TOKENS
        } else {
            0
        }
    }

    pub fn token_estimate(&self) -> usize {
        self.estimated_message_tokens
            .saturating_add(self.estimated_static_overhead_tokens())
    }

    pub fn effective_token_count(&self) -> usize {
        let observed = self
            .observed_input_tokens
            .map(|tokens| usize::try_from(tokens).unwrap_or(usize::MAX))
            .map(|tokens| {
                let appended_message_tokens = self
                    .estimated_message_tokens_at_observation
                    .map(|at_observation| {
                        self.estimated_message_tokens.saturating_sub(at_observation)
                    })
                    .unwrap_or_default();
                tokens.saturating_add(appended_message_tokens)
            })
            .unwrap_or_default();
        self.token_estimate().max(observed)
    }

    pub fn context_usage(&self) -> f32 {
        if self.token_budget == 0 {
            0.0
        } else {
            self.effective_token_count() as f32 / self.token_budget as f32
        }
    }

    pub fn stats(&self) -> ContextBudgetStats {
        ContextBudgetStats {
            token_budget: self.token_budget,
            message_count: self.message_count,
            estimated_message_chars: self.estimated_message_chars,
            estimated_message_tokens: self.estimated_message_tokens,
            estimated_static_overhead_tokens: self.estimated_static_overhead_tokens(),
            token_estimate: self.token_estimate(),
            observed_input_tokens: self.observed_input_tokens,
            effective_tokens: self.effective_token_count(),
            context_usage: self.context_usage(),
        }
    }
}

impl Default for ContextBudgetTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::{ContentBlock, Role};

    fn text_message(text: &str) -> Message {
        Message::user(text)
    }

    fn stored_text_message(id: &str, text: &str) -> StoredMessage {
        StoredMessage {
            origin: None,
            id: id.to_string(),
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    #[test]
    fn defaults_and_explicit_budget_are_stable() {
        let default = ContextBudgetTracker::new();
        assert_eq!(default.token_budget(), DEFAULT_CONTEXT_TOKEN_BUDGET);
        assert_eq!(default.message_count(), 0);
        assert_eq!(default.estimated_message_tokens(), 0);
        assert_eq!(
            default.token_estimate(),
            ESTIMATED_STATIC_CONTEXT_OVERHEAD_TOKENS
        );
        assert_eq!(default.effective_token_count(), default.token_estimate());

        let explicit = ContextBudgetTracker::new().with_budget(12_345);
        assert_eq!(explicit.token_budget(), 12_345);
        assert_eq!(explicit.estimated_static_overhead_tokens(), 0);
        assert_eq!(explicit.context_usage(), 0.0);
    }

    #[test]
    fn append_and_full_reseed_are_equivalent() {
        let messages = vec![text_message("first"), text_message("second message")];
        let mut appended = ContextBudgetTracker::new();
        for message in &messages {
            appended.record_message(message);
        }

        let mut seeded = ContextBudgetTracker::new();
        seeded.seed_messages(&messages);
        assert_eq!(appended.stats(), seeded.stats());
    }

    #[test]
    fn stored_append_and_full_reseed_are_equivalent() {
        let messages = vec![
            stored_text_message("message-1", "first"),
            stored_text_message("message-2", "second message"),
        ];
        let mut appended = ContextBudgetTracker::new();
        for message in &messages {
            appended.record_stored_message(message);
        }

        let mut seeded = ContextBudgetTracker::new();
        seeded.seed_stored_messages(&messages);
        assert_eq!(appended.stats(), seeded.stats());
    }

    #[test]
    fn full_reseed_replaces_prior_values_and_clears_stale_observation() {
        let mut tracker = ContextBudgetTracker::new();
        tracker.record_message(&text_message(&"old".repeat(1_000)));
        tracker.update_observed_input_tokens(99_000);
        tracker.seed_messages(&[text_message("replacement")]);

        assert_eq!(tracker.message_count(), 1);
        assert_eq!(tracker.observed_input_tokens(), None);
        assert!(tracker.effective_token_count() < 99_000);
    }

    #[test]
    fn effective_tokens_use_the_larger_of_estimate_and_observation() {
        let mut tracker = ContextBudgetTracker::new().with_budget(10_000);
        tracker.seed_messages(&[text_message(&"x".repeat(4_000))]);
        let estimate = tracker.token_estimate();

        tracker.update_observed_input_tokens((estimate.saturating_sub(1)) as u64);
        assert_eq!(tracker.effective_token_count(), estimate);

        tracker.update_observed_input_tokens((estimate + 500) as u64);
        assert_eq!(tracker.effective_token_count(), estimate + 500);

        tracker.clear_observed_input_tokens();
        assert_eq!(tracker.effective_token_count(), estimate);
    }

    #[test]
    fn provider_observation_is_advanced_by_messages_appended_after_that_request() {
        let mut tracker = ContextBudgetTracker::new().with_budget(372_000);
        tracker.seed_messages(&[text_message(&"historical ".repeat(40_000))]);
        let observed = tracker.token_estimate().saturating_add(20_000);
        tracker.update_observed_input_tokens(observed as u64);

        let appended = text_message(&"assistant continuation ".repeat(1_000));
        let appended_tokens = estimate_message_tokens(&appended);
        tracker.record_message(&appended);

        assert_eq!(
            tracker.effective_token_count(),
            observed.saturating_add(appended_tokens)
        );
        assert_eq!(tracker.observed_input_tokens(), Some(observed as u64));

        tracker.seed_messages(&[text_message("replacement")]);
        assert_eq!(tracker.observed_input_tokens(), None);
        assert_eq!(tracker.effective_token_count(), tracker.token_estimate());
    }

    #[test]
    fn budget_changes_only_recompute_budget_dependent_overhead() {
        let messages = vec![text_message(&"x".repeat(4_000))];
        let mut tracker = ContextBudgetTracker::new().with_budget(50_000);
        tracker.seed_messages(&messages);
        let chars = tracker.estimated_message_chars();
        let message_tokens = tracker.estimated_message_tokens();
        let messages_before = serde_json::to_vec(&messages).unwrap();
        assert_eq!(tracker.estimated_static_overhead_tokens(), 0);

        tracker.set_budget(200_000);
        assert_eq!(tracker.estimated_message_chars(), chars);
        assert_eq!(tracker.estimated_message_tokens(), message_tokens);
        assert_eq!(
            tracker.estimated_static_overhead_tokens(),
            ESTIMATED_STATIC_CONTEXT_OVERHEAD_TOKENS
        );
        assert_eq!(serde_json::to_vec(&messages).unwrap(), messages_before);
    }

    #[test]
    fn zero_budget_has_safe_finite_usage() {
        let mut tracker = ContextBudgetTracker::new().with_budget(0);
        tracker.record_message(&text_message("content"));
        assert_eq!(tracker.context_usage(), 0.0);
        assert!(tracker.context_usage().is_finite());
    }

    #[test]
    fn trace_only_content_costs_zero_and_is_never_transformed() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ReasoningTrace {
                text: "history only".repeat(10_000),
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        let before = serde_json::to_vec(&message).unwrap();
        let mut tracker = ContextBudgetTracker::new().with_budget(1_000);
        tracker.record_message(&message);

        assert_eq!(tracker.estimated_message_chars(), 0);
        assert_eq!(tracker.estimated_message_tokens(), 0);
        assert_eq!(serde_json::to_vec(&message).unwrap(), before);
    }

    #[test]
    fn image_cost_is_bounded_independently_of_base64_size() {
        let image_message = |size| Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "x".repeat(size),
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        let mut small = ContextBudgetTracker::new().with_budget(10_000);
        small.record_message(&image_message(10));
        let mut large = ContextBudgetTracker::new().with_budget(10_000);
        large.record_message(&image_message(1_000_000));

        assert_eq!(
            small.estimated_message_chars(),
            large.estimated_message_chars()
        );
        assert_eq!(
            small.estimated_message_tokens(),
            large.estimated_message_tokens()
        );
    }

    #[test]
    fn arithmetic_saturates_instead_of_overflowing() {
        let mut tracker = ContextBudgetTracker::new();
        tracker.estimated_message_chars = usize::MAX;
        tracker.estimated_message_tokens = usize::MAX;
        tracker.message_count = usize::MAX;
        tracker.record_message(&text_message("more"));

        assert_eq!(tracker.estimated_message_chars(), usize::MAX);
        assert_eq!(tracker.estimated_message_tokens(), usize::MAX);
        assert_eq!(tracker.message_count(), usize::MAX);
        assert_eq!(tracker.token_estimate(), usize::MAX);
    }

    #[test]
    fn stored_reseed_uses_saturating_aggregation() {
        assert_eq!(saturating_sum([usize::MAX, 1]), usize::MAX);
    }

    #[test]
    fn reset_preserves_budget_and_clears_all_accounting() {
        let mut tracker = ContextBudgetTracker::new().with_budget(372_000);
        tracker.record_message(&text_message("message"));
        tracker.update_observed_input_tokens(123);
        tracker.reset();

        assert_eq!(tracker.token_budget(), 372_000);
        assert_eq!(tracker.message_count(), 0);
        assert_eq!(tracker.estimated_message_chars(), 0);
        assert_eq!(tracker.estimated_message_tokens(), 0);
        assert_eq!(tracker.observed_input_tokens(), None);
    }

    #[test]
    fn seed_and_record_leave_input_messages_byte_for_byte_unchanged() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "answer".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "src/lib.rs"}),
                    thought_signature: Some("signature".to_string()),
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        };
        let messages = vec![message.clone()];
        let before = serde_json::to_vec(&messages).unwrap();

        let mut tracker = ContextBudgetTracker::new();
        tracker.seed_messages(&messages);
        tracker.record_message(&message);
        tracker.set_budget(1_000_000);
        tracker.update_observed_input_tokens(42);
        tracker.clear_observed_input_tokens();

        assert_eq!(serde_json::to_vec(&messages).unwrap(), before);
    }
}
