//! Shared reasoning-effort ladders.
//!
//! Keep these in provider-core so provider runtimes and UI clients expose the
//! same ordered values. `swarm` and `swarm-deep` are Jcode UI sentinels rather
//! than wire-level provider values, but they belong in the selectable ladder.

/// OpenAI Responses API effort levels, followed by Jcode's swarm modes.
pub const OPENAI_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "swarm",
    "swarm-deep",
];

/// OpenRouter's unified reasoning effort levels.
///
/// OpenRouter currently treats `max` as an alias for `xhigh`, so it is not a
/// separate rung in this ladder.
pub const OPENROUTER_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "swarm",
    "swarm-deep",
];

/// Direct DeepSeek effort levels, followed by Jcode's swarm modes.
pub const DEEPSEEK_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "low",
    "medium",
    "high",
    "max",
    "swarm",
    "swarm-deep",
];

/// Convert a provider-advertised OpenAI/OpenRouter effort into the canonical
/// static value used by the provider trait.
pub fn canonical_reasoning_effort(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// The selectable effort ladder of a Claude model, the one source for the
/// Anthropic runtime and for clients that know only the model. `none` is
/// offered only where the model can run without thinking (INT-01 DESIGN §7).
pub fn anthropic_selectable_efforts(model: &str) -> Vec<&'static str> {
    let caps = crate::anthropic_reasoning_caps(model);
    if !caps.supports_reasoning_effort() {
        return Vec::new();
    }
    let mut efforts = Vec::new();
    if caps.thinking_off != crate::ThinkingOff::AlwaysOn {
        efforts.push("none");
    }
    efforts.extend(["low", "medium", "high"]);
    if caps.xhigh_effort {
        efforts.push("xhigh");
    }
    if caps.max_effort {
        efforts.push("max");
    }
    efforts.extend(["swarm", "swarm-deep"]);
    efforts
}

/// Infer the selectable effort ladder when only provider/model identity is
/// available, such as in a remote TUI session.
pub fn inferred_reasoning_efforts(
    provider_name: Option<&str>,
    model_name: Option<&str>,
) -> Vec<&'static str> {
    let provider = provider_name.unwrap_or_default().to_ascii_lowercase();
    let model = model_name.unwrap_or_default().to_ascii_lowercase();

    if provider.contains("openrouter") {
        return OPENROUTER_SELECTABLE_EFFORTS.to_vec();
    }

    if provider.contains("deepseek") || model.contains("deepseek") {
        return DEEPSEEK_SELECTABLE_EFFORTS.to_vec();
    }

    let is_openai_model = model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("o5");
    if provider.contains("openai-compatible") {
        return if is_openai_model {
            OPENAI_SELECTABLE_EFFORTS.to_vec()
        } else {
            Vec::new()
        };
    }

    let is_anthropic = provider.contains("anthropic")
        || provider.contains("claude")
        || model.starts_with("claude-");
    if is_anthropic {
        return anthropic_selectable_efforts(&model);
    }

    let is_openai = provider.contains("openai") || provider.contains("codex") || is_openai_model;
    if is_openai {
        return OPENAI_SELECTABLE_EFFORTS.to_vec();
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_ladders_offer_none_only_where_thinking_can_be_turned_off() {
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"] {
            let efforts = anthropic_selectable_efforts(model);
            assert!(!efforts.contains(&"none"), "{model}: {efforts:?}");
            assert_eq!(&efforts[..3], ["low", "medium", "high"], "{model}");
            assert_eq!(
                inferred_reasoning_efforts(Some("Claude"), Some(model)),
                efforts,
                "the remote client infers the runtime's ladder"
            );
        }
        for model in ["claude-opus-5", "claude-sonnet-5", "claude-opus-4-8"] {
            assert_eq!(anthropic_selectable_efforts(model)[0], "none", "{model}");
        }
    }

    #[test]
    fn provider_ladders_preserve_distinct_max_semantics() {
        assert_eq!(
            inferred_reasoning_efforts(Some("openai"), Some("gpt-5.4")),
            OPENAI_SELECTABLE_EFFORTS
        );
        assert!(OPENAI_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(OPENAI_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(OPENROUTER_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(!OPENROUTER_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(DEEPSEEK_SELECTABLE_EFFORTS.contains(&"max"));
        assert_eq!(
            inferred_reasoning_efforts(Some("openai-compatible:custom"), Some("gpt-5.6")),
            OPENAI_SELECTABLE_EFFORTS,
            "direct OpenAI-compatible runtimes use the OpenAI reasoning_effort vocabulary"
        );
    }

    #[test]
    fn anthropic_ladder_comes_from_model_capabilities() {
        assert_eq!(
            inferred_reasoning_efforts(Some("anthropic"), Some("claude-sonnet-4-6")),
            vec![
                "none",
                "low",
                "medium",
                "high",
                "max",
                "swarm",
                "swarm-deep"
            ]
        );
        assert_eq!(
            inferred_reasoning_efforts(Some("anthropic"), Some("claude-opus-4-7")),
            vec![
                "none",
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
                "swarm",
                "swarm-deep"
            ]
        );
    }
}
