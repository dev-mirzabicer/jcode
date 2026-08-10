#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPressureTier {
    Normal,
    Notice,
    Urgent,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextPressure {
    pub tier: ContextPressureTier,
    pub projected_input_tokens: usize,
    pub context_window: usize,
    pub safe_input_budget: usize,
    pub remaining_context_tokens: usize,
    pub remaining_safe_input_tokens: usize,
}

pub fn calculate_context_pressure(
    projected_input_tokens: usize,
    context_window: usize,
    safe_input_budget: usize,
) -> ContextPressure {
    let remaining_context_tokens = context_window.saturating_sub(projected_input_tokens);
    let remaining_safe_input_tokens = safe_input_budget.saturating_sub(projected_input_tokens);
    let notice_remaining = (context_window / 10).max(24_000);
    let urgent_remaining = (context_window / 20).max(12_000);
    let tier = if projected_input_tokens > safe_input_budget {
        ContextPressureTier::Blocked
    } else if remaining_context_tokens <= urgent_remaining {
        ContextPressureTier::Urgent
    } else if remaining_context_tokens <= notice_remaining {
        ContextPressureTier::Notice
    } else {
        ContextPressureTier::Normal
    };
    ContextPressure {
        tier,
        projected_input_tokens,
        context_window,
        safe_input_budget,
        remaining_context_tokens,
        remaining_safe_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_tiers_match_large_context_ux_thresholds() {
        assert_eq!(
            calculate_context_pressure(334_799, 372_000, 368_000).tier,
            ContextPressureTier::Normal
        );
        assert_eq!(
            calculate_context_pressure(334_800, 372_000, 368_000).tier,
            ContextPressureTier::Notice
        );
        assert_eq!(
            calculate_context_pressure(353_400, 372_000, 368_000).tier,
            ContextPressureTier::Urgent
        );
        assert_eq!(
            calculate_context_pressure(368_001, 372_000, 368_000).tier,
            ContextPressureTier::Blocked
        );
        assert_eq!(
            calculate_context_pressure(368_000, 372_000, 368_000).tier,
            ContextPressureTier::Urgent
        );
    }

    #[test]
    fn fixed_headroom_floors_apply_to_smaller_context_windows() {
        assert_eq!(
            calculate_context_pressure(75_999, 100_000, 96_000).tier,
            ContextPressureTier::Normal
        );
        assert_eq!(
            calculate_context_pressure(76_000, 100_000, 96_000).tier,
            ContextPressureTier::Notice
        );
        assert_eq!(
            calculate_context_pressure(88_000, 100_000, 96_000).tier,
            ContextPressureTier::Urgent
        );
        assert_eq!(
            calculate_context_pressure(96_001, 100_000, 96_000).tier,
            ContextPressureTier::Blocked
        );
    }
}
