use super::*;

impl Agent {
    pub(super) fn is_context_limit_error(error: &str) -> bool {
        let lower = error.to_ascii_lowercase();
        lower.contains("context length")
            || lower.contains("context window")
            || lower.contains("maximum context")
            || lower.contains("max context")
            || lower.contains("token limit")
            || lower.contains("too many tokens")
            || lower.contains("prompt is too long")
            || lower.contains("input is too long")
            || lower.contains("request too large")
            || lower.contains("length limit")
            || lower.contains("maximum tokens")
            || (lower.contains("exceeded") && lower.contains("tokens"))
    }

    pub(super) fn update_context_usage_from_stream(
        &mut self,
        input_tokens: u64,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    ) {
        if input_tokens == 0 {
            return;
        }
        let observed = jcode_provider_core::effective_context_tokens_from_usage(
            self.provider.name(),
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
        let context_budget = self.registry.context_budget();
        if let Ok(mut tracker) = context_budget.try_write() {
            tracker.update_observed_input_tokens(observed);
        } else {
            logging::warn("Context budget lock unavailable during provider usage update");
        }
    }
}
