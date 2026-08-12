use crate::{estimate_message_tokens, estimate_messages_tokens};
use jcode_message_types::{Message, cache_relevant_message_hashes};
use jcode_session_types::{
    StoredContextBillingMode, StoredContextCacheWarmth, StoredContextEconomics,
    StoredContextInputPriceTier, StoredContextPricingSnapshot,
};

const TOKENS_PER_MILLION: f64 = 1_000_000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachePrefixAnalysis {
    pub unchanged_prefix_items: usize,
    pub earliest_changed_provider_item: Option<usize>,
    pub old_total_tokens: usize,
    pub new_total_tokens: usize,
    pub old_affected_suffix_tokens: usize,
    pub new_affected_suffix_tokens: usize,
    pub deleted_input_tokens: usize,
}

pub fn analyze_cache_prefix(old: &[Message], new: &[Message]) -> CachePrefixAnalysis {
    let old_hashes = cache_relevant_message_hashes(old);
    let new_hashes = cache_relevant_message_hashes(new);
    let unchanged_prefix_items = old_hashes
        .iter()
        .zip(&new_hashes)
        .take_while(|(old, new)| old == new)
        .count();
    let identical = unchanged_prefix_items == old.len() && unchanged_prefix_items == new.len();
    let old_total_tokens = estimate_messages_tokens(old);
    let new_total_tokens = estimate_messages_tokens(new);
    let old_affected_suffix_tokens = old[unchanged_prefix_items..]
        .iter()
        .map(estimate_message_tokens)
        .sum();
    let new_affected_suffix_tokens = new[unchanged_prefix_items..]
        .iter()
        .map(estimate_message_tokens)
        .sum();
    CachePrefixAnalysis {
        unchanged_prefix_items,
        earliest_changed_provider_item: (!identical).then_some(unchanged_prefix_items),
        old_total_tokens,
        new_total_tokens,
        old_affected_suffix_tokens,
        new_affected_suffix_tokens,
        deleted_input_tokens: old_total_tokens.saturating_sub(new_total_tokens),
    }
}

#[derive(Clone, Debug)]
pub struct ContextEconomicsInput<'a> {
    pub analysis: &'a CachePrefixAnalysis,
    /// Whole-request estimates include provider messages plus system, tool, and
    /// other request overhead. They are mandatory for tiered dollar estimates.
    pub estimated_total_request_tokens_before: Option<usize>,
    pub estimated_total_request_tokens_after: Option<usize>,
    pub context_window: Option<usize>,
    pub safe_input_budget: Option<usize>,
    pub pricing: Option<&'a StoredContextPricingSnapshot>,
    pub resulting_suffix_cacheable: bool,
}

pub fn calculate_context_economics(input: ContextEconomicsInput<'_>) -> StoredContextEconomics {
    let analysis = input.analysis;
    let mut assumptions = Vec::new();
    let mut first_request_delta_usd = None;
    let mut recurring_savings_per_turn_usd = None;
    let mut break_even_turns = None;

    if let Some(pricing) = input.pricing {
        if pricing.billing_mode == StoredContextBillingMode::Metered {
            let pricing_request_tokens =
                pricing_request_token_totals(pricing, &input, analysis, &mut assumptions);
            if let Some((old_pricing_tokens, new_pricing_tokens)) = pricing_request_tokens {
                match pricing.cache_warmth {
                    StoredContextCacheWarmth::Warm => {
                        let old_rates =
                            context_token_rates_for_input_tokens(pricing, old_pricing_tokens);
                        let new_rates =
                            context_token_rates_for_input_tokens(pricing, new_pricing_tokens);
                        if let (Some(old_read), Some(new_read), Some(new_rebuild)) = (
                            old_rates.cache_read_usd_per_million,
                            new_rates.cache_read_usd_per_million,
                            new_rates
                                .cache_write_usd_per_million
                                .or(new_rates.input_usd_per_million),
                        ) {
                            if let Some(stable_prefix_tokens) = stable_request_prefix_tokens(
                                old_pricing_tokens,
                                new_pricing_tokens,
                                analysis,
                                &mut assumptions,
                            ) {
                                let old_cost = token_cost(stable_prefix_tokens, old_read)
                                    + token_cost(analysis.old_affected_suffix_tokens, old_read);
                                let new_cost = token_cost(stable_prefix_tokens, new_read)
                                    + token_cost(analysis.new_affected_suffix_tokens, new_rebuild);
                                first_request_delta_usd = Some(new_cost - old_cost);
                            }
                        } else {
                            assumptions.push(
                                "cache-read or rebuild price is unavailable; first-request dollar economics omitted"
                                    .to_string(),
                            );
                        }
                        recurring_savings_per_turn_usd = recurring_cache_savings(
                            old_pricing_tokens,
                            new_pricing_tokens,
                            old_rates,
                            new_rates,
                            input.resulting_suffix_cacheable,
                            &mut assumptions,
                        );
                        record_output_tier_assumption(old_rates, new_rates, &mut assumptions);
                    }
                    StoredContextCacheWarmth::Cold => {
                        let old_rates =
                            context_token_rates_for_input_tokens(pricing, old_pricing_tokens);
                        let new_rates =
                            context_token_rates_for_input_tokens(pricing, new_pricing_tokens);
                        if let (Some(old_rebuild), Some(new_rebuild)) = (
                            old_rates
                                .cache_write_usd_per_million
                                .or(old_rates.input_usd_per_million),
                            new_rates
                                .cache_write_usd_per_million
                                .or(new_rates.input_usd_per_million),
                        ) {
                            let delta = token_cost(new_pricing_tokens, new_rebuild)
                                - token_cost(old_pricing_tokens, old_rebuild);
                            first_request_delta_usd = Some(delta);
                            assumptions.push(
                                "the prior prefix is cold; the first-request comparison bills complete old and new prompts at rebuild rates"
                                    .to_string(),
                            );
                        } else {
                            assumptions.push(
                                "the prior prefix is cold and rebuild pricing is unavailable; first-request dollar economics omitted"
                                    .to_string(),
                            );
                        }
                        recurring_savings_per_turn_usd = recurring_cache_savings(
                            old_pricing_tokens,
                            new_pricing_tokens,
                            old_rates,
                            new_rates,
                            input.resulting_suffix_cacheable,
                            &mut assumptions,
                        );
                        record_output_tier_assumption(old_rates, new_rates, &mut assumptions);
                    }
                    StoredContextCacheWarmth::Unknown => assumptions.push(
                        "cache warmth is unknown; dollar cache-transition estimates were omitted"
                            .to_string(),
                    ),
                }
            }
        } else {
            assumptions.push(format!(
                "billing mode is {:?}; token effects are reported without dollar claims",
                pricing.billing_mode
            ));
        }
    } else {
        assumptions.push("route pricing is unavailable; dollar economics omitted".to_string());
    }
    if !input.resulting_suffix_cacheable {
        assumptions.push(
            "the resulting suffix is not cacheable; recurring cache-read savings were omitted"
                .to_string(),
        );
    }
    if let (Some(delta), Some(recurring)) =
        (first_request_delta_usd, recurring_savings_per_turn_usd)
        && recurring > 0.0
    {
        break_even_turns = Some(if delta <= 0.0 {
            0
        } else {
            (delta / recurring).ceil() as u64
        });
    }

    StoredContextEconomics {
        projected_tokens_before: analysis.old_total_tokens,
        projected_tokens_after: analysis.new_total_tokens,
        estimated_total_request_tokens_before: input.estimated_total_request_tokens_before,
        estimated_total_request_tokens_after: input.estimated_total_request_tokens_after,
        unchanged_prefix_items: analysis.unchanged_prefix_items,
        earliest_changed_provider_item: analysis.earliest_changed_provider_item,
        old_affected_suffix_tokens: analysis.old_affected_suffix_tokens,
        new_affected_suffix_tokens: analysis.new_affected_suffix_tokens,
        deleted_input_tokens: analysis.deleted_input_tokens,
        context_window: input.context_window,
        safe_input_budget: input.safe_input_budget,
        pricing: input.pricing.cloned(),
        first_request_delta_usd,
        recurring_savings_per_turn_usd,
        break_even_turns,
        assumptions,
    }
}

fn stable_request_prefix_tokens(
    old_request_tokens: usize,
    new_request_tokens: usize,
    analysis: &CachePrefixAnalysis,
    assumptions: &mut Vec<String>,
) -> Option<usize> {
    let old_prefix = old_request_tokens.checked_sub(analysis.old_affected_suffix_tokens);
    let new_prefix = new_request_tokens.checked_sub(analysis.new_affected_suffix_tokens);
    match (old_prefix, new_prefix) {
        (Some(old_prefix), Some(new_prefix)) if old_prefix == new_prefix => Some(old_prefix),
        _ => {
            assumptions.push(
                "whole-request estimates do not preserve the unchanged request prefix; first-request dollar economics omitted"
                    .to_string(),
            );
            None
        }
    }
}

fn recurring_cache_savings(
    old_request_tokens: usize,
    new_request_tokens: usize,
    old_rates: ContextTokenRates,
    new_rates: ContextTokenRates,
    resulting_suffix_cacheable: bool,
    assumptions: &mut Vec<String>,
) -> Option<f64> {
    if !resulting_suffix_cacheable {
        return None;
    }
    match (
        old_rates.cache_read_usd_per_million,
        new_rates.cache_read_usd_per_million,
    ) {
        (Some(old_read), Some(new_read)) => Some(
            token_cost(old_request_tokens, old_read) - token_cost(new_request_tokens, new_read),
        ),
        _ => {
            assumptions.push(
                "old or new cache-read price is unavailable; recurring dollar economics omitted"
                    .to_string(),
            );
            None
        }
    }
}

fn record_output_tier_assumption(
    old_rates: ContextTokenRates,
    new_rates: ContextTokenRates,
    assumptions: &mut Vec<String>,
) {
    if old_rates.output_usd_per_million != new_rates.output_usd_per_million {
        assumptions.push(
            "the request-size tier changes output pricing; output-cost effects are omitted because future output tokens are unknown"
                .to_string(),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextTokenRates {
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    pub cache_read_usd_per_million: Option<f64>,
    pub cache_write_usd_per_million: Option<f64>,
}

/// Resolve the effective rates for a whole-request input-token count.
/// Thresholds are strict, output rates inherit when omitted, and missing cache
/// rates remain unknown so callers cannot fabricate cache economics.
pub fn context_token_rates_for_input_tokens(
    pricing: &StoredContextPricingSnapshot,
    input_tokens: usize,
) -> ContextTokenRates {
    let mut rates = ContextTokenRates {
        input_usd_per_million: pricing.input_usd_per_million,
        output_usd_per_million: pricing.output_usd_per_million,
        cache_read_usd_per_million: pricing.cache_read_usd_per_million,
        cache_write_usd_per_million: pricing.cache_write_usd_per_million,
    };
    for tier in sorted_tiers(&pricing.input_price_tiers) {
        if input_tokens > tier.above_input_tokens {
            rates.input_usd_per_million = Some(tier.input_usd_per_million);
            if let Some(output) = tier.output_usd_per_million {
                rates.output_usd_per_million = Some(output);
            }
            rates.cache_read_usd_per_million = tier.cache_read_usd_per_million;
            rates.cache_write_usd_per_million = tier.cache_write_usd_per_million;
        }
    }
    rates
}

fn pricing_request_token_totals(
    pricing: &StoredContextPricingSnapshot,
    input: &ContextEconomicsInput<'_>,
    analysis: &CachePrefixAnalysis,
    assumptions: &mut Vec<String>,
) -> Option<(usize, usize)> {
    if pricing.input_price_tiers.is_empty() {
        return Some((analysis.old_total_tokens, analysis.new_total_tokens));
    }

    match (
        input.estimated_total_request_tokens_before,
        input.estimated_total_request_tokens_after,
    ) {
        (Some(before), Some(after))
            if before >= analysis.old_total_tokens && after >= analysis.new_total_tokens =>
        {
            Some((before, after))
        }
        _ => {
            assumptions.push(
                "request-size pricing tiers are known, but valid whole-request token estimates are unavailable; dollar economics omitted"
                    .to_string(),
            );
            None
        }
    }
}

fn sorted_tiers(tiers: &[StoredContextInputPriceTier]) -> Vec<&StoredContextInputPriceTier> {
    let mut tiers = tiers.iter().collect::<Vec<_>>();
    tiers.sort_by_key(|tier| tier.above_input_tokens);
    tiers
}

fn token_cost(tokens: usize, usd_per_million: f64) -> f64 {
    tokens as f64 * usd_per_million / TOKENS_PER_MILLION
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::{ContentBlock, Role};

    fn text_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn pricing(billing_mode: StoredContextBillingMode) -> StoredContextPricingSnapshot {
        StoredContextPricingSnapshot {
            billing_mode,
            input_usd_per_million: Some(5.0),
            output_usd_per_million: Some(30.0),
            cache_read_usd_per_million: Some(0.5),
            cache_write_usd_per_million: Some(6.25),
            input_price_tiers: Vec::new(),
            cache_warmth: StoredContextCacheWarmth::Warm,
        }
    }

    #[test]
    fn middle_edit_invalidates_only_the_changed_suffix() {
        let old = vec![
            text_message("stable prefix"),
            text_message(&"large old middle ".repeat(1_000)),
            text_message("tail"),
        ];
        let new = vec![old[0].clone(), text_message("summary"), old[2].clone()];
        let analysis = analyze_cache_prefix(&old, &new);
        assert_eq!(analysis.unchanged_prefix_items, 1);
        assert_eq!(analysis.earliest_changed_provider_item, Some(1));
        assert!(analysis.old_affected_suffix_tokens < analysis.old_total_tokens);
        assert!(analysis.deleted_input_tokens > 0);
    }

    #[test]
    fn subscription_pricing_never_claims_dollar_precision() {
        let analysis = analyze_cache_prefix(
            &[text_message(&"old ".repeat(1_000))],
            &[text_message("summary")],
        );
        let pricing = pricing(StoredContextBillingMode::Subscription);
        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: Some(372_000),
            safe_input_budget: Some(368_000),
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });
        assert_eq!(economics.first_request_delta_usd, None);
        assert_eq!(economics.recurring_savings_per_turn_usd, None);
        assert_eq!(economics.break_even_turns, None);
        assert!(economics.assumptions[0].contains("without dollar claims"));
    }

    #[test]
    fn warm_metered_cache_uses_affected_suffix_and_reports_break_even() {
        let analysis = analyze_cache_prefix(
            &[
                text_message("stable"),
                text_message(&"old suffix ".repeat(10_000)),
            ],
            &[text_message("stable"), text_message("summary")],
        );
        let pricing = pricing(StoredContextBillingMode::Metered);
        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: Some(372_000),
            safe_input_budget: Some(368_000),
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });
        assert!(economics.first_request_delta_usd.is_some());
        assert!(economics.recurring_savings_per_turn_usd.unwrap_or_default() > 0.0);
        assert!(economics.break_even_turns.is_some());
        assert_eq!(economics.unchanged_prefix_items, 1);
    }

    #[test]
    fn identical_context_has_full_prefix_and_no_earliest_change() {
        let messages = vec![text_message("one"), text_message("two")];

        let analysis = analyze_cache_prefix(&messages, &messages);

        assert_eq!(analysis.unchanged_prefix_items, messages.len());
        assert_eq!(analysis.earliest_changed_provider_item, None);
        assert_eq!(analysis.old_affected_suffix_tokens, 0);
        assert_eq!(analysis.new_affected_suffix_tokens, 0);
        assert_eq!(analysis.deleted_input_tokens, 0);
    }

    #[test]
    fn later_edits_never_move_earliest_change_before_the_first_real_edit() {
        let old = vec![
            text_message("stable"),
            text_message("first old"),
            text_message("later old"),
            text_message("tail"),
        ];
        let new = vec![
            old[0].clone(),
            text_message("first new"),
            text_message("later new"),
            old[3].clone(),
        ];

        let analysis = analyze_cache_prefix(&old, &new);

        assert_eq!(analysis.unchanged_prefix_items, 1);
        assert_eq!(analysis.earliest_changed_provider_item, Some(1));
    }

    #[test]
    fn immediate_savings_and_zero_recurring_savings_are_reported_without_division_errors() {
        let pricing = pricing(StoredContextBillingMode::Metered);
        let immediate = analyze_cache_prefix(
            &[text_message(&"old suffix ".repeat(100_000))],
            &[text_message("tiny")],
        );
        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &immediate,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });
        assert!(economics.first_request_delta_usd.unwrap_or_default() < 0.0);
        assert_eq!(economics.break_even_turns, Some(0));

        let unchanged = analyze_cache_prefix(&[text_message("same")], &[text_message("same")]);
        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &unchanged,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });
        assert_eq!(economics.recurring_savings_per_turn_usd, Some(0.0));
        assert_eq!(economics.break_even_turns, None);
    }

    #[test]
    fn absent_cache_write_price_falls_back_to_uncached_input_rate() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 1,
            earliest_changed_provider_item: Some(1),
            old_total_tokens: 20_000,
            new_total_tokens: 12_000,
            old_affected_suffix_tokens: 10_000,
            new_affected_suffix_tokens: 2_000,
            deleted_input_tokens: 8_000,
        };
        let mut pricing = pricing(StoredContextBillingMode::Metered);
        pricing.cache_write_usd_per_million = None;

        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });

        let expected = token_cost(2_000, 5.0) - token_cost(10_000, 0.5);
        assert!((economics.first_request_delta_usd.unwrap() - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn tiered_pricing_uses_old_and_new_total_request_bands() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 0,
            earliest_changed_provider_item: Some(0),
            old_total_tokens: 250_000,
            new_total_tokens: 200_000,
            old_affected_suffix_tokens: 250_000,
            new_affected_suffix_tokens: 200_000,
            deleted_input_tokens: 50_000,
        };
        let mut pricing = pricing(StoredContextBillingMode::Metered);
        pricing.input_price_tiers = vec![StoredContextInputPriceTier {
            above_input_tokens: 272_000,
            input_usd_per_million: 10.0,
            output_usd_per_million: Some(45.0),
            cache_read_usd_per_million: Some(1.0),
            cache_write_usd_per_million: Some(12.5),
        }];

        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: Some(300_000),
            estimated_total_request_tokens_after: Some(250_000),
            context_window: Some(372_000),
            safe_input_budget: Some(368_000),
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });

        let expected =
            token_cost(50_000, 0.5) + token_cost(200_000, 6.25) - token_cost(300_000, 1.0);
        assert!((economics.first_request_delta_usd.unwrap() - expected).abs() < f64::EPSILON);
        assert_eq!(economics.recurring_savings_per_turn_usd, Some(0.175));
        assert_eq!(economics.break_even_turns, Some(6));
        assert!(
            economics
                .assumptions
                .iter()
                .any(|assumption| assumption.contains("output pricing"))
        );
        assert_eq!(
            economics.estimated_total_request_tokens_before,
            Some(300_000)
        );
        assert_eq!(
            economics.estimated_total_request_tokens_after,
            Some(250_000)
        );
    }

    #[test]
    fn tier_crossing_reprices_the_unchanged_cached_prefix() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 1,
            earliest_changed_provider_item: Some(1),
            old_total_tokens: 250_000,
            new_total_tokens: 200_000,
            old_affected_suffix_tokens: 100_000,
            new_affected_suffix_tokens: 50_000,
            deleted_input_tokens: 50_000,
        };
        let mut pricing = pricing(StoredContextBillingMode::Metered);
        pricing.input_price_tiers = vec![StoredContextInputPriceTier {
            above_input_tokens: 272_000,
            input_usd_per_million: 10.0,
            output_usd_per_million: Some(45.0),
            cache_read_usd_per_million: Some(1.0),
            cache_write_usd_per_million: Some(12.5),
        }];

        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: Some(300_000),
            estimated_total_request_tokens_after: Some(250_000),
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });

        let expected_first =
            token_cost(200_000, 0.5) + token_cost(50_000, 6.25) - token_cost(300_000, 1.0);
        assert!((economics.first_request_delta_usd.unwrap() - expected_first).abs() < f64::EPSILON);
        assert_eq!(economics.recurring_savings_per_turn_usd, Some(0.175));
        assert_eq!(economics.break_even_turns, Some(1));
    }

    #[test]
    fn inconsistent_whole_request_prefix_evidence_omits_first_request_dollars() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 1,
            earliest_changed_provider_item: Some(1),
            old_total_tokens: 250_000,
            new_total_tokens: 200_000,
            old_affected_suffix_tokens: 100_000,
            new_affected_suffix_tokens: 50_000,
            deleted_input_tokens: 50_000,
        };
        let mut pricing = pricing(StoredContextBillingMode::Metered);
        pricing.input_price_tiers = vec![StoredContextInputPriceTier {
            above_input_tokens: 272_000,
            input_usd_per_million: 10.0,
            output_usd_per_million: Some(45.0),
            cache_read_usd_per_million: Some(1.0),
            cache_write_usd_per_million: Some(12.5),
        }];

        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: Some(300_000),
            estimated_total_request_tokens_after: Some(260_000),
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });

        assert_eq!(economics.first_request_delta_usd, None);
        assert!(economics.recurring_savings_per_turn_usd.is_some());
        assert_eq!(economics.break_even_turns, None);
        assert!(
            economics
                .assumptions
                .iter()
                .any(|assumption| assumption.contains("unchanged request prefix"))
        );
    }

    #[test]
    fn tiered_pricing_without_whole_request_evidence_omits_dollar_claims() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 0,
            earliest_changed_provider_item: Some(0),
            old_total_tokens: 250_000,
            new_total_tokens: 200_000,
            old_affected_suffix_tokens: 250_000,
            new_affected_suffix_tokens: 200_000,
            deleted_input_tokens: 50_000,
        };
        let mut pricing = pricing(StoredContextBillingMode::Metered);
        pricing.input_price_tiers = vec![StoredContextInputPriceTier {
            above_input_tokens: 272_000,
            input_usd_per_million: 10.0,
            output_usd_per_million: Some(45.0),
            cache_read_usd_per_million: Some(1.0),
            cache_write_usd_per_million: Some(12.5),
        }];

        let economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&pricing),
            resulting_suffix_cacheable: true,
        });

        assert_eq!(economics.first_request_delta_usd, None);
        assert_eq!(economics.recurring_savings_per_turn_usd, None);
        assert_eq!(economics.break_even_turns, None);
        assert!(
            economics
                .assumptions
                .iter()
                .any(|assumption| assumption.contains("whole-request token estimates"))
        );
    }

    #[test]
    fn cold_unknown_and_non_metered_routes_state_assumptions_without_fake_dollars() {
        let analysis = CachePrefixAnalysis {
            unchanged_prefix_items: 0,
            earliest_changed_provider_item: Some(0),
            old_total_tokens: 100_000,
            new_total_tokens: 10_000,
            old_affected_suffix_tokens: 100_000,
            new_affected_suffix_tokens: 10_000,
            deleted_input_tokens: 90_000,
        };

        let mut cold = pricing(StoredContextBillingMode::Metered);
        cold.cache_warmth = StoredContextCacheWarmth::Cold;
        let cold_economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&cold),
            resulting_suffix_cacheable: true,
        });
        assert!(cold_economics.first_request_delta_usd.unwrap_or_default() < 0.0);
        assert!(
            cold_economics
                .assumptions
                .iter()
                .any(|value| value.contains("cold"))
        );

        let mut unknown = cold.clone();
        unknown.cache_warmth = StoredContextCacheWarmth::Unknown;
        let unknown_economics = calculate_context_economics(ContextEconomicsInput {
            analysis: &analysis,
            estimated_total_request_tokens_before: None,
            estimated_total_request_tokens_after: None,
            context_window: None,
            safe_input_budget: None,
            pricing: Some(&unknown),
            resulting_suffix_cacheable: true,
        });
        assert_eq!(unknown_economics.first_request_delta_usd, None);
        assert!(
            unknown_economics
                .assumptions
                .iter()
                .any(|value| value.contains("unknown"))
        );

        for billing_mode in [
            StoredContextBillingMode::Subscription,
            StoredContextBillingMode::IncludedQuota,
            StoredContextBillingMode::Unknown,
        ] {
            let route = pricing(billing_mode);
            let economics = calculate_context_economics(ContextEconomicsInput {
                analysis: &analysis,
                estimated_total_request_tokens_before: None,
                estimated_total_request_tokens_after: None,
                context_window: None,
                safe_input_budget: None,
                pricing: Some(&route),
                resulting_suffix_cacheable: false,
            });
            assert_eq!(economics.first_request_delta_usd, None);
            assert_eq!(economics.recurring_savings_per_turn_usd, None);
            assert_eq!(economics.break_even_turns, None);
            assert!(
                economics
                    .assumptions
                    .iter()
                    .any(|value| value.contains("without dollar"))
            );
            assert!(
                economics
                    .assumptions
                    .iter()
                    .any(|value| value.contains("not cacheable"))
            );
        }
    }
}
