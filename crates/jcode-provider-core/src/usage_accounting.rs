//! Provider-reported prompt-usage normalization.
//!
//! Providers disagree on whether cached input is included in `input_tokens` or
//! reported as a disjoint counter. This module owns the policy-free accounting
//! heuristic used by request pressure, status, and context-budget tracking.

/// Return the effective provider-context size represented by one usage report.
///
/// Anthropic-style split accounting reports uncached input separately from
/// cache reads and cache creation. OpenAI-style subset accounting includes
/// cached tokens in `input_tokens`, so adding the cache counter would double
/// count. Unknown providers are treated conservatively as subset accounting
/// unless the counter shape is strong evidence of a split report.
pub fn effective_context_tokens_from_usage(
    provider_name: &str,
    input_tokens: u64,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
) -> u64 {
    if input_tokens == 0 {
        return 0;
    }
    let cache_read = cache_read_input_tokens.unwrap_or(0);
    let cache_creation = cache_creation_input_tokens.unwrap_or(0);
    let provider_name = provider_name.to_ascii_lowercase();

    let split_cache_accounting = provider_name.contains("anthropic")
        || provider_name.contains("claude")
        || cache_creation > 0
        || cache_read > input_tokens;

    if split_cache_accounting {
        input_tokens
            .saturating_add(cache_read)
            .saturating_add(cache_creation)
    } else {
        input_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_accounting_adds_cache_counters() {
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", 10_000, Some(300_000), Some(5_000),),
            315_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("Claude", 10_000, Some(300_000), None),
            310_000
        );
    }

    #[test]
    fn subset_accounting_does_not_double_count() {
        assert_eq!(
            effective_context_tokens_from_usage("openai", 400_000, Some(390_000), None),
            400_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("opencode-go", 396_000, None, None),
            396_000
        );
    }

    #[test]
    fn counter_shape_can_prove_split_accounting() {
        assert_eq!(
            effective_context_tokens_from_usage("unknown", 10_000, Some(500_000), None),
            510_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("unknown", 10_000, Some(2_000), Some(1_000)),
            13_000
        );
    }

    #[test]
    fn zero_input_reports_zero() {
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", 0, Some(300_000), Some(5_000)),
            0
        );
    }

    #[test]
    fn split_accounting_saturates() {
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", u64::MAX, Some(1), Some(1)),
            u64::MAX
        );
    }
}
