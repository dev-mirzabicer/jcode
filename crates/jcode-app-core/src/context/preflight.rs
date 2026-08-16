use crate::message::{Message, ToolDefinition};
use crate::prompt::SplitSystemPrompt;
use crate::protocol::{
    ContextPayloadPressure, ContextPreflightReport, ContextPressureLevel,
    ContextRequestTokenBreakdown,
};
use jcode_provider_core::ContextRequestBudget;

pub fn estimate_pending_input_tokens(content: &str, image_count: usize) -> usize {
    let mut blocks = Vec::with_capacity(image_count.saturating_add(1));
    blocks.extend(
        (0..image_count).map(|_| jcode_message_types::ContentBlock::Image {
            media_type: "image/unknown".to_string(),
            data: String::new(),
        }),
    );
    blocks.push(jcode_message_types::ContentBlock::Text {
        text: content.to_string(),
        cache_control: None,
    });
    jcode_context_core::estimate_content_blocks_tokens(&blocks)
}

pub fn request_payload_pressure(messages: &[Message]) -> ContextPayloadPressure {
    let mut image_count = 0usize;
    let mut estimated_base64_bytes = 0usize;
    for block in messages.iter().flat_map(|message| message.content.iter()) {
        if let jcode_message_types::ContentBlock::Image { data, .. } = block {
            image_count = image_count.saturating_add(1);
            estimated_base64_bytes = estimated_base64_bytes.saturating_add(data.len());
        }
    }
    ContextPayloadPressure {
        image_count,
        estimated_base64_bytes,
    }
}

pub fn request_token_breakdown(
    messages: &[Message],
    pending_input_tokens: usize,
    memory_tokens: usize,
    system: &SplitSystemPrompt,
    tools: &[ToolDefinition],
) -> ContextRequestTokenBreakdown {
    let all_message_tokens = jcode_context_core::estimate_messages_tokens(messages);
    ContextRequestTokenBreakdown {
        system_tokens: system.estimated_tokens(),
        tool_definition_tokens: ToolDefinition::aggregate_prompt_token_estimate(tools),
        historical_message_tokens: all_message_tokens
            .saturating_sub(pending_input_tokens)
            .saturating_sub(memory_tokens),
        pending_input_tokens,
        memory_tokens,
    }
}

pub fn evaluate_context_preflight(
    context_revision: u64,
    budget: ContextRequestBudget,
    breakdown: ContextRequestTokenBreakdown,
) -> ContextPreflightReport {
    let projected_input_tokens = breakdown.projected_input_tokens();
    let safe_input_budget = budget.safe_input_budget();
    let pressure = jcode_context_core::calculate_context_pressure(
        projected_input_tokens,
        budget.context_window,
        safe_input_budget,
    );
    ContextPreflightReport {
        context_revision,
        pressure: match pressure.tier {
            jcode_context_core::ContextPressureTier::Normal => ContextPressureLevel::Normal,
            jcode_context_core::ContextPressureTier::Notice => ContextPressureLevel::Notice,
            jcode_context_core::ContextPressureTier::Urgent => ContextPressureLevel::Urgent,
            jcode_context_core::ContextPressureTier::Blocked => ContextPressureLevel::Blocked,
        },
        context_window: budget.context_window,
        safe_input_budget,
        projected_input_tokens,
        required_reduction_tokens: projected_input_tokens.saturating_sub(safe_input_budget),
        remaining_context_tokens: pressure.remaining_context_tokens,
        remaining_safe_input_tokens: pressure.remaining_safe_input_tokens,
        semantics: budget.semantics,
        requested_max_output_tokens: budget.requested_max_output_tokens,
        output_reserve_tokens: budget.output_reserve_tokens(),
        estimator_margin_tokens: budget.estimator_margin_tokens,
        exact_output_reserve_known: budget.exact_output_reserve_known(),
        breakdown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_provider_core::ContextWindowSemantics;

    fn breakdown(projected: usize) -> ContextRequestTokenBreakdown {
        ContextRequestTokenBreakdown {
            system_tokens: 1_000,
            tool_definition_tokens: 2_000,
            historical_message_tokens: projected.saturating_sub(3_300),
            pending_input_tokens: 200,
            memory_tokens: 100,
        }
    }

    #[test]
    fn combined_limit_uses_actual_request_output_cap_and_margin() {
        let report = evaluate_context_preflight(
            7,
            ContextRequestBudget {
                context_window: 100_000,
                semantics: ContextWindowSemantics::InputPlusOutput,
                requested_max_output_tokens: Some(8_000),
                estimator_margin_tokens: 4_000,
            },
            breakdown(88_001),
        );
        assert_eq!(report.safe_input_budget, 88_000);
        assert_eq!(report.required_reduction_tokens, 1);
        assert_eq!(report.pressure, ContextPressureLevel::Blocked);
        assert_eq!(report.output_reserve_tokens, 8_000);
        assert!(report.exact_output_reserve_known);
        assert_eq!(report.breakdown.projected_input_tokens(), 88_001);
    }

    #[test]
    fn unknown_semantics_never_invents_an_output_reserve() {
        let report = evaluate_context_preflight(
            2,
            ContextRequestBudget::unknown(100_000),
            breakdown(95_903),
        );
        assert_eq!(report.safe_input_budget, 95_904);
        assert_eq!(report.output_reserve_tokens, 0);
        assert!(!report.exact_output_reserve_known);
        assert_eq!(report.pressure, ContextPressureLevel::Urgent);
    }

    #[test]
    fn token_breakdown_counts_each_request_category_once() {
        let messages = vec![Message::user("history"), Message::user("pending")];
        let pending = jcode_context_core::estimate_message_tokens(&messages[1]);
        let system = SplitSystemPrompt {
            static_part: "system".to_string(),
            dynamic_part: "dynamic".to_string(),
        };
        let result = request_token_breakdown(&messages, pending, 0, &system, &[]);
        assert_eq!(result.pending_input_tokens, pending);
        assert_eq!(
            result.historical_message_tokens.saturating_add(pending),
            jcode_context_core::estimate_messages_tokens(&messages)
        );
        assert_eq!(
            result.projected_input_tokens(),
            result.system_tokens
                + result.tool_definition_tokens
                + jcode_context_core::estimate_messages_tokens(&messages)
        );
    }
}
